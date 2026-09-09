use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::ingest::envelope::{
    validate_relative_path, validate_sha256, RecordingEnvelope, TrackRole, MAX_CAPTURE_WARNINGS,
};
use crate::ingest::inbox::{
    FileDigest, Inbox, InboxEvent, MAX_INBOX_EVENTS_BYTES, MAX_INBOX_EVENT_BYTES,
};
use crate::ingest::state::JobState;

use super::model::{CaptureModelError, CapturePlan, CapturePreflight};

pub const ROLLING_SEGMENT_NS: u64 = 5 * 60 * 1_000_000_000;
const CAPTURE_TARGET_SECONDS: u64 = 2 * 60 * 60;
const DESKTOP_CAPTURE_SAMPLE_RATE: u64 = 48_000;
const DESKTOP_CAPTURE_MAX_CHANNELS: u64 = 2;
const PCM16_BYTES_PER_SAMPLE: u64 = 2;
const NORMALIZED_CAPTURE_SAMPLE_RATE: u64 = 16_000;
const CAPTURE_STORAGE_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
const CAPTURE_SNAPSHOT_VERSION: u32 = 1;
const SNAPSHOT_FILE: &str = "capture-session.json";
const EVENTS_FILE: &str = "events.ndjson";
pub(super) const MAX_CAPTURE_SNAPSHOT_BYTES: u64 = 1024 * 1024;
pub(super) const MAX_CAPTURE_EVENTS_BYTES: u64 = MAX_INBOX_EVENTS_BYTES;
pub(super) const CAPTURE_EVENT_RESERVE_BYTES: u64 = 1024 * 1024;
const MAX_CAPTURE_EVENT_BYTES: u64 = MAX_INBOX_EVENT_BYTES;
const MAX_CAPTURE_SEGMENTS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePhase {
    Starting,
    Recording,
    Paused,
    SourceLost,
    Interrupted,
    Finalizing,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentCloseReason {
    RollingLimit,
    Pause,
    SourceLoss,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    Pause,
    SourceLoss,
    DeviceDiscontinuity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentMetadata {
    pub role: TrackRole,
    pub sequence: u32,
    pub relative_path: String,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub frames_written: u64,
    pub clock_start_ns: u64,
    pub clock_end_ns: u64,
    pub sha256: String,
}

impl SegmentMetadata {
    pub fn duration_ms(&self) -> u64 {
        self.frames_written
            .saturating_mul(1_000)
            .checked_div(u64::from(self.sample_rate))
            .unwrap_or(0)
    }

    fn validate(&self) -> Result<(), CaptureError> {
        validate_relative_path(&self.relative_path, "segment.relative_path")?;
        validate_sha256(&self.sha256, "segment.sha256")?;
        if self.codec.is_empty() || self.codec.chars().count() > 64 {
            return Err(CaptureError::local(
                "invalid_segment",
                "segment codec is invalid",
            ));
        }
        if self.sample_rate == 0
            || self.sample_rate > 768_000
            || self.channels == 0
            || self.channels > 64
            || self.frames_written == 0
            || self.clock_end_ns <= self.clock_start_ns
        {
            return Err(CaptureError::local(
                "invalid_segment",
                "segment timing or audio metadata is invalid",
            ));
        }
        let clock_span_ns = self.clock_end_ns - self.clock_start_ns;
        let audio_ns = self
            .frames_written
            .saturating_mul(1_000_000_000)
            .checked_div(u64::from(self.sample_rate))
            .unwrap_or(u64::MAX);
        if audio_ns.abs_diff(clock_span_ns) > 100_000_000 {
            return Err(CaptureError::local(
                "invalid_segment",
                "segment audio duration diverges from its monotonic clock span",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureGap {
    pub reason: GapReason,
    pub clock_start_ns: u64,
    pub clock_end_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureNotice {
    pub code: String,
    pub message: String,
    pub clock_at_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSnapshot {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub plan: CapturePlan,
    pub phase: CapturePhase,
    pub wall_started_at: DateTime<FixedOffset>,
    pub monotonic_started_ns: u64,
    pub segment_started_ns: Option<u64>,
    pub paused_at_ns: Option<u64>,
    pub segments: Vec<SegmentMetadata>,
    pub gaps: Vec<CaptureGap>,
    pub notices: Vec<CaptureNotice>,
}

impl CaptureSnapshot {
    pub fn observed_end_ns(&self) -> Option<u64> {
        self.segments
            .iter()
            .map(|segment| segment.clock_end_ns)
            .max()
    }

    pub fn observed_duration_ms(&self) -> u64 {
        self.observed_end_ns()
            .and_then(|end| end.checked_sub(self.monotonic_started_ns))
            .unwrap_or(0)
            / 1_000_000
    }

    fn validate(&self) -> Result<(), CaptureError> {
        if self.schema_version != CAPTURE_SNAPSHOT_VERSION {
            return Err(CaptureError::local(
                "invalid_snapshot",
                "capture snapshot version is unsupported",
            ));
        }
        if self.segments.len() > MAX_CAPTURE_SEGMENTS
            || self.gaps.len().saturating_add(self.notices.len()) > MAX_CAPTURE_WARNINGS
        {
            return Err(CaptureError::local(
                "capture_journal_too_large",
                "capture snapshot collections exceed recovery limits",
            ));
        }
        self.plan.validate()?;
        if self
            .segment_started_ns
            .is_some_and(|start| start < self.monotonic_started_ns)
            || self
                .paused_at_ns
                .is_some_and(|start| start < self.monotonic_started_ns)
        {
            return Err(CaptureError::local(
                "invalid_snapshot",
                "capture snapshot clock precedes session start",
            ));
        }
        let mut paths = HashSet::new();
        let mut role_sequences: BTreeMap<String, u32> = BTreeMap::new();
        let mut role_ends: BTreeMap<String, u64> = BTreeMap::new();
        for segment in &self.segments {
            segment.validate()?;
            if !self.plan.required_roles().contains(&segment.role)
                || segment.clock_start_ns < self.monotonic_started_ns
                || !paths.insert(segment.relative_path.as_str())
            {
                return Err(CaptureError::local(
                    "invalid_snapshot",
                    "capture segment path or clock is invalid",
                ));
            }
            let role = role_name(segment.role).to_owned();
            let expected_sequence = role_sequences.entry(role.clone()).or_insert(0);
            if segment.sequence != *expected_sequence {
                return Err(CaptureError::local(
                    "invalid_snapshot",
                    "capture segment sequence is not contiguous",
                ));
            }
            *expected_sequence += 1;
            let previous_end = role_ends.entry(role).or_insert(self.monotonic_started_ns);
            if segment.clock_start_ns < *previous_end {
                return Err(CaptureError::local(
                    "invalid_snapshot",
                    "capture segments overlap on one track",
                ));
            }
            *previous_end = segment.clock_end_ns;
        }
        let mut previous_gap_end = self.monotonic_started_ns;
        for gap in &self.gaps {
            if gap.clock_start_ns < self.monotonic_started_ns
                || gap.clock_end_ns <= gap.clock_start_ns
                || gap.clock_start_ns < previous_gap_end
            {
                return Err(CaptureError::local(
                    "invalid_snapshot",
                    "capture gap timing is invalid",
                ));
            }
            previous_gap_end = gap.clock_end_ns;
        }
        for notice in &self.notices {
            if !valid_event_code(&notice.code)
                || notice.message.is_empty()
                || notice.message.chars().count() > 2048
                || notice.clock_at_ns < self.monotonic_started_ns
            {
                return Err(CaptureError::local(
                    "invalid_snapshot",
                    "capture notice is invalid",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CaptureStartContext {
    pub recording_id: Uuid,
    pub package_directory: PathBuf,
    pub plan: CapturePlan,
    pub monotonic_started_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NativeCaptureLevels {
    pub microphone: Option<f32>,
    pub system: Option<f32>,
}

pub trait NativeCaptureStream: Send {
    fn close_segment(
        &mut self,
        reason: SegmentCloseReason,
        clock_at_ns: u64,
    ) -> Result<Vec<SegmentMetadata>, CaptureError>;

    fn resume(&mut self, clock_at_ns: u64) -> Result<(), CaptureError>;

    fn take_signals(&mut self) -> Result<Vec<NativeCaptureSignal>, CaptureError> {
        Ok(Vec::new())
    }

    fn levels(&self) -> NativeCaptureLevels {
        NativeCaptureLevels::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeCaptureSignal {
    Gap {
        clock_start_ns: u64,
        clock_end_ns: u64,
        code: &'static str,
        message: String,
    },
    SourceLost {
        clock_at_ns: u64,
        code: &'static str,
        message: String,
    },
}

pub trait DesktopCaptureBackend: Send + Sync {
    fn preflight(&self, plan: &CapturePlan) -> Result<CapturePreflight, CaptureError>;

    fn start(
        &self,
        context: CaptureStartContext,
    ) -> Result<Box<dyn NativeCaptureStream>, CaptureError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct FinalizedCapture {
    pub envelope: RecordingEnvelope,
    pub normalized: NormalizedArtifact,
}

pub trait CaptureFinalizer {
    fn finalize(
        &self,
        snapshot: &CaptureSnapshot,
        package_directory: &Path,
    ) -> Result<FinalizedCapture, CaptureError>;
}

pub struct CaptureSession {
    inbox: Arc<Inbox>,
    journal: CaptureJournal,
    snapshot: CaptureSnapshot,
    native: Option<Box<dyn NativeCaptureStream>>,
}

impl CaptureSession {
    pub fn preflight(
        backend: Option<&dyn DesktopCaptureBackend>,
        plan: &CapturePlan,
    ) -> Result<CapturePreflight, CaptureError> {
        plan.validate()?;
        match backend {
            Some(backend) => backend.preflight(plan),
            None => Ok(CapturePreflight {
                backend_available: false,
                microphone_permission: super::model::PermissionState::Unavailable,
                system_audio_permission: super::model::PermissionState::Unavailable,
                selected_source: super::model::SourceAvailability::Unavailable,
                warnings: vec!["native capture backend is not installed".to_owned()],
            }),
        }
    }

    pub fn start(
        inbox: Arc<Inbox>,
        backend: Option<&dyn DesktopCaptureBackend>,
        plan: CapturePlan,
        wall_started_at: DateTime<FixedOffset>,
        monotonic_started_ns: u64,
    ) -> Result<Self, CaptureError> {
        plan.validate()?;
        let backend = backend.ok_or_else(|| {
            CaptureError::local(
                "backend_unavailable",
                "capture cannot start without an injected native backend",
            )
        })?;
        let preflight = backend.preflight(&plan)?;
        if !preflight.ready_for(&plan) {
            return Err(CaptureError::local(
                "preflight_not_ready",
                "capture permissions or selected source are not ready",
            ));
        }
        let available = fs2::available_space(inbox.root()).map_err(|_| {
            CaptureError::local(
                "capture_storage_check_failed",
                "available capture storage could not be checked",
            )
        })?;
        validate_capture_capacity(available, &plan)?;
        let recording_id = Uuid::new_v4();
        let journal = CaptureJournal::create(Arc::clone(&inbox), recording_id)?;
        let mut snapshot = CaptureSnapshot {
            schema_version: CAPTURE_SNAPSHOT_VERSION,
            recording_id,
            plan: plan.clone(),
            phase: CapturePhase::Starting,
            wall_started_at,
            monotonic_started_ns,
            segment_started_ns: Some(monotonic_started_ns),
            paused_at_ns: None,
            segments: Vec::new(),
            gaps: Vec::new(),
            notices: Vec::new(),
        };
        journal.persist(&snapshot)?;
        journal.event("capture_start_requested", BTreeMap::new())?;
        let native = match backend.start(CaptureStartContext {
            recording_id,
            package_directory: journal.package.clone(),
            plan,
            monotonic_started_ns,
        }) {
            Ok(native) => native,
            Err(error) => {
                snapshot.phase = CapturePhase::Interrupted;
                snapshot.notices.push(CaptureNotice {
                    code: "backend_start_failed".to_owned(),
                    message: "native capture backend failed to start".to_owned(),
                    clock_at_ns: monotonic_started_ns,
                });
                journal.persist(&snapshot)?;
                journal.event(
                    "capture_start_failed",
                    BTreeMap::from([("code".to_owned(), json!(error.code))]),
                )?;
                return Err(error);
            }
        };
        snapshot.phase = CapturePhase::Recording;
        journal.persist(&snapshot)?;
        journal.event("capture_started", BTreeMap::new())?;
        Ok(Self {
            inbox,
            journal,
            snapshot,
            native: Some(native),
        })
    }

    pub fn recover(inbox: Arc<Inbox>, recording_id: Uuid) -> Result<Self, CaptureError> {
        if inbox
            .root()
            .join(recording_id.to_string())
            .join("recording.json")
            .exists()
        {
            return Err(CaptureError::local(
                "already_finalized",
                "capture already has a finalized recording envelope",
            ));
        }
        let journal = CaptureJournal::open(Arc::clone(&inbox), recording_id)?;
        let mut snapshot = journal.load()?;
        snapshot.validate()?;
        for segment in &snapshot.segments {
            verify_segment_file(&inbox, recording_id, segment)?;
        }
        journal.validate_events()?;
        snapshot.phase = CapturePhase::Interrupted;
        snapshot.segment_started_ns = None;
        snapshot.paused_at_ns = None;
        let clock_at_ns = snapshot
            .observed_end_ns()
            .unwrap_or(snapshot.monotonic_started_ns);
        snapshot.notices.push(CaptureNotice {
            code: "crash_recovered".to_owned(),
            message: "Recovered only durably closed capture segments".to_owned(),
            clock_at_ns,
        });
        journal.persist(&snapshot)?;
        journal.event(
            "capture_recovered",
            BTreeMap::from([(
                "observed_duration_ms".to_owned(),
                json!(snapshot.observed_duration_ms()),
            )]),
        )?;
        Ok(Self {
            inbox,
            journal,
            snapshot,
            native: None,
        })
    }

    pub fn recording_id(&self) -> Uuid {
        self.snapshot.recording_id
    }

    pub fn snapshot(&self) -> &CaptureSnapshot {
        &self.snapshot
    }

    pub fn levels(&self) -> NativeCaptureLevels {
        self.native
            .as_deref()
            .map(NativeCaptureStream::levels)
            .unwrap_or_default()
    }

    pub fn tick(&mut self, clock_now_ns: u64) -> Result<usize, CaptureError> {
        self.require_phase(CapturePhase::Recording)?;
        self.poll_native_signals()?;
        if self.snapshot.phase != CapturePhase::Recording {
            return Ok(0);
        }
        let mut rolled = 0;
        while let Some(start) = self.snapshot.segment_started_ns {
            let boundary = start.saturating_add(ROLLING_SEGMENT_NS);
            if clock_now_ns < boundary {
                break;
            }
            self.close_native(SegmentCloseReason::RollingLimit, boundary, false)?;
            self.snapshot.segment_started_ns = Some(boundary);
            self.journal.persist(&self.snapshot)?;
            self.journal.event(
                "capture_segment_rolled",
                BTreeMap::from([("clock_at_ns".to_owned(), json!(boundary))]),
            )?;
            rolled += 1;
        }
        Ok(rolled)
    }

    pub fn poll_native_signals(&mut self) -> Result<usize, CaptureError> {
        let signals = self.native_mut()?.take_signals()?;
        let count = signals.len();
        let mut durable_events = Vec::new();
        for signal in signals {
            match signal {
                NativeCaptureSignal::Gap {
                    clock_start_ns,
                    clock_end_ns,
                    code,
                    message,
                } => {
                    if clock_end_ns > clock_start_ns {
                        insert_non_overlapping_gap(
                            &mut self.snapshot.gaps,
                            CaptureGap {
                                reason: GapReason::DeviceDiscontinuity,
                                clock_start_ns,
                                clock_end_ns,
                            },
                        );
                    }
                    self.snapshot.notices.push(CaptureNotice {
                        code: code.to_owned(),
                        message,
                        clock_at_ns: clock_start_ns,
                    });
                    durable_events.push((
                        "capture_gap_recorded",
                        BTreeMap::from([
                            ("clock_start_ns".to_owned(), json!(clock_start_ns)),
                            ("clock_end_ns".to_owned(), json!(clock_end_ns)),
                            ("code".to_owned(), json!(code)),
                        ]),
                    ));
                }
                NativeCaptureSignal::SourceLost {
                    clock_at_ns,
                    code,
                    message,
                } => {
                    if self.snapshot.phase == CapturePhase::Recording {
                        self.close_native(SegmentCloseReason::SourceLoss, clock_at_ns, true)?;
                        self.snapshot.phase = CapturePhase::SourceLost;
                        self.snapshot.segment_started_ns = None;
                        self.snapshot.paused_at_ns = Some(clock_at_ns);
                    }
                    self.snapshot.notices.push(CaptureNotice {
                        code: code.to_owned(),
                        message,
                        clock_at_ns,
                    });
                    durable_events.push((
                        "capture_source_lost",
                        BTreeMap::from([
                            ("clock_at_ns".to_owned(), json!(clock_at_ns)),
                            ("code".to_owned(), json!(code)),
                        ]),
                    ));
                }
            }
        }
        if count > 0 {
            self.snapshot.gaps.sort_by_key(|gap| gap.clock_start_ns);
            self.snapshot.validate()?;
            self.journal.persist(&self.snapshot)?;
            for (kind, payload) in durable_events {
                self.journal.event(kind, payload)?;
            }
            self.journal.event(
                "capture_native_signal",
                BTreeMap::from([("count".to_owned(), json!(count))]),
            )?;
        }
        Ok(count)
    }

    pub fn pause(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        self.require_phase(CapturePhase::Recording)?;
        self.close_native(SegmentCloseReason::Pause, clock_at_ns, false)?;
        self.snapshot.phase = CapturePhase::Paused;
        self.snapshot.segment_started_ns = None;
        self.snapshot.paused_at_ns = Some(clock_at_ns);
        self.journal.persist(&self.snapshot)?;
        self.journal.event(
            "capture_paused",
            BTreeMap::from([("clock_at_ns".to_owned(), json!(clock_at_ns))]),
        )
    }

    pub fn resume(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        if !matches!(
            self.snapshot.phase,
            CapturePhase::Paused | CapturePhase::SourceLost
        ) {
            return Err(CaptureError::local(
                "invalid_capture_state",
                "capture can resume only after pause or source loss",
            ));
        }
        let gap_start = self.snapshot.paused_at_ns.ok_or_else(|| {
            CaptureError::local("invalid_snapshot", "paused capture has no gap start")
        })?;
        if clock_at_ns < gap_start {
            return Err(CaptureError::local(
                "invalid_clock",
                "resume clock precedes pause clock",
            ));
        }
        self.native_mut()?.resume(clock_at_ns)?;
        let mut gap_payload = None;
        if clock_at_ns > gap_start {
            self.snapshot.gaps.push(CaptureGap {
                reason: if self.snapshot.phase == CapturePhase::Paused {
                    GapReason::Pause
                } else {
                    GapReason::SourceLoss
                },
                clock_start_ns: gap_start,
                clock_end_ns: clock_at_ns,
            });
            gap_payload = Some(BTreeMap::from([
                ("clock_start_ns".to_owned(), json!(gap_start)),
                ("clock_end_ns".to_owned(), json!(clock_at_ns)),
            ]));
        }
        self.snapshot.phase = CapturePhase::Recording;
        self.snapshot.paused_at_ns = None;
        self.snapshot.segment_started_ns = Some(clock_at_ns);
        self.journal.persist(&self.snapshot)?;
        if let Some(payload) = gap_payload {
            self.journal.event("capture_gap_recorded", payload)?;
        }
        self.journal.event(
            "capture_resumed",
            BTreeMap::from([("clock_at_ns".to_owned(), json!(clock_at_ns))]),
        )
    }

    pub fn source_lost(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        self.require_phase(CapturePhase::Recording)?;
        self.close_native(SegmentCloseReason::SourceLoss, clock_at_ns, true)?;
        self.snapshot.phase = CapturePhase::SourceLost;
        self.snapshot.segment_started_ns = None;
        self.snapshot.paused_at_ns = Some(clock_at_ns);
        self.snapshot.notices.push(CaptureNotice {
            code: "source_lost".to_owned(),
            message: "Selected meeting source stopped producing audio".to_owned(),
            clock_at_ns,
        });
        self.journal.persist(&self.snapshot)?;
        self.journal.event(
            "capture_source_lost",
            BTreeMap::from([("clock_at_ns".to_owned(), json!(clock_at_ns))]),
        )
    }

    pub fn stop(
        &mut self,
        clock_at_ns: u64,
        finalizer: &dyn CaptureFinalizer,
    ) -> Result<RecordingEnvelope, CaptureError> {
        match self.snapshot.phase {
            CapturePhase::Recording => {
                self.close_native(SegmentCloseReason::Stop, clock_at_ns, false)?;
            }
            CapturePhase::Paused | CapturePhase::SourceLost => {
                self.close_trailing_gap(clock_at_ns)?;
            }
            CapturePhase::Interrupted | CapturePhase::Finalizing => {}
            _ => {
                return Err(CaptureError::local(
                    "invalid_capture_state",
                    "capture cannot stop from its current state",
                ));
            }
        }
        self.snapshot.phase = CapturePhase::Finalizing;
        self.snapshot.segment_started_ns = None;
        self.snapshot.paused_at_ns = None;
        self.journal.persist(&self.snapshot)?;
        self.journal.event(
            "capture_stopped",
            BTreeMap::from([(
                "observed_duration_ms".to_owned(),
                json!(self.snapshot.observed_duration_ms()),
            )]),
        )?;
        self.finalize(finalizer)
    }

    pub fn finalize_recovered(
        &mut self,
        finalizer: &dyn CaptureFinalizer,
    ) -> Result<RecordingEnvelope, CaptureError> {
        if self.native.is_some() || self.snapshot.segments.is_empty() {
            return Err(CaptureError::local(
                "recovery_not_finalizable",
                "recovered capture has no closed segments to finalize",
            ));
        }
        self.snapshot.phase = CapturePhase::Finalizing;
        self.journal.persist(&self.snapshot)?;
        self.journal.event(
            "capture_recovery_finalizing",
            BTreeMap::from([(
                "observed_duration_ms".to_owned(),
                json!(self.snapshot.observed_duration_ms()),
            )]),
        )?;
        self.finalize(finalizer)
    }

    fn finalize(
        &mut self,
        finalizer: &dyn CaptureFinalizer,
    ) -> Result<RecordingEnvelope, CaptureError> {
        let finalized = finalizer.finalize(&self.snapshot, &self.journal.package)?;
        self.validate_finalized(&finalized)?;
        self.inbox.persist_envelope(&finalized.envelope)?;
        let event = InboxEvent::new(
            self.snapshot.recording_id,
            "capture_finalized",
            Utc::now().fixed_offset(),
            BTreeMap::from([(
                "duration_ms".to_owned(),
                json!(finalized.envelope.duration_ms),
            )]),
        )?;
        self.inbox.append_event(&event)?;
        self.snapshot.phase = CapturePhase::Stopped;
        self.journal.persist(&self.snapshot)?;
        Ok(finalized.envelope)
    }

    fn validate_finalized(&self, finalized: &FinalizedCapture) -> Result<(), CaptureError> {
        let envelope = &finalized.envelope;
        envelope.validate()?;
        if envelope.recording_id != self.snapshot.recording_id
            || envelope.source != self.snapshot.plan.recording_source()
            || envelope.captured_at != self.snapshot.wall_started_at
            || envelope.imported_name.is_some()
            || envelope.job.state != JobState::Ready
            || envelope.duration_ms != finalized.normalized.duration_ms
            || envelope.normalized_audio.as_deref()
                != Some(finalized.normalized.relative_path.as_str())
            || envelope.normalized_sha256.as_deref() != Some(finalized.normalized.sha256.as_str())
        {
            return Err(CaptureError::local(
                "invalid_finalizer_output",
                "capture finalizer output does not match the session",
            ));
        }
        let normalized = self
            .inbox
            .hash_package_file(envelope.recording_id, &finalized.normalized.relative_path)?;
        if normalized.sha256 != finalized.normalized.sha256
            || normalized.size_bytes != finalized.normalized.size_bytes
        {
            return Err(CaptureError::local(
                "invalid_finalizer_output",
                "normalized artifact does not match finalizer metadata",
            ));
        }
        for track in &envelope.tracks {
            let digest = self
                .inbox
                .hash_package_file(envelope.recording_id, &track.relative_path)?;
            if digest.sha256 != track.sha256 {
                return Err(CaptureError::local(
                    "invalid_finalizer_output",
                    "final track does not match its envelope hash",
                ));
            }
        }
        Ok(())
    }

    fn close_native(
        &mut self,
        reason: SegmentCloseReason,
        clock_at_ns: u64,
        allow_missing_roles: bool,
    ) -> Result<(), CaptureError> {
        let segment_start = self.snapshot.segment_started_ns.ok_or_else(|| {
            CaptureError::local("invalid_snapshot", "recording has no active segment start")
        })?;
        if clock_at_ns <= segment_start {
            return Err(CaptureError::local(
                "invalid_clock",
                "segment close clock must follow its start",
            ));
        }
        let closed = self.native_mut()?.close_segment(reason, clock_at_ns)?;
        let roles: HashSet<_> = closed.iter().map(|segment| segment.role).collect();
        if !allow_missing_roles
            && self
                .snapshot
                .plan
                .required_roles()
                .iter()
                .any(|role| !roles.contains(role))
        {
            return Err(CaptureError::local(
                "missing_capture_track",
                "native backend omitted a required capture track",
            ));
        }
        for segment in closed {
            segment.validate()?;
            if segment.clock_start_ns != segment_start || segment.clock_end_ns != clock_at_ns {
                return Err(CaptureError::local(
                    "invalid_segment",
                    "native segment does not cover its declared active clock interval",
                ));
            }
            verify_segment_file(&self.inbox, self.snapshot.recording_id, &segment)?;
            self.snapshot.segments.push(segment);
        }
        self.snapshot.validate()?;
        self.journal.persist(&self.snapshot)
    }

    fn close_trailing_gap(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        let start = self.snapshot.paused_at_ns.ok_or_else(|| {
            CaptureError::local("invalid_snapshot", "paused capture has no gap start")
        })?;
        if clock_at_ns < start {
            return Err(CaptureError::local(
                "invalid_clock",
                "stop clock precedes pause clock",
            ));
        }
        if clock_at_ns > start {
            self.snapshot.gaps.push(CaptureGap {
                reason: if self.snapshot.phase == CapturePhase::Paused {
                    GapReason::Pause
                } else {
                    GapReason::SourceLoss
                },
                clock_start_ns: start,
                clock_end_ns: clock_at_ns,
            });
        }
        Ok(())
    }

    fn native_mut(&mut self) -> Result<&mut (dyn NativeCaptureStream + '_), CaptureError> {
        match self.native.as_deref_mut() {
            Some(native) => Ok(native),
            None => Err(CaptureError::local(
                "backend_unavailable",
                "native capture stream is unavailable after recovery",
            )),
        }
    }

    fn require_phase(&self, expected: CapturePhase) -> Result<(), CaptureError> {
        if self.snapshot.phase == expected {
            Ok(())
        } else {
            Err(CaptureError::local(
                "invalid_capture_state",
                "capture action is unavailable in the current state",
            ))
        }
    }
}

pub(crate) fn minimum_capture_free_bytes(plan: &CapturePlan) -> u64 {
    let raw_tracks = u64::try_from(plan.required_roles().len()).unwrap_or(u64::MAX);
    let raw = DESKTOP_CAPTURE_SAMPLE_RATE
        .saturating_mul(DESKTOP_CAPTURE_MAX_CHANNELS)
        .saturating_mul(PCM16_BYTES_PER_SAMPLE)
        .saturating_mul(CAPTURE_TARGET_SECONDS)
        .saturating_mul(raw_tracks);
    let normalized = NORMALIZED_CAPTURE_SAMPLE_RATE
        .saturating_mul(PCM16_BYTES_PER_SAMPLE)
        .saturating_mul(CAPTURE_TARGET_SECONDS);
    raw.saturating_add(normalized)
        .saturating_add(CAPTURE_STORAGE_RESERVE_BYTES)
}

pub(crate) fn validate_capture_capacity(
    available_bytes: u64,
    plan: &CapturePlan,
) -> Result<(), CaptureError> {
    if available_bytes < minimum_capture_free_bytes(plan) {
        return Err(CaptureError::local(
            "capture_storage_low",
            "not enough free storage for a two-hour capture",
        ));
    }
    Ok(())
}

fn insert_non_overlapping_gap(gaps: &mut Vec<CaptureGap>, mut candidate: CaptureGap) {
    gaps.retain(|existing| {
        let overlaps = candidate.clock_start_ns < existing.clock_end_ns
            && existing.clock_start_ns < candidate.clock_end_ns;
        if overlaps && existing.reason == candidate.reason {
            candidate.clock_start_ns = candidate.clock_start_ns.min(existing.clock_start_ns);
            candidate.clock_end_ns = candidate.clock_end_ns.max(existing.clock_end_ns);
            false
        } else {
            true
        }
    });
    gaps.push(candidate);
    gaps.sort_by_key(|gap| gap.clock_start_ns);
}

fn role_name(role: TrackRole) -> &'static str {
    match role {
        TrackRole::Microphone => "microphone",
        TrackRole::System => "system",
        TrackRole::Mixed => "mixed",
        TrackRole::Imported => "imported",
    }
}

fn verify_segment_file(
    inbox: &Inbox,
    recording_id: Uuid,
    segment: &SegmentMetadata,
) -> Result<FileDigest, CaptureError> {
    let digest = inbox.hash_package_file(recording_id, &segment.relative_path)?;
    if digest.sha256 != segment.sha256 || digest.size_bytes == 0 {
        return Err(CaptureError::local(
            "segment_integrity",
            "capture segment does not match its durable metadata",
        ));
    }
    Ok(digest)
}

#[derive(Debug)]
pub struct CaptureError {
    pub code: &'static str,
    pub message: String,
}

impl CaptureError {
    pub fn local(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CaptureError {}

impl From<CaptureModelError> for CaptureError {
    fn from(error: CaptureModelError) -> Self {
        Self::local(error.code, error.message)
    }
}

impl From<crate::ingest::envelope::EnvelopeValidationError> for CaptureError {
    fn from(_: crate::ingest::envelope::EnvelopeValidationError) -> Self {
        Self::local("invalid_envelope", "capture envelope is invalid")
    }
}

impl From<crate::ingest::inbox::InboxError> for CaptureError {
    fn from(_: crate::ingest::inbox::InboxError) -> Self {
        Self::local("inbox_error", "capture inbox operation failed")
    }
}

impl From<std::io::Error> for CaptureError {
    fn from(_: std::io::Error) -> Self {
        Self::local("capture_io", "capture journal I/O failed")
    }
}

impl From<serde_json::Error> for CaptureError {
    fn from(_: serde_json::Error) -> Self {
        Self::local("capture_json", "capture journal JSON is invalid")
    }
}

struct CaptureJournal {
    recording_id: Uuid,
    package: PathBuf,
}

impl CaptureJournal {
    fn create(inbox: Arc<Inbox>, recording_id: Uuid) -> Result<Self, CaptureError> {
        validate_inbox_root(inbox.root())?;
        let package = inbox.root().join(recording_id.to_string());
        if package.exists() {
            return Err(CaptureError::local(
                "recording_exists",
                "capture recording package already exists",
            ));
        }
        fs::create_dir(&package)?;
        if fs::canonicalize(&package)?.parent() != Some(inbox.root()) {
            return Err(CaptureError::local(
                "invalid_capture_package",
                "capture package escaped the inbox root",
            ));
        }
        fs::create_dir(package.join("tracks"))?;
        fs::create_dir(package.join("derived"))?;
        Ok(Self {
            recording_id,
            package,
        })
    }

    fn open(inbox: Arc<Inbox>, recording_id: Uuid) -> Result<Self, CaptureError> {
        validate_inbox_root(inbox.root())?;
        let package = inbox.root().join(recording_id.to_string());
        if fs::symlink_metadata(&package)?.file_type().is_symlink() || !package.is_dir() {
            return Err(CaptureError::local(
                "invalid_capture_package",
                "capture package is not a plain directory",
            ));
        }
        if fs::canonicalize(&package)?.parent() != Some(inbox.root()) {
            return Err(CaptureError::local(
                "invalid_capture_package",
                "capture package escaped the inbox root",
            ));
        }
        Ok(Self {
            recording_id,
            package,
        })
    }

    fn persist(&self, snapshot: &CaptureSnapshot) -> Result<(), CaptureError> {
        snapshot.validate()?;
        self.ensure_event_capacity(CAPTURE_EVENT_RESERVE_BYTES)?;
        let mut bytes = serde_json::to_vec_pretty(snapshot)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_CAPTURE_SNAPSHOT_BYTES {
            return Err(CaptureError::local(
                "capture_journal_too_large",
                "capture snapshot exceeds the recovery size limit",
            ));
        }
        atomic_replace(&self.package.join(SNAPSHOT_FILE), &bytes)
    }

    fn load(&self) -> Result<CaptureSnapshot, CaptureError> {
        let path = self.package.join(SNAPSHOT_FILE);
        let bytes =
            read_bounded_capture_file(&path, MAX_CAPTURE_SNAPSHOT_BYTES, "capture snapshot")?;
        let snapshot: CaptureSnapshot = serde_json::from_slice(&bytes)?;
        if snapshot.recording_id != self.recording_id {
            return Err(CaptureError::local(
                "invalid_capture_package",
                "capture snapshot recording ID does not match its directory",
            ));
        }
        Ok(snapshot)
    }

    fn event(&self, kind: &str, payload: BTreeMap<String, Value>) -> Result<(), CaptureError> {
        let event = InboxEvent::new(self.recording_id, kind, Utc::now().fixed_offset(), payload)?;
        let path = self.package.join(EVENTS_FILE);
        reject_symlink_if_present(&path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        repair_event_tail(&mut file, self.recording_id)?;
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_CAPTURE_EVENT_BYTES {
            return Err(CaptureError::local(
                "capture_journal_too_large",
                "capture event exceeds the per-event size limit",
            ));
        }
        let current_length = file.metadata()?.len();
        if current_length
            .checked_add(bytes.len() as u64)
            .is_none_or(|length| length > MAX_CAPTURE_EVENTS_BYTES)
        {
            return Err(CaptureError::local(
                "capture_journal_too_large",
                "capture event journal exceeds the recovery size limit",
            ));
        }
        file.write_all(&bytes)?;
        file.sync_data()?;
        Ok(())
    }

    fn validate_events(&self) -> Result<(), CaptureError> {
        let path = self.package.join(EVENTS_FILE);
        reject_symlink_if_present(&path)?;
        let mut file = match OpenOptions::new().read(true).append(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        repair_event_tail(&mut file, self.recording_id)
    }

    fn ensure_event_capacity(&self, required_bytes: u64) -> Result<(), CaptureError> {
        let path = self.package.join(EVENTS_FILE);
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if required_bytes <= MAX_CAPTURE_EVENTS_BYTES {
                    return Ok(());
                }
                return Err(CaptureError::local(
                    "capture_journal_too_large",
                    "capture event journal cannot reserve recovery capacity",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CaptureError::local(
                "invalid_capture_package",
                "capture event journal must be a regular file",
            ));
        }
        if metadata
            .len()
            .checked_add(required_bytes)
            .is_none_or(|length| length > MAX_CAPTURE_EVENTS_BYTES)
        {
            return Err(CaptureError::local(
                "capture_journal_too_large",
                "capture event journal cannot reserve recovery capacity",
            ));
        }
        Ok(())
    }
}

fn validate_inbox_root(root: &Path) -> Result<(), CaptureError> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || fs::canonicalize(root)? != root {
        return Err(CaptureError::local(
            "invalid_capture_package",
            "capture inbox root is not a stable plain directory",
        ));
    }
    Ok(())
}

fn repair_event_tail(file: &mut File, recording_id: Uuid) -> Result<(), CaptureError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(CaptureError::local(
            "invalid_capture_package",
            "capture event journal must be a regular file",
        ));
    }
    if metadata.len() > MAX_CAPTURE_EVENTS_BYTES {
        return Err(CaptureError::local(
            "capture_journal_too_large",
            "capture event journal exceeds the recovery size limit",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(MAX_CAPTURE_EVENTS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CAPTURE_EVENTS_BYTES {
        return Err(CaptureError::local(
            "capture_journal_too_large",
            "capture event journal exceeds the recovery size limit",
        ));
    }
    let complete_length = if bytes.is_empty() || bytes.ends_with(b"\n") {
        bytes.len()
    } else {
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1)
    };
    for line in bytes[..complete_length].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let event: InboxEvent = serde_json::from_slice(line)?;
        event.validate()?;
        if event.recording_id != recording_id {
            return Err(CaptureError::local(
                "invalid_capture_events",
                "capture event belongs to another recording",
            ));
        }
    }
    if complete_length != bytes.len() {
        let tail = &bytes[complete_length..];
        let preserve_tail = serde_json::from_slice::<InboxEvent>(tail)
            .ok()
            .is_some_and(|event| event.recording_id == recording_id && event.validate().is_ok());
        if preserve_tail {
            file.write_all(b"\n")?;
        } else {
            file.set_len(complete_length as u64)?;
        }
        file.sync_data()?;
    }
    Ok(())
}

fn read_bounded_capture_file(
    path: &Path,
    maximum_bytes: u64,
    label: &'static str,
) -> Result<Vec<u8>, CaptureError> {
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(CaptureError::local(
            "invalid_capture_package",
            format!("{label} must be a regular file"),
        ));
    }
    if path_metadata.len() > maximum_bytes {
        return Err(CaptureError::local(
            "capture_journal_too_large",
            format!("{label} exceeds the recovery size limit"),
        ));
    }
    let file = File::open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(CaptureError::local(
            "invalid_capture_package",
            format!("{label} must remain a regular file"),
        ));
    }
    if opened_metadata.len() > maximum_bytes {
        return Err(CaptureError::local(
            "capture_journal_too_large",
            format!("{label} exceeds the recovery size limit"),
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened_metadata.len().min(maximum_bytes)).unwrap_or_default(),
    );
    file.take(maximum_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(CaptureError::local(
            "capture_journal_too_large",
            format!("{label} exceeds the recovery size limit"),
        ));
    }
    Ok(bytes)
}

fn valid_event_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn reject_symlink_if_present(path: &Path) -> Result<(), CaptureError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(CaptureError::local(
            "invalid_capture_package",
            "capture files must not be symbolic links",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), CaptureError> {
    reject_symlink_if_present(path)?;
    let parent = path.parent().ok_or_else(|| {
        CaptureError::local("invalid_capture_package", "capture file has no parent")
    })?;
    let temporary = parent.join(format!(".{SNAPSHOT_FILE}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    replace_file(&temporary, path)?;
    sync_directory(parent)
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), CaptureError> {
    fs::rename(source, destination)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> Result<(), CaptureError> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), CaptureError> {
    File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), CaptureError> {
    Ok(())
}
