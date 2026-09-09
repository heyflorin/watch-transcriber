use std::collections::HashMap;
use std::f64::consts::TAU;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::Mutex;

use chrono::{DateTime, TimeDelta};
use tempfile::TempDir;

use super::*;
use crate::ingest::envelope::{
    AudioTrack, CaptureScope, CaptureWarning, JobStatus, Platform, RecordingEnvelope, TrackRole,
    RECORDING_ENVELOPE_VERSION,
};
use crate::ingest::inbox::{hash_file_streaming, Inbox};
use crate::ingest::state::JobState;

const SAMPLE_RATE: u32 = 100;
const MIC_HZ: f64 = 10.0;
const SYSTEM_HZ: f64 = 20.0;

fn microphone() -> AudioInputSelection {
    AudioInputSelection {
        id: "fabricated-mic".to_owned(),
        label: "Fabricated microphone".to_owned(),
    }
}

fn consent() -> ConsentAcknowledgements {
    ConsentAcknowledgements {
        microphone: true,
        selected_source: true,
        whole_browser_warning: true,
        all_system_audio: true,
    }
}

fn plan(mode: CaptureMode) -> CapturePlan {
    let target = match mode {
        CaptureMode::VoiceMemo => CaptureTarget::VoiceMemo {
            microphone: microphone(),
        },
        CaptureMode::Meeting => CaptureTarget::Meeting {
            microphone: microphone(),
            source: MeetingSource::NativeApplication {
                id: "fabricated.app".to_owned(),
                label: "Fabricated Meeting".to_owned(),
            },
        },
        CaptureMode::SystemCapture => CaptureTarget::SystemCapture {
            microphone: microphone(),
            output: AudioOutputSelection {
                id: "default-output".to_owned(),
                label: "All system audio".to_owned(),
            },
        },
    };
    CapturePlan {
        mode,
        platform: Platform::Macos,
        target,
        consent: consent(),
    }
}

#[test]
fn two_hour_storage_floor_scales_with_desktop_track_count() {
    let voice = plan(CaptureMode::VoiceMemo);
    let meeting = plan(CaptureMode::Meeting);
    let voice_minimum = super::session::minimum_capture_free_bytes(&voice);
    let meeting_minimum = super::session::minimum_capture_free_bytes(&meeting);
    assert!(voice_minimum >= 2 * 1024 * 1024 * 1024);
    assert!(meeting_minimum > voice_minimum);
    assert_eq!(
        super::session::validate_capture_capacity(meeting_minimum - 1, &meeting)
            .unwrap_err()
            .code,
        "capture_storage_low"
    );
    assert!(super::session::validate_capture_capacity(meeting_minimum, &meeting).is_ok());
}

fn wall_start() -> DateTime<chrono::FixedOffset> {
    DateTime::parse_from_rfc3339("2026-09-02T09:00:00-07:00").unwrap()
}

fn inbox(temp: &TempDir) -> Arc<Inbox> {
    let archive = temp.path().join("archive-data");
    fs::create_dir_all(&archive).unwrap();
    Arc::new(Inbox::open(temp.path().join("app-data"), archive).unwrap())
}

struct SyntheticBackend {
    starts: AtomicUsize,
}

impl SyntheticBackend {
    fn new() -> Self {
        Self {
            starts: AtomicUsize::new(0),
        }
    }
}

impl DesktopCaptureBackend for SyntheticBackend {
    fn preflight(&self, plan: &CapturePlan) -> Result<CapturePreflight, CaptureError> {
        Ok(CapturePreflight {
            backend_available: true,
            microphone_permission: PermissionState::Granted,
            system_audio_permission: if plan.mode == CaptureMode::VoiceMemo {
                PermissionState::NotRequired
            } else {
                PermissionState::Granted
            },
            selected_source: SourceAvailability::Available,
            warnings: Vec::new(),
        })
    }

    fn start(
        &self,
        context: CaptureStartContext,
    ) -> Result<Box<dyn NativeCaptureStream>, CaptureError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(SyntheticStream {
            package: context.package_directory,
            roles: context.plan.required_roles().to_vec(),
            active_start_ns: Some(context.monotonic_started_ns),
            sequences: HashMap::new(),
        }))
    }
}

struct SyntheticStream {
    package: PathBuf,
    roles: Vec<TrackRole>,
    active_start_ns: Option<u64>,
    sequences: HashMap<TrackRole, u32>,
}

impl NativeCaptureStream for SyntheticStream {
    fn close_segment(
        &mut self,
        reason: SegmentCloseReason,
        clock_at_ns: u64,
    ) -> Result<Vec<SegmentMetadata>, CaptureError> {
        let start = self.active_start_ns.ok_or_else(|| {
            CaptureError::local("synthetic_state", "synthetic stream is not active")
        })?;
        let mut segments = Vec::new();
        for role in &self.roles {
            let sequence = self.sequences.entry(*role).or_insert(0);
            let role_name = match role {
                TrackRole::Microphone => "mic",
                TrackRole::System => "system",
                _ => return Err(CaptureError::local("synthetic_role", "unexpected role")),
            };
            let relative_path = format!("tracks/{role_name}-{sequence:04}.wav");
            let frames =
                ((clock_at_ns - start) / 1_000_000_000).saturating_mul(u64::from(SAMPLE_RATE));
            let frequency = if *role == TrackRole::Microphone {
                MIC_HZ
            } else {
                SYSTEM_HZ
            };
            write_tone_wav(
                &self.package.join(&relative_path),
                SAMPLE_RATE,
                frames as usize,
                frequency,
            );
            let digest = hash_file_streaming(self.package.join(&relative_path))?;
            segments.push(SegmentMetadata {
                role: *role,
                sequence: *sequence,
                relative_path,
                codec: "pcm_s16le".to_owned(),
                sample_rate: SAMPLE_RATE,
                channels: 1,
                frames_written: frames,
                clock_start_ns: start,
                clock_end_ns: clock_at_ns,
                sha256: digest.sha256,
            });
            *sequence += 1;
        }
        self.active_start_ns = if reason == SegmentCloseReason::RollingLimit {
            Some(clock_at_ns)
        } else {
            None
        };
        Ok(segments)
    }

    fn resume(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        if self.active_start_ns.is_some() {
            return Err(CaptureError::local(
                "synthetic_state",
                "synthetic stream is already active",
            ));
        }
        self.active_start_ns = Some(clock_at_ns);
        Ok(())
    }
}

struct SignallingBackend;

impl DesktopCaptureBackend for SignallingBackend {
    fn preflight(&self, plan: &CapturePlan) -> Result<CapturePreflight, CaptureError> {
        SyntheticBackend::new().preflight(plan)
    }

    fn start(
        &self,
        context: CaptureStartContext,
    ) -> Result<Box<dyn NativeCaptureStream>, CaptureError> {
        Ok(Box::new(SignallingStream {
            inner: SyntheticStream {
                package: context.package_directory,
                roles: context.plan.required_roles().to_vec(),
                active_start_ns: Some(context.monotonic_started_ns),
                sequences: HashMap::new(),
            },
            signals: vec![NativeCaptureSignal::SourceLost {
                clock_at_ns: 1_000_000_000,
                code: "wasapi_source_exited",
                message: "Selected process exited".to_owned(),
            }],
        }))
    }
}

struct SignallingStream {
    inner: SyntheticStream,
    signals: Vec<NativeCaptureSignal>,
}

impl NativeCaptureStream for SignallingStream {
    fn close_segment(
        &mut self,
        reason: SegmentCloseReason,
        clock_at_ns: u64,
    ) -> Result<Vec<SegmentMetadata>, CaptureError> {
        self.inner.close_segment(reason, clock_at_ns)
    }

    fn resume(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        self.inner.resume(clock_at_ns)
    }

    fn take_signals(&mut self) -> Result<Vec<NativeCaptureSignal>, CaptureError> {
        Ok(std::mem::take(&mut self.signals))
    }
}

struct SyntheticFinalizer;

impl CaptureFinalizer for SyntheticFinalizer {
    fn finalize(
        &self,
        snapshot: &CaptureSnapshot,
        package: &Path,
    ) -> Result<FinalizedCapture, CaptureError> {
        let duration_ms = snapshot.observed_duration_ms();
        if duration_ms == 0 {
            return Err(CaptureError::local(
                "no_audio",
                "synthetic finalizer needs closed segments",
            ));
        }
        let total_frames = (duration_ms * u64::from(SAMPLE_RATE) / 1_000) as usize;
        let mut tracks = Vec::new();
        let mut role_samples = HashMap::new();
        for role in snapshot.plan.required_roles() {
            let mut samples = vec![0_i16; total_frames];
            for segment in snapshot
                .segments
                .iter()
                .filter(|segment| segment.role == *role)
            {
                let source = read_wav_samples(&package.join(&segment.relative_path));
                let offset = ((segment.clock_start_ns - snapshot.monotonic_started_ns)
                    * u64::from(SAMPLE_RATE)
                    / 1_000_000_000) as usize;
                samples[offset..offset + source.len()].copy_from_slice(&source);
            }
            let role_name = if *role == TrackRole::Microphone {
                "microphone"
            } else {
                "system"
            };
            let relative_path = format!("tracks/{role_name}-combined.wav");
            write_pcm_wav(&package.join(&relative_path), SAMPLE_RATE, &samples);
            let digest = hash_file_streaming(package.join(&relative_path))?;
            tracks.push(AudioTrack {
                role: *role,
                relative_path,
                codec: "pcm_s16le".to_owned(),
                sample_rate: SAMPLE_RATE,
                channels: 1,
                duration_ms,
                clock_start_ns: snapshot.monotonic_started_ns,
                sha256: digest.sha256,
            });
            role_samples.insert(*role, samples);
        }

        let mut mixed = vec![0_i16; total_frames];
        for index in 0..total_frames {
            let sum: i32 = role_samples
                .values()
                .map(|samples| i32::from(samples[index]))
                .sum();
            mixed[index] = (sum / role_samples.len() as i32) as i16;
        }
        let normalized_path = "derived/mixed.wav".to_owned();
        write_pcm_wav(&package.join(&normalized_path), SAMPLE_RATE, &mixed);
        let normalized_digest = hash_file_streaming(package.join(&normalized_path))?;
        let ended_at =
            snapshot.wall_started_at + TimeDelta::try_milliseconds(duration_ms as i64).unwrap();
        let warnings = snapshot
            .gaps
            .iter()
            .map(|gap| CaptureWarning {
                code: match gap.reason {
                    GapReason::Pause => "pause_gap",
                    GapReason::SourceLoss => "source_gap",
                    GapReason::DeviceDiscontinuity => "device_gap",
                }
                .to_owned(),
                message: "Capture contains an explicit silent gap".to_owned(),
                at_ms: ((gap.clock_start_ns - snapshot.monotonic_started_ns) / 1_000_000)
                    .min(duration_ms),
            })
            .chain(snapshot.notices.iter().map(|notice| {
                CaptureWarning {
                    code: notice.code.clone(),
                    message: notice.message.clone(),
                    at_ms: ((notice.clock_at_ns - snapshot.monotonic_started_ns) / 1_000_000)
                        .min(duration_ms),
                }
            }))
            .collect();
        let envelope = RecordingEnvelope {
            schema_version: RECORDING_ENVELOPE_VERSION,
            recording_id: snapshot.recording_id,
            source: snapshot.plan.recording_source(),
            captured_at: snapshot.wall_started_at,
            ended_at,
            duration_ms,
            tracks,
            normalized_audio: Some(normalized_path.clone()),
            normalized_sha256: Some(normalized_digest.sha256.clone()),
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
        envelope.validate()?;
        Ok(FinalizedCapture {
            envelope,
            normalized: NormalizedArtifact {
                relative_path: normalized_path,
                sha256: normalized_digest.sha256,
                size_bytes: normalized_digest.size_bytes,
                duration_ms,
            },
        })
    }
}

fn write_tone_wav(path: &Path, sample_rate: u32, frames: usize, frequency: f64) {
    let samples: Vec<i16> = (0..frames)
        .map(|index| {
            let phase = TAU * frequency * index as f64 / f64::from(sample_rate);
            (phase.sin() * 10_000.0) as i16
        })
        .collect();
    write_pcm_wav(path, sample_rate, &samples);
}

fn write_pcm_wav(path: &Path, sample_rate: u32, samples: &[i16]) {
    let data_length = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_length as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_length).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_length.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(path, bytes).unwrap();
}

fn read_wav_samples(path: &Path) -> Vec<i16> {
    fs::read(path).unwrap()[44..]
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect()
}

fn tone_power(samples: &[i16], sample_rate: u32, frequency: f64) -> f64 {
    let (sin_sum, cos_sum) =
        samples
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(sin_sum, cos_sum), (index, sample)| {
                let phase = TAU * frequency * index as f64 / f64::from(sample_rate);
                (
                    sin_sum + f64::from(*sample) * phase.sin(),
                    cos_sum + f64::from(*sample) * phase.cos(),
                )
            });
    sin_sum.hypot(cos_sum)
}

#[test]
fn consent_and_scope_validation_covers_all_three_modes() {
    assert!(plan(CaptureMode::VoiceMemo).validate().is_ok());
    assert!(plan(CaptureMode::Meeting).validate().is_ok());
    assert!(plan(CaptureMode::SystemCapture).validate().is_ok());

    let mut no_mic = plan(CaptureMode::VoiceMemo);
    no_mic.consent.microphone = false;
    assert_eq!(
        no_mic.validate().unwrap_err().code,
        "microphone_consent_required"
    );

    let mut browser = plan(CaptureMode::Meeting);
    browser.target = CaptureTarget::Meeting {
        microphone: microphone(),
        source: MeetingSource::WholeBrowser {
            id: "chrome".to_owned(),
            label: "Chrome".to_owned(),
        },
    };
    browser.consent.whole_browser_warning = false;
    assert_eq!(
        browser.validate().unwrap_err().code,
        "browser_scope_consent_required"
    );

    let mut system = plan(CaptureMode::SystemCapture);
    system.consent.all_system_audio = false;
    assert_eq!(
        system.validate().unwrap_err().code,
        "system_scope_consent_required"
    );

    let mut mismatch = plan(CaptureMode::VoiceMemo);
    mismatch.mode = CaptureMode::Meeting;
    assert_eq!(
        mismatch.validate().unwrap_err().code,
        "mode_target_mismatch"
    );
}

#[test]
fn preflight_requires_permissions_but_allows_an_explicitly_selected_silent_source() {
    let capture_plan = plan(CaptureMode::Meeting);
    let mut preflight = CapturePreflight {
        backend_available: true,
        microphone_permission: PermissionState::Granted,
        system_audio_permission: PermissionState::Granted,
        selected_source: SourceAvailability::Silent,
        warnings: vec!["selected source is currently silent".to_owned()],
    };
    assert!(preflight.ready_for(&capture_plan));
    preflight.microphone_permission = PermissionState::Denied;
    assert!(!preflight.ready_for(&capture_plan));
    preflight.microphone_permission = PermissionState::Granted;
    preflight.system_audio_permission = PermissionState::NotDetermined;
    assert!(!preflight.ready_for(&capture_plan));
}

#[test]
fn start_without_a_real_or_synthetic_backend_is_unavailable() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let error = match CaptureSession::start(
        Arc::clone(&inbox),
        None,
        plan(CaptureMode::VoiceMemo),
        wall_start(),
        0,
    ) {
        Ok(_) => panic!("capture unexpectedly started"),
        Err(error) => error,
    };
    assert_eq!(error.code, "backend_unavailable");
    assert_eq!(fs::read_dir(inbox.root()).unwrap().count(), 0);
}

#[test]
fn missing_scope_acknowledgement_never_reaches_the_backend_or_inbox() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let backend = SyntheticBackend::new();
    let mut capture_plan = plan(CaptureMode::Meeting);
    capture_plan.consent.selected_source = false;

    let error = match CaptureSession::start(
        Arc::clone(&inbox),
        Some(&backend),
        capture_plan,
        wall_start(),
        0,
    ) {
        Ok(_) => panic!("capture unexpectedly started"),
        Err(error) => error,
    };
    assert_eq!(error.code, "source_consent_required");
    assert_eq!(backend.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read_dir(inbox.root()).unwrap().count(), 0);
}

#[test]
fn rolling_pause_resume_and_stop_emit_aligned_dual_tone_artifacts() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let backend = SyntheticBackend::new();
    let mut session = CaptureSession::start(
        Arc::clone(&inbox),
        Some(&backend),
        plan(CaptureMode::Meeting),
        wall_start(),
        0,
    )
    .unwrap();
    session.pause(1_000_000_000).unwrap();
    session.resume(2_000_000_000).unwrap();
    let envelope = session.stop(3_000_000_000, &SyntheticFinalizer).unwrap();

    assert_eq!(envelope.duration_ms, 3_000);
    assert_eq!(envelope.source.capture_scope, CaptureScope::Application);
    assert_eq!(session.snapshot().segments.len(), 4);
    assert_eq!(session.snapshot().gaps.len(), 1);
    let first_mic = &session.snapshot().segments[0];
    let first_system = &session.snapshot().segments[1];
    assert_eq!(first_mic.clock_start_ns, first_system.clock_start_ns);
    assert_eq!(first_mic.clock_end_ns, first_system.clock_end_ns);

    let mixed = read_wav_samples(
        &inbox
            .root()
            .join(session.recording_id().to_string())
            .join("derived/mixed.wav"),
    );
    assert!(tone_power(&mixed[..100], SAMPLE_RATE, MIC_HZ) > 100_000.0);
    assert!(tone_power(&mixed[..100], SAMPLE_RATE, SYSTEM_HZ) > 100_000.0);
    assert!(mixed[100..200].iter().all(|sample| *sample == 0));
    assert!(inbox
        .load_events(session.recording_id())
        .unwrap()
        .iter()
        .any(|event| event.kind == "capture_finalized"));
}

#[test]
fn five_minute_tick_rolls_every_elapsed_boundary() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let backend = SyntheticBackend::new();
    let mut session = CaptureSession::start(
        inbox,
        Some(&backend),
        plan(CaptureMode::VoiceMemo),
        wall_start(),
        0,
    )
    .unwrap();

    assert_eq!(session.tick(2 * ROLLING_SEGMENT_NS + 1).unwrap(), 2);
    assert_eq!(session.snapshot().segments.len(), 2);
    assert_eq!(session.snapshot().segments[0].duration_ms(), 300_000);
    assert_eq!(session.snapshot().segments[1].sequence, 1);
}

#[test]
fn crash_recovery_counts_only_closed_segments_and_ignores_orphan_bytes() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let backend = SyntheticBackend::new();
    let mut session = CaptureSession::start(
        Arc::clone(&inbox),
        Some(&backend),
        plan(CaptureMode::VoiceMemo),
        wall_start(),
        0,
    )
    .unwrap();
    session.tick(ROLLING_SEGMENT_NS).unwrap();
    let id = session.recording_id();
    let package = inbox.root().join(id.to_string());
    write_tone_wav(
        &package.join("tracks/orphan-open.wav"),
        SAMPLE_RATE,
        6_000,
        MIC_HZ,
    );
    drop(session);

    let mut recovered = CaptureSession::recover(Arc::clone(&inbox), id).unwrap();
    assert_eq!(recovered.snapshot().phase, CapturePhase::Interrupted);
    assert_eq!(recovered.snapshot().segments.len(), 1);
    assert_eq!(recovered.snapshot().observed_duration_ms(), 300_000);
    let envelope = recovered.finalize_recovered(&SyntheticFinalizer).unwrap();
    assert_eq!(envelope.duration_ms, 300_000);
    assert!(!envelope
        .tracks
        .iter()
        .any(|track| track.relative_path.contains("orphan")));
}

#[test]
fn recovery_rejects_oversized_snapshot_before_parsing() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let backend = SyntheticBackend::new();
    let session = CaptureSession::start(
        Arc::clone(&inbox),
        Some(&backend),
        plan(CaptureMode::VoiceMemo),
        wall_start(),
        0,
    )
    .unwrap();
    let id = session.recording_id();
    drop(session);

    let snapshot_path = inbox
        .root()
        .join(id.to_string())
        .join("capture-session.json");
    fs::write(
        &snapshot_path,
        vec![b' '; super::session::MAX_CAPTURE_SNAPSHOT_BYTES as usize + 1],
    )
    .unwrap();

    let error = match CaptureSession::recover(Arc::clone(&inbox), id) {
        Ok(_) => panic!("oversized capture snapshot unexpectedly recovered"),
        Err(error) => error,
    };
    assert_eq!(error.code, "capture_journal_too_large");
    assert_eq!(
        fs::metadata(snapshot_path).unwrap().len(),
        super::session::MAX_CAPTURE_SNAPSHOT_BYTES + 1
    );
}

#[test]
fn recovery_rejects_oversized_event_journal_without_truncating_it() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let backend = SyntheticBackend::new();
    let session = CaptureSession::start(
        Arc::clone(&inbox),
        Some(&backend),
        plan(CaptureMode::VoiceMemo),
        wall_start(),
        0,
    )
    .unwrap();
    let id = session.recording_id();
    drop(session);

    let snapshot_path = inbox
        .root()
        .join(id.to_string())
        .join("capture-session.json");
    let snapshot_before = fs::read(&snapshot_path).unwrap();
    let events_path = inbox.root().join(id.to_string()).join("events.ndjson");
    fs::write(
        &events_path,
        vec![b'x'; super::session::MAX_CAPTURE_EVENTS_BYTES as usize + 1],
    )
    .unwrap();

    let error = match CaptureSession::recover(Arc::clone(&inbox), id) {
        Ok(_) => panic!("oversized capture event journal unexpectedly recovered"),
        Err(error) => error,
    };
    assert_eq!(error.code, "capture_journal_too_large");
    assert_eq!(
        fs::metadata(events_path).unwrap().len(),
        super::session::MAX_CAPTURE_EVENTS_BYTES + 1
    );
    assert_eq!(fs::read(snapshot_path).unwrap(), snapshot_before);
}

#[test]
fn recovery_reserves_event_capacity_before_mutating_the_snapshot() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let backend = SyntheticBackend::new();
    let session = CaptureSession::start(
        Arc::clone(&inbox),
        Some(&backend),
        plan(CaptureMode::VoiceMemo),
        wall_start(),
        0,
    )
    .unwrap();
    let id = session.recording_id();
    drop(session);

    let package = inbox.root().join(id.to_string());
    let snapshot_path = package.join("capture-session.json");
    let snapshot_before = fs::read(&snapshot_path).unwrap();
    let events_path = package.join("events.ndjson");
    let mut events = fs::read(&events_path).unwrap();
    assert_eq!(events.pop(), Some(b'\n'));
    let threshold = (super::session::MAX_CAPTURE_EVENTS_BYTES
        - super::session::CAPTURE_EVENT_RESERVE_BYTES
        + 1) as usize;
    events.resize(threshold - 1, b' ');
    events.push(b'\n');
    fs::write(&events_path, &events).unwrap();

    let error = match CaptureSession::recover(Arc::clone(&inbox), id) {
        Ok(_) => panic!("capture recovered without reserved event capacity"),
        Err(error) => error,
    };
    assert_eq!(error.code, "capture_journal_too_large");
    assert_eq!(fs::read(snapshot_path).unwrap(), snapshot_before);
    assert_eq!(fs::read(events_path).unwrap(), events);
}

#[test]
fn recovery_rejects_unbounded_capture_notice_collection() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let backend = SyntheticBackend::new();
    let session = CaptureSession::start(
        Arc::clone(&inbox),
        Some(&backend),
        plan(CaptureMode::VoiceMemo),
        wall_start(),
        0,
    )
    .unwrap();
    let id = session.recording_id();
    drop(session);

    let snapshot_path = inbox
        .root()
        .join(id.to_string())
        .join("capture-session.json");
    let mut snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(&snapshot_path).unwrap()).unwrap();
    snapshot["notices"] = serde_json::Value::Array(
        (0..=crate::ingest::envelope::MAX_CAPTURE_WARNINGS)
            .map(|_| {
                serde_json::json!({
                    "code": "source_lost",
                    "message": "bounded fabricated notice",
                    "clock_at_ns": 0
                })
            })
            .collect(),
    );
    fs::write(&snapshot_path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

    let error = match CaptureSession::recover(inbox, id) {
        Ok(_) => panic!("capture snapshot with unbounded notices unexpectedly recovered"),
        Err(error) => error,
    };
    assert_eq!(error.code, "capture_journal_too_large");
}

#[test]
fn source_loss_is_an_explicit_gap_before_resume() {
    let temp = TempDir::new().unwrap();
    let backend = SyntheticBackend::new();
    let mut session = CaptureSession::start(
        inbox(&temp),
        Some(&backend),
        plan(CaptureMode::Meeting),
        wall_start(),
        0,
    )
    .unwrap();
    session.source_lost(1_000_000_000).unwrap();
    assert_eq!(session.snapshot().phase, CapturePhase::SourceLost);
    session.resume(2_000_000_000).unwrap();
    assert_eq!(session.snapshot().gaps[0].reason, GapReason::SourceLoss);
    let envelope = session.stop(3_000_000_000, &SyntheticFinalizer).unwrap();
    assert!(envelope
        .capture_warnings
        .iter()
        .any(|warning| warning.code == "source_lost"));
}

#[test]
fn native_source_exit_signal_is_persisted_before_resume() {
    let temp = TempDir::new().unwrap();
    let inbox = inbox(&temp);
    let mut session = CaptureSession::start(
        Arc::clone(&inbox),
        Some(&SignallingBackend),
        plan(CaptureMode::Meeting),
        wall_start(),
        0,
    )
    .unwrap();

    assert_eq!(session.poll_native_signals().unwrap(), 1);
    assert_eq!(session.snapshot().phase, CapturePhase::SourceLost);
    assert!(session
        .snapshot()
        .notices
        .iter()
        .any(|notice| notice.code == "wasapi_source_exited"));
    assert!(inbox
        .load_events(session.recording_id())
        .unwrap()
        .iter()
        .any(|event| event.kind == "capture_native_signal"));
}

#[cfg(target_os = "macos")]
mod mac_boundary {
    use super::*;
    use crate::capture::macos::{
        MacCaptureFilter, MacCaptureSource, MacCaptureSourceKind, MacNativeBridge,
        MacOsCaptureAdapter, MacPermissionSnapshot,
    };

    struct Bridge {
        starts: AtomicUsize,
        filter: Mutex<Option<MacCaptureFilter>>,
        permission_requests: Mutex<Vec<(bool, bool)>>,
    }

    impl MacNativeBridge for Bridge {
        fn current_permissions(&self) -> Result<MacPermissionSnapshot, CaptureError> {
            Ok(MacPermissionSnapshot {
                microphone: PermissionState::Granted,
                screen_and_system_audio: PermissionState::Granted,
            })
        }

        fn enumerate_sources(&self) -> Result<Vec<MacCaptureSource>, CaptureError> {
            Ok(vec![
                MacCaptureSource {
                    id: "fabricated.app".to_owned(),
                    label: "Fabricated Meeting".to_owned(),
                    bundle_identifier: Some("example.fabricated".to_owned()),
                    process_id: Some(42),
                    kind: MacCaptureSourceKind::NativeApplication,
                    currently_audible: Some(true),
                },
                MacCaptureSource {
                    id: "chrome".to_owned(),
                    label: "Chrome".to_owned(),
                    bundle_identifier: Some("com.google.Chrome".to_owned()),
                    process_id: Some(43),
                    kind: MacCaptureSourceKind::BrowserApplication,
                    currently_audible: Some(true),
                },
            ])
        }

        fn request_permissions(
            &self,
            microphone: bool,
            screen_and_system_audio: bool,
        ) -> Result<MacPermissionSnapshot, CaptureError> {
            self.permission_requests
                .lock()
                .unwrap()
                .push((microphone, screen_and_system_audio));
            self.current_permissions()
        }

        fn start_filtered_stream(
            &self,
            context: CaptureStartContext,
            filter: MacCaptureFilter,
        ) -> Result<Box<dyn NativeCaptureStream>, CaptureError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            *self.filter.lock().unwrap() = Some(filter);
            Ok(Box::new(SyntheticStream {
                package: context.package_directory,
                roles: context.plan.required_roles().to_vec(),
                active_start_ns: Some(context.monotonic_started_ns),
                sequences: HashMap::new(),
            }))
        }
    }

    #[test]
    fn mac_adapter_builds_a_selected_application_filter_without_framework_calls() {
        let temp = TempDir::new().unwrap();
        let bridge = Arc::new(Bridge {
            starts: AtomicUsize::new(0),
            filter: Mutex::new(None),
            permission_requests: Mutex::new(Vec::new()),
        });
        let adapter = MacOsCaptureAdapter::new(Arc::clone(&bridge));
        assert!(adapter.request_permission_snapshot(false, false).is_err());
        assert_eq!(
            adapter
                .request_permission_snapshot(true, false)
                .unwrap()
                .microphone,
            PermissionState::Granted
        );
        assert_eq!(
            &*bridge.permission_requests.lock().unwrap(),
            &[(true, false)]
        );
        let capture_plan = plan(CaptureMode::Meeting);
        assert!(adapter
            .preflight(&capture_plan)
            .unwrap()
            .ready_for(&capture_plan));

        let _session =
            CaptureSession::start(inbox(&temp), Some(&adapter), capture_plan, wall_start(), 0)
                .unwrap();
        let filter = bridge.filter.lock().unwrap().clone().unwrap();
        assert_eq!(
            filter.selected_application_id.as_deref(),
            Some("fabricated.app")
        );
        assert!(filter.capture_microphone);
        assert!(filter.capture_system_audio);
        assert!(filter.exclude_echowall_audio);

        let mut browser_plan = plan(CaptureMode::Meeting);
        browser_plan.target = CaptureTarget::Meeting {
            microphone: microphone(),
            source: MeetingSource::WholeBrowser {
                id: "chrome".to_owned(),
                label: "Chrome".to_owned(),
            },
        };
        assert!(adapter
            .preflight(&browser_plan)
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("other tabs")));
        let _browser_session =
            CaptureSession::start(inbox(&temp), Some(&adapter), browser_plan, wall_start(), 10)
                .unwrap();
        assert_eq!(
            bridge
                .filter
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .selected_application_id
                .as_deref(),
            Some("chrome")
        );
        assert_eq!(bridge.starts.load(Ordering::SeqCst), 2);
    }
}
