//! Deterministic, bounded quiet-boundary planning over verified 16 kHz S16 mono.
//! No filesystem, transcript, speaker truth, runtime, or audio-output dependency.
use serde::Serialize;

use crate::ProtocolError;

pub const QUIET_WINDOW_POLICY: &str = "quiet12m-energy300-back5s-v1";
pub const SAMPLE_RATE: u64 = 16_000;
pub const MAX_WINDOW_FRAMES: u64 = 720 * SAMPLE_RATE;
pub const MAX_SOURCE_FRAMES: u64 = 5 * 60 * 60 * SAMPLE_RATE;
pub const MAX_WINDOWS: usize = 26;
pub const QUIET_LOOKBACK_FRAMES: u64 = 5 * SAMPLE_RATE;
pub const QUIET_CUT_GRID_FRAMES: usize = 160;
const ENERGY_FRAMES: usize = 4_800;
const READ_FRAMES: usize = QUIET_LOOKBACK_FRAMES as usize + ENERGY_FRAMES;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PcmWindow {
    pub index: usize,
    pub start_frame: u64,
    pub end_frame: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QuietWindowPlan {
    pub policy: &'static str,
    pub sample_rate: u64,
    pub source_frames: u64,
    pub windows: Vec<PcmWindow>,
}

/// Plan contiguous windows without loading or copying the full recording.
///
/// `read_samples` must fill the complete requested slice from a verified PCM
/// source or return an error; short reads must never become zero padding.
/// At most 84,800 samples are read per non-final boundary. Final tails retain
/// exact frame coordinates, including partial milliseconds. Container padding
/// must not be included in `source_frames`. This is the measured policy, not
/// proof of quality for arbitrary tiny tails or a selected App routing policy.
pub fn plan_quiet_windows(
    source_frames: u64,
    mut read_samples: impl FnMut(u64, &mut [i16]) -> Result<(), ProtocolError>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<QuietWindowPlan, ProtocolError> {
    if source_frames == 0 || source_frames >= MAX_SOURCE_FRAMES {
        return Err(ProtocolError("window_source_invalid"));
    }
    let mut samples = vec![0_i16; READ_FRAMES];
    let mut windows = Vec::new();
    let mut start = 0;
    while start < source_frames {
        check_cancelled(&mut is_cancelled)?;
        if windows.len() >= MAX_WINDOWS {
            return Err(ProtocolError("window_limit"));
        }
        let target = (start + MAX_WINDOW_FRAMES).min(source_frames);
        let end = if target < source_frames {
            let read_from = target - READ_FRAMES as u64;
            read_samples(read_from, &mut samples)?;
            check_cancelled(&mut is_cancelled)?;
            let offset = quiet_offset(&samples, &mut is_cancelled)?;
            read_from + offset as u64
        } else {
            target
        };
        if end <= start || end > target {
            return Err(ProtocolError("window_invalid"));
        }
        windows.push(PcmWindow {
            index: windows.len(),
            start_frame: start,
            end_frame: end,
        });
        start = end;
    }
    check_cancelled(&mut is_cancelled)?;
    Ok(QuietWindowPlan {
        policy: QUIET_WINDOW_POLICY,
        sample_rate: SAMPLE_RATE,
        source_frames,
        windows,
    })
}

fn check_cancelled(cancelled: &mut impl FnMut() -> bool) -> Result<(), ProtocolError> {
    if cancelled() {
        Err(ProtocolError("window_cancelled"))
    } else {
        Ok(())
    }
}

fn quiet_offset(
    samples: &[i16],
    cancelled: &mut impl FnMut() -> bool,
) -> Result<usize, ProtocolError> {
    if samples.len() != READ_FRAMES {
        return Err(ProtocolError("window_read_invalid"));
    }
    let square = |sample: i16| {
        let value = i64::from(sample);
        (value * value) as u64
    };
    let mut sum: u64 = samples[..ENERGY_FRAMES].iter().map(|v| square(*v)).sum();
    let mut best = (sum, ENERGY_FRAMES);
    for offset in 0..=samples.len() - ENERGY_FRAMES {
        if offset % 1_024 == 0 {
            check_cancelled(cancelled)?;
        }
        if offset % QUIET_CUT_GRID_FRAMES == 0 && sum <= best.0 {
            best = (sum, offset + ENERGY_FRAMES);
        }
        if offset + ENERGY_FRAMES < samples.len() {
            sum = sum - square(samples[offset]) + square(samples[offset + ENERGY_FRAMES]);
        }
    }
    // Fixed RMS <=164/32768. Latest tie wins. Above-threshold audio retains
    // the fixed cut; no speech is relabeled as silence and no frames are added.
    Ok(if best.0 <= 164 * 164 * ENERGY_FRAMES as u64 {
        best.1
    } else {
        samples.len()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_and_fractional_ms_tail_are_never_dropped_or_padded() {
        for frames in [1, 15, 16, MAX_WINDOW_FRAMES, MAX_WINDOW_FRAMES + 1] {
            let plan = plan_quiet_windows(
                frames,
                |_, out| {
                    out.fill(0);
                    Ok(())
                },
                || false,
            )
            .unwrap();
            assert_eq!(plan.windows.first().unwrap().start_frame, 0);
            assert_eq!(plan.windows.last().unwrap().end_frame, frames);
            assert_eq!(
                plan.windows
                    .iter()
                    .map(|w| w.end_frame - w.start_frame)
                    .sum::<u64>(),
                frames
            );
            assert!(plan
                .windows
                .windows(2)
                .all(|w| w[0].end_frame == w[1].start_frame));
            assert!(plan
                .windows
                .iter()
                .all(|w| w.end_frame - w.start_frame <= MAX_WINDOW_FRAMES));
        }
    }

    #[test]
    fn quiet_region_uses_latest_equal_energy_cut_and_loud_audio_falls_back() {
        let mut samples = vec![1_000_i16; READ_FRAMES];
        samples[16_000..22_400].fill(0);
        assert_eq!(quiet_offset(&samples, &mut || false).unwrap(), 22_400);
        samples.fill(0);
        assert_eq!(quiet_offset(&samples, &mut || false).unwrap(), READ_FRAMES);
        for value in [165, i16::MIN, i16::MAX] {
            samples.fill(value);
            assert_eq!(quiet_offset(&samples, &mut || false).unwrap(), READ_FRAMES);
        }
        // A quieter span that is still above threshold must not move the cut.
        samples.fill(1_000);
        samples[16_000..22_400].fill(165);
        assert_eq!(quiet_offset(&samples, &mut || false).unwrap(), READ_FRAMES);
        samples[16_000..22_400].fill(164);
        assert_eq!(quiet_offset(&samples, &mut || false).unwrap(), 22_400);
    }

    #[test]
    fn reads_and_cancellation_are_bounded_and_no_partial_plan_escapes() {
        let mut reads = 0;
        let plan = plan_quiet_windows(
            MAX_SOURCE_FRAMES - 1,
            |_, out| {
                reads += 1;
                assert_eq!(out.len(), READ_FRAMES);
                out.fill(0);
                Ok(())
            },
            || false,
        )
        .unwrap();
        assert!(plan.windows.len() <= MAX_WINDOWS);
        assert_eq!(reads, plan.windows.len() - 1);
        assert_eq!(
            plan_quiet_windows(0, |_, _| unreachable!(), || false)
                .unwrap_err()
                .0,
            "window_source_invalid"
        );
        assert_eq!(
            plan_quiet_windows(MAX_SOURCE_FRAMES, |_, _| unreachable!(), || false)
                .unwrap_err()
                .0,
            "window_source_invalid"
        );
        assert_eq!(
            plan_quiet_windows(1, |_, _| unreachable!(), || true)
                .unwrap_err()
                .0,
            "window_cancelled"
        );
        assert_eq!(
            plan_quiet_windows(
                MAX_WINDOW_FRAMES + 1,
                |_, _| Err(ProtocolError("short_read")),
                || false
            )
            .unwrap_err()
            .0,
            "short_read"
        );
        let mut checks = 0;
        assert_eq!(
            plan_quiet_windows(
                MAX_WINDOW_FRAMES + 1,
                |_, out| {
                    out.fill(0);
                    Ok(())
                },
                || {
                    checks += 1;
                    checks == 5
                }
            )
            .unwrap_err()
            .0,
            "window_cancelled"
        );
    }
}
