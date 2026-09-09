//! Development-only, sample-exact public PCM windowing; no inference/playback.
use std::{fs::OpenOptions, path::Path};

pub const WINDOW_MS: u64 = 720_000;
pub const FIXED_POLICY: &str = "fixed12m-asr-experiment-v1";
pub const QUIET_POLICY: &str = "quiet12m-energy300-back5s-v1";

pub fn frame_spans(source: &Path, quiet: bool) -> Result<Vec<(u64, u64)>, &'static str> {
    let total = frames(source)?;
    let mut reader = hound::WavReader::open(source).map_err(|_| "window_source_invalid")?;
    if quiet {
        use echowall_local_moss_protocol::{windows::plan_quiet_windows, ProtocolError};
        return plan_quiet_windows(
            total,
            |start, output| {
                reader
                    .seek(u32::try_from(start).map_err(|_| ProtocolError("window_invalid"))?)
                    .map_err(|_| ProtocolError("window_source_invalid"))?;
                let mut values = reader.samples::<i16>();
                for sample in output {
                    *sample = values
                        .next()
                        .ok_or(ProtocolError("window_source_short"))?
                        .map_err(|_| ProtocolError("window_source_invalid"))?;
                }
                Ok(())
            },
            || false,
        )
        .map(|plan| {
            plan.windows
                .into_iter()
                .map(|w| (w.start_frame, w.end_frame))
                .collect()
        })
        .map_err(|error| error.0);
    }
    if total == 0 || total >= 18_000_000 * 16 {
        return Err("window_source_invalid");
    }
    let mut spans = Vec::new();
    let mut start = 0;
    while start < total {
        let end = (start + WINDOW_MS * 16).min(total);
        spans.push((start, end));
        start = end;
    }
    Ok(spans)
}

pub fn spans(source: &Path, quiet: bool) -> Result<Vec<(u64, u64)>, &'static str> {
    let duration = duration_ms(source)?;
    if duration == 0 || duration >= 18_000_000 {
        return Err("window_source_invalid");
    }
    let mut reader = hound::WavReader::open(source).map_err(|_| "window_source_invalid")?;
    let mut start = 0;
    let mut result = Vec::new();
    while start < duration {
        let target = (start + WINDOW_MS).min(duration);
        let end = if quiet && target < duration {
            quiet_cut(&mut reader, target, target - 5_000)?
        } else {
            target
        };
        if end <= start || end > target {
            return Err("window_invalid");
        }
        result.push((start, end));
        start = end;
    }
    Ok(result)
}

fn quiet_cut(
    reader: &mut hound::WavReader<std::io::BufReader<std::fs::File>>,
    target_ms: u64,
    earliest_ms: u64,
) -> Result<u64, &'static str> {
    const FRAMES: usize = 4_800;
    let read_from = earliest_ms.checked_sub(300).ok_or("window_invalid")?;
    let count = usize::try_from((target_ms - read_from) * 16).map_err(|_| "window_invalid")?;
    if !(FRAMES..=84_800).contains(&count) {
        return Err("window_invalid");
    }
    reader
        .seek(u32::try_from(read_from * 16).map_err(|_| "window_invalid")?)
        .map_err(|_| "window_source_invalid")?;
    let samples = reader
        .samples::<i16>()
        .take(count)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "window_source_invalid")?;
    if samples.len() != count {
        return Err("window_source_short");
    }
    let square = |v: i16| {
        let v = i64::from(v);
        (v * v) as u64
    };
    let mut sum: u64 = samples[..FRAMES].iter().map(|v| square(*v)).sum();
    let mut best = (sum, earliest_ms);
    for offset in 0..=samples.len() - FRAMES {
        if offset % 160 == 0 && sum <= best.0 {
            best = (sum, read_from + (offset + FRAMES) as u64 / 16);
        }
        if offset + FRAMES < samples.len() {
            sum = sum - square(samples[offset]) + square(samples[offset + FRAMES]);
        }
    }
    //300ms RMS <=~0.005 full scale, a fixed rule independent of transcript
    // and speaker truth. Otherwise retain the fixed cut; never fake silence.
    Ok(if best.0 <= 164 * 164 * FRAMES as u64 {
        best.1
    } else {
        target_ms
    })
}

pub fn duration_ms(source: &Path) -> Result<u64, &'static str> {
    Ok(frames(source)? / 16)
}

pub fn frames(source: &Path) -> Result<u64, &'static str> {
    let reader = hound::WavReader::open(source).map_err(|_| "window_source_invalid")?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != 16_000
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err("window_source_invalid");
    }
    Ok(u64::from(reader.duration()))
}

pub fn clip(
    source: &Path,
    destination: &Path,
    start_ms: u64,
    end_ms: u64,
) -> Result<(), &'static str> {
    if end_ms <= start_ms || end_ms > 18_000_000 {
        return Err("window_invalid");
    }
    clip_frames(source, destination, start_ms * 16, end_ms * 16)
}

pub fn clip_frames(
    source: &Path,
    destination: &Path,
    start: u64,
    end: u64,
) -> Result<(), &'static str> {
    if end <= start || end > 18_000_000 * 16 {
        return Err("window_invalid");
    }
    let mut reader = hound::WavReader::open(source).map_err(|_| "window_source_invalid")?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != 16_000
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err("window_source_invalid");
    }
    let count = end - start;
    if end > u64::from(reader.duration()) {
        return Err("window_source_short");
    }
    reader
        .seek(u32::try_from(start).map_err(|_| "window_invalid")?)
        .map_err(|_| "window_source_invalid")?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| "window_output_failed")?;
    let mut writer = hound::WavWriter::new(file, spec).map_err(|_| "window_output_failed")?;
    let mut written = 0;
    for sample in reader.samples::<i16>().take(count as usize) {
        writer
            .write_sample(sample.map_err(|_| "window_source_invalid")?)
            .map_err(|_| "window_output_failed")?;
        written += 1;
    }
    if written != count {
        return Err("window_source_short");
    }
    writer.finalize().map_err(|_| "window_output_failed")
}

#[test]
fn windows_are_sample_exact_and_cannot_overwrite_or_pad() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.wav");
    let output = directory.path().join("window.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&source, spec).unwrap();
    for value in 0_i16..64 {
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
    assert_eq!(duration_ms(&source).unwrap(), 4);
    clip(&source, &output, 1, 3).unwrap();
    let values: Vec<_> = hound::WavReader::open(&output)
        .unwrap()
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(values, (16_i16..48).collect::<Vec<_>>());
    assert!(clip(&source, &output, 1, 3).is_err());
    assert!(clip(&source, &directory.path().join("too-long.wav"), 0, 5).is_err());
    let fractional = directory.path().join("fractional.wav");
    clip_frames(&source, &fractional, 0, 63).unwrap();
    assert_eq!(frames(&fractional).unwrap(), 63);
    assert_eq!(frame_spans(&fractional, true).unwrap(), vec![(0, 63)]);
    assert_eq!(
        hound::WavReader::open(fractional)
            .unwrap()
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        (0..63_i16).collect::<Vec<_>>()
    );
}

#[test]
fn quiet_boundary_is_deterministic_and_does_not_claim_speech_is_silence() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&source, spec).unwrap();
    for index in 0..48_000 {
        writer
            .write_sample(if (16_000..22_400).contains(&index) {
                0_i16
            } else {
                1000_i16
            })
            .unwrap();
    }
    writer.finalize().unwrap();
    let mut reader = hound::WavReader::open(&source).unwrap();
    assert_eq!(quiet_cut(&mut reader, 2_000, 1_000).unwrap(), 1_400);
    assert_eq!(quiet_cut(&mut reader, 3_000, 2_000).unwrap(), 3_000);
    assert_eq!(spans(&source, true).unwrap(), vec![(0, 3000)]);
}
