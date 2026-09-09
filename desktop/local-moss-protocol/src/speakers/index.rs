use std::{collections::BTreeMap, ops::Range};

use super::{
    add, MappingError, TimingSegment, Window, Work, MAX_ID_BYTES, MAX_SLOTS, MAX_TIMING_SEGMENTS,
    MAX_WINDOWS,
};

pub(super) fn validate_segments(
    segments: &[TimingSegment],
    duration: u64,
    work: &mut Work<'_>,
) -> Result<(), MappingError> {
    if segments.is_empty() || segments.len() > MAX_TIMING_SEGMENTS {
        return Err(MappingError::SegmentLimit);
    }
    let mut last_start = 0;
    let mut last_end = BTreeMap::new();
    for segment in segments {
        work.tick()?;
        if segment.start_ms < last_start
            || segment.end_ms <= segment.start_ms
            || segment.end_ms > duration
        {
            return Err(MappingError::InvalidTiming);
        }
        if let Some(id) = segment.speaker.as_deref() {
            if id.is_empty()
                || id.len() > MAX_ID_BYTES
                || id == "local_unknown"
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(MappingError::InvalidId);
            }
        }
        let id = segment.speaker.as_deref();
        if segment.start_ms < last_end.get(&id).copied().unwrap_or(0) {
            return Err(MappingError::SameSpeakerOverlap);
        }
        last_end.insert(id, segment.end_ms);
        last_start = segment.start_ms;
    }
    Ok(())
}

pub(super) fn window_ranges(
    segments: &[TimingSegment],
    windows: &[Window],
    duration: u64,
    work: &mut Work<'_>,
) -> Result<Vec<Range<usize>>, MappingError> {
    if windows.is_empty() || windows.len() > MAX_WINDOWS {
        return Err(MappingError::InvalidWindows);
    }
    let mut last_end = 0;
    let mut cursor = 0;
    let mut ranges = Vec::with_capacity(windows.len());
    for (index, window) in windows.iter().enumerate() {
        work.tick()?;
        if window.index != index
            || window.start_ms != last_end
            || window.end_ms <= window.start_ms
            || window.end_ms > duration
        {
            return Err(MappingError::InvalidWindows);
        }
        let start = cursor;
        while cursor < segments.len() && segments[cursor].start_ms < window.end_ms {
            work.tick()?;
            if segments[cursor].start_ms < window.start_ms
                || segments[cursor].end_ms > window.end_ms
            {
                return Err(MappingError::SegmentOutsideWindow);
            }
            cursor += 1;
        }
        ranges.push(start..cursor);
        last_end = window.end_ms;
    }
    if cursor != segments.len() {
        return Err(MappingError::SegmentOutsideWindow);
    }
    Ok(ranges)
}

#[derive(Default)]
struct Intervals {
    intervals: Vec<(u64, u64)>,
    /// prefix[i] is covered duration of intervals preceding interval i.
    prefix: Vec<u64>,
}

impl Intervals {
    fn before(&self, time: u64, work: &mut Work<'_>) -> Result<u64, MappingError> {
        let (mut lower, mut upper) = (0, self.intervals.len());
        while lower < upper {
            work.tick()?;
            let middle = lower + (upper - lower) / 2;
            if self.intervals[middle].1 <= time {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        let covered = self.prefix[lower];
        match self.intervals.get(lower) {
            Some(&(start, _)) if start < time => add(covered, time - start),
            _ => Ok(covered),
        }
    }
}

pub(super) struct AnchorIndex {
    slots: BTreeMap<String, Intervals>,
}

impl AnchorIndex {
    pub(super) fn new(
        anchors: &[TimingSegment],
        work: &mut Work<'_>,
    ) -> Result<Self, MappingError> {
        let mut slots = BTreeMap::<String, Intervals>::new();
        for segment in anchors {
            work.tick()?;
            if let Some(id) = &segment.speaker {
                let slot = slots.entry(id.clone()).or_default();
                slot.intervals.push((segment.start_ms, segment.end_ms));
                if slots.len() > MAX_SLOTS {
                    return Err(MappingError::SlotLimit);
                }
            }
        }
        for slot in slots.values_mut() {
            slot.prefix.push(0);
            for &(start, end) in &slot.intervals {
                work.tick()?;
                let next = add(
                    *slot.prefix.last().ok_or(MappingError::InternalInvariant)?,
                    end - start,
                )?;
                slot.prefix.push(next);
            }
        }
        Ok(Self { slots })
    }

    pub(super) fn slots(&self) -> Vec<&str> {
        self.slots.keys().map(String::as_str).collect()
    }

    pub(super) fn overlap(
        &self,
        id: &str,
        start: u64,
        end: u64,
        work: &mut Work<'_>,
    ) -> Result<u64, MappingError> {
        work.tick()?;
        let slot = self.slots.get(id).ok_or(MappingError::InternalInvariant)?;
        slot.before(end, work)?
            .checked_sub(slot.before(start, work)?)
            .ok_or(MappingError::ArithmeticOverflow)
    }
}
