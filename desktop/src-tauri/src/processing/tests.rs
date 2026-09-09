use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use serde_json::json;
use tempfile::TempDir;

use super::*;

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn legacy_ledger_roundtrip_does_not_add_an_unrecognized_moss_field() {
    let fixture = Fixture::new();
    let ledger = fixture.enqueue();
    let bytes = serde_json::to_vec(&ledger).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value.get("local_moss").is_none());
    let restored: ProcessingLedger = serde_json::from_slice(&bytes).unwrap();
    restored.validate().unwrap();
    assert!(restored.local_moss.is_none());
    assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
}

struct Fixture {
    _temp: TempDir,
    store: ProcessingStore,
    recording_id: Uuid,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let inbox = temp.path().join("inbox");
        let archive = temp.path().join("archive");
        fs::create_dir_all(&inbox).unwrap();
        fs::create_dir_all(&archive).unwrap();
        let store = ProcessingStore::open(&app, &inbox, &archive).unwrap();
        Self {
            _temp: temp,
            store,
            recording_id: Uuid::new_v4(),
        }
    }

    fn normalized(&self) -> NormalizedArtifactCheckpoint {
        NormalizedArtifactCheckpoint {
            relative_path: "derived/mixed.wav".to_owned(),
            sha256: SHA.to_owned(),
            size_bytes: 44_100,
        }
    }

    fn enqueue(&self) -> ProcessingLedger {
        self.store
            .enqueue(self.recording_id, self.normalized())
            .unwrap()
    }

    fn uploaded(&self) -> ProcessingLedger {
        self.enqueue();
        self.store.begin_upload(self.recording_id).unwrap();
        self.store
            .checkpoint_tos_object(
                self.recording_id,
                "private-audio".to_owned(),
                "version-1".to_owned(),
                "etag-1".to_owned(),
                SHA.to_owned(),
                44_100,
            )
            .unwrap()
    }

    fn polling(&self) -> ProcessingLedger {
        self.uploaded();
        self.store.begin_miaoji_submit(self.recording_id).unwrap();
        self.store
            .checkpoint_miaoji_task(self.recording_id, "task-1".to_owned())
            .unwrap()
    }

    fn publishing(&self, modes: &[RetryMode]) -> ProcessingLedger {
        self.polling();
        self.store
            .checkpoint_transcript(self.recording_id, json!({"utterances": []}))
            .unwrap();
        assert_eq!(
            self.store.resume(self.recording_id).unwrap(),
            ResumeAction::Summarize
        );
        self.store
            .checkpoint_summary(self.recording_id, json!({"title": "fixture"}))
            .unwrap();
        self.store
            .plan_publication(
                self.recording_id,
                modes
                    .iter()
                    .enumerate()
                    .map(|(index, mode)| PublicationTargetPlan {
                        id: format!("target-{index}"),
                        retry_mode: *mode,
                        required: true,
                    })
                    .collect(),
            )
            .unwrap()
    }

    fn proof(&self, locator: &str) -> PublicationProof {
        PublicationProof {
            locator: locator.to_owned(),
            version: "v1".to_owned(),
            sha256: SHA.to_owned(),
            size_bytes: 44_100,
        }
    }

    fn backup(&self) -> CanonicalBackupCheckpoint {
        CanonicalBackupCheckpoint {
            locator: "backup://normalized".to_owned(),
            version_id: "backup-v1".to_owned(),
            sha256: SHA.to_owned(),
            size_bytes: 44_100,
            proof_json: json!({"head": "verified"}),
        }
    }
}

fn qwen_local_checkpoints(
    duration_ms: u64,
) -> (
    LocalQwenCheckpoint,
    LocalDiarizationCheckpoint,
    LocalSummaryCheckpoint,
) {
    use echowall_local_qwen_protocol::{
        LocalQwenModelFileIdentity, LOCAL_QWEN_ALIGNER_FILES, LOCAL_QWEN_ALIGNER_MODEL_ID,
        LOCAL_QWEN_ALIGNER_REVISION, LOCAL_QWEN_ASR_FILES, LOCAL_QWEN_ASR_MODEL_ID,
        LOCAL_QWEN_ASR_REVISION, LOCAL_QWEN_CHUNK_DURATION_MS, LOCAL_QWEN_CHUNK_POLICY,
        LOCAL_QWEN_RUNTIME_ID, LOCAL_QWEN_SPLIT_SEARCH_MS,
    };

    let identities = |paths: &[&str]| {
        paths
            .iter()
            .map(|path| LocalQwenModelFileIdentity {
                relative_path: (*path).to_owned(),
                sha256: "b".repeat(64),
                size_bytes: 1,
            })
            .collect()
    };
    (
        LocalQwenCheckpoint {
            runtime_id: LOCAL_QWEN_RUNTIME_ID.to_owned(),
            asr_model_id: LOCAL_QWEN_ASR_MODEL_ID.to_owned(),
            asr_model_revision: LOCAL_QWEN_ASR_REVISION.to_owned(),
            asr_model_files: identities(&LOCAL_QWEN_ASR_FILES),
            aligner_model_id: LOCAL_QWEN_ALIGNER_MODEL_ID.to_owned(),
            aligner_model_revision: LOCAL_QWEN_ALIGNER_REVISION.to_owned(),
            aligner_model_files: identities(&LOCAL_QWEN_ALIGNER_FILES),
            audio_duration_ms: duration_ms,
            language: None,
            chunk_policy: LOCAL_QWEN_CHUNK_POLICY.to_owned(),
            chunk_duration_ms: LOCAL_QWEN_CHUNK_DURATION_MS,
            split_search_ms: LOCAL_QWEN_SPLIT_SEARCH_MS,
        },
        LocalDiarizationCheckpoint {
            pack_id: "fluid-v1".to_owned(),
            quality_preset: Some(LOCAL_DIARIZATION_QUALITY_PRESET.to_owned()),
            model_files: vec![LocalModelFileIdentity {
                relative_path: "speaker-diarization/Segmentation.mlmodelc/model.mil".to_owned(),
                sha256: "c".repeat(64),
                size_bytes: 43_063,
            }],
            expected_speaker_count: Some(2),
        },
        LocalSummaryCheckpoint {
            model_id: "qwen3.8-27b-ud-q4-k-xl".to_owned(),
            model_sha256: "d".repeat(64),
            model_size_bytes: 17_559_178_144,
            prompt_version: LOCAL_SUMMARY_PROMPT_VERSION.to_owned(),
            transcript_sha256: None,
        },
    )
}

#[test]
fn legacy_qwen_checkpoint_replays_the_original_language_policy() {
    use echowall_local_qwen_protocol::{
        LOCAL_QWEN_CHUNK_DURATION_MS, LOCAL_QWEN_LEGACY_CHUNK_POLICY, LOCAL_QWEN_SPLIT_SEARCH_MS,
    };

    let (checkpoint, _, _) = qwen_local_checkpoints(60_000);
    let mut value = serde_json::to_value(checkpoint).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("chunk_policy");
    object.remove("chunk_duration_ms");
    object.remove("split_search_ms");
    let legacy: LocalQwenCheckpoint = serde_json::from_value(value).unwrap();
    assert_eq!(legacy.chunk_policy, LOCAL_QWEN_LEGACY_CHUNK_POLICY);
    assert_eq!(legacy.chunk_duration_ms, LOCAL_QWEN_CHUNK_DURATION_MS);
    assert_eq!(legacy.split_search_ms, LOCAL_QWEN_SPLIT_SEARCH_MS);
}

#[test]
fn enqueue_is_idempotent_and_derives_object_identity_internally() {
    let fixture = Fixture::new();
    let first = fixture.enqueue();
    let duplicate = fixture.enqueue();
    assert_eq!(first, duplicate);
    assert_eq!(
        first.object_key(),
        format!("echowall/processing/v1/{}/{SHA}.wav", fixture.recording_id)
    );

    let mut collision = fixture.normalized();
    collision.size_bytes += 1;
    let error = fixture
        .store
        .enqueue(fixture.recording_id, collision)
        .unwrap_err();
    assert_eq!(error.code, "recording_collision");

    for extension in ["mp3", "m4a"] {
        let recording_id = Uuid::new_v4();
        let mut compressed = fixture.normalized();
        compressed.relative_path = format!("tracks/imported.{extension}");
        let ledger = fixture.store.enqueue(recording_id, compressed).unwrap();
        assert_eq!(
            ledger.object_key(),
            format!("echowall/processing/v1/{recording_id}/{SHA}.{extension}")
        );
    }
}

#[test]
fn crash_after_submit_fence_becomes_durable_ambiguous_and_never_auto_submits() {
    let fixture = Fixture::new();
    fixture.uploaded();
    let submitting = fixture
        .store
        .begin_miaoji_submit(fixture.recording_id)
        .unwrap();
    assert_eq!(submitting.state, ProcessingState::Submitting);
    let request_id = submitting.miaoji.unwrap().request_id;
    assert!(Uuid::parse_str(&request_id).is_ok());
    assert_eq!(
        request_id,
        super::deterministic_request_id(fixture.recording_id)
    );

    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::ManualSubmitResolution
    );
    assert_eq!(
        fixture.store.load(fixture.recording_id).unwrap().state,
        ProcessingState::SubmitAmbiguous
    );
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::ManualSubmitResolution
    );
}

#[test]
fn local_whisper_selection_is_durable_identity_bound_and_skips_remote_submit() {
    use super::local_whisper::{LocalWhisperResponse, LocalWhisperSegment};

    let fixture = Fixture::new();
    fixture.enqueue();
    fixture.store.begin_upload(fixture.recording_id).unwrap();
    let selected = fixture
        .store
        .select_local_whisper(
            fixture.recording_id,
            "large-v3-turbo-q5_0".to_owned(),
            "b".repeat(64),
            1_500_000_000,
            60_000,
            Some("zh".to_owned()),
        )
        .unwrap();
    assert_eq!(selected.state, ProcessingState::LocalTranscribing);
    assert_eq!(
        selected.transcription_backend,
        TranscriptionBackend::WhisperLocal
    );
    assert!(selected.miaoji.is_none());
    assert!(selected.tos_object.is_none());

    let request = match fixture.store.resume(fixture.recording_id).unwrap() {
        ResumeAction::TranscribeLocal { request } => request,
        action => panic!("unexpected local resume action: {action:?}"),
    };
    let whisper = request.whisper.as_ref().unwrap();
    assert_eq!(whisper.recording_id, fixture.recording_id);
    assert_eq!(whisper.audio_sha256, SHA);
    assert_eq!(whisper.audio_relative_path, "derived/mixed.wav");
    assert_eq!(whisper.model_id, "large-v3-turbo-q5_0");
    assert!(request.qwen.is_none());
    assert!(request.diarization.is_none());
    assert!(request.transcript_only_accepted);

    let response = LocalWhisperResponse {
        schema_version: whisper.schema_version,
        recording_id: whisper.recording_id,
        model_id: whisper.model_id.clone(),
        model_sha256: whisper.model_sha256.clone(),
        audio_sha256: whisper.audio_sha256.clone(),
        language: "zh".to_owned(),
        segments: vec![LocalWhisperSegment {
            start_ms: 0,
            end_ms: 1_000,
            text: "fabricated local transcript".to_owned(),
            speaker_id: None,
        }],
    };
    let checkpointed = fixture
        .store
        .checkpoint_local_whisper_transcript(fixture.recording_id, response.clone())
        .unwrap();
    assert_eq!(checkpointed.state, ProcessingState::Summarizing);
    assert_eq!(
        checkpointed.transcript_json.as_ref().unwrap()[0]["stt_backend"],
        "whisper_local"
    );
    let duplicate = fixture
        .store
        .checkpoint_local_whisper_transcript(fixture.recording_id, response)
        .unwrap();
    assert_eq!(duplicate.revision, checkpointed.revision);
}

#[test]
fn local_whisper_selection_cannot_bypass_ambiguous_or_accepted_remote_submit() {
    let ambiguous = Fixture::new();
    ambiguous.uploaded();
    ambiguous
        .store
        .begin_miaoji_submit(ambiguous.recording_id)
        .unwrap();
    ambiguous
        .store
        .mark_submit_ambiguous(ambiguous.recording_id)
        .unwrap();
    assert_eq!(
        ambiguous
            .store
            .select_local_whisper(
                ambiguous.recording_id,
                "large-v3-turbo-q5_0".to_owned(),
                "b".repeat(64),
                1_500_000_000,
                60_000,
                None,
            )
            .unwrap_err()
            .code,
        "manual_resolution_required"
    );

    let accepted = Fixture::new();
    accepted.polling();
    assert_eq!(
        accepted
            .store
            .select_local_whisper(
                accepted.recording_id,
                "large-v3-turbo-q5_0".to_owned(),
                "b".repeat(64),
                1_500_000_000,
                60_000,
                None,
            )
            .unwrap_err()
            .code,
        "manual_resolution_required"
    );
}

#[test]
fn accepted_remote_task_requires_explicit_durable_full_local_takeover() {
    use super::local_whisper::LocalModelFileIdentity;

    let fixture = Fixture::new();
    fixture.polling();
    let diarization_files = vec![LocalModelFileIdentity {
        relative_path: "speaker-diarization/Segmentation.mlmodelc/model.mil".to_owned(),
        sha256: "c".repeat(64),
        size_bytes: 43_063,
    }];
    assert_eq!(
        fixture
            .store
            .select_full_local(
                fixture.recording_id,
                "large-v3-turbo-q5_0".to_owned(),
                "b".repeat(64),
                574_041_195,
                60_000,
                None,
                "fluid-v1".to_owned(),
                diarization_files.clone(),
                Some(2),
                "qwen3.8-27b-ud-q4-k-xl".to_owned(),
                "d".repeat(64),
                17_559_178_144,
            )
            .unwrap_err()
            .code,
        "manual_resolution_required"
    );

    let taken_over = fixture
        .store
        .take_over_with_full_local(
            fixture.recording_id,
            "large-v3-turbo-q5_0".to_owned(),
            "b".repeat(64),
            574_041_195,
            60_000,
            None,
            "fluid-v1".to_owned(),
            diarization_files.clone(),
            Some(2),
            "qwen3.8-27b-ud-q4-k-xl".to_owned(),
            "d".repeat(64),
            17_559_178_144,
        )
        .unwrap();
    assert_eq!(taken_over.state, ProcessingState::LocalTranscribing);
    assert_eq!(
        taken_over.transcription_backend,
        TranscriptionBackend::WhisperLocal
    );
    let remote = taken_over.miaoji.as_ref().unwrap();
    assert_eq!(remote.task_id.as_deref(), Some("task-1"));
    assert!(remote.superseded);
    assert_eq!(
        fixture.store.load(fixture.recording_id).unwrap(),
        taken_over
    );
    let duplicate = fixture
        .store
        .take_over_with_full_local(
            fixture.recording_id,
            "large-v3-turbo-q5_0".to_owned(),
            "b".repeat(64),
            574_041_195,
            60_000,
            None,
            "fluid-v1".to_owned(),
            diarization_files,
            Some(2),
            "qwen3.8-27b-ud-q4-k-xl".to_owned(),
            "d".repeat(64),
            17_559_178_144,
        )
        .unwrap();
    assert_eq!(duplicate.revision, taken_over.revision);
    assert_eq!(
        fixture
            .store
            .checkpoint_transcript(
                fixture.recording_id,
                json!({"utterances": [{"content": "late remote result"}]})
            )
            .unwrap_err()
            .code,
        "invalid_state"
    );
    assert!(matches!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::TranscribeLocal { .. }
    ));

    let remote_wins = Fixture::new();
    remote_wins.polling();
    remote_wins
        .store
        .checkpoint_transcript(
            remote_wins.recording_id,
            json!({"utterances": [{"content": "fabricated remote result"}]}),
        )
        .unwrap();
    assert_eq!(
        remote_wins
            .store
            .take_over_with_full_local(
                remote_wins.recording_id,
                "large-v3-turbo-q5_0".to_owned(),
                "b".repeat(64),
                574_041_195,
                60_000,
                None,
                "fluid-v1".to_owned(),
                vec![LocalModelFileIdentity {
                    relative_path: "speaker-diarization/Segmentation.mlmodelc/model.mil".to_owned(),
                    sha256: "c".repeat(64),
                    size_bytes: 43_063,
                }],
                Some(2),
                "qwen3.8-27b-ud-q4-k-xl".to_owned(),
                "d".repeat(64),
                17_559_178_144,
            )
            .unwrap_err()
            .code,
        "invalid_state"
    );
    let remote_ledger = remote_wins.store.load(remote_wins.recording_id).unwrap();
    assert_eq!(
        remote_ledger.transcription_backend,
        TranscriptionBackend::MiaojiRemote
    );
    assert!(!remote_ledger.miaoji.unwrap().superseded);
}

#[test]
fn legacy_miaoji_checkpoint_defaults_to_not_superseded() {
    let fixture = Fixture::new();
    fixture.polling();
    let path = fixture.store.job_path(fixture.recording_id);
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    persisted["miaoji"]
        .as_object_mut()
        .unwrap()
        .remove("superseded");
    fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();
    let loaded = fixture.store.load(fixture.recording_id).unwrap();
    assert!(!loaded.miaoji.unwrap().superseded);
}

#[test]
fn legacy_remote_ledger_without_backend_fields_defaults_to_miaoji() {
    let fixture = Fixture::new();
    fixture.enqueue();
    let path = fixture.store.job_path(fixture.recording_id);
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    persisted
        .as_object_mut()
        .unwrap()
        .remove("transcription_backend");
    persisted.as_object_mut().unwrap().remove("local_whisper");
    fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();

    let loaded = fixture.store.load(fixture.recording_id).unwrap();
    assert_eq!(
        loaded.transcription_backend,
        TranscriptionBackend::MiaojiRemote
    );
    assert!(loaded.local_whisper.is_none());
    assert_eq!(loaded.state, ProcessingState::Queued);
}

#[test]
fn local_whisper_failure_retries_the_same_durable_model_request() {
    let fixture = Fixture::new();
    fixture.enqueue();
    fixture
        .store
        .select_local_whisper(
            fixture.recording_id,
            "large-v3-turbo-q5_0".to_owned(),
            "b".repeat(64),
            1_500_000_000,
            60_000,
            None,
        )
        .unwrap();
    fixture
        .store
        .mark_local_whisper_failed(fixture.recording_id)
        .unwrap();
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::ManualLocalResolution
    );
    fixture.store.retry_provider(fixture.recording_id).unwrap();
    let request = match fixture.store.resume(fixture.recording_id).unwrap() {
        ResumeAction::TranscribeLocal { request } => request,
        action => panic!("unexpected local retry action: {action:?}"),
    };
    let whisper = request.whisper.as_ref().unwrap();
    assert_eq!(whisper.model_id, "large-v3-turbo-q5_0");
    assert_eq!(whisper.model_sha256, "b".repeat(64));
}

#[test]
fn full_local_selection_is_default_diarized_and_rejects_incomplete_labels() {
    use super::local_whisper::{LocalModelFileIdentity, LocalWhisperSegment};

    let fixture = Fixture::new();
    fixture.enqueue();
    let selected = fixture
        .store
        .select_full_local(
            fixture.recording_id,
            "large-v3-turbo-q5_0".to_owned(),
            "b".repeat(64),
            574_041_195,
            60_000,
            Some("en".to_owned()),
            "fluid-v1".to_owned(),
            vec![LocalModelFileIdentity {
                relative_path: "speaker-diarization/Segmentation.mlmodelc/model.mil".to_owned(),
                sha256: "c".repeat(64),
                size_bytes: 43_063,
            }],
            Some(2),
            "qwen3.8-27b-ud-q4-k-xl".to_owned(),
            "d".repeat(64),
            17_559_178_144,
        )
        .unwrap();
    assert!(!selected.transcript_only_accepted);
    let request = match fixture.store.resume(fixture.recording_id).unwrap() {
        ResumeAction::TranscribeLocal { request } => request,
        action => panic!("unexpected full-local resume action: {action:?}"),
    };
    let whisper = request.whisper.as_ref().unwrap();
    assert_eq!(
        request
            .diarization
            .as_ref()
            .and_then(|request| request.expected_speaker_count),
        Some(2)
    );

    let unlabeled = LocalWhisperResponse {
        schema_version: whisper.schema_version,
        recording_id: whisper.recording_id,
        model_id: whisper.model_id.clone(),
        model_sha256: whisper.model_sha256.clone(),
        audio_sha256: whisper.audio_sha256.clone(),
        language: "en".to_owned(),
        segments: vec![LocalWhisperSegment {
            start_ms: 0,
            end_ms: 1_000,
            text: "fabricated full-local transcript".to_owned(),
            speaker_id: None,
        }],
    };
    assert_eq!(
        fixture
            .store
            .checkpoint_local_whisper_transcript(fixture.recording_id, unlabeled.clone())
            .unwrap_err()
            .code,
        "diarization_incomplete"
    );
    let mut labeled = unlabeled;
    labeled.segments[0].speaker_id = Some("local_unknown".to_owned());
    assert_eq!(
        fixture
            .store
            .checkpoint_local_whisper_transcript(fixture.recording_id, labeled.clone())
            .unwrap_err()
            .code,
        "diarization_incomplete"
    );
    labeled.segments[0].speaker_id = Some("local_speaker_01".to_owned());
    let checkpointed = fixture
        .store
        .checkpoint_local_whisper_transcript(fixture.recording_id, labeled)
        .unwrap();
    assert_eq!(checkpointed.state, ProcessingState::Summarizing);
    assert_eq!(
        checkpointed.transcript_json.unwrap()[0]["speaker"]["id"],
        "local_speaker_01"
    );
}

#[test]
fn qwen_candidate_selection_reopens_with_exact_model_set_and_takeover_fence() {
    let fixture = Fixture::new();
    fixture.enqueue();
    let (qwen, diarization, summary) = qwen_local_checkpoints(60_000);
    let selected = fixture
        .store
        .select_full_local_qwen(
            fixture.recording_id,
            qwen.clone(),
            diarization.clone(),
            summary.clone(),
        )
        .unwrap();
    assert_eq!(
        selected.transcription_backend,
        TranscriptionBackend::QwenLocal
    );
    assert_eq!(selected.local_qwen.as_ref(), Some(&qwen));
    assert!(selected.local_whisper.is_none());

    let reopened = ProcessingStore::open(
        &fixture._temp.path().join("app"),
        &fixture._temp.path().join("inbox"),
        &fixture._temp.path().join("archive"),
    )
    .unwrap();
    assert_eq!(reopened.load(fixture.recording_id).unwrap(), selected);
    let request = match reopened.resume(fixture.recording_id).unwrap() {
        ResumeAction::TranscribeLocal { request } => request,
        action => panic!("unexpected Qwen restart action: {action:?}"),
    };
    assert!(request.whisper.is_none());
    assert_eq!(request.qwen.as_ref().unwrap().asr_model_files.len(), 3);
    assert_eq!(request.qwen.as_ref().unwrap().aligner_model_files.len(), 2);
    assert_eq!(
        reopened
            .load(fixture.recording_id)
            .unwrap()
            .canonical_local_transcript_request()
            .unwrap()
            .model_sha256,
        selected
            .canonical_local_transcript_request()
            .unwrap()
            .model_sha256
    );

    let takeover = Fixture::new();
    takeover.polling();
    let (qwen, diarization, summary) = qwen_local_checkpoints(60_000);
    let selected = takeover
        .store
        .take_over_with_full_local_qwen(
            takeover.recording_id,
            qwen.clone(),
            diarization.clone(),
            summary.clone(),
        )
        .unwrap();
    assert_eq!(
        selected.transcription_backend,
        TranscriptionBackend::QwenLocal
    );
    assert!(selected.miaoji.as_ref().unwrap().superseded);
    assert_eq!(
        takeover
            .store
            .take_over_with_full_local_qwen(takeover.recording_id, qwen, diarization, summary,)
            .unwrap(),
        selected
    );
    assert!(takeover
        .store
        .checkpoint_transcript(takeover.recording_id, json!({"late": "remote"}))
        .is_err());
}

#[test]
fn transcript_only_requires_explicit_acceptance_after_full_local_failure() {
    use super::local_whisper::LocalModelFileIdentity;

    let fixture = Fixture::new();
    fixture.enqueue();
    fixture
        .store
        .select_full_local(
            fixture.recording_id,
            "large-v3-turbo-q5_0".to_owned(),
            "b".repeat(64),
            574_041_195,
            60_000,
            None,
            "fluid-v1".to_owned(),
            vec![LocalModelFileIdentity {
                relative_path: "speaker-diarization/Segmentation.mlmodelc/model.mil".to_owned(),
                sha256: "c".repeat(64),
                size_bytes: 43_063,
            }],
            None,
            "qwen3.8-27b-ud-q4-k-xl".to_owned(),
            "d".repeat(64),
            17_559_178_144,
        )
        .unwrap();
    fixture
        .store
        .mark_local_whisper_failed(fixture.recording_id)
        .unwrap();

    let accepted = fixture
        .store
        .accept_local_transcript_only(fixture.recording_id)
        .unwrap();
    assert_eq!(accepted.state, ProcessingState::LocalTranscribing);
    assert!(accepted.local_diarization.is_none());
    assert!(accepted.transcript_only_accepted);
    let request = match fixture.store.resume(fixture.recording_id).unwrap() {
        ResumeAction::TranscribeLocal { request } => request,
        action => panic!("unexpected transcript-only action: {action:?}"),
    };
    assert!(request.diarization.is_none());
    assert!(request.transcript_only_accepted);
    assert_eq!(fixture.store.load(fixture.recording_id).unwrap(), accepted);
    assert_eq!(
        fixture
            .store
            .accept_local_transcript_only(fixture.recording_id)
            .unwrap_err()
            .code,
        "invalid_state"
    );
}

#[test]
fn accepted_task_and_provider_payload_checkpoints_are_idempotent_but_immutable() {
    let fixture = Fixture::new();
    fixture.uploaded();
    fixture
        .store
        .begin_miaoji_submit(fixture.recording_id)
        .unwrap();
    let accepted = fixture
        .store
        .checkpoint_miaoji_task(fixture.recording_id, "task-1".to_owned())
        .unwrap();
    assert_eq!(accepted.state, ProcessingState::Polling);
    let duplicate_acceptance = fixture
        .store
        .checkpoint_miaoji_task(fixture.recording_id, "task-1".to_owned())
        .unwrap();
    assert_eq!(duplicate_acceptance.revision, accepted.revision);
    // Once accepted, resume polls and never returns a submit action.
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::PollMiaoji {
            task_id: "task-1".to_owned()
        }
    );
    fixture
        .store
        .checkpoint_transcript(fixture.recording_id, json!({"text": "hello"}))
        .unwrap();
    let duplicate = fixture
        .store
        .checkpoint_transcript(fixture.recording_id, json!({"text": "hello"}))
        .unwrap();
    assert_eq!(duplicate.state, ProcessingState::Summarizing);
    let conflict = fixture
        .store
        .checkpoint_transcript(fixture.recording_id, json!({"text": "changed"}))
        .unwrap_err();
    assert_eq!(conflict.code, "checkpoint_conflict");
}

#[test]
fn crash_after_summary_dispatch_fence_never_auto_charges_again() {
    let fixture = Fixture::new();
    fixture.polling();
    fixture
        .store
        .checkpoint_transcript(fixture.recording_id, json!({"text": "hello"}))
        .unwrap();
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::Summarize
    );
    assert_eq!(
        fixture.store.load(fixture.recording_id).unwrap().state,
        ProcessingState::SummarySubmitting
    );
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::ManualSummaryResolution
    );
    assert_eq!(
        fixture.store.load(fixture.recording_id).unwrap().state,
        ProcessingState::SummaryAmbiguous
    );
    fixture
        .store
        .resolve_summary_for_retry(fixture.recording_id)
        .unwrap();
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::Summarize
    );
}

#[test]
fn higher_revision_atomic_temp_wins_after_a_pre_rename_crash() {
    let fixture = Fixture::new();
    let queued = fixture.enqueue();
    let destination = fixture.store.job_path(fixture.recording_id);
    let queued_bytes = fs::read(&destination).unwrap();
    let uploading = fixture.store.begin_upload(fixture.recording_id).unwrap();
    let uploading_bytes = fs::read(&destination).unwrap();

    fs::write(&destination, queued_bytes).unwrap();
    let interrupted = fixture.store.jobs.join(format!(
        ".{}.{}.crash.tmp",
        fixture.recording_id, uploading.revision
    ));
    fs::write(&interrupted, uploading_bytes).unwrap();

    let recovered = fixture.store.load(fixture.recording_id).unwrap();
    assert_eq!(recovered.state, ProcessingState::Uploading);
    assert_eq!(recovered.revision, queued.revision + 1);
    assert!(!interrupted.exists());
    let persisted: ProcessingLedger =
        serde_json::from_slice(&fs::read(destination).unwrap()).unwrap();
    assert_eq!(persisted.revision, recovered.revision);
}

#[test]
fn publication_is_ordered_skips_verified_targets_and_cleanup_requires_real_proofs() {
    let fixture = Fixture::new();
    fixture.publishing(&[RetryMode::Idempotent, RetryMode::ReconcileBeforeRetry]);
    let generation = 1;
    let out_of_order = fixture
        .store
        .start_publication_target(fixture.recording_id, generation, "target-1")
        .unwrap_err();
    assert_eq!(out_of_order.code, "target_out_of_order");
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::PublishTarget {
            generation,
            target_id: "target-0".to_owned()
        }
    );
    fixture
        .store
        .start_publication_target(fixture.recording_id, generation, "target-0")
        .unwrap();
    fixture
        .store
        .checkpoint_publication_target(
            fixture.recording_id,
            generation,
            "target-0",
            fixture.proof("archive://one"),
        )
        .unwrap();
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::PublishTarget {
            generation,
            target_id: "target-1".to_owned()
        }
    );
    assert_eq!(
        fixture
            .store
            .checkpoint_cleanup(fixture.recording_id, generation)
            .unwrap_err()
            .code,
        "cleanup_not_safe"
    );
    fixture
        .store
        .start_publication_target(fixture.recording_id, generation, "target-1")
        .unwrap();
    fixture
        .store
        .checkpoint_publication_target(
            fixture.recording_id,
            generation,
            "target-1",
            fixture.proof("backup://one"),
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .checkpoint_cleanup(fixture.recording_id, generation)
            .unwrap_err()
            .code,
        "cleanup_not_safe"
    );
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::VerifyCanonicalBackup { generation }
    );
    let mut mismatch = fixture.backup();
    mismatch.size_bytes += 1;
    assert_eq!(
        fixture
            .store
            .checkpoint_canonical_backup(fixture.recording_id, generation, mismatch)
            .unwrap_err()
            .code,
        "backup_mismatch"
    );
    fixture
        .store
        .checkpoint_canonical_backup(fixture.recording_id, generation, fixture.backup())
        .unwrap();
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::Cleanup { generation }
    );
    let complete = fixture
        .store
        .checkpoint_cleanup(fixture.recording_id, generation)
        .unwrap();
    assert_eq!(complete.state, ProcessingState::Complete);
    assert!(complete.cleanup.temporary_tos_deleted);
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::Done
    );
}

#[test]
fn crash_after_at_most_once_target_start_never_resends() {
    let fixture = Fixture::new();
    fixture.publishing(&[RetryMode::AtMostOnce]);
    fixture
        .store
        .start_publication_target(fixture.recording_id, 1, "target-0")
        .unwrap();

    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::ManualPublicationResolution
    );
    let persisted = fixture.store.load(fixture.recording_id).unwrap();
    assert_eq!(persisted.state, ProcessingState::PublishAmbiguous);
    assert_eq!(
        persisted.publication.unwrap().targets[0].state,
        PublicationTargetState::Ambiguous
    );
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::ManualPublicationResolution
    );
}

#[test]
fn cancel_wins_against_late_effect_checkpoint_and_is_durable() {
    let fixture = Fixture::new();
    fixture.uploaded();
    fixture
        .store
        .begin_miaoji_submit(fixture.recording_id)
        .unwrap();
    let canceled = fixture.store.cancel(fixture.recording_id).unwrap();
    assert_eq!(canceled.state, ProcessingState::CanceledAfterUpload);
    let late = fixture
        .store
        .checkpoint_miaoji_task(fixture.recording_id, "accepted-too-late".to_owned())
        .unwrap_err();
    assert_eq!(late.code, "invalid_state");
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::Canceled
    );
}

#[test]
fn cancel_during_upload_reconciles_a_late_receipt_before_cleanup() {
    let fixture = Fixture::new();
    fixture.enqueue();
    fixture.store.begin_upload(fixture.recording_id).unwrap();
    let canceling = fixture.store.cancel(fixture.recording_id).unwrap();
    assert_eq!(canceling.state, ProcessingState::CancelingUpload);
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::ReconcileCanceledUpload
    );

    let canceled = fixture
        .store
        .checkpoint_tos_object(
            fixture.recording_id,
            "private-audio".to_owned(),
            "version-1".to_owned(),
            "etag-1".to_owned(),
            SHA.to_owned(),
            44_100,
        )
        .unwrap();
    assert_eq!(canceled.state, ProcessingState::CanceledAfterUpload);
    assert!(canceled.tos_object.is_some());
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::Canceled
    );
}

#[test]
fn cancel_during_upload_can_checkpoint_verified_remote_absence() {
    let fixture = Fixture::new();
    fixture.enqueue();
    fixture.store.begin_upload(fixture.recording_id).unwrap();
    fixture.store.cancel(fixture.recording_id).unwrap();

    let canceled = fixture
        .store
        .checkpoint_canceled_upload_absent(fixture.recording_id)
        .unwrap();
    assert_eq!(canceled.state, ProcessingState::CanceledBeforeUpload);
    assert!(canceled.tos_object.is_none());
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::Canceled
    );
}

#[test]
fn two_store_handles_serialize_writes_without_corrupting_the_ledger() {
    let fixture = Fixture::new();
    fixture.enqueue();
    let second = ProcessingStore::open(
        fixture._temp.path().join("app").as_path(),
        fixture._temp.path().join("inbox").as_path(),
        fixture._temp.path().join("archive").as_path(),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&fixture.store.writer, &second.writer));
    let barrier = Arc::new(Barrier::new(3));
    let first_store = fixture.store.clone();
    let first_barrier = Arc::clone(&barrier);
    let recording_id = fixture.recording_id;
    let first = thread::spawn(move || {
        first_barrier.wait();
        first_store.begin_upload(recording_id)
    });
    let second_barrier = Arc::clone(&barrier);
    let second_thread = thread::spawn(move || {
        second_barrier.wait();
        second.cancel(recording_id)
    });
    barrier.wait();
    let begin_result = first.join().unwrap();
    let cancel_result = second_thread.join().unwrap();
    let state = fixture.store.load(recording_id).unwrap().state;
    assert!(cancel_result.is_ok());
    assert!(
        matches!(
            state,
            ProcessingState::CancelingUpload
                | ProcessingState::CanceledBeforeUpload
                | ProcessingState::CanceledAfterUpload
        ),
        "concurrent begin/cancel ended in {state:?}; begin={:?}; cancel={:?}",
        begin_result
            .as_ref()
            .map(|ledger| ledger.state)
            .map_err(|error| error.code),
        cancel_result
            .as_ref()
            .map(|ledger| ledger.state)
            .map_err(|error| error.code)
    );
}

#[test]
fn process_file_lock_is_released_during_unwind() {
    let fixture = Fixture::new();
    fixture.enqueue();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), ProcessingError> = fixture.store.with_writer(|| {
            panic!("fabricated crash while holding both processing locks");
        });
    }));
    assert!(result.is_err());

    let second = ProcessingStore::open(
        fixture._temp.path().join("app").as_path(),
        fixture._temp.path().join("inbox").as_path(),
        fixture._temp.path().join("archive").as_path(),
    )
    .unwrap();
    assert_eq!(
        second.load(fixture.recording_id).unwrap().revision,
        fixture.store.load(fixture.recording_id).unwrap().revision
    );
}

#[test]
fn publication_conflict_is_persisted_and_errors_are_bounded_and_redacted() {
    let fixture = Fixture::new();
    fixture.publishing(&[RetryMode::Idempotent]);
    let conflict = fixture
        .store
        .mark_publication_conflict(fixture.recording_id)
        .unwrap();
    assert_eq!(conflict.state, ProcessingState::PublishConflict);
    assert_eq!(
        fixture.store.resume(fixture.recording_id).unwrap(),
        ResumeAction::ManualPublicationResolution
    );

    let secret = "Bearer secret-value";
    let error = fixture
        .store
        .checkpoint_miaoji_task(fixture.recording_id, secret.to_owned())
        .unwrap_err();
    assert!(!error.to_string().contains(secret));
    assert!(!format!("{error:?}").contains(secret));
}

#[test]
fn rejects_storage_overlap_and_unbounded_json() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    fs::create_dir_all(app.join("processing")).unwrap();
    assert_eq!(
        ProcessingStore::open(&app, &app.join("processing/inbox"), temp.path())
            .unwrap_err()
            .code,
        "unsafe_storage_layout"
    );

    let fixture = Fixture::new();
    fixture.polling();
    let too_many = Value::Array((0..=MAX_JSON_ARRAY_ITEMS).map(|_| Value::Null).collect());
    assert_eq!(
        fixture
            .store
            .checkpoint_transcript(fixture.recording_id, too_many)
            .unwrap_err()
            .code,
        "checkpoint_too_large"
    );
}
