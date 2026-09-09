use super::*;
use crate::processing::local_whisper::{
    LOCAL_DIARIZATION_SPEAKERKIT_PRESET, LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET,
};

fn selection_with_policy(
    fixture: &Fixture,
    schema: u32,
    policy: Option<&str>,
    diarization_preset: &str,
    timing_policy: &str,
) -> MossPreparationCheckpoint {
    let mut value = serde_json::to_value(&fixture.selection).unwrap();
    value["schema_version"] = serde_json::json!(schema);
    value["model_pins"]["speakerkit_quality_preset"] = serde_json::json!(diarization_preset);
    value["timing_policy"] = serde_json::json!(timing_policy);
    match policy {
        Some(policy) => value["mapping_policy"] = serde_json::json!(policy),
        None => {
            value.as_object_mut().unwrap().remove("mapping_policy");
        }
    }
    let selection = serde_json::from_value::<MossPreparationCheckpoint>(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&selection).unwrap(), value);
    selection
}

#[test]
fn selection_pins_policy_before_decode_and_restart_keeps_old_and_new_policies() {
    for (schema, policy, plan_schema, mapping_policy, diarization_preset, timing_policy) in [
        (
            1,
            None,
            2,
            moss::speakers::POLICY,
            LOCAL_DIARIZATION_SPEAKERKIT_PRESET,
            moss::COALESCING_TIMING_POLICY_V2,
        ),
        (
            2,
            Some(COMPOSED_MAPPING_POLICY),
            3,
            COMPOSED_MAPPING_POLICY,
            LOCAL_DIARIZATION_SPEAKERKIT_PRESET,
            moss::COALESCING_TIMING_POLICY_V2,
        ),
        (
            2,
            Some(COMPOSED_MAPPING_POLICY),
            3,
            COMPOSED_MAPPING_POLICY,
            LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET,
            moss::COALESCING_TIMING_POLICY_V2,
        ),
        (
            2,
            Some(moss::speakers::POLICY),
            3,
            moss::speakers::POLICY,
            LOCAL_DIARIZATION_SPEAKERKIT_PRESET,
            moss::COALESCING_TIMING_POLICY_V2,
        ),
        (
            2,
            Some(moss::speakers::POLICY),
            3,
            moss::speakers::POLICY,
            LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET,
            moss::COALESCING_TIMING_POLICY_V2,
        ),
        (
            2,
            Some(COMPOSED_MAPPING_POLICY),
            3,
            COMPOSED_MAPPING_POLICY,
            LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET,
            moss::CHRONOLOGICAL_TIMING_POLICY_V3,
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.selection =
            selection_with_policy(&fixture, schema, policy, diarization_preset, timing_policy);
        let owner = fixture.artifacts.try_owner().unwrap();
        let claim = fixture.begin(&owner);
        let selected = fixture.store.load(fixture.recording).unwrap();
        let checkpoint = selected.local_moss_preparation.as_ref().unwrap();
        assert_eq!(checkpoint.plan_schema_version(), plan_schema);
        assert_eq!(checkpoint.mapping_policy(), mapping_policy);
        assert_eq!(checkpoint.timing_policy(), timing_policy);
        assert!(!fixture
            .app
            .join(format!("inbox/{}/derived", fixture.recording))
            .exists());
        let window = fixture.publish();
        fixture
            .store
            .record_moss_prepared_window(fixture.recording, &owner, &claim, window.clone())
            .unwrap();
        // Crash after immutable plan publication but before the ready ledger
        // swap. An upgrade must reproduce these exact bytes, including v2's
        // absent policy field, when it resumes the old preparation claim.
        let published_plan = fixture.plan(&window);
        let published_ref = fixture
            .artifacts
            .write(&owner, ArtifactKind::Plan, published_plan.plan_bytes())
            .unwrap();
        drop(owner);
        let reopened =
            ProcessingStore::open(&fixture.app, &fixture.app.join("inbox"), &fixture.archive)
                .unwrap();
        assert_eq!(
            reopened.resume(fixture.recording).unwrap(),
            ResumeAction::PrepareMoss {
                generation: fixture.generation
            }
        );
        let retained = reopened
            .load(fixture.recording)
            .unwrap()
            .local_moss_preparation
            .unwrap();
        assert_eq!(retained.plan_schema_version(), plan_schema);
        assert_eq!(retained.mapping_policy(), mapping_policy);
        assert_eq!(retained.timing_policy(), timing_policy);
        let owner = fixture.artifacts.try_owner().unwrap();
        let claim = reopened
            .claim_moss_preparation(fixture.recording, &owner)
            .unwrap();
        let ready = prepare_moss_source(
            &fixture.app,
            &reopened,
            &owner,
            &claim,
            &AtomicBool::new(false),
        )
        .unwrap();
        let bytes = fixture
            .artifacts
            .read(ready.local_moss.as_ref().unwrap().plan_ref())
            .unwrap();
        let plan = LocalMossPlan::from_json(&bytes).unwrap();
        assert_eq!(
            ready.local_moss.as_ref().unwrap().plan_ref(),
            &published_ref
        );
        assert_eq!(plan.plan_bytes(), published_plan.plan_bytes());
        assert_eq!(plan.plan_sha256(), published_plan.plan_sha256());
        assert_eq!(plan.spec().schema_version, plan_schema);
        assert_eq!(plan.mapping_policy(), mapping_policy);
        assert_eq!(plan.adaptation_policy(), timing_policy);
        assert!(plan
            .windows()
            .iter()
            .all(|window| window.request().timing_policy == timing_policy));
        assert_eq!(
            plan.diarization_request().quality_preset,
            diarization_preset
        );
        assert_eq!(plan.spec().mapping_policy.as_deref(), policy);
        plan.require_executable().unwrap();
        assert_eq!(plan.spec().pcm_source_frames, FRAMES as u64);
        fixture.source_unchanged();
    }
}

#[test]
fn completion_requires_the_claimed_preparations_exact_schema_and_mapping_policy() {
    for (schema, policy) in [
        (1, None),
        (2, Some(COMPOSED_MAPPING_POLICY)),
        (2, Some(moss::speakers::POLICY)),
    ] {
        let mut fixture = Fixture::new();
        fixture.selection = selection_with_policy(
            &fixture,
            schema,
            policy,
            if schema == 1 {
                LOCAL_DIARIZATION_SPEAKERKIT_PRESET
            } else {
                LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET
            },
            if schema == 1 {
                moss::COALESCING_TIMING_POLICY_V2
            } else {
                moss::CHRONOLOGICAL_TIMING_POLICY_V3
            },
        );
        let owner = fixture.artifacts.try_owner().unwrap();
        let claim = fixture.begin(&owner);
        let window = fixture.publish();
        fixture
            .store
            .record_moss_prepared_window(fixture.recording, &owner, &claim, window.clone())
            .unwrap();
        let plan = fixture.plan(&window);
        let reference = fixture
            .artifacts
            .write(&owner, ArtifactKind::Plan, plan.plan_bytes())
            .unwrap();
        let before = fixture.store.load(fixture.recording).unwrap();
        for (wrong_schema, wrong_policy) in [
            (2, None),
            (3, Some(moss::speakers::POLICY)),
            (3, Some(COMPOSED_MAPPING_POLICY)),
        ] {
            if wrong_schema == plan.spec().schema_version
                && wrong_policy == plan.spec().mapping_policy.as_deref()
            {
                continue;
            }
            let mut spec = plan.spec().clone();
            spec.schema_version = wrong_schema;
            spec.mapping_policy = wrong_policy.map(str::to_owned);
            if wrong_schema == 2 {
                spec.diarization_request.quality_preset =
                    LOCAL_DIARIZATION_SPEAKERKIT_PRESET.into();
                for window in &mut spec.windows {
                    window.request.timing_policy = moss::COALESCING_TIMING_POLICY_V2.into();
                }
            }
            let wrong = LocalMossPlan::new_for_preparation(spec).unwrap();
            assert_eq!(
                fixture
                    .store
                    .complete_moss_preparation(
                        fixture.recording,
                        &owner,
                        &claim,
                        &wrong,
                        reference.clone()
                    )
                    .unwrap_err()
                    .code,
                "moss_preparation_conflict"
            );
            let retained = fixture.store.load(fixture.recording).unwrap();
            assert_eq!(retained.revision, before.revision);
            assert_eq!(
                retained.local_moss_preparation,
                before.local_moss_preparation
            );
            assert!(retained.local_moss.is_none());
        }
        let ready = fixture
            .store
            .complete_moss_preparation(fixture.recording, &owner, &claim, &plan, reference)
            .unwrap();
        assert_eq!(ready.state, ProcessingState::LocalTranscribing);
        fixture.source_unchanged();
    }
}

#[test]
fn completion_rejects_wrong_timing_without_consuming_the_claim_or_state() {
    for (selected_timing, wrong_timing) in [
        (
            moss::COALESCING_TIMING_POLICY_V2,
            moss::CHRONOLOGICAL_TIMING_POLICY_V3,
        ),
        (
            moss::CHRONOLOGICAL_TIMING_POLICY_V3,
            moss::COALESCING_TIMING_POLICY_V2,
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.selection = selection_with_policy(
            &fixture,
            2,
            Some(COMPOSED_MAPPING_POLICY),
            LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET,
            selected_timing,
        );
        let owner = fixture.artifacts.try_owner().unwrap();
        let claim = fixture.begin(&owner);
        let window = fixture.publish();
        fixture
            .store
            .record_moss_prepared_window(fixture.recording, &owner, &claim, window.clone())
            .unwrap();
        let plan = fixture.plan(&window);
        let reference = fixture
            .artifacts
            .write(&owner, ArtifactKind::Plan, plan.plan_bytes())
            .unwrap();
        let before = fixture.store.load(fixture.recording).unwrap();
        let mut wrong_spec = plan.spec().clone();
        for window in &mut wrong_spec.windows {
            window.request.timing_policy = wrong_timing.into();
        }
        let wrong = LocalMossPlan::new(wrong_spec).unwrap();
        assert_eq!(wrong.spec().schema_version, plan.spec().schema_version);
        assert_eq!(wrong.mapping_policy(), plan.mapping_policy());
        assert_eq!(wrong.diarization_request(), plan.diarization_request());
        assert_eq!(
            fixture
                .store
                .complete_moss_preparation(
                    fixture.recording,
                    &owner,
                    &claim,
                    &wrong,
                    reference.clone()
                )
                .unwrap_err()
                .code,
            "moss_preparation_conflict"
        );
        assert_eq!(fixture.store.load(fixture.recording).unwrap(), before);
        assert_eq!(
            fixture.artifacts.read(&reference).unwrap(),
            plan.plan_bytes()
        );
        let retained = fixture
            .store
            .check_moss_preparation_claim(fixture.recording, &owner, &claim)
            .unwrap();
        assert_eq!(retained.timing_policy(), selected_timing);
        let ready = fixture
            .store
            .complete_moss_preparation(fixture.recording, &owner, &claim, &plan, reference)
            .unwrap();
        assert_eq!(ready.state, ProcessingState::LocalTranscribing);
        fixture.source_unchanged();
    }
}

#[test]
fn preparation_revision_and_policy_tampering_are_rejected() {
    let fixture = Fixture::new();
    let original = serde_json::to_value(&fixture.selection).unwrap();
    assert_eq!(original["schema_version"], 2);
    assert_eq!(original["mapping_policy"], COMPOSED_MAPPING_POLICY);
    assert_eq!(
        original["timing_policy"],
        moss::CHRONOLOGICAL_TIMING_POLICY_V3
    );
    assert_eq!(
        original["model_pins"]["speakerkit_quality_preset"],
        LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET
    );
    for schema in [0, 1, 3] {
        let mut changed = original.clone();
        changed["schema_version"] = serde_json::json!(schema);
        assert!(serde_json::from_value::<MossPreparationCheckpoint>(changed).is_err());
    }
    for policy in [
        serde_json::Value::Null,
        serde_json::json!("future-policy"),
        serde_json::json!(2),
    ] {
        let mut changed = original.clone();
        changed["mapping_policy"] = policy;
        assert!(serde_json::from_value::<MossPreparationCheckpoint>(changed).is_err());
    }
    for timing in [
        serde_json::Value::Null,
        serde_json::json!("unknown-timing"),
        serde_json::json!(moss::TIMING_POLICY),
    ] {
        let mut changed = original.clone();
        changed["timing_policy"] = timing;
        assert!(serde_json::from_value::<MossPreparationCheckpoint>(changed).is_err());
    }
    let mut missing = original;
    missing.as_object_mut().unwrap().remove("mapping_policy");
    assert!(serde_json::from_value::<MossPreparationCheckpoint>(missing.clone()).is_err());
    missing["schema_version"] = serde_json::json!(1);
    assert!(serde_json::from_value::<MossPreparationCheckpoint>(missing.clone()).is_err());
    missing["model_pins"]["speakerkit_quality_preset"] =
        serde_json::json!(LOCAL_DIARIZATION_SPEAKERKIT_PRESET);
    // Even with the historical diarizer, preparation 1 cannot claim v3 timing.
    assert!(serde_json::from_value::<MossPreparationCheckpoint>(missing.clone()).is_err());
    missing["timing_policy"] = serde_json::json!(moss::COALESCING_TIMING_POLICY_V2);
    let legacy = serde_json::from_value::<MossPreparationCheckpoint>(missing.clone()).unwrap();
    assert_eq!(legacy.plan_schema_version(), 2);
    assert_eq!(legacy.mapping_policy(), moss::speakers::POLICY);
    assert_eq!(legacy.timing_policy(), moss::COALESCING_TIMING_POLICY_V2);
    assert_eq!(serde_json::to_value(legacy).unwrap(), missing);
    missing["mapping_policy"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<MossPreparationCheckpoint>(missing).is_err());
}
