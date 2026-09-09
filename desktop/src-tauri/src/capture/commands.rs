//! Tauri-ready desktop capture command boundary.
//!
//! The full command/state implementation lives in this module so application
//! integration needs one state initializer and one handler macro.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{Local, TimeDelta};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

#[cfg(test)]
use crate::ingest::envelope::TrackRole;
use crate::ingest::envelope::{
    AudioTrack, CaptureWarning, JobStatus, Platform, RecordingEnvelope, RECORDING_ENVELOPE_VERSION,
};
use crate::ingest::inbox::Inbox;
use crate::ingest::state::JobState;

use super::model::{CaptureMode, CapturePlan, CapturePreflight, CaptureTarget, PermissionState};
use super::session::{
    CaptureError, CaptureFinalizer, CapturePhase, CaptureSession, CaptureSnapshot,
    DesktopCaptureBackend, FinalizedCapture, NormalizedArtifact,
};

const NORMALIZED_SAMPLE_RATE: u32 = 16_000;
const MAX_NORMALIZED_BYTES: u64 = u32::MAX as u64 - 44;
type DesktopBackendHandle = Arc<dyn DesktopCaptureBackend>;
type CaptureCatalogHandle = Arc<dyn CaptureCatalog>;
type ProductionBackend = (Option<DesktopBackendHandle>, Option<CaptureCatalogHandle>);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureCapabilitiesDto {
    pub platform: Option<Platform>,
    pub backend_available: bool,
    pub meeting_available: bool,
    pub modes: [CaptureMode; 3],
    pub system_capture_scope: &'static str,
    pub browser_capture_scope: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePermissionsDto {
    pub microphone: PermissionState,
    pub screen_and_system_audio: PermissionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSourceKindDto {
    Microphone,
    NativeApplication,
    BrowserApplication,
    SystemOutput,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceDto {
    pub id: String,
    pub label: String,
    pub kind: CaptureSourceKindDto,
    pub available: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourcesDto {
    pub sources: Vec<CaptureSourceDto>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureSourceIconRequest {
    pub source_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceIconDto {
    pub icon_data_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapturePlanRequest {
    pub plan: CapturePlan,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapturePermissionRequest {
    pub microphone: bool,
    pub screen_and_system_audio: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePermissionSettingsKind {
    Microphone,
    ScreenAndSystemAudio,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapturePermissionSettingsRequest {
    pub kind: CapturePermissionSettingsKind,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureActionRequest {}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatusDto {
    pub recording_id: Option<String>,
    pub phase: Option<CapturePhase>,
    pub mode: Option<CaptureMode>,
    pub duration_ms: u64,
    pub segment_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<CaptureStatusWarningDto>,
    pub gap_duration_ms: u64,
    pub recovered: bool,
    pub microphone_level: Option<f32>,
    pub system_level: Option<f32>,
    pub microphone_label: Option<String>,
    pub source_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatusWarningDto {
    pub code: String,
    pub message: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStopDto {
    pub recording_id: String,
    pub envelope: RecordingEnvelope,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureCommandError {
    pub code: &'static str,
    pub message: &'static str,
}

impl CaptureCommandError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            message: public_error_message(code),
        }
    }
}

impl From<CaptureError> for CaptureCommandError {
    fn from(error: CaptureError) -> Self {
        Self::new(error.code)
    }
}

impl std::fmt::Display for CaptureCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for CaptureCommandError {}

pub trait CaptureCatalog: Send + Sync {
    fn permissions(&self) -> Result<CapturePermissionsDto, CaptureError>;
    fn sources(&self) -> Result<Vec<CaptureSourceDto>, CaptureError>;

    fn icon_data_url(&self, _source_id: &str) -> Result<Option<String>, CaptureError> {
        Ok(None)
    }

    fn request_permissions(
        &self,
        microphone: bool,
        screen_and_system_audio: bool,
    ) -> Result<CapturePermissionsDto, CaptureError> {
        let _ = (microphone, screen_and_system_audio);
        self.permissions()
    }
}

pub trait CaptureClock: Send + Sync {
    fn now_ns(&self) -> u64;
}

struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl CaptureClock for MonotonicClock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

pub struct CaptureCommandState {
    inbox: Arc<Inbox>,
    backend: Option<Arc<dyn DesktopCaptureBackend>>,
    catalog: Option<Arc<dyn CaptureCatalog>>,
    finalizer: Arc<dyn CaptureFinalizer + Send + Sync>,
    clock: Arc<dyn CaptureClock>,
    active: Mutex<Option<CaptureSession>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayCaptureSnapshot {
    pub phase: CapturePhase,
    pub duration_ms: u64,
    pub source_label: String,
}

impl CaptureCommandState {
    pub fn with_components(
        inbox: Arc<Inbox>,
        backend: Option<Arc<dyn DesktopCaptureBackend>>,
        catalog: Option<Arc<dyn CaptureCatalog>>,
        finalizer: Arc<dyn CaptureFinalizer + Send + Sync>,
        clock: Arc<dyn CaptureClock>,
    ) -> Result<Self, CaptureCommandError> {
        let recovered = recover_single_session(Arc::clone(&inbox))?;
        Ok(Self {
            inbox,
            backend,
            catalog,
            finalizer,
            clock,
            active: Mutex::new(recovered),
        })
    }

    fn active(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<CaptureSession>>, CaptureCommandError> {
        self.active
            .lock()
            .map_err(|_| CaptureCommandError::new("capture_state_unavailable"))
    }

    fn capabilities(&self, recording_enabled: bool) -> CaptureCapabilitiesDto {
        let platform = current_platform();
        CaptureCapabilitiesDto {
            platform,
            backend_available: recording_enabled && self.backend.is_some(),
            meeting_available: match platform {
                #[cfg(target_os = "macos")]
                Some(Platform::Macos) => super::macos::process_tap_available(),
                #[cfg(target_os = "windows")]
                Some(Platform::Windows) => true,
                _ => false,
            },
            modes: [
                CaptureMode::VoiceMemo,
                CaptureMode::Meeting,
                CaptureMode::SystemCapture,
            ],
            system_capture_scope: "all_system_audio",
            browser_capture_scope: "whole_browser_application",
        }
    }

    fn status_locked(&self, session: Option<&CaptureSession>, recovered: bool) -> CaptureStatusDto {
        match session {
            Some(session) => {
                let levels = session.levels();
                let snapshot = session.snapshot();
                let warnings = snapshot
                    .notices
                    .iter()
                    .rev()
                    .take(16)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|notice| CaptureStatusWarningDto {
                        code: notice.code.clone(),
                        message: notice.message.clone(),
                        at_ms: notice
                            .clock_at_ns
                            .saturating_sub(snapshot.monotonic_started_ns)
                            / 1_000_000,
                    })
                    .collect();
                let gap_duration_ms = snapshot.gaps.iter().fold(0_u64, |total, gap| {
                    total.saturating_add(
                        gap.clock_end_ns.saturating_sub(gap.clock_start_ns) / 1_000_000,
                    )
                });
                let (microphone_label, source_label) = capture_status_labels(&snapshot.plan);
                CaptureStatusDto {
                    recording_id: Some(session.recording_id().to_string()),
                    phase: Some(snapshot.phase),
                    mode: Some(snapshot.plan.mode),
                    duration_ms: snapshot.observed_duration_ms(),
                    segment_count: snapshot.segments.len(),
                    warning_count: snapshot.notices.len(),
                    warnings,
                    gap_duration_ms,
                    recovered,
                    microphone_level: levels.microphone,
                    system_level: levels.system,
                    microphone_label: Some(microphone_label),
                    source_label,
                }
            }
            None => CaptureStatusDto {
                recording_id: None,
                phase: None,
                mode: None,
                duration_ms: 0,
                segment_count: 0,
                warning_count: 0,
                warnings: Vec::new(),
                gap_duration_ms: 0,
                recovered: false,
                microphone_level: None,
                system_level: None,
                microphone_label: None,
                source_label: None,
            },
        }
    }

    pub fn is_active(&self) -> bool {
        self.active().is_ok_and(|active| active.is_some())
    }

    pub fn tray_snapshot(&self) -> Result<Option<TrayCaptureSnapshot>, CaptureCommandError> {
        let mut active = self.active()?;
        let now = self.clock.now_ns();
        let Some(session) = active.as_mut() else {
            return Ok(None);
        };
        if session.snapshot().phase == CapturePhase::Recording {
            session.tick(now)?;
        }
        let snapshot = session.snapshot();
        let closed_gap_ns = snapshot.gaps.iter().fold(0_u64, |total, gap| {
            total.saturating_add(gap.clock_end_ns.saturating_sub(gap.clock_start_ns))
        });
        let open_gap_ns = snapshot
            .paused_at_ns
            .map(|paused_at| now.saturating_sub(paused_at))
            .unwrap_or(0);
        let duration_ms = now
            .saturating_sub(snapshot.monotonic_started_ns)
            .saturating_sub(closed_gap_ns)
            .saturating_sub(open_gap_ns)
            .checked_div(1_000_000)
            .unwrap_or(0);
        Ok(Some(TrayCaptureSnapshot {
            phase: snapshot.phase,
            duration_ms,
            source_label: snapshot
                .plan
                .recording_source()
                .label
                .unwrap_or_else(|| "EchoWall".to_owned()),
        }))
    }

    pub fn toggle_pause_for_tray(&self) -> Result<CaptureStatusDto, CaptureCommandError> {
        let mut active = self.active()?;
        let session = active
            .as_mut()
            .ok_or_else(|| CaptureCommandError::new("capture_not_active"))?;
        let now = self.clock.now_ns();
        match session.snapshot().phase {
            CapturePhase::Recording => {
                session.tick(now)?;
                session.pause(now)?;
            }
            CapturePhase::Paused => session.resume(now)?,
            _ => return Err(CaptureCommandError::new("capture_not_active")),
        }
        Ok(self.status_locked(Some(session), false))
    }

    pub fn stop_for_tray(&self) -> Result<CaptureStopDto, CaptureCommandError> {
        let mut active = self.active()?;
        let session = active
            .as_mut()
            .ok_or_else(|| CaptureCommandError::new("capture_not_active"))?;
        let envelope = session.stop(self.clock.now_ns(), self.finalizer.as_ref())?;
        let recording_id = envelope.recording_id.to_string();
        *active = None;
        Ok(CaptureStopDto {
            recording_id,
            envelope,
        })
    }
}

fn capture_status_labels(plan: &CapturePlan) -> (String, Option<String>) {
    match &plan.target {
        CaptureTarget::VoiceMemo { microphone } => (microphone.label.clone(), None),
        CaptureTarget::Meeting { microphone, source } => {
            let label = match source {
                super::model::MeetingSource::NativeApplication { label, .. }
                | super::model::MeetingSource::WholeBrowser { label, .. } => label.clone(),
            };
            (microphone.label.clone(), Some(label))
        }
        CaptureTarget::SystemCapture { microphone, output } => {
            (microphone.label.clone(), Some(output.label.clone()))
        }
    }
}

pub fn initialize_capture_state(
    inbox: Arc<Inbox>,
) -> Result<CaptureCommandState, CaptureCommandError> {
    let (backend, catalog) = production_backend();
    CaptureCommandState::with_components(
        inbox,
        backend,
        catalog,
        Arc::new(PcmTimelineFinalizer),
        Arc::new(MonotonicClock::new()),
    )
}

#[tauri::command]
pub fn capture_capabilities(
    state: tauri::State<'_, CaptureCommandState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
) -> CaptureCapabilitiesDto {
    state.capabilities(features.recording)
}

#[tauri::command]
pub fn capture_permissions(
    state: tauri::State<'_, CaptureCommandState>,
) -> Result<CapturePermissionsDto, CaptureCommandError> {
    state
        .catalog
        .as_deref()
        .ok_or_else(|| CaptureCommandError::new("backend_unavailable"))?
        .permissions()
        .map_err(Into::into)
}

#[tauri::command]
pub async fn capture_request_permissions(
    state: tauri::State<'_, CaptureCommandState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: CapturePermissionRequest,
) -> Result<CapturePermissionsDto, CaptureCommandError> {
    validate_permission_request(&request, features.recording)?;
    let catalog = state
        .catalog
        .as_ref()
        .cloned()
        .ok_or_else(|| CaptureCommandError::new("backend_unavailable"))?;
    tauri::async_runtime::spawn_blocking(move || {
        catalog.request_permissions(request.microphone, request.screen_and_system_audio)
    })
    .await
    .map_err(|_| CaptureCommandError::new("permission_request_failed"))?
    .map_err(Into::into)
}

#[tauri::command]
pub fn open_capture_permission_settings(
    app: tauri::AppHandle,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: CapturePermissionSettingsRequest,
) -> Result<(), CaptureCommandError> {
    if !features.recording {
        return Err(CaptureCommandError::new("recording_disabled"));
    }
    let url = capture_permission_settings_url(request.kind)
        .ok_or_else(|| CaptureCommandError::new("settings_unavailable"))?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|_| CaptureCommandError::new("settings_open_failed"))
}

#[tauri::command]
pub fn capture_sources(
    state: tauri::State<'_, CaptureCommandState>,
) -> Result<CaptureSourcesDto, CaptureCommandError> {
    let sources = state
        .catalog
        .as_deref()
        .ok_or_else(|| CaptureCommandError::new("backend_unavailable"))?
        .sources()?;
    Ok(CaptureSourcesDto { sources })
}

#[tauri::command]
pub fn capture_source_icon(
    state: tauri::State<'_, CaptureCommandState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: CaptureSourceIconRequest,
) -> Result<CaptureSourceIconDto, CaptureCommandError> {
    capture_source_icon_for_catalog(state.catalog.as_deref(), features.recording, request)
}

fn capture_source_icon_for_catalog(
    catalog: Option<&dyn CaptureCatalog>,
    recording_enabled: bool,
    request: CaptureSourceIconRequest,
) -> Result<CaptureSourceIconDto, CaptureCommandError> {
    if !recording_enabled {
        return Err(CaptureCommandError::new("recording_disabled"));
    }
    if request.source_id.is_empty()
        || request.source_id.len() > 256
        || request.source_id.chars().any(char::is_control)
    {
        return Err(CaptureCommandError::new("selected_source_missing"));
    }
    let catalog = catalog.ok_or_else(|| CaptureCommandError::new("backend_unavailable"))?;
    let allowed = catalog.sources()?.into_iter().any(|source| {
        source.available
            && source.id == request.source_id
            && matches!(
                source.kind,
                CaptureSourceKindDto::NativeApplication | CaptureSourceKindDto::BrowserApplication
            )
    });
    if !allowed {
        return Err(CaptureCommandError::new("selected_source_missing"));
    }
    let icon_data_url = catalog.icon_data_url(&request.source_id)?;
    if icon_data_url.as_deref().is_some_and(|value| {
        value.len() > 96 * 1024
            || !value.starts_with("data:image/png;base64,")
            || !value["data:image/png;base64,".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    }) {
        return Err(CaptureCommandError::new("capture_source_icon_invalid"));
    }
    Ok(CaptureSourceIconDto { icon_data_url })
}

#[tauri::command]
pub fn capture_preflight(
    state: tauri::State<'_, CaptureCommandState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: CapturePlanRequest,
) -> Result<CapturePreflight, CaptureCommandError> {
    require_capture_feature(&features, &request.plan)?;
    validate_plan_sources(&request.plan, state.catalog.as_deref())?;
    CaptureSession::preflight(state.backend.as_deref(), &request.plan).map_err(Into::into)
}

#[tauri::command]
pub fn start_capture(
    state: tauri::State<'_, CaptureCommandState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: CapturePlanRequest,
) -> Result<CaptureStatusDto, CaptureCommandError> {
    require_capture_feature(&features, &request.plan)?;
    validate_plan_sources(&request.plan, state.catalog.as_deref())?;
    let mut active = state.active()?;
    if active.is_some() {
        return Err(CaptureCommandError::new("capture_already_active"));
    }
    let started = state.clock.now_ns();
    let session = CaptureSession::start(
        Arc::clone(&state.inbox),
        state.backend.as_deref(),
        request.plan,
        Local::now().fixed_offset(),
        started,
    )?;
    *active = Some(session);
    Ok(state.status_locked(active.as_ref(), false))
}

fn require_capture_feature(
    features: &crate::features::RuntimeFeatures,
    plan: &CapturePlan,
) -> Result<(), CaptureCommandError> {
    if !features.recording {
        return Err(CaptureCommandError::new("recording_disabled"));
    }
    if !features.browser_capture
        && matches!(
            plan.target,
            CaptureTarget::Meeting {
                source: super::model::MeetingSource::WholeBrowser { .. },
                ..
            }
        )
    {
        return Err(CaptureCommandError::new("browser_capture_disabled"));
    }
    Ok(())
}

#[tauri::command]
pub fn pause_capture(
    state: tauri::State<'_, CaptureCommandState>,
    request: CaptureActionRequest,
) -> Result<CaptureStatusDto, CaptureCommandError> {
    let mut active = state.active()?;
    let session = active
        .as_mut()
        .ok_or_else(|| CaptureCommandError::new("capture_not_active"))?;
    let _ = request;
    let clock = state.clock.now_ns();
    session.tick(clock)?;
    session.pause(clock)?;
    Ok(state.status_locked(Some(session), false))
}

#[tauri::command]
pub fn resume_capture(
    state: tauri::State<'_, CaptureCommandState>,
    request: CaptureActionRequest,
) -> Result<CaptureStatusDto, CaptureCommandError> {
    let mut active = state.active()?;
    let session = active
        .as_mut()
        .ok_or_else(|| CaptureCommandError::new("capture_not_active"))?;
    let _ = request;
    session.resume(state.clock.now_ns())?;
    Ok(state.status_locked(Some(session), false))
}

#[tauri::command]
pub fn stop_capture(
    state: tauri::State<'_, CaptureCommandState>,
    request: CaptureActionRequest,
) -> Result<CaptureStopDto, CaptureCommandError> {
    let _ = request;
    state.stop_for_tray()
}

#[tauri::command]
pub fn capture_status(
    state: tauri::State<'_, CaptureCommandState>,
) -> Result<CaptureStatusDto, CaptureCommandError> {
    let mut active = state.active()?;
    let clock = state.clock.now_ns();
    if let Some(session) = active.as_mut() {
        if session.snapshot().phase == CapturePhase::Recording {
            session.tick(clock)?;
        }
    }
    let recovered = active
        .as_ref()
        .is_some_and(|session| session.snapshot().phase == CapturePhase::Interrupted);
    Ok(state.status_locked(active.as_ref(), recovered))
}

#[macro_export]
macro_rules! register_capture_commands {
    () => {
        tauri::generate_handler![
            $crate::capture::commands::capture_capabilities,
            $crate::capture::commands::capture_permissions,
            $crate::capture::commands::capture_source_icon,
            $crate::capture::commands::capture_sources,
            $crate::capture::commands::capture_preflight,
            $crate::capture::commands::capture_request_permissions,
            $crate::capture::commands::open_capture_permission_settings,
            $crate::capture::commands::start_capture,
            $crate::capture::commands::pause_capture,
            $crate::capture::commands::resume_capture,
            $crate::capture::commands::stop_capture,
            $crate::capture::commands::capture_status
        ]
    };
}

fn current_platform() -> Option<Platform> {
    #[cfg(target_os = "macos")]
    return Some(Platform::Macos);
    #[cfg(target_os = "windows")]
    return Some(Platform::Windows);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    None
}

fn production_backend() -> ProductionBackend {
    #[cfg(target_os = "macos")]
    {
        let backend = Arc::new(super::macos::MacOsCaptureBackend::default());
        let desktop: Arc<dyn DesktopCaptureBackend> = backend.clone();
        let catalog: Arc<dyn CaptureCatalog> = backend;
        (Some(desktop), Some(catalog))
    }
    #[cfg(target_os = "windows")]
    {
        let backend = Arc::new(super::windows::WindowsCaptureBackend);
        let desktop: Arc<dyn DesktopCaptureBackend> = backend.clone();
        let catalog: Arc<dyn CaptureCatalog> = backend;
        (Some(desktop), Some(catalog))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    (None, None)
}

#[cfg(target_os = "macos")]
impl CaptureCatalog for super::macos::MacOsCaptureBackend {
    fn permissions(&self) -> Result<CapturePermissionsDto, CaptureError> {
        let value = self.permission_snapshot()?;
        Ok(CapturePermissionsDto {
            microphone: value.microphone,
            screen_and_system_audio: value.screen_and_system_audio,
        })
    }

    fn sources(&self) -> Result<Vec<CaptureSourceDto>, CaptureError> {
        let mut result: Vec<_> = super::macos::MacOsNativeBridge::enumerate_microphones()
            .into_iter()
            .map(|source| CaptureSourceDto {
                id: source.id,
                label: source.label,
                kind: CaptureSourceKindDto::Microphone,
                available: true,
                is_default: source.is_default,
            })
            .collect();
        if self.permission_snapshot()?.screen_and_system_audio == PermissionState::Granted {
            result.extend(self.enumerate_capture_sources()?.into_iter().map(|source| {
                CaptureSourceDto {
                    id: source.id,
                    label: source.label,
                    kind: match source.kind {
                        super::macos::MacCaptureSourceKind::NativeApplication => {
                            CaptureSourceKindDto::NativeApplication
                        }
                        super::macos::MacCaptureSourceKind::BrowserApplication => {
                            CaptureSourceKindDto::BrowserApplication
                        }
                    },
                    available: true,
                    is_default: false,
                }
            }));
        }
        result.push(CaptureSourceDto {
            id: "macos:all-system".to_owned(),
            label: "All system audio".to_owned(),
            kind: CaptureSourceKindDto::SystemOutput,
            available: true,
            is_default: false,
        });
        Ok(result)
    }

    fn icon_data_url(&self, source_id: &str) -> Result<Option<String>, CaptureError> {
        self.capture_source_icon(source_id)
    }

    fn request_permissions(
        &self,
        microphone: bool,
        screen_and_system_audio: bool,
    ) -> Result<CapturePermissionsDto, CaptureError> {
        let value = self.request_permission_snapshot(microphone, screen_and_system_audio)?;
        Ok(CapturePermissionsDto {
            microphone: value.microphone,
            screen_and_system_audio: value.screen_and_system_audio,
        })
    }
}

#[cfg(target_os = "windows")]
impl CaptureCatalog for super::windows::WindowsCaptureBackend {
    fn permissions(&self) -> Result<CapturePermissionsDto, CaptureError> {
        Ok(CapturePermissionsDto {
            microphone: PermissionState::NotDetermined,
            screen_and_system_audio: PermissionState::Granted,
        })
    }

    fn sources(&self) -> Result<Vec<CaptureSourceDto>, CaptureError> {
        Ok(Self::enumerate_sources()?
            .into_iter()
            .map(|source| match source {
                super::windows::WindowsCaptureSource::Endpoint {
                    id,
                    label,
                    is_microphone,
                } => CaptureSourceDto {
                    id,
                    label,
                    kind: if is_microphone {
                        CaptureSourceKindDto::Microphone
                    } else {
                        CaptureSourceKindDto::SystemOutput
                    },
                    available: true,
                    is_default: false,
                },
                super::windows::WindowsCaptureSource::Process {
                    id, label, browser, ..
                } => CaptureSourceDto {
                    id,
                    label,
                    kind: if browser {
                        CaptureSourceKindDto::BrowserApplication
                    } else {
                        CaptureSourceKindDto::NativeApplication
                    },
                    available: true,
                    is_default: false,
                },
            })
            .collect())
    }
}

fn validate_plan_sources(
    plan: &CapturePlan,
    catalog: Option<&dyn CaptureCatalog>,
) -> Result<(), CaptureCommandError> {
    plan.validate().map_err(CaptureError::from)?;
    let catalog = catalog.ok_or_else(|| CaptureCommandError::new("backend_unavailable"))?;
    let permissions = catalog.permissions()?;
    if permissions.microphone != PermissionState::Granted {
        return Ok(());
    }
    let requires_system = !matches!(plan.mode, CaptureMode::VoiceMemo);
    if requires_system && permissions.screen_and_system_audio != PermissionState::Granted {
        return Ok(());
    }
    let sources = catalog.sources()?;
    let (microphone_id, selected) = match &plan.target {
        CaptureTarget::VoiceMemo { microphone } => (microphone.id.as_str(), None),
        CaptureTarget::Meeting { microphone, source } => {
            let (id, kind) = match source {
                super::model::MeetingSource::NativeApplication { id, .. } => {
                    (id.as_str(), CaptureSourceKindDto::NativeApplication)
                }
                super::model::MeetingSource::WholeBrowser { id, .. } => {
                    (id.as_str(), CaptureSourceKindDto::BrowserApplication)
                }
            };
            (microphone.id.as_str(), Some((id, kind)))
        }
        CaptureTarget::SystemCapture { microphone, output } => (
            microphone.id.as_str(),
            Some((output.id.as_str(), CaptureSourceKindDto::SystemOutput)),
        ),
    };
    if !sources.iter().any(|source| {
        source.available
            && source.kind == CaptureSourceKindDto::Microphone
            && source.id == microphone_id
    }) {
        return Err(CaptureCommandError::new("selected_microphone_missing"));
    }
    if let Some((id, kind)) = selected {
        if !sources
            .iter()
            .any(|source| source.available && source.kind == kind && source.id == id)
        {
            return Err(CaptureCommandError::new("selected_source_missing"));
        }
    }
    Ok(())
}

fn validate_permission_request(
    request: &CapturePermissionRequest,
    recording_enabled: bool,
) -> Result<(), CaptureCommandError> {
    if !recording_enabled {
        return Err(CaptureCommandError::new("recording_disabled"));
    }
    if !request.microphone && !request.screen_and_system_audio {
        return Err(CaptureCommandError::new("invalid_permission_request"));
    }
    Ok(())
}

fn capture_permission_settings_url(kind: CapturePermissionSettingsKind) -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    return Some(match kind {
        CapturePermissionSettingsKind::Microphone => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        }
        CapturePermissionSettingsKind::ScreenAndSystemAudio => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        }
    });
    #[cfg(target_os = "windows")]
    return Some(match kind {
        CapturePermissionSettingsKind::Microphone
        | CapturePermissionSettingsKind::ScreenAndSystemAudio => "ms-settings:privacy-microphone",
    });
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = kind;
        None
    }
}

fn recover_single_session(
    inbox: Arc<Inbox>,
) -> Result<Option<CaptureSession>, CaptureCommandError> {
    let mut candidates = Vec::new();
    let entries = fs::read_dir(inbox.root())
        .map_err(|_| CaptureCommandError::new("capture_recovery_failed"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(recording_id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| Uuid::parse_str(name).ok())
        else {
            continue;
        };
        if path.join("capture-session.json").is_file() && !path.join("recording.json").exists() {
            candidates.push(recording_id);
        }
    }
    candidates.sort_unstable();
    if candidates.len() > 1 {
        return Err(CaptureCommandError::new("multiple_capture_recovery"));
    }
    candidates
        .pop()
        .map(|recording_id| CaptureSession::recover(inbox, recording_id).map_err(Into::into))
        .transpose()
}

fn public_error_message(code: &str) -> &'static str {
    match code {
        "backend_unavailable" => "native capture is unavailable on this platform",
        "capture_already_active" => "another recording is already active",
        "capture_not_active" => "no recording is active",
        "capture_storage_low" => "not enough free storage for a two-hour recording",
        "capture_storage_check_failed" => "available recording storage could not be checked",
        "invalid_permission_request" => "capture permission request is invalid",
        "permission_request_failed" => "capture permission request did not finish",
        "settings_unavailable" => "capture permission settings are unavailable",
        "settings_open_failed" => "capture permission settings could not be opened",
        "selected_microphone_missing" => "the selected microphone is unavailable",
        "selected_source_missing" => "the selected capture source is unavailable",
        "capture_source_icon_invalid" => "the selected capture source icon is invalid",
        "multiple_capture_recovery" => "multiple interrupted recordings require reconciliation",
        "capture_state_unavailable" => "capture state is unavailable",
        "invalid_capture_state" => "capture action is unavailable in the current state",
        "preflight_not_ready" => "capture permissions or selected source are not ready",
        "microphone_consent_required"
        | "source_consent_required"
        | "browser_scope_consent_required"
        | "system_scope_consent_required"
        | "mode_target_mismatch"
        | "invalid_source_identity"
        | "invalid_source_label" => "capture request is invalid",
        _ => "capture operation failed",
    }
}

struct PcmTimelineFinalizer;

impl CaptureFinalizer for PcmTimelineFinalizer {
    fn finalize(
        &self,
        snapshot: &CaptureSnapshot,
        package_directory: &Path,
    ) -> Result<FinalizedCapture, CaptureError> {
        let duration_ms = snapshot.observed_duration_ms();
        if duration_ms == 0 || snapshot.segments.is_empty() {
            return Err(CaptureError::local(
                "no_audio",
                "desktop capture has no closed audio segments",
            ));
        }
        let output_frames = duration_ms
            .checked_mul(u64::from(NORMALIZED_SAMPLE_RATE))
            .and_then(|frames| frames.checked_div(1_000))
            .ok_or_else(|| {
                CaptureError::local(
                    "normalized_too_large",
                    "desktop normalized audio duration overflowed",
                )
            })?;
        let output_bytes = output_frames.saturating_mul(2);
        if output_bytes == 0 || output_bytes > MAX_NORMALIZED_BYTES {
            return Err(CaptureError::local(
                "normalized_too_large",
                "desktop normalized audio exceeds its WAV size bound",
            ));
        }
        let roles = snapshot.plan.required_roles();
        let relative_path = "derived/mixed.wav".to_owned();
        let derived = package_directory.join("derived");
        fs::create_dir_all(&derived).map_err(finalizer_io_error)?;
        let temporary = derived.join(format!(".capture-finalize-{}.tmp", Uuid::new_v4()));
        let output = package_directory.join(&relative_path);
        let mut normalized =
            initialize_silent_pcm16_wav(&temporary, NORMALIZED_SAMPLE_RATE, output_frames)?;
        let mix_result = (|| {
            for segment in &snapshot.segments {
                if !roles.contains(&segment.role) {
                    return Err(CaptureError::local(
                        "invalid_segment_role",
                        "desktop capture segment has an unexpected role",
                    ));
                }
                let source = decode_pcm16_wav(
                    &package_directory.join(&segment.relative_path),
                    segment.sample_rate,
                    segment.channels,
                    segment.frames_written,
                )?;
                let mono = downmix(&source, segment.channels)?;
                let corrected_frames = timeline_frame_count(segment, NORMALIZED_SAMPLE_RATE)?;
                let resampled = linear_resample_to_len(&mono, corrected_frames);
                let offset = segment
                    .clock_start_ns
                    .saturating_sub(snapshot.monotonic_started_ns)
                    .saturating_mul(u64::from(NORMALIZED_SAMPLE_RATE))
                    / 1_000_000_000;
                mix_mono_into_wav(
                    &mut normalized,
                    offset,
                    &resampled,
                    output_frames,
                    roles.len(),
                )?;
            }
            normalized.sync_all().map_err(finalizer_io_error)
        })();
        drop(normalized);
        if let Err(error) = mix_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if output.exists() {
            fs::remove_file(&output).map_err(finalizer_io_error)?;
        }
        fs::rename(&temporary, &output).map_err(finalizer_io_error)?;
        let (sha256, size_bytes) = hash_path(&output)?;

        let tracks = snapshot
            .segments
            .iter()
            .map(|segment| AudioTrack {
                role: segment.role,
                relative_path: segment.relative_path.clone(),
                codec: segment.codec.clone(),
                sample_rate: segment.sample_rate,
                channels: segment.channels,
                duration_ms: segment.duration_ms(),
                clock_start_ns: segment.clock_start_ns,
                sha256: segment.sha256.clone(),
            })
            .collect();
        let mut warnings: Vec<CaptureWarning> = snapshot
            .gaps
            .iter()
            .map(|gap| CaptureWarning {
                code: match gap.reason {
                    super::session::GapReason::Pause => "pause_gap",
                    super::session::GapReason::SourceLoss => "source_gap",
                    super::session::GapReason::DeviceDiscontinuity => "device_gap",
                }
                .to_owned(),
                message: "Capture contains an explicit silent gap".to_owned(),
                at_ms: gap
                    .clock_start_ns
                    .saturating_sub(snapshot.monotonic_started_ns)
                    .checked_div(1_000_000)
                    .unwrap_or(0)
                    .min(duration_ms),
            })
            .collect();
        warnings.extend(snapshot.notices.iter().map(|notice| {
            CaptureWarning {
                code: notice.code.clone(),
                message: notice.message.clone(),
                at_ms: notice
                    .clock_at_ns
                    .saturating_sub(snapshot.monotonic_started_ns)
                    .checked_div(1_000_000)
                    .unwrap_or(0)
                    .min(duration_ms),
            }
        }));
        let ended_at = snapshot.wall_started_at
            + TimeDelta::try_milliseconds(i64::try_from(duration_ms).map_err(|_| {
                CaptureError::local(
                    "invalid_capture_duration",
                    "desktop capture duration is too large",
                )
            })?)
            .ok_or_else(|| {
                CaptureError::local(
                    "invalid_capture_duration",
                    "desktop capture duration is too large",
                )
            })?;
        let envelope = RecordingEnvelope {
            schema_version: RECORDING_ENVELOPE_VERSION,
            recording_id: snapshot.recording_id,
            source: snapshot.plan.recording_source(),
            captured_at: snapshot.wall_started_at,
            ended_at,
            duration_ms,
            tracks,
            normalized_audio: Some(relative_path.clone()),
            normalized_sha256: Some(sha256.clone()),
            imported_name: None,
            import_review: None,
            capture_warnings: warnings,
            job: JobStatus {
                state: JobState::Ready,
                attempt: 0,
                remote_job_id: None,
                last_error: None,
            },
        };
        Ok(FinalizedCapture {
            envelope,
            normalized: NormalizedArtifact {
                relative_path,
                sha256,
                size_bytes,
                duration_ms,
            },
        })
    }
}

fn initialize_silent_pcm16_wav(
    path: &Path,
    sample_rate: u32,
    frames: u64,
) -> Result<File, CaptureError> {
    let data_bytes = frames.checked_mul(2).ok_or_else(|| {
        CaptureError::local("normalized_too_large", "normalized WAV is too large")
    })?;
    let data_bytes = u32::try_from(data_bytes)
        .map_err(|_| CaptureError::local("normalized_too_large", "normalized WAV is too large"))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .map_err(finalizer_io_error)?;
    write_pcm16_wav_header(&mut file, sample_rate, 1, data_bytes)?;
    file.set_len(44 + u64::from(data_bytes))
        .map_err(finalizer_io_error)?;
    Ok(file)
}

fn mix_mono_into_wav(
    output: &mut File,
    frame_offset: u64,
    samples: &[f32],
    output_frames: u64,
    role_count: usize,
) -> Result<(), CaptureError> {
    const CHUNK_FRAMES: usize = 8 * 1024;
    if role_count == 0 || frame_offset >= output_frames {
        return Ok(());
    }
    let available = usize::try_from(output_frames - frame_offset).unwrap_or(usize::MAX);
    let count = samples.len().min(available);
    let mut encoded = vec![0u8; CHUNK_FRAMES * 2];
    for (chunk_index, chunk) in samples[..count].chunks(CHUNK_FRAMES).enumerate() {
        let byte_count = chunk.len() * 2;
        let chunk_frame = frame_offset.saturating_add((chunk_index * CHUNK_FRAMES) as u64);
        let byte_offset = 44u64.saturating_add(chunk_frame.saturating_mul(2));
        output
            .seek(SeekFrom::Start(byte_offset))
            .map_err(finalizer_io_error)?;
        output
            .read_exact(&mut encoded[..byte_count])
            .map_err(finalizer_io_error)?;
        for (index, sample) in chunk.iter().enumerate() {
            let start = index * 2;
            let current = i16::from_le_bytes([encoded[start], encoded[start + 1]]) as i32;
            let contribution = (*sample / role_count as f32 * f32::from(i16::MAX)).round() as i32;
            let mixed = current
                .saturating_add(contribution)
                .clamp(i16::MIN as i32, i16::MAX as i32);
            encoded[start..start + 2].copy_from_slice(&(mixed as i16).to_le_bytes());
        }
        output
            .seek(SeekFrom::Start(byte_offset))
            .map_err(finalizer_io_error)?;
        output
            .write_all(&encoded[..byte_count])
            .map_err(finalizer_io_error)?;
    }
    Ok(())
}

fn decode_pcm16_wav(
    path: &Path,
    expected_rate: u32,
    expected_channels: u32,
    expected_frames: u64,
) -> Result<Vec<i16>, CaptureError> {
    let mut file = File::open(path).map_err(finalizer_io_error)?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).map_err(finalizer_io_error)?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(invalid_pcm_segment());
    }
    let mut format = None;
    let mut data = None;
    loop {
        let mut chunk = [0u8; 8];
        match file.read_exact(&mut chunk) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(finalizer_io_error(error)),
        }
        let size = u32::from_le_bytes(chunk[4..8].try_into().expect("four bytes"));
        let start = file.stream_position().map_err(finalizer_io_error)?;
        match &chunk[0..4] {
            b"fmt " => {
                if size < 16 {
                    return Err(invalid_pcm_segment());
                }
                let mut value = [0u8; 16];
                file.read_exact(&mut value).map_err(finalizer_io_error)?;
                format = Some((
                    u16::from_le_bytes(value[0..2].try_into().expect("two bytes")),
                    u16::from_le_bytes(value[2..4].try_into().expect("two bytes")),
                    u32::from_le_bytes(value[4..8].try_into().expect("four bytes")),
                    u16::from_le_bytes(value[14..16].try_into().expect("two bytes")),
                ));
            }
            b"data" => {
                let len = usize::try_from(size).map_err(|_| invalid_pcm_segment())?;
                let mut value = vec![0u8; len];
                file.read_exact(&mut value).map_err(finalizer_io_error)?;
                data = Some(value);
            }
            _ => {}
        }
        file.seek(SeekFrom::Start(
            start.saturating_add(u64::from(size) + u64::from(size % 2)),
        ))
        .map_err(finalizer_io_error)?;
    }
    let (encoding, channels, rate, bits) = format.ok_or_else(invalid_pcm_segment)?;
    if encoding != 1
        || bits != 16
        || u32::from(channels) != expected_channels
        || rate != expected_rate
    {
        return Err(invalid_pcm_segment());
    }
    let data = data.ok_or_else(invalid_pcm_segment)?;
    let expected_samples = expected_frames.saturating_mul(u64::from(expected_channels));
    if u64::try_from(data.len()).unwrap_or(u64::MAX) != expected_samples.saturating_mul(2) {
        return Err(invalid_pcm_segment());
    }
    Ok(data
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect())
}

fn downmix(samples: &[i16], channels: u32) -> Result<Vec<f32>, CaptureError> {
    let channels = usize::try_from(channels).map_err(|_| invalid_pcm_segment())?;
    if channels == 0 || channels > 64 {
        return Err(invalid_pcm_segment());
    }
    Ok(samples
        .chunks_exact(channels)
        .map(|frame| {
            frame
                .iter()
                .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
                .sum::<f32>()
                / channels as f32
        })
        .collect())
}

fn timeline_frame_count(
    segment: &super::session::SegmentMetadata,
    target_rate: u32,
) -> Result<usize, CaptureError> {
    let clock_duration_ns = segment
        .clock_end_ns
        .checked_sub(segment.clock_start_ns)
        .filter(|duration| *duration > 0)
        .ok_or_else(|| {
            CaptureError::local(
                "invalid_segment_timeline",
                "desktop capture segment has an invalid shared-clock duration",
            )
        })?;
    let frames = clock_duration_ns
        .checked_mul(u64::from(target_rate))
        .and_then(|value| value.checked_div(1_000_000_000))
        .filter(|frames| *frames > 0)
        .ok_or_else(|| {
            CaptureError::local(
                "invalid_segment_timeline",
                "desktop capture segment is too short for the normalized timeline",
            )
        })?;
    usize::try_from(frames).map_err(|_| {
        CaptureError::local(
            "normalized_too_large",
            "desktop capture segment exceeds the normalized timeline bound",
        )
    })
}

fn linear_resample_to_len(samples: &[f32], output_len: usize) -> Vec<f32> {
    if samples.is_empty() || output_len == 0 {
        return Vec::new();
    }
    if samples.len() == output_len {
        return samples.to_vec();
    }
    if output_len == 1 || samples.len() == 1 {
        return vec![samples[0]; output_len];
    }
    let source_span = (samples.len() - 1) as f64;
    let output_span = (output_len - 1) as f64;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * source_span / output_span;
            let left = position.floor() as usize;
            let right = left.saturating_add(1).min(samples.len() - 1);
            let fraction = (position - left as f64) as f32;
            samples[left] * (1.0 - fraction) + samples[right] * fraction
        })
        .collect()
}

#[cfg(test)]
fn write_pcm16_wav(
    path: &Path,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> Result<(), CaptureError> {
    let data_bytes = u32::try_from(samples.len().saturating_mul(2))
        .map_err(|_| CaptureError::local("normalized_too_large", "normalized WAV is too large"))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(finalizer_io_error)?;
    write_pcm16_wav_header(&mut file, sample_rate, channels, data_bytes)?;
    let mut encoded = Vec::with_capacity(samples.len().saturating_mul(2));
    encoded.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    file.write_all(&encoded).map_err(finalizer_io_error)?;
    file.sync_all().map_err(finalizer_io_error)
}

fn write_pcm16_wav_header(
    file: &mut File,
    sample_rate: u32,
    channels: u16,
    data_bytes: u32,
) -> Result<(), CaptureError> {
    file.write_all(b"RIFF").map_err(finalizer_io_error)?;
    file.write_all(&data_bytes.saturating_add(36).to_le_bytes())
        .map_err(finalizer_io_error)?;
    file.write_all(b"WAVEfmt ").map_err(finalizer_io_error)?;
    file.write_all(&16u32.to_le_bytes())
        .map_err(finalizer_io_error)?;
    file.write_all(&1u16.to_le_bytes())
        .map_err(finalizer_io_error)?;
    file.write_all(&channels.to_le_bytes())
        .map_err(finalizer_io_error)?;
    file.write_all(&sample_rate.to_le_bytes())
        .map_err(finalizer_io_error)?;
    file.write_all(
        &sample_rate
            .saturating_mul(u32::from(channels))
            .saturating_mul(2)
            .to_le_bytes(),
    )
    .map_err(finalizer_io_error)?;
    file.write_all(&channels.saturating_mul(2).to_le_bytes())
        .map_err(finalizer_io_error)?;
    file.write_all(&16u16.to_le_bytes())
        .map_err(finalizer_io_error)?;
    file.write_all(b"data").map_err(finalizer_io_error)?;
    file.write_all(&data_bytes.to_le_bytes())
        .map_err(finalizer_io_error)?;
    Ok(())
}

fn hash_path(path: &Path) -> Result<(String, u64), CaptureError> {
    let mut file = File::open(path).map_err(finalizer_io_error)?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(finalizer_io_error)?;
        if count == 0 {
            break;
        }
        size = size.saturating_add(count as u64);
        digest.update(&buffer[..count]);
    }
    Ok((hex::encode(digest.finalize()), size))
}

fn invalid_pcm_segment() -> CaptureError {
    CaptureError::local(
        "invalid_pcm_segment",
        "desktop capture segment is not canonical PCM16 WAV",
    )
}

fn finalizer_io_error(_: std::io::Error) -> CaptureError {
    CaptureError::local(
        "capture_finalizer_io",
        "desktop capture finalizer could not persist normalized audio",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use chrono::DateTime;
    use tempfile::TempDir;

    use super::*;
    use crate::capture::model::{
        AudioInputSelection, ConsentAcknowledgements, MeetingSource, SourceAvailability,
    };
    use crate::capture::session::{
        CaptureStartContext, NativeCaptureLevels, NativeCaptureStream, SegmentCloseReason,
        SegmentMetadata,
    };

    #[test]
    fn normalized_timeline_uses_host_clock_duration_without_changing_raw_metadata() {
        let segment = SegmentMetadata {
            role: TrackRole::Microphone,
            sequence: 0,
            relative_path: "tracks/microphone-0000.wav".to_owned(),
            codec: "pcm_s16le".to_owned(),
            sample_rate: 48_000,
            channels: 1,
            frames_written: 48_000,
            clock_start_ns: 1_000_000_000,
            clock_end_ns: 2_100_000_000,
            sha256: "a".repeat(64),
        };
        assert_eq!(timeline_frame_count(&segment, 48_000).unwrap(), 52_800);
        assert_eq!(segment.frames_written, 48_000);

        let corrected = linear_resample_to_len(&[0.0, 0.5, 1.0], 5);
        assert_eq!(corrected, vec![0.0, 0.25, 0.5, 0.75, 1.0]);
    }

    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn set(&self, value: u64) {
            self.0.store(value, Ordering::SeqCst);
        }
    }

    impl CaptureClock for FakeClock {
        fn now_ns(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct SyntheticBackend {
        source_available: AtomicBool,
        secret_error: bool,
    }

    impl DesktopCaptureBackend for SyntheticBackend {
        fn preflight(&self, _plan: &CapturePlan) -> Result<CapturePreflight, CaptureError> {
            if self.secret_error {
                return Err(CaptureError::local(
                    "backend_secret",
                    "https://private.invalid?token=must-not-leak",
                ));
            }
            Ok(CapturePreflight {
                backend_available: true,
                microphone_permission: PermissionState::Granted,
                system_audio_permission: PermissionState::Granted,
                selected_source: if self.source_available.load(Ordering::SeqCst) {
                    SourceAvailability::Available
                } else {
                    SourceAvailability::Missing
                },
                warnings: Vec::new(),
            })
        }

        fn start(
            &self,
            context: CaptureStartContext,
        ) -> Result<Box<dyn NativeCaptureStream>, CaptureError> {
            Ok(Box::new(SyntheticStream {
                package: context.package_directory,
                roles: context.plan.required_roles().to_vec(),
                active_start: context.monotonic_started_ns,
                sequence: HashMap::new(),
            }))
        }
    }

    impl CaptureCatalog for SyntheticBackend {
        fn permissions(&self) -> Result<CapturePermissionsDto, CaptureError> {
            Ok(CapturePermissionsDto {
                microphone: PermissionState::Granted,
                screen_and_system_audio: PermissionState::Granted,
            })
        }

        fn sources(&self) -> Result<Vec<CaptureSourceDto>, CaptureError> {
            let available = self.source_available.load(Ordering::SeqCst);
            Ok(vec![
                CaptureSourceDto {
                    id: "synthetic-mic".to_owned(),
                    label: "Synthetic microphone".to_owned(),
                    kind: CaptureSourceKindDto::Microphone,
                    available: true,
                    is_default: false,
                },
                CaptureSourceDto {
                    id: "synthetic-app".to_owned(),
                    label: "Synthetic meeting".to_owned(),
                    kind: CaptureSourceKindDto::NativeApplication,
                    available,
                    is_default: false,
                },
                CaptureSourceDto {
                    id: "synthetic-browser".to_owned(),
                    label: "Synthetic browser".to_owned(),
                    kind: CaptureSourceKindDto::BrowserApplication,
                    available,
                    is_default: false,
                },
                CaptureSourceDto {
                    id: "synthetic-system".to_owned(),
                    label: "Synthetic system audio".to_owned(),
                    kind: CaptureSourceKindDto::SystemOutput,
                    available,
                    is_default: false,
                },
            ])
        }

        fn icon_data_url(&self, source_id: &str) -> Result<Option<String>, CaptureError> {
            Ok((source_id == "synthetic-app").then(|| {
                "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".to_owned()
            }))
        }
    }

    struct SyntheticStream {
        package: PathBuf,
        roles: Vec<TrackRole>,
        active_start: u64,
        sequence: HashMap<TrackRole, u32>,
    }

    impl NativeCaptureStream for SyntheticStream {
        fn close_segment(
            &mut self,
            _reason: SegmentCloseReason,
            clock_at_ns: u64,
        ) -> Result<Vec<SegmentMetadata>, CaptureError> {
            let frames = (clock_at_ns - self.active_start) / 10_000_000;
            let mut result = Vec::new();
            for role in &self.roles {
                let sequence = *self.sequence.entry(*role).or_default();
                let relative = format!("tracks/test-{}-{sequence}.wav", role.as_str());
                let samples = vec![
                    if *role == TrackRole::Microphone {
                        1_000
                    } else {
                        500
                    };
                    frames as usize
                ];
                write_pcm16_wav(&self.package.join(&relative), 100, 1, &samples)?;
                let (sha256, _) = hash_path(&self.package.join(&relative))?;
                result.push(SegmentMetadata {
                    role: *role,
                    sequence,
                    relative_path: relative,
                    codec: "pcm_s16le".to_owned(),
                    sample_rate: 100,
                    channels: 1,
                    frames_written: frames,
                    clock_start_ns: self.active_start,
                    clock_end_ns: clock_at_ns,
                    sha256,
                });
                self.sequence.insert(*role, sequence + 1);
            }
            Ok(result)
        }

        fn resume(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
            self.active_start = clock_at_ns;
            Ok(())
        }

        fn levels(&self) -> NativeCaptureLevels {
            NativeCaptureLevels {
                microphone: self.roles.contains(&TrackRole::Microphone).then_some(0.25),
                system: self.roles.contains(&TrackRole::System).then_some(0.5),
            }
        }
    }

    fn inbox(temp: &TempDir) -> Arc<Inbox> {
        let archive = temp.path().join("archive");
        fs::create_dir_all(&archive).unwrap();
        Arc::new(Inbox::open(temp.path().join("app-data"), archive).unwrap())
    }

    fn voice_plan() -> CapturePlan {
        CapturePlan {
            mode: CaptureMode::VoiceMemo,
            platform: current_platform().unwrap_or(Platform::Macos),
            target: CaptureTarget::VoiceMemo {
                microphone: AudioInputSelection {
                    id: "synthetic-mic".to_owned(),
                    label: "Synthetic microphone".to_owned(),
                },
            },
            consent: ConsentAcknowledgements {
                microphone: true,
                selected_source: true,
                whole_browser_warning: true,
                all_system_audio: true,
            },
        }
    }

    fn meeting_plan() -> CapturePlan {
        let mut plan = voice_plan();
        plan.mode = CaptureMode::Meeting;
        plan.target = CaptureTarget::Meeting {
            microphone: AudioInputSelection {
                id: "synthetic-mic".to_owned(),
                label: "Synthetic microphone".to_owned(),
            },
            source: MeetingSource::NativeApplication {
                id: "synthetic-app".to_owned(),
                label: "Synthetic meeting".to_owned(),
            },
        };
        plan
    }

    fn state(
        inbox: Arc<Inbox>,
        backend: Arc<SyntheticBackend>,
        clock: Arc<FakeClock>,
    ) -> CaptureCommandState {
        let native: Arc<dyn DesktopCaptureBackend> = backend.clone();
        let catalog: Arc<dyn CaptureCatalog> = backend;
        CaptureCommandState::with_components(
            inbox,
            Some(native),
            Some(catalog),
            Arc::new(PcmTimelineFinalizer),
            clock,
        )
        .unwrap()
    }

    fn start_for_test(
        state: &CaptureCommandState,
        plan: CapturePlan,
    ) -> Result<CaptureStatusDto, CaptureCommandError> {
        validate_plan_sources(&plan, state.catalog.as_deref())?;
        let mut active = state.active()?;
        if active.is_some() {
            return Err(CaptureCommandError::new("capture_already_active"));
        }
        let started = state.clock.now_ns();
        let session = CaptureSession::start(
            Arc::clone(&state.inbox),
            state.backend.as_deref(),
            plan,
            DateTime::parse_from_rfc3339("2026-09-02T09:00:00-07:00").unwrap(),
            started,
        )?;
        *active = Some(session);
        Ok(state.status_locked(active.as_ref(), false))
    }

    #[test]
    fn command_state_serializes_start_pause_resume_stop_and_returns_ready_id() {
        let temp = TempDir::new().unwrap();
        let backend = Arc::new(SyntheticBackend {
            source_available: AtomicBool::new(true),
            secret_error: false,
        });
        let clock = Arc::new(FakeClock(AtomicU64::new(0)));
        let state = state(inbox(&temp), backend, Arc::clone(&clock));
        let started = start_for_test(&state, voice_plan()).unwrap();
        assert_eq!(started.phase, Some(CapturePhase::Recording));
        assert_eq!(started.microphone_level, Some(0.25));
        assert_eq!(started.system_level, None);
        assert_eq!(
            started.microphone_label.as_deref(),
            Some("Synthetic microphone")
        );
        assert_eq!(started.source_label, None);
        assert!(started.warnings.is_empty());
        assert_eq!(started.gap_duration_ms, 0);
        assert_eq!(
            start_for_test(&state, voice_plan()).unwrap_err().code,
            "capture_already_active"
        );
        clock.set(1_000_000_000);
        let tray = state.tray_snapshot().unwrap().unwrap();
        assert_eq!(tray.duration_ms, 1_000);
        assert_eq!(tray.source_label, "Synthetic microphone");
        assert_eq!(
            state.toggle_pause_for_tray().unwrap().phase,
            Some(CapturePhase::Paused)
        );
        clock.set(2_000_000_000);
        let resumed = state.toggle_pause_for_tray().unwrap();
        assert_eq!(resumed.phase, Some(CapturePhase::Recording));
        assert_eq!(resumed.gap_duration_ms, 1_000);
        clock.set(3_000_000_000);
        let envelope = state.stop_for_tray().unwrap().envelope;
        assert_eq!(envelope.job.state, JobState::Ready);
        assert_eq!(
            envelope.normalized_audio.as_deref(),
            Some("derived/mixed.wav")
        );
        assert_eq!(
            envelope.recording_id.to_string(),
            started.recording_id.unwrap()
        );
    }

    #[test]
    fn source_disappearance_blocks_start_without_fallback() {
        let temp = TempDir::new().unwrap();
        let backend = Arc::new(SyntheticBackend {
            source_available: AtomicBool::new(false),
            secret_error: false,
        });
        let clock = Arc::new(FakeClock(AtomicU64::new(0)));
        let state = state(inbox(&temp), backend, clock);
        assert_eq!(
            start_for_test(&state, meeting_plan()).unwrap_err().code,
            "selected_source_missing"
        );
        assert!(state.active().unwrap().is_none());
    }

    #[test]
    fn source_icon_request_is_closed_catalog_bound_and_kill_switch_guarded() {
        let backend = SyntheticBackend {
            source_available: AtomicBool::new(true),
            secret_error: false,
        };
        let icon = capture_source_icon_for_catalog(
            Some(&backend),
            true,
            CaptureSourceIconRequest {
                source_id: "synthetic-app".to_owned(),
            },
        )
        .unwrap()
        .icon_data_url
        .unwrap();
        assert!(icon.starts_with("data:image/png;base64,"));
        assert_eq!(
            capture_source_icon_for_catalog(
                Some(&backend),
                true,
                CaptureSourceIconRequest {
                    source_id: "synthetic-mic".to_owned(),
                },
            )
            .unwrap_err()
            .code,
            "selected_source_missing"
        );
        assert_eq!(
            capture_source_icon_for_catalog(
                Some(&backend),
                false,
                CaptureSourceIconRequest {
                    source_id: "synthetic-app".to_owned(),
                },
            )
            .unwrap_err()
            .code,
            "recording_disabled"
        );
        assert!(
            serde_json::from_value::<CaptureSourceIconRequest>(serde_json::json!({
                "sourceId": "synthetic-app",
                "url": "file:///private/icon.png"
            }))
            .is_err()
        );
    }

    #[test]
    fn release_kill_switches_block_recording_and_whole_browser_natively() {
        let mut features = crate::features::RuntimeFeatures {
            recording: false,
            audio_import: true,
            direct_processing: true,
            browser_capture: true,
            local_stt: false,
            local_qwen_candidate: false,
            local_speakerkit_candidate: false,
            local_moss_candidate: false,
        };
        assert_eq!(
            require_capture_feature(&features, &voice_plan())
                .unwrap_err()
                .code,
            "recording_disabled"
        );

        features.recording = true;
        features.browser_capture = false;
        let mut browser = meeting_plan();
        browser.target = CaptureTarget::Meeting {
            microphone: AudioInputSelection {
                id: "synthetic-mic".to_owned(),
                label: "Synthetic microphone".to_owned(),
            },
            source: MeetingSource::WholeBrowser {
                id: "synthetic-browser".to_owned(),
                label: "Synthetic browser".to_owned(),
            },
        };
        assert_eq!(
            require_capture_feature(&features, &browser)
                .unwrap_err()
                .code,
            "browser_capture_disabled"
        );
    }

    #[test]
    fn permission_commands_are_closed_explicit_and_kill_switch_guarded() {
        let request: CapturePermissionRequest = serde_json::from_value(serde_json::json!({
            "microphone": true,
            "screenAndSystemAudio": false
        }))
        .unwrap();
        assert!(validate_permission_request(&request, true).is_ok());
        assert_eq!(
            validate_permission_request(&request, false)
                .unwrap_err()
                .code,
            "recording_disabled"
        );
        assert_eq!(
            validate_permission_request(
                &CapturePermissionRequest {
                    microphone: false,
                    screen_and_system_audio: false,
                },
                true,
            )
            .unwrap_err()
            .code,
            "invalid_permission_request"
        );
        assert!(
            serde_json::from_value::<CapturePermissionRequest>(serde_json::json!({
                "microphone": true,
                "screenAndSystemAudio": false,
                "url": "https://attacker.invalid"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CapturePermissionSettingsRequest>(serde_json::json!({
                "kind": "arbitrary_url"
            }))
            .is_err()
        );
        let microphone = capture_permission_settings_url(CapturePermissionSettingsKind::Microphone)
            .expect("desktop settings URL");
        let screen =
            capture_permission_settings_url(CapturePermissionSettingsKind::ScreenAndSystemAudio)
                .expect("desktop settings URL");
        assert!(!microphone.starts_with("http"));
        assert!(!screen.starts_with("http"));
    }

    #[test]
    fn interrupted_session_recovers_and_finalizes_only_closed_segments() {
        let temp = TempDir::new().unwrap();
        let shared_inbox = inbox(&temp);
        let backend = Arc::new(SyntheticBackend {
            source_available: AtomicBool::new(true),
            secret_error: false,
        });
        let first_clock = Arc::new(FakeClock(AtomicU64::new(0)));
        let first = state(
            Arc::clone(&shared_inbox),
            Arc::clone(&backend),
            Arc::clone(&first_clock),
        );
        let started = start_for_test(&first, voice_plan()).unwrap();
        first_clock.set(1_000_000_000);
        first
            .active()
            .unwrap()
            .as_mut()
            .unwrap()
            .pause(first_clock.now_ns())
            .unwrap();
        drop(first);

        let recovered_clock = Arc::new(FakeClock(AtomicU64::new(2_000_000_000)));
        let recovered = state(shared_inbox, backend, Arc::clone(&recovered_clock));
        assert_eq!(
            recovered
                .active()
                .unwrap()
                .as_ref()
                .unwrap()
                .snapshot()
                .phase,
            CapturePhase::Interrupted
        );
        let envelope = recovered
            .active()
            .unwrap()
            .as_mut()
            .unwrap()
            .stop(recovered_clock.now_ns(), recovered.finalizer.as_ref())
            .unwrap();
        assert_eq!(
            envelope.recording_id.to_string(),
            started.recording_id.unwrap()
        );
    }

    #[test]
    fn public_errors_redact_backend_details_and_action_dto_rejects_clock() {
        let temp = TempDir::new().unwrap();
        let backend = Arc::new(SyntheticBackend {
            source_available: AtomicBool::new(true),
            secret_error: true,
        });
        let clock = Arc::new(FakeClock(AtomicU64::new(0)));
        let state = state(inbox(&temp), backend, clock);
        let error = CaptureSession::preflight(state.backend.as_deref(), &voice_plan())
            .map_err(CaptureCommandError::from)
            .unwrap_err();
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(!serialized.contains("private.invalid"));
        assert!(!serialized.contains("must-not-leak"));
        assert!(serde_json::from_str::<CaptureActionRequest>("{}").is_ok());
        assert!(serde_json::from_str::<CaptureActionRequest>(r#"{"clockAtNs":42}"#).is_err());
    }
}
