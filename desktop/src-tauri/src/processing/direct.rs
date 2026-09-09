//! Production direct-effect composition for the embedded processing engine.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use crate::ingest::envelope::RecordingEnvelope;
use crate::secrets::ProcessingCredentialsState;

use super::engine::{EffectError, EffectErrorKind, PollResult, ProcessingEffects, SummaryRequest};
use super::local_whisper::{
    merge_aligned_words_with_diarization, merge_diarization, LocalAlignedWord,
    LocalDiarizationRequest, LocalDiarizationResponse, LocalWhisperRequest, LocalWhisperResponse,
    LocalWhisperSegment, LOCAL_WHISPER_PROTOCOL_VERSION,
};
use super::local_worker::{
    LocalWhisperWorker, LocalWhisperWorkerError, LocalWhisperWorkerErrorKind,
};
use super::providers::{
    DirectProviders, MiaojiPoll, ProviderError, ProviderErrorKind, ProviderTransport,
    ReqwestProviderTransport, SecretText,
};
use super::tos::{DirectTos, TosConfiguration, TosErrorKind, TosErrorSafe, TosObjectReceipt};
use super::{
    CanonicalBackupCheckpoint, LocalTranscriptionRequest, NormalizedArtifactCheckpoint,
    PublicationBackend, PublicationProof, PublicationTargetPlan, TosObjectCheckpoint,
};

const CANCELED_UPLOAD_ABSENCE_PROBES: usize = 3;
const CANCELED_UPLOAD_ABSENCE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

#[async_trait]
pub trait ArchiveEffects: Send + Sync + 'static {
    fn plan(
        &self,
        envelope: &RecordingEnvelope,
        backend: PublicationBackend,
    ) -> Result<Vec<PublicationTargetPlan>, EffectError>;

    async fn publish(
        &self,
        target_id: &str,
        generation: u64,
        envelope: &RecordingEnvelope,
        transcript: &Value,
        summary: &Value,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<PublicationProof, EffectError>;

    async fn verify_backup(
        &self,
        backend: PublicationBackend,
        generation: u64,
        envelope: &RecordingEnvelope,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<CanonicalBackupCheckpoint, EffectError>;
}

pub struct DirectEffects<A: ArchiveEffects, T: ProviderTransport = ReqwestProviderTransport> {
    credentials: Arc<ProcessingCredentialsState>,
    providers: DirectProviders<T>,
    archive: Arc<A>,
    local_whisper: LocalWhisperWorker,
    local_moss: Option<super::moss_worker::MossWorker>,
}

impl<A: ArchiveEffects> DirectEffects<A, ReqwestProviderTransport> {
    pub fn production(
        credentials: Arc<ProcessingCredentialsState>,
        archive: Arc<A>,
        local_whisper: LocalWhisperWorker,
    ) -> Result<Self, EffectError> {
        let transport = ReqwestProviderTransport::new().map_err(map_provider_error)?;
        Ok(Self {
            credentials,
            providers: DirectProviders::new(Arc::new(transport)),
            archive,
            local_whisper,
            local_moss: None,
        })
    }
}

impl<A: ArchiveEffects, T: ProviderTransport> DirectEffects<A, T> {
    pub fn with_moss_worker(mut self, worker: super::moss_worker::MossWorker) -> Self {
        self.local_moss = Some(worker);
        self
    }
    fn load_credentials(&self) -> Result<crate::secrets::ProcessingCredentials, EffectError> {
        self.credentials
            .load()
            .map_err(|_| EffectError::new(EffectErrorKind::Verification))?
            .ok_or_else(|| EffectError::new(EffectErrorKind::Rejected))
    }

    fn tos(credentials: &crate::secrets::ProcessingCredentials) -> Result<DirectTos, EffectError> {
        DirectTos::new(TosConfiguration::from_credentials(credentials).map_err(map_tos_error)?)
            .map_err(map_tos_error)
    }
}

#[async_trait]
impl<A: ArchiveEffects, T: ProviderTransport> ProcessingEffects for DirectEffects<A, T> {
    async fn transcribe_moss(
        &self,
        request: &echowall_local_moss_protocol::MossRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, EffectError> {
        self.local_moss
            .as_ref()
            .ok_or_else(|| EffectError::new(EffectErrorKind::NotDispatched))?
            .transcribe(request, cancel)
            .await
            .map_err(map_moss_error)
    }

    async fn diarize_moss(
        &self,
        request: &LocalDiarizationRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, EffectError> {
        self.local_moss
            .as_ref()
            .ok_or_else(|| EffectError::new(EffectErrorKind::NotDispatched))?
            .diarize(request, cancel)
            .await
            .map_err(map_moss_error)
    }

    async fn summarize_moss(
        &self,
        request: &echowall_local_summary_protocol::LocalSummaryRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value, EffectError> {
        let bytes = self
            .local_moss
            .as_ref()
            .ok_or_else(|| EffectError::new(EffectErrorKind::NotDispatched))?
            .summarize(request, cancel)
            .await
            .map_err(map_moss_error)?;
        let response = echowall_local_summary_protocol::decode_response(&bytes, request)
            .map_err(|_| EffectError::new(EffectErrorKind::Verification))?;
        serde_json::to_value(response.summary)
            .map_err(|_| EffectError::new(EffectErrorKind::Verification))
    }
    async fn transcribe_local(
        &self,
        request: &LocalTranscriptionRequest,
    ) -> Result<LocalWhisperResponse, EffectError> {
        match (&request.whisper, &request.qwen) {
            (Some(whisper), None) => {
                let mut response = self
                    .local_whisper
                    .transcribe(whisper)
                    .await
                    .map_err(map_local_whisper_error)?;
                if let Some(diarization_request) = &request.diarization {
                    let diarization = self
                        .local_whisper
                        .diarize(diarization_request)
                        .await
                        .map_err(map_local_whisper_error)?;
                    let stats = merge_diarization(
                        &mut response,
                        whisper,
                        &diarization,
                        diarization_request,
                    )
                    .map_err(|_| EffectError::new(EffectErrorKind::Verification))?;
                    if stats.assigned_segments.saturating_mul(100)
                        < response.segments.len().saturating_mul(95)
                    {
                        return Err(EffectError::new(EffectErrorKind::Verification));
                    }
                } else if !request.transcript_only_accepted {
                    return Err(EffectError::new(EffectErrorKind::Verification));
                }
                Ok(response)
            }
            (None, Some(qwen)) => {
                let response = self
                    .local_whisper
                    .transcribe_qwen(qwen)
                    .await
                    .map_err(map_local_whisper_error)?;
                if let Some(diarization_request) = &request.diarization {
                    let diarization = self
                        .local_whisper
                        .diarize(diarization_request)
                        .await
                        .map_err(map_local_whisper_error)?;
                    qwen_to_canonical_transcript(
                        response,
                        qwen,
                        Some((&diarization, diarization_request)),
                    )
                } else if request.transcript_only_accepted {
                    qwen_to_canonical_transcript(response, qwen, None)
                } else {
                    Err(EffectError::new(EffectErrorKind::Verification))
                }
            }
            _ => Err(EffectError::new(EffectErrorKind::Verification)),
        }
    }

    async fn upload_tos(
        &self,
        recording_id: Uuid,
        object_key: &str,
        source: &Path,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<TosObjectCheckpoint, EffectError> {
        let credentials = self.load_credentials()?;
        let receipt = Self::tos(&credentials)?
            .upload_verified(
                &recording_id.to_string(),
                object_key,
                source,
                &artifact.sha256,
                artifact.size_bytes,
            )
            .await
            .map_err(map_tos_error)?;
        Ok(receipt_to_checkpoint(receipt))
    }

    async fn probe_tos(
        &self,
        recording_id: Uuid,
        object_key: &str,
        source: &Path,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<Option<TosObjectCheckpoint>, EffectError> {
        let credentials = self.load_credentials()?;
        let tos = Self::tos(&credentials)?;
        for attempt in 0..CANCELED_UPLOAD_ABSENCE_PROBES {
            let receipt = tos
                .probe_source_expected(
                    &recording_id.to_string(),
                    object_key,
                    source,
                    &artifact.sha256,
                    artifact.size_bytes,
                )
                .await
                .map_err(map_tos_error)?;
            if receipt.is_some() || attempt + 1 == CANCELED_UPLOAD_ABSENCE_PROBES {
                return Ok(receipt.map(receipt_to_checkpoint));
            }
            // A canceled request may have reached TOS even if its client future
            // ended without a receipt. Never PUT during reconciliation; require
            // repeated absence after the in-flight call has stopped.
            tokio::time::sleep(CANCELED_UPLOAD_ABSENCE_INTERVAL).await;
        }
        unreachable!("the bounded absence probe loop always returns")
    }

    async fn submit_miaoji(
        &self,
        object: &TosObjectCheckpoint,
        request_id: &str,
        speaker_count: Option<u32>,
    ) -> Result<String, EffectError> {
        let credentials = self
            .load_credentials()
            .map_err(|_| submit_not_dispatched())?;
        let tos = Self::tos(&credentials).map_err(|_| submit_not_dispatched())?;
        let receipt = checkpoint_to_receipt(object);
        let download_url = tos
            .presign_exact_get(&receipt, 7_200)
            .await
            .map_err(|_| submit_not_dispatched())?;
        let api_key = SecretText::new(credentials.volc_api_key().to_owned())
            .map_err(|_| submit_not_dispatched())?;
        self.providers
            .submit_miaoji(&api_key, &download_url, request_id, speaker_count)
            .await
            .map_err(map_submit_provider_error)
    }

    async fn poll_miaoji(
        &self,
        request_id: &str,
        task_id: &str,
    ) -> Result<PollResult, EffectError> {
        let credentials = self.load_credentials()?;
        let api_key =
            SecretText::new(credentials.volc_api_key().to_owned()).map_err(map_provider_error)?;
        match self
            .providers
            .poll_miaoji_once(&api_key, request_id, task_id)
            .await
            .map_err(map_provider_error)?
        {
            MiaojiPoll::Running => Ok(PollResult::Running),
            MiaojiPoll::Complete { transcript_url } => self
                .providers
                .fetch_miaoji_transcript(transcript_url)
                .await
                .map(PollResult::Complete)
                .map_err(map_provider_error),
        }
    }

    async fn summarize(&self, request: &SummaryRequest) -> Result<Value, EffectError> {
        match request {
            SummaryRequest::Gemini { transcript } => {
                let credentials = self
                    .load_credentials()
                    .map_err(|_| EffectError::new(EffectErrorKind::NotDispatched))?;
                let api_key = SecretText::new(credentials.gemini_api_key().to_owned())
                    .map_err(|_| EffectError::new(EffectErrorKind::NotDispatched))?;
                self.providers
                    .summarize_gemini(&api_key, credentials.gemini_model(), transcript)
                    .await
                    .map_err(|error| {
                        if error.kind == ProviderErrorKind::NotDispatched {
                            EffectError::new(EffectErrorKind::NotDispatched)
                        } else {
                            map_provider_error(error)
                        }
                    })
            }
            SummaryRequest::Local(request) => self
                .local_whisper
                .summarize(request)
                .await
                .map(|response| {
                    serde_json::to_value(response.summary)
                        .expect("validated local summary must serialize")
                })
                .map_err(map_local_whisper_error),
        }
    }

    fn publication_plan(
        &self,
        envelope: &RecordingEnvelope,
        backend: PublicationBackend,
    ) -> Result<Vec<PublicationTargetPlan>, EffectError> {
        self.archive.plan(envelope, backend)
    }

    async fn publish_target(
        &self,
        target_id: &str,
        generation: u64,
        envelope: &RecordingEnvelope,
        transcript: &Value,
        summary: &Value,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<PublicationProof, EffectError> {
        self.archive
            .publish(
                target_id, generation, envelope, transcript, summary, artifact,
            )
            .await
    }

    async fn verify_canonical_backup(
        &self,
        backend: PublicationBackend,
        generation: u64,
        envelope: &RecordingEnvelope,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<CanonicalBackupCheckpoint, EffectError> {
        self.archive
            .verify_backup(backend, generation, envelope, artifact)
            .await
    }

    async fn cleanup_tos(&self, object: &TosObjectCheckpoint) -> Result<(), EffectError> {
        let credentials = self.load_credentials()?;
        Self::tos(&credentials)?
            .delete_exact(&checkpoint_to_receipt(object))
            .await
            .map_err(map_tos_error)
    }
}

fn checkpoint_to_receipt(value: &TosObjectCheckpoint) -> TosObjectReceipt {
    TosObjectReceipt {
        bucket: value.bucket.clone(),
        key: value.key.clone(),
        version_id: value.version_id.clone(),
        etag: value.etag.clone(),
        sha256: value.sha256.clone(),
        size_bytes: value.size_bytes,
    }
}

fn receipt_to_checkpoint(value: TosObjectReceipt) -> TosObjectCheckpoint {
    TosObjectCheckpoint {
        bucket: value.bucket,
        key: value.key,
        version_id: value.version_id,
        etag: value.etag,
        sha256: value.sha256,
        size_bytes: value.size_bytes,
    }
}

fn map_provider_error(error: ProviderError) -> EffectError {
    let kind = match error.kind {
        ProviderErrorKind::NotDispatched => EffectErrorKind::NotDispatched,
        ProviderErrorKind::Network => EffectErrorKind::Temporary,
        ProviderErrorKind::Rejected => EffectErrorKind::Rejected,
        ProviderErrorKind::SubmitAmbiguous => EffectErrorKind::SubmitAmbiguous,
        ProviderErrorKind::Configuration
        | ProviderErrorKind::InvalidResponse
        | ProviderErrorKind::ResponseTooLarge => EffectErrorKind::Verification,
    };
    EffectError::new(kind)
}

fn map_submit_provider_error(error: ProviderError) -> EffectError {
    if error.kind == ProviderErrorKind::NotDispatched {
        submit_not_dispatched()
    } else {
        map_provider_error(error)
    }
}

fn submit_not_dispatched() -> EffectError {
    // The engine already persists Rejected as ProviderFailed, whose retry path
    // clears the submit fence. This is the existing safe-retry state for a
    // failure proven to occur before any HTTP request was sent.
    EffectError::new(EffectErrorKind::Rejected)
}

fn qwen_to_canonical_transcript(
    response: echowall_local_qwen_protocol::LocalQwenResponse,
    qwen_request: &echowall_local_qwen_protocol::LocalQwenRequest,
    diarization: Option<(&LocalDiarizationResponse, &LocalDiarizationRequest)>,
) -> Result<LocalWhisperResponse, EffectError> {
    response
        .validate_against(qwen_request)
        .map_err(|_| EffectError::new(EffectErrorKind::Verification))?;
    let canonical_request = canonical_qwen_request(qwen_request)?;
    let language = qwen_request
        .language
        .clone()
        .or_else(|| {
            let mut languages = response
                .segments
                .iter()
                .filter_map(|segment| segment.language.clone());
            let first = languages.next()?;
            languages.all(|language| language == first).then_some(first)
        })
        .unwrap_or_else(|| "und".to_owned());

    let segments = if let Some((diarization, diarization_request)) = diarization {
        let counts: Vec<usize> = response
            .segments
            .iter()
            .map(|segment| segment.words.len())
            .collect();
        let mut words: Vec<LocalAlignedWord> = response
            .segments
            .iter()
            .flat_map(|segment| {
                segment.words.iter().map(|word| LocalAlignedWord {
                    start_ms: word.start_ms,
                    end_ms: word.end_ms,
                    text: word.text.clone(),
                    speaker_id: None,
                })
            })
            .collect();
        merge_aligned_words_with_diarization(&mut words, diarization, diarization_request)
            .map_err(|_| EffectError::new(EffectErrorKind::Verification))?;
        let mut offset = 0_usize;
        let mut canonical = Vec::new();
        for (source, count) in response.segments.iter().zip(counts) {
            let end = offset
                .checked_add(count)
                .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
            let chunk_words = words
                .get(offset..end)
                .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
            let grouped = if chunk_words.iter().any(|word| word.end_ms <= word.start_ms) {
                // Point-only tokens without two real anchors cannot become a
                // positive-duration canonical turn. Preserve the exact ASR
                // chunk and mark the whole chunk unresolved instead of
                // borrowing a neighbouring speaker.
                unresolved_qwen_segment(source)
            } else {
                group_qwen_words(chunk_words)?
            };
            let grouped_text = grouped
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let mut grouped = if normalized_transcript_text(&grouped_text)
                == normalized_transcript_text(&source.text)
                && qwen_segments_are_canonical(&grouped, source.start_ms, source.end_ms)
            {
                grouped
            } else {
                // Dense forced-alignment points may need overlapping 1 ms
                // intervals to retain positive durations. Those intervals are
                // useful for evaluating speaker coverage but cannot be emitted
                // as ordered canonical turns. Keep the ASR chunk lossless and
                // expose the uncertainty instead of shifting timestamps or
                // assigning a guessed speaker.
                unresolved_qwen_segment(source)
            };
            canonical.append(&mut grouped);
            offset = end;
        }
        if offset != words.len() {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        canonical
    } else {
        response
            .segments
            .into_iter()
            .map(|segment| LocalWhisperSegment {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: segment.text,
                speaker_id: None,
            })
            .collect()
    };
    let canonical = LocalWhisperResponse {
        schema_version: LOCAL_WHISPER_PROTOCOL_VERSION,
        recording_id: qwen_request.recording_id,
        model_id: qwen_request.asr_model_id.clone(),
        model_sha256: canonical_request.model_sha256.clone(),
        audio_sha256: qwen_request.audio_sha256.clone(),
        language,
        segments,
    };
    canonical
        .validate_against(&canonical_request)
        .map_err(|_| EffectError::new(EffectErrorKind::Verification))?;
    Ok(canonical)
}

fn unresolved_qwen_segment(
    source: &echowall_local_qwen_protocol::LocalQwenSegment,
) -> Vec<LocalWhisperSegment> {
    vec![LocalWhisperSegment {
        start_ms: source.start_ms,
        end_ms: source.end_ms,
        text: source.text.clone(),
        speaker_id: None,
    }]
}

fn qwen_segments_are_canonical(
    segments: &[LocalWhisperSegment],
    chunk_start_ms: u64,
    chunk_end_ms: u64,
) -> bool {
    let mut previous_end = chunk_start_ms;
    !segments.is_empty()
        && segments.iter().all(|segment| {
            let valid = segment.start_ms >= previous_end
                && segment.end_ms > segment.start_ms
                && segment.end_ms <= chunk_end_ms
                && !segment.text.trim().is_empty();
            previous_end = segment.end_ms;
            valid
        })
}

fn canonical_qwen_request(
    qwen: &echowall_local_qwen_protocol::LocalQwenRequest,
) -> Result<LocalWhisperRequest, EffectError> {
    let model_size_bytes = qwen
        .asr_model_files
        .iter()
        .chain(&qwen.aligner_model_files)
        .try_fold(0_u64, |total, file| total.checked_add(file.size_bytes))
        .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
    let request = LocalWhisperRequest {
        schema_version: LOCAL_WHISPER_PROTOCOL_VERSION,
        recording_id: qwen.recording_id,
        model_id: qwen.asr_model_id.clone(),
        model_sha256: qwen
            .model_set_sha256()
            .map_err(|_| EffectError::new(EffectErrorKind::Verification))?,
        model_size_bytes,
        audio_relative_path: qwen.audio_relative_path.clone(),
        audio_sha256: qwen.audio_sha256.clone(),
        audio_size_bytes: qwen.audio_size_bytes,
        audio_duration_ms: qwen.audio_duration_ms,
        language: qwen.language.clone(),
    };
    request
        .validate()
        .map_err(|_| EffectError::new(EffectErrorKind::Verification))?;
    Ok(request)
}

fn group_qwen_words(words: &[LocalAlignedWord]) -> Result<Vec<LocalWhisperSegment>, EffectError> {
    let mut output: Vec<LocalWhisperSegment> = Vec::new();
    for word in words {
        if word.end_ms <= word.start_ms {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        let speaker = word.speaker_id.clone();
        if let Some(previous) = output.last_mut() {
            if previous.speaker_id == speaker {
                append_aligned_token(&mut previous.text, &word.text);
                previous.end_ms = previous.end_ms.max(word.end_ms);
                continue;
            }
        }
        output.push(LocalWhisperSegment {
            start_ms: word.start_ms,
            end_ms: word.end_ms,
            text: word.text.clone(),
            speaker_id: speaker,
        });
    }
    if output.is_empty() {
        Err(EffectError::new(EffectErrorKind::Verification))
    } else {
        Ok(output)
    }
}

fn append_aligned_token(output: &mut String, token: &str) {
    let previous = output.chars().next_back();
    let next = token.chars().next();
    let no_space = previous.is_none()
        || next.is_none()
        || next.is_some_and(|character| !character.is_alphanumeric())
        || previous.is_some_and(is_cjk)
        || next.is_some_and(is_cjk);
    if !no_space {
        output.push(' ');
    }
    output.push_str(token);
}

fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{ac00}'..='\u{d7af}'
    )
}

fn normalized_transcript_text(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric() || *character == '\'')
        .collect()
}

fn map_tos_error(error: TosErrorSafe) -> EffectError {
    let kind = match error.kind {
        TosErrorKind::Network => EffectErrorKind::Temporary,
        TosErrorKind::Conflict => EffectErrorKind::PublicationConflict,
        TosErrorKind::Configuration | TosErrorKind::NotFound | TosErrorKind::Verification => {
            EffectErrorKind::Verification
        }
    };
    EffectError::new(kind)
}

fn map_moss_error(error: super::moss_worker::MossWorkerError) -> EffectError {
    use super::moss_worker::MossWorkerErrorKind;
    EffectError::new(match error.kind {
        MossWorkerErrorKind::Unavailable => EffectErrorKind::NotDispatched,
        MossWorkerErrorKind::Cancelled => EffectErrorKind::Cancelled,
        MossWorkerErrorKind::Temporary => EffectErrorKind::Temporary,
        MossWorkerErrorKind::Verification => EffectErrorKind::Verification,
    })
}

fn map_local_whisper_error(error: LocalWhisperWorkerError) -> EffectError {
    let kind = match error.kind {
        LocalWhisperWorkerErrorKind::Unavailable => EffectErrorKind::Rejected,
        LocalWhisperWorkerErrorKind::Temporary => EffectErrorKind::Temporary,
        LocalWhisperWorkerErrorKind::Verification => EffectErrorKind::Verification,
    };
    EffectError::new(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_errors_map_without_carrying_provider_details() {
        let provider = map_provider_error(ProviderError::submit_ambiguous());
        assert_eq!(provider.kind, EffectErrorKind::SubmitAmbiguous);

        let not_dispatched = map_submit_provider_error(ProviderError::not_dispatched());
        assert_eq!(not_dispatched.kind, EffectErrorKind::Rejected);

        let tos = map_tos_error(TosErrorSafe::network());
        assert_eq!(tos.kind, EffectErrorKind::Temporary);

        let local = map_local_whisper_error(LocalWhisperWorkerError {
            kind: LocalWhisperWorkerErrorKind::Verification,
        });
        assert_eq!(local.kind, EffectErrorKind::Verification);
    }

    #[test]
    fn qwen_words_merge_into_canonical_speaker_turns_without_text_drift() {
        use super::super::local_whisper::{
            LocalDiarizationSegment, LocalModelFileIdentity, LOCAL_DIARIZATION_PROTOCOL_VERSION,
            LOCAL_DIARIZATION_QUALITY_PRESET,
        };
        use echowall_local_qwen_protocol::{
            LocalQwenAlignedWord, LocalQwenModelFileIdentity, LocalQwenRequest, LocalQwenResponse,
            LocalQwenSegment, LOCAL_QWEN_ALIGNER_FILES, LOCAL_QWEN_ALIGNER_MODEL_ID,
            LOCAL_QWEN_ALIGNER_REVISION, LOCAL_QWEN_ASR_FILES, LOCAL_QWEN_ASR_MODEL_ID,
            LOCAL_QWEN_ASR_REVISION, LOCAL_QWEN_CHUNK_DURATION_MS, LOCAL_QWEN_CHUNK_POLICY,
            LOCAL_QWEN_PROTOCOL_VERSION, LOCAL_QWEN_RUNTIME_ID, LOCAL_QWEN_SPLIT_SEARCH_MS,
        };

        let files = |paths: &[&str]| {
            paths
                .iter()
                .map(|path| LocalQwenModelFileIdentity {
                    relative_path: (*path).to_owned(),
                    sha256: "a".repeat(64),
                    size_bytes: 1,
                })
                .collect()
        };
        let request = LocalQwenRequest {
            schema_version: LOCAL_QWEN_PROTOCOL_VERSION,
            recording_id: Uuid::nil(),
            runtime_id: LOCAL_QWEN_RUNTIME_ID.to_owned(),
            asr_model_id: LOCAL_QWEN_ASR_MODEL_ID.to_owned(),
            asr_model_revision: LOCAL_QWEN_ASR_REVISION.to_owned(),
            asr_model_files: files(&LOCAL_QWEN_ASR_FILES),
            aligner_model_id: LOCAL_QWEN_ALIGNER_MODEL_ID.to_owned(),
            aligner_model_revision: LOCAL_QWEN_ALIGNER_REVISION.to_owned(),
            aligner_model_files: files(&LOCAL_QWEN_ALIGNER_FILES),
            audio_relative_path: "derived/input.wav".to_owned(),
            audio_sha256: "b".repeat(64),
            audio_size_bytes: 64_000,
            audio_duration_ms: 2_000,
            language: None,
            chunk_policy: LOCAL_QWEN_CHUNK_POLICY.to_owned(),
            chunk_duration_ms: LOCAL_QWEN_CHUNK_DURATION_MS,
            split_search_ms: LOCAL_QWEN_SPLIT_SEARCH_MS,
        };
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
            segments: vec![LocalQwenSegment {
                start_ms: 0,
                end_ms: 2_000,
                language: None,
                text: "Hello, 世界!".to_owned(),
                words: vec![
                    LocalQwenAlignedWord {
                        start_ms: 100,
                        end_ms: 500,
                        text: "Hello,".to_owned(),
                    },
                    LocalQwenAlignedWord {
                        start_ms: 600,
                        end_ms: 800,
                        text: "世".to_owned(),
                    },
                    LocalQwenAlignedWord {
                        start_ms: 800,
                        end_ms: 1_000,
                        text: "界!".to_owned(),
                    },
                ],
            }],
        };
        let diarization_request = LocalDiarizationRequest {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: "fluid-v1".to_owned(),
            quality_preset: LOCAL_DIARIZATION_QUALITY_PRESET.to_owned(),
            model_files: vec![LocalModelFileIdentity {
                relative_path: "speaker/model.bin".to_owned(),
                sha256: "c".repeat(64),
                size_bytes: 1,
            }],
            audio_relative_path: request.audio_relative_path.clone(),
            audio_sha256: request.audio_sha256.clone(),
            audio_size_bytes: request.audio_size_bytes,
            audio_duration_ms: request.audio_duration_ms,
            expected_speaker_count: Some(1),
        };
        let diarization = LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: "fluid-v1".to_owned(),
            quality_preset: LOCAL_DIARIZATION_QUALITY_PRESET.to_owned(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 1,
            segments: vec![LocalDiarizationSegment {
                start_ms: 0,
                end_ms: 2_000,
                speaker_slot: 1,
                confidence_milli: 900,
            }],
        };
        let mut unresolved = response.clone();
        let last = unresolved.segments[0].words.last_mut().unwrap();
        last.start_ms = 2_000;
        last.end_ms = 2_000;
        let canonical = qwen_to_canonical_transcript(
            response,
            &request,
            Some((&diarization, &diarization_request)),
        )
        .unwrap();
        assert_eq!(canonical.language, "und");
        assert_eq!(canonical.segments.len(), 1);
        assert_eq!(canonical.segments[0].text, "Hello,世界!");
        assert_eq!(
            canonical.segments[0].speaker_id.as_deref(),
            Some("local_speaker_01")
        );
        assert_eq!(canonical.model_sha256, request.model_set_sha256().unwrap());

        let fallback = qwen_to_canonical_transcript(
            unresolved,
            &request,
            Some((&diarization, &diarization_request)),
        )
        .unwrap();
        assert_eq!(fallback.segments.len(), 1);
        assert_eq!(fallback.segments[0].text, "Hello, 世界!");
        assert!(fallback.segments[0].speaker_id.is_none());
    }

    #[test]
    #[ignore = "requires explicitly authorized local Qwen and diarization artifacts"]
    fn live_qwen_artifacts_convert_to_a_valid_canonical_transcript() {
        fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
            let path = std::env::var_os(name).expect("private artifact environment is required");
            let metadata = std::fs::metadata(&path).expect("private artifact must exist");
            assert!(metadata.is_file());
            assert!(metadata.len() > 0 && metadata.len() <= 32 * 1024 * 1024);
            serde_json::from_slice(&std::fs::read(path).expect("private artifact must be readable"))
                .expect("private artifact must match the closed protocol")
        }

        let qwen_request = read_json("ECHOWALL_PRIVATE_QWEN_REQUEST");
        let qwen_response = read_json("ECHOWALL_PRIVATE_QWEN_RESPONSE");
        let diarization_request = read_json("ECHOWALL_PRIVATE_DIARIZATION_REQUEST");
        let diarization_response = read_json("ECHOWALL_PRIVATE_DIARIZATION_RESPONSE");
        let canonical = qwen_to_canonical_transcript(
            qwen_response,
            &qwen_request,
            Some((&diarization_response, &diarization_request)),
        )
        .expect("authorized artifacts must convert without leaking transcript content");
        let canonical_request = canonical_qwen_request(&qwen_request).unwrap();
        canonical.validate_against(&canonical_request).unwrap();
        assert!(!canonical.segments.is_empty());
    }
}
