//! Stateless one-shot MOSS inference. Receives one App-owned job, verifies
//! local inputs, returns a bounded joint response, and exits. No queue or API.

pub use echowall_local_moss_protocol::RAW_TIMING_POLICY;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod files;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "../../local-worker-support/parent_guard.rs"]
mod parent_guard;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod scratch;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn run_once() -> Result<(), &'static str> {
    use echowall_local_moss_protocol::*;
    use std::{
        io::{self, Read, Write},
        path::PathBuf,
    };
    use transcribe_cpp::{Backend, Diarize, Model, ModelOptions, RunOptions, TimestampKind};

    if std::env::args_os().len() != 1 {
        return Err("arguments_forbidden");
    }
    let _parent_guard = parent_guard::bind_from_environment()?;
    let root = PathBuf::from(std::env::var_os("ECHOWALL_APP_DATA_ROOT").ok_or("root_missing")?);
    let root = files::validate_root(&root)?;
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input_failed")?;
    let request = decode_request(&bytes).map_err(|_| "invalid_request")?;
    let mut model_file = files::VerifiedFile::open(
        &root,
        &format!("models/moss/{MODEL_ID}/model.gguf"),
        MODEL_SIZE_BYTES,
        MODEL_SHA256,
    )?;
    let mut audio_file = files::VerifiedFile::open(
        &root,
        &request.audio_relative_path,
        request.audio_size_bytes,
        &request.audio_sha256,
    )?;
    let samples = echowall_local_audio::decode_source(
        audio_file.reader()?,
        &request.audio_relative_path,
        request.audio_duration_ms,
    )?;
    audio_file.revalidate()?;
    let mut model_snapshot = model_file.snapshot(&root)?;

    transcribe_cpp::disable_logging();
    let model = Model::load_with(
        model_snapshot.path(),
        &ModelOptions {
            backend: Backend::Metal,
            device: None,
        },
    )
    .map_err(|error| match error {
        transcribe_cpp::Error::ModelFileNotFound(_) => "model_descriptor_unavailable",
        transcribe_cpp::Error::ModelLoad(_) => "model_format_rejected",
        transcribe_cpp::Error::Backend(_) => "model_backend_unavailable",
        transcribe_cpp::Error::OutOfMemory(_) => "model_memory_exhausted",
        _ => "model_load_failed",
    })?;
    // backend() is a display name (e.g. Metal/MTL0); kind is the classified
    // backend contract. Do not mistake capitalization for a CPU fallback.
    if model.device().map_err(|_| "backend_unavailable")?.kind != "metal" {
        return Err("backend_mismatch");
    }
    model_file.revalidate()?;
    model_snapshot.revalidate()?;
    let mut session = model.session().map_err(|_| "session_failed")?;
    let limits = session.limits().map_err(|_| "session_failed")?;
    if limits.effective_max_audio_ms > 0
        && request.audio_duration_ms > limits.effective_max_audio_ms as u64
    {
        return Err("audio_duration_unsupported");
    }
    let transcript = session
        .run(
            &samples,
            &RunOptions {
                diarize: Diarize::On,
                timestamps: TimestampKind::Segment,
                language: request.language.clone(),
                ..RunOptions::default()
            },
        )
        .map_err(|_| "inference_incomplete")?;
    if session.was_aborted() || session.was_truncated() {
        return Err("inference_incomplete");
    }
    // The native parser repairs unknown times, including stretching the last
    // turn to the file's end. Use only boundaries explicitly emitted by the
    // model; EOS alone does not authorize inventing timestamp coverage.
    let segments = parse_raw_segments(&transcript.raw_text)?;
    let response = MossResponse {
        schema_version: PROTOCOL_VERSION,
        recording_id: request.recording_id,
        runtime_id: request.runtime_id.clone(),
        model_id: request.model_id.clone(),
        model_sha256: request.model_sha256.clone(),
        audio_sha256: request.audio_sha256.clone(),
        timing_policy: request.timing_policy.clone(),
        complete: true,
        segments,
    };
    model_file.revalidate()?;
    model_snapshot.revalidate()?;
    audio_file.revalidate()?;
    let output = encode_response(&response, &request).map_err(|error| error.0)?;
    // Keep the OS snapshot lease until every native user is truly destroyed.
    // Error unwinding follows this same reverse declaration order.
    drop(session);
    drop(model);
    drop(model_snapshot);
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&output)
        .and_then(|()| stdout.flush())
        .map_err(|_| "output_failed")
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub fn run_once() -> Result<(), &'static str> {
    Err("unsupported_platform")
}
