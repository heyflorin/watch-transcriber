//! Validate and merge one Whisper plus diarization response without printing
//! recording content, timestamps, or interval assignments.

use std::env;
use std::fs;

use echowall_local_whisper_protocol::{
    decode_diarization_request, decode_diarization_response, decode_request, decode_response,
    merge_diarization,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let whisper_request_path = arguments.next().ok_or("Whisper request path is required")?;
    let whisper_response_path = arguments
        .next()
        .ok_or("Whisper response path is required")?;
    let diarization_request_path = arguments
        .next()
        .ok_or("diarization request path is required")?;
    let diarization_response_path = arguments
        .next()
        .ok_or("diarization response path is required")?;
    if arguments.next().is_some() {
        return Err("exactly four paths are required".into());
    }

    let whisper_request = decode_request(&fs::read(whisper_request_path)?)?;
    let mut whisper = decode_response(&fs::read(whisper_response_path)?, &whisper_request)?;
    let diarization_request = decode_diarization_request(&fs::read(diarization_request_path)?)?;
    let diarization =
        decode_diarization_response(&fs::read(diarization_response_path)?, &diarization_request)?;
    let stats = merge_diarization(
        &mut whisper,
        &whisper_request,
        &diarization,
        &diarization_request,
    )?;
    let total = stats
        .assigned_segments
        .saturating_add(stats.unknown_segments);
    let coverage_milli = stats
        .assigned_segments
        .saturating_mul(1_000)
        .checked_div(total)
        .unwrap_or(0);
    println!(
        "{{\"segments\":{},\"assigned\":{},\"unknown\":{},\"coverage_milli\":{}}}",
        total, stats.assigned_segments, stats.unknown_segments, coverage_milli,
    );
    Ok(())
}
