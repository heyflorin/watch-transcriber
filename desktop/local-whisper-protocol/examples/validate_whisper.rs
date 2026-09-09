//! Validate a Whisper worker request/response pair without printing recording
//! content or segment timestamps.

use std::env;
use std::fs;

use echowall_local_whisper_protocol::{decode_request, decode_response};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let request_path = arguments.next().ok_or("request path is required")?;
    let response_path = arguments.next().ok_or("response path is required")?;
    if arguments.next().is_some() {
        return Err("exactly two paths are required".into());
    }
    let request = decode_request(&fs::read(request_path)?)?;
    let response = decode_response(&fs::read(response_path)?, &request)?;
    let transcript_bytes = response
        .segments
        .iter()
        .map(|segment| segment.text.len())
        .sum::<usize>();
    println!(
        "{{\"segments\":{},\"transcript_bytes\":{},\"language\":\"{}\"}}",
        response.segments.len(),
        transcript_bytes,
        response.language,
    );
    Ok(())
}
