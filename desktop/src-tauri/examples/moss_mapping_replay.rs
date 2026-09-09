//! Public-corpus proof through the actual App plan/bundle/finalizer.
//! In-memory candidate receipt rebinding is explicitly a replay experiment,
//! never new inference or a ledger migration. Output contains aggregates only.
//! An optional native run name writes isolated create-new-or-match replay files.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use desktop_lib::processing::local_moss::{
    self, CompleteMossResponses, LocalMossPlan, ResponseBinding, ValidatedMossWindowResponse,
    ValidatedSpeakerKitResponse,
};
use desktop_lib::processing::local_whisper::{
    decode_diarization_request, decode_diarization_response,
    LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const RUN: &str = "outputs/moss-uniform-app-v1/run-7RKXpm";
const OUTCOME: &str = "outcome-398747e3-31cc-4984-bf83-5b705e9a4f36.json";
const OUTCOME_SHA: &str = "6c03330c8489820680aff7b7e1c70ee85d88db21a5e4a9663a16d7397fcb85ab";
const CANDIDATE: &str = "single-window-native-multi-window-graph3-v1";
const NATIVE_PARENT: &str = "outputs/speakerkit-tail-context-v2";
const NATIVE_PRESET: &str = "speakerkit-pyannote-v3-exclusive-tail-context-v2";
const NATIVE_WORKER_SHA: &str = "ff120dbd199421ac212105fed595857da022819fccf9ae9428daed4186ba35a9";
const REPLAY_SCOPE: &str = "actual-App-finalizer-public-retained-MOSS-fresh-anchor-replay-not-inference-or-ledger-migration";
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn checked_path(root: &Path, relative: &str) -> Result<PathBuf, &'static str> {
    if !fs::symlink_metadata(root)
        .map_err(|_| "input_unavailable")?
        .is_dir()
        || fs::canonicalize(root).map_err(|_| "input_unavailable")? != root
    {
        return Err("root_rejected");
    }
    let mut target = root.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || matches!(part, "." | "..") || part.contains(['\\', '\0']) {
            return Err("path_rejected");
        }
        target.push(part);
        if fs::symlink_metadata(&target)
            .map_err(|_| "input_unavailable")?
            .file_type()
            .is_symlink()
        {
            return Err("symlink_rejected");
        }
    }
    Ok(target)
}
fn open_input(root: &Path, relative: &str, limit: u64) -> Result<fs::File, &'static str> {
    let target = checked_path(root, relative)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(target).map_err(|_| "input_unavailable")?;
    let metadata = file.metadata().map_err(|_| "input_unavailable")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err("input_rejected");
    }
    Ok(file)
}
fn read(root: &Path, relative: &str) -> Result<Vec<u8>, &'static str> {
    let file = open_input(root, relative, MAX_FILE_BYTES)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input_unavailable")?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("input_rejected");
    }
    Ok(bytes)
}
fn file_identity(root: &Path, relative: &str, limit: u64) -> Result<Value, &'static str> {
    let mut file = open_input(root, relative, limit)?.take(limit + 1);
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| "input_unavailable")?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > limit {
            return Err("input_rejected");
        }
        hash.update(&buffer[..count]);
    }
    let sha256: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(json!({"relative_path":relative,"sha256":sha256,"size_bytes":size}))
}
fn parse(bytes: &[u8]) -> Result<Value, &'static str> {
    serde_json::from_slice(bytes).map_err(|_| "invalid_input")
}
fn string(value: &Value) -> Result<&str, &'static str> {
    value.as_str().ok_or("invalid_input")
}

fn canonical(transcript: &Value) -> Result<Value, &'static str> {
    let segments = transcript
        .as_array()
        .ok_or("transcript_invalid")?
        .iter()
        .map(|s| {
            json!({"start_ms":s["start_time"],"end_ms":s["end_time"],
            "speaker":s["speaker"]["id"],"text":s["content"]})
        })
        .collect::<Vec<_>>();
    Ok(json!({"schema_version":1,"segments":segments}))
}

fn windows(
    root: &Path,
    base: &str,
    plan: &LocalMossPlan,
    old_plan: &LocalMossPlan,
    ledger: &Value,
) -> Result<CompleteMossResponses, &'static str> {
    plan.require_executable().map_err(|e| e.code)?;
    let receipts = ledger["completed_windows"]
        .as_array()
        .ok_or("receipts_missing")?;
    if receipts.len() != plan.windows().len() {
        return Err("receipts_mismatch");
    }
    let mut windows = Vec::new();
    for (index, receipt) in receipts.iter().enumerate() {
        let body = read(root, &format!("{base}/window-{index:02}.json"))?;
        let mut binding: ResponseBinding =
            serde_json::from_value(receipt["binding"].clone()).map_err(|_| "receipt_invalid")?;
        if binding.plan_sha256 != old_plan.plan_sha256()
            || binding.request_sha256 != old_plan.windows()[index].request_sha256()
            || plan.windows()[index].request_bytes() != old_plan.windows()[index].request_bytes()
            || binding.response_sha256 != digest(&body)
            || receipt["reference"]["sha256"] != binding.response_sha256
            || receipt["reference"]["size_bytes"] != body.len()
        {
            return Err("retained_identity_changed");
        }
        // Explicit fixture rebinding only; original receipt is never persisted.
        binding.plan_sha256 = plan.plan_sha256().to_owned();
        windows.push(
            ValidatedMossWindowResponse::decode(plan, index, binding, &body).map_err(|e| e.code)?,
        );
    }
    CompleteMossResponses::new(plan, windows).map_err(|e| e.code)
}

fn finalize(
    root: &Path,
    base: &str,
    plan: &LocalMossPlan,
    old_plan: &LocalMossPlan,
    ledger: &Value,
) -> Result<Value, &'static str> {
    let windows = windows(root, base, plan, old_plan, ledger)?;
    let body = read(root, &format!("{base}/anchors.json"))?;
    let mut binding: ResponseBinding = serde_json::from_value(ledger["anchors"]["binding"].clone())
        .map_err(|_| "receipt_invalid")?;
    if binding.plan_sha256 != old_plan.plan_sha256()
        || binding.request_sha256 != old_plan.diarization_request_sha256()
        || plan.diarization_request_bytes() != old_plan.diarization_request_bytes()
        || binding.response_sha256 != digest(&body)
        || ledger["anchors"]["reference"]["sha256"] != binding.response_sha256
        || ledger["anchors"]["reference"]["size_bytes"] != body.len()
    {
        return Err("retained_identity_changed");
    }
    binding.plan_sha256 = plan.plan_sha256().to_owned();
    let anchors = ValidatedSpeakerKitResponse::decode(plan, binding, &body).map_err(|e| e.code)?;
    let result = local_moss::finalize(plan, &windows, &anchors).map_err(|e| e.code)?;
    canonical(result.transcript_json())
}

fn original_replay(matrix: &Path) -> Result<Value, &'static str> {
    let outcome_bytes = read(matrix, &format!("{RUN}/{OUTCOME}"))?;
    if digest(&outcome_bytes) != OUTCOME_SHA {
        return Err("outcome_changed");
    }
    let outcome = parse(&outcome_bytes)?;
    let mut cases = 0;
    let mut segments = 0;
    let mut single_window = 0;
    let mut old_unknown = 0;
    let mut new_unknown = 0;
    for case in outcome["cases"].as_array().ok_or("cases_missing")? {
        if case["successful"] != true {
            continue;
        }
        let id = string(&case["case_id"])?;
        let recording = string(&case["recording_id"])?;
        let generation = string(&case["generation"])?;
        let base = format!("{RUN}/processing/moss/{recording}/{generation}");
        let plan_bytes = read(matrix, &format!("{base}/plan.json"))?;
        let old = LocalMossPlan::from_json(&plan_bytes).map_err(|e| e.code)?;
        if old.spec().schema_version != 2
            || old.plan_bytes() != plan_bytes
            || old.plan_sha256() != string(&case["plan_sha256"])?
        {
            return Err("retained_plan_changed");
        }
        let ledger = parse(&read(
            matrix,
            &format!("{RUN}/processing/jobs/{recording}.json"),
        )?)?;
        if ledger["local_moss"]["generation"] != generation
            || ledger["local_moss"]["plan"]["sha256"] != old.plan_sha256()
        {
            return Err("retained_ledger_changed");
        }
        let expected_bytes = read(matrix, &format!("{RUN}/canonical/{id}.json"))?;
        if digest(&expected_bytes) != string(&case["canonical_sha256"])? {
            return Err("retained_canonical_changed");
        }
        let expected = parse(&expected_bytes)?;
        let previous = finalize(matrix, &base, &old, &old, &ledger["local_moss"])?;
        if previous != expected {
            return Err("schema2_replay_mismatch");
        }
        let mut candidate_spec = parse(&plan_bytes)?;
        candidate_spec["schema_version"] = json!(3);
        candidate_spec["mapping_policy"] = json!(CANDIDATE);
        let candidate = LocalMossPlan::from_json(
            &serde_json::to_vec(&candidate_spec).map_err(|_| "fixture_encoding_failed")?,
        )
        .map_err(|e| e.code)?;
        if candidate.plan_sha256() == old.plan_sha256() {
            return Err("policy_identity_not_changed");
        }
        let updated = finalize(matrix, &base, &candidate, &old, &ledger["local_moss"])?;
        let challenger = parse(&read(
            matrix,
            &format!("diagnostics/uniform-mapping/single-window-native-v1/{id}.json"),
        )?)?;
        if updated != challenger {
            return Err("candidate_replay_mismatch");
        }
        let rows = updated["segments"].as_array().ok_or("transcript_invalid")?;
        segments += rows.len();
        old_unknown += previous["segments"]
            .as_array()
            .ok_or("transcript_invalid")?
            .iter()
            .filter(|s| s["speaker"] == "local_unknown")
            .count();
        new_unknown += rows
            .iter()
            .filter(|s| s["speaker"] == "local_unknown")
            .count();
        cases += 1;
        single_window += usize::from(old.windows().len() == 1);
    }
    if cases != 42 || read(matrix, &format!("{RUN}/{OUTCOME}"))? != outcome_bytes {
        return Err("corpus_or_retained_outcome_changed");
    }
    Ok(
        json!({"scope":"read-only-actual-App-finalizer-public-replay-not-inference-or-ledger-migration",
        "schema2_exact_cases":cases,"schema3_challenger_exact_cases":cases,"segments":segments,
        "single_window_cases":single_window,"old_unknown_segments":old_unknown,
        "candidate_unknown_segments":new_unknown,"failed_cases_retained":2,"artifact_writes":0}),
    )
}

fn valid_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty()
            && name.len() <= 64
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

fn ensure_directory(root: &Path, relative: &str) -> Result<PathBuf, &'static str> {
    let mut target = root.to_path_buf();
    if fs::canonicalize(root).map_err(|_| "output_root_rejected")? != root {
        return Err("output_root_rejected");
    }
    for part in relative.split('/') {
        if !valid_name(part, "") {
            return Err("output_path_rejected");
        }
        target.push(part);
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&target) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err("output_directory_unavailable"),
        }
        let metadata = fs::symlink_metadata(&target).map_err(|_| "output_directory_unavailable")?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || fs::canonicalize(&target).map_err(|_| "output_directory_unavailable")? != target
        {
            return Err("output_path_rejected");
        }
    }
    Ok(target)
}

fn write_artifact(
    run_root: &Path,
    kind: &str,
    id: &str,
    bytes: &[u8],
) -> Result<bool, &'static str> {
    if !matches!(kind, "canonical" | "evidence")
        || !valid_name(id, "")
        || bytes.is_empty()
        || bytes.len() as u64 > MAX_FILE_BYTES
    {
        return Err("output_rejected");
    }
    let relative = format!("app-mapping-replay/{kind}");
    let directory = ensure_directory(run_root, &relative)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(directory.join(format!("{id}.json"))) {
        Ok(mut file) => {
            file.write_all(bytes).map_err(|_| "output_write_failed")?;
            file.sync_all().map_err(|_| "output_write_failed")?;
            fs::File::open(directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| "output_write_failed")?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read(run_root, &format!("{relative}/{id}.json"))? != bytes {
                return Err("existing_output_mismatch");
            }
            Ok(false)
        }
        Err(_) => Err("output_write_failed"),
    }
}

fn optional_result(root: &Path, relative: &str) -> Result<Option<Vec<u8>>, &'static str> {
    // Result publication is the native runner's completion marker. Never read
    // a still-growing response merely because its file has appeared.
    let path = root.join(relative);
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("input_unavailable"),
        Ok(_) => read(root, relative).map(Some),
    }
}

fn fresh_anchor_replay(matrix: &Path, run_name: &str) -> Result<Value, &'static str> {
    if !valid_name(run_name, "run-")
        || NATIVE_PRESET != LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET
        || CANDIDATE != local_moss::COMPOSED_MAPPING_POLICY
    {
        return Err("native_run_rejected");
    }
    let run_relative = format!("{NATIVE_PARENT}/{run_name}");
    let run_root = checked_path(matrix, &run_relative)?;
    if !run_root.is_dir() {
        return Err("native_run_rejected");
    }
    let started_bytes = read(&run_root, "started.json")?;
    let started = parse(&started_bytes)?;
    if started["schema_version"] != 1
        || started["original_outcome_sha256"] != OUTCOME_SHA
        || started["source_root"] != format!("local-eval/matrix/{RUN}")
        || started["planned_cases"] != 44
        || started["quality_preset"] != NATIVE_PRESET
        || started["worker_sha256"] != NATIVE_WORKER_SHA
        || started.get("count_hint") != Some(&Value::Null)
        || started["reference_used_for_inference"] != false
        || started["playback"] != false
        || started["network_policy"] != "OS-deny-network"
        || started["credentials"] != "none"
        || digest(&read(&run_root, "runner.py")?) != string(&started["runner_sha256"])?
        || digest(&read(&run_root, "echowall-diarization-worker")?) != NATIVE_WORKER_SHA
    {
        return Err("native_run_identity_changed");
    }
    let outcome_bytes = read(matrix, &format!("{RUN}/{OUTCOME}"))?;
    if digest(&outcome_bytes) != OUTCOME_SHA {
        return Err("outcome_changed");
    }
    let outcome = parse(&outcome_bytes)?;
    let cases = outcome["cases"].as_array().ok_or("cases_missing")?;
    if cases.len() != 44 {
        return Err("corpus_scope_rejected");
    }
    let (mut replayed, mut pending, mut failed, mut original_failed) = (0, 0, 0, 0);
    let (mut single_window, mut segments, mut unknown, mut writes, mut matched) = (0, 0, 0, 0, 0);
    for case in cases {
        if case["successful"] != true {
            original_failed += 1;
            continue;
        }
        let id = string(&case["case_id"])?;
        if !valid_name(id, "") {
            return Err("case_id_rejected");
        }
        let Some(result_bytes) = optional_result(&run_root, &format!("{id}/result.json"))? else {
            pending += 1;
            continue;
        };
        let native_result = parse(&result_bytes)?;
        if native_result["case_id"] != id {
            return Err("native_case_identity_changed");
        }
        match native_result.get("successful").and_then(Value::as_bool) {
            Some(true) => (),
            Some(false) => {
                failed += 1;
                continue;
            }
            None => return Err("native_result_invalid"),
        }
        let recording = string(&case["recording_id"])?;
        let generation = string(&case["generation"])?;
        let base = format!("{RUN}/processing/moss/{recording}/{generation}");
        let plan_bytes = read(matrix, &format!("{base}/plan.json"))?;
        let old = LocalMossPlan::from_json(&plan_bytes).map_err(|e| e.code)?;
        if old.spec().schema_version != 2
            || old.plan_sha256() != string(&case["plan_sha256"])?
            || old.plan_bytes() != plan_bytes
        {
            return Err("retained_plan_changed");
        }
        let ledger_bytes = read(matrix, &format!("{RUN}/processing/jobs/{recording}.json"))?;
        let ledger = parse(&ledger_bytes)?;
        if ledger["local_moss"]["generation"] != generation
            || ledger["local_moss"]["plan"]["sha256"] != old.plan_sha256()
        {
            return Err("retained_ledger_changed");
        }
        let expected_bytes = read(matrix, &format!("{RUN}/canonical/{id}.json"))?;
        if digest(&expected_bytes) != string(&case["canonical_sha256"])? {
            return Err("retained_canonical_changed");
        }
        let previous = finalize(matrix, &base, &old, &old, &ledger["local_moss"])?;
        if previous != parse(&expected_bytes)? {
            return Err("schema2_replay_mismatch");
        }
        let request_bytes = read(&run_root, &format!("{id}/request.json"))?;
        let response_bytes = read(&run_root, &format!("{id}/response.json"))?;
        let request = decode_diarization_request(&request_bytes).map_err(|e| e.code)?;
        let response =
            decode_diarization_response(&response_bytes, &request).map_err(|e| e.code)?;
        let mut expected_request = old.diarization_request().clone();
        expected_request.quality_preset = NATIVE_PRESET.into();
        if request != expected_request
            || request.expected_speaker_count.is_some()
            || parse(&request_bytes)?.get("expected_speaker_count") != Some(&Value::Null)
            || native_result["exit_code"] != 0
            || native_result["bounded_stop"] != Value::Null
            || native_result["quality_preset"] != NATIVE_PRESET
            || native_result["worker_sha256"] != NATIVE_WORKER_SHA
            || native_result["source_plan_sha256"] != old.plan_sha256()
            || native_result["source_sha256"] != old.spec().source.sha256
            || native_result["request_sha256"] != digest(&request_bytes)
            || native_result["response_sha256"] != digest(&response_bytes)
            || native_result["speaker_count"] != response.speaker_count
            || native_result["segments"] != response.segments.len()
        {
            return Err("native_case_identity_changed");
        }
        let source = file_identity(
            matrix,
            &format!("{RUN}/{}", old.spec().source.relative_path),
            512 * 1024 * 1024,
        )?;
        if source["sha256"] != request.audio_sha256
            || source["size_bytes"] != request.audio_size_bytes
        {
            return Err("source_file_changed");
        }
        let mut model_files = Vec::new();
        for model in &request.model_files {
            let identity = file_identity(
                matrix,
                &format!(
                    "{RUN}/models/diarization/{}/{}",
                    request.pack_id, model.relative_path
                ),
                MAX_FILE_BYTES,
            )?;
            if identity["sha256"] != model.sha256 || identity["size_bytes"] != model.size_bytes {
                return Err("model_file_changed");
            }
            model_files.push(identity);
        }
        let mut spec = old.spec().clone();
        spec.schema_version = 3;
        spec.mapping_policy = Some(CANDIDATE.into());
        spec.diarization_request = request;
        let candidate = LocalMossPlan::new(spec).map_err(|e| e.code)?;
        let windows = windows(matrix, &base, &candidate, &old, &ledger["local_moss"])?;
        // Python wrote sorted, pretty request JSON. The typed request matches
        // the candidate plan, but its original raw bytes have a different hash
        // from Rust's canonical encoding. Preserve both identities explicitly.
        let anchors = ValidatedSpeakerKitResponse::decode(
            &candidate,
            ResponseBinding {
                plan_sha256: candidate.plan_sha256().into(),
                request_sha256: candidate.diarization_request_sha256().into(),
                response_sha256: digest(&response_bytes),
            },
            &response_bytes,
        )
        .map_err(|e| e.code)?;
        let finalized = local_moss::finalize(&candidate, &windows, &anchors).map_err(|e| e.code)?;
        let updated = canonical(finalized.transcript_json())?;
        let old_rows = previous["segments"]
            .as_array()
            .ok_or("transcript_invalid")?;
        let rows = updated["segments"].as_array().ok_or("transcript_invalid")?;
        if rows.len() != old_rows.len()
            || rows.iter().zip(old_rows).any(|(new, old)| {
                ["start_ms", "end_ms", "text"]
                    .iter()
                    .any(|key| new[key] != old[key])
            })
        {
            return Err("retained_transcript_content_changed");
        }
        let canonical_bytes =
            serde_json::to_vec(&updated).map_err(|_| "fixture_encoding_failed")?;
        let evidence = json!({
            "schema_version":1,"scope":REPLAY_SCOPE,"case_id":id,"native_run":run_name,
            "original_outcome_sha256":OUTCOME_SHA,
            "original_plan_sha256":old.plan_sha256(),"original_plan_size_bytes":plan_bytes.len(),
            "original_ledger_sha256":digest(&ledger_bytes),
            "original_canonical_sha256":digest(&expected_bytes),
            "original_window_receipts":ledger["local_moss"]["completed_windows"],
            "original_anchor_receipt":ledger["local_moss"]["anchors"],
            "source_file":source,"diarization_model_files":model_files,
            "native_started_sha256":digest(&started_bytes),"native_runner_sha256":started["runner_sha256"],
            "native_worker_sha256":NATIVE_WORKER_SHA,"native_result_sha256":digest(&result_bytes),
            "native_request_raw_sha256":digest(&request_bytes),"native_request_raw_size_bytes":request_bytes.len(),
            "native_response_sha256":digest(&response_bytes),"native_response_size_bytes":response_bytes.len(),
            "request_comparison":"closed-Rust-decoded-semantic-equality-with-original-request-changing-only-quality-preset",
            "request_raw_bytes_match_planned_encoding":request_bytes == candidate.diarization_request_bytes(),
            "replay_plan":candidate.spec(),"replay_plan_sha256":candidate.plan_sha256(),
            "replay_planned_anchor_request_sha256":candidate.diarization_request_sha256(),
            "receipt_binding_scope":"synthetic-in-memory-REPLAY-bindings-not-fresh-App-dispatch-receipts",
            "canonical_sha256":digest(&canonical_bytes),"canonical_size_bytes":canonical_bytes.len(),
            "finalization":finalized.evidence()
        });
        let evidence_bytes =
            serde_json::to_vec(&evidence).map_err(|_| "fixture_encoding_failed")?;
        if read(&run_root, &format!("{id}/result.json"))? != result_bytes
            || read(&run_root, &format!("{id}/request.json"))? != request_bytes
            || read(&run_root, &format!("{id}/response.json"))? != response_bytes
            || read(matrix, &format!("{base}/plan.json"))? != plan_bytes
        {
            return Err("replay_input_changed");
        }
        for (kind, bytes) in [("canonical", canonical_bytes), ("evidence", evidence_bytes)] {
            if write_artifact(&run_root, kind, id, &bytes)? {
                writes += 1;
            } else {
                matched += 1;
            }
        }
        replayed += 1;
        single_window += usize::from(candidate.windows().len() == 1);
        segments += rows.len();
        unknown += rows
            .iter()
            .filter(|row| row["speaker"] == "local_unknown")
            .count();
    }
    if original_failed != 2
        || read(matrix, &format!("{RUN}/{OUTCOME}"))? != outcome_bytes
        || read(&run_root, "started.json")? != started_bytes
        || digest(&read(&run_root, "echowall-diarization-worker")?) != NATIVE_WORKER_SHA
    {
        return Err("corpus_or_native_run_changed");
    }
    Ok(
        json!({"scope":REPLAY_SCOPE,"native_run":run_name,"replayed_cases":replayed,
        "original_successful_cases":42,"skipped_original_failed_cases":original_failed,
        "skipped_pending_native_cases":pending,"skipped_failed_native_cases":failed,
        "single_window_cases":single_window,"segments":segments,"unknown_segments":unknown,
        "artifacts_created":writes,"artifacts_matched":matched,
        "artifact_root":format!("local-eval/matrix/{run_relative}/app-mapping-replay")}),
    )
}

fn run() -> Result<Value, &'static str> {
    if std::env::var("ECHOWALL_MOSS_MAPPING_REPLAY_CONFIRM").as_deref()
        != Ok("public-retained-app-mapping-replay-authorized")
    {
        return Err("confirmation_required");
    }
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() > 1 {
        return Err("optional_native_run_name_required");
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("repository_missing")?;
    let matrix = checked_path(repo, "local-eval/matrix")?;
    match arguments.first() {
        Some(run) => fresh_anchor_replay(&matrix, run.to_str().ok_or("native_run_rejected")?),
        None => original_replay(&matrix),
    }
}

fn main() {
    match run() {
        Ok(report) => println!("{report}"),
        Err(code) => {
            eprintln!("{}", json!({"state":"failed","code":code}));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_native_run_name_cannot_escape_the_named_public_run_parent() {
        assert!(valid_name("run-fvnxhhlz", "run-"));
        for name in [
            "run-",
            "../run-x",
            "run-x/other",
            "run-x\\other",
            "/run-x",
            "run-x.json",
            "other",
        ] {
            assert!(!valid_name(name, "run-"), "{name}");
        }
        assert!(!valid_name(&format!("run-{}", "x".repeat(64)), "run-"));
    }

    #[test]
    fn replay_artifacts_are_create_new_or_exact_match_without_overwriting() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        assert!(write_artifact(&root, "canonical", "english_01", b"{\"segments\":[]}").unwrap());
        assert!(!write_artifact(&root, "canonical", "english_01", b"{\"segments\":[]}").unwrap());
        assert_eq!(
            write_artifact(&root, "canonical", "english_01", b"changed").unwrap_err(),
            "existing_output_mismatch"
        );
        assert_eq!(
            read(&root, "app-mapping-replay/canonical/english_01.json").unwrap(),
            b"{\"segments\":[]}"
        );
        assert!(write_artifact(&root, "../canonical", "english_01", b"value").is_err());
        assert!(write_artifact(&root, "canonical", "../escape", b"value").is_err());
        assert!(write_artifact(&root, "evidence", "english_01", b"").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replay_rejects_symlinked_directories_and_existing_artifact_targets() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("app-mapping-replay")).unwrap();
        assert!(write_artifact(&root, "canonical", "english_01", b"value").is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        fs::remove_file(root.join("app-mapping-replay")).unwrap();
        let directory = ensure_directory(&root, "app-mapping-replay/canonical").unwrap();
        fs::write(outside.join("original.json"), b"original").unwrap();
        symlink(
            outside.join("original.json"),
            directory.join("english_01.json"),
        )
        .unwrap();
        assert!(write_artifact(&root, "canonical", "english_01", b"original").is_err());
        assert_eq!(
            fs::read(outside.join("original.json")).unwrap(),
            b"original"
        );
    }
}
