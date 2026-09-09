//! Shared, platform-neutral recording ingest contracts.
//!
//! Native capture/import adapters terminate at this boundary. The inbox owns
//! only app working data and is deliberately disjoint from the Git-managed
//! archive; provider and archive orchestration remain outside this module.

pub mod envelope;
pub mod import;
pub mod inbox;
pub mod mobile;
pub mod state;
