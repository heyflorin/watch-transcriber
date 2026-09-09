//! Validate a diarization worker request/response pair without printing any
//! recording content or interval timestamps.

use std::env;
use std::fs;

use echowall_local_whisper_protocol::{decode_diarization_request, decode_diarization_response};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let request_path = arguments.next().ok_or("request path is required")?;
    let response_path = arguments.next().ok_or("response path is required")?;
    if arguments.next().is_some() {
        return Err("exactly two paths are required".into());
    }
    let request = decode_diarization_request(&fs::read(request_path)?)?;
    let response = decode_diarization_response(&fs::read(response_path)?, &request)?;
    println!(
        "{{\"model_files\":{},\"speaker_count\":{},\"segments\":{}}}",
        request.model_files.len(),
        response.speaker_count,
        response.segments.len(),
    );
    Ok(())
}
