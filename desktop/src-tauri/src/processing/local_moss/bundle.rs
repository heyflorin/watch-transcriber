use serde::{Deserialize, Serialize};

use super::{
    digest, error, moss, valid_hash, LocalMossError, LocalMossPlan, MAX_ARCHIVE_SEGMENTS,
    MAX_BUNDLE_RESPONSE_BYTES,
};
use crate::processing::local_whisper::{decode_diarization_response, LocalDiarizationResponse};

/// The ledger supplies the binding captured at dispatch plus the retained body
/// hash. Worker response schemas do not attest which on-disk model was loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseBinding {
    pub plan_sha256: String,
    pub request_sha256: String,
    pub response_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResponseReceipt {
    pub binding: ResponseBinding,
    pub window_index: Option<usize>,
    pub response_size_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChronologicalWindowEvidence {
    pub window_index: usize,
    pub provenance: moss::ChronologicalProvenance,
    pub source_emission_joined_text_sha256: String,
    pub chronological_joined_text_sha256: String,
}

fn validate_binding(
    plan: &LocalMossPlan,
    request_hash: &str,
    binding: &ResponseBinding,
    bytes: &[u8],
) -> Result<(), LocalMossError> {
    if !valid_hash(&binding.plan_sha256)
        || !valid_hash(&binding.request_sha256)
        || !valid_hash(&binding.response_sha256)
        || binding.plan_sha256 != plan.plan_sha256()
        || binding.request_sha256 != request_hash
        || binding.response_sha256 != digest(bytes)
    {
        return Err(error("local_moss_response_binding_mismatch"));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ValidatedMossWindowResponse {
    pub(super) receipt: ResponseReceipt,
    pub(super) adapted: moss::AdaptedTranscript,
    source_segments: usize,
    text_bytes: usize,
    chronology: Option<ChronologicalWindowEvidence>,
}

impl ValidatedMossWindowResponse {
    pub fn decode(
        plan: &LocalMossPlan,
        index: usize,
        binding: ResponseBinding,
        bytes: &[u8],
    ) -> Result<Self, LocalMossError> {
        if bytes.is_empty() || bytes.len() > moss::MAX_RESPONSE_BYTES {
            return Err(error("local_moss_response_bytes_invalid"));
        }
        let window = plan
            .windows()
            .get(index)
            .ok_or_else(|| error("local_moss_window_index_invalid"))?;
        validate_binding(plan, window.request_sha256(), &binding, bytes)?;
        let response = moss::decode_response(bytes, window.request())
            .map_err(|_| error("local_moss_response_invalid"))?;
        let adapted = response
            .adapt(window.request())
            .map_err(|_| error("local_moss_response_invalid"))?;
        let source_joined = joined_hash(
            response
                .segments
                .iter()
                .map(|segment| segment.text.as_str()),
        );
        let chronology = if window.request().timing_policy == moss::CHRONOLOGICAL_TIMING_POLICY_V3 {
            Some(verify_chronological_conservation(
                index,
                &response,
                &adapted,
                source_joined,
            )?)
        } else {
            // Retained v1/v2 compare the original global emission-order text.
            if source_joined
                != joined_hash(adapted.segments.iter().map(|segment| segment.text.as_str()))
            {
                return Err(error("local_moss_source_text_changed"));
            }
            None
        };
        let text_bytes = adapted
            .segments
            .iter()
            .try_fold(0_usize, |sum, segment| sum.checked_add(segment.text.len()))
            .ok_or_else(|| error("local_moss_text_limit"))?;
        Ok(Self {
            receipt: ResponseReceipt {
                binding,
                window_index: Some(index),
                response_size_bytes: bytes.len(),
            },
            source_segments: response.segments.len(),
            adapted,
            text_bytes,
            chronology,
        })
    }
    pub fn receipt(&self) -> &ResponseReceipt {
        &self.receipt
    }
    pub fn source_segments(&self) -> usize {
        self.source_segments
    }
    pub fn output_segments(&self) -> usize {
        self.adapted.segments.len()
    }
    pub fn chronology(&self) -> Option<&ChronologicalWindowEvidence> {
        self.chronology.as_ref()
    }
}

fn verify_chronological_conservation(
    window_index: usize,
    response: &moss::MossResponse,
    adapted: &moss::AdaptedTranscript,
    source_emission_joined_text_sha256: String,
) -> Result<ChronologicalWindowEvidence, LocalMossError> {
    let provenance = adapted
        .chronology
        .as_ref()
        .ok_or_else(|| error("local_moss_source_text_changed"))?;
    let indices = &provenance.source_index_permutation;
    if indices.len() != response.segments.len()
        || provenance.output_source_indices.len() != adapted.segments.len()
        || provenance
            .output_source_indices
            .iter()
            .flatten()
            .copied()
            .ne(indices.iter().copied())
    {
        return Err(error("local_moss_source_text_changed"));
    }
    let mut seen = vec![false; response.segments.len()];
    let mut by_speaker = std::collections::BTreeMap::new();
    let mut previous = None;
    for &index in indices {
        let segment = response
            .segments
            .get(index)
            .ok_or_else(|| error("local_moss_source_text_changed"))?;
        if seen[index]
            || previous.is_some_and(|key| (segment.start_ms, index) < key)
            || by_speaker
                .insert(segment.speaker_id, index)
                .is_some_and(|last| index < last)
        {
            return Err(error("local_moss_source_text_changed"));
        }
        seen[index] = true;
        previous = Some((segment.start_ms, index));
    }
    for (group, segment) in provenance
        .output_source_indices
        .iter()
        .zip(&adapted.segments)
    {
        if group.is_empty()
            || joined_hash(
                group
                    .iter()
                    .map(|&index| response.segments[index].text.as_str()),
            ) != digest(segment.text.as_bytes())
        {
            return Err(error("local_moss_source_text_changed"));
        }
    }
    let chronological_joined_text_sha256 = joined_hash(
        indices
            .iter()
            .map(|&index| response.segments[index].text.as_str()),
    );
    if chronological_joined_text_sha256
        != joined_hash(adapted.segments.iter().map(|segment| segment.text.as_str()))
    {
        return Err(error("local_moss_source_text_changed"));
    }
    Ok(ChronologicalWindowEvidence {
        window_index,
        provenance: provenance.clone(),
        source_emission_joined_text_sha256,
        chronological_joined_text_sha256,
    })
}

fn joined_hash<'a>(texts: impl Iterator<Item = &'a str>) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for (index, text) in texts.enumerate() {
        if index > 0 {
            hash.update(b" ");
        }
        hash.update(text.as_bytes());
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Debug)]
pub struct CompleteMossResponses {
    pub(super) plan_sha256: String,
    pub(super) windows: Vec<ValidatedMossWindowResponse>,
    pub(super) source_segments: usize,
    pub(super) output_segments: usize,
    pub(super) coalesced_source_segments: usize,
    pub(super) clipped_tail_ms_sum: u64,
}

impl CompleteMossResponses {
    pub fn new(
        plan: &LocalMossPlan,
        windows: Vec<ValidatedMossWindowResponse>,
    ) -> Result<Self, LocalMossError> {
        Self::try_collect(plan, windows.into_iter().map(Ok))
    }

    /// Allows the owner to load/checkpoint sidecars lazily without embedding
    /// response bodies into its ledger. Failure returns no partial bundle.
    pub fn try_collect(
        plan: &LocalMossPlan,
        windows: impl IntoIterator<Item = Result<ValidatedMossWindowResponse, LocalMossError>>,
    ) -> Result<Self, LocalMossError> {
        let mut result = Self {
            plan_sha256: plan.plan_sha256().into(),
            windows: Vec::new(),
            source_segments: 0,
            output_segments: 0,
            coalesced_source_segments: 0,
            clipped_tail_ms_sum: 0,
        };
        let (mut bytes, mut text) = (0_usize, 0_usize);
        for (index, response) in windows.into_iter().enumerate() {
            let response = response?;
            let expected = plan
                .windows()
                .get(index)
                .ok_or_else(|| error("local_moss_window_set_invalid"))?;
            if response.receipt.window_index != Some(index)
                || response.receipt.binding.plan_sha256 != plan.plan_sha256()
                || response.receipt.binding.request_sha256 != expected.request_sha256()
            {
                return Err(error("local_moss_window_set_invalid"));
            }
            bytes = bytes
                .checked_add(response.receipt.response_size_bytes)
                .ok_or_else(|| error("local_moss_bundle_bytes_limit"))?;
            text = text
                .checked_add(response.text_bytes)
                .ok_or_else(|| error("local_moss_text_limit"))?;
            result.source_segments = result
                .source_segments
                .checked_add(response.source_segments)
                .ok_or_else(|| error("local_moss_segment_limit"))?;
            result.output_segments = result
                .output_segments
                .checked_add(response.adapted.segments.len())
                .ok_or_else(|| error("local_moss_segment_limit"))?;
            result.coalesced_source_segments = result
                .coalesced_source_segments
                .checked_add(response.adapted.coalesced_source_segments)
                .ok_or_else(|| error("local_moss_segment_limit"))?;
            result.clipped_tail_ms_sum = result
                .clipped_tail_ms_sum
                .checked_add(response.adapted.clipped_tail_ms)
                .ok_or_else(|| error("local_moss_timing_overflow"))?;
            if bytes > MAX_BUNDLE_RESPONSE_BYTES
                || text > moss::MAX_TEXT_BYTES
                || result.output_segments > MAX_ARCHIVE_SEGMENTS
            {
                return Err(error("local_moss_bundle_limit"));
            }
            result.windows.push(response);
        }
        if result.windows.len() != plan.windows().len() {
            return Err(error("local_moss_window_set_incomplete"));
        }
        Ok(result)
    }
    pub fn plan_sha256(&self) -> &str {
        &self.plan_sha256
    }
    pub fn window_receipts(&self) -> impl Iterator<Item = &ResponseReceipt> {
        self.windows.iter().map(|window| &window.receipt)
    }
}

#[derive(Clone, Debug)]
pub struct ValidatedSpeakerKitResponse {
    pub(super) receipt: ResponseReceipt,
    pub(super) response: LocalDiarizationResponse,
}

impl ValidatedSpeakerKitResponse {
    pub fn decode(
        plan: &LocalMossPlan,
        binding: ResponseBinding,
        bytes: &[u8],
    ) -> Result<Self, LocalMossError> {
        if bytes.is_empty()
            || bytes.len() > crate::processing::local_whisper::MAX_LOCAL_DIARIZATION_RESPONSE_BYTES
        {
            return Err(error("local_moss_diarization_response_bytes_invalid"));
        }
        validate_binding(plan, plan.diarization_request_sha256(), &binding, bytes)?;
        let response = decode_diarization_response(bytes, plan.diarization_request())
            .map_err(|_| error("local_moss_diarization_response_invalid"))?;
        Ok(Self {
            receipt: ResponseReceipt {
                binding,
                window_index: None,
                response_size_bytes: bytes.len(),
            },
            response,
        })
    }
    pub fn receipt(&self) -> &ResponseReceipt {
        &self.receipt
    }
}
