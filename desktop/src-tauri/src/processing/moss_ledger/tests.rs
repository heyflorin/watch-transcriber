use std::{
    sync::{Arc, Barrier},
    thread,
};

use echowall_local_moss_protocol as moss;
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::processing::{local_moss, local_whisper::*, ResumeAction};

struct Fixture {
    _temp: TempDir,
    store: ProcessingStore,
    artifacts: MossArtifacts,
    plan: LocalMossPlan,
    recording_id: Uuid,
    generation: Uuid,
}

impl Fixture {
    fn new(two_windows: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let store = ProcessingStore::open(
            &app,
            &temp.path().join("inbox"),
            &temp.path().join("archive"),
        )
        .unwrap();
        let recording_id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let frame_lengths = if two_windows {
            vec![moss::windows::MAX_WINDOW_FRAMES, 48_000]
        } else {
            vec![48_000]
        };
        let frames: u64 = frame_lengths.iter().sum();
        let source = SourceAudioIdentity {
            relative_path: format!("inbox/{recording_id}/derived/mixed.wav"),
            sha256: digest(b"synthetic normalized source"),
            size_bytes: 4096,
            duration_ms: frames / 16 + 64,
        };
        let mut start = 0;
        let windows = frame_lengths
            .into_iter()
            .enumerate()
            .map(|(index, length)| {
                let result = local_moss::MossWindowRequestSpec {
                    index,
                    start_frame: start,
                    end_frame: start + length,
                    request: moss::MossRequest {
                        schema_version: moss::PROTOCOL_VERSION,
                        recording_id,
                        runtime_id: moss::RUNTIME_ID.into(),
                        model_id: moss::MODEL_ID.into(),
                        model_revision: moss::MODEL_REVISION.into(),
                        model_sha256: moss::MODEL_SHA256.into(),
                        model_size_bytes: moss::MODEL_SIZE_BYTES,
                        timing_policy: moss::COALESCING_TIMING_POLICY_V2.into(),
                        audio_relative_path: format!(
                            "inbox/{recording_id}/derived/moss_{generation}_{index}.wav"
                        ),
                        audio_sha256: digest(format!("synthetic window {index}").as_bytes()),
                        audio_size_bytes: length * 2 + 44,
                        audio_duration_ms: length / 16,
                        language: None,
                    },
                };
                start += length;
                result
            })
            .collect();
        let diarization_request = LocalDiarizationRequest {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id,
            pack_id: "speakerkit-v1".into(),
            quality_preset: LOCAL_DIARIZATION_SPEAKERKIT_PRESET.into(),
            model_files: LOCAL_DIARIZATION_SPEAKERKIT_FILES
                .iter()
                .map(|path| LocalModelFileIdentity {
                    relative_path: (*path).into(),
                    sha256: digest(path.as_bytes()),
                    size_bytes: 1,
                })
                .collect(),
            audio_relative_path: "derived/mixed.wav".into(),
            audio_sha256: source.sha256.clone(),
            audio_size_bytes: source.size_bytes,
            audio_duration_ms: source.duration_ms,
            expected_speaker_count: None,
        };
        let plan = LocalMossPlan::new(local_moss::LocalMossPlanSpec {
            schema_version: local_moss::LOCAL_MOSS_PLAN_VERSION,
            mapping_policy: Some(local_moss::COMPOSED_MAPPING_POLICY.into()),
            recording_id,
            source,
            pcm_sample_rate: 16_000,
            pcm_source_frames: frames,
            pcm_quantization_policy: None,
            window_policy: moss::windows::QUIET_WINDOW_POLICY.into(),
            windows,
            diarization_request,
        })
        .unwrap();
        store
            .enqueue(
                recording_id,
                NormalizedArtifactCheckpoint {
                    relative_path: "derived/mixed.wav".into(),
                    sha256: plan.spec().source.sha256.clone(),
                    size_bytes: plan.spec().source.size_bytes,
                },
            )
            .unwrap();
        let artifacts = MossArtifacts::open(&app, recording_id, generation).unwrap();
        Self {
            _temp: temp,
            store,
            artifacts,
            plan,
            recording_id,
            generation,
        }
    }

    fn summary(&self) -> LocalSummaryCheckpoint {
        LocalSummaryCheckpoint {
            model_id: "qwen3.8-27b-ud-q4-k-xl".into(),
            model_sha256: digest(b"synthetic summary model"),
            model_size_bytes: 1,
            prompt_version: echowall_local_summary_protocol::LOCAL_SUMMARY_PROMPT_VERSION.into(),
            transcript_sha256: None,
        }
    }

    fn select(&self, owner: &OwnerLease) -> ProcessingLedger {
        let reference = self
            .artifacts
            .write(owner, ArtifactKind::Plan, self.plan.plan_bytes())
            .unwrap();
        self.store
            .select_full_local_moss(
                self.recording_id,
                owner,
                &self.plan,
                reference,
                self.summary(),
            )
            .unwrap()
    }

    fn window_bytes(&self, index: usize) -> Vec<u8> {
        let request = self.plan.windows()[index].request();
        let segments = if index == 0 {
            vec![
                moss::MossSegment {
                    start_ms: 0,
                    end_ms: 1800,
                    speaker_id: 1,
                    text: "first 世界".into(),
                },
                moss::MossSegment {
                    start_ms: 1000,
                    end_ms: 3000,
                    speaker_id: 2,
                    text: "second".into(),
                },
            ]
        } else {
            vec![moss::MossSegment {
                start_ms: 0,
                end_ms: 2000,
                speaker_id: 1,
                text: "later window".into(),
            }]
        };
        serde_json::to_vec(&moss::MossResponse {
            schema_version: moss::PROTOCOL_VERSION,
            recording_id: self.recording_id,
            runtime_id: request.runtime_id.clone(),
            model_id: request.model_id.clone(),
            model_sha256: request.model_sha256.clone(),
            audio_sha256: request.audio_sha256.clone(),
            timing_policy: request.timing_policy.clone(),
            complete: true,
            segments,
        })
        .unwrap()
    }

    fn binding(&self, request_sha256: &str, bytes: &[u8]) -> ResponseBinding {
        ResponseBinding {
            plan_sha256: self.plan.plan_sha256().into(),
            request_sha256: request_sha256.into(),
            response_sha256: digest(bytes),
        }
    }

    fn window(
        &self,
        owner: &OwnerLease,
        index: usize,
    ) -> (ArtifactRef, ValidatedMossWindowResponse) {
        let bytes = self.window_bytes(index);
        let binding = self.binding(self.plan.windows()[index].request_sha256(), &bytes);
        let response =
            ValidatedMossWindowResponse::decode(&self.plan, index, binding, &bytes).unwrap();
        let reference = self
            .artifacts
            .write(owner, ArtifactKind::Window(index as u32), &bytes)
            .unwrap();
        (reference, response)
    }

    fn anchors(&self, owner: &OwnerLease) -> (ArtifactRef, ValidatedSpeakerKitResponse) {
        let request = self.plan.diarization_request();
        let bytes = serde_json::to_vec(&LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: self.recording_id,
            pack_id: request.pack_id.clone(),
            quality_preset: request.quality_preset.clone(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 2,
            segments: vec![
                LocalDiarizationSegment {
                    start_ms: 0,
                    end_ms: 1500,
                    speaker_slot: 1,
                    confidence_milli: 0,
                },
                LocalDiarizationSegment {
                    start_ms: 1500,
                    end_ms: request.audio_duration_ms,
                    speaker_slot: 2,
                    confidence_milli: 0,
                },
            ],
        })
        .unwrap();
        let response = ValidatedSpeakerKitResponse::decode(
            &self.plan,
            self.binding(self.plan.diarization_request_sha256(), &bytes),
            &bytes,
        )
        .unwrap();
        let reference = self
            .artifacts
            .write(owner, ArtifactKind::Anchors, &bytes)
            .unwrap();
        (reference, response)
    }

    fn ready_to_finalize(&self, owner: &OwnerLease) -> (MossEffectClaim, FinalizedMossTranscript) {
        let mut responses = Vec::new();
        for index in 0..self.plan.windows().len() {
            let claim = self
                .store
                .claim_next_moss_effect(self.recording_id, owner)
                .unwrap();
            assert_eq!(claim.kind(), MossEffectKind::Window(index));
            let (reference, response) = self.window(owner, index);
            self.store
                .checkpoint_moss_window(self.recording_id, owner, &claim, reference, &response)
                .unwrap();
            responses.push(response);
        }
        let claim = self
            .store
            .claim_next_moss_effect(self.recording_id, owner)
            .unwrap();
        assert_eq!(claim.kind(), MossEffectKind::Anchors);
        let (reference, anchors) = self.anchors(owner);
        self.store
            .checkpoint_moss_anchors(self.recording_id, owner, &claim, reference, &anchors)
            .unwrap();
        let bundle = local_moss::CompleteMossResponses::new(&self.plan, responses).unwrap();
        let sealed = local_moss::finalize(&self.plan, &bundle, &anchors).unwrap();
        let claim = self
            .store
            .claim_next_moss_effect(self.recording_id, owner)
            .unwrap();
        assert_eq!(claim.kind(), MossEffectKind::Finalize);
        (claim, sealed)
    }
}

#[test]
fn crash_reclaim_uses_fresh_owner_session_and_reuses_completed_prefix() {
    let fixture = Fixture::new(true);
    let owner = fixture.artifacts.try_owner().unwrap();
    fixture.select(&owner);
    let first = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    let (reference, response) = fixture.window(&owner, 0);
    fixture
        .store
        .checkpoint_moss_window(
            fixture.recording_id,
            &owner,
            &first,
            reference.clone(),
            &response,
        )
        .unwrap();
    let interrupted = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    assert_eq!(interrupted.kind(), MossEffectKind::Window(1));
    let previous_session = owner.session_id();
    drop(owner);
    let owner = fixture.artifacts.try_owner().unwrap();
    assert_ne!(owner.session_id(), previous_session);
    let resumed = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    assert_eq!(resumed.kind(), MossEffectKind::Window(1));
    assert_ne!(resumed.token(), interrupted.token());
    let ledger = fixture.store.load(fixture.recording_id).unwrap();
    let checkpoint = ledger.local_moss.as_ref().unwrap();
    assert_eq!(checkpoint.completed_windows().len(), 1);
    assert_eq!(checkpoint.completed_windows()[0].reference(), &reference);
    let (reference, response) = fixture.window(&owner, 1);
    let before = fixture.store.load(fixture.recording_id).unwrap();
    assert_eq!(
        fixture
            .store
            .checkpoint_moss_window(
                fixture.recording_id,
                &owner,
                &interrupted,
                reference.clone(),
                &response
            )
            .unwrap_err()
            .code,
        "stale_moss_effect_claim"
    );
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), before);
    fixture
        .store
        .checkpoint_moss_window(fixture.recording_id, &owner, &resumed, reference, &response)
        .unwrap();
}

#[test]
fn pending_response_is_identity_bound_idempotent_and_old_claims_cannot_complete_it() {
    let fixture = Fixture::new(false);
    let owner = fixture.artifacts.try_owner().unwrap();
    let selected = fixture.select(&owner);
    assert!(!serde_json::to_value(&selected).unwrap()["local_moss"]
        .as_object()
        .unwrap()
        .contains_key("pending_response"));
    let claim = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    let bytes = fixture.window_bytes(0);
    let response = ValidatedMossWindowResponse::decode(
        &fixture.plan,
        0,
        fixture.binding(fixture.plan.windows()[0].request_sha256(), &bytes),
        &bytes,
    )
    .unwrap();
    let reference = fixture
        .store
        .prepare_moss_window_response(fixture.recording_id, &owner, &claim, &response)
        .unwrap();
    assert!(fixture
        .artifacts
        .read_if_present(&reference)
        .unwrap()
        .is_none());
    let prepared = fixture.store.load(fixture.recording_id).unwrap();
    fixture
        .store
        .prepare_moss_window_response(fixture.recording_id, &owner, &claim, &response)
        .unwrap();
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), prepared);
    let mut invalid = serde_json::to_value(&prepared).unwrap();
    invalid["local_moss"]["pending_response"]["binding"]["request_sha256"] =
        json!(digest(b"wrong request"));
    assert!(serde_json::from_value::<ProcessingLedger>(invalid).is_err());
    fixture
        .artifacts
        .write(&owner, ArtifactKind::Window(0), &bytes)
        .unwrap();
    assert_eq!(
        fixture
            .store
            .clear_missing_moss_response(fixture.recording_id, &owner, &claim)
            .unwrap_err()
            .code,
        "moss_response_already_present"
    );
    drop(owner);
    let owner = fixture.artifacts.try_owner().unwrap();
    let recovered = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    let before = fixture.store.load(fixture.recording_id).unwrap();
    assert!(fixture
        .store
        .prepare_moss_window_response(fixture.recording_id, &owner, &claim, &response)
        .is_err());
    assert!(fixture
        .store
        .clear_missing_moss_response(fixture.recording_id, &owner, &claim)
        .is_err());
    assert!(fixture
        .store
        .checkpoint_moss_window(
            fixture.recording_id,
            &owner,
            &claim,
            reference.clone(),
            &response
        )
        .is_err());
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), before);
    let complete = fixture
        .store
        .checkpoint_moss_window(
            fixture.recording_id,
            &owner,
            &recovered,
            reference,
            &response,
        )
        .unwrap();
    assert!(complete.local_moss.unwrap().pending_response().is_none());
}

#[test]
fn cancellation_keeps_intent_for_inspection_but_rejects_late_completion() {
    let fixture = Fixture::new(false);
    let owner = fixture.artifacts.try_owner().unwrap();
    fixture.select(&owner);
    let claim = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    let (reference, response) = fixture.window(&owner, 0);
    fixture
        .store
        .prepare_moss_window_response(fixture.recording_id, &owner, &claim, &response)
        .unwrap();
    let canceled = fixture.store.cancel(fixture.recording_id).unwrap();
    assert!(canceled
        .local_moss
        .as_ref()
        .unwrap()
        .pending_response()
        .is_some());
    assert!(fixture
        .store
        .checkpoint_moss_window(fixture.recording_id, &owner, &claim, reference, &response)
        .is_err());
    assert!(fixture
        .store
        .clear_missing_moss_response(fixture.recording_id, &owner, &claim)
        .is_err());
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), canceled);
}

#[test]
fn concurrent_and_repeated_same_owner_claims_have_exactly_one_winner() {
    let fixture = Fixture::new(false);
    let owner = Arc::new(fixture.artifacts.try_owner().unwrap());
    fixture.select(&owner);
    let barrier = Arc::new(Barrier::new(3));
    let tasks: Vec<_> = (0..2)
        .map(|_| {
            let store = fixture.store.clone();
            let owner = owner.clone();
            let barrier = barrier.clone();
            let id = fixture.recording_id;
            thread::spawn(move || {
                barrier.wait();
                store.claim_next_moss_effect(id, &owner)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = tasks.into_iter().map(|task| task.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .next()
            .unwrap()
            .code,
        "moss_effect_in_flight"
    );
    let before = fixture.store.load(fixture.recording_id).unwrap();
    assert_eq!(
        fixture
            .store
            .claim_next_moss_effect(fixture.recording_id, &owner)
            .unwrap_err()
            .code,
        "moss_effect_in_flight"
    );
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), before);
    assert!(fixture.artifacts.try_owner().is_err());
}

#[test]
fn failure_retry_replaces_token_and_late_old_completion_cannot_mutate() {
    let fixture = Fixture::new(false);
    let owner = fixture.artifacts.try_owner().unwrap();
    fixture.select(&owner);
    let old = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    fixture
        .store
        .mark_moss_effect_failed(fixture.recording_id, &owner, &old)
        .unwrap();
    assert!(fixture.store.retry_provider(fixture.recording_id).is_err());
    fixture
        .store
        .retry_local_moss(fixture.recording_id, &owner)
        .unwrap();
    let current = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    assert_ne!(old.token(), current.token());
    let (reference, response) = fixture.window(&owner, 0);
    let before = fixture.store.load(fixture.recording_id).unwrap();
    assert!(fixture
        .store
        .checkpoint_moss_window(
            fixture.recording_id,
            &owner,
            &old,
            reference.clone(),
            &response
        )
        .is_err());
    assert!(fixture
        .store
        .mark_moss_effect_failed(fixture.recording_id, &owner, &old)
        .is_err());
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), before);
    fixture
        .store
        .checkpoint_moss_window(fixture.recording_id, &owner, &current, reference, &response)
        .unwrap();
}

#[test]
fn cancellation_fences_even_an_already_sealed_finalization() {
    let fixture = Fixture::new(false);
    let owner = fixture.artifacts.try_owner().unwrap();
    fixture.select(&owner);
    let (claim, sealed) = fixture.ready_to_finalize(&owner);
    let canceled = fixture.store.cancel(fixture.recording_id).unwrap();
    assert_eq!(canceled.state, ProcessingState::CanceledBeforeUpload);
    assert!(fixture
        .store
        .finalize_moss_transcript(fixture.recording_id, &owner, &claim, &sealed)
        .is_err());
    assert!(fixture
        .store
        .mark_moss_effect_failed(fixture.recording_id, &owner, &claim)
        .is_err());
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), canceled);
    assert!(canceled.transcript_json.is_none());
    assert!(canceled.local_summary.unwrap().transcript_sha256.is_none());
}

#[test]
fn overlapping_canonical_data_reaches_same_summary_digest_and_stored_transcript() {
    let fixture = Fixture::new(false);
    let owner = fixture.artifacts.try_owner().unwrap();
    fixture.select(&owner);
    let (claim, sealed) = fixture.ready_to_finalize(&owner);
    let finished = fixture
        .store
        .finalize_moss_transcript(fixture.recording_id, &owner, &claim, &sealed)
        .unwrap();
    assert_eq!(finished.state, ProcessingState::Summarizing);
    let transcript = finished.transcript_json.as_ref().unwrap();
    assert_eq!(transcript, sealed.transcript_json());
    assert_eq!(transcript[0]["end_time"], 1800);
    assert_eq!(transcript[1]["start_time"], 1000);
    assert_eq!(transcript[0]["stt_backend"], "moss_local");
    assert_eq!(
        finished
            .local_summary
            .as_ref()
            .unwrap()
            .transcript_sha256
            .as_deref(),
        Some(digest(transcript_for_summary(transcript).unwrap().as_bytes()).as_str())
    );
    let checkpoint = finished.local_moss.as_ref().unwrap();
    assert!(checkpoint.is_finalized());
    assert!(checkpoint.active_claim().is_none());
    assert!(checkpoint.next_effect().is_none());
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), finished);
    assert!(fixture
        .store
        .finalize_moss_transcript(fixture.recording_id, &owner, &claim, &sealed)
        .is_err());
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::Summarize
    );
}

#[test]
fn references_need_real_sidecars_and_source_namespace_is_exact() {
    let fixture = Fixture::new(false);
    let owner = fixture.artifacts.try_owner().unwrap();
    let fake = ArtifactRef {
        recording_id: fixture.recording_id,
        generation: fixture.generation,
        kind: ArtifactKind::Plan,
        sha256: fixture.plan.plan_sha256().into(),
        size_bytes: fixture.plan.plan_bytes().len() as u64,
    };
    let before = fixture.store.load(fixture.recording_id).unwrap();
    assert!(fixture
        .store
        .select_full_local_moss(
            fixture.recording_id,
            &owner,
            &fixture.plan,
            fake,
            fixture.summary()
        )
        .is_err());
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), before);
    let selected = fixture.select(&owner);
    let mut wrong_namespace = selected.normalized.clone();
    wrong_namespace.relative_path = fixture.plan.spec().source.relative_path.clone();
    assert!(selected
        .local_moss
        .as_ref()
        .unwrap()
        .validate_against(fixture.recording_id, &wrong_namespace)
        .is_err());
    let claim = fixture
        .store
        .claim_next_moss_effect(fixture.recording_id, &owner)
        .unwrap();
    let bytes = fixture.window_bytes(0);
    let response = ValidatedMossWindowResponse::decode(
        &fixture.plan,
        0,
        fixture.binding(fixture.plan.windows()[0].request_sha256(), &bytes),
        &bytes,
    )
    .unwrap();
    let fake = ArtifactRef {
        recording_id: fixture.recording_id,
        generation: selected.local_moss.as_ref().unwrap().generation(),
        kind: ArtifactKind::Window(0),
        sha256: digest(&bytes),
        size_bytes: bytes.len() as u64,
    };
    let before = fixture.store.load(fixture.recording_id).unwrap();
    assert!(fixture
        .store
        .checkpoint_moss_window(fixture.recording_id, &owner, &claim, fake, &response)
        .is_err());
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), before);
    let mut serialized = serde_json::to_value(before.local_moss.as_ref().unwrap()).unwrap();
    serialized["window_request_sha256"] = json!([]);
    assert!(serde_json::from_value::<MossCheckpoint>(serialized).is_err());
}

#[test]
fn accepted_or_ambiguous_remote_work_requires_explicit_takeover_fence() {
    let fixture = Fixture::new(false);
    let owner = fixture.artifacts.try_owner().unwrap();
    let reference = fixture
        .artifacts
        .write(&owner, ArtifactKind::Plan, fixture.plan.plan_bytes())
        .unwrap();
    fixture.store.begin_upload(fixture.recording_id).unwrap();
    fixture
        .store
        .checkpoint_tos_object(
            fixture.recording_id,
            "bucket".into(),
            "version".into(),
            "etag".into(),
            fixture.plan.spec().source.sha256.clone(),
            fixture.plan.spec().source.size_bytes,
        )
        .unwrap();
    fixture
        .store
        .begin_miaoji_submit(fixture.recording_id)
        .unwrap();
    assert_eq!(
        fixture
            .store
            .select_full_local_moss(
                fixture.recording_id,
                &owner,
                &fixture.plan,
                reference.clone(),
                fixture.summary()
            )
            .unwrap_err()
            .code,
        "manual_resolution_required"
    );
    assert!(fixture
        .store
        .take_over_with_full_local_moss(
            fixture.recording_id,
            &owner,
            &fixture.plan,
            reference.clone(),
            fixture.summary()
        )
        .is_err());
    fixture
        .store
        .checkpoint_miaoji_task(fixture.recording_id, "accepted-task".into())
        .unwrap();
    assert!(fixture
        .store
        .select_full_local_moss(
            fixture.recording_id,
            &owner,
            &fixture.plan,
            reference.clone(),
            fixture.summary()
        )
        .is_err());
    let takeover = fixture
        .store
        .take_over_with_full_local_moss(
            fixture.recording_id,
            &owner,
            &fixture.plan,
            reference,
            fixture.summary(),
        )
        .unwrap();
    assert_eq!(takeover.state, ProcessingState::LocalTranscribing);
    assert!(takeover.miaoji.as_ref().unwrap().superseded);
    assert_eq!(
        takeover.miaoji.as_ref().unwrap().task_id.as_deref(),
        Some("accepted-task")
    );
    assert!(fixture
        .store
        .checkpoint_transcript(fixture.recording_id, json!({"text":"late remote result"}))
        .is_err());
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), takeover);
}
