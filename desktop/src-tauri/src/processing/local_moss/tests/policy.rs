use super::*;

#[test]
fn real_context_diarizer_is_explicit_in_current_plans_and_not_in_legacy_plans() {
    let mut spec = plan_spec(&[16000]);
    let original = LocalMossPlan::new(spec.clone()).unwrap();
    spec.diarization_request.quality_preset =
        LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET.into();
    let candidate = LocalMossPlan::new(spec.clone()).unwrap();
    assert_ne!(candidate.plan_sha256(), original.plan_sha256());
    assert_ne!(
        candidate.diarization_request_sha256(),
        original.diarization_request_sha256()
    );
    assert_eq!(
        candidate.windows()[0].request_bytes(),
        original.windows()[0].request_bytes()
    );
    spec.schema_version = 2;
    spec.mapping_policy = None;
    let bytes = serde_json::to_vec(&spec).unwrap();
    assert_eq!(
        LocalMossPlan::from_json(&bytes).unwrap_err().code,
        "local_moss_diarization_identity_mismatch"
    );
}

fn candidate_spec(frame_lengths: &[u64]) -> LocalMossPlanSpec {
    let mut spec = plan_spec(frame_lengths);
    spec.mapping_policy = Some(COMPOSED_MAPPING_POLICY.into());
    spec
}

fn finalize_segments(
    plan: &LocalMossPlan,
    segments: Vec<Vec<moss::MossSegment>>,
    intervals: &[(u64, u64, u32)],
) -> FinalizedMossTranscript {
    let responses = CompleteMossResponses::new(
        plan,
        segments
            .into_iter()
            .enumerate()
            .map(|(index, rows)| window_response(plan, index, rows))
            .collect(),
    )
    .unwrap();
    finalize(plan, &responses, &anchors(plan, intervals)).unwrap()
}

#[test]
fn legacy_v2_graph_output_evidence_bytes_and_hash_stay_exact() {
    let mut spec = plan_spec(&[3000 * FRAMES_PER_MS]);
    spec.schema_version = 2;
    spec.mapping_policy = None;
    let old_bytes = serde_json::to_vec_pretty(&spec).unwrap();
    let plan = LocalMossPlan::from_json(&old_bytes).unwrap();
    plan.require_executable().unwrap();
    assert_eq!(plan.plan_bytes(), old_bytes);
    assert_eq!(plan.plan_sha256(), digest(&old_bytes));
    assert_eq!(plan.mapping_policy(), speakers::POLICY);
    assert!(!String::from_utf8(old_bytes)
        .unwrap()
        .contains("mapping_policy"));
    assert_eq!(
        LocalMossPlan::new(spec).unwrap_err().code,
        "legacy_moss_plan_requires_reprepare"
    );
    let result = finalize_segments(
        &plan,
        vec![vec![
            segment(0, 1800, 9, "hello 世界"),
            segment(1000, 3000, 2, "yes"),
        ]],
        &[(0, 1500, 1), (1500, 3000, 2)],
    );
    let expected = json!([
        {"start_time":0,"end_time":1800,"speaker":{"id":"local_speaker_01"},"content":"hello 世界","stt_backend":BACKEND,"model_id":moss::MODEL_ID,"language":"auto"},
        {"start_time":1000,"end_time":3000,"speaker":{"id":"local_speaker_02"},"content":"yes","stt_backend":BACKEND,"model_id":moss::MODEL_ID,"language":"auto"}
    ]);
    assert_eq!(result.json_bytes(), serde_json::to_vec(&expected).unwrap());
    let graph = speakers::reconcile_with_cancel(
        &[
            speakers::TimingSegment {
                start_ms: 0,
                end_ms: 1800,
                speaker: Some("moss_speaker_9".into()),
            },
            speakers::TimingSegment {
                start_ms: 1000,
                end_ms: 3000,
                speaker: Some("moss_speaker_2".into()),
            },
        ],
        &[
            speakers::TimingSegment {
                start_ms: 0,
                end_ms: 1500,
                speaker: Some("local_speaker_01".into()),
            },
            speakers::TimingSegment {
                start_ms: 1500,
                end_ms: 3000,
                speaker: Some("local_speaker_02".into()),
            },
        ],
        &[speakers::Window {
            index: 0,
            start_ms: 0,
            end_ms: 3000,
        }],
        plan.timeline_duration_ms(),
        || false,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&result.evidence().mapping).unwrap(),
        serde_json::to_vec(&graph).unwrap()
    );
    assert_eq!(result.evidence().mapping_policy, speakers::POLICY);
}

#[test]
fn native_slots_preserve_original_ids_unknown_text_time_and_order_without_anchor_support() {
    let plan = LocalMossPlan::new(candidate_spec(&[3000 * FRAMES_PER_MS])).unwrap();
    let rows = vec![
        segment(0, 1000, 9, " one "),
        segment(500, 1500, 9, "\ttwo"),
        segment(500, 1800, 2, "世界!"),
        // This nonadjacent overlap is adapted to unknown. Native mapping must
        // not recover the original raw slot 9 after adaptation rejected it.
        segment(1400, 1850, 9, " conflicting "),
        segment(1900, 2300, 0, " unknown "),
        segment(2300, 3000, 9, "again"),
    ];
    let result = finalize_segments(&plan, vec![rows], &[(2999, 3000, 1)]);
    assert_eq!(
        result.transcript_json(),
        &json!([
            {"start_time":0,"end_time":1500,"speaker":{"id":"moss_speaker_9"},"content":" one  \ttwo","stt_backend":BACKEND,"model_id":moss::MODEL_ID,"language":"auto"},
        {"start_time":500,"end_time":1800,"speaker":{"id":"moss_speaker_2"},"content":"世界!","stt_backend":BACKEND,"model_id":moss::MODEL_ID,"language":"auto"},
        {"start_time":1400,"end_time":1850,"speaker":{"id":"local_unknown"},"content":" conflicting ","stt_backend":BACKEND,"model_id":moss::MODEL_ID,"language":"auto"},
            {"start_time":1900,"end_time":2300,"speaker":{"id":"local_unknown"},"content":" unknown ","stt_backend":BACKEND,"model_id":moss::MODEL_ID,"language":"auto"},
            {"start_time":2300,"end_time":3000,"speaker":{"id":"moss_speaker_9"},"content":"again","stt_backend":BACKEND,"model_id":moss::MODEL_ID,"language":"auto"}
        ])
    );
    assert_eq!(result.evidence().coalesced_source_segments, 1);
    assert_eq!(result.evidence().mapping_policy, COMPOSED_MAPPING_POLICY);
    assert_eq!(
        serde_json::to_value(&result.evidence().mapping).unwrap(),
        json!({
            "policy": COMPOSED_MAPPING_POLICY,
            "assignment_source": "adapted_joint_moss_slots",
        "assignments": ["moss_speaker_9", "moss_speaker_2", null, null, "moss_speaker_9"],
            "windows": [{"index":0,"start_ms":0,"end_ms":3000}],
        "input_unknown_segments":2,"output_unknown_segments":2,"new_unknown_segments":0
        })
    );
    assert_eq!(result.evidence().diarization_receipt.window_index, None);
}

#[test]
fn composed_multiwindow_uses_identical_graph_assignments_and_evidence() {
    let lengths = [moss::windows::MAX_WINDOW_FRAMES, 2000 * FRAMES_PER_MS];
    let candidate = LocalMossPlan::new(candidate_spec(&lengths)).unwrap();
    let graph = LocalMossPlan::new(plan_spec(&lengths)).unwrap();
    let segments = vec![
        vec![segment(0, 1500, 9, "first")],
        vec![
            segment(0, 300, 0, "unknown"),
            segment(500, 2000, 9, "second"),
        ],
    ];
    let intervals = [(0, 720_000, 1), (720_000, 722_000, 2)];
    let candidate_result = finalize_segments(&candidate, segments.clone(), &intervals);
    let graph_result = finalize_segments(&graph, segments, &intervals);
    assert_eq!(candidate_result.json_bytes(), graph_result.json_bytes());
    assert_eq!(
        serde_json::to_vec(&candidate_result.evidence().mapping).unwrap(),
        serde_json::to_vec(&graph_result.evidence().mapping).unwrap()
    );
    assert_eq!(
        candidate_result.evidence().mapping_policy,
        COMPOSED_MAPPING_POLICY
    );
    assert_eq!(graph_result.evidence().mapping_policy, speakers::POLICY);
    assert!(matches!(
        candidate_result.evidence().mapping,
        MossMappingEvidence::Graph(_)
    ));
}

#[test]
fn native_mapping_cancels_mid_assignment_and_retains_raw_order_and_tail_gates() {
    let plan = LocalMossPlan::new(candidate_spec(&[3000 * FRAMES_PER_MS])).unwrap();
    let responses = CompleteMossResponses::new(
        &plan,
        vec![window_response(
            &plan,
            0,
            vec![
                segment(0, 1000, 9, "one"),
                segment(1000, 2000, 2, "two"),
                segment(2000, 3000, 0, "three"),
            ],
        )],
    )
    .unwrap();
    let mut polls = 0;
    assert_eq!(
        finalize_with_cancel(&plan, &responses, &anchors(&plan, &[(0, 3000, 1)]), || {
            polls += 1;
            polls == 4
        })
        .unwrap_err()
        .code,
        "mapping_cancelled"
    );
    assert_eq!(polls, 4);
    for rows in [
        vec![
            segment(1000, 2000, 1, "later"),
            segment(0, 1500, 1, "earlier"),
        ],
        vec![segment(0, 3101, 1, "beyond allowed tail")],
    ] {
        let bytes = serde_json::to_vec(&response(&plan, 0, rows)).unwrap();
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
    }
}

#[test]
fn schema_and_mapping_field_tampering_fail_closed() {
    let candidate = LocalMossPlan::new(candidate_spec(&[16_000])).unwrap();
    let original = serde_json::to_value(candidate.spec()).unwrap();
    for schema in [0, 1, 2, 4] {
        let mut value = original.clone();
        value["schema_version"] = json!(schema);
        assert!(LocalMossPlan::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for policy in [Value::Null, json!("unknown-policy"), json!(1)] {
        let mut value = original.clone();
        value["mapping_policy"] = policy;
        assert!(LocalMossPlan::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("mapping_policy");
    assert!(LocalMossPlan::from_json(&serde_json::to_vec(&missing).unwrap()).is_err());
    missing["schema_version"] = json!(2);
    let legacy = LocalMossPlan::from_json(&serde_json::to_vec(&missing).unwrap()).unwrap();
    assert_eq!(legacy.mapping_policy(), speakers::POLICY);
    missing["mapping_policy"] = Value::Null;
    assert!(LocalMossPlan::from_json(&serde_json::to_vec(&missing).unwrap()).is_err());
    for policy in [speakers::POLICY, COMPOSED_MAPPING_POLICY] {
        let mut value = original.clone();
        value["mapping_policy"] = json!(policy);
        let restored = LocalMossPlan::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(restored.mapping_policy(), policy);
        assert_eq!(
            restored.windows()[0].request_bytes(),
            candidate.windows()[0].request_bytes()
        );
        assert_eq!(
            restored.diarization_request_bytes(),
            candidate.diarization_request_bytes()
        );
    }
}
