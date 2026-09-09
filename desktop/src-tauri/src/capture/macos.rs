//! macOS 13+ capture boundary.
//!
//! ScreenCaptureKit owns selected-application/all-system output. Microphone
//! capture deliberately uses a separate CoreAudio input stream through `cpal`:
//! `SCStreamConfiguration.captureMicrophone` is macOS 15+, while this backend's
//! deployment floor is macOS 13. Keeping one microphone path also preserves
//! separate queues/tracks on every supported OS release.

use std::collections::{HashMap, VecDeque};
use std::ffi::{c_char, c_void, CString};
use std::fs::{self, File, OpenOptions};
use std::io::Cursor;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use objc2::runtime::{AnyObject, Bool};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSRunningApplication,
};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{NSDictionary, NSProcessInfo};
use screencapturekit::audio_devices::AudioInputDevice;
use screencapturekit::cm::CMSampleBufferExt;
use screencapturekit::prelude::*;
use sha2::{Digest, Sha256};

use crate::ingest::envelope::{Platform, TrackRole};

use super::model::{
    CaptureMode, CapturePlan, CapturePreflight, CaptureTarget, MeetingSource, PermissionState,
    SourceAvailability,
};
use super::session::{
    CaptureError, CaptureStartContext, DesktopCaptureBackend, NativeCaptureLevels,
    NativeCaptureStream,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacPermissionSnapshot {
    pub microphone: PermissionState,
    pub screen_and_system_audio: PermissionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacCaptureSourceKind {
    NativeApplication,
    BrowserApplication,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacCaptureSource {
    /// Stable adapter identifier used to build an SCContentFilter later.
    pub id: String,
    pub label: String,
    pub bundle_identifier: Option<String>,
    pub process_id: Option<u32>,
    pub kind: MacCaptureSourceKind,
    /// ScreenCaptureKit cannot preflight audibility without starting capture.
    /// Native enumeration therefore returns `None`; injected/virtual bridges
    /// may provide a known value.
    pub currently_audible: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacCaptureFilter {
    /// Selected ScreenCaptureKit application identifier. None means either
    /// microphone-only or all-system output, depending on `capture_system_audio`.
    pub selected_application_id: Option<String>,
    /// AVFAudio input identifier.
    pub microphone_id: String,
    pub capture_system_audio: bool,
    pub capture_microphone: bool,
    pub exclude_echowall_audio: bool,
}

pub trait MacNativeBridge: Send + Sync {
    /// Read current TCC state only; implementations must not prompt here.
    fn current_permissions(&self) -> Result<MacPermissionSnapshot, CaptureError>;

    /// Enumerate SCShareableContent-style application sources without starting
    /// an SCStream.
    fn enumerate_sources(&self) -> Result<Vec<MacCaptureSource>, CaptureError>;

    fn meeting_capture_available(&self) -> bool {
        true
    }

    fn icon_data_url(&self, _source: &MacCaptureSource) -> Result<Option<String>, CaptureError> {
        Ok(None)
    }

    /// This is called only from the explicit permission action. Preflight and
    /// source enumeration must remain prompt-free.
    fn request_permissions(
        &self,
        microphone: bool,
        screen_and_system_audio: bool,
    ) -> Result<MacPermissionSnapshot, CaptureError> {
        let _ = (microphone, screen_and_system_audio);
        self.current_permissions()
    }

    /// The future concrete bridge creates SCContentFilter/SCStream and an
    /// AVFAudio microphone input only after the shared core accepted consent.
    fn start_filtered_stream(
        &self,
        context: CaptureStartContext,
        filter: MacCaptureFilter,
    ) -> Result<Box<dyn NativeCaptureStream>, CaptureError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacMicrophoneSource {
    pub id: String,
    pub label: String,
    pub is_default: bool,
}

#[derive(Debug, Default)]
pub struct MacOsNativeBridge;

impl MacOsNativeBridge {
    pub fn enumerate_microphones() -> Vec<MacMicrophoneSource> {
        AudioInputDevice::list()
            .into_iter()
            .map(|device| MacMicrophoneSource {
                id: device.id,
                label: device.name,
                is_default: device.is_default,
            })
            .collect()
    }
}

const MAX_APPLICATION_SOURCES: usize = 256;
const MAX_NATIVE_ICON_TIFF_BYTES: usize = 64 * 1024 * 1024;
const MAX_NATIVE_ICON_PNG_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_ICON_PNG_BYTES: usize = 64 * 1024;
const SOURCE_ICON_PIXELS: u32 = 32;

fn application_icon_data_url(process_id: i32) -> Option<String> {
    let application = NSRunningApplication::runningApplicationWithProcessIdentifier(process_id)?;
    let icon = application.icon()?;
    let tiff = icon.TIFFRepresentation()?;
    if tiff.length() == 0 || tiff.length() > MAX_NATIVE_ICON_TIFF_BYTES {
        return None;
    }
    let bitmap = NSBitmapImageRep::imageRepWithData(&tiff)?;
    let properties = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
    let png = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
    }?;
    bounded_icon_data_url(&png.to_vec())
}

fn bounded_icon_data_url(png: &[u8]) -> Option<String> {
    if png.is_empty() || png.len() > MAX_NATIVE_ICON_PNG_BYTES {
        return None;
    }
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(2_048);
    limits.max_image_height = Some(2_048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    encode_icon_data_url(reader.decode().ok()?)
}

fn encode_icon_data_url(image: DynamicImage) -> Option<String> {
    let icon = image.thumbnail(SOURCE_ICON_PIXELS, SOURCE_ICON_PIXELS);
    let mut encoded = Cursor::new(Vec::new());
    icon.write_to(&mut encoded, ImageFormat::Png).ok()?;
    let encoded = encoded.into_inner();
    if encoded.is_empty() || encoded.len() > MAX_SOURCE_ICON_PNG_BYTES {
        return None;
    }
    Some(format!(
        "data:image/png;base64,{}",
        BASE64_STANDARD.encode(encoded)
    ))
}

pub struct MacOsCaptureAdapter<B> {
    bridge: Arc<B>,
}

impl<B> MacOsCaptureAdapter<B>
where
    B: MacNativeBridge,
{
    pub fn new(bridge: Arc<B>) -> Self {
        Self { bridge }
    }

    pub fn permission_snapshot(&self) -> Result<MacPermissionSnapshot, CaptureError> {
        self.bridge.current_permissions()
    }

    pub fn enumerate_capture_sources(&self) -> Result<Vec<MacCaptureSource>, CaptureError> {
        self.bridge.enumerate_sources()
    }

    pub fn capture_source_icon(&self, id: &str) -> Result<Option<String>, CaptureError> {
        let sources = self.bridge.enumerate_sources()?;
        let source = sources
            .iter()
            .find(|source| source.id == id)
            .ok_or_else(|| {
                CaptureError::local(
                    "selected_source_missing",
                    "selected macOS application is no longer available",
                )
            })?;
        self.bridge.icon_data_url(source)
    }

    pub fn request_permission_snapshot(
        &self,
        microphone: bool,
        screen_and_system_audio: bool,
    ) -> Result<MacPermissionSnapshot, CaptureError> {
        if !microphone && !screen_and_system_audio {
            return Err(CaptureError::local(
                "invalid_permission_request",
                "capture permission request selected no permission",
            ));
        }
        self.bridge
            .request_permissions(microphone, screen_and_system_audio)
    }

    fn filter(plan: &CapturePlan) -> Result<MacCaptureFilter, CaptureError> {
        plan.validate()?;
        if plan.platform != Platform::Macos {
            return Err(CaptureError::local(
                "platform_mismatch",
                "macOS adapter received a non-macOS capture plan",
            ));
        }
        let (microphone_id, selected_application_id, capture_system_audio) = match &plan.target {
            CaptureTarget::VoiceMemo { microphone } => (microphone.id.clone(), None, false),
            CaptureTarget::Meeting { microphone, source } => {
                let id = match source {
                    MeetingSource::NativeApplication { id, .. }
                    | MeetingSource::WholeBrowser { id, .. } => id.clone(),
                };
                (microphone.id.clone(), Some(id), true)
            }
            CaptureTarget::SystemCapture { microphone, .. } => (microphone.id.clone(), None, true),
        };
        Ok(MacCaptureFilter {
            selected_application_id,
            microphone_id,
            capture_system_audio,
            capture_microphone: true,
            exclude_echowall_audio: plan.mode != CaptureMode::VoiceMemo,
        })
    }
}

impl<B> DesktopCaptureBackend for MacOsCaptureAdapter<B>
where
    B: MacNativeBridge + 'static,
{
    fn preflight(&self, plan: &CapturePlan) -> Result<CapturePreflight, CaptureError> {
        plan.validate()?;
        if plan.platform != Platform::Macos {
            return Err(CaptureError::local(
                "platform_mismatch",
                "macOS adapter received a non-macOS capture plan",
            ));
        }
        let permissions = self.bridge.current_permissions()?;
        let mut warnings = Vec::new();
        let (system_audio, source) = match &plan.target {
            CaptureTarget::VoiceMemo { .. } => {
                (PermissionState::NotRequired, SourceAvailability::Available)
            }
            CaptureTarget::Meeting { source, .. } => {
                if !self.bridge.meeting_capture_available() {
                    return Ok(CapturePreflight {
                        backend_available: false,
                        microphone_permission: permissions.microphone,
                        system_audio_permission: permissions.screen_and_system_audio,
                        selected_source: SourceAvailability::Unavailable,
                        warnings: vec![
                            "Selected-process audio capture requires macOS 14.2 or later"
                                .to_owned(),
                        ],
                    });
                }
                if permissions.screen_and_system_audio != PermissionState::Granted {
                    return Ok(CapturePreflight {
                        backend_available: true,
                        microphone_permission: permissions.microphone,
                        system_audio_permission: permissions.screen_and_system_audio,
                        selected_source: SourceAvailability::Unavailable,
                        warnings: vec![
                            "Screen Recording permission is required before source enumeration"
                                .to_owned(),
                        ],
                    });
                }
                let (expected_id, expected_kind) = match source {
                    MeetingSource::NativeApplication { id, .. } => {
                        (id, MacCaptureSourceKind::NativeApplication)
                    }
                    MeetingSource::WholeBrowser { id, .. } => {
                        warnings.push(
                            "Whole-browser capture includes audio from other tabs and windows"
                                .to_owned(),
                        );
                        (id, MacCaptureSourceKind::BrowserApplication)
                    }
                };
                let sources = self.bridge.enumerate_sources()?;
                let selected = sources.iter().find(|candidate| {
                    candidate.id == *expected_id && candidate.kind == expected_kind
                });
                let availability = match selected {
                    Some(candidate) if candidate.currently_audible == Some(false) => {
                        warnings.push("selected source is currently silent".to_owned());
                        SourceAvailability::Silent
                    }
                    Some(candidate) => {
                        if candidate.currently_audible.is_none() {
                            warnings.push(
                                "Source audibility is checked only after explicit start".to_owned(),
                            );
                        }
                        SourceAvailability::Available
                    }
                    None => SourceAvailability::Unavailable,
                };
                (permissions.screen_and_system_audio, availability)
            }
            CaptureTarget::SystemCapture { .. } => (
                permissions.screen_and_system_audio,
                SourceAvailability::Available,
            ),
        };
        Ok(CapturePreflight {
            backend_available: true,
            microphone_permission: permissions.microphone,
            system_audio_permission: system_audio,
            selected_source: source,
            warnings,
        })
    }

    fn start(
        &self,
        context: CaptureStartContext,
    ) -> Result<Box<dyn NativeCaptureStream>, CaptureError> {
        let filter = Self::filter(&context.plan)?;
        self.bridge.start_filtered_stream(context, filter)
    }
}

impl MacNativeBridge for MacOsNativeBridge {
    fn meeting_capture_available(&self) -> bool {
        process_tap_available()
    }

    fn current_permissions(&self) -> Result<MacPermissionSnapshot, CaptureError> {
        Ok(MacPermissionSnapshot {
            microphone: microphone_permission(),
            screen_and_system_audio: if unsafe { CGPreflightScreenCaptureAccess() } {
                PermissionState::Granted
            } else {
                PermissionState::Denied
            },
        })
    }

    fn enumerate_sources(&self) -> Result<Vec<MacCaptureSource>, CaptureError> {
        let content = SCShareableContent::get().map_err(|_| {
            CaptureError::local(
                "macos_shareable_content_unavailable",
                "macOS shareable applications are unavailable without Screen Recording access",
            )
        })?;
        let snapshot = content.snapshot().ok_or_else(|| {
            CaptureError::local(
                "macos_shareable_content_invalid",
                "macOS returned an invalid shareable-content snapshot",
            )
        })?;
        if snapshot.truncation.applications {
            return Err(CaptureError::local(
                "macos_source_list_truncated",
                "macOS application enumeration exceeded its safety bound",
            ));
        }
        if snapshot.applications.len() > MAX_APPLICATION_SOURCES {
            return Err(CaptureError::local(
                "macos_source_list_truncated",
                "macOS application enumeration exceeded its safety bound",
            ));
        }
        Ok(snapshot
            .applications
            .into_iter()
            .filter(|application| application.process_id > 0)
            .map(|application| {
                let process_id = application.process_id;
                let browser = is_browser_bundle(&application.bundle_identifier);
                MacCaptureSource {
                    id: application_id(process_id),
                    label: application.application_name,
                    bundle_identifier: Some(application.bundle_identifier),
                    process_id: u32::try_from(process_id).ok(),
                    kind: if browser {
                        MacCaptureSourceKind::BrowserApplication
                    } else {
                        MacCaptureSourceKind::NativeApplication
                    },
                    // ScreenCaptureKit exposes shareability, not an audibility
                    // probe. Silence is detected from audio samples after the
                    // user explicitly starts capture.
                    currently_audible: None,
                }
            })
            .collect())
    }

    fn icon_data_url(&self, source: &MacCaptureSource) -> Result<Option<String>, CaptureError> {
        let process_id = source
            .process_id
            .and_then(|value| i32::try_from(value).ok());
        Ok(process_id.and_then(application_icon_data_url))
    }

    fn request_permissions(
        &self,
        microphone: bool,
        screen_and_system_audio: bool,
    ) -> Result<MacPermissionSnapshot, CaptureError> {
        if microphone && microphone_permission() == PermissionState::NotDetermined {
            let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
                return Err(CaptureError::local(
                    "macos_microphone_permission_unavailable",
                    "macOS microphone permission API is unavailable",
                ));
            };
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let completion = block2::RcBlock::new(move |granted: Bool| {
                let _ = sender.send(granted.as_bool());
            });
            unsafe {
                AVCaptureDevice::requestAccessForMediaType_completionHandler(
                    media_type,
                    &completion,
                )
            };
            receiver
                .recv_timeout(std::time::Duration::from_secs(120))
                .map_err(|_| {
                    CaptureError::local(
                        "macos_microphone_permission_timeout",
                        "macOS microphone permission request did not finish",
                    )
                })?;
        }
        if screen_and_system_audio && !unsafe { CGPreflightScreenCaptureAccess() } {
            let _ = unsafe { CGRequestScreenCaptureAccess() };
        }
        self.current_permissions()
    }

    fn start_filtered_stream(
        &self,
        context: CaptureStartContext,
        filter: MacCaptureFilter,
    ) -> Result<Box<dyn NativeCaptureStream>, CaptureError> {
        let selected_application_pid = filter
            .selected_application_id
            .as_deref()
            .map(parse_application_id)
            .transpose()?;
        if selected_application_pid == i32::try_from(std::process::id()).ok() {
            return Err(CaptureError::local(
                "macos_self_capture_forbidden",
                "EchoWall cannot be selected as its own meeting-audio source",
            ));
        }
        let shared = Arc::new(SharedCapture::new(&context)?);
        let microphone = start_microphone(&filter.microphone_id, Arc::clone(&shared))?;
        let system = if filter.capture_system_audio {
            Some(start_system_audio(&filter, Arc::clone(&shared))?)
        } else {
            None
        };
        let process_tap_watchdog = matches!(system, Some(MacSystemAudioStream::ProcessTap(_)))
            .then(|| ProcessTapWatchdog::new(context.monotonic_started_ns));
        Ok(Box::new(MacNativeStream {
            shared,
            microphone,
            system,
            process_tap_watchdog,
            microphone_id: filter.microphone_id,
            selected_application_pid,
            paused: false,
            stopped: false,
        }))
    }
}

pub type MacOsCaptureBackend = MacOsCaptureAdapter<MacOsNativeBridge>;

impl Default for MacOsCaptureAdapter<MacOsNativeBridge> {
    fn default() -> Self {
        Self::new(Arc::new(MacOsNativeBridge))
    }
}

struct MacNativeStream {
    shared: Arc<SharedCapture>,
    microphone: cpal::Stream,
    system: Option<MacSystemAudioStream>,
    process_tap_watchdog: Option<ProcessTapWatchdog>,
    microphone_id: String,
    selected_application_pid: Option<i32>,
    paused: bool,
    stopped: bool,
}

enum MacSystemAudioStream {
    ScreenCaptureKit(SCStream),
    ProcessTap(ProcessTapStream),
}

impl MacSystemAudioStream {
    fn pause(&mut self) -> Result<(), CaptureError> {
        match self {
            Self::ScreenCaptureKit(stream) => stream.stop_capture().map_err(mac_stream_error),
            Self::ProcessTap(stream) => stream.pause(),
        }
    }

    fn resume(&mut self) -> Result<(), CaptureError> {
        match self {
            Self::ScreenCaptureKit(stream) => stream.start_capture().map_err(mac_stream_error),
            Self::ProcessTap(stream) => stream.resume(),
        }
    }
}

impl NativeCaptureStream for MacNativeStream {
    fn close_segment(
        &mut self,
        reason: super::session::SegmentCloseReason,
        _clock_at_ns: u64,
    ) -> Result<Vec<super::session::SegmentMetadata>, CaptureError> {
        if matches!(
            reason,
            super::session::SegmentCloseReason::Pause
                | super::session::SegmentCloseReason::SourceLoss
                | super::session::SegmentCloseReason::Stop
        ) {
            self.microphone.pause().map_err(mac_audio_error)?;
            if let Some(stream) = self.system.as_mut() {
                stream.pause()?;
            }
            self.paused = !matches!(reason, super::session::SegmentCloseReason::Stop);
            self.stopped = matches!(reason, super::session::SegmentCloseReason::Stop);
        }
        self.shared.close_segments()
    }

    fn resume(&mut self, _clock_at_ns: u64) -> Result<(), CaptureError> {
        if self.stopped {
            return Err(CaptureError::local(
                "macos_stream_stopped",
                "a stopped macOS capture stream cannot resume",
            ));
        }
        if self.paused {
            if let Some(stream) = self.system.as_mut() {
                stream.resume()?;
            }
            if let Some(watchdog) = self.process_tap_watchdog.as_mut() {
                watchdog.reset(self.shared.clock.now_ns());
            }
            if let Err(error) = self.microphone.play() {
                if let Some(stream) = self.system.as_mut() {
                    let _ = stream.pause();
                }
                return Err(mac_audio_error(error));
            }
            self.paused = false;
        }
        Ok(())
    }

    fn take_signals(&mut self) -> Result<Vec<super::session::NativeCaptureSignal>, CaptureError> {
        if let (Some(watchdog), Some(MacSystemAudioStream::ProcessTap(stream))) =
            (self.process_tap_watchdog.as_mut(), self.system.as_mut())
        {
            let now_ns = self.shared.clock.now_ns();
            let last_callback_ns = self.shared.last_callback_ns(TrackRole::System);
            match watchdog.poll(now_ns, last_callback_ns) {
                ProcessTapWatchdogAction::None => {}
                ProcessTapWatchdogAction::Restart { inactive_since_ns } => {
                    push_gap(
                        &self.shared.signals,
                        inactive_since_ns,
                        now_ns,
                        "macos_process_tap_callback_stalled",
                        "selected system source stopped producing audio callbacks; capture restarted",
                    );
                    if stream.pause().and_then(|_| stream.resume()).is_err() {
                        watchdog.fail();
                        self.shared.source_lost(
                            "macos_process_tap_restart_failed",
                            "selected system-audio capture could not be restarted",
                        );
                    }
                }
                ProcessTapWatchdogAction::SourceLost => self.shared.source_lost(
                    "macos_process_tap_no_audio_callbacks",
                    "selected system source produced no audio callbacks after one restart",
                ),
            }
        }
        let still_present = AudioInputDevice::list()
            .iter()
            .any(|device| device.id == self.microphone_id);
        if !still_present {
            self.shared.source_lost(
                "macos_microphone_device_changed",
                "selected microphone was disconnected or replaced",
            );
        }
        if self
            .selected_application_pid
            .is_some_and(|pid| !application_process_is_running(pid))
        {
            self.shared.source_lost(
                "macos_capture_source_exited",
                "selected macOS application exited",
            );
            self.selected_application_pid = None;
        }
        Ok(self.shared.take_signals())
    }

    fn levels(&self) -> NativeCaptureLevels {
        self.shared.levels()
    }
}

impl Drop for MacNativeStream {
    fn drop(&mut self) {
        let _ = self.microphone.pause();
        if let Some(stream) = self.system.as_mut() {
            let _ = stream.pause();
        }
    }
}

struct CaptureClock {
    origin: Instant,
    monotonic_origin_ns: u64,
    native_origin_ns: Option<u64>,
}

impl CaptureClock {
    fn now_ns(&self) -> u64 {
        self.monotonic_origin_ns
            .saturating_add(u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX))
    }

    fn map_native_ns(&self, native_ns: u64) -> Option<u64> {
        native_ns
            .checked_sub(self.native_origin_ns?)
            .and_then(|delta| self.monotonic_origin_ns.checked_add(delta))
    }
}

struct SharedCapture {
    clock: CaptureClock,
    package: PathBuf,
    tracks: HashMap<TrackRole, Mutex<TrackRecorder>>,
    levels: Mutex<HashMap<TrackRole, RecentLevel>>,
    callback_activity: Mutex<HashMap<TrackRole, u64>>,
    signals: Mutex<VecDeque<super::session::NativeCaptureSignal>>,
}

#[derive(Debug, Clone, Copy)]
struct RecentLevel {
    rms: f32,
    clock_at_ns: u64,
}

impl SharedCapture {
    fn new(context: &CaptureStartContext) -> Result<Self, CaptureError> {
        let tracks_directory = context.package_directory.join("tracks");
        fs::create_dir_all(&tracks_directory).map_err(|_| {
            CaptureError::local(
                "macos_capture_directory_failed",
                "macOS capture track directory could not be created",
            )
        })?;
        let tracks: HashMap<TrackRole, Mutex<TrackRecorder>> = context
            .plan
            .required_roles()
            .iter()
            .copied()
            .map(|role| {
                (
                    role,
                    Mutex::new(TrackRecorder::new_at(role, context.monotonic_started_ns)),
                )
            })
            .collect();
        let initial_levels = tracks
            .keys()
            .copied()
            .map(|role| {
                (
                    role,
                    RecentLevel {
                        rms: 0.0,
                        clock_at_ns: context.monotonic_started_ns,
                    },
                )
            })
            .collect();
        Ok(Self {
            clock: CaptureClock {
                origin: Instant::now(),
                monotonic_origin_ns: context.monotonic_started_ns,
                native_origin_ns: native_clock_ns(),
            },
            package: context.package_directory.clone(),
            tracks,
            levels: Mutex::new(initial_levels),
            callback_activity: Mutex::new(HashMap::new()),
            signals: Mutex::new(VecDeque::new()),
        })
    }

    fn push_i16_at(
        &self,
        role: TrackRole,
        samples: &[i16],
        sample_rate: u32,
        channels: u16,
        native_start_ns: Option<u64>,
    ) {
        let Some(track) = self.tracks.get(&role) else {
            return;
        };
        let frames = samples.len() / usize::from(channels);
        if frames == 0 {
            return;
        }
        let duration_ns = u64::try_from(frames)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000_000_000)
            / u64::from(sample_rate);
        let start_ns = native_start_ns
            .and_then(|timestamp| self.clock.map_native_ns(timestamp))
            .unwrap_or_else(|| self.clock.now_ns().saturating_sub(duration_ns));
        let end_ns = start_ns.saturating_add(duration_ns);
        let silent = samples.iter().all(|sample| sample.unsigned_abs() <= 2);
        let rms = normalized_rms(samples);
        if let Ok(mut levels) = self.levels.lock() {
            levels.insert(
                role,
                RecentLevel {
                    rms,
                    clock_at_ns: end_ns,
                },
            );
        }
        if let Ok(mut activity) = self.callback_activity.lock() {
            activity.insert(role, end_ns);
        }
        if let Ok(mut track) = track.lock() {
            if let Err(error) = track.write(
                &self.package,
                samples,
                sample_rate,
                channels,
                start_ns,
                end_ns,
                silent,
                &self.signals,
            ) {
                self.source_lost(error.code, error.message);
            }
        }
    }

    fn close_segments(&self) -> Result<Vec<super::session::SegmentMetadata>, CaptureError> {
        let mut result = Vec::new();
        for track in self.tracks.values() {
            let mut track = track.lock().map_err(|_| {
                CaptureError::local(
                    "macos_track_lock_failed",
                    "macOS capture track state is unavailable",
                )
            })?;
            result.extend(track.finalize_closed(&self.package)?);
            if let Some(segment) = track.close(&self.package)? {
                result.push(segment);
            }
        }
        result.sort_by_key(|segment| (role_order(segment.role), segment.sequence));
        Ok(result)
    }

    fn source_lost(&self, code: &'static str, message: impl Into<String>) {
        if let Ok(mut signals) = self.signals.lock() {
            signals.push_back(super::session::NativeCaptureSignal::SourceLost {
                clock_at_ns: self.clock.now_ns(),
                code,
                message: message.into(),
            });
        }
    }

    fn take_signals(&self) -> Vec<super::session::NativeCaptureSignal> {
        self.signals
            .lock()
            .map(|mut signals| signals.drain(..).collect())
            .unwrap_or_default()
    }

    fn last_callback_ns(&self, role: TrackRole) -> Option<u64> {
        self.callback_activity
            .lock()
            .ok()
            .and_then(|activity| activity.get(&role).copied())
    }

    fn levels(&self) -> NativeCaptureLevels {
        let now = self.clock.now_ns();
        let Ok(levels) = self.levels.lock() else {
            return NativeCaptureLevels::default();
        };
        let level = |role| {
            levels.get(&role).map(|recent| {
                if now.saturating_sub(recent.clock_at_ns) <= 750_000_000 {
                    recent.rms
                } else {
                    0.0
                }
            })
        };
        NativeCaptureLevels {
            microphone: level(TrackRole::Microphone),
            system: level(TrackRole::System),
        }
    }
}

fn normalized_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mean_square = samples
        .iter()
        .map(|sample| {
            let normalized = f64::from(*sample) / f64::from(i16::MAX);
            normalized * normalized
        })
        .sum::<f64>()
        / samples.len() as f64;
    mean_square.sqrt().min(1.0) as f32
}

struct OpenSegment {
    path: PathBuf,
    file: File,
    sample_rate: u32,
    channels: u16,
    frames: u64,
    clock_start_ns: u64,
    clock_end_ns: u64,
}

struct ClosedSegmentDraft {
    sequence: u32,
    open: OpenSegment,
}

struct TrackRecorder {
    role: TrackRole,
    sequence: u32,
    open: Option<OpenSegment>,
    closed: VecDeque<ClosedSegmentDraft>,
    expected_clock_ns: Option<u64>,
    silence_started_ns: Option<u64>,
    silence_reported: bool,
}

impl TrackRecorder {
    fn new_at(role: TrackRole, expected_clock_ns: u64) -> Self {
        Self::new_with_expected_clock(role, Some(expected_clock_ns))
    }

    fn new_with_expected_clock(role: TrackRole, expected_clock_ns: Option<u64>) -> Self {
        Self {
            role,
            sequence: 0,
            open: None,
            closed: VecDeque::new(),
            expected_clock_ns,
            silence_started_ns: None,
            silence_reported: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write(
        &mut self,
        package: &Path,
        samples: &[i16],
        sample_rate: u32,
        channels: u16,
        clock_start_ns: u64,
        clock_end_ns: u64,
        silent: bool,
        signals: &Mutex<VecDeque<super::session::NativeCaptureSignal>>,
    ) -> Result<(), CaptureError> {
        if sample_rate == 0 || channels == 0 || channels > 2 {
            return Err(CaptureError::local(
                "macos_audio_format_changed",
                "macOS audio device changed to an unsupported format",
            ));
        }
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.sample_rate != sample_rate || open.channels != channels)
        {
            return Err(CaptureError::local(
                "macos_audio_device_changed",
                "macOS audio format changed during a segment",
            ));
        }
        if let Some(expected) = self.expected_clock_ns {
            if expected.abs_diff(clock_start_ns) > 100_000_000 {
                let first_callback = self.sequence == 0 && self.open.is_none();
                if !first_callback {
                    self.rotate();
                }
                push_gap(
                    signals,
                    expected.min(clock_start_ns),
                    expected.max(clock_start_ns),
                    if first_callback {
                        "macos_audio_startup_delay"
                    } else {
                        "macos_audio_discontinuity"
                    },
                    if first_callback {
                        "macOS audio source started after the recording clock"
                    } else {
                        "macOS audio callback clock was discontinuous"
                    },
                );
            }
        }
        self.expected_clock_ns = Some(clock_end_ns);
        if self.role == TrackRole::System {
            if silent {
                let start = *self.silence_started_ns.get_or_insert(clock_start_ns);
                if !self.silence_reported && clock_end_ns.saturating_sub(start) >= 1_000_000_000 {
                    push_gap(
                        signals,
                        start,
                        clock_end_ns,
                        "macos_system_silence",
                        "selected system source produced sustained silence",
                    );
                    self.silence_reported = true;
                }
            } else {
                self.silence_started_ns = None;
                self.silence_reported = false;
            }
        }

        if self.open.is_none() {
            let relative = format!("tracks/{}-{:04}.wav", role_slug(self.role), self.sequence);
            let path = package.join(&relative);
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .read(true)
                .open(&path)
                .map_err(|_| {
                    CaptureError::local(
                        "macos_segment_open_failed",
                        "macOS WAV segment could not be created",
                    )
                })?;
            write_wav_header(&mut file, sample_rate, channels, 0)?;
            self.open = Some(OpenSegment {
                path,
                file,
                sample_rate,
                channels,
                frames: 0,
                clock_start_ns,
                clock_end_ns,
            });
        }
        let open = self.open.as_mut().expect("segment initialized above");
        let mut encoded = Vec::with_capacity(samples.len().saturating_mul(2));
        encoded.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        open.file.write_all(&encoded).map_err(|_| {
            CaptureError::local(
                "macos_segment_write_failed",
                "macOS WAV segment could not be written",
            )
        })?;
        open.frames = open
            .frames
            .saturating_add(u64::try_from(samples.len() / usize::from(channels)).unwrap_or(0));
        open.clock_end_ns = open.clock_end_ns.max(clock_end_ns);
        Ok(())
    }

    fn close(
        &mut self,
        package: &Path,
    ) -> Result<Option<super::session::SegmentMetadata>, CaptureError> {
        let Some(mut open) = self.open.take() else {
            return Ok(None);
        };
        if open.frames == 0 {
            let _ = fs::remove_file(open.path);
            return Ok(None);
        }
        let metadata = finalize_open_segment(&mut open, package, self.role, self.sequence)?;
        self.sequence = self.sequence.saturating_add(1);
        Ok(Some(metadata))
    }

    fn rotate(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        if open.frames == 0 {
            let _ = fs::remove_file(open.path);
            return;
        }
        self.closed.push_back(ClosedSegmentDraft {
            sequence: self.sequence,
            open,
        });
        self.sequence = self.sequence.saturating_add(1);
    }

    fn finalize_closed(
        &mut self,
        package: &Path,
    ) -> Result<Vec<super::session::SegmentMetadata>, CaptureError> {
        let mut metadata = Vec::with_capacity(self.closed.len());
        while let Some(draft) = self.closed.front_mut() {
            metadata.push(finalize_open_segment(
                &mut draft.open,
                package,
                self.role,
                draft.sequence,
            )?);
            self.closed.pop_front();
        }
        Ok(metadata)
    }
}

fn finalize_open_segment(
    open: &mut OpenSegment,
    package: &Path,
    role: TrackRole,
    sequence: u32,
) -> Result<super::session::SegmentMetadata, CaptureError> {
    let data_bytes = open
        .frames
        .saturating_mul(u64::from(open.channels))
        .saturating_mul(2);
    write_wav_header(&mut open.file, open.sample_rate, open.channels, data_bytes)?;
    open.file.sync_all().map_err(|_| {
        CaptureError::local(
            "macos_segment_sync_failed",
            "macOS WAV segment could not be synchronized",
        )
    })?;
    let relative_path = open
        .path
        .strip_prefix(package)
        .map_err(|_| {
            CaptureError::local(
                "macos_segment_path_failed",
                "macOS segment path escaped its package",
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");
    let sha256 = hash_file(&open.path)?;
    Ok(super::session::SegmentMetadata {
        role,
        sequence,
        relative_path,
        codec: "pcm_s16le".to_owned(),
        sample_rate: open.sample_rate,
        channels: u32::from(open.channels),
        frames_written: open.frames,
        clock_start_ns: open.clock_start_ns,
        clock_end_ns: open.clock_end_ns,
        sha256,
    })
}

struct ProcessTapContext {
    shared: Arc<SharedCapture>,
}

const PROCESS_TAP_CALLBACK_TIMEOUT_NS: u64 = 3_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessTapWatchdogAction {
    None,
    Restart { inactive_since_ns: u64 },
    SourceLost,
}

struct ProcessTapWatchdog {
    started_at_ns: u64,
    restarted_at_ns: Option<u64>,
    failed: bool,
}

impl ProcessTapWatchdog {
    fn new(started_at_ns: u64) -> Self {
        Self {
            started_at_ns,
            restarted_at_ns: None,
            failed: false,
        }
    }

    fn reset(&mut self, started_at_ns: u64) {
        self.started_at_ns = started_at_ns;
        self.restarted_at_ns = None;
        self.failed = false;
    }

    fn fail(&mut self) {
        self.failed = true;
    }

    fn poll(&mut self, now_ns: u64, last_callback_ns: Option<u64>) -> ProcessTapWatchdogAction {
        if self.failed {
            return ProcessTapWatchdogAction::None;
        }
        if self
            .restarted_at_ns
            .is_some_and(|restart| last_callback_ns.is_some_and(|callback| callback >= restart))
        {
            self.restarted_at_ns = None;
        }
        let inactive_since_ns = last_callback_ns.unwrap_or(self.started_at_ns);
        if now_ns.saturating_sub(inactive_since_ns) < PROCESS_TAP_CALLBACK_TIMEOUT_NS {
            return ProcessTapWatchdogAction::None;
        }
        if let Some(restarted_at_ns) = self.restarted_at_ns {
            if now_ns.saturating_sub(restarted_at_ns) >= PROCESS_TAP_CALLBACK_TIMEOUT_NS {
                self.failed = true;
                return ProcessTapWatchdogAction::SourceLost;
            }
            return ProcessTapWatchdogAction::None;
        }
        self.restarted_at_ns = Some(now_ns);
        ProcessTapWatchdogAction::Restart { inactive_since_ns }
    }
}

struct ProcessTapStream {
    handle: NonNull<c_void>,
    context: NonNull<ProcessTapContext>,
}

// The Swift object and callback context are accessed only by Core Audio's
// serialized IO queue plus the capture session's serialized control path.
unsafe impl Send for ProcessTapStream {}

impl ProcessTapStream {
    fn start(
        process_id: i32,
        bundle_id: &str,
        shared: Arc<SharedCapture>,
    ) -> Result<Self, CaptureError> {
        let bundle_id = CString::new(bundle_id).map_err(|_| process_tap_error())?;
        let context = NonNull::new(Box::into_raw(Box::new(ProcessTapContext { shared })))
            .ok_or_else(process_tap_error)?;
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            echowall_process_tap_create(
                process_id,
                bundle_id.as_ptr(),
                context.as_ptr().cast(),
                Some(process_tap_audio_callback),
                &mut handle,
            )
        };
        let Some(handle) = NonNull::new(handle) else {
            unsafe { drop(Box::from_raw(context.as_ptr())) };
            return Err(process_tap_error());
        };
        if status != 0 {
            unsafe {
                echowall_process_tap_destroy(handle.as_ptr());
                drop(Box::from_raw(context.as_ptr()));
            }
            return Err(process_tap_error());
        }
        Ok(Self { handle, context })
    }

    fn pause(&mut self) -> Result<(), CaptureError> {
        (unsafe { echowall_process_tap_pause(self.handle.as_ptr()) } == 0)
            .then_some(())
            .ok_or_else(process_tap_error)
    }

    fn resume(&mut self) -> Result<(), CaptureError> {
        (unsafe { echowall_process_tap_resume(self.handle.as_ptr()) } == 0)
            .then_some(())
            .ok_or_else(process_tap_error)
    }
}

impl Drop for ProcessTapStream {
    fn drop(&mut self) {
        unsafe {
            echowall_process_tap_destroy(self.handle.as_ptr());
            drop(Box::from_raw(self.context.as_ptr()));
        }
    }
}

type ProcessTapAudioCallback = unsafe extern "C" fn(*mut c_void, *const f32, u32, u32, f64, u64);

unsafe extern "C" {
    fn echowall_process_tap_create(
        process_id: i32,
        bundle_id: *const c_char,
        context: *mut c_void,
        callback: Option<ProcessTapAudioCallback>,
        output: *mut *mut c_void,
    ) -> i32;
    fn echowall_process_tap_pause(handle: *mut c_void) -> i32;
    fn echowall_process_tap_resume(handle: *mut c_void) -> i32;
    fn echowall_process_tap_destroy(handle: *mut c_void);
}

unsafe extern "C" fn process_tap_audio_callback(
    context: *mut c_void,
    samples: *const f32,
    frame_count: u32,
    channel_count: u32,
    sample_rate: f64,
    host_time_ns: u64,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(context) = NonNull::new(context.cast::<ProcessTapContext>()) else {
            return;
        };
        if samples.is_null()
            || frame_count == 0
            || !(1..=2).contains(&channel_count)
            || !(8_000.0..=384_000.0).contains(&sample_rate)
        {
            unsafe { context.as_ref() }.shared.source_lost(
                "macos_process_tap_format_invalid",
                "Core Audio process tap returned an unsupported buffer",
            );
            return;
        }
        let Some(sample_count) = usize::try_from(frame_count)
            .ok()
            .and_then(|frames| frames.checked_mul(channel_count as usize))
        else {
            return;
        };
        if sample_count > 4_000_000 {
            return;
        }
        let input = unsafe { std::slice::from_raw_parts(samples, sample_count) };
        let converted: Vec<i16> = input.iter().copied().map(float_to_i16).collect();
        let sample_rate = sample_rate.round() as u32;
        let native_start = (host_time_ns != 0).then_some(host_time_ns);
        unsafe { context.as_ref() }.shared.push_i16_at(
            TrackRole::System,
            &converted,
            sample_rate,
            channel_count as u16,
            native_start,
        );
    }));
}

pub(crate) fn process_tap_supported() -> bool {
    let version = NSProcessInfo::processInfo().operatingSystemVersion();
    process_tap_supported_version(version.majorVersion, version.minorVersion)
}

pub(crate) fn process_tap_available() -> bool {
    process_tap_supported()
        && std::env::var("ECHOWALL_PROCESS_TAP_ENABLED")
            .ok()
            .as_deref()
            .or(option_env!("ECHOWALL_PROCESS_TAP_ENABLED"))
            .is_some_and(process_tap_flag_enabled)
}

fn process_tap_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes"
    )
}

const fn process_tap_supported_version(major: isize, minor: isize) -> bool {
    major > 14 || (major == 14 && minor >= 2)
}

fn process_tap_error() -> CaptureError {
    CaptureError::local(
        "macos_process_tap_unavailable",
        "selected-process audio capture requires macOS 14.2 or later",
    )
}

fn start_system_audio(
    filter: &MacCaptureFilter,
    shared: Arc<SharedCapture>,
) -> Result<MacSystemAudioStream, CaptureError> {
    let content = SCShareableContent::get().map_err(mac_stream_error)?;
    let applications = content.applications();
    if let Some(selected_id) = &filter.selected_application_id {
        if !process_tap_available() {
            return Err(process_tap_error());
        }
        let pid = parse_application_id(selected_id)?;
        let selected = applications
            .iter()
            .find(|application| application.process_id() == pid)
            .ok_or_else(|| {
                CaptureError::local("macos_source_missing", "selected macOS application exited")
            })?;
        return ProcessTapStream::start(pid, &selected.bundle_identifier(), shared)
            .map(MacSystemAudioStream::ProcessTap);
    }
    let displays = content.displays();
    let display = displays.first().ok_or_else(|| {
        CaptureError::local("macos_display_missing", "macOS has no shareable display")
    })?;
    let own_pid = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    let excluded: Vec<&SCRunningApplication> = applications
        .iter()
        .filter(|application| application.process_id() == own_pid)
        .collect();
    let content_filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_applications(&excluded, &[])
        .build();
    let configuration = SCStreamConfiguration::new()
        .with_width(2)
        .with_height(2)
        .with_captures_audio(true)
        .with_sample_rate(48_000)
        .with_channel_count(2)
        .with_excludes_current_process_audio(filter.exclude_echowall_audio);
    let delegate_shared = Arc::clone(&shared);
    let delegate = screencapturekit::stream::delegate_trait::ErrorHandler::new(move |_| {
        delegate_shared.source_lost(
            "macos_capture_source_exited",
            "selected macOS application or system-audio stream stopped",
        );
    });
    let mut stream = SCStream::new_with_delegate(&content_filter, &configuration, delegate);
    let output_shared = Arc::clone(&shared);
    stream.add_output_handler(
        move |sample: CMSampleBuffer, output_type: SCStreamOutputType| {
            if output_type != SCStreamOutputType::Audio {
                return;
            }
            if let Some((samples, channels)) = sample_buffer_f32_to_i16(&sample) {
                let timestamp = sample.output_presentation_timestamp();
                let native_start_ns =
                    (timestamp.timescale > 0 && timestamp.value >= 0).then(|| {
                        u64::try_from(timestamp.value)
                            .unwrap_or(u64::MAX)
                            .saturating_mul(1_000_000_000)
                            / u64::try_from(timestamp.timescale).unwrap_or(1)
                    });
                output_shared.push_i16_at(
                    TrackRole::System,
                    &samples,
                    48_000,
                    channels,
                    native_start_ns,
                );
            } else {
                output_shared.source_lost(
                    "macos_system_audio_format_invalid",
                    "ScreenCaptureKit returned an unsupported system-audio buffer",
                );
            }
        },
        SCStreamOutputType::Audio,
    );
    stream.start_capture().map_err(mac_stream_error)?;
    Ok(MacSystemAudioStream::ScreenCaptureKit(stream))
}

fn start_microphone(
    selected_id: &str,
    shared: Arc<SharedCapture>,
) -> Result<cpal::Stream, CaptureError> {
    if !AudioInputDevice::list()
        .iter()
        .any(|device| device.id == selected_id)
    {
        return Err(CaptureError::local(
            "macos_microphone_missing",
            "selected macOS microphone is unavailable",
        ));
    }
    let host = cpal::default_host();
    let device = host
        .input_devices()
        .map_err(mac_audio_error)?
        .find(|device| device.id().ok().is_some_and(|id| id.id() == selected_id))
        .ok_or_else(|| {
            CaptureError::local(
                "macos_microphone_missing",
                "selected CoreAudio microphone is unavailable",
            )
        })?;
    let supported = device.default_input_config().map_err(mac_audio_error)?;
    let sample_rate = supported.sample_rate();
    let channels = supported.channels();
    if channels == 0 || channels > 2 {
        return Err(CaptureError::local(
            "macos_microphone_format_invalid",
            "selected microphone has an unsupported channel count",
        ));
    }
    let config: cpal::StreamConfig = supported.into();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => {
            let callback_shared = Arc::clone(&shared);
            let error_shared = Arc::clone(&shared);
            device.build_input_stream(
                config,
                move |data: &[f32], info| {
                    let samples: Vec<i16> = data.iter().copied().map(float_to_i16).collect();
                    callback_shared.push_i16_at(
                        TrackRole::Microphone,
                        &samples,
                        sample_rate,
                        channels,
                        u64::try_from(info.timestamp().capture.as_nanos()).ok(),
                    );
                },
                move |_| {
                    error_shared.source_lost(
                        "macos_microphone_device_changed",
                        "CoreAudio microphone stream stopped or changed device",
                    )
                },
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let callback_shared = Arc::clone(&shared);
            let error_shared = Arc::clone(&shared);
            device.build_input_stream(
                config,
                move |data: &[i16], info| {
                    callback_shared.push_i16_at(
                        TrackRole::Microphone,
                        data,
                        sample_rate,
                        channels,
                        u64::try_from(info.timestamp().capture.as_nanos()).ok(),
                    );
                },
                move |_| {
                    error_shared.source_lost(
                        "macos_microphone_device_changed",
                        "CoreAudio microphone stream stopped or changed device",
                    )
                },
                None,
            )
        }
        _ => {
            return Err(CaptureError::local(
                "macos_microphone_format_invalid",
                "selected microphone does not expose PCM16 or Float32 samples",
            ))
        }
    }
    .map_err(mac_audio_error)?;
    stream.play().map_err(mac_audio_error)?;
    Ok(stream)
}

fn sample_buffer_f32_to_i16(sample: &CMSampleBuffer) -> Option<(Vec<i16>, u16)> {
    let buffers = sample.audio_buffer_list()?;
    if buffers.num_buffers() == 1 {
        let buffer = buffers.buffer(0)?;
        let channels = u16::try_from(buffer.number_channels()).ok()?;
        if !(1..=2).contains(&channels) {
            return None;
        }
        return Some((
            buffer
                .data()
                .chunks_exact(4)
                .map(|bytes| {
                    let bytes: [u8; 4] = bytes.try_into().expect("chunks_exact yields four bytes");
                    float_to_i16(f32::from_ne_bytes(bytes))
                })
                .collect(),
            channels,
        ));
    }
    if buffers.num_buffers() != 2 {
        return None;
    }
    let left = buffers.buffer(0)?;
    let right = buffers.buffer(1)?;
    let left = left.data().chunks_exact(4);
    let right = right.data().chunks_exact(4);
    let mut result = Vec::with_capacity(left.len().min(right.len()) * 2);
    for (left, right) in left.zip(right) {
        result.push(float_to_i16(f32::from_ne_bytes(left.try_into().ok()?)));
        result.push(float_to_i16(f32::from_ne_bytes(right.try_into().ok()?)));
    }
    Some((result, 2))
}

fn float_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

fn push_gap(
    signals: &Mutex<VecDeque<super::session::NativeCaptureSignal>>,
    start: u64,
    end: u64,
    code: &'static str,
    message: &str,
) {
    if end <= start {
        return;
    }
    if let Ok(mut signals) = signals.lock() {
        signals.push_back(super::session::NativeCaptureSignal::Gap {
            clock_start_ns: start,
            clock_end_ns: end,
            code,
            message: message.to_owned(),
        });
    }
}

fn write_wav_header(
    file: &mut File,
    sample_rate: u32,
    channels: u16,
    data_bytes: u64,
) -> Result<(), CaptureError> {
    let data_bytes = u32::try_from(data_bytes).map_err(|_| {
        CaptureError::local(
            "macos_segment_too_large",
            "macOS WAV segment exceeded 4 GiB",
        )
    })?;
    let byte_rate = sample_rate
        .saturating_mul(u32::from(channels))
        .saturating_mul(2);
    file.seek(SeekFrom::Start(0)).map_err(segment_io_error)?;
    file.write_all(b"RIFF").map_err(segment_io_error)?;
    file.write_all(&data_bytes.saturating_add(36).to_le_bytes())
        .map_err(segment_io_error)?;
    file.write_all(b"WAVEfmt ").map_err(segment_io_error)?;
    file.write_all(&16u32.to_le_bytes())
        .map_err(segment_io_error)?;
    file.write_all(&1u16.to_le_bytes())
        .map_err(segment_io_error)?;
    file.write_all(&channels.to_le_bytes())
        .map_err(segment_io_error)?;
    file.write_all(&sample_rate.to_le_bytes())
        .map_err(segment_io_error)?;
    file.write_all(&byte_rate.to_le_bytes())
        .map_err(segment_io_error)?;
    file.write_all(&(channels.saturating_mul(2)).to_le_bytes())
        .map_err(segment_io_error)?;
    file.write_all(&16u16.to_le_bytes())
        .map_err(segment_io_error)?;
    file.write_all(b"data").map_err(segment_io_error)?;
    file.write_all(&data_bytes.to_le_bytes())
        .map_err(segment_io_error)?;
    file.seek(SeekFrom::End(0)).map_err(segment_io_error)?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, CaptureError> {
    let mut file = File::open(path).map_err(segment_io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(segment_io_error)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn segment_io_error(_: std::io::Error) -> CaptureError {
    CaptureError::local(
        "macos_segment_io_failed",
        "macOS capture segment could not be persisted",
    )
}

fn mac_audio_error<E>(_: E) -> CaptureError {
    CaptureError::local(
        "macos_microphone_unavailable",
        "selected CoreAudio microphone could not be opened",
    )
}

fn mac_stream_error<E>(_: E) -> CaptureError {
    CaptureError::local(
        "macos_system_audio_unavailable",
        "ScreenCaptureKit system audio could not be started",
    )
}

fn application_id(pid: i32) -> String {
    format!("pid:{pid}")
}

fn application_process_is_running(pid: i32) -> bool {
    application_process_status(pid).is_some_and(|status| status != libc::SZOMB)
}

fn application_process_status(pid: i32) -> Option<u32> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = std::mem::size_of::<libc::proc_bsdinfo>();
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            i32::try_from(expected).unwrap_or(i32::MAX),
        )
    };
    (read == i32::try_from(expected).unwrap_or(-1))
        .then(|| unsafe { info.assume_init() }.pbi_status)
}

fn parse_application_id(value: &str) -> Result<i32, CaptureError> {
    value
        .strip_prefix("pid:")
        .and_then(|pid| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| {
            CaptureError::local(
                "macos_source_id_invalid",
                "selected macOS application identifier is invalid",
            )
        })
}

fn is_browser_bundle(bundle: &str) -> bool {
    matches!(
        bundle,
        "com.apple.Safari"
            | "com.google.Chrome"
            | "com.microsoft.edgemac"
            | "org.mozilla.firefox"
            | "company.thebrowser.Browser"
            | "com.brave.Browser"
    )
}

fn role_slug(role: TrackRole) -> &'static str {
    match role {
        TrackRole::Microphone => "microphone",
        TrackRole::System => "system",
        TrackRole::Mixed => "mixed",
        TrackRole::Imported => "imported",
    }
}

fn role_order(role: TrackRole) -> u8 {
    match role {
        TrackRole::Microphone => 0,
        TrackRole::System => 1,
        TrackRole::Mixed => 2,
        TrackRole::Imported => 3,
    }
}

fn microphone_permission() -> PermissionState {
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return PermissionState::Unavailable;
    };
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) } {
        AVAuthorizationStatus::Authorized => PermissionState::Granted,
        AVAuthorizationStatus::Denied => PermissionState::Denied,
        AVAuthorizationStatus::Restricted => PermissionState::Restricted,
        AVAuthorizationStatus::NotDetermined => PermissionState::NotDetermined,
        _ => PermissionState::Unavailable,
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

fn native_clock_ns() -> Option<u64> {
    let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
    let status = unsafe { mach_timebase_info(&mut info) };
    if status != 0 || info.denom == 0 {
        return None;
    }
    let ticks = unsafe { mach_absolute_time() };
    Some(
        ticks
            .saturating_mul(u64::from(info.numer))
            .checked_div(u64::from(info.denom))
            .unwrap_or(0),
    )
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[cfg(test)]
mod native_tests {
    use super::*;

    #[test]
    fn process_liveness_rejects_missing_process_and_accepts_current_process() {
        assert!(!application_process_is_running(-1));
        assert!(application_process_is_running(
            i32::try_from(std::process::id()).unwrap()
        ));
    }

    #[test]
    fn process_tap_watchdog_restarts_once_then_reports_missing_callbacks() {
        let mut watchdog = ProcessTapWatchdog::new(1_000_000_000);
        assert_eq!(
            watchdog.poll(3_999_999_999, None),
            ProcessTapWatchdogAction::None
        );
        assert_eq!(
            watchdog.poll(4_000_000_000, None),
            ProcessTapWatchdogAction::Restart {
                inactive_since_ns: 1_000_000_000
            }
        );
        assert_eq!(
            watchdog.poll(6_999_999_999, None),
            ProcessTapWatchdogAction::None
        );
        assert_eq!(
            watchdog.poll(7_000_000_000, None),
            ProcessTapWatchdogAction::SourceLost
        );
        assert_eq!(
            watchdog.poll(20_000_000_000, None),
            ProcessTapWatchdogAction::None
        );
    }

    #[test]
    fn process_tap_watchdog_recovers_and_watches_for_a_later_stall() {
        let mut watchdog = ProcessTapWatchdog::new(1_000_000_000);
        assert!(matches!(
            watchdog.poll(4_000_000_000, None),
            ProcessTapWatchdogAction::Restart { .. }
        ));
        assert_eq!(
            watchdog.poll(4_500_000_000, Some(4_400_000_000)),
            ProcessTapWatchdogAction::None
        );
        assert_eq!(
            watchdog.poll(7_400_000_000, Some(4_400_000_000)),
            ProcessTapWatchdogAction::Restart {
                inactive_since_ns: 4_400_000_000
            }
        );
        watchdog.reset(9_000_000_000);
        assert_eq!(
            watchdog.poll(11_999_999_999, None),
            ProcessTapWatchdogAction::None
        );
    }

    #[test]
    fn source_icon_is_a_bounded_png_data_url() {
        let image = DynamicImage::new_rgba8(96, 64);
        let encoded = encode_icon_data_url(image).unwrap();
        assert!(encoded.starts_with("data:image/png;base64,"));
        assert!(encoded.len() < MAX_SOURCE_ICON_PNG_BYTES * 2);

        assert!(bounded_icon_data_url(&vec![0; MAX_NATIVE_ICON_PNG_BYTES + 1]).is_none());
    }

    #[test]
    #[ignore = "requires Screen Recording access to enumerate current macOS applications"]
    fn real_source_enumeration_returns_at_least_one_bounded_icon() {
        let sources = MacOsNativeBridge.enumerate_sources().unwrap();
        assert!(sources.len() <= MAX_APPLICATION_SOURCES);
        let icon = sources
            .iter()
            .find_map(|source| {
                source
                    .process_id
                    .and_then(|value| i32::try_from(value).ok())
                    .and_then(application_icon_data_url)
            })
            .expect("macOS returned no decodable application icon");
        assert!(icon.starts_with("data:image/png;base64,"));
        assert!(icon.len() <= MAX_SOURCE_ICON_PNG_BYTES * 2);
    }

    #[test]
    fn virtual_tracks_write_separate_wav_segments_on_one_clock() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("tracks")).unwrap();
        let signals = Mutex::new(VecDeque::new());
        let mut microphone = TrackRecorder::new_with_expected_clock(TrackRole::Microphone, None);
        let mut system = TrackRecorder::new_with_expected_clock(TrackRole::System, None);
        let mic_samples: Vec<i16> = (0..480).map(|value| (value % 100) as i16).collect();
        let system_samples: Vec<i16> = (0..960)
            .map(|value| if value % 2 == 0 { 100 } else { -100 })
            .collect();

        microphone
            .write(
                temp.path(),
                &mic_samples,
                48_000,
                1,
                1_000_000_000,
                1_010_000_000,
                false,
                &signals,
            )
            .unwrap();
        system
            .write(
                temp.path(),
                &system_samples,
                48_000,
                2,
                1_000_000_000,
                1_010_000_000,
                false,
                &signals,
            )
            .unwrap();
        let mic = microphone.close(temp.path()).unwrap().unwrap();
        let system = system.close(temp.path()).unwrap().unwrap();

        assert_eq!(mic.role, TrackRole::Microphone);
        assert_eq!(system.role, TrackRole::System);
        assert_eq!(mic.clock_start_ns, system.clock_start_ns);
        assert_eq!(mic.clock_end_ns, system.clock_end_ns);
        assert_eq!(
            &fs::read(temp.path().join(mic.relative_path)).unwrap()[0..4],
            b"RIFF"
        );
        assert_eq!(
            &fs::read(temp.path().join(system.relative_path)).unwrap()[8..12],
            b"WAVE"
        );
    }

    #[test]
    fn virtual_source_emits_discontinuity_and_sustained_silence_signals() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("tracks")).unwrap();
        let signals = Mutex::new(VecDeque::new());
        let mut system = TrackRecorder::new_with_expected_clock(TrackRole::System, None);
        let silence = vec![0i16; 96];
        let audible = vec![10i16; 96];
        system
            .write(
                temp.path(),
                &silence,
                48_000,
                2,
                0,
                1_000_000,
                true,
                &signals,
            )
            .unwrap();
        system
            .write(
                temp.path(),
                &silence,
                48_000,
                2,
                1_500_000_000,
                1_501_000_000,
                true,
                &signals,
            )
            .unwrap();
        system
            .write(
                temp.path(),
                &audible,
                48_000,
                2,
                1_600_000_000,
                1_601_000_000,
                false,
                &signals,
            )
            .unwrap();
        assert_eq!(system.closed.len(), 1);
        assert_eq!(system.closed[0].sequence, 0);
        assert_eq!(system.closed[0].open.clock_end_ns, 1_000_000);
        let closed = system.finalize_closed(temp.path()).unwrap();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].clock_end_ns, 1_000_000);
        let current = system.close(temp.path()).unwrap().unwrap();
        assert_eq!(current.sequence, 1);
        assert_eq!(current.clock_start_ns, 1_500_000_000);
        assert_eq!(current.clock_end_ns, 1_601_000_000);
        let codes: Vec<_> = signals
            .lock()
            .unwrap()
            .iter()
            .map(|signal| match signal {
                super::super::session::NativeCaptureSignal::Gap { code, .. }
                | super::super::session::NativeCaptureSignal::SourceLost { code, .. } => *code,
            })
            .collect();
        assert!(codes.contains(&"macos_audio_discontinuity"));
        assert!(codes.contains(&"macos_system_silence"));
    }

    #[test]
    fn discontinuity_never_masks_a_real_audio_format_change() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("tracks")).unwrap();
        let signals = Mutex::new(VecDeque::new());
        let mut microphone = TrackRecorder::new_with_expected_clock(TrackRole::Microphone, None);
        microphone
            .write(
                temp.path(),
                &[100_i16; 48],
                48_000,
                1,
                0,
                1_000_000,
                false,
                &signals,
            )
            .unwrap();
        let error = microphone
            .write(
                temp.path(),
                &[100_i16; 96],
                48_000,
                2,
                1_500_000_000,
                1_501_000_000,
                false,
                &signals,
            )
            .unwrap_err();
        assert_eq!(error.code, "macos_audio_device_changed");
        assert!(microphone.closed.is_empty());
        assert_eq!(microphone.open.as_ref().unwrap().channels, 1);
    }

    #[test]
    fn first_native_callback_records_startup_delay_instead_of_fabricating_audio() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("tracks")).unwrap();
        let signals = Mutex::new(VecDeque::new());
        let mut system = TrackRecorder::new_at(TrackRole::System, 1_000_000_000);
        system
            .write(
                temp.path(),
                &[100_i16; 96],
                48_000,
                2,
                2_400_000_000,
                2_401_000_000,
                false,
                &signals,
            )
            .unwrap();
        let values = signals.lock().unwrap();
        assert!(matches!(
            values.as_slices().0.first(),
            Some(super::super::session::NativeCaptureSignal::Gap {
                clock_start_ns: 1_000_000_000,
                clock_end_ns: 2_400_000_000,
                code: "macos_audio_startup_delay",
                ..
            })
        ));
    }

    #[test]
    fn application_ids_and_browser_classification_are_bounded() {
        assert_eq!(parse_application_id(&application_id(42)).unwrap(), 42);
        assert!(parse_application_id("bundle:unsafe").is_err());
        assert!(is_browser_bundle("com.google.Chrome"));
        assert!(!is_browser_bundle("com.example.Meeting"));
    }

    #[test]
    fn process_tap_availability_is_macos_14_2_or_later() {
        assert!(!process_tap_supported_version(13, 9));
        assert!(!process_tap_supported_version(14, 1));
        assert!(process_tap_supported_version(14, 2));
        assert!(process_tap_supported_version(15, 0));
        for enabled in ["1", "true", "ON", " yes "] {
            assert!(process_tap_flag_enabled(enabled));
        }
        for disabled in ["", "0", "false", "unexpected"] {
            assert!(!process_tap_flag_enabled(disabled));
        }
    }

    #[test]
    fn native_meter_rms_is_normalized_and_bounded() {
        assert_eq!(normalized_rms(&[]), 0.0);
        assert_eq!(normalized_rms(&[0, 0, 0]), 0.0);
        assert!((normalized_rms(&[i16::MAX, i16::MIN]) - 1.0).abs() < 0.0001);
        assert!((normalized_rms(&[i16::MAX / 2, i16::MAX / 2]) - 0.5).abs() < 0.001);
    }
}
