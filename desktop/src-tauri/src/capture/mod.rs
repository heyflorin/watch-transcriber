//! Platform-neutral desktop capture lifecycle and native adapter boundaries.
//!
//! This module never opens an audio device by itself. A session can start only
//! after an explicitly injected native backend validates consent and preflight.

pub mod model;
pub mod session;

#[cfg(not(mobile))]
pub mod commands;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(any(target_os = "windows", test))]
pub mod windows;

pub use model::*;
pub use session::*;

#[cfg(test)]
mod tests;
