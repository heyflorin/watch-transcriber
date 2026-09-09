//! Opt-in replay of already generated public-corpus output. No inference,
//! credentials, audio playback, or writes occur in this test.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use echowall_local_moss_protocol::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn read_bounded(root: &Path, relative: &str, limit: u64) -> Result<Vec<u8>, &'static str> {
    let supplied = root.join(relative);
    let metadata = fs::symlink_metadata(&supplied).map_err(|_| "fixture_missing")?;
    let path = fs::canonicalize(&supplied).map_err(|_| "fixture_missing")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > limit
        || !path.starts_with(root)
    {
        return Err("fixture_rejected");
    }
    fs::read(path).map_err(|_| "fixture_read_failed")
}

#[test]
#[ignore = "replays the authorized 40-case public MOSS output and prints only aggregates"]
fn public_adapter_matches_all_completed_diagnostics() -> Result<(), &'static str> {
    if std::env::var("ECHOWALL_MOSS_REPLAY_CONFIRM").as_deref()
        != Ok("public-corpus-moss-adapter-authorized")
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
    let manifest: Value =
        serde_json::from_slice(&read_bounded(&root, "manifest.json", 1024 * 1024)?)
            .map_err(|_| "manifest_invalid")?;
    let cases = manifest["cases"].as_array().ok_or("manifest_invalid")?;
    let mut records = BTreeMap::new();
    for relative in [
        "outputs/moss-transcribe-q8-mmo/results.jsonl",
        "outputs/moss-transcribe-q8-english/results.jsonl",
    ] {
        let data = read_bounded(&root, relative, 32 * 1024 * 1024)?;
        for line in data
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
            let name = Path::new(row["file"].as_str().ok_or("native_file_missing")?)
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or("native_file_invalid")?
                .to_owned();
            if records.insert(name, row).is_some() {
                return Err("duplicate_native_case");
            }
        }
    }
    if records.len() != 40 {
        return Err("incomplete_native_cases");
    }
    let mut seen = BTreeSet::new();
    let mut segments = 0;
    let mut unknown = 0;
    let mut clipped_ms = 0;
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
        if suffix.len() != 2 || !suffix.bytes().all(|b| b.is_ascii_digit()) || !seen.insert(id) {
            return Err("case_invalid");
        }
        let row = records.get(id).ok_or("native_case_missing")?;
        let audio = read_bounded(&root, &format!("audio-moss/{id}.wav"), 512 * 1024 * 1024)?;
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
            audio_sha256: Sha256::digest(&audio)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            audio_size_bytes: audio.len() as u64,
            audio_duration_ms: case["duration_ms"].as_u64().ok_or("case_invalid")?,
            language: None,
        };
        let raw_segments = row["segments"]
            .as_array()
            .ok_or("native_segments_missing")?
            .iter()
            .map(|segment| {
                Ok(MossSegment {
                    start_ms: segment["t0_ms"].as_i64().ok_or("native_timing_invalid")?,
                    end_ms: segment["t1_ms"].as_i64().ok_or("native_timing_invalid")?,
                    speaker_id: u32::try_from(
                        segment["speaker_id"]
                            .as_u64()
                            .ok_or("native_speaker_invalid")?,
                    )
                    .map_err(|_| "native_speaker_invalid")?,
                    text: segment["text"]
                        .as_str()
                        .ok_or("native_text_invalid")?
                        .to_owned(),
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        let response = MossResponse {
            schema_version: PROTOCOL_VERSION,
            recording_id,
            runtime_id: RUNTIME_ID.into(),
            model_id: MODEL_ID.into(),
            model_sha256: MODEL_SHA256.into(),
            audio_sha256: request.audio_sha256.clone(),
            timing_policy: TIMING_POLICY.into(),
            complete: true,
            segments: raw_segments,
        };
        let adapted = response.adapt(&request).map_err(|error| error.0)?;
        let canonical = json!({"schema_version": 1, "segments": adapted.segments.iter().map(|segment| json!({
            "start_ms": segment.start_ms, "end_ms": segment.end_ms,
            "speaker": segment.speaker_id.map_or_else(|| "local_unknown".into(), |id| format!("moss_speaker_{id}")),
            "text": segment.text,
        })).collect::<Vec<_>>()});
        let previous: Value = serde_json::from_slice(&read_bounded(
            &root,
            &format!("outputs/moss-transcribe-q8-40-hybrid/{id}.json"),
            32 * 1024 * 1024,
        )?)
        .map_err(|_| "diagnostic_invalid")?;
        if canonical != previous {
            return Err("diagnostic_parity_mismatch");
        }
        segments += adapted.segments.len();
        unknown += adapted.unknown_segments;
        clipped_ms += adapted.clipped_tail_ms;
    }
    if seen.len() != 40 || segments != 4607 || unknown != 4 || clipped_ms != 84 {
        return Err("aggregate_mismatch");
    }
    println!(
        "{}",
        json!({"state":"pass", "cases":seen.len(), "segments":segments,
        "unknown_segments":unknown, "clipped_tail_ms":clipped_ms,
        "text_and_timing_match":true, "inference_run":false})
    );
    Ok(())
}
