//! Explicit, lossless cross-speaker emission-order correction. The response
//! retains its original segment vector; only this index permutation is sorted.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    MossRequest, MossSegment, ProtocolError, MAX_SPEAKERS, MAX_TAIL_OVERRUN_MS, MAX_TEXT_BYTES,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChronologicalProvenance {
    /// Original emission indices in stable chronological order, before union.
    pub source_index_permutation: Vec<usize>,
    /// Exactly one group per adapted segment. Flattening these groups yields
    /// the permutation exactly, accounting for every coalesced source turn.
    pub output_source_indices: Vec<Vec<usize>>,
}

pub(super) fn source_index_permutation(
    segments: &[MossSegment],
    request: &MossRequest,
) -> Result<Vec<usize>, ProtocolError> {
    let mut text_bytes = 0_usize;
    // Validate original fields and original-final tail eligibility BEFORE
    // changing order. Sorting cannot promote another interval into eligibility.
    for (index, segment) in segments.iter().enumerate() {
        let start = u64::try_from(segment.start_ms).map_err(|_| ProtocolError("invalid_timing"))?;
        let end = u64::try_from(segment.end_ms).map_err(|_| ProtocolError("invalid_timing"))?;
        if end <= start
            || start >= request.audio_duration_ms
            || (end > request.audio_duration_ms
                && (index + 1 != segments.len()
                    || end - request.audio_duration_ms > MAX_TAIL_OVERRUN_MS))
        {
            return Err(ProtocolError("invalid_timing"));
        }
        if segment.speaker_id > MAX_SPEAKERS
            || segment.text.trim().is_empty()
            || segment.text.contains('\0')
            || segment.text.len() > 64 * 1024
        {
            return Err(ProtocolError("invalid_segment"));
        }
        text_bytes = text_bytes
            .checked_add(segment.text.len())
            .ok_or(ProtocolError("response_too_large"))?;
        if text_bytes > MAX_TEXT_BYTES {
            return Err(ProtocolError("response_too_large"));
        }
    }
    let mut indices: Vec<_> = (0..segments.len()).collect();
    indices.sort_by_key(|&index| (segments[index].start_ms, index));
    let mut previous = BTreeMap::<u32, usize>::new();
    for &index in &indices {
        // Zero is included: ordering unknown turns is not permission to
        // reinterpret the source's unattributed-speaker subsequence.
        if previous
            .insert(segments[index].speaker_id, index)
            .is_some_and(|last| index < last)
        {
            return Err(ProtocolError("invalid_timing"));
        }
    }
    Ok(indices)
}
