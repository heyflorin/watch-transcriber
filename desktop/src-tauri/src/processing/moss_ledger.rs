//! Durable MOSS metadata and exact effect-completion fences. Bodies remain in
//! immutable sidecars. An OS owner lease grants execution ownership; a persisted
//! token/session/generation claim separately grants completion authority.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::local_moss::{
    FinalizedMossTranscript, LocalMossPlan, ResponseBinding, ResponseReceipt, SourceAudioIdentity,
    ValidatedMossWindowResponse, ValidatedSpeakerKitResponse, MAX_PLAN_BYTES,
};
use super::moss_artifacts::{ArtifactKind, ArtifactRef, MossArtifacts, OwnerLease};
use super::{
    bump_revision, require_state, transcript_for_summary, transition, validate_json,
    validate_normalized, LocalSummaryCheckpoint, NormalizedArtifactCheckpoint, ProcessingError,
    ProcessingLedger, ProcessingState, ProcessingStore, PublicationBackend, SummaryBackend,
    TranscriptionBackend,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MossEffectKind {
    Window(usize),
    Anchors,
    Finalize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MossEffectClaim {
    token: Uuid,
    owner_session: Uuid,
    recording_id: Uuid,
    generation: Uuid,
    kind: MossEffectKind,
}

impl MossEffectClaim {
    pub fn token(&self) -> Uuid {
        self.token
    }
    pub fn owner_session(&self) -> Uuid {
        self.owner_session
    }
    pub fn recording_id(&self) -> Uuid {
        self.recording_id
    }
    pub fn generation(&self) -> Uuid {
        self.generation
    }
    pub fn kind(&self) -> MossEffectKind {
        self.kind
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MossResponseCheckpoint {
    reference: ArtifactRef,
    binding: ResponseBinding,
}

impl MossResponseCheckpoint {
    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }
    pub fn binding(&self) -> &ResponseBinding {
        &self.binding
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointData {
    schema_version: u32,
    recording_id: Uuid,
    generation: Uuid,
    plan: ArtifactRef,
    source: SourceAudioIdentity,
    window_request_sha256: Vec<String>,
    anchor_request_sha256: String,
    completed_windows: Vec<MossResponseCheckpoint>,
    anchors: Option<MossResponseCheckpoint>,
    active_claim: Option<MossEffectClaim>,
    /// Written before the immutable response file; omitted for old ledgers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_response: Option<MossResponseCheckpoint>,
    finalized_transcript_sha256: Option<String>,
}

/// Private fields and validated deserialization prevent an unchecked prefix or
/// effect claim from entering the processing ledger through its JSON reader.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct MossCheckpoint(CheckpointData);

impl<'de> Deserialize<'de> for MossCheckpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let checkpoint = Self(CheckpointData::deserialize(deserializer)?);
        checkpoint
            .validate_internal()
            .map_err(|failure| serde::de::Error::custom(failure.code))?;
        Ok(checkpoint)
    }
}

impl MossCheckpoint {
    pub(crate) fn new(
        plan: &LocalMossPlan,
        reference: ArtifactRef,
    ) -> Result<Self, ProcessingError> {
        plan.require_executable()
            .map_err(|error| fail(error.code))?;
        let checkpoint = Self(CheckpointData {
            schema_version: 1,
            recording_id: plan.spec().recording_id,
            generation: reference.generation,
            plan: reference,
            source: plan.spec().source.clone(),
            window_request_sha256: plan
                .windows()
                .iter()
                .map(|window| window.request_sha256().into())
                .collect(),
            anchor_request_sha256: plan.diarization_request_sha256().into(),
            completed_windows: Vec::new(),
            anchors: None,
            active_claim: None,
            pending_response: None,
            finalized_transcript_sha256: None,
        });
        checkpoint.validate_internal()?;
        if checkpoint.0.plan.sha256 != plan.plan_sha256()
            || checkpoint.0.plan.size_bytes != plan.plan_bytes().len() as u64
        {
            return Err(fail("moss_plan_reference_mismatch"));
        }
        Ok(checkpoint)
    }

    pub fn generation(&self) -> Uuid {
        self.0.generation
    }
    pub fn plan_ref(&self) -> &ArtifactRef {
        &self.0.plan
    }
    pub fn source(&self) -> &SourceAudioIdentity {
        &self.0.source
    }
    pub fn window_request_hashes(&self) -> &[String] {
        &self.0.window_request_sha256
    }
    pub fn anchor_request_sha256(&self) -> &str {
        &self.0.anchor_request_sha256
    }
    pub fn completed_windows(&self) -> &[MossResponseCheckpoint] {
        &self.0.completed_windows
    }
    pub fn anchors(&self) -> Option<&MossResponseCheckpoint> {
        self.0.anchors.as_ref()
    }
    pub fn active_claim(&self) -> Option<&MossEffectClaim> {
        self.0.active_claim.as_ref()
    }
    pub fn pending_response(&self) -> Option<&MossResponseCheckpoint> {
        self.0.pending_response.as_ref()
    }
    pub fn is_finalized(&self) -> bool {
        self.0.finalized_transcript_sha256.is_some()
    }

    /// Describes the next required stage, never permission to dispatch it.
    pub fn next_effect(&self) -> Option<MossEffectKind> {
        if self.is_finalized() {
            None
        } else if self.0.completed_windows.len() < self.0.window_request_sha256.len() {
            Some(MossEffectKind::Window(self.0.completed_windows.len()))
        } else if self.0.anchors.is_none() {
            Some(MossEffectKind::Anchors)
        } else {
            Some(MossEffectKind::Finalize)
        }
    }

    fn validate_internal(&self) -> Result<(), ProcessingError> {
        let data = &self.0;
        if data.schema_version != 1
            || data.recording_id.is_nil()
            || data.generation.is_nil()
            || data.source.duration_ms == 0
            || data.source.duration_ms >= 18_000_000
            || !data
                .source
                .relative_path
                .starts_with(&format!("inbox/{}/", data.recording_id))
            || data.window_request_sha256.is_empty()
            || data.window_request_sha256.len() > 26
            || data.completed_windows.len() > data.window_request_sha256.len()
            || !valid_hash(&data.anchor_request_sha256)
        {
            return Err(fail("invalid_moss_checkpoint"));
        }
        validate_normalized(&NormalizedArtifactCheckpoint {
            relative_path: data.source.relative_path.clone(),
            sha256: data.source.sha256.clone(),
            size_bytes: data.source.size_bytes,
        })?;
        self.validate_ref(&data.plan, ArtifactKind::Plan, MAX_PLAN_BYTES as u64)?;
        let mut hashes = HashSet::new();
        for hash in &data.window_request_sha256 {
            if !valid_hash(hash) || !hashes.insert(hash) {
                return Err(fail("invalid_moss_checkpoint"));
            }
        }
        for (index, completed) in data.completed_windows.iter().enumerate() {
            self.validate_completed(
                completed,
                ArtifactKind::Window(index as u32),
                &data.window_request_sha256[index],
                16 * 1024 * 1024,
            )?;
        }
        if let Some(anchors) = &data.anchors {
            if data.completed_windows.len() != data.window_request_sha256.len() {
                return Err(fail("invalid_moss_checkpoint"));
            }
            self.validate_completed(
                anchors,
                ArtifactKind::Anchors,
                &data.anchor_request_sha256,
                4 * 1024 * 1024,
            )?;
        }
        if let Some(hash) = &data.finalized_transcript_sha256 {
            if !valid_hash(hash) || data.anchors.is_none() || data.active_claim.is_some() {
                return Err(fail("invalid_moss_checkpoint"));
            }
        }
        if let Some(pending) = &data.pending_response {
            match self.next_effect() {
                Some(MossEffectKind::Window(index)) => self.validate_completed(
                    pending,
                    ArtifactKind::Window(index as u32),
                    &data.window_request_sha256[index],
                    16 * 1024 * 1024,
                )?,
                Some(MossEffectKind::Anchors) => self.validate_completed(
                    pending,
                    ArtifactKind::Anchors,
                    &data.anchor_request_sha256,
                    4 * 1024 * 1024,
                )?,
                _ => return Err(fail("invalid_moss_checkpoint")),
            }
        }
        if let Some(claim) = &data.active_claim {
            if claim.token.is_nil()
                || claim.owner_session.is_nil()
                || claim.recording_id != data.recording_id
                || claim.generation != data.generation
                || self.next_effect() != Some(claim.kind)
            {
                return Err(fail("invalid_moss_checkpoint"));
            }
        }
        Ok(())
    }

    pub fn validate_against(
        &self,
        recording_id: Uuid,
        normalized: &NormalizedArtifactCheckpoint,
    ) -> Result<(), ProcessingError> {
        self.validate_internal()?;
        if self.0.recording_id != recording_id
            || self.0.source.relative_path
                != format!("inbox/{recording_id}/{}", normalized.relative_path)
            || self.0.source.sha256 != normalized.sha256
            || self.0.source.size_bytes != normalized.size_bytes
        {
            return Err(fail("moss_source_identity_mismatch"));
        }
        Ok(())
    }

    pub fn validate_transcript(
        &self,
        transcript: Option<&serde_json::Value>,
    ) -> Result<(), ProcessingError> {
        match (&self.0.finalized_transcript_sha256, transcript) {
            (None, None) => Ok(()),
            (Some(expected), Some(value))
                if *expected
                    == digest(
                        &serde_json::to_vec(value).map_err(|_| fail("invalid_checkpoint"))?,
                    ) =>
            {
                Ok(())
            }
            _ => Err(fail("moss_transcript_binding_mismatch")),
        }
    }

    fn same_selection(&self, other: &Self) -> bool {
        self.0.recording_id == other.0.recording_id
            && self.0.generation == other.0.generation
            && self.0.plan == other.0.plan
            && self.0.source == other.0.source
            && self.0.window_request_sha256 == other.0.window_request_sha256
            && self.0.anchor_request_sha256 == other.0.anchor_request_sha256
    }

    fn validate_ref(
        &self,
        reference: &ArtifactRef,
        kind: ArtifactKind,
        limit: u64,
    ) -> Result<(), ProcessingError> {
        if reference.recording_id != self.0.recording_id
            || reference.generation != self.0.generation
            || reference.kind != kind
            || !valid_hash(&reference.sha256)
            || reference.size_bytes == 0
            || reference.size_bytes > limit
        {
            return Err(fail("moss_artifact_binding_mismatch"));
        }
        Ok(())
    }

    fn validate_completed(
        &self,
        completed: &MossResponseCheckpoint,
        kind: ArtifactKind,
        request_hash: &str,
        limit: u64,
    ) -> Result<(), ProcessingError> {
        self.validate_ref(&completed.reference, kind, limit)?;
        if completed.binding.plan_sha256 != self.0.plan.sha256
            || completed.binding.request_sha256 != request_hash
            || completed.binding.response_sha256 != completed.reference.sha256
        {
            return Err(fail("moss_response_binding_mismatch"));
        }
        Ok(())
    }

    fn require_claim(
        &self,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
        kind: MossEffectKind,
    ) -> Result<(), ProcessingError> {
        if claim.owner_session != owner.session_id()
            || claim.recording_id != self.0.recording_id
            || claim.generation != self.0.generation
            || claim.kind != kind
            || self.0.active_claim.as_ref() != Some(claim)
            || self.next_effect() != Some(kind)
        {
            return Err(fail("stale_moss_effect_claim"));
        }
        Ok(())
    }
}

impl ProcessingStore {
    /// An exclusive MOSS run may repeat a crashed, pinned local summary.
    /// This never resets a remote Gemini charge fence.
    pub fn recover_moss_local_summary(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state != ProcessingState::SummarySubmitting {
                return Ok(false);
            }
            if ledger.transcription_backend != TranscriptionBackend::MossLocal
                || ledger.summary_backend != SummaryBackend::QwenLocal
                || ledger.summary_json.is_some()
            {
                return Err(fail("invalid_state"));
            }
            let checkpoint = ledger
                .local_moss
                .as_ref()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            if !checkpoint.is_finalized() {
                return Err(fail("missing_checkpoint"));
            }
            self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.validate_transcript(ledger.transcript_json.as_ref())?;
            transition(ledger, ProcessingState::Summarizing)?;
            Ok(true)
        })
    }

    pub(crate) fn moss_owned_artifacts(
        &self,
        recording_id: Uuid,
        generation: Uuid,
        owner: &OwnerLease,
    ) -> Result<MossArtifacts, ProcessingError> {
        let root = self
            .root
            .parent()
            .ok_or_else(|| fail("unsafe_storage_layout"))?;
        let artifacts = MossArtifacts::open(root, recording_id, generation)
            .map_err(|failure| fail(failure.code()))?;
        artifacts
            .validate_owner(owner)
            .map_err(|failure| fail(failure.code()))?;
        Ok(artifacts)
    }

    pub fn select_full_local_moss(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        plan: &LocalMossPlan,
        reference: ArtifactRef,
        summary: LocalSummaryCheckpoint,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.select_moss(recording_id, owner, plan, reference, summary, false)
    }

    pub fn take_over_with_full_local_moss(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        plan: &LocalMossPlan,
        reference: ArtifactRef,
        summary: LocalSummaryCheckpoint,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.select_moss(recording_id, owner, plan, reference, summary, true)
    }

    fn select_moss(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        plan: &LocalMossPlan,
        reference: ArtifactRef,
        summary: LocalSummaryCheckpoint,
        takeover: bool,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if !takeover
                && (ledger
                    .miaoji
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.task_id.as_ref())
                    .is_some()
                    || matches!(
                        ledger.state,
                        ProcessingState::Submitting
                            | ProcessingState::SubmitAmbiguous
                            | ProcessingState::Polling
                    ))
            {
                return Err(fail("manual_resolution_required"));
            }
            let checkpoint = MossCheckpoint::new(plan, reference)?;
            checkpoint.validate_against(recording_id, &ledger.normalized)?;
            let artifacts =
                self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            let stored = artifacts
                .read(checkpoint.plan_ref())
                .map_err(|failure| fail(failure.code()))?;
            if stored != plan.plan_bytes() {
                return Err(fail("moss_plan_reference_mismatch"));
            }
            read_plan(&artifacts, &checkpoint)?;
            if summary.transcript_sha256.is_some() {
                return Err(fail("invalid_checkpoint"));
            }
            if ledger.transcription_backend == TranscriptionBackend::MossLocal {
                return if ledger
                    .local_moss
                    .as_ref()
                    .is_some_and(|existing| existing.same_selection(&checkpoint))
                    && ledger.local_summary.as_ref() == Some(&summary)
                    && ledger.summary_backend == SummaryBackend::QwenLocal
                    && ledger.publication_backend == PublicationBackend::LocalArchive
                    && !ledger.transcript_only_accepted
                    && ledger.state == ProcessingState::LocalTranscribing
                    && (!takeover
                        || ledger
                            .miaoji
                            .as_ref()
                            .is_some_and(|checkpoint| checkpoint.superseded))
                {
                    Ok(false)
                } else {
                    Err(fail("checkpoint_conflict"))
                };
            }
            if takeover {
                require_state(ledger, ProcessingState::Polling)?;
                if ledger.transcript_json.is_some()
                    || ledger.miaoji.as_ref().is_none_or(|checkpoint| {
                        checkpoint.task_id.is_none() || checkpoint.superseded
                    })
                {
                    return Err(fail("invalid_state"));
                }
                ledger
                    .miaoji
                    .as_mut()
                    .ok_or_else(|| fail("missing_checkpoint"))?
                    .superseded = true;
            } else {
                if !matches!(
                    ledger.state,
                    ProcessingState::Queued
                        | ProcessingState::Uploading
                        | ProcessingState::Transcribing
                        | ProcessingState::ProviderFailed
                ) || ledger.transcript_json.is_some()
                {
                    return Err(fail("invalid_state"));
                }
                ledger.miaoji = None;
            }
            ledger.transcription_backend = TranscriptionBackend::MossLocal;
            ledger.summary_backend = SummaryBackend::QwenLocal;
            ledger.publication_backend = PublicationBackend::LocalArchive;
            ledger.local_whisper = None;
            ledger.local_qwen = None;
            ledger.local_diarization = None;
            ledger.local_moss = Some(checkpoint);
            ledger.local_summary = Some(summary);
            ledger.transcript_only_accepted = false;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }

    pub fn claim_next_moss_effect(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
    ) -> Result<MossEffectClaim, ProcessingError> {
        self.with_writer(|| {
            let mut ledger = self
                .load_locked(recording_id)?
                .ok_or_else(|| fail("recording_not_found"))?;
            require_moss_active(&ledger)?;
            let checkpoint = ledger
                .local_moss
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            let artifacts =
                self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            read_plan(&artifacts, checkpoint)?;
            if checkpoint
                .active_claim()
                .is_some_and(|claim| claim.owner_session == owner.session_id())
            {
                return Err(fail("moss_effect_in_flight"));
            }
            let claim = MossEffectClaim {
                token: Uuid::new_v4(),
                owner_session: owner.session_id(),
                recording_id,
                generation: checkpoint.generation(),
                kind: checkpoint
                    .next_effect()
                    .ok_or_else(|| fail("invalid_state"))?,
            };
            checkpoint.0.active_claim = Some(claim.clone());
            bump_revision(&mut ledger)?;
            self.persist_locked(&ledger)?;
            Ok(claim)
        })
    }

    pub fn prepare_moss_window_response(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
        response: &ValidatedMossWindowResponse,
    ) -> Result<ArtifactRef, ProcessingError> {
        let MossEffectKind::Window(index) = claim.kind() else {
            return Err(fail("invalid_state"));
        };
        self.prepare_moss_response(recording_id, owner, claim, response.receipt(), Some(index))
    }

    pub fn prepare_moss_anchor_response(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
        response: &ValidatedSpeakerKitResponse,
    ) -> Result<ArtifactRef, ProcessingError> {
        if claim.kind() != MossEffectKind::Anchors {
            return Err(fail("invalid_state"));
        }
        self.prepare_moss_response(recording_id, owner, claim, response.receipt(), None)
    }

    fn prepare_moss_response(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
        receipt: &ResponseReceipt,
        index: Option<usize>,
    ) -> Result<ArtifactRef, ProcessingError> {
        let reference = ArtifactRef {
            recording_id,
            generation: claim.generation(),
            kind: index.map_or(ArtifactKind::Anchors, |index| {
                ArtifactKind::Window(index as u32)
            }),
            sha256: receipt.binding.response_sha256.clone(),
            size_bytes: receipt.response_size_bytes as u64,
        };
        self.mutate(recording_id, |ledger| {
            require_moss_active(ledger)?;
            let checkpoint = ledger
                .local_moss
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            let artifacts =
                self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.require_claim(owner, claim, claim.kind())?;
            read_plan(&artifacts, checkpoint)?;
            let pending = completed_from_receipt(reference.clone(), receipt, index)?;
            let request_hash = match claim.kind() {
                MossEffectKind::Window(index) => &checkpoint.0.window_request_sha256[index],
                MossEffectKind::Anchors => &checkpoint.0.anchor_request_sha256,
                MossEffectKind::Finalize => return Err(fail("invalid_state")),
            };
            checkpoint.validate_completed(
                &pending,
                reference.kind,
                request_hash,
                if index.is_some() {
                    16 * 1024 * 1024
                } else {
                    4 * 1024 * 1024
                },
            )?;
            if let Some(existing) = &checkpoint.0.pending_response {
                return if existing == &pending {
                    Ok(false)
                } else {
                    Err(fail("moss_response_intent_conflict"))
                };
            }
            checkpoint.0.pending_response = Some(pending);
            Ok(true)
        })?;
        Ok(reference)
    }

    /// A crash before file publication may lose only the in-memory response.
    /// Clear its intent solely after proving the canonical file is absent.
    pub fn clear_missing_moss_response(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_moss_active(ledger)?;
            let checkpoint = ledger
                .local_moss
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            let artifacts =
                self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.require_claim(owner, claim, claim.kind())?;
            let Some(pending) = &checkpoint.0.pending_response else {
                return Ok(false);
            };
            if artifacts
                .read_if_present(&pending.reference)
                .map_err(|error| fail(error.code()))?
                .is_some()
            {
                return Err(fail("moss_response_already_present"));
            }
            checkpoint.0.pending_response = None;
            Ok(true)
        })
    }

    pub fn checkpoint_moss_window(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
        reference: ArtifactRef,
        response: &ValidatedMossWindowResponse,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_moss_active(ledger)?;
            let checkpoint = ledger
                .local_moss
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            let artifacts =
                self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            let index = checkpoint.0.completed_windows.len();
            checkpoint.require_claim(owner, claim, MossEffectKind::Window(index))?;
            let completed = completed_from_receipt(reference, response.receipt(), Some(index))?;
            if checkpoint
                .0
                .pending_response
                .as_ref()
                .is_some_and(|pending| pending != &completed)
            {
                return Err(fail("moss_response_intent_conflict"));
            }
            checkpoint.validate_completed(
                &completed,
                ArtifactKind::Window(index as u32),
                &checkpoint.0.window_request_sha256[index],
                16 * 1024 * 1024,
            )?;
            let plan = read_plan(&artifacts, checkpoint)?;
            let bytes = artifacts
                .read(&completed.reference)
                .map_err(|failure| fail(failure.code()))?;
            ValidatedMossWindowResponse::decode(&plan, index, completed.binding.clone(), &bytes)
                .map_err(|failure| fail(failure.code))?;
            checkpoint.0.completed_windows.push(completed);
            checkpoint.0.pending_response = None;
            checkpoint.0.active_claim = None;
            Ok(true)
        })
    }

    pub fn checkpoint_moss_anchors(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
        reference: ArtifactRef,
        response: &ValidatedSpeakerKitResponse,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_moss_active(ledger)?;
            let checkpoint = ledger
                .local_moss
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            let artifacts =
                self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.require_claim(owner, claim, MossEffectKind::Anchors)?;
            let completed = completed_from_receipt(reference, response.receipt(), None)?;
            if checkpoint
                .0
                .pending_response
                .as_ref()
                .is_some_and(|pending| pending != &completed)
            {
                return Err(fail("moss_response_intent_conflict"));
            }
            checkpoint.validate_completed(
                &completed,
                ArtifactKind::Anchors,
                &checkpoint.0.anchor_request_sha256,
                4 * 1024 * 1024,
            )?;
            let plan = read_plan(&artifacts, checkpoint)?;
            let bytes = artifacts
                .read(&completed.reference)
                .map_err(|failure| fail(failure.code()))?;
            ValidatedSpeakerKitResponse::decode(&plan, completed.binding.clone(), &bytes)
                .map_err(|failure| fail(failure.code))?;
            checkpoint.0.anchors = Some(completed);
            checkpoint.0.pending_response = None;
            checkpoint.0.active_claim = None;
            Ok(true)
        })
    }

    pub fn finalize_moss_transcript(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
        result: &FinalizedMossTranscript,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_moss_active(ledger)?;
            let checkpoint = ledger
                .local_moss
                .as_ref()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            let artifacts =
                self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.require_claim(owner, claim, MossEffectKind::Finalize)?;
            read_plan(&artifacts, checkpoint)?;
            let evidence = result.evidence();
            if evidence.plan_sha256 != checkpoint.0.plan.sha256
                || evidence.window_receipts.len() != checkpoint.0.completed_windows.len()
            {
                return Err(fail("moss_finalization_binding_mismatch"));
            }
            for (index, (completed, receipt)) in checkpoint
                .0
                .completed_windows
                .iter()
                .zip(&evidence.window_receipts)
                .enumerate()
            {
                if completed_from_receipt(completed.reference.clone(), receipt, Some(index))?
                    != *completed
                {
                    return Err(fail("moss_finalization_binding_mismatch"));
                }
                artifacts
                    .read(&completed.reference)
                    .map_err(|failure| fail(failure.code()))?;
            }
            let anchor = checkpoint
                .0
                .anchors
                .as_ref()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            if completed_from_receipt(
                anchor.reference.clone(),
                &evidence.diarization_receipt,
                None,
            )? != *anchor
            {
                return Err(fail("moss_finalization_binding_mismatch"));
            }
            artifacts
                .read(&anchor.reference)
                .map_err(|failure| fail(failure.code()))?;
            let transcript = result.transcript_json();
            validate_json(transcript)?;
            let summary_text = transcript_for_summary(transcript)?;
            let summary_hash = digest(summary_text.as_bytes());
            let summary = ledger
                .local_summary
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            if summary
                .transcript_sha256
                .as_ref()
                .is_some_and(|hash| *hash != summary_hash)
                || ledger.transcript_json.is_some()
            {
                return Err(fail("checkpoint_conflict"));
            }
            summary.transcript_sha256 = Some(summary_hash);
            ledger.transcript_json = Some(transcript.clone());
            let checkpoint = ledger
                .local_moss
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            checkpoint.0.finalized_transcript_sha256 = Some(digest(result.json_bytes()));
            checkpoint.0.active_claim = None;
            transition(ledger, ProcessingState::Summarizing)?;
            Ok(true)
        })
    }

    pub fn mark_moss_effect_failed(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossEffectClaim,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_moss_active(ledger)?;
            let checkpoint = ledger
                .local_moss
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.require_claim(owner, claim, claim.kind)?;
            checkpoint.0.active_claim = None;
            transition(ledger, ProcessingState::ProviderFailed)?;
            Ok(true)
        })
    }

    pub fn retry_local_moss(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::ProviderFailed)?;
            if ledger.transcription_backend != TranscriptionBackend::MossLocal
                || ledger.transcript_json.is_some()
            {
                return Err(fail("invalid_state"));
            }
            let checkpoint = ledger
                .local_moss
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            let artifacts =
                self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            read_plan(&artifacts, checkpoint)?;
            checkpoint.0.active_claim = None;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }
}

fn read_plan(
    artifacts: &MossArtifacts,
    checkpoint: &MossCheckpoint,
) -> Result<LocalMossPlan, ProcessingError> {
    let bytes = artifacts
        .read(checkpoint.plan_ref())
        .map_err(|failure| fail(failure.code()))?;
    let plan = LocalMossPlan::from_json(&bytes).map_err(|failure| fail(failure.code))?;
    if !checkpoint.same_selection(&MossCheckpoint::new(&plan, checkpoint.plan_ref().clone())?) {
        return Err(fail("moss_plan_reference_mismatch"));
    }
    Ok(plan)
}

fn completed_from_receipt(
    reference: ArtifactRef,
    receipt: &ResponseReceipt,
    index: Option<usize>,
) -> Result<MossResponseCheckpoint, ProcessingError> {
    if receipt.window_index != index
        || receipt.response_size_bytes as u64 != reference.size_bytes
        || receipt.binding.response_sha256 != reference.sha256
    {
        return Err(fail("moss_response_binding_mismatch"));
    }
    Ok(MossResponseCheckpoint {
        reference,
        binding: receipt.binding.clone(),
    })
}

fn require_moss_active(ledger: &ProcessingLedger) -> Result<(), ProcessingError> {
    require_state(ledger, ProcessingState::LocalTranscribing)?;
    if ledger.transcription_backend != TranscriptionBackend::MossLocal
        || ledger.transcript_json.is_some()
    {
        return Err(fail("invalid_state"));
    }
    Ok(())
}
fn fail(code: &'static str) -> ProcessingError {
    ProcessingError::new(code)
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
