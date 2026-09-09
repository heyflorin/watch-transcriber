//! Bounded file decoding, mono downmix, and band-limited conversion. Native
//! 16 kHz mono PCM remains a bit-identical path for the public MOSS matrix.

mod mp4_timing;
mod resampling;

pub use mp4_timing::{ContainerTimeline, DecodedFrameTimeline, TimelineSummary};

use std::{fs::File, path::Path};

use symphonia::core::{
    codecs::audio::AudioDecoderOptions,
    formats::{probe::Hint, FormatOptions, TrackType},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

const SAMPLE_RATE: u32 = 16_000;
const MAX_PACKET_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHANNELS: usize = 8;
const MAX_PACKETS: usize = 1_000_000;

/// Explicit PCM publication policy used by App-owned MOSS preparation.
pub const PCM_QUANTIZATION_POLICY: &str = "pcm16-round-nearest-clamp-fullscale-v1";

/// Decode a caller-verified file to bounded 16 kHz mono samples. The caller
/// owns file identity/hash verification before and after decoding. No path is
/// opened, no audio is played, and the supplied name is only a format hint.
pub fn decode_source(file: File, name: &str, duration_ms: u64) -> Result<Vec<f32>, &'static str> {
    decode_source_with_cancel(file, name, duration_ms, || false)
}

/// Same exact decoding policy, with cancellation checked between bounded
/// packets and before final resampler flush. Cancellation never returns PCM.
pub fn decode_source_with_cancel(
    mut file: File,
    name: &str,
    duration_ms: u64,
    mut cancelled: impl FnMut() -> bool,
) -> Result<Vec<f32>, &'static str> {
    if cancelled() {
        return Err("decode_cancelled");
    }
    let container_timeline = ContainerTimeline::read(&mut file, name)?;
    let mut hint = Hint::new();
    let extension = Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .ok_or("audio_format_unsupported")?;
    hint.with_extension(extension);
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|_| "decode_failed")?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("decode_failed")?;
    let parameters = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("decode_failed")?;
    let mut decoder = symphonia::default::get_codecs()
        // AAC 0.6.1 ignores the gapless option, while MP3 honors it. Disable
        // decoder-side trimming and apply container trim explicitly once.
        .make_audio_decoder(parameters, &AudioDecoderOptions::default().gapless(false))
        .map_err(|_| "decode_failed")?;
    let track_id = track.id;
    // Container durations are rounded; allow at most 100 ms, not a percentage
    // that could conceal minutes missing from a long recording.
    let mut converter = None;
    let mut expected_format = None;
    let mut timeline = None;
    let mut interleaved = Vec::<f32>::new();
    let mut mono = Vec::<f32>::new();
    let mut packets = 0_usize;
    loop {
        if cancelled() {
            return Err("decode_cancelled");
        }
        let Some(packet) = format.next_packet().map_err(|_| "decode_failed")? else {
            break;
        };
        packets += 1;
        if packets > MAX_PACKETS || packet.data.len() > MAX_PACKET_BYTES {
            return Err("decode_limit");
        }
        if packet.track_id != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(|_| "decode_failed")?;
        let rate = decoded.spec().rate();
        let channels = decoded.spec().channels().count();
        if !(8_000..=192_000).contains(&rate)
            || channels == 0
            || channels > MAX_CHANNELS
            || expected_format.is_some_and(|format| format != (rate, channels))
        {
            return Err("audio_format_unsupported");
        }
        expected_format = Some((rate, channels));
        if converter.is_none() {
            converter = Some(resampling::MonoConverter::new(rate, duration_ms)?);
            timeline = Some(container_timeline.for_track(track_id, rate)?);
        }
        let count = decoded.samples_interleaved();
        if !count.is_multiple_of(channels) || count > rate as usize * channels * 10 {
            return Err("decode_limit");
        }
        interleaved.resize(count, 0.0);
        decoded.copy_to_slice_interleaved(&mut interleaved);
        if interleaved.iter().any(|sample| !sample.is_finite()) {
            return Err("invalid_samples");
        }
        let selected = timeline.as_mut().ok_or("decode_failed")?.select_packet(
            count / channels,
            packet.trim_start.get(),
            packet.trim_end.get(),
        )?;
        let start = selected.start.checked_mul(channels).ok_or("decode_limit")?;
        let end = selected.end.checked_mul(channels).ok_or("decode_limit")?;
        mono.clear();
        for frame in interleaved[start..end].chunks_exact(channels) {
            // Lossy codecs may overshoot full scale; normalize each channel
            // before averaging so even extreme finite values cannot overflow.
            mono.push(frame.iter().map(|v| v.clamp(-1.0, 1.0)).sum::<f32>() / channels as f32);
        }
        converter.as_mut().ok_or("decode_failed")?.push(&mono)?;
    }
    if cancelled() {
        return Err("decode_cancelled");
    }
    timeline.ok_or("duration_mismatch")?.finish()?;
    let samples = converter.ok_or("duration_mismatch")?.finish()?;
    if cancelled() {
        return Err("decode_cancelled");
    }
    let actual_duration_ms = samples.len() as u64 * 1_000 / u64::from(SAMPLE_RATE);
    if samples.is_empty() || actual_duration_ms.abs_diff(duration_ms) > 100 {
        return Err("duration_mismatch");
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};

    fn wav(rate: u32, channels: u16, frames: usize, float: bool) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let width = if float { 4_u16 } else { 2 };
        let data_size = frames as u32 * u32::from(channels * width);
        let mut bytes = b"RIFF".to_vec();
        bytes.extend((36 + data_size).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend((if float { 3_u16 } else { 1 }).to_le_bytes());
        bytes.extend(channels.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * u32::from(channels * width)).to_le_bytes());
        bytes.extend((channels * width).to_le_bytes());
        bytes.extend((width * 8).to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data_size.to_le_bytes());
        for _ in 0..frames * usize::from(channels) {
            if float {
                bytes.extend(f32::NAN.to_le_bytes());
            } else {
                bytes.extend(8_192_i16.to_le_bytes());
            }
        }
        file.write_all(&bytes).unwrap();
        file.as_file_mut().rewind().unwrap();
        file
    }

    #[test]
    fn mono_pcm_is_not_resampled_or_rewritten() {
        let file = wav(16_000, 1, 16_000, false);
        let samples = decode_source(file.reopen().unwrap(), "input.wav", 1_000).unwrap();
        assert_eq!(samples.len(), 16_000);
        assert!(samples.iter().all(|sample| *sample == 0.25));
    }

    #[test]
    fn cancellation_before_and_during_decode_never_returns_partial_pcm() {
        let file = wav(16_000, 1, 32_017, false);
        assert_eq!(
            decode_source_with_cancel(file.reopen().unwrap(), "input.wav", 2_002, || true),
            Err("decode_cancelled")
        );
        let mut polls = 0;
        assert_eq!(
            decode_source_with_cancel(file.reopen().unwrap(), "input.wav", 2_002, || {
                polls += 1;
                polls == 3
            }),
            Err("decode_cancelled")
        );
        assert_eq!(polls, 3);
        let exact = decode_source_with_cancel(file.reopen().unwrap(), "input.wav", 2_002, || false)
            .unwrap();
        assert_eq!(exact.len(), 32_017);
        assert!(exact.iter().all(|sample| *sample == 0.25));
    }

    #[test]
    fn common_sample_rates_and_stereo_convert_without_duration_drift() {
        for (rate, channels) in [(48_000, 1), (16_000, 2), (8_000, 1)] {
            let file = wav(rate, channels, rate as usize, false);
            let samples = decode_source(file.reopen().unwrap(), "input.wav", 1_000).unwrap();
            assert_eq!(samples.len(), 16_000);
            assert!(samples[100..15_900]
                .iter()
                .all(|v| (*v - 0.25).abs() < 0.001));
        }
        let file = wav(4_000, 1, 4_000, false);
        assert_eq!(
            decode_source(file.reopen().unwrap(), "input.wav", 1_000),
            Err("audio_format_unsupported")
        );
    }

    #[test]
    fn invalid_and_truncated_audio_cannot_be_reported_complete() {
        let file = wav(16_000, 1, 16_000, true);
        assert_eq!(
            decode_source(file.reopen().unwrap(), "input.wav", 1_000),
            Err("invalid_samples")
        );
        let file = wav(16_000, 1, 16_000, false);
        assert_eq!(
            decode_source(file.reopen().unwrap(), "input.wav", 2_000),
            Err("duration_mismatch")
        );
        assert_eq!(
            decode_source(file.reopen().unwrap(), "input.wav", 100),
            Err("decode_limit")
        );
        file.as_file().set_len(100).unwrap();
        assert!(decode_source(file.reopen().unwrap(), "input.wav", 1_000).is_err());
        assert!(decode_source(tempfile::tempfile().unwrap(), "input.wav", 1_000).is_err());
    }

    #[test]
    #[ignore = "synthetic codec file encoding only; no audio device or inference"]
    fn synthetic_aac_and_mp3_preserve_duration_and_signal() {
        assert_eq!(
            std::env::var("ECHOWALL_CODEC_TEST_CONFIRM").as_deref(),
            Ok("synthetic-codec-files-authorized")
        );
        let mut source = wav(48_000, 2, 96_000, false);
        source
            .as_file_mut()
            .seek(std::io::SeekFrom::Start(44))
            .unwrap();
        for index in 0..96_000 {
            let value = ((std::f64::consts::TAU * 1_000.0 * index as f64 / 48_000.0).sin()
                * 8_192.0) as i16;
            for _ in 0..2 {
                source.write_all(&value.to_le_bytes()).unwrap();
            }
        }
        let header = std::fs::read(source.path()).unwrap();
        assert_eq!(&header[..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(header.len(), 384_044);
        let directory = tempfile::tempdir().unwrap();
        for (extension, codec) in [("m4a", "aac"), ("mp3", "libmp3lame")] {
            let path = directory.path().join(format!("encoded.{extension}"));
            let result = std::process::Command::new("/opt/homebrew/bin/ffmpeg")
                .args([
                    "-nostdin",
                    "-n",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "wav",
                    "-i",
                ])
                .arg(source.path())
                .args(["-c:a", codec, "-b:a", "128k"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "synthetic {extension} encoder failed: {}",
                String::from_utf8_lossy(&result.stderr[..result.stderr.len().min(2_000)])
            );
            let decoded = decode_source(
                File::open(path).unwrap(),
                &format!("encoded.{extension}"),
                2_000,
            )
            .unwrap();
            assert!(
                decoded.len().abs_diff(32_000) <= 256,
                "{extension} produced {} samples; expected32,000 within16ms",
                decoded.len()
            );
            // The in-memory sine becomes only a file; nothing is played.
            // Speech-band energy avoids treating a codec's DC filter as a bug.
            let middle = &decoded[8_000..24_000];
            let rms = (middle.iter().map(|v| v * v).sum::<f32>() / middle.len() as f32).sqrt();
            assert!(
                (rms - 0.25 * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.025,
                "codec/downmix changed the signal scale"
            );
        }
    }
}
