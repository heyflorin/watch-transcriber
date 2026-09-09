use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::ingest::envelope::{Platform, RecordingEnvelope};
use crate::ingest::import::{DesktopImporter, DEFAULT_MAX_IMPORT_BYTES};
use crate::ingest::inbox::Inbox;
use crate::secrets::{ProcessingCredentials, ProcessingCredentialsState};

use super::direct::{ArchiveEffects, DirectEffects};
use super::engine::{EffectError, EffectErrorKind, EngineError, ProcessingEngine};
use super::local_models::LocalModelPackManager;
use super::local_worker::LocalWhisperWorker;
use super::{
    CanonicalBackupCheckpoint, ProcessingLedger, ProcessingState, ProcessingStore,
    PublicationBackend, PublicationProof, PublicationTargetPlan, RetryMode, TranscriptionBackend,
};

const REMOTE_CONFIRM: &str = "public-corpus-paid-provider-authorized";
const LOCAL_CONFIRM: &str = "public-corpus-local-model-authorized";
const WHISPER_CONFIRM: &str = "public-corpus-whisper-baseline-authorized";
const POLL_INTERVAL: Duration = Duration::from_secs(31);
const REMOTE_DEADLINE: Duration = Duration::from_secs(3 * 60 * 60);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QualityManifest {
    schema_version: u32,
    cases: Vec<QualityCase>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QualityCase {
    case_id: String,
    stratum: String,
    duration_ms: u64,
    ground_truth: String,
    miaoji: String,
    local: String,
    named_terms: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalTranscript {
    schema_version: u32,
    segments: Vec<CanonicalSegment>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalSegment {
    start_ms: u64,
    end_ms: u64,
    speaker: String,
    text: String,
}

#[derive(Clone, Copy, Debug)]
struct ExternalDiarizationInterval {
    start_ms: u64,
    end_ms: u64,
    speaker_slot: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct ExternalDiarizationMergeStats {
    lexical_words: usize,
    overlap_assigned_words: usize,
    point_assigned_words: usize,
    bridge_assigned_words: usize,
    unknown_lexical_words: usize,
    interpolated_zero_duration_words: usize,
}

#[derive(Clone, Debug, Default)]
struct ExternalDiarizationAggregate {
    cases: usize,
    duration_ms: u64,
    detected_speaker_count_exact: usize,
    diarization_segments: usize,
    lexical_words: usize,
    unknown_lexical_words: usize,
    cases_below_99_percent_coverage: usize,
    minimum_coverage_ppm: u64,
    overlap_assigned_words: usize,
    point_assigned_words: usize,
    bridge_assigned_words: usize,
    interpolated_zero_duration_words: usize,
    processing_seconds: f64,
}

struct QualityArchive;

#[async_trait]
impl ArchiveEffects for QualityArchive {
    fn plan(
        &self,
        _: &RecordingEnvelope,
        _: PublicationBackend,
    ) -> Result<Vec<PublicationTargetPlan>, EffectError> {
        Ok(vec![PublicationTargetPlan {
            id: "quality_matrix_local_proof".to_owned(),
            retry_mode: RetryMode::ReconcileBeforeRetry,
            required: true,
        }])
    }

    async fn publish(
        &self,
        _: &str,
        generation: u64,
        _: &RecordingEnvelope,
        _: &Value,
        _: &Value,
        artifact: &super::NormalizedArtifactCheckpoint,
    ) -> Result<PublicationProof, EffectError> {
        Ok(PublicationProof {
            locator: "local:quality-matrix".to_owned(),
            version: format!("fixture-{generation}"),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        })
    }

    async fn verify_backup(
        &self,
        _: PublicationBackend,
        generation: u64,
        _: &RecordingEnvelope,
        artifact: &super::NormalizedArtifactCheckpoint,
    ) -> Result<CanonicalBackupCheckpoint, EffectError> {
        Ok(CanonicalBackupCheckpoint {
            locator: "local:quality-matrix".to_owned(),
            version_id: format!("fixture-{generation}"),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
            proof_json: json!({"test_only": true}),
        })
    }
}

fn repository_root() -> PathBuf {
    fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("src-tauri must remain below desktop and the repository"),
    )
    .expect("repository root must be available")
}

fn authorized_root(variable: &str) -> PathBuf {
    let supplied = PathBuf::from(std::env::var(variable).expect("quality root must be supplied"));
    fs::create_dir_all(&supplied).expect("quality root must be creatable");
    let root = fs::canonicalize(supplied).expect("quality root must resolve");
    let allowed = repository_root().join("local-eval");
    assert!(
        root.starts_with(&allowed) && root != allowed,
        "quality data must remain below the ignored local-eval root"
    );
    assert!(
        !fs::symlink_metadata(&root)
            .expect("quality root metadata must be available")
            .file_type()
            .is_symlink(),
        "quality root must not be a symlink"
    );
    root
}

fn load_manifest(root: &Path) -> QualityManifest {
    let path = root.join("manifest.json");
    let metadata = fs::symlink_metadata(&path).expect("quality manifest must exist");
    assert!(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() < 1_048_576);
    let manifest: QualityManifest =
        serde_json::from_slice(&fs::read(path).expect("quality manifest must be readable"))
            .expect("quality manifest must match its closed schema");
    assert_eq!(manifest.schema_version, 1);
    let mut ids = HashSet::new();
    for case in &manifest.cases {
        assert!(ids.insert(case.case_id.as_str()));
        assert!(case.duration_ms > 0 && case.duration_ms <= 5 * 60 * 60 * 1_000);
        assert!(!case.ground_truth.is_empty() && !case.named_terms.is_empty());
        assert_eq!(case.miaoji, format!("outputs/miaoji/{}.json", case.case_id));
        assert_eq!(case.local, format!("outputs/local/{}.json", case.case_id));
        assert!(matches!(
            case.stratum.as_str(),
            "english" | "mandarin" | "mixed" | "overlap" | "long_form"
        ));
    }
    manifest
}

fn selected_cases(manifest: &QualityManifest) -> Vec<QualityCase> {
    match std::env::var("ECHOWALL_QUALITY_CASE_SET").as_deref() {
        Ok("all") => manifest.cases.clone(),
        Ok("pilot") => ["english", "mandarin", "mixed", "overlap", "long_form"]
            .iter()
            .map(|stratum| {
                manifest
                    .cases
                    .iter()
                    .find(|case| case.stratum == *stratum)
                    .cloned()
                    .expect("quality pilot requires every stratum")
            })
            .collect(),
        _ => panic!("quality case set must be exactly pilot or all"),
    }
}

fn workers(app_root: &Path) -> LocalWhisperWorker {
    let binaries = repository_root().join("desktop/src-tauri/binaries");
    LocalWhisperWorker::from_paths_for_test(
        app_root.to_path_buf(),
        binaries.join("echowall-whisper-worker-aarch64-apple-darwin"),
        binaries.join("echowall-diarization-worker-aarch64-apple-darwin"),
        binaries.join("echowall-summary-worker-aarch64-apple-darwin"),
        binaries.join("echowall-qwen-worker-aarch64-apple-darwin"),
    )
}

fn build_engine(
    matrix_root: &Path,
    app_root: &Path,
    credentials: Arc<ProcessingCredentialsState>,
) -> (
    Arc<Inbox>,
    Arc<ProcessingStore>,
    ProcessingEngine<DirectEffects<QualityArchive>>,
) {
    let archive_boundary = matrix_root.join("work/archive-boundary");
    fs::create_dir_all(&archive_boundary).unwrap();
    let inbox = Arc::new(Inbox::open(app_root, &archive_boundary).unwrap());
    let store = Arc::new(ProcessingStore::open(app_root, inbox.root(), &archive_boundary).unwrap());
    let effects = Arc::new(
        DirectEffects::production(credentials, Arc::new(QualityArchive), workers(app_root))
            .unwrap(),
    );
    let engine = ProcessingEngine::new(Arc::clone(&inbox), Arc::clone(&store), effects);
    (inbox, store, engine)
}

fn import_case(
    importer: &DesktopImporter,
    inbox: &Inbox,
    matrix_root: &Path,
    case: &QualityCase,
) -> Uuid {
    let audio = matrix_root
        .join("audio")
        .join(format!("{}.m4a", case.case_id));
    let imported = importer.import_paths(vec![audio.to_string_lossy().into_owned()]);
    let recording_id = Uuid::parse_str(
        imported.results[0]
            .recording_id
            .as_deref()
            .expect("quality audio must import"),
    )
    .unwrap();
    let envelope = inbox.load_envelope(recording_id).unwrap();
    assert!(envelope.duration_ms.abs_diff(case.duration_ms) <= 250);
    recording_id
}

// Historical evaluation timelines remain frozen even when the importer gains
// a more accurate codec-priming/padding interpretation. Validate nearby source
// metadata, but never rewrite already-scored corpus manifests in a live test.
fn validate_frozen_manifest_durations(
    inbox: &Inbox,
    manifest: &QualityManifest,
) -> Result<(), &'static str> {
    let mut app_durations = BTreeMap::new();
    for entry in fs::read_dir(inbox.root()).map_err(|_| "inbox_scan_failed")? {
        let entry = entry.map_err(|_| "inbox_scan_failed")?;
        if !entry.file_type().map_err(|_| "inbox_scan_failed")?.is_dir() {
            continue;
        }
        let path = entry.path().join("recording.json");
        let bytes = fs::read(path).map_err(|_| "inbox_manifest_failed")?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "inbox_manifest_failed")?;
        let Some(name) = value.get("imported_name").and_then(Value::as_str) else {
            continue;
        };
        let duration_ms = value
            .get("duration_ms")
            .and_then(Value::as_u64)
            .ok_or("inbox_manifest_failed")?;
        app_durations.insert(name.to_owned(), duration_ms);
    }
    for case in &manifest.cases {
        let name = format!("{}.m4a", case.case_id);
        if let Some(duration_ms) = app_durations.get(&name) {
            if duration_ms.abs_diff(case.duration_ms) > 250 {
                return Err("duration_mismatch");
            }
        }
    }
    Ok(())
}

#[test]
fn corrected_import_duration_never_rewrites_frozen_evaluation_timeline() {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("app");
    let archive = root.path().join("archive");
    let inbox = Inbox::open(&app, &archive).unwrap();
    let record = inbox.root().join(Uuid::new_v4().to_string());
    fs::create_dir(&record).unwrap();
    let metadata = record.join("recording.json");
    fs::write(
        &metadata,
        serde_json::to_vec(&json!({
            "imported_name":"mixed_10.m4a", "duration_ms":720261
        }))
        .unwrap(),
    )
    .unwrap();
    let manifest = QualityManifest {
        schema_version: 1,
        cases: vec![QualityCase {
            case_id: "mixed_10".into(),
            stratum: "mixed".into(),
            duration_ms: 720384,
            ground_truth: "truth.json".into(),
            miaoji: "miaoji.json".into(),
            local: "local.json".into(),
            named_terms: vec!["fixture".into()],
        }],
    };
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let frozen = root.path().join("manifest.json");
    fs::write(&frozen, &bytes).unwrap();
    validate_frozen_manifest_durations(&inbox, &manifest).unwrap();
    assert_eq!(serde_json::to_vec(&manifest).unwrap(), bytes);
    assert_eq!(fs::read(&frozen).unwrap(), bytes);
    fs::write(
        &metadata,
        br#"{"imported_name":"mixed_10.m4a","duration_ms":700000}"#,
    )
    .unwrap();
    assert_eq!(
        validate_frozen_manifest_durations(&inbox, &manifest),
        Err("duration_mismatch")
    );
    assert_eq!(fs::read(frozen).unwrap(), bytes);
}

fn value_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
            .or_else(|| value.as_str().and_then(|number| number.parse().ok()))
    })
}

fn canonical_transcript(value: &Value) -> Result<CanonicalTranscript, &'static str> {
    let source = value.as_array().ok_or("transcript_not_array")?;
    let mut segments: Vec<CanonicalSegment> = Vec::with_capacity(source.len());
    let mut pending_point_text = BTreeMap::<String, Vec<String>>::new();
    let mut last_start = 0;
    for item in source {
        let object = item.as_object().ok_or("segment_not_object")?;
        let start_ms = value_u64(object.get("start_time")).ok_or("start_missing")?;
        let end_ms = value_u64(object.get("end_time")).ok_or("end_missing")?;
        let text = object
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or("text_missing")?;
        let speaker = object
            .get("speaker")
            .and_then(Value::as_object)
            .and_then(|speaker| speaker.get("id"))
            .and_then(Value::as_str)
            .filter(|speaker| !speaker.is_empty())
            .ok_or("speaker_missing")?;
        if end_ms < start_ms || start_ms < last_start {
            return Err("timeline_invalid");
        }
        last_start = start_ms;
        if end_ms == start_ms {
            if let Some(previous) = segments
                .iter_mut()
                .rev()
                .find(|segment| segment.speaker == speaker)
            {
                previous.text.push(' ');
                previous.text.push_str(text);
            } else {
                pending_point_text
                    .entry(speaker.to_owned())
                    .or_default()
                    .push(text.to_owned());
            }
            continue;
        }
        let text = pending_point_text.remove(speaker).map_or_else(
            || text.to_owned(),
            |pending| format!("{} {text}", pending.join(" ")),
        );
        segments.push(CanonicalSegment {
            start_ms,
            end_ms,
            speaker: speaker.to_owned(),
            text,
        });
    }
    if segments.is_empty() || !pending_point_text.is_empty() {
        return Err("transcript_empty");
    }
    let mut by_speaker = BTreeMap::<String, Vec<CanonicalSegment>>::new();
    for segment in segments {
        let speaker_segments = by_speaker.entry(segment.speaker.clone()).or_default();
        if let Some(previous) = speaker_segments.last_mut() {
            if segment.start_ms < previous.end_ms {
                previous.end_ms = previous.end_ms.max(segment.end_ms);
                previous.text.push(' ');
                previous.text.push_str(&segment.text);
                continue;
            }
        }
        speaker_segments.push(segment);
    }
    let mut segments: Vec<_> = by_speaker.into_values().flatten().collect();
    segments.sort_by(|left, right| {
        left.start_ms
            .cmp(&right.start_ms)
            .then_with(|| left.end_ms.cmp(&right.end_ms))
            .then_with(|| left.speaker.cmp(&right.speaker))
    });
    Ok(CanonicalTranscript {
        schema_version: 1,
        segments,
    })
}

fn group_adjacent_canonical_turns(transcript: CanonicalTranscript) -> CanonicalTranscript {
    let mut grouped: Vec<CanonicalSegment> = Vec::with_capacity(transcript.segments.len());
    for segment in transcript.segments {
        if let Some(previous) = grouped.last_mut() {
            if previous.speaker == segment.speaker {
                previous.end_ms = previous.end_ms.max(segment.end_ms);
                previous.text.push(' ');
                previous.text.push_str(&segment.text);
                continue;
            }
        }
        grouped.push(segment);
    }
    CanonicalTranscript {
        schema_version: transcript.schema_version,
        segments: grouped,
    }
}

fn canonical_aligned_diagnostic(
    words: &[super::local_whisper::LocalAlignedWord],
) -> Result<CanonicalTranscript, &'static str> {
    let mut segments: Vec<CanonicalSegment> = Vec::with_capacity(words.len());
    let mut pending_leading = Vec::new();
    for word in words {
        if word.text.trim().is_empty() || word.end_ms < word.start_ms {
            return Err("aligned_word_invalid");
        }
        if word.end_ms == word.start_ms {
            if let Some(previous) = segments.last_mut() {
                previous.text.push(' ');
                previous.text.push_str(&word.text);
                previous.speaker = "local_unknown".to_owned();
            } else {
                pending_leading.push(word.text.clone());
            }
            continue;
        }
        let mut text = word.text.clone();
        let mut speaker = word
            .speaker_id
            .clone()
            .unwrap_or_else(|| "local_unknown".to_owned());
        if !pending_leading.is_empty() {
            pending_leading.push(text);
            text = pending_leading.join(" ");
            pending_leading.clear();
            speaker = "local_unknown".to_owned();
        }
        segments.push(CanonicalSegment {
            start_ms: word.start_ms,
            end_ms: word.end_ms,
            speaker,
            text,
        });
    }
    if segments.is_empty() || !pending_leading.is_empty() {
        return Err("transcript_empty");
    }
    let mut by_speaker = BTreeMap::<String, Vec<CanonicalSegment>>::new();
    for segment in segments {
        let speaker_segments = by_speaker.entry(segment.speaker.clone()).or_default();
        if let Some(previous) = speaker_segments.last_mut() {
            if segment.start_ms < previous.end_ms {
                previous.end_ms = previous.end_ms.max(segment.end_ms);
                previous.text.push(' ');
                previous.text.push_str(&segment.text);
                continue;
            }
        }
        speaker_segments.push(segment);
    }
    let mut segments: Vec<_> = by_speaker.into_values().flatten().collect();
    segments.sort_by(|left, right| {
        left.start_ms
            .cmp(&right.start_ms)
            .then_with(|| left.end_ms.cmp(&right.end_ms))
            .then_with(|| left.speaker.cmp(&right.speaker))
    });
    Ok(group_adjacent_canonical_turns(CanonicalTranscript {
        schema_version: 1,
        segments,
    }))
}

fn external_diarization_intervals(
    path: &Path,
    audio_duration_ms: u64,
) -> Result<(usize, Vec<ExternalDiarizationInterval>, Option<f64>), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "external_diarization_missing")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > 4 * 1024 * 1024
    {
        return Err("external_diarization_rejected");
    }
    let value: Value =
        serde_json::from_slice(&fs::read(path).map_err(|_| "external_diarization_read")?)
            .map_err(|_| "external_diarization_json")?;
    let object = value.as_object().ok_or("external_diarization_not_object")?;
    let source = object
        .get("segments")
        .and_then(Value::as_array)
        .filter(|segments| !segments.is_empty() && segments.len() <= 20_000)
        .ok_or("external_diarization_segments")?;
    if object.get("segmentCount").and_then(Value::as_u64) != Some(source.len() as u64) {
        return Err("external_diarization_count");
    }
    let processing_seconds = object
        .get("processingTimeSeconds")
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .ok_or("external_diarization_timing")?;
    let mut intervals = Vec::with_capacity(source.len());
    let mut speakers = HashSet::new();
    for item in source {
        let segment = item.as_object().ok_or("external_diarization_segment")?;
        let start_seconds = segment
            .get("startTimeSeconds")
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .ok_or("external_diarization_start")?;
        let end_seconds = segment
            .get("endTimeSeconds")
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite() && *seconds > start_seconds)
            .ok_or("external_diarization_end")?;
        let speaker_index = segment
            .get("speakerIndex")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .filter(|index| *index < 16)
            .ok_or("external_diarization_speaker")?;
        if segment.get("speaker").and_then(Value::as_str)
            != Some(format!("Speaker {speaker_index}").as_str())
        {
            return Err("external_diarization_speaker");
        }
        let start_ms = (start_seconds * 1_000.0).round() as u64;
        let raw_end_ms = (end_seconds * 1_000.0).round() as u64;
        if start_ms >= audio_duration_ms || raw_end_ms > audio_duration_ms.saturating_add(250) {
            return Err("external_diarization_timeline");
        }
        let end_ms = raw_end_ms.min(audio_duration_ms);
        if end_ms <= start_ms {
            return Err("external_diarization_timeline");
        }
        speakers.insert(speaker_index);
        intervals.push(ExternalDiarizationInterval {
            start_ms,
            end_ms,
            speaker_slot: speaker_index,
        });
    }
    intervals.sort_by_key(|interval| (interval.start_ms, interval.end_ms, interval.speaker_slot));
    if speakers.is_empty()
        || speakers.len() > 16
        || speakers.iter().copied().max().unwrap_or(0) + 1 != speakers.len()
    {
        return Err("external_diarization_speaker");
    }
    Ok((speakers.len(), intervals, Some(processing_seconds)))
}

fn external_rttm_intervals(
    path: &Path,
    case_id: &str,
    audio_duration_ms: u64,
) -> Result<(usize, Vec<ExternalDiarizationInterval>, Option<f64>), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "external_diarization_missing")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > 4 * 1024 * 1024
    {
        return Err("external_diarization_rejected");
    }
    let source = fs::read_to_string(path).map_err(|_| "external_diarization_read")?;
    let mut raw = Vec::new();
    let mut labels = BTreeMap::<String, usize>::new();
    for line in source.lines() {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        if fields.len() != 10
            || fields[0] != "SPEAKER"
            || fields[1] != case_id
            || fields[2] != "1"
            || fields[5] != "<NA>"
            || fields[6] != "<NA>"
            || fields[8] != "<NA>"
            || fields[9] != "<NA>"
            || fields[7].is_empty()
            || fields[7].len() > 64
            || !fields[7].chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err("external_diarization_rttm");
        }
        let start_seconds: f64 = fields[3]
            .parse()
            .ok()
            .filter(|value: &f64| value.is_finite() && *value >= 0.0)
            .ok_or("external_diarization_start")?;
        let duration_seconds: f64 = fields[4]
            .parse()
            .ok()
            .filter(|value: &f64| value.is_finite() && *value > 0.0)
            .ok_or("external_diarization_end")?;
        let start_ms = (start_seconds * 1_000.0).round() as u64;
        let raw_end_ms = ((start_seconds + duration_seconds) * 1_000.0).round() as u64;
        if start_ms >= audio_duration_ms || raw_end_ms > audio_duration_ms.saturating_add(250) {
            return Err("external_diarization_timeline");
        }
        let end_ms = raw_end_ms.min(audio_duration_ms);
        if end_ms <= start_ms {
            return Err("external_diarization_timeline");
        }
        let next_slot = labels.len();
        let speaker_slot = *labels.entry(fields[7].to_owned()).or_insert(next_slot);
        raw.push(ExternalDiarizationInterval {
            start_ms,
            end_ms,
            speaker_slot,
        });
    }
    if raw.is_empty() || raw.len() > 20_000 || labels.is_empty() || labels.len() > 16 {
        return Err("external_diarization_segments");
    }
    raw.sort_by_key(|interval| (interval.start_ms, interval.end_ms, interval.speaker_slot));
    Ok((labels.len(), raw, None))
}

fn parse_srt_timestamp(value: &str) -> Result<u64, &'static str> {
    let (hours, rest) = value.split_once(':').ok_or("external_srt_timestamp")?;
    let (minutes, rest) = rest.split_once(':').ok_or("external_srt_timestamp")?;
    let (seconds, milliseconds) = rest
        .split_once([',', '.'])
        .ok_or("external_srt_timestamp")?;
    let hours: u64 = hours.parse().map_err(|_| "external_srt_timestamp")?;
    let minutes: u64 = minutes.parse().map_err(|_| "external_srt_timestamp")?;
    let seconds: u64 = seconds.parse().map_err(|_| "external_srt_timestamp")?;
    let milliseconds: u64 = milliseconds.parse().map_err(|_| "external_srt_timestamp")?;
    if minutes >= 60 || seconds >= 60 || milliseconds >= 1_000 {
        return Err("external_srt_timestamp");
    }
    hours
        .checked_mul(3_600_000)
        .and_then(|value| value.checked_add(minutes * 60_000))
        .and_then(|value| value.checked_add(seconds * 1_000))
        .and_then(|value| value.checked_add(milliseconds))
        .ok_or("external_srt_timestamp")
}

fn external_srt_transcript(
    path: &Path,
    audio_duration_ms: u64,
) -> Result<CanonicalTranscript, &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "external_srt_missing")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > 32 * 1024 * 1024
    {
        return Err("external_srt_rejected");
    }
    let source = fs::read_to_string(path).map_err(|_| "external_srt_read")?;
    let normalized = source.replace("\r\n", "\n");
    let mut segments = Vec::new();
    let mut previous_end = 0_u64;
    for (expected_index, block) in (1_usize..).zip(
        normalized
            .split("\n\n")
            .filter(|block| !block.trim().is_empty()),
    ) {
        let mut lines = block.lines();
        let index: usize = lines
            .next()
            .ok_or("external_srt_index")?
            .trim()
            .parse()
            .map_err(|_| "external_srt_index")?;
        if index != expected_index {
            return Err("external_srt_index");
        }
        let timeline = lines.next().ok_or("external_srt_timeline")?;
        let (start, end) = timeline
            .split_once(" --> ")
            .ok_or("external_srt_timeline")?;
        let start_ms = parse_srt_timestamp(start.trim())?;
        let raw_end_ms = parse_srt_timestamp(end.trim())?;
        if start_ms < previous_end
            || start_ms >= audio_duration_ms
            || raw_end_ms <= start_ms
            || raw_end_ms > audio_duration_ms.saturating_add(250)
        {
            return Err("external_srt_timeline");
        }
        let text = lines.collect::<Vec<_>>().join(" ");
        let text = text.trim();
        if text.is_empty() || text.len() > 65_536 || text.contains('\0') {
            return Err("external_srt_text");
        }
        let end_ms = raw_end_ms.min(audio_duration_ms);
        segments.push(CanonicalSegment {
            start_ms,
            end_ms,
            speaker: "local_unknown".to_owned(),
            text: text.to_owned(),
        });
        previous_end = end_ms;
    }
    if segments.is_empty() || segments.len() > 50_000 {
        return Err("external_srt_empty");
    }
    Ok(CanonicalTranscript {
        schema_version: 1,
        segments,
    })
}

fn diagnostic_punctuation_only(value: &str) -> bool {
    value
        .chars()
        .all(|character| character.is_whitespace() || !character.is_alphanumeric())
}

fn interpolate_diagnostic_zero_duration_words(
    words: &mut [super::local_whisper::LocalAlignedWord],
    punctuation: &[bool],
) -> usize {
    let lexical: Vec<usize> = punctuation
        .iter()
        .enumerate()
        .filter_map(|(index, punctuation)| (!punctuation).then_some(index))
        .collect();
    let mut interpolated = 0_usize;
    let mut position = 0_usize;
    while position < lexical.len() {
        let index = lexical[position];
        if words[index].end_ms > words[index].start_ms {
            position += 1;
            continue;
        }
        let run_start = position;
        while position < lexical.len()
            && words[lexical[position]].end_ms == words[lexical[position]].start_ms
        {
            position += 1;
        }
        if run_start == 0 || position >= lexical.len() {
            continue;
        }
        let previous = lexical[run_start - 1];
        let next = lexical[position];
        if words[previous].end_ms > words[next].start_ms {
            continue;
        }
        let count = position - run_start;
        let span = words[next].start_ms - words[previous].end_ms;
        if span == 0 {
            continue;
        }
        for (offset, lexical_position) in (run_start..position).enumerate() {
            let word = lexical[lexical_position];
            let count = count as u128;
            let span = span as u128;
            let start = span.saturating_mul(offset as u128) / count;
            let end = span
                .saturating_mul(offset as u128 + 1)
                .saturating_add(count - 1)
                / count;
            words[word].start_ms = words[previous]
                .end_ms
                .saturating_add(u64::try_from(start).unwrap_or(u64::MAX));
            words[word].end_ms = words[previous]
                .end_ms
                .saturating_add(u64::try_from(end).unwrap_or(u64::MAX));
            interpolated = interpolated.saturating_add(1);
        }
    }
    interpolated
}

fn merge_external_diarization(
    words: &mut [super::local_whisper::LocalAlignedWord],
    speaker_count: usize,
    intervals: &[ExternalDiarizationInterval],
) -> ExternalDiarizationMergeStats {
    let punctuation: Vec<bool> = words
        .iter()
        .map(|word| diagnostic_punctuation_only(&word.text))
        .collect();
    let interpolated_zero_duration_words =
        interpolate_diagnostic_zero_duration_words(words, &punctuation);
    let mut overlap_assigned_words = 0_usize;
    let mut point_assigned_words = 0_usize;
    for (word, punctuation) in words.iter_mut().zip(&punctuation) {
        word.speaker_id = None;
        if *punctuation {
            continue;
        }
        let mut overlap_by_speaker = vec![0_u64; speaker_count];
        if word.end_ms > word.start_ms {
            for interval in intervals {
                if interval.end_ms <= word.start_ms {
                    continue;
                }
                if interval.start_ms >= word.end_ms {
                    break;
                }
                let overlap =
                    word.end_ms.min(interval.end_ms) - word.start_ms.max(interval.start_ms);
                overlap_by_speaker[interval.speaker_slot] =
                    overlap_by_speaker[interval.speaker_slot].saturating_add(overlap);
            }
        } else {
            for interval in intervals.iter().filter(|interval| {
                interval.start_ms < word.start_ms && word.start_ms < interval.end_ms
            }) {
                overlap_by_speaker[interval.speaker_slot] = 1;
            }
        }
        let mut ranked: Vec<_> = overlap_by_speaker.into_iter().enumerate().collect();
        ranked.sort_by_key(|(_, overlap)| std::cmp::Reverse(*overlap));
        let (speaker, top) = ranked[0];
        let second = ranked.get(1).map(|(_, overlap)| *overlap).unwrap_or(0);
        if top > 0 && top > second {
            if word.end_ms == word.start_ms {
                word.end_ms = word.end_ms.saturating_add(1);
                point_assigned_words = point_assigned_words.saturating_add(1);
            } else {
                overlap_assigned_words = overlap_assigned_words.saturating_add(1);
            }
            word.speaker_id = Some(format!("local_speaker_{:02}", speaker + 1));
        }
    }

    let lexical_indices: Vec<usize> = punctuation
        .iter()
        .enumerate()
        .filter_map(|(index, punctuation)| (!punctuation).then_some(index))
        .collect();
    let mut bridge_assigned_words = 0_usize;
    let mut position = 0_usize;
    while position < lexical_indices.len() {
        if words[lexical_indices[position]].speaker_id.is_some() {
            position += 1;
            continue;
        }
        let run_start = position;
        while position < lexical_indices.len()
            && words[lexical_indices[position]].speaker_id.is_none()
        {
            position += 1;
        }
        let indices = &lexical_indices[run_start..position];
        let gap_start = indices
            .iter()
            .map(|index| words[*index].start_ms)
            .min()
            .unwrap();
        let gap_end = indices
            .iter()
            .map(|index| words[*index].end_ms)
            .max()
            .unwrap();
        let before_end = intervals
            .iter()
            .filter(|interval| interval.end_ms <= gap_start)
            .map(|interval| interval.end_ms)
            .max();
        let after_start = intervals
            .iter()
            .filter(|interval| interval.start_ms >= gap_end)
            .map(|interval| interval.start_ms)
            .min();
        let before_speakers: HashSet<_> = before_end
            .into_iter()
            .flat_map(|end| {
                intervals
                    .iter()
                    .filter(move |interval| interval.end_ms == end)
            })
            .map(|interval| interval.speaker_slot)
            .collect();
        let after_speakers: HashSet<_> = after_start
            .into_iter()
            .flat_map(|start| {
                intervals
                    .iter()
                    .filter(move |interval| interval.start_ms == start)
            })
            .map(|interval| interval.speaker_slot)
            .collect();
        if before_speakers.len() == 1
            && before_speakers == after_speakers
            && before_end
                .zip(after_start)
                .is_some_and(|(before, after)| after.saturating_sub(before) <= 1_000)
        {
            let speaker = *before_speakers.iter().next().unwrap();
            for index in indices {
                words[*index].speaker_id = Some(format!("local_speaker_{:02}", speaker + 1));
                bridge_assigned_words = bridge_assigned_words.saturating_add(1);
            }
        }
    }
    let mut preceding_speaker = None;
    for (word, punctuation) in words.iter_mut().zip(&punctuation) {
        if *punctuation {
            word.speaker_id.clone_from(&preceding_speaker);
        } else {
            preceding_speaker.clone_from(&word.speaker_id);
        }
    }
    let lexical_words = punctuation.iter().filter(|value| !**value).count();
    let unknown_lexical_words = words
        .iter()
        .zip(&punctuation)
        .filter(|(word, punctuation)| !**punctuation && word.speaker_id.is_none())
        .count();
    ExternalDiarizationMergeStats {
        lexical_words,
        overlap_assigned_words,
        point_assigned_words,
        bridge_assigned_words,
        unknown_lexical_words,
        interpolated_zero_duration_words,
    }
}

fn valid_output(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() > 0
            && metadata.len() <= 32 * 1024 * 1024
            && fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<CanonicalTranscript>(&bytes).ok())
                .is_some_and(|transcript| !transcript.segments.is_empty())
    })
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), &'static str> {
    let parent = path.parent().ok_or("output_parent_missing")?;
    fs::create_dir_all(parent).map_err(|_| "output_parent_failed")?;
    let bytes = serde_json::to_vec(value).map_err(|_| "output_encode_failed")?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        Uuid::new_v4()
    ));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| "output_create_failed")?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "output_write_failed")?;
    fs::rename(&temporary, path).map_err(|_| "output_replace_failed")
}

fn collect_model_file_identities(
    root: &Path,
) -> Result<Vec<super::local_whisper::LocalModelFileIdentity>, &'static str> {
    fn walk(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), &'static str> {
        let mut entries: Vec<_> = fs::read_dir(directory)
            .map_err(|_| "model_scan_failed")?
            .collect::<Result<_, _>>()
            .map_err(|_| "model_scan_failed")?;
        entries.sort_by_key(fs::DirEntry::path);
        for entry in entries {
            let metadata = fs::symlink_metadata(entry.path()).map_err(|_| "model_scan_failed")?;
            if metadata.file_type().is_symlink() {
                return Err("model_scan_failed");
            }
            if metadata.is_dir() {
                walk(&entry.path(), paths)?;
            } else if metadata.is_file() {
                paths.push(entry.path());
            } else {
                return Err("model_scan_failed");
            }
            if paths.len() > 64 {
                return Err("model_scan_failed");
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    walk(root, &mut paths)?;
    let mut identities = Vec::with_capacity(paths.len());
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "model_scan_failed")?
            .to_str()
            .ok_or("model_scan_failed")?
            .to_owned();
        let digest =
            crate::ingest::inbox::hash_file_streaming(&path).map_err(|_| "model_scan_failed")?;
        identities.push(super::local_whisper::LocalModelFileIdentity {
            relative_path: relative,
            sha256: digest.sha256,
            size_bytes: digest.size_bytes,
        });
    }
    identities.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(identities)
}

fn persist_completed_case(
    matrix_root: &Path,
    case: &QualityCase,
    ledger: &ProcessingLedger,
    output_relative: &str,
    summary_root: &str,
) -> Result<usize, &'static str> {
    if ledger.state != ProcessingState::Complete || !ledger.cleanup.temporary_tos_deleted {
        return Err("ledger_not_complete");
    }
    let transcript = canonical_transcript(
        ledger
            .transcript_json
            .as_ref()
            .ok_or("transcript_missing")?,
    )?;
    write_json_atomic(&matrix_root.join(output_relative), &transcript)?;
    let summary = ledger.summary_json.as_ref().ok_or("summary_missing")?;
    write_json_atomic(
        &matrix_root
            .join("summaries")
            .join(summary_root)
            .join(format!("{}.json", case.case_id)),
        summary,
    )?;
    Ok(transcript.segments.len())
}

fn processing_credentials() -> ProcessingCredentials {
    let required = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| panic!("missing live credential field {name}"))
    };
    serde_json::from_value(json!({
        "tosAccessKey": required("VOLC_TOS_ACCESS_KEY"),
        "tosSecretKey": required("VOLC_TOS_SECRET_KEY"),
        "tosBucket": required("VOLC_TOS_BUCKET"),
        "tosRegion": required("VOLC_TOS_REGION"),
        "tosEndpoint": required("VOLC_TOS_ENDPOINT"),
        "volcApiKey": required("VOLC_API_KEY"),
        "geminiApiKey": required("GEMINI_API_KEY"),
        "geminiModel": required("GEMINI_MODEL"),
    }))
    .expect("quality provider credential schema must decode")
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "runs the explicitly authorized paid Miaoji/Gemini route over the private quality matrix"]
async fn live_quality_matrix_miaoji_writes_aggregate_only_private_outputs() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_REMOTE_CONFIRM").as_deref(),
        Ok(REMOTE_CONFIRM)
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_REMOTE_APP_ROOT");
    let manifest = load_manifest(&matrix_root);
    let cases = selected_cases(&manifest);
    let credential_state = Arc::new(ProcessingCredentialsState::ephemeral());
    credential_state.save(&processing_credentials()).unwrap();
    let (inbox, store, engine) =
        build_engine(&matrix_root, &app_root, Arc::clone(&credential_state));
    let importer = DesktopImporter::new(
        Arc::clone(&inbox),
        Platform::Macos,
        DEFAULT_MAX_IMPORT_BYTES,
    )
    .unwrap();
    let mut pending = Vec::new();
    let mut completed_cases = 0_usize;
    let mut completed_duration_ms = 0_u64;
    let mut transcript_segments = 0_usize;
    for case in cases {
        let output = matrix_root.join(&case.miaoji);
        if valid_output(&output) {
            completed_cases += 1;
            completed_duration_ms = completed_duration_ms.saturating_add(case.duration_ms);
            continue;
        }
        let recording_id = import_case(&importer, &inbox, &matrix_root, &case);
        engine.enqueue(recording_id).unwrap();
        if store.load(recording_id).unwrap().state == ProcessingState::ProviderFailed {
            engine.retry_provider(recording_id).unwrap();
        }
        pending.push((recording_id, case));
    }
    let started = Instant::now();
    while !pending.is_empty() {
        let mut still_pending = Vec::new();
        for (recording_id, case) in pending {
            match engine.run_until_wait(recording_id).await {
                Ok(ledger) if ledger.state == ProcessingState::Complete => {
                    transcript_segments += persist_completed_case(
                        &matrix_root,
                        &case,
                        &ledger,
                        &case.miaoji,
                        "miaoji",
                    )
                    .unwrap();
                    completed_cases += 1;
                    completed_duration_ms = completed_duration_ms.saturating_add(case.duration_ms);
                }
                Ok(ledger) if ledger.state == ProcessingState::Polling => {
                    still_pending.push((recording_id, case));
                }
                Err(EngineError::Effect(error))
                    if error.kind == EffectErrorKind::Temporary
                        && store.load(recording_id).unwrap().state == ProcessingState::Polling =>
                {
                    still_pending.push((recording_id, case));
                }
                Ok(ledger) => panic!("quality provider route stopped in {:?}", ledger.state),
                Err(error) => panic!("quality provider route failed safely: {error}"),
            }
        }
        pending = still_pending;
        assert!(started.elapsed() < REMOTE_DEADLINE);
        if !pending.is_empty() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    credential_state.delete().unwrap();
    validate_frozen_manifest_durations(&inbox, &manifest).unwrap();
    println!(
        "{}",
        json!({
            "state": "complete",
            "cases": completed_cases,
            "durationMs": completed_duration_ms,
            "transcriptSegmentsWritten": transcript_segments,
            "pollIntervalSeconds": POLL_INTERVAL.as_secs(),
            "temporaryTosDeleted": true,
            "remoteArchiveEffects": false,
            "elapsedSeconds": started.elapsed().as_secs(),
        })
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "runs the exact retained Qwen/FluidAudio/local-summary packs over the private quality matrix"]
async fn live_quality_matrix_qwen_writes_aggregate_only_private_outputs() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_LOCAL_CONFIRM").as_deref(),
        Ok(LOCAL_CONFIRM)
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_LOCAL_APP_ROOT");
    let manifest = load_manifest(&matrix_root);
    let cases = selected_cases(&manifest);
    let manager = LocalModelPackManager::open(&app_root).unwrap();
    let base_proof = manager.proof().await.unwrap();
    let qwen_proof = manager.qwen_proof().await.unwrap();
    let credential_state = Arc::new(ProcessingCredentialsState::ephemeral());
    let (inbox, _store, engine) = build_engine(&matrix_root, &app_root, credential_state);
    let importer = DesktopImporter::new(
        Arc::clone(&inbox),
        Platform::Macos,
        DEFAULT_MAX_IMPORT_BYTES,
    )
    .unwrap();
    let started = Instant::now();
    let mut completed_cases = 0_usize;
    let mut completed_duration_ms = 0_u64;
    let mut transcript_segments = 0_usize;
    for case in cases {
        let output = matrix_root.join(&case.local);
        if valid_output(&output) {
            completed_cases += 1;
            completed_duration_ms = completed_duration_ms.saturating_add(case.duration_ms);
            continue;
        }
        let recording_id = import_case(&importer, &inbox, &matrix_root, &case);
        engine.enqueue(recording_id).unwrap();
        let current = engine.status(recording_id).unwrap();
        if current.transcription_backend == TranscriptionBackend::QwenLocal {
            if current.state == ProcessingState::ProviderFailed {
                engine.retry_provider(recording_id).unwrap();
            } else if !matches!(
                current.state,
                ProcessingState::LocalTranscribing | ProcessingState::Complete
            ) {
                panic!("existing quality-local ledger has an incompatible state");
            }
        } else {
            engine
                .select_full_local_qwen(recording_id, base_proof.clone(), qwen_proof.clone(), None)
                .unwrap();
        }
        let ledger = engine.run_until_wait(recording_id).await.unwrap();
        transcript_segments +=
            persist_completed_case(&matrix_root, &case, &ledger, &case.local, "local").unwrap();
        completed_cases += 1;
        completed_duration_ms = completed_duration_ms.saturating_add(case.duration_ms);
    }
    validate_frozen_manifest_durations(&inbox, &manifest).unwrap();
    println!(
        "{}",
        json!({
            "state": "complete",
            "cases": completed_cases,
            "durationMs": completed_duration_ms,
            "transcriptSegmentsWritten": transcript_segments,
            "providerCalls": 0,
            "elapsedSeconds": started.elapsed().as_secs(),
        })
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "runs the exact retained Whisper/FluidAudio/local-summary pack over the private quality matrix"]
async fn live_quality_matrix_whisper_writes_aggregate_only_private_outputs() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_WHISPER_CONFIRM").as_deref(),
        Ok(WHISPER_CONFIRM)
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_WHISPER_APP_ROOT");
    let manifest = load_manifest(&matrix_root);
    let cases = selected_cases(&manifest);
    let manager = LocalModelPackManager::open(&app_root).unwrap();
    let base_proof = manager.proof().await.unwrap();
    let credential_state = Arc::new(ProcessingCredentialsState::ephemeral());
    let (inbox, _store, engine) = build_engine(&matrix_root, &app_root, credential_state);
    let importer = DesktopImporter::new(
        Arc::clone(&inbox),
        Platform::Macos,
        DEFAULT_MAX_IMPORT_BYTES,
    )
    .unwrap();
    let started = Instant::now();
    let mut completed_cases = 0_usize;
    let mut completed_duration_ms = 0_u64;
    let mut transcript_segments = 0_usize;
    for case in cases {
        let relative = format!("outputs/whisper/{}.json", case.case_id);
        let output = matrix_root.join(&relative);
        if valid_output(&output) {
            completed_cases += 1;
            completed_duration_ms = completed_duration_ms.saturating_add(case.duration_ms);
            continue;
        }
        let recording_id = import_case(&importer, &inbox, &matrix_root, &case);
        engine.enqueue(recording_id).unwrap();
        let current = engine.status(recording_id).unwrap();
        if current.transcription_backend == TranscriptionBackend::WhisperLocal {
            if current.state == ProcessingState::ProviderFailed {
                engine.retry_provider(recording_id).unwrap();
            } else if !matches!(
                current.state,
                ProcessingState::LocalTranscribing | ProcessingState::Complete
            ) {
                panic!("existing Whisper quality ledger has an incompatible state");
            }
        } else {
            engine
                .select_full_local(recording_id, base_proof.clone(), None)
                .unwrap();
        }
        let ledger = engine.run_until_wait(recording_id).await.unwrap();
        transcript_segments +=
            persist_completed_case(&matrix_root, &case, &ledger, &relative, "whisper").unwrap();
        completed_cases += 1;
        completed_duration_ms = completed_duration_ms.saturating_add(case.duration_ms);
    }
    validate_frozen_manifest_durations(&inbox, &manifest).unwrap();
    println!(
        "{}",
        json!({
            "state": "complete",
            "cases": completed_cases,
            "durationMs": completed_duration_ms,
            "transcriptSegmentsWritten": transcript_segments,
            "providerCalls": 0,
            "elapsedSeconds": started.elapsed().as_secs(),
        })
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "runs the retained Whisper model as an ASR-only diagnostic over the private quality matrix"]
async fn live_quality_matrix_whisper_asr_only_is_aggregate_only() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_WHISPER_ASR_CONFIRM").as_deref(),
        Ok("public-corpus-whisper-asr-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_WHISPER_APP_ROOT");
    let manifest = load_manifest(&matrix_root);
    let cases = selected_cases(&manifest);
    let manager = LocalModelPackManager::open(&app_root).unwrap();
    let proof = manager.proof().await.unwrap();
    let archive_boundary = matrix_root.join("work/archive-boundary");
    fs::create_dir_all(&archive_boundary).unwrap();
    let inbox = Arc::new(Inbox::open(&app_root, &archive_boundary).unwrap());
    let importer = DesktopImporter::new(
        Arc::clone(&inbox),
        Platform::Macos,
        DEFAULT_MAX_IMPORT_BYTES,
    )
    .unwrap();
    let worker = workers(&app_root);
    let started = Instant::now();
    let mut completed_cases = 0_usize;
    let mut completed_duration_ms = 0_u64;
    let mut transcript_segments = 0_usize;
    for case in cases {
        let relative = format!("outputs/whisper-asr/{}.json", case.case_id);
        if valid_output(&matrix_root.join(&relative)) {
            completed_cases += 1;
            completed_duration_ms = completed_duration_ms.saturating_add(case.duration_ms);
            continue;
        }
        let recording_id = import_case(&importer, &inbox, &matrix_root, &case);
        let envelope = inbox.load_envelope(recording_id).unwrap();
        let audio_relative_path = envelope.normalized_audio.clone().unwrap();
        let digest = inbox
            .hash_package_file(recording_id, &audio_relative_path)
            .unwrap();
        let request = super::local_whisper::LocalWhisperRequest {
            schema_version: super::local_whisper::LOCAL_WHISPER_PROTOCOL_VERSION,
            recording_id,
            model_id: proof.whisper_model_id.clone(),
            model_sha256: proof.whisper_sha256.clone(),
            model_size_bytes: proof.whisper_size_bytes,
            audio_relative_path,
            audio_sha256: digest.sha256,
            audio_size_bytes: digest.size_bytes,
            audio_duration_ms: envelope.duration_ms,
            language: None,
        };
        let response = worker.transcribe(&request).await.unwrap();
        let transcript =
            canonical_transcript(&response.transcript_json(&request).unwrap()).unwrap();
        transcript_segments = transcript_segments.saturating_add(transcript.segments.len());
        write_json_atomic(&matrix_root.join(&relative), &transcript).unwrap();
        completed_cases += 1;
        completed_duration_ms = completed_duration_ms.saturating_add(case.duration_ms);
    }
    println!(
        "{}",
        json!({
            "state": "complete",
            "cases": completed_cases,
            "durationMs": completed_duration_ms,
            "transcriptSegmentsWritten": transcript_segments,
            "speakerLabels": "local_unknown",
            "providerCalls": 0,
            "elapsedSeconds": started.elapsed().as_secs(),
        })
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "reruns one failed public-corpus Whisper case and prints aggregate stage diagnostics"]
async fn live_quality_whisper_stage_diagnostic_is_aggregate_only() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_WHISPER_DIAGNOSTIC_CONFIRM").as_deref(),
        Ok("public-corpus-stage-diagnostic-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_WHISPER_APP_ROOT");
    let archive_boundary = matrix_root.join("work/archive-boundary");
    fs::create_dir_all(&archive_boundary).unwrap();
    let inbox = Inbox::open(&app_root, &archive_boundary).unwrap();
    let store = ProcessingStore::open(&app_root, inbox.root(), &archive_boundary).unwrap();
    let recording_id = store
        .list_recording_ids()
        .unwrap()
        .into_iter()
        .find(|recording_id| {
            store.load(*recording_id).is_ok_and(|ledger| {
                ledger.state == ProcessingState::ProviderFailed
                    && ledger.transcription_backend == TranscriptionBackend::WhisperLocal
            })
        })
        .expect("one failed Whisper quality ledger must exist");
    let ledger = store.load(recording_id).unwrap();
    let request = ledger.local_transcription_request().unwrap();
    let whisper_request = request.whisper.as_ref().unwrap();
    let diarization_request = request.diarization.as_ref().unwrap();
    let worker = workers(&app_root);
    let mut transcript = worker.transcribe(whisper_request).await.unwrap();
    let diarization = worker.diarize(diarization_request).await.unwrap();
    let transcript_segments = transcript.segments.len();
    let stats = super::local_whisper::merge_diarization(
        &mut transcript,
        whisper_request,
        &diarization,
        diarization_request,
    )
    .unwrap();
    println!(
        "{}",
        json!({
            "state": "complete",
            "transcriptSegments": transcript_segments,
            "diarizationSegments": diarization.segments.len(),
            "detectedSpeakers": diarization.speaker_count,
            "assignedSegments": stats.assigned_segments,
            "unknownSegments": stats.unknown_segments,
            "assignedCoverage": stats.assigned_segments as f64 / transcript_segments as f64,
        })
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "repeats one public-corpus FluidAudio request and prints determinism aggregates"]
async fn live_quality_diarization_repeat_is_aggregate_only() {
    use sha2::{Digest, Sha256};

    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_DIARIZATION_REPEAT_CONFIRM").as_deref(),
        Ok("public-corpus-diarization-repeat-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_WHISPER_APP_ROOT");
    let archive_boundary = matrix_root.join("work/archive-boundary");
    fs::create_dir_all(&archive_boundary).unwrap();
    let inbox = Inbox::open(&app_root, &archive_boundary).unwrap();
    let store = ProcessingStore::open(&app_root, inbox.root(), &archive_boundary).unwrap();
    let recording_id = store
        .list_recording_ids()
        .unwrap()
        .into_iter()
        .find(|recording_id| {
            store.load(*recording_id).is_ok_and(|ledger| {
                ledger.transcription_backend == TranscriptionBackend::WhisperLocal
            })
        })
        .expect("one Whisper quality ledger must exist");
    let ledger = store.load(recording_id).unwrap();
    let request = ledger
        .local_transcription_request()
        .unwrap()
        .diarization
        .unwrap();
    let worker = workers(&app_root);
    let mut speaker_counts = BTreeMap::<u32, usize>::new();
    let mut segment_counts = BTreeMap::<usize, usize>::new();
    let mut digests = HashSet::new();
    for _ in 0..5 {
        let response = worker.diarize(&request).await.unwrap();
        *speaker_counts.entry(response.speaker_count).or_default() += 1;
        *segment_counts.entry(response.segments.len()).or_default() += 1;
        digests.insert(hex::encode(Sha256::digest(
            serde_json::to_vec(&response).unwrap(),
        )));
    }
    println!(
        "{}",
        json!({
            "state": "complete",
            "runs": 5,
            "speakerCountHistogram": speaker_counts,
            "segmentCountHistogram": segment_counts,
            "distinctOutputDigests": digests.len(),
        })
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "compares closed FluidAudio presets on one public-corpus case and prints aggregates"]
async fn live_quality_diarization_preset_comparison_is_aggregate_only() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_DIARIZATION_PRESET_CONFIRM").as_deref(),
        Ok("public-corpus-diarization-preset-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_WHISPER_APP_ROOT");
    let archive_boundary = matrix_root.join("work/archive-boundary");
    fs::create_dir_all(&archive_boundary).unwrap();
    let inbox = Inbox::open(&app_root, &archive_boundary).unwrap();
    let store = ProcessingStore::open(&app_root, inbox.root(), &archive_boundary).unwrap();
    let recording_id = store
        .list_recording_ids()
        .unwrap()
        .into_iter()
        .find(|recording_id| {
            store.load(*recording_id).is_ok_and(|ledger| {
                ledger.transcription_backend == TranscriptionBackend::WhisperLocal
            })
        })
        .expect("one Whisper quality ledger must exist");
    let ledger = store.load(recording_id).unwrap();
    let request = ledger.local_transcription_request().unwrap();
    let whisper_request = request.whisper.as_ref().unwrap();
    let base_diarization_request = request.diarization.as_ref().unwrap();
    let worker = workers(&app_root);
    let base_transcript = worker.transcribe(whisper_request).await.unwrap();
    let imported_name = inbox
        .load_envelope(recording_id)
        .unwrap()
        .imported_name
        .unwrap();
    let case_id = imported_name.strip_suffix(".m4a").unwrap();
    let presets = [
        (
            "selected",
            super::local_whisper::LOCAL_DIARIZATION_QUALITY_PRESET,
        ),
        (
            "legacy",
            super::local_whisper::LOCAL_DIARIZATION_LEGACY_PRESET,
        ),
    ];
    let mut results = Vec::new();
    for (label, preset) in presets {
        let mut diarization_request = base_diarization_request.clone();
        diarization_request.quality_preset = preset.to_owned();
        diarization_request.validate().unwrap();
        let diarization = match worker.diarize(&diarization_request).await {
            Ok(response) => response,
            Err(error) => {
                results.push(json!({
                    "preset": label,
                    "errorKind": format!("{:?}", error.kind),
                }));
                continue;
            }
        };
        let mut transcript = base_transcript.clone();
        let stats = super::local_whisper::merge_diarization(
            &mut transcript,
            whisper_request,
            &diarization,
            &diarization_request,
        )
        .unwrap();
        let canonical =
            canonical_transcript(&transcript.transcript_json(whisper_request).unwrap()).unwrap();
        write_json_atomic(
            &matrix_root
                .join("outputs")
                .join(format!("whisper-diar-{label}"))
                .join(format!("{case_id}.json")),
            &canonical,
        )
        .unwrap();
        results.push(json!({
            "preset": label,
            "detectedSpeakers": diarization.speaker_count,
            "diarizationSegments": diarization.segments.len(),
            "assignedSegments": stats.assigned_segments,
            "unknownSegments": stats.unknown_segments,
        }));
    }
    println!("{}", json!({"state": "complete", "results": results}));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "merges private MLX oracle alignment with selected FluidAudio and prints aggregates"]
async fn live_quality_mlx_alignment_fluid_merge_is_aggregate_only() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_MLX_FLUID_CONFIRM").as_deref(),
        Ok("public-corpus-mlx-fluid-merge-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_LOCAL_APP_ROOT");
    let archive_boundary = matrix_root.join("work/archive-boundary");
    fs::create_dir_all(&archive_boundary).unwrap();
    let inbox = Inbox::open(&app_root, &archive_boundary).unwrap();
    let store = ProcessingStore::open(&app_root, inbox.root(), &archive_boundary).unwrap();
    let manifest = load_manifest(&matrix_root);
    let worker = workers(&app_root);
    let mut results = Vec::new();
    for stratum in ["mandarin", "mixed"] {
        let case = manifest
            .cases
            .iter()
            .find(|case| case.stratum == stratum)
            .unwrap();
        let aligned_path = matrix_root
            .join("outputs/mlx-qwen-aligned")
            .join(format!("{}.json", case.case_id));
        let aligned: CanonicalTranscript =
            serde_json::from_slice(&fs::read(aligned_path).unwrap()).unwrap();
        let imported_name = format!("{}.m4a", case.case_id);
        let recording_id = fs::read_dir(inbox.root())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| fs::read(entry.path().join("recording.json")).ok())
            .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .find(|value| {
                value.get("imported_name").and_then(Value::as_str) == Some(imported_name.as_str())
            })
            .and_then(|value| {
                value
                    .get("recording_id")
                    .and_then(Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
            })
            .expect("quality case must already be imported");
        let request = store
            .load(recording_id)
            .unwrap()
            .local_transcription_request()
            .unwrap()
            .diarization
            .unwrap();
        let diarization = worker.diarize(&request).await.unwrap();
        let mut words: Vec<_> = aligned
            .segments
            .into_iter()
            .map(|segment| super::local_whisper::LocalAlignedWord {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: segment.text,
                speaker_id: None,
            })
            .collect();
        let stats = super::local_whisper::merge_aligned_words_with_diarization(
            &mut words,
            &diarization,
            &request,
        )
        .unwrap();
        let canonical = canonical_aligned_diagnostic(&words).unwrap();
        write_json_atomic(
            &matrix_root
                .join("outputs/mlx-qwen-fluid")
                .join(format!("{}.json", case.case_id)),
            &canonical,
        )
        .unwrap();
        results.push(json!({
            "stratum": stratum,
            "detectedSpeakers": diarization.speaker_count,
            "diarizationSegments": diarization.segments.len(),
            "lexicalWords": stats.lexical_words,
            "overlapAssignedWords": stats.overlap_assigned_words,
            "pointAssignedWords": stats.point_assigned_words,
            "bridgeAssignedWords": stats.bridge_assigned_words,
            "unknownLexicalWords": stats.unknown_lexical_words,
        }));
    }
    println!("{}", json!({"state": "complete", "results": results}));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "runs the current signed FluidAudio worker over the public full matrix"]
async fn live_quality_fluid_diarization_full_matrix_is_aggregate_only() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_FLUID_MATRIX_CONFIRM").as_deref(),
        Ok("public-corpus-fluid-diarization-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_LOCAL_APP_ROOT");
    let manifest = load_manifest(&matrix_root);
    let mut evaluation_manifest = manifest.clone();
    let manager = LocalModelPackManager::open(&app_root).unwrap();
    let proof = manager.proof().await.unwrap();
    let credentials = Arc::new(ProcessingCredentialsState::ephemeral());
    let (inbox, _store, engine) = build_engine(&matrix_root, &app_root, credentials);
    let importer = DesktopImporter::new(
        Arc::clone(&inbox),
        Platform::Macos,
        DEFAULT_MAX_IMPORT_BYTES,
    )
    .unwrap();
    let worker = workers(&app_root);
    let started = Instant::now();
    let mut by_stratum = BTreeMap::<String, (usize, u64, usize, usize)>::new();
    for (case, evaluation_case) in manifest.cases.iter().zip(&mut evaluation_manifest.cases) {
        let recording_id = import_case(&importer, &inbox, &matrix_root, case);
        let ledger = engine
            .select_full_local(recording_id, proof.clone(), None)
            .unwrap();
        let request = ledger
            .local_transcription_request()
            .unwrap()
            .diarization
            .unwrap();
        let response = worker.diarize(&request).await.unwrap();
        let accepted: Vec<_> = response
            .segments
            .iter()
            .filter(|segment| {
                segment.confidence_milli
                    >= super::local_whisper::MIN_LOCAL_DIARIZATION_CONFIDENCE_MILLI
            })
            .collect();
        assert!(!accepted.is_empty());
        let value = Value::Array(
            accepted
                .iter()
                .map(|segment| {
                    json!({
                        "start_time": segment.start_ms,
                        "end_time": segment.end_ms,
                        "speaker": {"id": format!("local_speaker_{:02}", segment.speaker_slot)},
                        "content": "speech",
                    })
                })
                .collect(),
        );
        let canonical = canonical_transcript(&value).unwrap();
        let relative = format!("outputs/fluid-selected-diar-only/{}.json", case.case_id);
        write_json_atomic(&matrix_root.join(&relative), &canonical).unwrap();
        evaluation_case.local = relative;
        let truth: CanonicalTranscript =
            serde_json::from_slice(&fs::read(matrix_root.join(&case.ground_truth)).unwrap())
                .unwrap();
        let truth_speakers: HashSet<_> = truth
            .segments
            .iter()
            .map(|segment| segment.speaker.as_str())
            .collect();
        let accepted_speakers: HashSet<_> = accepted
            .iter()
            .map(|segment| segment.speaker_slot)
            .collect();
        let entry = by_stratum
            .entry(case.stratum.clone())
            .or_insert((0, 0, 0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(case.duration_ms);
        entry.2 += accepted.len();
        entry.3 += usize::from(accepted_speakers.len() == truth_speakers.len());
    }
    write_json_atomic(
        &matrix_root.join("manifest-fluid-selected-diar-only.json"),
        &evaluation_manifest,
    )
    .unwrap();
    let results: Vec<_> = by_stratum
        .into_iter()
        .map(
            |(stratum, (cases, duration_ms, segments, exact_speaker_counts))| {
                json!({
                    "stratum": stratum,
                    "cases": cases,
                    "durationMs": duration_ms,
                    "segments": segments,
                    "exactSpeakerCounts": exact_speaker_counts,
                })
            },
        )
        .collect();
    println!(
        "{}",
        json!({
            "state": "complete",
            "candidate": "fluid-selected",
            "elapsedSeconds": started.elapsed().as_secs(),
            "results": results,
        })
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "runs the no-network SpeakerKit subset through the closed App worker protocol"]
async fn live_quality_speakerkit_worker_matches_public_pilot_shape() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_SPEAKERKIT_WORKER_CONFIRM").as_deref(),
        Ok("public-corpus-speakerkit-worker-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let app_root = authorized_root("ECHOWALL_LIVE_QUALITY_LOCAL_APP_ROOT");
    let archive_boundary = matrix_root.join("work/archive-boundary");
    fs::create_dir_all(&archive_boundary).unwrap();
    let inbox = Arc::new(Inbox::open(&app_root, &archive_boundary).unwrap());
    let importer = DesktopImporter::new(
        Arc::clone(&inbox),
        Platform::Macos,
        DEFAULT_MAX_IMPORT_BYTES,
    )
    .unwrap();
    let manifest = load_manifest(&matrix_root);
    let case = manifest
        .cases
        .iter()
        .find(|case| case.stratum == "mixed")
        .unwrap();
    let recording_id = import_case(&importer, &inbox, &matrix_root, case);
    let envelope = inbox.load_envelope(recording_id).unwrap();
    let track = envelope.tracks.first().unwrap();
    let audio = inbox
        .hash_package_file(recording_id, &track.relative_path)
        .unwrap();
    assert_eq!(audio.sha256, track.sha256);
    let model_root = app_root.join("models/diarization/speakerkit-v1");
    let model_files = collect_model_file_identities(&model_root).unwrap();
    assert_eq!(model_files.len(), 29);
    let request = super::local_whisper::LocalDiarizationRequest {
        schema_version: super::local_whisper::LOCAL_DIARIZATION_PROTOCOL_VERSION,
        recording_id,
        pack_id: "speakerkit-v1".to_owned(),
        quality_preset: super::local_whisper::LOCAL_DIARIZATION_SPEAKERKIT_PRESET.to_owned(),
        model_files,
        audio_relative_path: track.relative_path.clone(),
        audio_sha256: audio.sha256,
        audio_size_bytes: audio.size_bytes,
        audio_duration_ms: envelope.duration_ms,
        expected_speaker_count: None,
    };
    request.validate().unwrap();
    let executable = repository_root()
        .join("desktop/diarization-worker/.build/arm64-apple-macosx/release")
        .join("echowall-diarization-worker");
    let worker = LocalWhisperWorker::from_paths_for_test(
        app_root,
        executable.clone(),
        executable.clone(),
        executable.clone(),
        executable,
    );
    let started = Instant::now();
    let response = worker.diarize(&request).await.unwrap();
    response.validate_against(&request).unwrap();
    let minimum_confidence = response
        .segments
        .iter()
        .map(|segment| segment.confidence_milli)
        .min()
        .unwrap();
    assert_eq!(response.speaker_count, 8);
    assert_eq!(response.segments.len(), 246);
    assert!(minimum_confidence >= 500);
    println!(
        "{}",
        json!({
            "state": "complete",
            "speakerCount": response.speaker_count,
            "segments": response.segments.len(),
            "minimumConfidenceMilli": minimum_confidence,
            "elapsedSeconds": started.elapsed().as_secs_f64(),
            "modelFiles": request.model_files.len(),
            "networkEffects": 0,
        })
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "merges public MLX alignment with a local external diarizer diagnostic"]
fn live_quality_mlx_alignment_external_diarizer_merge_is_aggregate_only() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_EXTERNAL_DIAR_CONFIRM").as_deref(),
        Ok("public-corpus-external-diarization-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let candidate = std::env::var("ECHOWALL_EXTERNAL_DIARIZATION_CANDIDATE")
        .expect("external diarization candidate must be supplied");
    assert!(matches!(
        candidate.as_str(),
        "sortformer-offline"
            | "lseend-ami"
            | "lseend-dihard3"
            | "speakerkit-community1"
            | "speakerkit-community1-regular"
            | "speakerkit-community1-t045"
            | "speakerkit-community1-t075"
            | "speakerkit-community1-truth-count"
    ));
    let manifest = load_manifest(&matrix_root);
    let mut evaluation_manifest = manifest.clone();
    let aligned_candidate = std::env::var("ECHOWALL_ALIGNED_TRANSCRIPT_CANDIDATE")
        .unwrap_or_else(|_| "mlx-qwen-aligned".to_owned());
    assert!(matches!(
        aligned_candidate.as_str(),
        "mlx-qwen-aligned" | "mlx-qwen-vad30-aligned" | "mlx-qwen-fixed300-aligned"
    ));
    let output_prefix = match aligned_candidate.as_str() {
        "mlx-qwen-aligned" => "mlx-qwen",
        "mlx-qwen-vad30-aligned" => "mlx-qwen-vad30",
        "mlx-qwen-fixed300-aligned" => "mlx-qwen-fixed300",
        _ => unreachable!(),
    };
    let strata = std::env::var("ECHOWALL_EXTERNAL_DIARIZATION_STRATA")
        .unwrap_or_else(|_| "mandarin,mixed".to_owned());
    let strata: Vec<_> = strata.split(',').collect();
    assert!(!strata.is_empty());
    let case_set = std::env::var("ECHOWALL_EXTERNAL_DIARIZATION_CASE_SET")
        .unwrap_or_else(|_| "pilot".to_owned());
    assert!(matches!(case_set.as_str(), "pilot" | "all"));
    let mut cases = Vec::new();
    for stratum in &strata {
        assert!(matches!(
            *stratum,
            "english" | "mandarin" | "mixed" | "overlap" | "long_form"
        ));
        let matching: Vec<_> = manifest
            .cases
            .iter()
            .filter(|case| case.stratum == *stratum)
            .collect();
        assert!(!matching.is_empty());
        if case_set == "all" {
            cases.extend(matching);
        } else {
            cases.push(matching[0]);
        }
    }
    let selected_case_ids: HashSet<_> = cases.iter().map(|case| case.case_id.clone()).collect();
    let mut aggregates = BTreeMap::<String, ExternalDiarizationAggregate>::new();
    for case in cases {
        let aligned_path = matrix_root
            .join("outputs")
            .join(&aligned_candidate)
            .join(format!("{}.json", case.case_id));
        let aligned: CanonicalTranscript =
            serde_json::from_slice(&fs::read(aligned_path).unwrap()).unwrap();
        let candidate_root = matrix_root.join("outputs").join(&candidate);
        let (speaker_count, intervals, processing_seconds) =
            if candidate.starts_with("speakerkit-community1") {
                external_rttm_intervals(
                    &candidate_root.join(format!("{}.rttm", case.case_id)),
                    &case.case_id,
                    case.duration_ms,
                )
                .unwrap()
            } else {
                external_diarization_intervals(
                    &candidate_root.join(format!("{}.raw.json", case.case_id)),
                    case.duration_ms,
                )
                .unwrap()
            };
        let mut words: Vec<_> = aligned
            .segments
            .into_iter()
            .map(|segment| super::local_whisper::LocalAlignedWord {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: segment.text,
                speaker_id: None,
            })
            .collect();
        let stats = merge_external_diarization(&mut words, speaker_count, &intervals);
        let canonical = canonical_aligned_diagnostic(&words).unwrap();
        write_json_atomic(
            &matrix_root
                .join("outputs")
                .join(format!("{output_prefix}-{candidate}"))
                .join(format!("{}.json", case.case_id)),
            &canonical,
        )
        .unwrap();
        evaluation_manifest
            .cases
            .iter_mut()
            .find(|candidate| candidate.case_id == case.case_id)
            .unwrap()
            .local = format!("outputs/{output_prefix}-{candidate}/{}.json", case.case_id);
        let truth: CanonicalTranscript =
            serde_json::from_slice(&fs::read(matrix_root.join(&case.ground_truth)).unwrap())
                .unwrap();
        let truth_speakers: HashSet<_> = truth
            .segments
            .iter()
            .map(|segment| segment.speaker.as_str())
            .collect();
        let assigned = stats
            .lexical_words
            .saturating_sub(stats.unknown_lexical_words);
        let coverage_ppm = u64::try_from(assigned)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000_000)
            / u64::try_from(stats.lexical_words).unwrap_or(1).max(1);
        let aggregate = aggregates.entry(case.stratum.clone()).or_default();
        aggregate.cases += 1;
        aggregate.duration_ms = aggregate.duration_ms.saturating_add(case.duration_ms);
        aggregate.detected_speaker_count_exact +=
            usize::from(speaker_count == truth_speakers.len());
        aggregate.diarization_segments += intervals.len();
        aggregate.lexical_words += stats.lexical_words;
        aggregate.unknown_lexical_words += stats.unknown_lexical_words;
        aggregate.cases_below_99_percent_coverage +=
            usize::from(assigned.saturating_mul(100) < stats.lexical_words.saturating_mul(99));
        aggregate.minimum_coverage_ppm = if aggregate.minimum_coverage_ppm == 0 {
            coverage_ppm
        } else {
            aggregate.minimum_coverage_ppm.min(coverage_ppm)
        };
        aggregate.overlap_assigned_words += stats.overlap_assigned_words;
        aggregate.point_assigned_words += stats.point_assigned_words;
        aggregate.bridge_assigned_words += stats.bridge_assigned_words;
        aggregate.interpolated_zero_duration_words += stats.interpolated_zero_duration_words;
        aggregate.processing_seconds += processing_seconds.unwrap_or(0.0);
    }
    if case_set == "all" {
        evaluation_manifest
            .cases
            .retain(|case| selected_case_ids.contains(&case.case_id));
        write_json_atomic(
            &matrix_root.join(format!("manifest-{output_prefix}-{candidate}.json")),
            &evaluation_manifest,
        )
        .unwrap();
    }
    let results: Vec<_> = aggregates
        .into_iter()
        .map(|(stratum, aggregate)| {
            json!({
                "stratum": stratum,
                "cases": aggregate.cases,
                "durationMs": aggregate.duration_ms,
                "detectedSpeakerCountExact": aggregate.detected_speaker_count_exact,
                "diarizationSegments": aggregate.diarization_segments,
                "lexicalWords": aggregate.lexical_words,
                "unknownLexicalWords": aggregate.unknown_lexical_words,
                "casesBelow99PercentCoverage": aggregate.cases_below_99_percent_coverage,
                "minimumAssignedWordCoverage": aggregate.minimum_coverage_ppm as f64 / 1_000_000.0,
                "overlapAssignedWords": aggregate.overlap_assigned_words,
                "pointAssignedWords": aggregate.point_assigned_words,
                "bridgeAssignedWords": aggregate.bridge_assigned_words,
                "interpolatedZeroDurationWords": aggregate.interpolated_zero_duration_words,
                "processingSeconds": aggregate.processing_seconds,
            })
        })
        .collect();
    println!(
        "{}",
        json!({"state": "complete", "candidate": candidate, "results": results})
    );
}

#[test]
#[ignore = "writes aggregate-scored diarization-only fixtures from public RTTM"]
fn live_quality_external_diarization_full_matrix_is_aggregate_only() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_EXTERNAL_DIAR_CONFIRM").as_deref(),
        Ok("public-corpus-external-diarization-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let candidate = std::env::var("ECHOWALL_EXTERNAL_DIARIZATION_CANDIDATE")
        .expect("external diarization candidate must be supplied");
    assert_eq!(candidate, "speakerkit-community1");
    let manifest = load_manifest(&matrix_root);
    let mut evaluation_manifest = manifest.clone();
    let mut by_stratum = BTreeMap::<String, (usize, u64, usize, usize)>::new();
    for (case, evaluation_case) in manifest.cases.iter().zip(&mut evaluation_manifest.cases) {
        let path = matrix_root
            .join("outputs")
            .join(&candidate)
            .join(format!("{}.rttm", case.case_id));
        let (speaker_count, intervals, _) =
            external_rttm_intervals(&path, &case.case_id, case.duration_ms).unwrap();
        let value = Value::Array(
            intervals
                .iter()
                .map(|interval| {
                    json!({
                        "start_time": interval.start_ms,
                        "end_time": interval.end_ms,
                        "speaker": {"id": format!("local_speaker_{:02}", interval.speaker_slot + 1)},
                        "content": "speech",
                    })
                })
                .collect(),
        );
        let canonical = canonical_transcript(&value).unwrap();
        let relative = format!("outputs/{candidate}-diar-only/{}.json", case.case_id);
        write_json_atomic(&matrix_root.join(&relative), &canonical).unwrap();
        evaluation_case.local = relative;
        let truth: CanonicalTranscript =
            serde_json::from_slice(&fs::read(matrix_root.join(&case.ground_truth)).unwrap())
                .unwrap();
        let truth_speakers: HashSet<_> = truth
            .segments
            .iter()
            .map(|segment| segment.speaker.as_str())
            .collect();
        let entry = by_stratum
            .entry(case.stratum.clone())
            .or_insert((0, 0, 0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(case.duration_ms);
        entry.2 += intervals.len();
        entry.3 += usize::from(speaker_count == truth_speakers.len());
    }
    write_json_atomic(
        &matrix_root.join("manifest-speakerkit-diar-only.json"),
        &evaluation_manifest,
    )
    .unwrap();
    let aggregates: Vec<_> = by_stratum
        .into_iter()
        .map(
            |(stratum, (cases, duration_ms, segments, exact_speaker_counts))| {
                json!({
                    "stratum": stratum,
                    "cases": cases,
                    "durationMs": duration_ms,
                    "segments": segments,
                    "exactSpeakerCounts": exact_speaker_counts,
                })
            },
        )
        .collect();
    println!(
        "{}",
        json!({"state": "complete", "candidate": candidate, "results": aggregates})
    );
}

#[test]
#[ignore = "converts local Fun-ASR SRT diagnostics without printing transcript content"]
fn live_quality_funasr_srt_conversion_is_aggregate_only() {
    assert_eq!(
        std::env::var("ECHOWALL_LIVE_QUALITY_FUNASR_CONFIRM").as_deref(),
        Ok("public-corpus-funasr-authorized")
    );
    let matrix_root = authorized_root("ECHOWALL_LIVE_QUALITY_MATRIX_ROOT");
    let manifest = load_manifest(&matrix_root);
    let mut results = Vec::new();
    for stratum in ["mandarin", "mixed"] {
        let case = manifest
            .cases
            .iter()
            .find(|case| case.stratum == stratum)
            .unwrap();
        let root = matrix_root.join("outputs/funasr-nano");
        let transcript = external_srt_transcript(
            &root.join(format!("{}.srt", case.case_id)),
            case.duration_ms,
        )
        .unwrap();
        let segments = transcript.segments.len();
        let text_bytes = transcript
            .segments
            .iter()
            .map(|segment| segment.text.len())
            .sum::<usize>();
        write_json_atomic(&root.join(format!("{}.json", case.case_id)), &transcript).unwrap();
        results.push(json!({
            "stratum": stratum,
            "segments": segments,
            "textBytes": text_bytes,
        }));
    }
    println!("{}", json!({"state": "complete", "results": results}));
}

#[test]
fn canonical_quality_conversion_rejects_missing_speaker_and_bad_timeline() {
    assert!(canonical_transcript(&json!([{
        "start_time": 0,
        "end_time": 100,
        "speaker": {"id": "speaker_1"},
        "content": "public fixture"
    }]))
    .is_ok());
    assert_eq!(
        canonical_transcript(&json!([{
            "start_time": 0,
            "end_time": 100,
            "speaker": {"id": ""},
            "content": "public fixture"
        }]))
        .unwrap_err(),
        "speaker_missing"
    );
    assert_eq!(
        canonical_transcript(&json!([{
            "start_time": 100,
            "end_time": 99,
            "speaker": {"id": "speaker_1"},
            "content": "public fixture"
        }]))
        .unwrap_err(),
        "timeline_invalid"
    );
    let merged = canonical_transcript(&json!([
        {
            "start_time": 0,
            "end_time": 100,
            "speaker": {"id": "speaker_1"},
            "content": "public fixture"
        },
        {
            "start_time": 500,
            "end_time": 500,
            "speaker": {"id": "speaker_1"},
            "content": "boundary text"
        }
    ]))
    .unwrap();
    assert_eq!(merged.segments.len(), 1);
    assert!(merged.segments[0].text.contains("boundary text"));
    let overlap_merged = canonical_transcript(&json!([
        {
            "start_time": 0,
            "end_time": 1000,
            "speaker": {"id": "speaker_1"},
            "content": "first"
        },
        {
            "start_time": 900,
            "end_time": 1500,
            "speaker": {"id": "speaker_1"},
            "content": "second"
        },
        {
            "start_time": 950,
            "end_time": 1100,
            "speaker": {"id": "speaker_2"},
            "content": "real overlap"
        }
    ]))
    .unwrap();
    assert_eq!(overlap_merged.segments.len(), 2);
    assert_eq!(overlap_merged.segments[0].end_ms, 1500);
    assert!(overlap_merged.segments[0].text.contains("second"));
}
