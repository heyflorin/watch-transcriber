//! Explicit, public-only one-file native inference with a durable checkpoint.
//! Nothing is played or uploaded. A macOS sandbox denies network operations.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use echowall_local_moss_protocol::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[path = "support/window.rs"]
mod window;

struct WorkerGuard(Child);
impl std::ops::Deref for WorkerGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for WorkerGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        // All early errors must stop inference before the temporary App tree
        // is cleaned up. A Child alone does not terminate itself on drop.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn bounded_file(root: &Path, relative: &str, limit: u64) -> Result<PathBuf, &'static str> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).map_err(|_| "fixture_missing")?;
    let resolved = fs::canonicalize(&path).map_err(|_| "fixture_missing")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > limit
        || !resolved.starts_with(root)
    {
        return Err("fixture_rejected");
    }
    Ok(resolved)
}

fn digest(path: &Path) -> Result<String, &'static str> {
    let mut file = File::open(path).map_err(|_| "fixture_read_failed")?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|_| "fixture_read_failed")?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn new_file(path: &Path) -> Result<File, &'static str> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "checkpoint_failed")
}

fn verified_effective_frames(
    eval: &Path,
    id: &str,
    historical_ms: u64,
) -> Result<u64, &'static str> {
    let audit_path = bounded_file(
        eval,
        "matrix/outputs/source-duration-audit-v1.json",
        512 * 1024,
    )?;
    let audit: Value =
        serde_json::from_slice(&fs::read(audit_path).map_err(|_| "audit_unavailable")?)
            .map_err(|_| "audit_invalid")?;
    let manifest_hash = digest(&bounded_file(eval, "matrix/manifest.json", 1024 * 1024)?)?;
    if audit["scope"] != "public_source_duration_metadata_audit"
        || audit["manifest_sha256"].as_str() != Some(&manifest_hash)
    {
        return Err("audit_identity_mismatch");
    }
    let rows = audit["cases"].as_array().ok_or("audit_invalid")?;
    let mut matches = rows.iter().filter(|row| row["case_id"] == id);
    let row = matches.next().ok_or("audit_case_missing")?;
    if matches.next().is_some()
        || row["historical_manifest_duration_ms"].as_u64() != Some(historical_ms)
        || row["aac_and_wav_effective_frames_equal"] != true
        || row["miaoji_import_sha256_match"] != true
    {
        return Err("audit_identity_mismatch");
    }
    for (kind, relative) in [
        ("aac", format!("matrix/audio/{id}.m4a")),
        ("wav", format!("matrix/audio-moss/{id}.wav")),
    ] {
        let file = bounded_file(eval, &relative, 512 * 1024 * 1024)?;
        if row[kind]["sha256"].as_str() != Some(digest(&file)?.as_str())
            || row[kind]["size_bytes"].as_u64()
                != Some(
                    fs::metadata(file)
                        .map_err(|_| "audit_source_missing")?
                        .len(),
                )
        {
            return Err("audit_source_changed");
        }
    }
    let frames = row["effective_source_frames"]
        .as_u64()
        .ok_or("audit_invalid")?;
    let wav = bounded_file(
        eval,
        &format!("matrix/audio-moss/{id}.wav"),
        512 * 1024 * 1024,
    )?;
    if frames == 0 || frames != window::frames(&wav)? || frames >= 18_000_000 * 16 {
        return Err("audit_pcm_mismatch");
    }
    Ok(frames)
}

fn persist(path: &Path, value: &Value) -> Result<(), &'static str> {
    let temporary = path.with_extension("pending");
    let mut file = new_file(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|_| "checkpoint_failed")?;
    file.write_all(b"\n").map_err(|_| "checkpoint_failed")?;
    file.sync_all().map_err(|_| "checkpoint_failed")?;
    fs::rename(temporary, path).map_err(|_| "checkpoint_failed")
}

fn infer(
    root: &Path,
    output: &Path,
    binary: &Path,
    request: &MossRequest,
) -> Result<Value, &'static str> {
    let stdout_path = output.join("response.json");
    let stderr_path = output.join("stderr.txt");
    let mut child = WorkerGuard(
        Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1) (allow default) (deny network*)"])
            .arg(binary)
            .env_clear()
            .env("ECHOWALL_APP_DATA_ROOT", root)
            .env("ECHOWALL_WORKER_PARENT_PID", std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(new_file(&stdout_path)?)
            .stderr(new_file(&stderr_path)?)
            .spawn()
            .map_err(|_| "worker_launch_failed")?,
    );
    let started = Instant::now();
    let input = encode_request(request).map_err(|_| "request_failed")?;
    if child
        .stdin
        .take()
        .ok_or("worker_input_failed")?
        .write_all(&input)
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err("worker_input_failed");
    }
    let deadline = Duration::from_secs(30 * 60);
    let status = loop {
        let stdout_size = fs::metadata(&stdout_path)
            .map_err(|_| "checkpoint_failed")?
            .len();
        let stderr_size = fs::metadata(&stderr_path)
            .map_err(|_| "checkpoint_failed")?
            .len();
        if stdout_size > MAX_RESPONSE_BYTES as u64
            || stderr_size > 64 * 1024
            || started.elapsed() > deadline
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("worker_deadline_or_output_limit");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("worker_wait_failed");
            }
        }
    };
    if !status.success() {
        return Err("worker_failed");
    }
    let bytes = fs::read(&stdout_path).map_err(|_| "response_missing")?;
    let response = decode_response(&bytes, request).map_err(|_| "response_rejected")?;
    let adapted = response.adapt(request).map_err(|_| "adaptation_rejected")?;
    let canonical = json!({"schema_version":1, "segments":adapted.segments.iter().map(|segment| json!({
        "start_ms": segment.start_ms, "end_ms":segment.end_ms,
        "speaker":segment.speaker_id.map_or_else(|| "local_unknown".into(), |id| format!("moss_speaker_{id}")),
        "text":segment.text,
    })).collect::<Vec<_>>()});
    persist(&output.join("canonical.json"), &canonical)?;
    Ok(
        json!({"state":"pass", "scope":"native_protocol_only", "segments":adapted.segments.len(),
        "unknown_segments":adapted.unknown_segments, "clipped_tail_ms":adapted.clipped_tail_ms,
        "elapsed_seconds":started.elapsed().as_secs_f64(),
        "parent_lifetime_policy":"bound-kernel-parent-v1",
        "network_policy":"sandbox-deny-network", "stderr_bytes":fs::metadata(stderr_path).map_err(|_| "checkpoint_failed")?.len()}),
    )
}

#[test]
#[ignore = "authorized public model/audio, Metal inference, isolated App root, no network or playback"]
fn public_native_worker_checkpoint() -> Result<(), &'static str> {
    if std::env::var("ECHOWALL_MOSS_NATIVE_CONFIRM").as_deref()
        != Ok("public-corpus-native-worker-authorized")
    {
        return Err("confirmation_required");
    }
    let id = std::env::var("ECHOWALL_MOSS_NATIVE_CASE").unwrap_or_else(|_| "english_01".into());
    let (stratum, suffix) = id.rsplit_once('_').ok_or("case_rejected")?;
    if !matches!(
        stratum,
        "english" | "mandarin" | "mixed" | "overlap" | "long_form"
    ) || suffix.len() != 2
        || !suffix.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("case_rejected");
    }
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("repository_missing")?
        .to_owned();
    let eval =
        fs::canonicalize(repository.join("local-eval")).map_err(|_| "fixture_root_missing")?;
    let manifest: Value = serde_json::from_slice(
        &fs::read(bounded_file(&eval, "matrix/manifest.json", 1024 * 1024)?)
            .map_err(|_| "fixture_read_failed")?,
    )
    .map_err(|_| "manifest_invalid")?;
    let case = manifest["cases"]
        .as_array()
        .ok_or("manifest_invalid")?
        .iter()
        .find(|case| case["case_id"] == id)
        .ok_or("case_rejected")?;
    let duration_ms = case["duration_ms"].as_u64().ok_or("case_rejected")?;
    let window_index = std::env::var("ECHOWALL_MOSS_NATIVE_WINDOW_INDEX")
        .ok()
        .map(|index| index.parse::<u64>().map_err(|_| "window_invalid"))
        .transpose()?;
    let quiet_windows = match std::env::var("ECHOWALL_MOSS_NATIVE_WINDOW_POLICY").as_deref() {
        Err(_) | Ok("fixed12m") => false,
        Ok("quiet12m") if window_index.is_some() => true,
        _ => return Err("window_policy_invalid"),
    };
    let source_format = match std::env::var("ECHOWALL_MOSS_NATIVE_SOURCE").as_deref() {
        Err(_) | Ok("wav") => "wav",
        Ok("m4a") if window_index.is_none() => "m4a",
        _ => return Err("source_format_rejected"),
    };
    let audio = bounded_file(
        &eval,
        &if source_format == "wav" {
            format!("matrix/audio-moss/{id}.wav")
        } else {
            format!("matrix/audio/{id}.m4a")
        },
        512 * 1024 * 1024,
    )?;
    // The frozen evaluation timeline includes old importer AAC padding. It is
    // not a physical decode duration. Use the separate hash-bound audit of
    // effective AAC/WAV frames, preserving the historical manifest unchanged.
    let source_frames = verified_effective_frames(&eval, &id, duration_ms)?;
    let pcm_duration_ms = source_frames.div_ceil(16);
    let (start_frame, end_frame) = if let Some(index) = window_index {
        // The same bounded policy must also work for short public cases whose
        // PCM extends just beyond12min. Case IDs remain manifest-allowlisted.
        let spans = window::frame_spans(&audio, quiet_windows)?;
        *spans
            .get(usize::try_from(index).map_err(|_| "window_invalid")?)
            .ok_or("window_invalid")?
    } else {
        (0, source_frames)
    };
    let start_ms = start_frame / 16;
    let end_ms = end_frame.div_ceil(16);
    let model = bounded_file(
        &eval,
        "models/moss-transcribe-diarize-q8/MOSS-Transcribe-Diarize-Q8_0.gguf",
        MODEL_SIZE_BYTES,
    )?;
    // A new owned temporary tree guarantees model/audio adoption does not
    // overwrite the real App ledger or change any retained evaluation input.
    let workspace = tempfile::Builder::new()
        .prefix("moss-native-app-")
        .tempdir_in(&eval)
        .map_err(|_| "workspace_failed")?;
    let root = workspace.path();
    let recording_id = "018f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        .parse()
        .map_err(|_| "id_invalid")?;
    let relative = format!("inbox/{recording_id}/tracks/imported.{source_format}");
    let adopted = root.join(&relative);
    fs::create_dir_all(adopted.parent().ok_or("workspace_failed")?)
        .map_err(|_| "workspace_failed")?;
    if window_index.is_some() {
        window::clip_frames(&audio, &adopted, start_frame, end_frame)?;
    } else {
        fs::copy(&audio, &adopted).map_err(|_| "adoption_failed")?;
    }
    let model_path = root.join(format!("models/moss/{MODEL_ID}/model.gguf"));
    fs::create_dir_all(model_path.parent().ok_or("workspace_failed")?)
        .map_err(|_| "workspace_failed")?;
    fs::copy(&model, model_path).map_err(|_| "adoption_failed")?;
    let request = MossRequest {
        schema_version: PROTOCOL_VERSION,
        recording_id,
        runtime_id: RUNTIME_ID.into(),
        model_id: MODEL_ID.into(),
        model_revision: MODEL_REVISION.into(),
        model_sha256: MODEL_SHA256.into(),
        model_size_bytes: MODEL_SIZE_BYTES,
        timing_policy: TIMING_POLICY.into(),
        audio_relative_path: relative,
        audio_sha256: digest(&adopted)?,
        audio_size_bytes: fs::metadata(&adopted).map_err(|_| "adoption_failed")?.len(),
        audio_duration_ms: end_ms - start_ms,
        language: None,
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock_failed")?
        .as_nanos();
    let parent = eval.join("matrix/outputs/moss-native-worker-v1");
    fs::create_dir_all(&parent).map_err(|_| "checkpoint_failed")?;
    let label = window_index.map_or_else(
        || {
            if source_format == "wav" {
                id.clone()
            } else {
                format!("{id}-m4a")
            }
        },
        |index| {
            if quiet_windows {
                format!("{id}-quiet12-window{index:02}")
            } else {
                format!("{id}-window{index:02}")
            }
        },
    );
    let output = parent.join(format!("{label}-{stamp}"));
    fs::create_dir(&output).map_err(|_| "checkpoint_failed")?;
    persist(
        &output.join("request.json"),
        &serde_json::to_value(&request).map_err(|_| "request_failed")?,
    )?;
    // Freeze the exact executable per run: a concurrent cargo rebuild must
    // not make a completed checkpoint claim the replacement binary's hash.
    let binary = root.join("echowall-moss-worker");
    fs::copy(env!("CARGO_BIN_EXE_echowall-moss-worker"), &binary)
        .map_err(|_| "worker_snapshot_failed")?;
    let worker_sha256 = digest(&binary)?;
    let result = infer(root, &output, &binary, &request);
    let mut report = result
        .clone()
        .unwrap_or_else(|code| json!({"state":"fail", "error":code}));
    report["case_id"] = json!(id);
    report["source_format"] = json!(source_format);
    report["source_timing_policy"] = json!("hash-bound-effective-frames-audit-v1");
    report["historical_manifest_duration_ms"] = json!(duration_ms);
    report["effective_source_frames"] = json!(source_frames);
    report["audio_duration_ms"] = json!(request.audio_duration_ms);
    if window_index.is_some() {
        report["window"] = json!({"policy":if quiet_windows {window::QUIET_POLICY} else {window::FIXED_POLICY}, "index":window_index,
            "source_start_ms":start_ms, "source_end_ms":end_ms, "source_duration_ms":duration_ms,
            "source_pcm_duration_ms":pcm_duration_ms, "source_start_frame":start_frame,
            "source_end_frame":end_frame, "pcm_timestamp_round_up_frames":end_ms * 16 - end_frame,
            "source_sha256":digest(&audio)?, "cross_window_speakers_validated":false});
    }
    report["model_sha256"] = json!(MODEL_SHA256);
    report["runtime_id"] = json!(RUNTIME_ID);
    report["raw_timing_policy"] = json!(echowall_moss_worker::RAW_TIMING_POLICY);
    report["worker_sha256"] = json!(worker_sha256);
    persist(&output.join("result.json"), &report)?;
    println!("{report}");
    result.map(|_| ())
}
