//! Re-export the framework-free protocol shared with the App-bundled one-shot
//! local Whisper worker. Keeping the implementation in its own crate prevents
//! the worker from acquiring Tauri, network, provider, or persistence code.

pub use echowall_local_whisper_protocol::*;
