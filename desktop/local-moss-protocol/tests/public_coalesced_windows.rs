//! Explicit re-adaptation of32 retained public window responses; no inference.
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use echowall_local_moss_protocol::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[path = "support/public_raw_files.rs"]
mod public_raw_files;
use public_raw_files::{publish, read_bounded, safe_path};

const OUTPUT: &str = "outputs/moss-quiet12-coalesced-v2";
const MANIFEST: &str = "manifest-moss-quiet12-coalesced-v2-long4.json";
const SOURCE_MANIFEST: &str = "manifest-moss-quiet12-asr-long4-v1.json";

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

fn read_json(root: &Path, relative: &str, limit: u64) -> Result<(Value, Vec<u8>), &'static str> {
    let bytes = read_bounded(root, relative, limit)?;
    let value = serde_json::from_slice(&bytes).map_err(|_| "fixture_json_invalid")?;
    Ok((value, bytes))
}

fn canonical(segments: &[ValidatedSegment]) -> Value {
    json!({"schema_version":1,"segments":segments.iter().map(|segment| json!({
        "start_ms":segment.start_ms,"end_ms":segment.end_ms,"text":segment.text,
        "speaker":segment.speaker_id.map_or_else(|| "local_unknown".into(), |id| format!("moss_speaker_{id}")),
    })).collect::<Vec<_>>()})
}

fn union(mut intervals: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    intervals.sort_unstable();
    let mut output: Vec<(u64, u64)> = Vec::new();
    for (start, end) in intervals {
        if let Some(previous) = output.last_mut() {
            if start <= previous.1 {
                previous.1 = previous.1.max(end);
                continue;
            }
        }
        output.push((start, end));
    }
    output
}

fn joined(segments: &[ValidatedSegment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn intervals(segments: &[ValidatedSegment]) -> Vec<(u64, u64)> {
    union(
        segments
            .iter()
            .map(|segment| (segment.start_ms, segment.end_ms))
            .collect(),
    )
}

fn preservation_proof(
    raw: &[MossSegment],
    legacy: &AdaptedTranscript,
    coalesced: &AdaptedTranscript,
    duration: u64,
) -> Result<Value, &'static str> {
    let raw_text = raw
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if raw_text.as_bytes() != joined(&legacy.segments).as_bytes()
        || raw_text.as_bytes() != joined(&coalesced.segments).as_bytes()
        || raw.len() != legacy.segments.len()
        || raw.len() != coalesced.segments.len() + coalesced.coalesced_source_segments
        || legacy.clipped_tail_ms != coalesced.clipped_tail_ms
    {
        return Err("text_order_or_source_count_changed");
    }
    let clipped_raw = union(
        raw.iter()
            .map(|segment| {
                Ok((
                    u64::try_from(segment.start_ms).map_err(|_| "source_timing_invalid")?,
                    u64::try_from(segment.end_ms)
                        .map_err(|_| "source_timing_invalid")?
                        .min(duration),
                ))
            })
            .collect::<Result<Vec<_>, &'static str>>()?,
    );
    if clipped_raw != intervals(&legacy.segments) || clipped_raw != intervals(&coalesced.segments) {
        return Err("source_interval_union_changed");
    }
    Ok(json!({"joined_text_sha256":digest(raw_text.as_bytes()),
        "interval_union_sha256":digest(&encoded(&json!(clipped_raw))?),
        "joined_text_bytes_and_order_preserved":true,"clipped_source_interval_union_preserved":true}))
}

struct ReplayComputation {
    artifacts: Vec<(String, Vec<u8>)>,
    summary: Value,
}

fn fixture_root() -> Result<PathBuf, &'static str> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("repository_missing")?;
    fs::canonicalize(repository.join("local-eval/matrix")).map_err(|_| "fixture_root_missing")
}

#[test]
#[ignore = "re-adapts32 retained public windows with explicit v2 policy; requires confirmation"]
fn retained_quiet_windows_coalesce_only_under_explicit_v2() -> Result<(), &'static str> {
    if std::env::var("ECHOWALL_MOSS_COALESCED_REPLAY_CONFIRM").as_deref()
        != Ok("public-coalesced-window-replay-authorized")
    {
        return Err("confirmation_required");
    }
    let root = fixture_root()?;
    let replay = compute_replay(&root)?;
    let directory = safe_path(&root, OUTPUT)?;
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&directory)
                .map_err(|_| "artifact_rejected")?
                .is_dir()
            {
                return Err("artifact_rejected");
            }
        }
        Err(_) => return Err("artifact_write_failed"),
    }
    let created = publish(&root, &replay.artifacts)?;
    println!(
        "{}",
        json!({"cases":replay.summary["cases"],"windows":replay.summary["windows"],
        "source_segments":replay.summary["source_segments"],"output_segments":replay.summary["output_segments"],
        "coalesced_source_segments":replay.summary["coalesced_source_segments"],"unknown_segments":replay.summary["unknown_segments"],
        "artifacts_created":created,"artifacts_verified":replay.artifacts.len(),"inference_run":false})
    );
    Ok(())
}

/// Recompute only from retained inputs. Publication is exclusively in the
/// generation test above; the verification path never calls a writer.
fn compute_replay(root: &Path) -> Result<ReplayComputation, &'static str> {
    let root = root.to_owned();
    let (manifest, manifest_bytes) = read_json(&root, SOURCE_MANIFEST, 1024 * 1024)?;
    let cases = manifest["cases"].as_array().ok_or("manifest_invalid")?;
    if manifest["schema_version"] != 1 || cases.len() != 4 {
        return Err("manifest_invalid");
    }
    let mut artifacts = Vec::new();
    let mut generated_cases = Vec::new();
    let mut summaries = Vec::new();
    let (mut window_total, mut source_total, mut output_total, mut merged_total, mut unknown_total) =
        (0, 0, 0, 0, 0);
    for (case_index, case) in cases.iter().enumerate() {
        let id = format!("long_form_{:02}", case_index + 1);
        let source_dir = format!("outputs/moss-quiet12-asr-long{:02}-v1", case_index + 1);
        if case["case_id"] != id
            || case["stratum"] != "long_form"
            || case["local"] != format!("{source_dir}/candidate.json")
        {
            return Err("case_scope_invalid");
        }
        let provenance_path = format!("{source_dir}/provenance.json");
        let (original_provenance, provenance_bytes) =
            read_json(&root, &provenance_path, 1024 * 1024)?;
        let receipts = original_provenance["receipts"]
            .as_array()
            .ok_or("provenance_invalid")?;
        let pcm_duration = original_provenance["source_pcm_duration_ms"]
            .as_u64()
            .ok_or("provenance_invalid")?;
        if original_provenance["case_id"] != id
            || original_provenance["windows"] != 8
            || receipts.len() != 8
            || pcm_duration > case["duration_ms"].as_u64().ok_or("case_invalid")?
        {
            return Err("provenance_invalid");
        }
        let mut legacy_assembled = Vec::new();
        let mut coalesced_assembled = Vec::new();
        let mut generated_receipts = Vec::new();
        let (mut last_end, mut source_count, mut merged_count, mut unknown_count, mut clipped_ms) =
            (0, 0, 0, 0, 0);
        for (index, receipt) in receipts.iter().enumerate() {
            let start = receipt["start_ms"].as_u64().ok_or("receipt_invalid")?;
            let end = receipt["end_ms"].as_u64().ok_or("receipt_invalid")?;
            let directory = receipt["directory"].as_str().ok_or("receipt_invalid")?;
            let prefix = format!("{id}-quiet12-window{index:02}-");
            if receipt["index"] != index
                || start != last_end
                || end <= start
                || end > pcm_duration
                || !directory.strip_prefix(&prefix).is_some_and(|stamp| {
                    !stamp.is_empty() && stamp.bytes().all(|byte| byte.is_ascii_digit())
                })
            {
                return Err("receipt_invalid");
            }
            last_end = end;
            let native = format!("outputs/moss-native-worker-v1/{directory}");
            let request_path = format!("{native}/request.json");
            let response_path = format!("{native}/response.json");
            let result_path = format!("{native}/result.json");
            let canonical_path = format!("{native}/canonical.json");
            let request_bytes = read_bounded(&root, &request_path, MAX_REQUEST_BYTES as u64)?;
            let response_bytes = read_bounded(&root, &response_path, MAX_RESPONSE_BYTES as u64)?;
            let request = decode_request(&request_bytes).map_err(|error| error.0)?;
            let response = decode_response(&response_bytes, &request).map_err(|error| error.0)?;
            if request.timing_policy != TIMING_POLICY
                || response.timing_policy != TIMING_POLICY
                || request.audio_duration_ms != end - start
            {
                return Err("legacy_policy_or_duration_mismatch");
            }
            let (result, result_bytes) = read_json(&root, &result_path, 1024 * 1024)?;
            if result["state"] != "pass"
                || result["case_id"] != id
                || result["raw_timing_policy"] != RAW_TIMING_POLICY
                || result["model_sha256"] != MODEL_SHA256
                || result["runtime_id"] != RUNTIME_ID
                || result["audio_duration_ms"] != request.audio_duration_ms
                || result["window"]["index"] != index
                || result["window"]["source_start_ms"] != start
                || result["window"]["source_end_ms"] != end
                || result["window"]["source_pcm_duration_ms"] != pcm_duration
                || result["window"]["source_sha256"] != original_provenance["source_sha256"]
            {
                return Err("retained_native_receipt_mismatch");
            }
            let legacy = response.adapt(&request).map_err(|error| error.0)?;
            let (previous, previous_bytes) = read_json(&root, &canonical_path, 32 * 1024 * 1024)?;
            if canonical(&legacy.segments) != previous
                || receipt["canonical_sha256"] != digest(&previous_bytes)
                || result["segments"] != legacy.segments.len()
                || result["unknown_segments"] != legacy.unknown_segments
                || result["clipped_tail_ms"] != legacy.clipped_tail_ms
                || legacy.coalesced_source_segments != 0
            {
                return Err("legacy_canonical_or_metrics_mismatch");
            }
            let mut revised_request = request.clone();
            let mut revised_response = response.clone();
            revised_request.timing_policy = COALESCING_TIMING_POLICY_V2.into();
            revised_response.timing_policy = COALESCING_TIMING_POLICY_V2.into();
            let coalesced = revised_response
                .adapt(&revised_request)
                .map_err(|error| error.0)?;
            let proof = preservation_proof(
                &response.segments,
                &legacy,
                &coalesced,
                request.audio_duration_ms,
            )?;
            let relative = format!("{OUTPUT}/{id}-window{index:02}.json");
            let bytes = encoded(&canonical(&coalesced.segments))?;
            generated_receipts.push(json!({"index":index,"start_ms":start,"end_ms":end,
                "canonical_relative_path":relative,"canonical_sha256":digest(&bytes),
                "original_request":{"relative_path":request_path,"sha256":digest(&request_bytes)},
                "original_response":{"relative_path":response_path,"sha256":digest(&response_bytes)},
                "original_result":{"relative_path":result_path,"sha256":digest(&result_bytes)},
                "original_canonical":{"relative_path":canonical_path,"sha256":digest(&previous_bytes)},
                "recording_id":request.recording_id,"retained_audio_sha256":request.audio_sha256,
                "raw_timing_policy":RAW_TIMING_POLICY,"original_adaptation_policy":TIMING_POLICY,
                "replay_adaptation_policy":COALESCING_TIMING_POLICY_V2,"only_adaptation_policy_changed":true,
                "source_segments":response.segments.len(),"output_segments":coalesced.segments.len(),
                "coalesced_source_segments":coalesced.coalesced_source_segments,
                "legacy_unknown_segments":legacy.unknown_segments,"unknown_segments":coalesced.unknown_segments,
                "clipped_tail_ms":coalesced.clipped_tail_ms,"proof":proof}));
            artifacts.push((relative, bytes));
            source_count += response.segments.len();
            merged_count += coalesced.coalesced_source_segments;
            unknown_count += coalesced.unknown_segments;
            clipped_ms += coalesced.clipped_tail_ms;
            for (source, target) in [
                (&legacy.segments, &mut legacy_assembled),
                (&coalesced.segments, &mut coalesced_assembled),
            ] {
                for segment in source {
                    let mut shifted = segment.clone();
                    shifted.start_ms += start;
                    shifted.end_ms += start;
                    target.push(shifted);
                }
            }
            window_total += 1;
        }
        let (previous_assembled, previous_assembled_bytes) = read_json(
            &root,
            &format!("{source_dir}/candidate.json"),
            32 * 1024 * 1024,
        )?;
        if last_end != pcm_duration
            || canonical(&legacy_assembled) != previous_assembled
            || joined(&legacy_assembled).as_bytes() != joined(&coalesced_assembled).as_bytes()
            || intervals(&legacy_assembled) != intervals(&coalesced_assembled)
            || original_provenance["segments"] != source_count
        {
            return Err("assembled_preservation_mismatch");
        }
        let relative = format!("{OUTPUT}/{id}.json");
        let bytes = encoded(&canonical(&coalesced_assembled))?;
        let summary = json!({"case_id":id,"windows":8,"source_segments":source_count,
            "output_segments":coalesced_assembled.len(),"coalesced_source_segments":merged_count,
            "unknown_segments":unknown_count,"clipped_tail_ms":clipped_ms});
        let provenance = json!({"schema_version":1,"scope":"retained_response_readaptation_ASR_only",
            "case_id":id,"windows":8,"segments":coalesced_assembled.len(),"source_pcm_duration_ms":pcm_duration,
            "source_sha256":original_provenance["source_sha256"],"raw_timing_policy":RAW_TIMING_POLICY,
            "adaptation_policy":COALESCING_TIMING_POLICY_V2,"inference_run":false,"fresh_model_attestation":false,
            "global_speaker_identity_validated":false,"release_acceptance":false,
            "original_provenance":{"relative_path":provenance_path,"sha256":digest(&provenance_bytes)},
            "original_assembled_sha256":digest(&previous_assembled_bytes),"canonical_sha256":digest(&bytes),
            "joined_text_sha256":digest(joined(&coalesced_assembled).as_bytes()),
            "interval_union_sha256":digest(&encoded(&json!(intervals(&coalesced_assembled)))?),
            "joined_text_bytes_and_order_preserved":true,"clipped_source_interval_union_preserved":true,
            "summary":summary,"receipts":generated_receipts});
        artifacts.push((relative.clone(), bytes));
        artifacts.push((
            format!("{OUTPUT}/{id}.provenance.json"),
            encoded(&provenance)?,
        ));
        let mut generated_case = case.clone();
        generated_case["local"] = json!(relative);
        generated_cases.push(generated_case);
        source_total += source_count;
        output_total += coalesced_assembled.len();
        merged_total += merged_count;
        unknown_total += unknown_count;
        summaries.push(summary);
    }
    if window_total != 32 || source_total != output_total + merged_total {
        return Err("aggregate_mismatch");
    }
    let summary = json!({"schema_version":1,"scope":"retained_response_readaptation_ASR_only",
        "inference_run":false,"fresh_model_attestation":false,"global_speaker_identity_validated":false,
        "release_acceptance":false,"cases":4,"windows":window_total,"source_segments":source_total,
        "output_segments":output_total,"coalesced_source_segments":merged_total,"unknown_segments":unknown_total,
        "raw_timing_policy":RAW_TIMING_POLICY,"adaptation_policy":COALESCING_TIMING_POLICY_V2,
        "source_manifest":{"relative_path":SOURCE_MANIFEST,"sha256":digest(&manifest_bytes)},
        "replay_test_sha256":digest(include_bytes!("public_coalesced_windows.rs")),
        "protocol_source_sha256":digest(include_bytes!("../src/lib.rs")),"case_summaries":summaries});
    artifacts.push((format!("{OUTPUT}/replay-summary.json"), encoded(&summary)?));
    let mut generated_manifest = manifest.clone();
    generated_manifest["cases"] = json!(generated_cases);
    artifacts.push((MANIFEST.into(), encoded(&generated_manifest)?));
    Ok(ReplayComputation { artifacts, summary })
}

/// Historical source hashes are retained declarations, not hashes of the
/// current verifier. Only these two fields may differ; all content, receipts,
/// policies, input hashes, counts, and other summary fields must still match.
fn verify_summary(recorded: &Value, current: &Value) -> Result<Value, &'static str> {
    let mut recorded_stable = recorded.clone();
    let mut current_stable = current.clone();
    let mut hashes = serde_json::Map::new();
    for key in ["replay_test_sha256", "protocol_source_sha256"] {
        let recorded_hash = recorded[key]
            .as_str()
            .ok_or("historical_source_hash_invalid")?;
        let current_hash = current[key].as_str().ok_or("current_source_hash_invalid")?;
        for hash in [recorded_hash, current_hash] {
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err("source_hash_invalid");
            }
        }
        hashes.insert(key.into(), json!({"recorded":recorded_hash,"current":current_hash,"drift":recorded_hash != current_hash}));
        recorded_stable
            .as_object_mut()
            .ok_or("summary_invalid")?
            .remove(key);
        current_stable
            .as_object_mut()
            .ok_or("summary_invalid")?
            .remove(key);
    }
    if recorded_stable != current_stable {
        return Err("stable_summary_mismatch");
    }
    Ok(Value::Object(hashes))
}

#[test]
#[ignore = "read-only verification of42 retained coalesced artifacts; reports historical/current source drift"]
fn retained_coalesced_windows_verify_read_only_with_source_drift() -> Result<(), &'static str> {
    if std::env::var("ECHOWALL_MOSS_COALESCED_VERIFY_CONFIRM").as_deref()
        != Ok("public-coalesced-window-verification-authorized")
    {
        return Err("confirmation_required");
    }
    let root = fixture_root()?;
    let replay = compute_replay(&root)?;
    if replay.artifacts.len() != 42 {
        return Err("artifact_count_mismatch");
    }
    let summary_path = format!("{OUTPUT}/replay-summary.json");
    let mut source_hashes = None;
    let mut snapshots = Vec::with_capacity(replay.artifacts.len());
    for (relative, expected) in &replay.artifacts {
        let recorded_bytes = read_bounded(&root, relative, 32 * 1024 * 1024)?;
        if relative == &summary_path {
            let recorded: Value =
                serde_json::from_slice(&recorded_bytes).map_err(|_| "summary_invalid")?;
            source_hashes = Some(verify_summary(&recorded, &replay.summary)?);
        } else if &recorded_bytes != expected {
            return Err("retained_artifact_mismatch");
        }
        snapshots.push((relative, digest(&recorded_bytes)));
    }
    // This also catches concurrent content changes during verification. The
    // original byte snapshots, including historical provenance hashes, survive.
    for (relative, before) in snapshots {
        if digest(&read_bounded(&root, relative, 32 * 1024 * 1024)?) != before {
            return Err("retained_artifact_changed_during_verification");
        }
    }
    println!(
        "{}",
        json!({"state":"pass","mode":"read_only_retained_coalesced_verification",
        "cases":replay.summary["cases"],"windows":replay.summary["windows"],
        "source_segments":replay.summary["source_segments"],"output_segments":replay.summary["output_segments"],
        "coalesced_source_segments":replay.summary["coalesced_source_segments"],"unknown_segments":replay.summary["unknown_segments"],
        "artifacts_verified":42,"exact_byte_artifacts":41,"stable_summary_artifacts":1,"writes":0,
        "historical_artifact_bytes_unchanged":true,"historical_hashes_reattested":false,
        "current_verifier_sha256":replay.summary["replay_test_sha256"],
        "current_protocol_source_sha256":replay.summary["protocol_source_sha256"],
        "source_hash_scope":"public_coalesced_windows.rs_and_src/lib.rs",
        "source_hashes":source_hashes.ok_or("summary_missing")?,"inference_run":false})
    );
    Ok(())
}

#[test]
fn summary_verification_allows_only_explicit_source_hash_drift_without_mutation() {
    let recorded = json!({"replay_test_sha256":digest(b"historical test"),
        "protocol_source_sha256":digest(b"historical protocol"),"windows":32,
        "source_manifest":{"sha256":digest(b"retained input")}});
    let snapshot = recorded.clone();
    let mut current = recorded.clone();
    current["replay_test_sha256"] = json!(digest(b"current verifier"));
    current["protocol_source_sha256"] = json!(digest(b"current protocol"));
    let hashes = verify_summary(&recorded, &current).unwrap();
    assert_eq!(
        hashes["replay_test_sha256"]["recorded"],
        recorded["replay_test_sha256"]
    );
    assert_eq!(hashes["replay_test_sha256"]["drift"], true);
    assert_eq!(recorded, snapshot);
    current["windows"] = json!(31);
    assert_eq!(
        verify_summary(&recorded, &current),
        Err("stable_summary_mismatch")
    );
    current["windows"] = json!(32);
    current["source_manifest"]["sha256"] = json!(digest(b"different input"));
    assert_eq!(
        verify_summary(&recorded, &current),
        Err("stable_summary_mismatch")
    );
    let mut malformed = recorded.clone();
    malformed["protocol_source_sha256"] = json!("not-a-hash");
    assert_eq!(
        verify_summary(&malformed, &recorded),
        Err("source_hash_invalid")
    );
}

#[test]
fn interval_union_proof_preserves_overlap_and_gaps() {
    assert_eq!(
        union(vec![(4, 7), (0, 3), (2, 4), (9, 10)]),
        vec![(0, 7), (9, 10)]
    );
    assert_ne!(union(vec![(0, 4), (6, 8)]), union(vec![(0, 8)]));
}
