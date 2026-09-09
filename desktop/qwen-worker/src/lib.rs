//! EchoWall's independent Qwen3-ASR + forced-alignment crash boundary.
//!
//! The process handles one bounded request from stdin, reads exact App-owned
//! model/audio identities, emits one bounded response to stdout, and exits. It
//! owns no queue, credential, provider, downloader, listener, or persistence.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerError {
    code: &'static str,
}

impl WorkerError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local Qwen worker failed")
    }
}

impl std::error::Error for WorkerError {}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple_silicon {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::fs::{self, File, Metadata};
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::path::{Component, Path, PathBuf};

    use echowall_local_qwen_protocol::{
        decode_request, encode_response, LocalQwenAlignedWord, LocalQwenModelFileIdentity,
        LocalQwenRequest, LocalQwenResponse, LocalQwenSegment, LOCAL_QWEN_LEGACY_CHUNK_POLICY,
        LOCAL_QWEN_PROTOCOL_VERSION, MAX_LOCAL_QWEN_REQUEST_BYTES,
    };
    use qwen_asr::context::QwenCtx;
    use qwen_asr::transcribe;
    use sha2::{Digest, Sha256};
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    use super::WorkerError;

    pub const APP_DATA_ROOT_ENV: &str = "ECHOWALL_APP_DATA_ROOT";
    const TARGET_SAMPLE_RATE: u32 = 16_000;
    const MAX_CHANNELS: usize = 64;
    const MAX_SOURCE_SAMPLE_RATE: u32 = 384_000;
    const MAX_AUDIO_PACKETS: u64 = 4_000_000;
    const LANGUAGE_FRAME_SAMPLES: usize = 320;
    const LANGUAGE_SILENCE_FRAMES: usize = 30;
    const LANGUAGE_SILENCE_RMS: f32 = 0.003;
    const MIN_LANGUAGE_CHUNK_SAMPLES: usize = 8_000;
    const MAX_LANGUAGE_CHUNKS: usize = 1_000;

    pub fn run_once() -> Result<(), WorkerError> {
        if std::env::args_os().len() != 1 {
            return Err(WorkerError::new("arguments_forbidden"));
        }
        let root = std::env::var_os(APP_DATA_ROOT_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| WorkerError::new("root_missing"))?;
        let root = validate_root(&root)?;
        let request = read_request(io::stdin().lock())?;

        let asr_relative = PathBuf::from("models")
            .join("qwen")
            .join("asr")
            .join(&request.asr_model_id);
        let asr = VerifiedModelSet::open(
            &root,
            &asr_relative,
            &request.asr_model_files,
            "model_unavailable",
        )?;
        let aligner_relative = PathBuf::from("models")
            .join("qwen")
            .join("aligner")
            .join(&request.aligner_model_id);
        let aligner = VerifiedModelSet::open(
            &root,
            &aligner_relative,
            &request.aligner_model_files,
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
        asr.verify_unchanged()?;
        aligner.verify_unchanged()?;
        let response = transcribe(&asr, &aligner, &samples, &request)?;
        audio.verify_unchanged()?;
        asr.verify_unchanged()?;
        aligner.verify_unchanged()?;

        let encoded =
            encode_response(&response, &request).map_err(|_| WorkerError::new("invalid_output"))?;
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&encoded)
            .and_then(|()| stdout.flush())
            .map_err(|_| WorkerError::new("output_failed"))
    }

    fn read_request(mut reader: impl Read) -> Result<LocalQwenRequest, WorkerError> {
        let maximum = u64::try_from(MAX_LOCAL_QWEN_REQUEST_BYTES)
            .map_err(|_| WorkerError::new("invalid_limit"))?;
        let mut bytes = Vec::with_capacity(MAX_LOCAL_QWEN_REQUEST_BYTES.min(8 * 1024));
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
            if hash_file(&mut file)? != expected_sha256 {
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

    #[derive(Debug)]
    struct VerifiedModelSet {
        directory: PathBuf,
        files: Vec<VerifiedFile>,
    }

    impl VerifiedModelSet {
        fn open(
            root: &Path,
            relative_directory: &Path,
            identities: &[LocalQwenModelFileIdentity],
            error_code: &'static str,
        ) -> Result<Self, WorkerError> {
            let directory = resolve_directory(root, relative_directory, error_code)?;
            let expected: HashSet<&str> = identities
                .iter()
                .map(|identity| identity.relative_path.as_str())
                .collect();
            let mut actual = HashSet::new();
            for entry in fs::read_dir(&directory).map_err(|_| WorkerError::new(error_code))? {
                let entry = entry.map_err(|_| WorkerError::new(error_code))?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| WorkerError::new(error_code))?;
                let metadata = entry
                    .file_type()
                    .map_err(|_| WorkerError::new(error_code))?;
                if !metadata.is_file() || !actual.insert(name) {
                    return Err(WorkerError::new(error_code));
                }
            }
            if actual.len() != expected.len()
                || !actual.iter().all(|path| expected.contains(path.as_str()))
            {
                return Err(WorkerError::new(error_code));
            }
            let mut files = Vec::with_capacity(identities.len());
            for identity in identities {
                files.push(VerifiedFile::open(
                    root,
                    &relative_directory.join(&identity.relative_path),
                    identity.size_bytes,
                    &identity.sha256,
                    error_code,
                )?);
            }
            Ok(Self { directory, files })
        }

        fn verify_unchanged(&self) -> Result<(), WorkerError> {
            for file in &self.files {
                file.verify_unchanged()?;
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

    fn resolve_directory(
        root: &Path,
        relative: &Path,
        error_code: &'static str,
    ) -> Result<PathBuf, WorkerError> {
        resolve_path(root, relative, false, error_code)
    }

    fn resolve_regular_file(
        root: &Path,
        relative: &Path,
        error_code: &'static str,
    ) -> Result<PathBuf, WorkerError> {
        resolve_path(root, relative, true, error_code)
    }

    fn resolve_path(
        root: &Path,
        relative: &Path,
        final_is_file: bool,
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
        let mut candidate = root.to_path_buf();
        for (index, component) in components.iter().enumerate() {
            candidate.push(component);
            let metadata =
                fs::symlink_metadata(&candidate).map_err(|_| WorkerError::new(error_code))?;
            let final_component = index + 1 == components.len();
            if metadata.file_type().is_symlink()
                || !final_component && !metadata.is_dir()
                || final_component && final_is_file && !metadata.is_file()
                || final_component && !final_is_file && !metadata.is_dir()
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
        request: &LocalQwenRequest,
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

    fn samples_to_ms(samples: usize) -> u64 {
        u64::try_from(samples)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000)
            / u64::from(TARGET_SAMPLE_RATE)
    }

    fn multilingual_chunk_ranges(
        samples: &[f32],
        maximum_chunk_ms: u64,
        split_search_ms: u64,
    ) -> Result<Vec<(usize, usize)>, WorkerError> {
        if samples.is_empty() {
            return Err(WorkerError::new("decode_failed"));
        }
        let maximum_chunk_samples =
            usize::try_from(maximum_chunk_ms.saturating_mul(u64::from(TARGET_SAMPLE_RATE)) / 1_000)
                .map_err(|_| WorkerError::new("inference_limit"))?;
        let split_search_samples =
            usize::try_from(split_search_ms.saturating_mul(u64::from(TARGET_SAMPLE_RATE)) / 1_000)
                .map_err(|_| WorkerError::new("inference_limit"))?;
        if maximum_chunk_samples < MIN_LANGUAGE_CHUNK_SAMPLES.saturating_mul(2) {
            return Err(WorkerError::new("inference_limit"));
        }

        let mut boundaries = vec![0_usize];
        let mut silence_start = None;
        let frame_count = samples.len().div_ceil(LANGUAGE_FRAME_SAMPLES);
        for frame in 0..frame_count {
            let start = frame.saturating_mul(LANGUAGE_FRAME_SAMPLES);
            let end = start
                .saturating_add(LANGUAGE_FRAME_SAMPLES)
                .min(samples.len());
            let silent = rms(&samples[start..end]) <= LANGUAGE_SILENCE_RMS;
            match (silence_start, silent) {
                (None, true) => silence_start = Some(frame),
                (Some(run_start), false) => {
                    push_silence_boundary(&mut boundaries, run_start, frame, samples.len());
                    silence_start = None;
                }
                _ => {}
            }
        }
        boundaries.push(samples.len());

        let mut ranges = Vec::new();
        for pair in boundaries.windows(2) {
            let mut start = pair[0];
            let end = pair[1];
            while end.saturating_sub(start) > maximum_chunk_samples {
                let target = start.saturating_add(maximum_chunk_samples);
                let split = quiet_split_point(samples, start, end, target, split_search_samples);
                if split <= start || split >= end {
                    return Err(WorkerError::new("inference_limit"));
                }
                ranges.push((start, split));
                start = split;
            }
            if end > start {
                ranges.push((start, end));
            }
            if ranges.len() > MAX_LANGUAGE_CHUNKS {
                return Err(WorkerError::new("inference_limit"));
            }
        }
        if ranges.is_empty()
            || ranges.first().map(|range| range.0) != Some(0)
            || ranges.last().map(|range| range.1) != Some(samples.len())
            || ranges.windows(2).any(|pair| pair[0].1 != pair[1].0)
        {
            return Err(WorkerError::new("inference_limit"));
        }
        Ok(ranges)
    }

    fn push_silence_boundary(
        boundaries: &mut Vec<usize>,
        silence_start_frame: usize,
        silence_end_frame: usize,
        total_samples: usize,
    ) {
        if silence_end_frame.saturating_sub(silence_start_frame) < LANGUAGE_SILENCE_FRAMES {
            return;
        }
        let run_start = silence_start_frame.saturating_mul(LANGUAGE_FRAME_SAMPLES);
        let run_end = silence_end_frame
            .saturating_mul(LANGUAGE_FRAME_SAMPLES)
            .min(total_samples);
        let candidate = run_start.saturating_add(run_end.saturating_sub(run_start) / 2);
        let previous = *boundaries.last().unwrap_or(&0);
        if candidate.saturating_sub(previous) >= MIN_LANGUAGE_CHUNK_SAMPLES
            && total_samples.saturating_sub(candidate) >= MIN_LANGUAGE_CHUNK_SAMPLES
        {
            boundaries.push(candidate);
        }
    }

    fn quiet_split_point(
        samples: &[f32],
        range_start: usize,
        range_end: usize,
        target: usize,
        search_samples: usize,
    ) -> usize {
        let minimum = range_start.saturating_add(MIN_LANGUAGE_CHUNK_SAMPLES);
        let maximum = range_end.saturating_sub(MIN_LANGUAGE_CHUNK_SAMPLES);
        // The request's chunk duration is a hard protocol ceiling. Search for
        // a quiet boundary only before that ceiling; choosing a quieter point
        // after `target` used to produce nominal 30-second chunks as long as
        // 33 seconds.
        let upper = target.min(maximum);
        let lower = upper.saturating_sub(search_samples).max(minimum);
        let window = LANGUAGE_FRAME_SAMPLES.saturating_mul(5);
        let step = LANGUAGE_FRAME_SAMPLES;
        if lower >= upper || upper.saturating_sub(lower) < window {
            return target.clamp(minimum, maximum);
        }
        let mut best = target.clamp(lower, upper);
        let mut best_energy = f32::INFINITY;
        let mut position = lower;
        while position.saturating_add(window) <= upper {
            let energy = rms(&samples[position..position + window]);
            if energy < best_energy {
                best_energy = energy;
                best = position.saturating_add(window / 2);
            }
            position = position.saturating_add(step);
        }
        best.clamp(minimum, maximum)
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum = samples.iter().fold(0.0_f64, |total, sample| {
            total + f64::from(*sample) * f64::from(*sample)
        });
        (sum / samples.len() as f64).sqrt() as f32
    }

    fn transcribe(
        asr: &VerifiedModelSet,
        aligner: &VerifiedModelSet,
        samples: &[f32],
        request: &LocalQwenRequest,
    ) -> Result<LocalQwenResponse, WorkerError> {
        // qwen-asr otherwise persists a derived INT8 startup cache beside the
        // source model. EchoWall's model directory is immutable and every
        // inference worker is one-shot, so force the library's documented
        // no-sidecar path regardless of the caller environment.
        std::env::set_var("QWEN_ASR_SIDECAR", "0");
        qwen_asr::kernels::set_verbose(0);
        qwen_asr::kernels::set_threads(qwen_asr::kernels::get_default_threads());
        let asr_dir = asr
            .directory
            .to_str()
            .ok_or_else(|| WorkerError::new("model_unavailable"))?;
        let aligner_dir = aligner
            .directory
            .to_str()
            .ok_or_else(|| WorkerError::new("model_unavailable"))?;
        let mut asr_context =
            QwenCtx::load(asr_dir).ok_or_else(|| WorkerError::new("model_load_failed"))?;
        let mut aligner_context =
            QwenCtx::load(aligner_dir).ok_or_else(|| WorkerError::new("model_load_failed"))?;
        asr.verify_unchanged()?;
        aligner.verify_unchanged()?;
        asr_context.segment_sec = request.chunk_duration_ms as f32 / 1_000.0;
        asr_context.search_sec = request.split_search_ms as f32 / 1_000.0;
        asr_context.past_text_conditioning = false;
        asr_context.skip_silence = false;
        if let Some(language) = request.language.as_deref() {
            asr_context
                .set_force_language(language)
                .map_err(|_| WorkerError::new("unsupported_language"))?;
        } else {
            asr_context.want_language_detection = true;
        }

        let ranges = if request.chunk_policy == LOCAL_QWEN_LEGACY_CHUNK_POLICY {
            vec![(0, samples.len())]
        } else {
            multilingual_chunk_ranges(samples, request.chunk_duration_ms, request.split_search_ms)?
        };
        let mut segments = Vec::new();
        for (range_start, range_end) in ranges {
            if request.language.is_none() {
                // `transcribe_full` detects only once per invocation. Reset the
                // public language header state at each speech-bounded range so
                // a preceding English utterance cannot translate the following
                // Mandarin utterance (or vice versa).
                asr_context.detected_language = None;
                asr_context.want_language_detection = true;
                asr_context.prompt_tokens_ready = false;
            }
            let result = transcribe::transcribe_full(
                &mut asr_context,
                Some(&mut aligner_context),
                &samples[range_start..range_end],
                None,
            )
            .ok_or_else(|| WorkerError::new("inference_failed"))?;
            let range_start_ms = samples_to_ms(range_start);
            let range_end_ms = samples_to_ms(range_end).min(request.audio_duration_ms);
            let reported_language = request
                .language
                .clone()
                .or_else(|| (!result.language.is_empty()).then_some(result.language.clone()));
            for segment in result.segments {
                let segment_start_ms = range_start_ms
                    .saturating_add(segment.start_ms)
                    .min(range_end_ms);
                let segment_end_ms = range_start_ms
                    .saturating_add(segment.end_ms)
                    .min(range_end_ms);
                segments.push(LocalQwenSegment {
                    start_ms: segment_start_ms,
                    end_ms: segment_end_ms,
                    language: reported_language.clone(),
                    text: segment.text,
                    words: segment
                        .words
                        .into_iter()
                        .map(|word| LocalQwenAlignedWord {
                            start_ms: range_start_ms
                                .saturating_add(word.start_ms)
                                .min(segment_end_ms),
                            // The aligner's 80ms grid can round a final word just
                            // beyond its verified source range. Clamp only to
                            // that range; never reorder or extend into the next.
                            end_ms: range_start_ms
                                .saturating_add(word.end_ms)
                                .min(segment_end_ms),
                            text: word.word,
                        })
                        .collect(),
                });
            }
        }
        let response = LocalQwenResponse {
            schema_version: LOCAL_QWEN_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            runtime_id: request.runtime_id.clone(),
            asr_model_id: request.asr_model_id.clone(),
            asr_model_revision: request.asr_model_revision.clone(),
            aligner_model_id: request.aligner_model_id.clone(),
            aligner_model_revision: request.aligner_model_revision.clone(),
            audio_sha256: request.audio_sha256.clone(),
            chunk_policy: request.chunk_policy.clone(),
            segments,
        };
        response
            .validate_against(request)
            .map_err(|error| WorkerError::new(error.code()))?;
        Ok(response)
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::symlink;

        use echowall_local_qwen_protocol::{
            encode_request, LocalQwenModelFileIdentity, LocalQwenRequest, LOCAL_QWEN_ALIGNER_FILES,
            LOCAL_QWEN_ALIGNER_MODEL_ID, LOCAL_QWEN_ALIGNER_REVISION, LOCAL_QWEN_ASR_FILES,
            LOCAL_QWEN_ASR_MODEL_ID, LOCAL_QWEN_ASR_REVISION, LOCAL_QWEN_CHUNK_DURATION_MS,
            LOCAL_QWEN_CHUNK_POLICY, LOCAL_QWEN_PROTOCOL_VERSION, LOCAL_QWEN_RUNTIME_ID,
            LOCAL_QWEN_SPLIT_SEARCH_MS,
        };
        use tempfile::TempDir;

        use super::*;

        fn digest(bytes: &[u8]) -> String {
            hex::encode(Sha256::digest(bytes))
        }

        fn identities(paths: &[&str], bytes: &[u8]) -> Vec<LocalQwenModelFileIdentity> {
            paths
                .iter()
                .map(|path| LocalQwenModelFileIdentity {
                    relative_path: (*path).to_owned(),
                    sha256: digest(bytes),
                    size_bytes: bytes.len() as u64,
                })
                .collect()
        }

        fn request() -> LocalQwenRequest {
            LocalQwenRequest {
                schema_version: LOCAL_QWEN_PROTOCOL_VERSION,
                recording_id: "018f92d8-6ad4-7dc1-8e28-8b020d2942cb".parse().unwrap(),
                runtime_id: LOCAL_QWEN_RUNTIME_ID.to_owned(),
                asr_model_id: LOCAL_QWEN_ASR_MODEL_ID.to_owned(),
                asr_model_revision: LOCAL_QWEN_ASR_REVISION.to_owned(),
                asr_model_files: identities(&LOCAL_QWEN_ASR_FILES, b"model"),
                aligner_model_id: LOCAL_QWEN_ALIGNER_MODEL_ID.to_owned(),
                aligner_model_revision: LOCAL_QWEN_ALIGNER_REVISION.to_owned(),
                aligner_model_files: identities(&LOCAL_QWEN_ALIGNER_FILES, b"model"),
                audio_relative_path: "derived/mixed.wav".to_owned(),
                audio_sha256: "b".repeat(64),
                audio_size_bytes: 4,
                audio_duration_ms: 1_000,
                language: Some("zh".to_owned()),
                chunk_policy: LOCAL_QWEN_CHUNK_POLICY.to_owned(),
                chunk_duration_ms: LOCAL_QWEN_CHUNK_DURATION_MS,
                split_search_ms: LOCAL_QWEN_SPLIT_SEARCH_MS,
            }
        }

        #[test]
        fn multilingual_ranges_split_long_silence_without_losing_samples() {
            let mut samples = vec![0.1_f32; 16_000];
            samples.extend(vec![0.0_f32; 16_000]);
            samples.extend(vec![-0.1_f32; 16_000]);
            let ranges = multilingual_chunk_ranges(
                &samples,
                LOCAL_QWEN_CHUNK_DURATION_MS,
                LOCAL_QWEN_SPLIT_SEARCH_MS,
            )
            .unwrap();
            assert_eq!(ranges.len(), 2);
            assert_eq!(ranges[0].0, 0);
            assert_eq!(ranges[1].1, samples.len());
            assert_eq!(ranges[0].1, ranges[1].0);
            assert!(ranges[0].1.abs_diff(24_000) <= LANGUAGE_FRAME_SAMPLES);
        }

        #[test]
        fn multilingual_ranges_ignore_short_pause_and_bound_long_audio() {
            let mut short_pause = vec![0.1_f32; 16_000];
            short_pause.extend(vec![0.0_f32; LANGUAGE_FRAME_SAMPLES * 20]);
            short_pause.extend(vec![0.1_f32; 16_000]);
            assert_eq!(
                multilingual_chunk_ranges(
                    &short_pause,
                    LOCAL_QWEN_CHUNK_DURATION_MS,
                    LOCAL_QWEN_SPLIT_SEARCH_MS,
                )
                .unwrap(),
                [(0, short_pause.len())]
            );

            let long = vec![0.1_f32; 31 * TARGET_SAMPLE_RATE as usize];
            let ranges = multilingual_chunk_ranges(
                &long,
                LOCAL_QWEN_CHUNK_DURATION_MS,
                LOCAL_QWEN_SPLIT_SEARCH_MS,
            )
            .unwrap();
            assert_eq!(ranges.len(), 2);
            assert!(ranges.iter().all(|(start, end)| {
                end.saturating_sub(*start) <= 30 * TARGET_SAMPLE_RATE as usize
            }));
            assert_eq!(ranges[0].0, 0);
            assert_eq!(ranges[0].1, ranges[1].0);
            assert_eq!(ranges[1].1, long.len());
        }

        #[test]
        fn multilingual_ranges_never_choose_quiet_point_after_duration_ceiling() {
            let mut audio = vec![0.1_f32; 61 * TARGET_SAMPLE_RATE as usize];
            let quiet_start = 31 * TARGET_SAMPLE_RATE as usize;
            let quiet_end = 32 * TARGET_SAMPLE_RATE as usize;
            audio[quiet_start..quiet_end].fill(0.0);
            let ranges = multilingual_chunk_ranges(
                &audio,
                LOCAL_QWEN_CHUNK_DURATION_MS,
                LOCAL_QWEN_SPLIT_SEARCH_MS,
            )
            .unwrap();
            assert!(ranges.iter().all(|(start, end)| {
                end.saturating_sub(*start) <= 30 * TARGET_SAMPLE_RATE as usize
            }));
            assert_eq!(ranges[0].0, 0);
            assert_eq!(ranges.last().map(|range| range.1), Some(audio.len()));
            assert!(ranges.windows(2).all(|pair| pair[0].1 == pair[1].0));
        }

        #[test]
        fn request_reader_is_bounded_and_closed() {
            let encoded = encode_request(&request()).unwrap();
            assert_eq!(read_request(encoded.as_slice()).unwrap(), request());
            assert_eq!(
                read_request(vec![b' '; MAX_LOCAL_QWEN_REQUEST_BYTES + 1].as_slice())
                    .unwrap_err()
                    .code(),
                "invalid_request"
            );
        }

        #[test]
        fn model_set_requires_exact_regular_hash_bound_files() {
            let root = TempDir::new().unwrap();
            let canonical_root = validate_root(root.path()).unwrap();
            let relative = Path::new("models/qwen/asr/qwen3-asr-1.7b");
            let directory = root.path().join(relative);
            fs::create_dir_all(&directory).unwrap();
            let bytes = b"model";
            for path in LOCAL_QWEN_ASR_FILES {
                fs::write(directory.join(path), bytes).unwrap();
            }
            let identities = identities(&LOCAL_QWEN_ASR_FILES, bytes);
            VerifiedModelSet::open(&canonical_root, relative, &identities, "model_unavailable")
                .unwrap();
            fs::write(directory.join("unexpected.json"), b"{}").unwrap();
            assert!(VerifiedModelSet::open(
                &canonical_root,
                relative,
                &identities,
                "model_unavailable",
            )
            .is_err());
            fs::remove_file(directory.join("unexpected.json")).unwrap();
            fs::remove_file(directory.join(LOCAL_QWEN_ASR_FILES[0])).unwrap();
            let outside = root.path().join("outside");
            fs::write(&outside, bytes).unwrap();
            symlink(&outside, directory.join(LOCAL_QWEN_ASR_FILES[0])).unwrap();
            assert!(VerifiedModelSet::open(
                &canonical_root,
                relative,
                &identities,
                "model_unavailable",
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
