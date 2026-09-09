//! EchoWall's Apple-Silicon local Whisper crash boundary.
//!
//! This process handles exactly one bounded request from stdin, reads only the
//! named model and App-owned audio below `ECHOWALL_APP_DATA_ROOT`, emits one
//! bounded response to stdout, and exits. It owns no queue, credentials,
//! network client, listener, or persistent state.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerError {
    code: &'static str,
}

impl WorkerError {
    fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local Whisper worker failed")
    }
}

impl std::error::Error for WorkerError {}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple_silicon {
    use std::ffi::OsString;
    use std::fs::{self, File, Metadata};
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::path::{Component, Path, PathBuf};

    use echowall_local_whisper_protocol::{
        decode_request, encode_response, LocalWhisperRequest, LocalWhisperResponse,
        LocalWhisperSegment, LOCAL_WHISPER_PROTOCOL_VERSION, MAX_LOCAL_WHISPER_REQUEST_BYTES,
    };
    use sha2::{Digest, Sha256};
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    use super::WorkerError;

    pub const APP_DATA_ROOT_ENV: &str = "ECHOWALL_APP_DATA_ROOT";
    const MODEL_FILE_NAME: &str = "model.bin";
    const TARGET_SAMPLE_RATE: u32 = 16_000;
    const MAX_CHANNELS: usize = 64;
    const MAX_SOURCE_SAMPLE_RATE: u32 = 384_000;
    const MAX_AUDIO_PACKETS: u64 = 4_000_000;

    pub fn run_once() -> Result<(), WorkerError> {
        if std::env::args_os().len() != 1 {
            return Err(WorkerError::new("arguments_forbidden"));
        }
        let root = std::env::var_os(APP_DATA_ROOT_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| WorkerError::new("root_missing"))?;
        let root = validate_root(&root)?;
        let request = read_request(io::stdin().lock())?;

        let model_relative = PathBuf::from("models")
            .join("whisper")
            .join(&request.model_id)
            .join(MODEL_FILE_NAME);
        let model = VerifiedFile::open(
            &root,
            &model_relative,
            request.model_size_bytes,
            &request.model_sha256,
            "model_unavailable",
        )?;
        let audio_relative = PathBuf::from("inbox")
            .join(request.recording_id.to_string())
            .join(&request.audio_relative_path);
        let audio = VerifiedFile::open(
            &root,
            &audio_relative,
            request.audio_size_bytes,
            &request.audio_sha256,
            "audio_unavailable",
        )?;

        let samples = decode_audio(&audio, &request)?;
        audio.verify_unchanged()?;
        model.verify_unchanged()?;
        let response = transcribe(&model, samples, &request)?;
        model.verify_unchanged()?;

        let encoded =
            encode_response(&response, &request).map_err(|_| WorkerError::new("invalid_output"))?;
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&encoded)
            .and_then(|()| stdout.flush())
            .map_err(|_| WorkerError::new("output_failed"))
    }

    fn read_request(mut reader: impl Read) -> Result<LocalWhisperRequest, WorkerError> {
        let maximum = u64::try_from(MAX_LOCAL_WHISPER_REQUEST_BYTES)
            .map_err(|_| WorkerError::new("invalid_limit"))?;
        let mut bytes = Vec::with_capacity(MAX_LOCAL_WHISPER_REQUEST_BYTES.min(8 * 1024));
        reader
            .by_ref()
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| WorkerError::new("input_failed"))?;
        decode_request(&bytes).map_err(|_| WorkerError::new("invalid_request"))
    }

    fn validate_root(path: &Path) -> Result<PathBuf, WorkerError> {
        if !path.is_absolute() {
            return Err(WorkerError::new("invalid_root"));
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| WorkerError::new("invalid_root"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WorkerError::new("invalid_root"));
        }
        fs::canonicalize(path).map_err(|_| WorkerError::new("invalid_root"))
    }

    #[derive(Debug)]
    struct VerifiedFile {
        path: PathBuf,
        file: File,
        fingerprint: FileFingerprint,
        error_code: &'static str,
    }

    impl VerifiedFile {
        fn open(
            root: &Path,
            relative: &Path,
            expected_size: u64,
            expected_sha256: &str,
            error_code: &'static str,
        ) -> Result<Self, WorkerError> {
            let path = resolve_regular_file(root, relative, error_code)?;
            let mut file = File::open(&path).map_err(|_| WorkerError::new(error_code))?;
            let fingerprint =
                FileFingerprint::read(&file.metadata().map_err(|_| WorkerError::new(error_code))?)?;
            if fingerprint.size_bytes != expected_size {
                return Err(WorkerError::new("identity_mismatch"));
            }
            let actual_sha256 = hash_file(&mut file)?;
            if actual_sha256 != expected_sha256 {
                return Err(WorkerError::new("identity_mismatch"));
            }
            let opened = Self {
                path,
                file,
                fingerprint,
                error_code,
            };
            opened.verify_unchanged()?;
            Ok(opened)
        }

        fn cloned_from_start(&self) -> Result<File, WorkerError> {
            let mut file = self
                .file
                .try_clone()
                .map_err(|_| WorkerError::new(self.error_code))?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| WorkerError::new(self.error_code))?;
            Ok(file)
        }

        fn verify_unchanged(&self) -> Result<(), WorkerError> {
            let metadata = fs::symlink_metadata(&self.path)
                .map_err(|_| WorkerError::new("identity_changed"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(WorkerError::new("identity_changed"));
            }
            let path_fingerprint = FileFingerprint::read(&metadata)?;
            let open_fingerprint = FileFingerprint::read(
                &self
                    .file
                    .metadata()
                    .map_err(|_| WorkerError::new("identity_changed"))?,
            )?;
            if path_fingerprint != self.fingerprint || open_fingerprint != self.fingerprint {
                return Err(WorkerError::new("identity_changed"));
            }
            Ok(())
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FileFingerprint {
        size_bytes: u64,
        modified: Option<std::time::SystemTime>,
        device: u64,
        inode: u64,
    }

    impl FileFingerprint {
        fn read(metadata: &Metadata) -> Result<Self, WorkerError> {
            use std::os::unix::fs::MetadataExt;

            if !metadata.is_file() {
                return Err(WorkerError::new("not_regular_file"));
            }
            Ok(Self {
                size_bytes: metadata.len(),
                modified: metadata.modified().ok(),
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
    }

    fn resolve_regular_file(
        root: &Path,
        relative: &Path,
        error_code: &'static str,
    ) -> Result<PathBuf, WorkerError> {
        if relative.is_absolute()
            || relative.components().any(|component| {
                !matches!(component, Component::Normal(_))
                    || component.as_os_str().to_str().is_none()
            })
        {
            return Err(WorkerError::new(error_code));
        }
        let mut candidate = root.to_path_buf();
        let components: Vec<OsString> = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value.to_os_string()),
                _ => None,
            })
            .collect();
        if components.is_empty() {
            return Err(WorkerError::new(error_code));
        }
        for (index, component) in components.iter().enumerate() {
            candidate.push(component);
            let metadata =
                fs::symlink_metadata(&candidate).map_err(|_| WorkerError::new(error_code))?;
            if metadata.file_type().is_symlink()
                || index + 1 < components.len() && !metadata.is_dir()
                || index + 1 == components.len() && !metadata.is_file()
            {
                return Err(WorkerError::new(error_code));
            }
        }
        let canonical = fs::canonicalize(&candidate).map_err(|_| WorkerError::new(error_code))?;
        if !canonical.starts_with(root) {
            return Err(WorkerError::new(error_code));
        }
        Ok(canonical)
    }

    fn hash_file(file: &mut File) -> Result<String, WorkerError> {
        file.seek(SeekFrom::Start(0))
            .map_err(|_| WorkerError::new("identity_unavailable"))?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| WorkerError::new("identity_unavailable"))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| WorkerError::new("identity_unavailable"))?;
        Ok(hex::encode(digest.finalize()))
    }

    fn decode_audio(
        source: &VerifiedFile,
        request: &LocalWhisperRequest,
    ) -> Result<Vec<f32>, WorkerError> {
        let stream =
            MediaSourceStream::new(Box::new(source.cloned_from_start()?), Default::default());
        let mut hint = Hint::new();
        let extension = Path::new(&request.audio_relative_path)
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| WorkerError::new("decode_failed"))?;
        hint.with_extension(extension);
        let mut format = symphonia::default::get_probe()
            .probe(
                &hint,
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|_| WorkerError::new("decode_failed"))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| WorkerError::new("decode_failed"))?;
        let parameters = track
            .codec_params
            .as_ref()
            .and_then(|parameters| parameters.audio())
            .ok_or_else(|| WorkerError::new("decode_failed"))?;
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(parameters, &AudioDecoderOptions::default())
            .map_err(|_| WorkerError::new("decode_failed"))?;
        let track_id = track.id;
        let mut expected_rate = None;
        let mut expected_channels = None;
        let mut resampler = None;
        let mut interleaved = Vec::<f32>::new();
        let mut source_frames = 0_u64;
        let mut packets = 0_u64;

        loop {
            let packet = match format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(_) => return Err(WorkerError::new("decode_failed")),
            };
            if packet.track_id != track_id {
                continue;
            }
            packets = packets.saturating_add(1);
            if packets > MAX_AUDIO_PACKETS {
                return Err(WorkerError::new("decode_limit"));
            }
            let decoded = decoder
                .decode(&packet)
                .map_err(|_| WorkerError::new("decode_failed"))?;
            let rate = decoded.spec().rate();
            let channels = decoded.spec().channels().count();
            if rate == 0
                || rate > MAX_SOURCE_SAMPLE_RATE
                || channels == 0
                || channels > MAX_CHANNELS
                || expected_rate.is_some_and(|expected| expected != rate)
                || expected_channels.is_some_and(|expected| expected != channels)
            {
                return Err(WorkerError::new("decode_failed"));
            }
            expected_rate = Some(rate);
            expected_channels = Some(channels);
            if resampler.is_none() {
                resampler = Some(LinearResampler::new(rate, TARGET_SAMPLE_RATE)?);
            }
            interleaved.resize(decoded.samples_interleaved(), 0.0);
            decoded.copy_to_slice_interleaved(&mut interleaved);
            if !interleaved.len().is_multiple_of(channels) {
                return Err(WorkerError::new("decode_failed"));
            }
            let resampler = resampler
                .as_mut()
                .ok_or_else(|| WorkerError::new("decode_failed"))?;
            for frame in interleaved.chunks_exact(channels) {
                let mono = frame.iter().copied().sum::<f32>() / channels as f32;
                if !mono.is_finite() {
                    return Err(WorkerError::new("decode_failed"));
                }
                resampler.push(mono.clamp(-1.0, 1.0))?;
                source_frames = source_frames
                    .checked_add(1)
                    .ok_or_else(|| WorkerError::new("decode_limit"))?;
            }
        }

        let rate = expected_rate.ok_or_else(|| WorkerError::new("decode_failed"))?;
        let actual_duration_ms = source_frames
            .checked_mul(1_000)
            .map(|value| value / u64::from(rate))
            .ok_or_else(|| WorkerError::new("decode_limit"))?;
        let tolerance_ms = 2_000_u64.max(request.audio_duration_ms / 100);
        if actual_duration_ms == 0
            || actual_duration_ms.abs_diff(request.audio_duration_ms) > tolerance_ms
        {
            return Err(WorkerError::new("duration_mismatch"));
        }
        let samples = resampler
            .ok_or_else(|| WorkerError::new("decode_failed"))?
            .finish();
        let maximum_samples = request
            .audio_duration_ms
            .saturating_add(tolerance_ms)
            .saturating_mul(u64::from(TARGET_SAMPLE_RATE))
            / 1_000;
        if samples.is_empty() || u64::try_from(samples.len()).unwrap_or(u64::MAX) > maximum_samples
        {
            return Err(WorkerError::new("decode_limit"));
        }
        Ok(samples)
    }

    struct LinearResampler {
        source_rate: u64,
        target_rate: u64,
        source_index: u64,
        next_output_index: u64,
        previous: Option<f32>,
        output: Vec<f32>,
    }

    impl LinearResampler {
        fn new(source_rate: u32, target_rate: u32) -> Result<Self, WorkerError> {
            if source_rate == 0 || target_rate == 0 {
                return Err(WorkerError::new("decode_failed"));
            }
            Ok(Self {
                source_rate: u64::from(source_rate),
                target_rate: u64::from(target_rate),
                source_index: 0,
                next_output_index: 0,
                previous: None,
                output: Vec::new(),
            })
        }

        fn push(&mut self, sample: f32) -> Result<(), WorkerError> {
            let current_index = self.source_index;
            if let Some(previous) = self.previous {
                let current_position = current_index
                    .checked_mul(self.target_rate)
                    .ok_or_else(|| WorkerError::new("decode_limit"))?;
                loop {
                    let output_position = self
                        .next_output_index
                        .checked_mul(self.source_rate)
                        .ok_or_else(|| WorkerError::new("decode_limit"))?;
                    if output_position > current_position {
                        break;
                    }
                    let left_index = output_position / self.target_rate;
                    let value = if left_index == current_index {
                        sample
                    } else if left_index.saturating_add(1) == current_index {
                        let remainder = output_position % self.target_rate;
                        let fraction = remainder as f32 / self.target_rate as f32;
                        previous * (1.0 - fraction) + sample * fraction
                    } else {
                        return Err(WorkerError::new("decode_failed"));
                    };
                    self.output.push(value);
                    self.next_output_index = self
                        .next_output_index
                        .checked_add(1)
                        .ok_or_else(|| WorkerError::new("decode_limit"))?;
                }
            } else {
                self.output.push(sample);
                self.next_output_index = 1;
            }
            self.previous = Some(sample);
            self.source_index = self
                .source_index
                .checked_add(1)
                .ok_or_else(|| WorkerError::new("decode_limit"))?;
            Ok(())
        }

        fn finish(self) -> Vec<f32> {
            self.output
        }
    }

    fn transcribe(
        model: &VerifiedFile,
        samples: Vec<f32>,
        request: &LocalWhisperRequest,
    ) -> Result<LocalWhisperResponse, WorkerError> {
        if let Some(language) = request.language.as_deref() {
            if whisper_rs::get_lang_id(language).is_none() {
                return Err(WorkerError::new("unsupported_language"));
            }
        }
        whisper_rs::install_logging_hooks();
        let context =
            WhisperContext::new_with_params(&model.path, WhisperContextParameters::default())
                .map_err(|_| WorkerError::new("model_load_failed"))?;
        model.verify_unchanged()?;
        let mut state = context
            .create_state()
            .map_err(|_| WorkerError::new("model_load_failed"))?;
        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        });
        let threads = std::thread::available_parallelism()
            .map(|count| count.get().min(8))
            .unwrap_or(1);
        params.set_n_threads(i32::try_from(threads).unwrap_or(1));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_language(request.language.as_deref());
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        state
            .full(params, &samples)
            .map_err(|_| WorkerError::new("inference_failed"))?;

        let language = request.language.clone().unwrap_or_else(|| {
            whisper_rs::get_lang_str(state.full_lang_id_from_state())
                .unwrap_or("und")
                .to_owned()
        });
        let mut segments: Vec<LocalWhisperSegment> = Vec::new();
        let mut previous_end = 0_u64;
        let mut leading_zero_duration_text = String::new();
        for segment in state.as_iter() {
            let start = u64::try_from(segment.start_timestamp())
                .ok()
                .and_then(|value| value.checked_mul(10))
                .ok_or_else(|| WorkerError::new("invalid_output"))?;
            let end = u64::try_from(segment.end_timestamp())
                .ok()
                .and_then(|value| value.checked_mul(10))
                .ok_or_else(|| WorkerError::new("invalid_output"))?
                .min(request.audio_duration_ms);
            let raw_text = segment
                .to_str_lossy()
                .map_err(|_| WorkerError::new("invalid_output"))?
                .into_owned();
            if raw_text.trim().is_empty() {
                continue;
            }
            // Container duration and whisper.cpp's 10 ms timestamp grid may
            // differ by one final tick. Ignore only a segment wholly outside
            // the verified media duration; never clamp or reorder an
            // overlapping in-range segment.
            if start >= request.audio_duration_ms {
                continue;
            }
            if start == end {
                // whisper.cpp can emit a text-bearing boundary segment with
                // no duration. Preserve its exact text by attaching it to an
                // adjacent real segment; do not invent a time range.
                if let Some(previous) = segments.last_mut() {
                    previous.text.push_str(&raw_text);
                } else {
                    leading_zero_duration_text.push_str(&raw_text);
                }
                continue;
            }
            if start > end {
                return Err(WorkerError::new("segment_timestamp_reversed"));
            }
            if start < previous_end {
                return Err(WorkerError::new("segment_overlap"));
            }
            let mut text = std::mem::take(&mut leading_zero_duration_text);
            text.push_str(&raw_text);
            segments.push(LocalWhisperSegment {
                start_ms: start,
                end_ms: end,
                text: text.trim().to_owned(),
                speaker_id: None,
            });
            previous_end = end;
        }
        let response = LocalWhisperResponse {
            schema_version: LOCAL_WHISPER_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            model_id: request.model_id.clone(),
            model_sha256: request.model_sha256.clone(),
            audio_sha256: request.audio_sha256.clone(),
            language,
            segments,
        };
        response
            .validate_against(request)
            .map_err(|error| WorkerError::new(error.code))?;
        Ok(response)
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::symlink;

        use echowall_local_whisper_protocol::{
            encode_request, LocalWhisperRequest, LOCAL_WHISPER_PROTOCOL_VERSION,
        };
        use sha2::{Digest, Sha256};
        use tempfile::TempDir;

        use super::*;

        fn digest(bytes: &[u8]) -> String {
            hex::encode(Sha256::digest(bytes))
        }

        fn request() -> LocalWhisperRequest {
            LocalWhisperRequest {
                schema_version: LOCAL_WHISPER_PROTOCOL_VERSION,
                recording_id: "018f92d8-6ad4-7dc1-8e28-8b020d2942cb".parse().unwrap(),
                model_id: "tiny".to_owned(),
                model_sha256: "a".repeat(64),
                model_size_bytes: 4,
                audio_relative_path: "derived/mixed.wav".to_owned(),
                audio_sha256: "b".repeat(64),
                audio_size_bytes: 4,
                audio_duration_ms: 1_000,
                language: Some("zh".to_owned()),
            }
        }

        #[test]
        fn request_reader_is_bounded_and_closed() {
            let encoded = encode_request(&request()).unwrap();
            assert_eq!(read_request(encoded.as_slice()).unwrap(), request());
            assert_eq!(
                read_request(vec![b' '; MAX_LOCAL_WHISPER_REQUEST_BYTES + 1].as_slice())
                    .unwrap_err()
                    .code(),
                "invalid_request"
            );
        }

        #[test]
        fn verified_file_rejects_symlink_and_identity_mismatch() {
            let root = TempDir::new().unwrap();
            let canonical_root = validate_root(root.path()).unwrap();
            let model_dir = root.path().join("models/whisper/tiny");
            fs::create_dir_all(&model_dir).unwrap();
            let bytes = b"model";
            fs::write(model_dir.join(MODEL_FILE_NAME), bytes).unwrap();
            let relative = Path::new("models/whisper/tiny/model.bin");
            VerifiedFile::open(
                &canonical_root,
                relative,
                bytes.len() as u64,
                &digest(bytes),
                "model_unavailable",
            )
            .unwrap();
            assert_eq!(
                VerifiedFile::open(
                    &canonical_root,
                    relative,
                    bytes.len() as u64 + 1,
                    &digest(bytes),
                    "model_unavailable",
                )
                .unwrap_err()
                .code(),
                "identity_mismatch"
            );

            let outside = root.path().join("outside.bin");
            fs::write(&outside, bytes).unwrap();
            let linked = model_dir.join("linked.bin");
            symlink(&outside, &linked).unwrap();
            assert!(resolve_regular_file(
                &canonical_root,
                Path::new("models/whisper/tiny/linked.bin"),
                "model_unavailable"
            )
            .is_err());
        }

        #[test]
        fn streaming_resampler_preserves_endpoints_and_rate() {
            let mut resampler = LinearResampler::new(48_000, 16_000).unwrap();
            for index in 0..48_000 {
                resampler.push(index as f32 / 48_000.0).unwrap();
            }
            let output = resampler.finish();
            assert_eq!(output.len(), 16_000);
            assert_eq!(output[0], 0.0);
            assert!((output[15_999] - 47_997.0 / 48_000.0).abs() < 0.00001);

            let mut upsample = LinearResampler::new(8_000, 16_000).unwrap();
            upsample.push(0.0).unwrap();
            upsample.push(1.0).unwrap();
            assert_eq!(upsample.finish(), vec![0.0, 0.5, 1.0]);
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use apple_silicon::APP_DATA_ROOT_ENV;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn run_once() -> Result<(), WorkerError> {
    apple_silicon::run_once()
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub fn run_once() -> Result<(), WorkerError> {
    Err(WorkerError::new("unsupported_platform"))
}
