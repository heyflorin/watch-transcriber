//! Windows 11 WASAPI capture backend.
//!
//! Packet/timeline handling is platform-neutral so it can be exercised without
//! opening an audio device. The actual COM/WASAPI adapter is compiled only on
//! Windows and never substitutes a silent stub for an unavailable API.

mod packet;

pub use packet::{
    ClockMapper, PacketAnalysis, PacketFlags, PacketIssue, PacketTimeline, WasapiPacket,
};

pub const MICROPHONE_PRIVACY_SETTINGS_URI: &str = "ms-settings:privacy-microphone";
pub const PROTECTED_AUDIO_SUPPORTED: bool = false;
pub const PROCESS_LOOPBACK_MINIMUM_BUILD: u32 = 20_348;

#[cfg(target_os = "windows")]
mod native;

#[cfg(target_os = "windows")]
pub use native::{WindowsCaptureBackend, WindowsCaptureSource};

#[cfg(test)]
mod tests;
