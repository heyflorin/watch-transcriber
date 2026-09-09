//! Bounded, streaming conversion to the model's 16 kHz mono input.

use rubato::{FftFixedInOut, Resampler};

const TARGET: usize = 16_000;

pub struct MonoConverter {
    rate: usize,
    source_frames: usize,
    max_source_frames: usize,
    engine: Option<FftFixedInOut<f32>>,
    pending: Vec<f32>,
    scratch: Vec<Vec<f32>>,
    delay_left: usize,
    output: Vec<f32>,
}

impl MonoConverter {
    pub fn new(source_rate: u32, duration_ms: u64) -> Result<Self, &'static str> {
        if !(8_000..=192_000).contains(&source_rate)
            || duration_ms == 0
            || duration_ms >= 18_000_000
        {
            return Err("audio_format_unsupported");
        }
        let rate = source_rate as usize;
        let max_source_frames =
            usize::try_from((duration_ms + 100) * u64::from(source_rate) / 1_000)
                .map_err(|_| "decode_limit")?;
        let engine = if rate == TARGET {
            None
        } else {
            Some(FftFixedInOut::<f32>::new(rate, TARGET, 1024, 1).map_err(|_| "resampler_failed")?)
        };
        let scratch = engine
            .as_ref()
            .map_or_else(Vec::new, |e| e.output_buffer_allocate(true));
        let delay_left = engine.as_ref().map_or(0, |e| e.output_delay());
        let pending = Vec::with_capacity(engine.as_ref().map_or(0, |e| e.input_frames_next()));
        Ok(Self {
            rate,
            source_frames: 0,
            max_source_frames,
            engine,
            pending,
            scratch,
            delay_left,
            output: Vec::new(),
        })
    }

    pub fn push(&mut self, mut samples: &[f32]) -> Result<(), &'static str> {
        if samples
            .iter()
            .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
        {
            return Err("invalid_samples");
        }
        let total = self
            .source_frames
            .checked_add(samples.len())
            .ok_or("decode_limit")?;
        if total > self.max_source_frames {
            return Err("decode_limit");
        }
        self.source_frames = total;
        let Some(engine) = &self.engine else {
            self.output
                .try_reserve(samples.len())
                .map_err(|_| "decode_limit")?;
            self.output.extend_from_slice(samples);
            return Ok(());
        };
        let needed = engine.input_frames_next();
        while !samples.is_empty() {
            let take = samples.len().min(needed - self.pending.len());
            self.pending.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.pending.len() == needed {
                self.process_chunk()?;
            }
        }
        Ok(())
    }

    fn process_chunk(&mut self) -> Result<(), &'static str> {
        let engine = self.engine.as_mut().ok_or("resampler_failed")?;
        let (_, written) = engine
            .process_into_buffer(&[&self.pending], &mut self.scratch, None)
            .map_err(|_| "resampler_failed")?;
        self.pending.clear();
        let skip = self.delay_left.min(written);
        self.delay_left -= skip;
        let wanted = self.source_frames * TARGET / self.rate;
        let take = (written - skip).min(wanted.saturating_sub(self.output.len()));
        self.output.try_reserve(take).map_err(|_| "decode_limit")?;
        for value in &self.scratch[0][skip..skip + take] {
            if !value.is_finite() {
                return Err("invalid_samples");
            }
            // Band-limited interpolation can overshoot full scale. Match the
            // model's normalized PCM contract after filtering, not before it.
            self.output.push(value.clamp(-1.0, 1.0));
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<f32>, &'static str> {
        let wanted = self.source_frames * TARGET / self.rate;
        if let Some(engine) = &self.engine {
            let needed = engine.input_frames_next();
            if !self.pending.is_empty() {
                self.pending.resize(needed, 0.0);
                self.process_chunk()?;
            }
            // Fixed FFT delay is at most one output chunk; a fixed flush cap
            // prevents an unexpected library result becoming an endless loop.
            for _ in 0..3 {
                if self.output.len() >= wanted {
                    break;
                }
                self.pending.resize(needed, 0.0);
                self.process_chunk()?;
            }
        }
        if self.output.len() != wanted || wanted == 0 {
            return Err("resampler_incomplete");
        }
        Ok(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(samples: &[f32], rate: u32, chunk: usize) -> Vec<f32> {
        let duration_ms = samples.len() as u64 * 1_000 / u64::from(rate);
        let mut converter = MonoConverter::new(rate, duration_ms).unwrap();
        for data in samples.chunks(chunk) {
            converter.push(data).unwrap();
        }
        converter.finish().unwrap()
    }

    #[test]
    fn native_rate_is_bit_identical_and_fractional_rate_has_exact_length() {
        let samples = vec![0.25; 44_107];
        assert_eq!(convert(&samples, 16_000, 333), samples);
        assert_eq!(
            convert(&samples, 44_100, 333).len(),
            44_107 * 16_000 / 44_100
        );
    }

    #[test]
    fn chunk_boundaries_do_not_change_samples_and_filter_delay_is_removed() {
        let mut input = vec![0.0; 48_001];
        input[9_600] = 1.0;
        let first = convert(&input, 48_000, 73);
        assert_eq!(first, convert(&input, 48_000, 9_997));
        let maximum = first
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
            .unwrap()
            .0;
        assert!(
            maximum.abs_diff(3_200) <= 1,
            "filter delay changed the transcript time origin"
        );
        assert_eq!(first.len(), input.len() * 16_000 / 48_000);
    }

    #[test]
    fn downsampling_keeps_voice_band_and_rejects_above_nyquist_energy() {
        let rms = |frequency: f32| {
            let input: Vec<_> = (0..96_000)
                .map(|i| {
                    (std::f64::consts::TAU * f64::from(frequency) * i as f64 / 48_000.0).sin()
                        as f32
                })
                .collect();
            let result = convert(&input, 48_000, 317);
            let body = &result[500..result.len() - 500];
            (body.iter().map(|v| v * v).sum::<f32>() / body.len() as f32).sqrt()
        };
        assert!((rms(1_000.0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
        assert!(
            rms(12_000.0) < 0.001,
            "high frequencies aliased into the speech band"
        );
    }

    #[test]
    fn rates_samples_and_duration_are_bounded_before_growth() {
        assert!(MonoConverter::new(0, 100).is_err());
        assert!(MonoConverter::new(384_000, 100).is_err());
        let mut converter = MonoConverter::new(48_000, 100).unwrap();
        assert_eq!(converter.push(&[f32::NAN]), Err("invalid_samples"));
        assert_eq!(converter.push(&vec![0.0; 48_000]), Err("decode_limit"));
    }
}
