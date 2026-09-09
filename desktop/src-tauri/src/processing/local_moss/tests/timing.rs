use super::*;

fn chronological_spec(frame_lengths: &[u64]) -> LocalMossPlanSpec {
    let mut spec = plan_spec(frame_lengths);
    spec.mapping_policy = Some(COMPOSED_MAPPING_POLICY.into());
    for window in &mut spec.windows {
        window.request.timing_policy = moss::CHRONOLOGICAL_TIMING_POLICY_V3.into();
    }
    spec
}

#[test]
fn chronological_plan_is_explicit_schema3_uniform_and_changes_request_identity() {
    let lengths = [moss::windows::MAX_WINDOW_FRAMES, 16_000];
    let old = LocalMossPlan::new(plan_spec(&lengths)).unwrap();
    let spec = chronological_spec(&lengths);
    let plan = LocalMossPlan::new(spec.clone()).unwrap();
    assert_eq!(
        plan.adaptation_policy(),
        moss::CHRONOLOGICAL_TIMING_POLICY_V3
    );
    assert_eq!(old.adaptation_policy(), moss::COALESCING_TIMING_POLICY_V2);
    assert_ne!(plan.plan_sha256(), old.plan_sha256());
    assert_ne!(
        plan.windows()[0].request_sha256(),
        old.windows()[0].request_sha256()
    );
    let mut mixed = spec.clone();
    mixed.windows[1].request.timing_policy = moss::COALESCING_TIMING_POLICY_V2.into();
    assert!(LocalMossPlan::new(mixed).is_err());
    for schema in [1, 2] {
        let mut legacy = spec.clone();
        legacy.schema_version = schema;
        legacy.mapping_policy = None;
        if schema == 1 {
            legacy.diarization_request.audio_relative_path = legacy.source.relative_path.clone();
        }
        assert!(LocalMossPlan::from_json(&serde_json::to_vec(&legacy).unwrap()).is_err());
    }
}

#[test]
fn chronological_response_proves_permutation_and_union_through_summary_and_archive() {
    let plan = LocalMossPlan::new(chronological_spec(&[16_000])).unwrap();
    let raw = vec![
        segment(500, 900, 1, "later"),
        segment(0, 300, 2, "early"),
        segment(250, 450, 2, "second"),
        segment(900, 1000, 0, "unknown"),
    ];
    let raw_response = response(&plan, 0, raw.clone());
    let bytes = serde_json::to_vec(&raw_response).unwrap();
    let validated = ValidatedMossWindowResponse::decode(
        &plan,
        0,
        binding(&plan, plan.windows()[0].request_sha256(), &bytes),
        &bytes,
    )
    .unwrap();
    assert_eq!(validated.receipt().binding.response_sha256, digest(&bytes));
    let chronology = validated.chronology().unwrap();
    assert_eq!(chronology.provenance.source_index_permutation, [1, 2, 0, 3]);
    assert_eq!(
        chronology.provenance.output_source_indices,
        [vec![1, 2], vec![0], vec![3]]
    );
    assert_eq!(
        chronology.source_emission_joined_text_sha256,
        digest(b"later early second unknown")
    );
    assert_eq!(
        chronology.chronological_joined_text_sha256,
        digest(b"early second later unknown")
    );
    let responses = CompleteMossResponses::new(&plan, vec![validated]).unwrap();
    let result = finalize(&plan, &responses, &anchors(&plan, &[(0, 1000, 1)])).unwrap();
    assert_eq!(
        result.evidence().adaptation_policy,
        moss::CHRONOLOGICAL_TIMING_POLICY_V3
    );
    assert_eq!(result.evidence().chronological_windows.len(), 1);
    assert_eq!(result.evidence().source_segments, 4);
    assert_eq!(result.evidence().output_segments, 3);
    assert_eq!(result.evidence().coalesced_source_segments, 1);
    assert_eq!(result.transcript_json()[0]["content"], "early second");
    assert_eq!(result.transcript_json()[0]["start_time"], 0);
    assert_eq!(result.transcript_json()[0]["end_time"], 450);
    assert_eq!(result.transcript_json()[1]["start_time"], 500);
    assert_eq!(result.transcript_json()[1]["end_time"], 900);
    assert_eq!(
        result.transcript_json()[2]["speaker"]["id"],
        "local_unknown"
    );
    assert_eq!(transcript_for_summary(result.transcript_json()).unwrap(), "SPEAKER_moss_speaker_2: early second\nSPEAKER_moss_speaker_1: later\nSPEAKER_local_unknown: unknown\n");
    let archive =
        crate::processing::archive::moss_transcript_text_for_test(result.transcript_json())
            .unwrap();
    assert!(archive.find("early second").unwrap() < archive.find("later").unwrap());
    assert!(archive.find("later").unwrap() < archive.find("unknown: unknown").unwrap());
    assert_eq!(
        moss::decode_response(&bytes, plan.windows()[0].request())
            .unwrap()
            .segments,
        raw
    );
    assert_eq!(serde_json::to_vec(&raw_response).unwrap(), bytes);
}

#[test]
fn retained_v2_bundle_rejects_regression_and_omits_chronology_evidence() {
    let plan = LocalMossPlan::new(plan_spec(&[16_000])).unwrap();
    let bytes = serde_json::to_vec(&response(
        &plan,
        0,
        vec![segment(500, 900, 1, "later"), segment(0, 400, 2, "earlier")],
    ))
    .unwrap();
    assert_eq!(
        ValidatedMossWindowResponse::decode(
            &plan,
            0,
            binding(&plan, plan.windows()[0].request_sha256(), &bytes),
            &bytes
        )
        .unwrap_err()
        .code,
        "local_moss_response_invalid"
    );
    let validated = window_response(&plan, 0, vec![segment(0, 1000, 1, "retained")]);
    assert!(validated.chronology().is_none());
    let responses = CompleteMossResponses::new(&plan, vec![validated]).unwrap();
    let result = finalize(&plan, &responses, &anchors(&plan, &[(0, 1000, 1)])).unwrap();
    assert_eq!(
        result.evidence().adaptation_policy,
        moss::COALESCING_TIMING_POLICY_V2
    );
    assert!(result.evidence().chronological_windows.is_empty());
    assert!(serde_json::to_value(result.evidence())
        .unwrap()
        .get("chronological_windows")
        .is_none());
}
