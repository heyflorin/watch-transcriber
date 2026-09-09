//! Pure anonymous speaker reconciliation, matching the retained JavaScript v3.
//!
//! Inputs contain timing and anonymous slots only. `None` denotes UNKNOWN;
//! `Some("local_unknown")` is rejected to avoid two encodings. Known IDs are
//! 1..=64 ASCII alphanumeric/underscore bytes, so Rust ordering exactly matches
//! JavaScript's lexicographic ordering on accepted IDs. IDs are not identities.
//!
//! Distinct local slots may share an existing global slot only when they never
//! overlap in a window. Positive acoustic support is required for a real slot.
//! UNKNOWN has zero support, sorts last, and cannot overlap a fixed UNKNOWN.
//! Each connected conflict component is solved exactly, with a fixed search
//! limit. No incomplete assignment is returned on failure or cancellation.
//!
//! Anchor prefix-duration indexes avoid pairwise transcript scans. Conflict
//! construction scans at most16 active slots per input interval. A deterministic
//! work budget bounds the entire call; cancellation is polled on entry, every
//! 1,024 work units, and before returning. There is no I/O or model inference.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

mod index;
mod solver;
#[cfg(test)]
mod tests;

pub const POLICY: &str = "moss-speakerkit-window-max-overlap-conflict-graph-v3";
pub const MAX_TIMING_SEGMENTS: usize = 50_000;
pub const MAX_WINDOWS: usize = 256;
pub const MAX_SLOTS: usize = 16;
pub const MAX_ID_BYTES: usize = 64;
pub const MAX_DURATION_MS: u64 = 5 * 60 * 60 * 1_000;
pub const SEARCH_LIMIT_PER_COMPONENT: u64 = 1_000_000;
pub const MAX_WORK_UNITS: u64 = 16_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimingSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Window {
    pub index: usize,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Millisecond counts describe acoustic support, never confidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SlotEvidence {
    pub local_speaker: String,
    pub assigned_global_speaker: Option<String>,
    pub support_ms: u64,
    pub total_overlap_ms: u64,
    pub local_duration_ms: u64,
    pub support_by_global: BTreeMap<String, u64>,
    pub unknown_allowed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ComponentEvidence {
    pub vertices: Vec<usize>,
    pub total_support_ms: u64,
    pub search_nodes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowEvidence {
    pub window_index: usize,
    pub local_slot_order: Vec<String>,
    pub global_slot_order: Vec<String>,
    pub conflict_edges: Vec<Vec<bool>>,
    pub unknown_allowed: Vec<bool>,
    pub columns: Vec<Option<usize>>,
    pub total_support_ms: u64,
    pub components: Vec<ComponentEvidence>,
    pub search_nodes: u64,
    pub support: Vec<SlotEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MappingResult {
    pub policy: &'static str,
    /// Exactly one result per original MOSS interval, in the original order.
    pub assignments: Vec<Option<String>>,
    pub windows: Vec<WindowEvidence>,
    pub input_unknown_segments: usize,
    pub output_unknown_segments: usize,
    pub new_unknown_segments: usize,
    pub work_units: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappingError {
    InvalidDuration,
    InvalidWindows,
    InvalidTiming,
    InvalidId,
    SegmentLimit,
    SlotLimit,
    SameSpeakerOverlap,
    SegmentOutsideWindow,
    ArithmeticOverflow,
    Infeasible,
    SearchLimit,
    WorkLimit,
    Cancelled,
    InternalInvariant,
}

impl MappingError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidDuration => "invalid_mapping_duration",
            Self::InvalidWindows => "invalid_mapping_windows",
            Self::InvalidTiming => "invalid_mapping_timing",
            Self::InvalidId => "invalid_anonymous_slot",
            Self::SegmentLimit => "mapping_segment_limit",
            Self::SlotLimit => "mapping_slot_limit",
            Self::SameSpeakerOverlap => "same_speaker_overlap",
            Self::SegmentOutsideWindow => "segment_outside_window",
            Self::ArithmeticOverflow => "mapping_arithmetic_overflow",
            Self::Infeasible => "assignment_infeasible",
            Self::SearchLimit => "assignment_search_limit",
            Self::WorkLimit => "mapping_work_limit",
            Self::Cancelled => "mapping_cancelled",
            Self::InternalInvariant => "mapping_invariant_failed",
        }
    }
}

impl std::fmt::Display for MappingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for MappingError {}

struct Work<'a> {
    used: u64,
    remaining: u64,
    cancelled: &'a mut dyn FnMut() -> bool,
}

impl<'a> Work<'a> {
    fn new(cancelled: &'a mut dyn FnMut() -> bool) -> Result<Self, MappingError> {
        let mut work = Self {
            used: 0,
            remaining: MAX_WORK_UNITS,
            cancelled,
        };
        work.poll()?;
        Ok(work)
    }

    fn poll(&mut self) -> Result<(), MappingError> {
        if (self.cancelled)() {
            Err(MappingError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn tick(&mut self) -> Result<(), MappingError> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(MappingError::WorkLimit)?;
        self.used = self
            .used
            .checked_add(1)
            .ok_or(MappingError::ArithmeticOverflow)?;
        if self.used.is_multiple_of(1024) {
            self.poll()?;
        }
        Ok(())
    }
}

/// The explicit container duration may exceed the last PCM window's end.
/// Every MOSS interval must still fit entirely inside one contiguous window.
pub fn reconcile(
    moss: &[TimingSegment],
    anchors: &[TimingSegment],
    windows: &[Window],
    duration_ms: u64,
) -> Result<MappingResult, MappingError> {
    reconcile_with_cancel(moss, anchors, windows, duration_ms, || false)
}

/// The callback must be cheap and nonblocking (for example, an atomic flag).
/// Cancellation changes only whether the call returns an error, never a label.
pub fn reconcile_with_cancel(
    moss: &[TimingSegment],
    anchors: &[TimingSegment],
    windows: &[Window],
    duration_ms: u64,
    mut cancelled: impl FnMut() -> bool,
) -> Result<MappingResult, MappingError> {
    let mut work = Work::new(&mut cancelled)?;
    if duration_ms == 0 || duration_ms > MAX_DURATION_MS {
        return Err(MappingError::InvalidDuration);
    }
    index::validate_segments(moss, duration_ms, &mut work)?;
    index::validate_segments(anchors, duration_ms, &mut work)?;
    let ranges = index::window_ranges(moss, windows, duration_ms, &mut work)?;
    let anchors = index::AnchorIndex::new(anchors, &mut work)?;
    let mut result = MappingResult {
        policy: POLICY,
        assignments: Vec::with_capacity(moss.len()),
        windows: Vec::with_capacity(windows.len()),
        input_unknown_segments: 0,
        output_unknown_segments: 0,
        new_unknown_segments: 0,
        work_units: 0,
    };
    let mut last_end = BTreeMap::<Option<String>, u64>::new();
    for (window, range) in windows.iter().zip(ranges) {
        let segments = &moss[range];
        let mut slots = BTreeSet::new();
        for segment in segments {
            work.tick()?;
            if let Some(slot) = &segment.speaker {
                slots.insert(slot.as_str());
                if slots.len() > MAX_SLOTS {
                    return Err(MappingError::SlotLimit);
                }
            }
        }
        let local: Vec<&str> = slots.into_iter().collect();
        let globals = if local.is_empty() {
            Vec::new()
        } else {
            anchors.slots()
        };
        let slot_indices: BTreeMap<&str, usize> = local
            .iter()
            .enumerate()
            .map(|(index, slot)| (*slot, index))
            .collect();
        let mut weights = vec![vec![0_u64; globals.len()]; local.len()];
        let mut edges = vec![vec![false; local.len()]; local.len()];
        let mut unknown_allowed = vec![true; local.len()];
        let mut durations = vec![0_u64; local.len()];
        let mut active_ends = vec![0; local.len()];
        let mut unknown_end = 0;
        for segment in segments {
            work.tick()?;
            let current = segment.speaker.as_deref().map(|slot| slot_indices[slot]);
            for (previous, &end) in active_ends.iter().enumerate() {
                work.tick()?;
                if end > segment.start_ms {
                    if let Some(current) = current {
                        if current != previous {
                            edges[current][previous] = true;
                            edges[previous][current] = true;
                        }
                    } else {
                        unknown_allowed[previous] = false;
                    }
                }
            }
            if let Some(current) = current {
                if unknown_end > segment.start_ms {
                    unknown_allowed[current] = false;
                }
                active_ends[current] = segment.end_ms;
                durations[current] = add(durations[current], segment.end_ms - segment.start_ms)?;
                for (global, id) in globals.iter().enumerate() {
                    let support =
                        anchors.overlap(id, segment.start_ms, segment.end_ms, &mut work)?;
                    weights[current][global] = add(weights[current][global], support)?;
                }
            } else {
                unknown_end = segment.end_ms;
            }
        }
        let solved = solver::solve(&weights, &edges, &unknown_allowed, &mut work)?;
        let mut support = Vec::with_capacity(local.len());
        for (index, slot) in local.iter().enumerate() {
            work.tick()?;
            support.push(SlotEvidence {
                local_speaker: (*slot).into(),
                assigned_global_speaker: solved.columns[index]
                    .map(|column| globals[column].to_owned()),
                support_ms: solved.columns[index].map_or(0, |column| weights[index][column]),
                total_overlap_ms: weights[index]
                    .iter()
                    .try_fold(0, |sum, value| add(sum, *value))?,
                local_duration_ms: durations[index],
                support_by_global: globals
                    .iter()
                    .zip(&weights[index])
                    .map(|(slot, value)| ((*slot).to_owned(), *value))
                    .collect(),
                unknown_allowed: unknown_allowed[index],
            });
        }
        for segment in segments {
            work.tick()?;
            let assignment = segment
                .speaker
                .as_deref()
                .and_then(|slot| support[slot_indices[slot]].assigned_global_speaker.clone());
            if segment.start_ms < last_end.get(&assignment).copied().unwrap_or(0) {
                return Err(MappingError::InternalInvariant);
            }
            last_end.insert(assignment.clone(), segment.end_ms);
            result.input_unknown_segments += usize::from(segment.speaker.is_none());
            result.output_unknown_segments += usize::from(assignment.is_none());
            result.new_unknown_segments +=
                usize::from(segment.speaker.is_some() && assignment.is_none());
            result.assignments.push(assignment);
        }
        result.windows.push(WindowEvidence {
            window_index: window.index,
            local_slot_order: local.into_iter().map(str::to_owned).collect(),
            global_slot_order: globals.into_iter().map(str::to_owned).collect(),
            conflict_edges: edges,
            unknown_allowed,
            columns: solved.columns,
            total_support_ms: solved.total_support_ms,
            components: solved.components,
            search_nodes: solved.search_nodes,
            support,
        });
    }
    work.poll()?;
    result.work_units = work.used;
    Ok(result)
}

fn add(left: u64, right: u64) -> Result<u64, MappingError> {
    left.checked_add(right)
        .ok_or(MappingError::ArithmeticOverflow)
}
