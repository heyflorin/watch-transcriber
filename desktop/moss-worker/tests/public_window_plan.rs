//! Public PCM readback only. No inference, playback, network, or artifact writes.
use std::{fs::File, io::Read, path::Path};

use echowall_local_moss_protocol::{windows::plan_quiet_windows, ProtocolError};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[allow(dead_code)] // The independent oracle also exports policies for inference tests.
#[path = "support/window.rs"]
mod oracle;
#[allow(dead_code)]
#[path = "../../local-moss-protocol/tests/support/public_raw_files.rs"]
mod public_files;

fn hash(path: &Path) -> Result<String, &'static str> {
    let mut file = File::open(path).map_err(|_| "source_missing")?;
    let mut state = Sha256::new();
    let mut buffer = vec![0; 65_536];
    loop {
        let count = file.read(&mut buffer).map_err(|_| "source_read_failed")?;
        if count == 0 {
            break;
        }
        state.update(&buffer[..count]);
    }
    Ok(hex::encode(state.finalize()))
}

#[test]
#[ignore = "reads four existing public PCM files and retained window receipts; no inference"]
fn shared_streaming_planner_matches_all_retained_quiet_boundaries() -> Result<(), &'static str> {
    if std::env::var("ECHOWALL_MOSS_WINDOW_REPLAY_CONFIRM").as_deref()
        != Ok("public-window-plan-replay-authorized")
    {
        return Err("public_replay_confirmation_required");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../local-eval/matrix")
        .canonicalize()
        .map_err(|_| "fixture_missing")?;
    let mut total = 0;
    for index in 1..=4 {
        let provenance: Value = serde_json::from_slice(&public_files::read_bounded(
            &root,
            &format!("outputs/moss-quiet12-asr-long{index:02}-v1/provenance.json"),
            64 * 1024,
        )?)
        .map_err(|_| "receipt_invalid")?;
        let source =
            public_files::safe_path(&root, &format!("audio-moss/long_form_{index:02}.wav"))?;
        let metadata = source.metadata().map_err(|_| "source_missing")?;
        if !metadata.is_file() || metadata.len() > 512 * 1024 * 1024 {
            return Err("source_invalid");
        }
        let before = hash(&source)?;
        if provenance["source_sha256"].as_str() != Some(&before) {
            return Err("source_hash_mismatch");
        }
        let mut reader = hound::WavReader::open(&source).map_err(|_| "source_invalid")?;
        let spec = reader.spec();
        if spec.sample_rate != 16_000
            || spec.channels != 1
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
        {
            return Err("source_invalid");
        }
        let plan = plan_quiet_windows(
            u64::from(reader.duration()),
            |start, output| {
                reader
                    .seek(u32::try_from(start).map_err(|_| ProtocolError("read_range"))?)
                    .map_err(|_| ProtocolError("read_failed"))?;
                let mut values = reader.samples::<i16>();
                for sample in output {
                    *sample = values
                        .next()
                        .ok_or(ProtocolError("short_read"))?
                        .map_err(|_| ProtocolError("read_failed"))?;
                }
                Ok(())
            },
            || false,
        )
        .map_err(|error| error.0)?;
        if provenance["policy"] != plan.policy
            || plan.source_frames % 16 != 0
            || provenance["source_pcm_duration_ms"].as_u64() != Some(plan.source_frames / 16)
        {
            return Err("plan_identity_mismatch");
        }
        let spans: Vec<_> = plan
            .windows
            .iter()
            .map(|w| (w.start_frame / 16, w.end_frame / 16))
            .collect();
        if spans != oracle::spans(&source, true)? {
            return Err("oracle_boundary_mismatch");
        }
        let receipts = provenance["receipts"].as_array().ok_or("receipt_invalid")?;
        if receipts.len() != plan.windows.len() || receipts.len() != 8 {
            return Err("receipt_invalid");
        }
        for (receipt, window) in receipts.iter().zip(&plan.windows) {
            if receipt["index"].as_u64() != Some(window.index as u64)
                || receipt["start_ms"].as_u64() != Some(window.start_frame / 16)
                || receipt["end_ms"].as_u64() != Some(window.end_frame / 16)
            {
                return Err("receipt_boundary_mismatch");
            }
            total += 1;
        }
        if before != hash(&source)? {
            return Err("source_changed");
        }
    }
    println!(
        "{}",
        json!({"cases":4,"windows":total,"exact_retained_boundary_parity":true,
        "source_hashes_verified":true,"inference_run":false,"artifact_writes":0})
    );
    Ok(())
}
