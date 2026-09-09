use std::env;
use std::fs;

use echowall_local_qwen_protocol::{LocalQwenRequest, LocalQwenResponse};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let request_path = arguments.next().ok_or("request path is required")?;
    let response_path = arguments.next().ok_or("response path is required")?;
    if arguments.next().is_some() {
        return Err("exactly two paths are required".into());
    }
    let request: LocalQwenRequest = serde_json::from_slice(&fs::read(request_path)?)?;
    let response: LocalQwenResponse = serde_json::from_slice(&fs::read(response_path)?)?;
    response.validate_against(&request)?;
    println!("valid");
    Ok(())
}
