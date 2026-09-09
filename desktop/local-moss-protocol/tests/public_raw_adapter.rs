//! Explicitly authorized generation from retained public CLI raw strings only.
//! The separate public_adapter test remains a read-only historical replay.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use echowall_local_moss_protocol::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[path = "support/public_raw_files.rs"]
mod public_raw_files;
use public_raw_files::{publish, read_bounded, safe_path};

const OUTPUT: &str = "outputs/moss-raw-explicit-v1";
const MANIFEST: &str = "manifest-moss-raw-explicit-40.json";
const EXPECTED_CHANGED: [&str; 5] = [
    "english_02",
    "english_03",
    "english_04",
    "english_06",
    "english_08",
];

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn encoded(value: &Value) -> Result<Vec<u8>, &'static str> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| "artifact_invalid")?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn fixture_request(index: usize, case: &Value, audio: &[u8]) -> Result<MossRequest, &'static str> {
    // Match public_adapter.rs exactly, including the original manifest index.
    let recording_id = Uuid::from_u128(index as u128 + 1);
    let request = MossRequest {
        schema_version: PROTOCOL_VERSION,
        recording_id,
        runtime_id: RUNTIME_ID.into(),
        model_id: MODEL_ID.into(),
        model_revision: MODEL_REVISION.into(),
        model_sha256: MODEL_SHA256.into(),
        model_size_bytes: MODEL_SIZE_BYTES,
        timing_policy: TIMING_POLICY.into(),
        audio_relative_path: format!("inbox/{recording_id}/tracks/imported.wav"),
        audio_sha256: digest(audio),
        audio_size_bytes: audio.len() as u64,
        audio_duration_ms: case["duration_ms"].as_u64().ok_or("case_invalid")?,
        language: None,
    };
    request.validate().map_err(|error| error.0)?;
    Ok(request)
}

fn response(request: &MossRequest, segments: Vec<MossSegment>) -> MossResponse {
    MossResponse {
        schema_version: PROTOCOL_VERSION,
        recording_id: request.recording_id,
        runtime_id: request.runtime_id.clone(),
        model_id: request.model_id.clone(),
        model_sha256: request.model_sha256.clone(),
        audio_sha256: request.audio_sha256.clone(),
        timing_policy: request.timing_policy.clone(),
        complete: true,
        segments,
    }
}

fn canonical(adapted: &AdaptedTranscript) -> Value {
    json!({"schema_version": 1, "segments": adapted.segments.iter().map(|segment| json!({
        "start_ms": segment.start_ms, "end_ms": segment.end_ms,
        "speaker": segment.speaker_id.map_or_else(|| "local_unknown".into(), |id| format!("moss_speaker_{id}")),
        "text": segment.text,
    })).collect::<Vec<_>>()})
}

#[test]
#[ignore = "generates a separate canonical40 from retained public raw strings; requires explicit confirmation"]
fn retained_public_raw_generates_corrected_canonical40() -> Result<(), &'static str> {
    if std::env::var("ECHOWALL_MOSS_RAW_CANONICAL_CONFIRM").as_deref()
        != Ok("public-raw-canonical-generation-authorized")
    {
        return Err("confirmation_required");
    }
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("repository_missing")?
        .to_owned();
    let root = fs::canonicalize(repository.join("local-eval/matrix"))
        .map_err(|_| "fixture_root_missing")?;
    let manifest_bytes = read_bounded(&root, "manifest.json", 1024 * 1024)?;
    let manifest: Value =
        serde_json::from_slice(&manifest_bytes).map_err(|_| "manifest_invalid")?;
    if manifest["schema_version"] != 1 {
        return Err("manifest_invalid");
    }
    let cases = manifest["cases"].as_array().ok_or("manifest_invalid")?;
    let mut records = BTreeMap::new();
    let mut sources = Vec::new();
    for relative in [
        "outputs/moss-transcribe-q8-mmo/results.jsonl",
        "outputs/moss-transcribe-q8-english/results.jsonl",
    ] {
        let bytes = read_bounded(&root, relative, 32 * 1024 * 1024)?;
        sources.push(json!({"relative_path":relative, "sha256":digest(&bytes)}));
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let row: Value = serde_json::from_slice(line).map_err(|_| "native_json_invalid")?;
            if row["type"] == "batch_header" {
                continue;
            }
            if row.get("error").is_some() {
                return Err("native_run_failed");
            }
            let file = row["file"].as_str().ok_or("native_file_missing")?;
            let id = Path::new(file)
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or("native_file_invalid")?
                .to_owned();
            if file != format!("local-eval/matrix/audio-moss/{id}.wav")
                || (relative.contains("-english/") != id.starts_with("english_"))
            {
                return Err("native_file_identity_mismatch");
            }
            if records.insert(id, (relative, row)).is_some() {
                return Err("duplicate_native_case");
            }
        }
    }
    if records.len() != 40 {
        return Err("incomplete_native_cases");
    }
    let mut artifacts = Vec::new();
    let mut generated_cases = Vec::new();
    let mut provenance = Vec::new();
    let mut seen = BTreeSet::new();
    let mut strata = BTreeMap::<String, usize>::new();
    let mut changed = Vec::new();
    let (mut segments, mut unknown, mut conflicts, mut clipped_ms) = (0, 0, 0, 0);
    let (mut timing_changes, mut text_changes, mut overlapping_pairs) = (0, 0, 0);
    for (index, case) in cases.iter().enumerate() {
        let stratum = case["stratum"].as_str().ok_or("case_invalid")?;
        if stratum == "long_form" {
            continue;
        }
        if !matches!(stratum, "english" | "mandarin" | "mixed" | "overlap") {
            return Err("case_invalid");
        }
        let id = case["case_id"].as_str().ok_or("case_invalid")?;
        let suffix = id
            .strip_prefix(&format!("{stratum}_"))
            .ok_or("case_invalid")?;
        if suffix.len() != 2
            || !suffix.bytes().all(|byte| byte.is_ascii_digit())
            || !(1..=10).contains(&suffix.parse::<u32>().map_err(|_| "case_invalid")?)
            || !seen.insert(id)
        {
            return Err("case_invalid");
        }
        *strata.entry(stratum.into()).or_default() += 1;
        // Preserve reference paths and metadata exactly; validate their bounded inputs.
        for key in ["ground_truth", "miaoji"] {
            read_bounded(
                &root,
                case[key].as_str().ok_or("case_invalid")?,
                32 * 1024 * 1024,
            )?;
        }
        let (source, row) = records.get(id).ok_or("native_case_missing")?;
        let raw = row["raw_text"].as_str().ok_or("native_raw_missing")?;
        let parsed = parse_raw_segments(raw)?;
        let previous_native = row["segments"]
            .as_array()
            .ok_or("native_segments_missing")?;
        if parsed.len() != previous_native.len() {
            return Err("segment_count_changed");
        }
        for (new, old) in parsed.iter().zip(previous_native) {
            if Some(new.start_ms) != old["t0_ms"].as_i64()
                || Some(u64::from(new.speaker_id)) != old["speaker_id"].as_u64()
            {
                return Err("segment_order_or_speaker_changed");
            }
            timing_changes += usize::from(Some(new.end_ms) != old["t1_ms"].as_i64());
            text_changes += usize::from(Some(new.text.as_str()) != old["text"].as_str());
        }
        let audio = read_bounded(&root, &format!("audio-moss/{id}.wav"), 512 * 1024 * 1024)?;
        let request = fixture_request(index, case, &audio)?;
        let response = response(&request, parsed);
        let adapted = response.adapt(&request).map_err(|error| error.0)?;
        for (raw, adapted) in response.segments.iter().zip(&adapted.segments) {
            if raw.text != adapted.text
                || u64::try_from(raw.start_ms).ok() != Some(adapted.start_ms)
                || u64::try_from(raw.end_ms)
                    .ok()
                    .map(|end| end.min(request.audio_duration_ms))
                    != Some(adapted.end_ms)
            {
                return Err("raw_content_or_timing_changed");
            }
        }
        for (index, current) in adapted.segments.iter().enumerate() {
            overlapping_pairs += adapted.segments[..index]
                .iter()
                .filter(|previous| previous.end_ms > current.start_ms)
                .count();
        }
        let output = canonical(&adapted);
        let previous: Value = serde_json::from_slice(&read_bounded(
            &root,
            &format!("outputs/moss-transcribe-q8-40-hybrid/{id}.json"),
            32 * 1024 * 1024,
        )?)
        .map_err(|_| "historical_canonical_invalid")?;
        if output != previous {
            changed.push(id);
        }
        let relative = format!("{OUTPUT}/{id}.json");
        let output_bytes = encoded(&output)?;
        provenance.push(json!({
            "case_id":id, "source":source, "raw_text_sha256":digest(raw.as_bytes()),
            "request":request, "canonical_sha256":digest(&output_bytes),
        }));
        artifacts.push((relative.clone(), output_bytes));
        let mut generated_case = case.clone();
        generated_case["local"] = Value::String(relative);
        generated_cases.push(generated_case);
        segments += adapted.segments.len();
        unknown += adapted.unknown_segments;
        conflicts += adapted.conflicting_speaker_segments;
        clipped_ms += adapted.clipped_tail_ms;
    }
    if seen.len() != 40
        || strata.len() != 4
        || strata.values().any(|count| *count != 10)
        || segments != 4607
        || clipped_ms != 84
        || changed != EXPECTED_CHANGED
    {
        return Err("aggregate_mismatch");
    }
    let summary = json!({
        "state":"pass", "scope":"retained-public-raw-canonical40", "cases":seen.len(),
        "segments":segments, "unknown_segments":unknown, "conflicting_speaker_segments":conflicts,
        "clipped_tail_ms":clipped_ms, "overlapping_segment_pairs":overlapping_pairs,
        "changed_case_ids":changed, "timing_changes":timing_changes, "text_changes":text_changes,
        "inference_run":false,
    });
    let mut generated_manifest = manifest.clone();
    generated_manifest["cases"] = Value::Array(generated_cases);
    artifacts.push((MANIFEST.into(), encoded(&generated_manifest)?));
    artifacts.push((format!("{OUTPUT}/generation.json"), encoded(&json!({
        "schema_version":1, "raw_timing_policy":RAW_TIMING_POLICY,
        "adaptation_policy":TIMING_POLICY,
        "model_identity_basis":"existing-public-fixture-request; historical CLI records carry no model identity attestation",
        "source_manifest_sha256":digest(&manifest_bytes), "sources":sources,
        "summary":summary, "cases":provenance,
    }))?));
    let output_path = safe_path(&root, OUTPUT)?;
    match fs::create_dir(&output_path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&output_path)
                .map_err(|_| "artifact_rejected")?
                .is_dir()
            {
                return Err("artifact_rejected");
            }
        }
        Err(_) => return Err("artifact_write_failed"),
    }
    let created = publish(&root, &artifacts)?;
    println!(
        "{}",
        json!({"summary":summary,"artifacts_created":created,"artifacts_verified":artifacts.len()})
    );
    Ok(())
}

#[test]
fn shared_parser_and_adapter_preserve_speech_overlap_and_tail_policy() {
    let request = fixture_request(0, &json!({"duration_ms":3000}), b"public-test-audio").unwrap();
    let raw =
        "[0][S01]hello 世界[1.2]\n[1.0][S02]real overlap[2]\n[2][S01]final [2026] version[3.084]";
    let adapted = response(&request, parse_raw_segments(raw).unwrap())
        .adapt(&request)
        .unwrap();
    assert_eq!(adapted.segments.len(), 3);
    assert_eq!(adapted.segments[0].text, "hello 世界");
    assert_eq!(
        (adapted.segments[0].end_ms, adapted.segments[1].start_ms),
        (1200, 1000)
    );
    assert_eq!(adapted.segments[1].speaker_id, Some(2));
    assert_eq!(adapted.segments[2].text, "final [2026] version");
    assert_eq!(adapted.segments[2].end_ms, 3000);
    assert_eq!(adapted.clipped_tail_ms, 84);
    assert_eq!(adapted.unknown_segments, 0);
}
