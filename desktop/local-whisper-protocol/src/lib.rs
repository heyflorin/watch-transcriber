//! Closed protocol shared by EchoWall's Rust job owner and its App-bundled,
//! one-shot native Whisper worker.
//!
//! The protocol carries bounded identities and App-owned relative paths. It
//! deliberately has no Tauri, network, credential, queue, or provider
//! dependency, so the worker cannot accidentally become another processing
//! tier.

use std::collections::HashSet;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

pub const LOCAL_WHISPER_PROTOCOL_VERSION: u32 = 1;
pub const MAX_LOCAL_WHISPER_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_LOCAL_WHISPER_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_LOCAL_WHISPER_MODEL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_LOCAL_WHISPER_AUDIO_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_LOCAL_WHISPER_AUDIO_DURATION_MS: u64 = 5 * 60 * 60 * 1_000;
pub const MAX_LOCAL_WHISPER_SEGMENTS: usize = 10_000;
pub const MAX_LOCAL_WHISPER_SEGMENT_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_LOCAL_WHISPER_TRANSCRIPT_TEXT_BYTES: usize = 12 * 1024 * 1024;
pub const LOCAL_DIARIZATION_PROTOCOL_VERSION: u32 = 2;
pub const LOCAL_DIARIZATION_LEGACY_PRESET: &str = "fluid-community-v1";
pub const LOCAL_DIARIZATION_QUALITY_PRESET: &str = "fluid-step015-embed040-v1";
pub const LOCAL_DIARIZATION_SPEAKERKIT_PRESET: &str = "speakerkit-pyannote-v3-exclusive-v1";
/// Explicit candidate runtime policy; retained v1 requests keep their behavior.
pub const LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET: &str =
    "speakerkit-pyannote-v3-exclusive-tail-context-v2";
pub const LOCAL_DIARIZATION_SPEAKERKIT_FILES: [&str; 29] = [
    "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/analytics/coremldata.bin",
    "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/coremldata.bin",
    "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/metadata.json",
    "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/model.mil",
    "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/weights/weight.bin",
    "speaker_clusterer/pyannote-v4/W32A32/README.txt",
    "speaker_embedder/pyannote-v3/W8A16/README.txt",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/analytics/coremldata.bin",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/coremldata.bin",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/metadata.json",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/model.mil",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/weights/weight.bin",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/analytics/coremldata.bin",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/coremldata.bin",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/metadata.json",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/model.mil",
    "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/weights/weight.bin",
    "speaker_segmenter/pyannote-v3/W32A32/README.txt",
    "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/analytics/coremldata.bin",
    "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/coremldata.bin",
    "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/metadata.json",
    "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/model.mil",
    "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/weights/weight.bin",
    "speaker_segmenter/pyannote-v3/W8A16/README.txt",
    "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/analytics/coremldata.bin",
    "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/coremldata.bin",
    "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/metadata.json",
    "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/model.mil",
    "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/weights/weight.bin",
];
pub const MAX_LOCAL_DIARIZATION_REQUEST_BYTES: usize = 256 * 1024;
pub const MAX_LOCAL_DIARIZATION_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_LOCAL_DIARIZATION_MODEL_FILES: usize = 64;
pub const MAX_LOCAL_DIARIZATION_MODEL_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_LOCAL_DIARIZATION_SEGMENTS: usize = 20_000;
pub const MAX_LOCAL_SPEAKERS: u32 = 16;
pub const MIN_LOCAL_DIARIZATION_CONFIDENCE_MILLI: u16 = 500;
pub const MAX_LOCAL_ALIGNED_WORDS: usize = 200_000;
pub const MAX_LOCAL_ALIGNED_WORD_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_SAFE_SAME_SPEAKER_BRIDGE_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalWhisperRequest {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub model_id: String,
    pub model_sha256: String,
    pub model_size_bytes: u64,
    pub audio_relative_path: String,
    pub audio_sha256: String,
    pub audio_size_bytes: u64,
    pub audio_duration_ms: u64,
    pub language: Option<String>,
}

impl LocalWhisperRequest {
    pub fn validate(&self) -> Result<(), LocalWhisperProtocolError> {
        if self.schema_version != LOCAL_WHISPER_PROTOCOL_VERSION {
            return Err(LocalWhisperProtocolError::new("unsupported_protocol"));
        }
        if !valid_model_id(&self.model_id)
            || !valid_sha256(&self.model_sha256)
            || self.model_size_bytes == 0
            || self.model_size_bytes > MAX_LOCAL_WHISPER_MODEL_BYTES
            || !valid_relative_path(&self.audio_relative_path)
            || !matches!(
                self.audio_relative_path
                    .rsplit_once('.')
                    .map(|(_, extension)| extension.to_ascii_lowercase()),
                Some(extension) if matches!(extension.as_str(), "wav" | "m4a" | "mp3")
            )
            || !valid_sha256(&self.audio_sha256)
            || self.audio_size_bytes == 0
            || self.audio_size_bytes >= MAX_LOCAL_WHISPER_AUDIO_BYTES
            || self.audio_duration_ms == 0
            || self.audio_duration_ms >= MAX_LOCAL_WHISPER_AUDIO_DURATION_MS
            || self
                .language
                .as_deref()
                .is_some_and(|value| !valid_language(value))
        {
            return Err(LocalWhisperProtocolError::new("invalid_request"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalWhisperSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalWhisperResponse {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub model_id: String,
    pub model_sha256: String,
    pub audio_sha256: String,
    pub language: String,
    pub segments: Vec<LocalWhisperSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalTranscriptBackend {
    WhisperLocal,
    QwenLocal,
}

impl LocalTranscriptBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::WhisperLocal => "whisper_local",
            Self::QwenLocal => "qwen_local",
        }
    }
}

impl LocalWhisperResponse {
    pub fn validate_against(
        &self,
        request: &LocalWhisperRequest,
    ) -> Result<(), LocalWhisperProtocolError> {
        request.validate()?;
        if self.schema_version != LOCAL_WHISPER_PROTOCOL_VERSION
            || self.recording_id != request.recording_id
            || self.model_id != request.model_id
            || self.model_sha256 != request.model_sha256
            || self.audio_sha256 != request.audio_sha256
            || !valid_language(&self.language)
            || request
                .language
                .as_ref()
                .is_some_and(|language| language != &self.language)
            || self.segments.is_empty()
            || self.segments.len() > MAX_LOCAL_WHISPER_SEGMENTS
        {
            return Err(LocalWhisperProtocolError::new("invalid_response"));
        }
        let mut previous_end = 0_u64;
        let mut text_bytes = 0_usize;
        for segment in &self.segments {
            if segment.start_ms < previous_end
                || segment.end_ms <= segment.start_ms
                || segment.end_ms > request.audio_duration_ms
                || segment.text.trim().is_empty()
                || segment.text.len() > MAX_LOCAL_WHISPER_SEGMENT_TEXT_BYTES
                || segment.text.chars().any(|character| character == '\0')
                || segment
                    .speaker_id
                    .as_deref()
                    .is_some_and(|speaker| !valid_local_speaker_id(speaker))
            {
                return Err(LocalWhisperProtocolError::new("invalid_response"));
            }
            previous_end = segment.end_ms;
            text_bytes = text_bytes
                .checked_add(segment.text.len())
                .ok_or_else(|| LocalWhisperProtocolError::new("response_too_large"))?;
            if text_bytes > MAX_LOCAL_WHISPER_TRANSCRIPT_TEXT_BYTES {
                return Err(LocalWhisperProtocolError::new("response_too_large"));
            }
        }
        Ok(())
    }

    pub fn transcript_json(
        &self,
        request: &LocalWhisperRequest,
    ) -> Result<Value, LocalWhisperProtocolError> {
        self.transcript_json_for_backend(request, LocalTranscriptBackend::WhisperLocal)
    }

    pub fn transcript_json_for_backend(
        &self,
        request: &LocalWhisperRequest,
        backend: LocalTranscriptBackend,
    ) -> Result<Value, LocalWhisperProtocolError> {
        self.validate_against(request)?;
        Ok(Value::Array(
            self.segments
                .iter()
                .map(|segment| {
                    json!({
                        "start_time": segment.start_ms,
                        "end_time": segment.end_ms,
                        "speaker": { "id": segment.speaker_id.as_deref().unwrap_or("local_unknown") },
                        "content": segment.text.as_str(),
                        "stt_backend": backend.as_str(),
                        "model_id": self.model_id.as_str(),
                        "language": self.language.as_str(),
                    })
                })
                .collect(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalModelFileIdentity {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDiarizationRequest {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub pack_id: String,
    pub quality_preset: String,
    pub model_files: Vec<LocalModelFileIdentity>,
    pub audio_relative_path: String,
    pub audio_sha256: String,
    pub audio_size_bytes: u64,
    pub audio_duration_ms: u64,
    pub expected_speaker_count: Option<u32>,
}

impl LocalDiarizationRequest {
    pub fn validate(&self) -> Result<(), LocalWhisperProtocolError> {
        if self.schema_version != LOCAL_DIARIZATION_PROTOCOL_VERSION {
            return Err(LocalWhisperProtocolError::new("unsupported_protocol"));
        }
        let valid_pack_preset = match self.quality_preset.as_str() {
            LOCAL_DIARIZATION_LEGACY_PRESET | LOCAL_DIARIZATION_QUALITY_PRESET => {
                self.pack_id == "fluid-v1"
            }
            LOCAL_DIARIZATION_SPEAKERKIT_PRESET
            | LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET => self.pack_id == "speakerkit-v1",
            _ => false,
        };
        if !valid_model_id(&self.pack_id)
            || !valid_pack_preset
            || self.model_files.is_empty()
            || self.model_files.len() > MAX_LOCAL_DIARIZATION_MODEL_FILES
            || !valid_relative_audio(&self.audio_relative_path)
            || !valid_sha256(&self.audio_sha256)
            || self.audio_size_bytes == 0
            || self.audio_size_bytes >= MAX_LOCAL_WHISPER_AUDIO_BYTES
            || self.audio_duration_ms == 0
            || self.audio_duration_ms >= MAX_LOCAL_WHISPER_AUDIO_DURATION_MS
            || self
                .expected_speaker_count
                .is_some_and(|count| count == 0 || count > MAX_LOCAL_SPEAKERS)
        {
            return Err(LocalWhisperProtocolError::new("invalid_request"));
        }
        let mut paths = HashSet::new();
        let mut total_bytes = 0_u64;
        for file in &self.model_files {
            if !valid_relative_path(&file.relative_path)
                || !paths.insert(&file.relative_path)
                || !valid_sha256(&file.sha256)
                || file.size_bytes == 0
            {
                return Err(LocalWhisperProtocolError::new("invalid_request"));
            }
            total_bytes = total_bytes
                .checked_add(file.size_bytes)
                .ok_or_else(|| LocalWhisperProtocolError::new("request_too_large"))?;
            if total_bytes > MAX_LOCAL_DIARIZATION_MODEL_BYTES {
                return Err(LocalWhisperProtocolError::new("request_too_large"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDiarizationSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_slot: u32,
    pub confidence_milli: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDiarizationResponse {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub pack_id: String,
    pub quality_preset: String,
    pub audio_sha256: String,
    pub speaker_count: u32,
    pub segments: Vec<LocalDiarizationSegment>,
}

impl LocalDiarizationResponse {
    pub fn validate_against(
        &self,
        request: &LocalDiarizationRequest,
    ) -> Result<(), LocalWhisperProtocolError> {
        request.validate()?;
        if self.schema_version != LOCAL_DIARIZATION_PROTOCOL_VERSION
            || self.recording_id != request.recording_id
            || self.pack_id != request.pack_id
            || self.quality_preset != request.quality_preset
            || self.audio_sha256 != request.audio_sha256
            || self.speaker_count == 0
            || self.speaker_count > MAX_LOCAL_SPEAKERS
            || request
                .expected_speaker_count
                .is_some_and(|count| count != self.speaker_count)
            || self.segments.is_empty()
            || self.segments.len() > MAX_LOCAL_DIARIZATION_SEGMENTS
        {
            return Err(LocalWhisperProtocolError::new("invalid_response"));
        }
        let mut previous_end = 0_u64;
        let mut seen_speakers = HashSet::new();
        for segment in &self.segments {
            if segment.start_ms < previous_end
                || segment.end_ms <= segment.start_ms
                || segment.end_ms > request.audio_duration_ms
                || segment.speaker_slot == 0
                || segment.speaker_slot > self.speaker_count
                || segment.confidence_milli > 1_000
            {
                return Err(LocalWhisperProtocolError::new("invalid_response"));
            }
            previous_end = segment.end_ms;
            seen_speakers.insert(segment.speaker_slot);
        }
        if seen_speakers.len() != self.speaker_count as usize
            || !(1..=self.speaker_count).all(|slot| seen_speakers.contains(&slot))
        {
            return Err(LocalWhisperProtocolError::new("invalid_response"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiarizationMergeStats {
    pub assigned_segments: usize,
    pub unknown_segments: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAlignedWord {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlignedWordMergeStats {
    pub lexical_words: usize,
    pub overlap_assigned_words: usize,
    pub point_assigned_words: usize,
    pub bridge_assigned_words: usize,
    pub unknown_lexical_words: usize,
    pub attached_punctuation_tokens: usize,
    pub interpolated_zero_duration_words: usize,
}

/// Assign forced-aligned lexical words to validated diarization intervals.
///
/// Direct assignment requires a unique positive maximum overlap above the
/// confidence floor. An uncovered run is bridged only when the immediately
/// adjacent accepted intervals have the same speaker and their whole gap is at
/// most one second. Every other lexical word remains explicitly unresolved.
pub fn merge_aligned_words_with_diarization(
    words: &mut [LocalAlignedWord],
    diarization: &LocalDiarizationResponse,
    diarization_request: &LocalDiarizationRequest,
) -> Result<AlignedWordMergeStats, LocalWhisperProtocolError> {
    diarization.validate_against(diarization_request)?;
    validate_aligned_words(words, diarization_request.audio_duration_ms)?;
    let punctuation: Vec<bool> = words
        .iter()
        .map(|word| punctuation_only(&word.text))
        .collect();
    let interpolated_zero_duration_words =
        interpolate_bounded_zero_duration_runs(words, &punctuation);

    let mut overlap_assigned_words = 0_usize;
    let mut point_assigned_words = 0_usize;
    for (word, is_punctuation) in words.iter_mut().zip(&punctuation) {
        word.speaker_id = None;
        if *is_punctuation {
            continue;
        }
        if word.end_ms <= word.start_ms {
            if let Some((speaker, point_end)) = strict_point_speaker(diarization, word.start_ms) {
                word.end_ms = point_end;
                word.speaker_id = Some(format!("local_speaker_{speaker:02}"));
                point_assigned_words = point_assigned_words.saturating_add(1);
            }
            continue;
        }
        let mut overlap_by_speaker = vec![0_u64; diarization.speaker_count as usize];
        for interval in &diarization.segments {
            if interval.end_ms <= word.start_ms {
                continue;
            }
            if interval.start_ms >= word.end_ms {
                break;
            }
            if interval.confidence_milli < MIN_LOCAL_DIARIZATION_CONFIDENCE_MILLI {
                continue;
            }
            let overlap = word.end_ms.min(interval.end_ms) - word.start_ms.max(interval.start_ms);
            let slot = interval.speaker_slot as usize - 1;
            overlap_by_speaker[slot] = overlap_by_speaker[slot].saturating_add(overlap);
        }
        let mut ranked: Vec<(usize, u64)> = overlap_by_speaker.into_iter().enumerate().collect();
        ranked.sort_by_key(|(_, overlap)| std::cmp::Reverse(*overlap));
        let (speaker, top_overlap) = ranked[0];
        let second_overlap = ranked.get(1).map(|(_, overlap)| *overlap).unwrap_or(0);
        if top_overlap > 0 && top_overlap > second_overlap {
            word.speaker_id = Some(format!("local_speaker_{:02}", speaker + 1));
            overlap_assigned_words = overlap_assigned_words.saturating_add(1);
        }
    }

    let mut bridge_assigned_words = 0_usize;
    let lexical_indices: Vec<usize> = punctuation
        .iter()
        .enumerate()
        .filter_map(|(index, punctuation)| (!punctuation).then_some(index))
        .collect();
    let mut position = 0_usize;
    while position < lexical_indices.len() {
        let first = lexical_indices[position];
        if words[first].speaker_id.is_some() {
            position += 1;
            continue;
        }
        let mut end_position = position + 1;
        while end_position < lexical_indices.len()
            && words[lexical_indices[end_position]].speaker_id.is_none()
        {
            end_position += 1;
        }
        let indices = &lexical_indices[position..end_position];
        let gap_start = indices
            .iter()
            .map(|index| words[*index].start_ms)
            .min()
            .unwrap_or(0);
        let gap_end = indices
            .iter()
            .map(|index| words[*index].end_ms)
            .max()
            .unwrap_or(gap_start);
        if let Some(speaker) = safe_bridge_speaker(diarization, gap_start, gap_end) {
            for index in indices {
                words[*index].speaker_id = Some(format!("local_speaker_{speaker:02}"));
                bridge_assigned_words = bridge_assigned_words.saturating_add(1);
            }
        }
        position = end_position;
    }

    let mut attached_punctuation_tokens = 0_usize;
    let mut preceding_speaker: Option<String> = None;
    for (word, is_punctuation) in words.iter_mut().zip(&punctuation) {
        if *is_punctuation {
            word.speaker_id.clone_from(&preceding_speaker);
            if word.speaker_id.is_some() {
                attached_punctuation_tokens = attached_punctuation_tokens.saturating_add(1);
            }
        } else {
            preceding_speaker.clone_from(&word.speaker_id);
        }
    }
    let lexical_words = punctuation.iter().filter(|value| !**value).count();
    let unknown_lexical_words = words
        .iter()
        .zip(&punctuation)
        .filter(|(word, punctuation)| !**punctuation && word.speaker_id.is_none())
        .count();
    Ok(AlignedWordMergeStats {
        lexical_words,
        overlap_assigned_words,
        point_assigned_words,
        bridge_assigned_words,
        unknown_lexical_words,
        attached_punctuation_tokens,
        interpolated_zero_duration_words,
    })
}

fn strict_point_speaker(
    diarization: &LocalDiarizationResponse,
    point_ms: u64,
) -> Option<(u32, u64)> {
    let mut speaker = None;
    let mut containing_end = None;
    for interval in diarization.segments.iter().filter(|interval| {
        interval.confidence_milli >= MIN_LOCAL_DIARIZATION_CONFIDENCE_MILLI
            && interval.start_ms < point_ms
            && point_ms < interval.end_ms
    }) {
        match speaker {
            None => {
                speaker = Some(interval.speaker_slot);
                containing_end = Some(interval.end_ms);
            }
            Some(existing) if existing == interval.speaker_slot => {
                containing_end = Some(containing_end?.min(interval.end_ms));
            }
            Some(_) => return None,
        }
    }
    let speaker = speaker?;
    let point_end = point_ms.checked_add(1)?.min(containing_end?);
    (point_end > point_ms).then_some((speaker, point_end))
}

fn validate_aligned_words(
    words: &[LocalAlignedWord],
    audio_duration_ms: u64,
) -> Result<(), LocalWhisperProtocolError> {
    if words.is_empty() || words.len() > MAX_LOCAL_ALIGNED_WORDS {
        return Err(LocalWhisperProtocolError::new("invalid_response"));
    }
    let mut previous_start = 0_u64;
    for word in words {
        if word.start_ms < previous_start
            || word.end_ms < word.start_ms
            || word.end_ms > audio_duration_ms
            || word.text.is_empty()
            || word.text.len() > MAX_LOCAL_ALIGNED_WORD_TEXT_BYTES
            || word.text.contains('\0')
            || word.speaker_id.is_some()
        {
            return Err(LocalWhisperProtocolError::new("invalid_response"));
        }
        previous_start = word.start_ms;
    }
    Ok(())
}

fn punctuation_only(value: &str) -> bool {
    value
        .chars()
        .all(|character| character.is_whitespace() || !character.is_alphanumeric())
}

fn interpolate_bounded_zero_duration_runs(
    words: &mut [LocalAlignedWord],
    punctuation: &[bool],
) -> usize {
    let lexical: Vec<usize> = punctuation
        .iter()
        .enumerate()
        .filter_map(|(index, punctuation)| (!punctuation).then_some(index))
        .collect();
    let mut interpolated = 0_usize;
    let mut position = 0_usize;
    while position < lexical.len() {
        let index = lexical[position];
        if words[index].end_ms > words[index].start_ms {
            position += 1;
            continue;
        }
        let run_start = position;
        while position < lexical.len() {
            let candidate = lexical[position];
            if words[candidate].end_ms > words[candidate].start_ms {
                break;
            }
            position += 1;
        }
        if run_start == 0 || position >= lexical.len() {
            continue;
        }
        let previous = lexical[run_start - 1];
        let next = lexical[position];
        if words[previous].end_ms > words[next].start_ms {
            continue;
        }
        let count = position - run_start;
        let span = words[next].start_ms - words[previous].end_ms;
        if span == 0 {
            continue;
        }
        for (offset, lexical_position) in (run_start..position).enumerate() {
            let word = lexical[lexical_position];
            // Qwen's aligner uses an 80ms grid, so a bounded run may contain
            // more lexical tokens than whole milliseconds. Floor starts and
            // ceil ends inside the two real anchors. Dense tokens may overlap
            // by 1ms, but every interval remains positive, monotonic, and
            // confined to the only evidence-backed time span.
            let count_u128 = count as u128;
            let span_u128 = span as u128;
            let start_offset = span_u128.saturating_mul(offset as u128) / count_u128;
            let end_numerator = span_u128.saturating_mul(offset as u128 + 1);
            let end_offset = end_numerator.saturating_add(count_u128 - 1) / count_u128;
            words[word].start_ms = words[previous]
                .end_ms
                .saturating_add(u64::try_from(start_offset).unwrap_or(u64::MAX));
            words[word].end_ms = words[previous]
                .end_ms
                .saturating_add(u64::try_from(end_offset).unwrap_or(u64::MAX));
            interpolated = interpolated.saturating_add(1);
        }
    }
    interpolated
}

fn safe_bridge_speaker(
    diarization: &LocalDiarizationResponse,
    gap_start: u64,
    gap_end: u64,
) -> Option<u32> {
    let accepted = diarization
        .segments
        .iter()
        .filter(|segment| segment.confidence_milli >= MIN_LOCAL_DIARIZATION_CONFIDENCE_MILLI);
    let before = accepted
        .clone()
        .filter(|segment| segment.end_ms <= gap_start)
        .max_by_key(|segment| segment.end_ms)?;
    let after = accepted
        .filter(|segment| segment.start_ms >= gap_end)
        .min_by_key(|segment| segment.start_ms)?;
    (before.speaker_slot == after.speaker_slot
        && after.start_ms.saturating_sub(before.end_ms) <= MAX_SAFE_SAME_SPEAKER_BRIDGE_MS)
        .then_some(before.speaker_slot)
}

pub fn merge_diarization(
    whisper: &mut LocalWhisperResponse,
    whisper_request: &LocalWhisperRequest,
    diarization: &LocalDiarizationResponse,
    diarization_request: &LocalDiarizationRequest,
) -> Result<DiarizationMergeStats, LocalWhisperProtocolError> {
    whisper.validate_against(whisper_request)?;
    diarization.validate_against(diarization_request)?;
    if whisper.recording_id != diarization.recording_id
        || whisper.audio_sha256 != diarization.audio_sha256
        || whisper_request.audio_duration_ms != diarization_request.audio_duration_ms
    {
        return Err(LocalWhisperProtocolError::new("identity_mismatch"));
    }
    let mut assigned_segments = 0_usize;
    for segment in &mut whisper.segments {
        let mut overlap_by_speaker = vec![0_u64; diarization.speaker_count as usize];
        for interval in &diarization.segments {
            if interval.end_ms <= segment.start_ms {
                continue;
            }
            if interval.start_ms >= segment.end_ms {
                break;
            }
            if interval.confidence_milli < MIN_LOCAL_DIARIZATION_CONFIDENCE_MILLI {
                continue;
            }
            let overlap =
                segment.end_ms.min(interval.end_ms) - segment.start_ms.max(interval.start_ms);
            overlap_by_speaker[interval.speaker_slot as usize - 1] =
                overlap_by_speaker[interval.speaker_slot as usize - 1].saturating_add(overlap);
        }
        let mut ranked: Vec<(usize, u64)> = overlap_by_speaker.into_iter().enumerate().collect();
        ranked.sort_by_key(|(_, overlap)| std::cmp::Reverse(*overlap));
        let (speaker, top_overlap) = ranked[0];
        let second_overlap = ranked.get(1).map(|(_, overlap)| *overlap).unwrap_or(0);
        let duration = segment.end_ms - segment.start_ms;
        if top_overlap >= 100
            && top_overlap.saturating_mul(10) >= duration.saturating_mul(3)
            && top_overlap > second_overlap
        {
            segment.speaker_id = Some(format!("local_speaker_{:02}", speaker + 1));
            assigned_segments = assigned_segments.saturating_add(1);
        } else {
            segment.speaker_id = None;
        }
    }
    whisper.validate_against(whisper_request)?;
    Ok(DiarizationMergeStats {
        assigned_segments,
        unknown_segments: whisper.segments.len().saturating_sub(assigned_segments),
    })
}

pub fn encode_request(request: &LocalWhisperRequest) -> Result<Vec<u8>, LocalWhisperProtocolError> {
    request.validate()?;
    encode_bounded(
        request,
        MAX_LOCAL_WHISPER_REQUEST_BYTES,
        "request_too_large",
    )
}

pub fn decode_request(bytes: &[u8]) -> Result<LocalWhisperRequest, LocalWhisperProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_WHISPER_REQUEST_BYTES {
        return Err(LocalWhisperProtocolError::new("request_too_large"));
    }
    let request: LocalWhisperRequest = serde_json::from_slice(bytes)
        .map_err(|_| LocalWhisperProtocolError::new("invalid_json"))?;
    request.validate()?;
    Ok(request)
}

pub fn encode_response(
    response: &LocalWhisperResponse,
    request: &LocalWhisperRequest,
) -> Result<Vec<u8>, LocalWhisperProtocolError> {
    response.validate_against(request)?;
    encode_bounded(
        response,
        MAX_LOCAL_WHISPER_RESPONSE_BYTES,
        "response_too_large",
    )
}

pub fn decode_response(
    bytes: &[u8],
    request: &LocalWhisperRequest,
) -> Result<LocalWhisperResponse, LocalWhisperProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_WHISPER_RESPONSE_BYTES {
        return Err(LocalWhisperProtocolError::new("response_too_large"));
    }
    let response: LocalWhisperResponse = serde_json::from_slice(bytes)
        .map_err(|_| LocalWhisperProtocolError::new("invalid_json"))?;
    response.validate_against(request)?;
    Ok(response)
}

pub fn encode_diarization_request(
    request: &LocalDiarizationRequest,
) -> Result<Vec<u8>, LocalWhisperProtocolError> {
    request.validate()?;
    encode_bounded(
        request,
        MAX_LOCAL_DIARIZATION_REQUEST_BYTES,
        "request_too_large",
    )
}

pub fn decode_diarization_request(
    bytes: &[u8],
) -> Result<LocalDiarizationRequest, LocalWhisperProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_DIARIZATION_REQUEST_BYTES {
        return Err(LocalWhisperProtocolError::new("request_too_large"));
    }
    let request: LocalDiarizationRequest = serde_json::from_slice(bytes)
        .map_err(|_| LocalWhisperProtocolError::new("invalid_json"))?;
    request.validate()?;
    Ok(request)
}

pub fn encode_diarization_response(
    response: &LocalDiarizationResponse,
    request: &LocalDiarizationRequest,
) -> Result<Vec<u8>, LocalWhisperProtocolError> {
    response.validate_against(request)?;
    encode_bounded(
        response,
        MAX_LOCAL_DIARIZATION_RESPONSE_BYTES,
        "response_too_large",
    )
}

pub fn decode_diarization_response(
    bytes: &[u8],
    request: &LocalDiarizationRequest,
) -> Result<LocalDiarizationResponse, LocalWhisperProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_DIARIZATION_RESPONSE_BYTES {
        return Err(LocalWhisperProtocolError::new("response_too_large"));
    }
    let response: LocalDiarizationResponse = serde_json::from_slice(bytes)
        .map_err(|_| LocalWhisperProtocolError::new("invalid_json"))?;
    response.validate_against(request)?;
    Ok(response)
}

fn encode_bounded(
    value: &impl Serialize,
    maximum_bytes: usize,
    error_code: &'static str,
) -> Result<Vec<u8>, LocalWhisperProtocolError> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|_| LocalWhisperProtocolError::new("invalid_json"))?;
    bytes.push(b'\n');
    if bytes.len() > maximum_bytes {
        return Err(LocalWhisperProtocolError::new(error_code));
    }
    Ok(bytes)
}

fn valid_model_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=128).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
}

fn valid_language(value: &str) -> bool {
    (2..=16).contains(&value.len())
        && value.split('-').all(|part| {
            (2..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_lowercase())
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 2_048
        && !value.contains('\\')
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, Component::Normal(_)) && component.as_os_str().to_str().is_some()
        })
}

fn valid_relative_audio(value: &str) -> bool {
    valid_relative_path(value)
        && matches!(
            value
                .rsplit_once('.')
                .map(|(_, extension)| extension.to_ascii_lowercase()),
            Some(extension) if matches!(extension.as_str(), "wav" | "m4a" | "mp3")
        )
}

fn valid_local_speaker_id(value: &str) -> bool {
    value == "local_unknown"
        || value.strip_prefix("local_speaker_").is_some_and(|slot| {
            slot.len() == 2
                && slot.bytes().all(|byte| byte.is_ascii_digit())
                && slot
                    .parse::<u32>()
                    .is_ok_and(|slot| (1..=MAX_LOCAL_SPEAKERS).contains(&slot))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWhisperProtocolError {
    pub code: &'static str,
}

impl LocalWhisperProtocolError {
    fn new(code: &'static str) -> Self {
        Self { code }
    }
}

impl std::fmt::Display for LocalWhisperProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local Whisper worker protocol is invalid")
    }
}

impl std::error::Error for LocalWhisperProtocolError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const MODEL_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const AUDIO_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn request() -> LocalWhisperRequest {
        LocalWhisperRequest {
            schema_version: LOCAL_WHISPER_PROTOCOL_VERSION,
            recording_id: "018f92d8-6ad4-7dc1-8e28-8b020d2942cb".parse().unwrap(),
            model_id: "large-v3-turbo-q5_0".to_owned(),
            model_sha256: MODEL_SHA.to_owned(),
            model_size_bytes: 1_500_000_000,
            audio_relative_path: "derived/mixed.wav".to_owned(),
            audio_sha256: AUDIO_SHA.to_owned(),
            audio_size_bytes: 12_000_000,
            audio_duration_ms: 60_000,
            language: Some("zh".to_owned()),
        }
    }

    fn response(request: &LocalWhisperRequest) -> LocalWhisperResponse {
        LocalWhisperResponse {
            schema_version: LOCAL_WHISPER_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            model_id: request.model_id.clone(),
            model_sha256: request.model_sha256.clone(),
            audio_sha256: request.audio_sha256.clone(),
            language: "zh".to_owned(),
            segments: vec![
                LocalWhisperSegment {
                    start_ms: 0,
                    end_ms: 1_500,
                    text: "fabricated first segment".to_owned(),
                    speaker_id: None,
                },
                LocalWhisperSegment {
                    start_ms: 1_750,
                    end_ms: 3_000,
                    text: "fabricated second segment".to_owned(),
                    speaker_id: None,
                },
            ],
        }
    }

    #[test]
    fn bounded_request_and_response_round_trip() {
        let request = request();
        assert_eq!(
            decode_request(&encode_request(&request).unwrap()).unwrap(),
            request
        );
        let response = response(&request);
        assert_eq!(
            decode_response(&encode_response(&response, &request).unwrap(), &request).unwrap(),
            response
        );
        let transcript = response.transcript_json(&request).unwrap();
        assert_eq!(transcript[0]["speaker"]["id"], "local_unknown");
        assert_eq!(transcript[0]["stt_backend"], "whisper_local");
        let qwen_transcript = response
            .transcript_json_for_backend(&request, LocalTranscriptBackend::QwenLocal)
            .unwrap();
        assert_eq!(qwen_transcript[0]["stt_backend"], "qwen_local");
    }

    #[test]
    fn request_rejects_paths_unknown_fields_and_unbounded_input() {
        let mut invalid_path = request();
        invalid_path.audio_relative_path = "../../private.wav".to_owned();
        assert_eq!(invalid_path.validate().unwrap_err().code, "invalid_request");

        let mut invalid_language = request();
        invalid_language.language = Some("zh-".to_owned());
        assert_eq!(
            invalid_language.validate().unwrap_err().code,
            "invalid_request"
        );

        let mut value = serde_json::to_value(request()).unwrap();
        value["absoluteModelPath"] = json!("/Users/private/model.bin");
        assert_eq!(
            decode_request(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .code,
            "invalid_json"
        );
        assert_eq!(
            decode_request(&vec![b' '; MAX_LOCAL_WHISPER_REQUEST_BYTES + 1])
                .unwrap_err()
                .code,
            "request_too_large"
        );
    }

    #[test]
    fn response_is_identity_bound_and_timestamps_are_monotonic() {
        let request = request();
        let mut wrong_identity = response(&request);
        wrong_identity.audio_sha256 = MODEL_SHA.to_owned();
        assert_eq!(
            wrong_identity.validate_against(&request).unwrap_err().code,
            "invalid_response"
        );

        let mut overlap = response(&request);
        overlap.segments[1].start_ms = 1_000;
        assert_eq!(
            overlap.validate_against(&request).unwrap_err().code,
            "invalid_response"
        );
    }

    #[test]
    fn response_rejects_segment_and_total_output_bombs() {
        let request = request();
        let mut huge_segment = response(&request);
        huge_segment.segments[0].text = "x".repeat(MAX_LOCAL_WHISPER_SEGMENT_TEXT_BYTES + 1);
        assert_eq!(
            huge_segment.validate_against(&request).unwrap_err().code,
            "invalid_response"
        );

        let mut huge_total = response(&request);
        huge_total.segments = (0..=MAX_LOCAL_WHISPER_TRANSCRIPT_TEXT_BYTES
            / MAX_LOCAL_WHISPER_SEGMENT_TEXT_BYTES)
            .map(|index| LocalWhisperSegment {
                start_ms: index as u64,
                end_ms: index as u64 + 1,
                text: "x".repeat(MAX_LOCAL_WHISPER_SEGMENT_TEXT_BYTES),
                speaker_id: None,
            })
            .collect();
        assert_eq!(
            huge_total.validate_against(&request).unwrap_err().code,
            "response_too_large"
        );

        let mut too_many = response(&request);
        too_many.segments = (0..=MAX_LOCAL_WHISPER_SEGMENTS)
            .map(|index| LocalWhisperSegment {
                start_ms: index as u64,
                end_ms: index as u64 + 1,
                text: "x".to_owned(),
                speaker_id: None,
            })
            .collect();
        assert_eq!(
            too_many.validate_against(&request).unwrap_err().code,
            "invalid_response"
        );
    }

    fn diarization_request() -> LocalDiarizationRequest {
        LocalDiarizationRequest {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request().recording_id,
            pack_id: "fluid-v1".to_owned(),
            quality_preset: LOCAL_DIARIZATION_QUALITY_PRESET.to_owned(),
            model_files: vec![LocalModelFileIdentity {
                relative_path: "speaker-diarization-coreml/Segmentation.mlmodelc/model.mil"
                    .to_owned(),
                sha256: MODEL_SHA.to_owned(),
                size_bytes: 43_063,
            }],
            audio_relative_path: "derived/mixed.wav".to_owned(),
            audio_sha256: AUDIO_SHA.to_owned(),
            audio_size_bytes: 12_000_000,
            audio_duration_ms: 60_000,
            expected_speaker_count: Some(2),
        }
    }

    fn diarization_response(request: &LocalDiarizationRequest) -> LocalDiarizationResponse {
        LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: request.pack_id.clone(),
            quality_preset: request.quality_preset.clone(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 2,
            segments: vec![
                LocalDiarizationSegment {
                    start_ms: 0,
                    end_ms: 1_400,
                    speaker_slot: 1,
                    confidence_milli: 950,
                },
                LocalDiarizationSegment {
                    start_ms: 1_600,
                    end_ms: 3_100,
                    speaker_slot: 2,
                    confidence_milli: 900,
                },
            ],
        }
    }

    #[test]
    fn diarization_preset_is_bound_to_its_model_pack() {
        let mut request = diarization_request();
        request.pack_id = "speakerkit-v1".to_owned();
        request.quality_preset = LOCAL_DIARIZATION_SPEAKERKIT_PRESET.to_owned();
        request.validate().unwrap();

        request.pack_id = "fluid-v1".to_owned();
        assert_eq!(request.validate().unwrap_err().code, "invalid_request");
        request.quality_preset = LOCAL_DIARIZATION_QUALITY_PRESET.to_owned();
        request.validate().unwrap();
        request.pack_id = "speakerkit-v1".to_owned();
        assert_eq!(request.validate().unwrap_err().code, "invalid_request");
    }

    #[test]
    fn speakerkit_tail_context_is_explicit_and_responses_cannot_cross_presets() {
        let mut legacy = diarization_request();
        legacy.pack_id = "speakerkit-v1".into();
        legacy.quality_preset = LOCAL_DIARIZATION_SPEAKERKIT_PRESET.into();
        let mut candidate = legacy.clone();
        candidate.quality_preset = LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET.into();
        let bytes = encode_diarization_request(&candidate).unwrap();
        assert_eq!(decode_diarization_request(&bytes).unwrap(), candidate);
        assert_ne!(encode_diarization_request(&legacy).unwrap(), bytes);
        let legacy_response = serde_json::to_vec(&diarization_response(&legacy)).unwrap();
        let candidate_response = serde_json::to_vec(&diarization_response(&candidate)).unwrap();
        assert!(decode_diarization_response(&legacy_response, &candidate).is_err());
        assert!(decode_diarization_response(&candidate_response, &legacy).is_err());
        decode_diarization_response(&candidate_response, &candidate).unwrap();
        candidate.pack_id = "fluid-v1".into();
        assert_eq!(candidate.validate().unwrap_err().code, "invalid_request");
    }

    #[test]
    fn diarization_protocol_is_bounded_closed_and_identity_bound() {
        let request = diarization_request();
        assert_eq!(
            decode_diarization_request(&encode_diarization_request(&request).unwrap()).unwrap(),
            request
        );
        let response = diarization_response(&request);
        assert_eq!(
            decode_diarization_response(
                &encode_diarization_response(&response, &request).unwrap(),
                &request,
            )
            .unwrap(),
            response
        );

        let mut duplicate_path = request.clone();
        duplicate_path
            .model_files
            .push(duplicate_path.model_files[0].clone());
        assert_eq!(
            duplicate_path.validate().unwrap_err().code,
            "invalid_request"
        );
        let mut wrong_identity = response.clone();
        wrong_identity.audio_sha256 = MODEL_SHA.to_owned();
        assert_eq!(
            wrong_identity.validate_against(&request).unwrap_err().code,
            "invalid_response"
        );
        let mut missing_slot = response;
        missing_slot.segments[1].speaker_slot = 1;
        assert_eq!(
            missing_slot.validate_against(&request).unwrap_err().code,
            "invalid_response"
        );
    }

    #[test]
    fn diarization_merge_assigns_only_unambiguous_overlap() {
        let whisper_request = request();
        let mut whisper = response(&whisper_request);
        let diarization_request = diarization_request();
        let diarization = diarization_response(&diarization_request);
        let stats = merge_diarization(
            &mut whisper,
            &whisper_request,
            &diarization,
            &diarization_request,
        )
        .unwrap();
        assert_eq!(stats.assigned_segments, 2);
        assert_eq!(stats.unknown_segments, 0);
        assert_eq!(
            whisper.segments[0].speaker_id.as_deref(),
            Some("local_speaker_01")
        );
        assert_eq!(
            whisper.segments[1].speaker_id.as_deref(),
            Some("local_speaker_02")
        );

        let mut ambiguous_whisper = response(&whisper_request);
        ambiguous_whisper.segments[0].start_ms = 1_300;
        ambiguous_whisper.segments[0].end_ms = 1_700;
        ambiguous_whisper.segments.remove(1);
        let stats = merge_diarization(
            &mut ambiguous_whisper,
            &whisper_request,
            &diarization,
            &diarization_request,
        )
        .unwrap();
        assert_eq!(stats.assigned_segments, 0);
        assert_eq!(stats.unknown_segments, 1);
        assert!(ambiguous_whisper.segments[0].speaker_id.is_none());

        let mut low_confidence = diarization;
        low_confidence.segments[0].confidence_milli = MIN_LOCAL_DIARIZATION_CONFIDENCE_MILLI - 1;
        let mut partially_labeled = response(&whisper_request);
        let stats = merge_diarization(
            &mut partially_labeled,
            &whisper_request,
            &low_confidence,
            &diarization_request,
        )
        .unwrap();
        assert_eq!(stats.assigned_segments, 1);
        assert_eq!(stats.unknown_segments, 1);
        assert!(partially_labeled.segments[0].speaker_id.is_none());
    }

    #[test]
    fn aligned_word_merge_bridges_only_short_same_speaker_gaps_and_attaches_punctuation() {
        let mut request = diarization_request();
        request.expected_speaker_count = Some(1);
        let diarization = LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: request.pack_id.clone(),
            quality_preset: request.quality_preset.clone(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 1,
            segments: vec![
                LocalDiarizationSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    speaker_slot: 1,
                    confidence_milli: 900,
                },
                LocalDiarizationSegment {
                    start_ms: 1_700,
                    end_ms: 2_500,
                    speaker_slot: 1,
                    confidence_milli: 900,
                },
            ],
        };
        let mut words = vec![
            LocalAlignedWord {
                start_ms: 100,
                end_ms: 300,
                text: "Hello".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 300,
                end_ms: 300,
                text: ",".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 1_100,
                end_ms: 1_400,
                text: "bridge".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 1_800,
                end_ms: 2_000,
                text: "after".to_owned(),
                speaker_id: None,
            },
        ];
        let stats =
            merge_aligned_words_with_diarization(&mut words, &diarization, &request).unwrap();
        assert_eq!(stats.lexical_words, 3);
        assert_eq!(stats.overlap_assigned_words, 2);
        assert_eq!(stats.bridge_assigned_words, 1);
        assert_eq!(stats.unknown_lexical_words, 0);
        assert_eq!(stats.attached_punctuation_tokens, 1);
        assert!(words
            .iter()
            .all(|word| word.speaker_id.as_deref() == Some("local_speaker_01")));
    }

    #[test]
    fn aligned_word_merge_keeps_ambiguous_long_and_one_sided_gaps_unknown() {
        let mut request = diarization_request();
        request.expected_speaker_count = Some(2);
        let diarization = LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: request.pack_id.clone(),
            quality_preset: request.quality_preset.clone(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 2,
            segments: vec![
                LocalDiarizationSegment {
                    start_ms: 0,
                    end_ms: 500,
                    speaker_slot: 1,
                    confidence_milli: 900,
                },
                LocalDiarizationSegment {
                    start_ms: 2_000,
                    end_ms: 2_500,
                    speaker_slot: 2,
                    confidence_milli: 900,
                },
            ],
        };
        let mut words = vec![
            LocalAlignedWord {
                start_ms: 700,
                end_ms: 800,
                text: "long-gap".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 2_700,
                end_ms: 2_800,
                text: "one-sided".to_owned(),
                speaker_id: None,
            },
        ];
        let stats =
            merge_aligned_words_with_diarization(&mut words, &diarization, &request).unwrap();
        assert_eq!(stats.bridge_assigned_words, 0);
        assert_eq!(stats.unknown_lexical_words, 2);
        assert!(words.iter().all(|word| word.speaker_id.is_none()));
    }

    #[test]
    fn zero_duration_words_interpolate_only_between_valid_lexical_anchors() {
        let mut request = diarization_request();
        request.expected_speaker_count = Some(1);
        let diarization = LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: request.pack_id.clone(),
            quality_preset: request.quality_preset.clone(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 1,
            segments: vec![LocalDiarizationSegment {
                start_ms: 0,
                end_ms: 2_000,
                speaker_slot: 1,
                confidence_milli: 900,
            }],
        };
        let mut words = vec![
            LocalAlignedWord {
                start_ms: 100,
                end_ms: 200,
                text: "anchor".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 400,
                end_ms: 400,
                text: "zero-a".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 500,
                end_ms: 500,
                text: "zero-b".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 800,
                end_ms: 900,
                text: "anchor".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 1_000,
                end_ms: 1_000,
                text: "unbounded".to_owned(),
                speaker_id: None,
            },
        ];
        let stats =
            merge_aligned_words_with_diarization(&mut words, &diarization, &request).unwrap();
        assert_eq!(stats.interpolated_zero_duration_words, 2);
        assert_eq!(stats.point_assigned_words, 1);
        assert_eq!((words[1].start_ms, words[1].end_ms), (200, 500));
        assert_eq!((words[2].start_ms, words[2].end_ms), (500, 800));
        assert_eq!((words[4].start_ms, words[4].end_ms), (1_000, 1_001));
        assert_eq!(words[4].speaker_id.as_deref(), Some("local_speaker_01"));
    }

    #[test]
    fn dense_quantized_zero_run_stays_between_anchors_with_positive_intervals() {
        let mut request = diarization_request();
        request.expected_speaker_count = Some(1);
        let diarization = LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: request.pack_id.clone(),
            quality_preset: request.quality_preset.clone(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 1,
            segments: vec![LocalDiarizationSegment {
                start_ms: 0,
                end_ms: 1_000,
                speaker_slot: 1,
                confidence_milli: 900,
            }],
        };
        let mut words = vec![
            LocalAlignedWord {
                start_ms: 100,
                end_ms: 200,
                text: "left".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 200,
                end_ms: 200,
                text: "dense-a".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 200,
                end_ms: 200,
                text: "dense-b".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 200,
                end_ms: 200,
                text: "dense-c".to_owned(),
                speaker_id: None,
            },
            LocalAlignedWord {
                start_ms: 201,
                end_ms: 300,
                text: "right".to_owned(),
                speaker_id: None,
            },
        ];
        let stats =
            merge_aligned_words_with_diarization(&mut words, &diarization, &request).unwrap();
        assert_eq!(stats.interpolated_zero_duration_words, 3);
        assert_eq!(stats.unknown_lexical_words, 0);
        for word in &words[1..4] {
            assert_eq!((word.start_ms, word.end_ms), (200, 201));
            assert_eq!(word.speaker_id.as_deref(), Some("local_speaker_01"));
        }
    }

    #[test]
    fn zero_point_on_diarization_boundary_remains_unknown() {
        let mut request = diarization_request();
        request.expected_speaker_count = Some(1);
        let diarization = LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: request.pack_id.clone(),
            quality_preset: request.quality_preset.clone(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 1,
            segments: vec![LocalDiarizationSegment {
                start_ms: 100,
                end_ms: 1_000,
                speaker_slot: 1,
                confidence_milli: 900,
            }],
        };
        let mut words = vec![LocalAlignedWord {
            start_ms: 100,
            end_ms: 100,
            text: "boundary".to_owned(),
            speaker_id: None,
        }];
        let stats =
            merge_aligned_words_with_diarization(&mut words, &diarization, &request).unwrap();
        assert_eq!(stats.point_assigned_words, 0);
        assert_eq!(stats.unknown_lexical_words, 1);
        assert_eq!((words[0].start_ms, words[0].end_ms), (100, 100));
        assert!(words[0].speaker_id.is_none());
    }
}
