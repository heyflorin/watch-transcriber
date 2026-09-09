#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use desktop_lib::capture::macos::{MacNativeBridge, MacOsCaptureBackend, MacOsNativeBridge};
use desktop_lib::capture::{
    AudioInputSelection, AudioOutputSelection, CaptureMode, CapturePlan, CaptureStartContext,
    CaptureTarget, ConsentAcknowledgements, DesktopCaptureBackend, MeetingSource,
    NativeCaptureSignal, NativeCaptureStream, SegmentCloseReason,
};
use desktop_lib::ingest::envelope::{Platform, TrackRole};
use sha2::Digest;
use uuid::Uuid;

const MIC_FREQUENCY_HZ: f64 = 443.0;
const SYSTEM_FREQUENCY_HZ: f64 = 997.0;

#[test]
#[ignore = "requires explicitly authorized ambient microphone capture"]
fn authorized_voice_memo_captures_nonzero_physical_microphone_audio(
) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ECHOWALL_AMBIENT_MIC_CONFIRM").as_deref() != Ok("authorized") {
        return Err("ambient microphone capture requires the exact confirmation guard".into());
    }
    let duration_seconds: u64 = std::env::var("ECHOWALL_AMBIENT_MIC_SECONDS")
        .unwrap_or_else(|_| "5".to_owned())
        .parse()?;
    if !(3..=7_200).contains(&duration_seconds) {
        return Err("ambient microphone duration must be between 3 and 7200 seconds".into());
    }
    let microphone_label = std::env::var("ECHOWALL_AMBIENT_MIC_DEVICE")
        .unwrap_or_else(|_| "MacBook Pro Microphone".to_owned());
    let output = fs::canonicalize(std::env::var("ECHOWALL_AMBIENT_MIC_OUTPUT")?)?;
    let canonical_temp = fs::canonicalize(std::env::temp_dir())?;
    if !output.starts_with(&canonical_temp) {
        return Err("ambient microphone output must remain under the system temp directory".into());
    }
    eprintln!("ambient_phase=enumerate_microphones");
    let microphone = MacOsNativeBridge::enumerate_microphones()
        .into_iter()
        .find(|source| source.label == microphone_label)
        .ok_or("configured physical microphone is unavailable")?;
    let plan = CapturePlan {
        mode: CaptureMode::VoiceMemo,
        platform: Platform::Macos,
        target: CaptureTarget::VoiceMemo {
            microphone: AudioInputSelection {
                id: microphone.id,
                label: microphone.label,
            },
        },
        consent: ConsentAcknowledgements {
            microphone: true,
            selected_source: false,
            whole_browser_warning: false,
            all_system_audio: false,
        },
    };
    eprintln!("ambient_phase=preflight");
    let backend = MacOsCaptureBackend::default();
    let preflight = backend.preflight(&plan)?;
    if !preflight.ready_for(&plan) {
        return Err(format!("physical microphone preflight failed: {preflight:?}").into());
    }
    eprintln!("ambient_phase=start");
    let monotonic_started_ns = 1_000_000_000_u64;
    let mut stream = backend.start(CaptureStartContext {
        recording_id: Uuid::new_v4(),
        package_directory: output.clone(),
        plan,
        monotonic_started_ns,
    })?;
    eprintln!("ambient_phase=recording");
    let mut signals = collect_signals(&mut *stream, duration_seconds)?;
    signals.extend(stream.take_signals()?);
    let segments = stream.close_segment(
        SegmentCloseReason::Stop,
        monotonic_started_ns.saturating_add(duration_seconds.saturating_mul(1_000_000_000)),
    )?;
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| segment.role != TrackRole::Microphone)
    {
        return Err("Voice Memo must produce only non-empty microphone segments".into());
    }
    let duration_ms = segments
        .iter()
        .map(desktop_lib::capture::SegmentMetadata::duration_ms)
        .sum::<u64>();
    if duration_ms < duration_seconds.saturating_sub(2).saturating_mul(1_000) {
        return Err("physical microphone segment was too short".into());
    }
    let microphones = segments.iter().collect::<Vec<_>>();
    let (rms, peak) = segment_aggregate_level(&output, &microphones)?;
    if rms <= 0.000_01 || peak <= 0.000_1 {
        return Err("physical microphone produced only digital silence".into());
    }
    let identity = segments
        .iter()
        .flat_map(|segment| segment.sha256.as_bytes())
        .copied()
        .collect::<Vec<_>>();
    let sha256 = hex::encode(sha2::Sha256::digest(identity));
    println!(
        "{{\"duration_ms\":{duration_ms},\"segment_count\":{},\"sample_rate\":{},\"channels\":{},\"rms\":{rms:.8},\"peak\":{peak:.8},\"sha256_prefix\":{},\"signal_count\":{}}}",
        segments.len(),
        segments[0].sample_rate,
        segments[0].channels,
        serde_json::to_string(&sha256[..12])?,
        signals.len(),
    );
    Ok(())
}

#[test]
#[ignore = "requires Screen Recording permission and generated virtual CoreAudio fixtures"]
fn generated_meeting_capture_isolated_and_clock_aligned() -> Result<(), Box<dyn std::error::Error>>
{
    generated_capture_isolated_and_clock_aligned(CaptureMode::Meeting)
}

#[test]
#[ignore = "requires Screen Recording permission and generated virtual CoreAudio fixtures"]
fn generated_system_capture_is_clock_aligned() -> Result<(), Box<dyn std::error::Error>> {
    generated_capture_isolated_and_clock_aligned(CaptureMode::SystemCapture)
}

fn generated_capture_isolated_and_clock_aligned(
    capture_mode: CaptureMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture_pid: u32 = std::env::var("ECHOWALL_FIXTURE_SYSTEM_PID")?.parse()?;
    let duration_seconds: u64 = std::env::var("ECHOWALL_FIXTURE_DURATION_SECONDS")
        .unwrap_or_else(|_| "8".to_owned())
        .parse()?;
    if !(3..=7_200).contains(&duration_seconds) {
        return Err("fixture duration must be between 3 and 7200 seconds".into());
    }
    let output = PathBuf::from(std::env::var("ECHOWALL_FIXTURE_OUTPUT")?);
    let pause_after_seconds = std::env::var("ECHOWALL_CAPTURE_SPIKE_PAUSE_AFTER")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?;
    let pause_seconds = std::env::var("ECHOWALL_CAPTURE_SPIKE_PAUSE_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(2);
    let source_exit_after = std::env::var("ECHOWALL_CAPTURE_SPIKE_SOURCE_EXIT_AFTER")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .filter(|value| *value > 0);
    if pause_after_seconds.is_some_and(|after| {
        after < 2
            || after >= duration_seconds.saturating_sub(2)
            || !(1..=10).contains(&pause_seconds)
    }) {
        return Err("pause fixture requires a 2s margin and a 1-10s pause".into());
    }
    if source_exit_after.is_some_and(|after| {
        after < 2 || after >= duration_seconds.saturating_sub(1) || pause_after_seconds.is_some()
    }) {
        return Err("source-exit fixture requires a 2s margin and no pause".into());
    }
    if capture_mode != CaptureMode::Meeting && source_exit_after.is_some() {
        return Err("source-exit fixture requires Meeting mode".into());
    }
    let microphone_label =
        std::env::var("ECHOWALL_FIXTURE_MIC_DEVICE").unwrap_or_else(|_| "BlackHole 2ch".to_owned());
    fs::create_dir_all(&output)?;

    let bridge = MacOsNativeBridge;
    let microphone = MacOsNativeBridge::enumerate_microphones()
        .into_iter()
        .find(|source| source.label == microphone_label)
        .ok_or("configured fixture microphone input is required")?;
    let microphone = AudioInputSelection {
        id: microphone.id,
        label: microphone.label,
    };
    let (target, consent) = if capture_mode == CaptureMode::Meeting {
        let application = bridge
            .enumerate_sources()?
            .into_iter()
            .find(|source| source.process_id == Some(fixture_pid))
            .ok_or("synthetic system-audio application is not shareable")?;
        (
            CaptureTarget::Meeting {
                microphone,
                source: MeetingSource::NativeApplication {
                    id: application.id,
                    label: application.label,
                },
            },
            ConsentAcknowledgements {
                microphone: true,
                selected_source: true,
                whole_browser_warning: false,
                all_system_audio: false,
            },
        )
    } else {
        (
            CaptureTarget::SystemCapture {
                microphone,
                output: AudioOutputSelection {
                    id: "all-system-audio".to_owned(),
                    label: "All System Audio".to_owned(),
                },
            },
            ConsentAcknowledgements {
                microphone: true,
                selected_source: false,
                whole_browser_warning: false,
                all_system_audio: true,
            },
        )
    };
    let plan = CapturePlan {
        mode: capture_mode,
        platform: Platform::Macos,
        target,
        consent,
    };
    let backend = MacOsCaptureBackend::default();
    let preflight = backend.preflight(&plan)?;
    if !preflight.ready_for(&plan) {
        return Err(format!("macOS capture preflight failed: {preflight:?}").into());
    }
    let monotonic_started_ns = 1_000_000_000_u64;
    let mut stream = backend.start(CaptureStartContext {
        recording_id: Uuid::new_v4(),
        package_directory: output.clone(),
        plan,
        monotonic_started_ns,
    })?;
    if source_exit_after.is_some() {
        let marker = PathBuf::from(std::env::var("ECHOWALL_FIXTURE_CAPTURE_STARTED_MARKER")?);
        fs::write(marker, b"started")?;
    }
    let mut startup_signals = Vec::new();
    let mut segments = Vec::new();
    if let Some(after) = pause_after_seconds {
        startup_signals.extend(collect_signals(&mut *stream, after)?);
        segments.extend(stream.close_segment(
            SegmentCloseReason::Pause,
            monotonic_started_ns.saturating_add(after.saturating_mul(1_000_000_000)),
        )?);
        thread::sleep(Duration::from_secs(pause_seconds));
        stream.resume(
            monotonic_started_ns.saturating_add(
                after
                    .saturating_add(pause_seconds)
                    .saturating_mul(1_000_000_000),
            ),
        )?;
        startup_signals.extend(collect_signals(
            &mut *stream,
            duration_seconds.saturating_sub(after),
        )?);
    } else {
        startup_signals.extend(collect_signals(&mut *stream, duration_seconds)?);
    }
    startup_signals.extend(stream.take_signals()?);
    if startup_signals.iter().any(|signal| {
        matches!(
            signal,
            NativeCaptureSignal::SourceLost {
                code: "macos_process_tap_no_audio_callbacks" | "macos_process_tap_restart_failed",
                ..
            }
        )
    }) {
        return Err("selected system source did not recover its audio callbacks".into());
    }
    if source_exit_after.is_some() {
        for _ in 0..20 {
            if startup_signals.iter().any(|signal| {
                matches!(
                    signal,
                    desktop_lib::capture::NativeCaptureSignal::SourceLost {
                        code: "macos_capture_source_exited",
                        ..
                    }
                )
            }) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
            startup_signals.extend(stream.take_signals()?);
        }
    }
    if source_exit_after.is_some()
        && !startup_signals.iter().any(|signal| {
            matches!(
                signal,
                desktop_lib::capture::NativeCaptureSignal::SourceLost {
                    code: "macos_capture_source_exited",
                    ..
                }
            )
        })
    {
        return Err("exited selected process did not emit a durable source-loss signal".into());
    }
    segments.extend(
        stream.close_segment(
            SegmentCloseReason::Stop,
            monotonic_started_ns.saturating_add(
                duration_seconds
                    .saturating_add(pause_after_seconds.map_or(0, |_| pause_seconds))
                    .saturating_mul(1_000_000_000),
            ),
        )?,
    );
    let microphones: Vec<_> = segments
        .iter()
        .filter(|segment| segment.role == TrackRole::Microphone)
        .collect();
    let systems: Vec<_> = segments
        .iter()
        .filter(|segment| segment.role == TrackRole::System)
        .collect();
    let minimum_role_segments = if pause_after_seconds.is_some() { 2 } else { 1 };
    if microphones.len() < minimum_role_segments || systems.len() < minimum_role_segments {
        return Err(format!("expected aligned mic/system segments, got {segments:?}").into());
    }
    let start_drift_ms = microphones[0]
        .clock_start_ns
        .abs_diff(systems[0].clock_start_ns)
        / 1_000_000;
    let end_drift_ms = microphones
        .last()
        .unwrap()
        .clock_end_ns
        .abs_diff(systems.last().unwrap().clock_end_ns)
        / 1_000_000;
    if source_exit_after.is_none() && end_drift_ms > 100 {
        return Err(format!(
            "mic/system end drift exceeded 100ms: start={start_drift_ms} end={end_drift_ms}"
        )
        .into());
    }
    if start_drift_ms > 100
        && !startup_signals.iter().any(|signal| {
            matches!(
                signal,
                desktop_lib::capture::NativeCaptureSignal::Gap {
                    code: "macos_audio_startup_delay",
                    ..
                }
            )
        })
    {
        return Err("large capture start offset was not represented as a durable gap".into());
    }
    let minimum_duration_ms = duration_seconds.saturating_sub(2).saturating_mul(1_000);
    let minimum_system_duration_ms = source_exit_after
        .unwrap_or(duration_seconds)
        .saturating_sub(2)
        .saturating_mul(1_000);
    let microphone_duration_ms: u64 = microphones
        .iter()
        .map(|segment| segment.duration_ms())
        .sum();
    let system_duration_ms: u64 = systems.iter().map(|segment| segment.duration_ms()).sum();
    if microphone_duration_ms < minimum_duration_ms
        || system_duration_ms < minimum_system_duration_ms
    {
        return Err(format!(
            "capture was too short: mic_ms={microphone_duration_ms} system_ms={system_duration_ms}"
        )
        .into());
    }

    let physical_microphone = std::env::var("ECHOWALL_CAPTURE_PHYSICAL_MIC").as_deref() == Ok("1");
    let (mic_expected, mic_leak) = if physical_microphone {
        (0.0, 0.0)
    } else {
        segment_tone_extrema(&output, &microphones, MIC_FREQUENCY_HZ, SYSTEM_FREQUENCY_HZ)?
    };
    let (mic_rms, mic_peak) = segment_aggregate_level(&output, &microphones)?;
    let (system_expected, system_leak) =
        segment_tone_extrema(&output, &systems, SYSTEM_FREQUENCY_HZ, MIC_FREQUENCY_HZ)?;
    println!(
        "{{\"capture_mode\":{},\"duration_seconds\":{duration_seconds},\"start_drift_ms\":{start_drift_ms},\"end_drift_ms\":{end_drift_ms},\"startup_signal_count\":{},\"physical_microphone\":{physical_microphone},\"mic_rms\":{mic_rms:.8},\"mic_peak\":{mic_peak:.8},\"mic_443\":{mic_expected:.6},\"mic_997\":{mic_leak:.6},\"system_997\":{system_expected:.6},\"system_443\":{system_leak:.6},\"artifact_root\":{}}}",
        serde_json::to_string(match capture_mode {
            CaptureMode::Meeting => "meeting",
            CaptureMode::SystemCapture => "system_capture",
            CaptureMode::VoiceMemo => "voice_memo",
        })?,
        startup_signals.len(),
        serde_json::to_string(&output)?
    );
    if system_expected < 0.01 || system_expected < system_leak * 4.0 {
        return Err(format!(
            "selected application was missing or contaminated: expected={system_expected:.5} leak={system_leak:.5}"
        )
        .into());
    }
    if physical_microphone {
        if mic_rms <= 0.000_01 || mic_peak <= 0.000_1 {
            return Err("physical microphone produced only digital silence".into());
        }
        return Ok(());
    }
    if std::env::var("ECHOWALL_CAPTURE_SYSTEM_ONLY").as_deref() == Ok("1") {
        return Ok(());
    }
    if mic_expected < 0.01 || mic_expected < mic_leak * 4.0 {
        return Err(format!(
            "microphone fixture was missing or contaminated: expected={mic_expected:.5} leak={mic_leak:.5}"
        )
        .into());
    }
    Ok(())
}

fn collect_signals(
    stream: &mut dyn NativeCaptureStream,
    seconds: u64,
) -> Result<Vec<NativeCaptureSignal>, Box<dyn std::error::Error>> {
    let mut signals = Vec::new();
    for _ in 0..seconds {
        thread::sleep(Duration::from_secs(1));
        signals.extend(stream.take_signals()?);
    }
    Ok(signals)
}

fn segment_aggregate_level(
    output: &Path,
    segments: &[&desktop_lib::capture::SegmentMetadata],
) -> Result<(f64, f64), Box<dyn std::error::Error>> {
    let mut weighted_squares = 0.0_f64;
    let mut sample_count = 0_usize;
    let mut peak = 0.0_f64;
    for segment in segments {
        let audio = decode_pcm16_wav(&output.join(&segment.relative_path))?;
        let (segment_rms, segment_peak) = aggregate_level(&audio);
        weighted_squares += segment_rms * segment_rms * audio.samples.len() as f64;
        sample_count = sample_count.saturating_add(audio.samples.len());
        peak = peak.max(segment_peak);
    }
    if sample_count == 0 {
        return Err("capture segment set contained no microphone samples".into());
    }
    Ok(((weighted_squares / sample_count as f64).sqrt(), peak))
}

struct WavAudio {
    sample_rate: u32,
    channels: usize,
    samples: Vec<i16>,
}

fn segment_tone_extrema(
    output: &Path,
    segments: &[&desktop_lib::capture::SegmentMetadata],
    expected_frequency_hz: f64,
    leak_frequency_hz: f64,
) -> Result<(f64, f64), Box<dyn std::error::Error>> {
    let mut minimum_expected = f64::INFINITY;
    let mut maximum_leak = 0.0_f64;
    for segment in segments {
        let audio = decode_pcm16_wav(&output.join(&segment.relative_path))?;
        minimum_expected = minimum_expected.min(tone_amplitude(&audio, expected_frequency_hz));
        maximum_leak = maximum_leak.max(tone_amplitude(&audio, leak_frequency_hz));
    }
    if minimum_expected.is_finite() {
        Ok((minimum_expected, maximum_leak))
    } else {
        Err("capture segment set is empty".into())
    }
}

fn decode_pcm16_wav(path: &Path) -> Result<WavAudio, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("invalid WAV container: {}", path.display()).into());
    }
    let channels = usize::from(u16::from_le_bytes(bytes[22..24].try_into()?));
    let sample_rate = u32::from_le_bytes(bytes[24..28].try_into()?);
    let bits = u16::from_le_bytes(bytes[34..36].try_into()?);
    let data_bytes = usize::try_from(u32::from_le_bytes(bytes[40..44].try_into()?))?;
    if channels == 0 || bits != 16 || bytes.len() != 44_usize.saturating_add(data_bytes) {
        return Err(format!("unsupported or truncated WAV: {}", path.display()).into());
    }
    let samples = bytes[44..]
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect();
    Ok(WavAudio {
        sample_rate,
        channels,
        samples,
    })
}

fn tone_amplitude(audio: &WavAudio, frequency_hz: f64) -> f64 {
    let frame_count = audio.samples.len() / audio.channels;
    let trim = (usize::try_from(audio.sample_rate).unwrap_or(0) / 4).min(frame_count / 4);
    let usable = frame_count.saturating_sub(trim.saturating_mul(2));
    if usable == 0 {
        return 0.0;
    }
    let mut sin_projection = 0.0;
    let mut cos_projection = 0.0;
    for (offset, frame) in (trim..trim + usable).enumerate() {
        let mono = (0..audio.channels)
            .map(|channel| f64::from(audio.samples[frame * audio.channels + channel]))
            .sum::<f64>()
            / audio.channels as f64
            / f64::from(i16::MAX);
        let phase = 2.0 * std::f64::consts::PI * frequency_hz * offset as f64
            / f64::from(audio.sample_rate);
        sin_projection += mono * phase.sin();
        cos_projection += mono * phase.cos();
    }
    2.0 * sin_projection.hypot(cos_projection) / usable as f64
}

fn aggregate_level(audio: &WavAudio) -> (f64, f64) {
    if audio.samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut sum_squares = 0.0_f64;
    let mut peak = 0.0_f64;
    for sample in &audio.samples {
        let normalized = f64::from(*sample) / f64::from(i16::MAX);
        sum_squares += normalized * normalized;
        peak = peak.max(normalized.abs());
    }
    ((sum_squares / audio.samples.len() as f64).sqrt(), peak)
}
