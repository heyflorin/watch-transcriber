use super::*;

mod chronology;

fn request() -> MossRequest {
    MossRequest {
        schema_version: PROTOCOL_VERSION,
        recording_id: Uuid::parse_str("018f92d8-6ad4-7dc1-8e28-8b020d2942cb").unwrap(),
        runtime_id: RUNTIME_ID.to_owned(),
        model_id: MODEL_ID.to_owned(),
        model_revision: MODEL_REVISION.to_owned(),
        model_sha256: MODEL_SHA256.to_owned(),
        model_size_bytes: MODEL_SIZE_BYTES,
        timing_policy: TIMING_POLICY.to_owned(),
        audio_relative_path: "inbox/018f92d8-6ad4-7dc1-8e28-8b020d2942cb/tracks/imported.wav"
            .to_owned(),
        audio_sha256: "a".repeat(64),
        audio_size_bytes: 32_044,
        audio_duration_ms: 1_000,
        language: None,
    }
}

fn segment(start_ms: i64, end_ms: i64, speaker_id: u32, text: &str) -> MossSegment {
    MossSegment {
        start_ms,
        end_ms,
        speaker_id,
        text: text.to_owned(),
    }
}

fn response(segments: Vec<MossSegment>) -> MossResponse {
    let request = request();
    MossResponse {
        schema_version: PROTOCOL_VERSION,
        recording_id: request.recording_id,
        runtime_id: request.runtime_id,
        model_id: request.model_id,
        model_sha256: request.model_sha256,
        audio_sha256: request.audio_sha256,
        timing_policy: request.timing_policy,
        complete: true,
        segments,
    }
}

fn coalesced(segments: Vec<MossSegment>) -> Result<AdaptedTranscript, ProtocolError> {
    let mut request = request();
    request.timing_policy = COALESCING_TIMING_POLICY_V2.into();
    let mut source = response(segments);
    source.timing_policy = request.timing_policy.clone();
    source.adapt(&request)
}

#[test]
fn coalescing_unions_only_adjacent_known_same_speaker_overlaps_without_rewriting_text() {
    let output = coalesced(vec![
        segment(0, 500, 1, "first"),
        segment(480, 900, 1, "second"),
    ])
    .unwrap();
    assert_eq!(output.segments.len(), 1);
    assert_eq!(
        (output.segments[0].start_ms, output.segments[0].end_ms),
        (0, 900)
    );
    assert_eq!(output.segments[0].speaker_id, Some(1));
    assert_eq!(output.segments[0].text, "first second");
    assert_eq!(output.unknown_segments, 0);
    assert_eq!(output.coalesced_source_segments, 1);
}

#[test]
fn coalescing_keeps_interleaved_turn_order_and_real_other_speaker_overlap() {
    let output = coalesced(vec![
        segment(0, 700, 1, "first"),
        segment(400, 500, 2, "interjection"),
        segment(650, 900, 1, "last"),
    ])
    .unwrap();
    assert_eq!(output.segments.len(), 3);
    assert_eq!(output.unknown_segments, 1);
    assert_eq!(
        output
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "first interjection last"
    );
    assert_eq!(
        (output.segments[0].end_ms, output.segments[1].start_ms),
        (700, 400)
    );
    assert_eq!(output.segments[2].speaker_id, None);
}

#[test]
fn coalescing_preserves_chain_union_and_existing_tail_limit() {
    let output = coalesced(vec![
        segment(0, 500, 1, "a"),
        segment(450, 900, 1, "b"),
        segment(800, 1084, 1, "c"),
    ])
    .unwrap();
    assert_eq!(output.segments.len(), 1);
    assert_eq!(output.segments[0].text, "a b c");
    assert_eq!(output.segments[0].end_ms, 1000);
    assert_eq!(output.clipped_tail_ms, 84);
    assert_eq!(output.coalesced_source_segments, 2);
    assert!(coalesced(vec![segment(0, 1110, 1, "overrun")]).is_err());
}

#[test]
fn coalescing_never_merges_unknowns_or_exceeds_per_segment_text_limits() {
    assert!(coalesced(vec![segment(0, 500, 0, "a"), segment(450, 900, 0, "b")]).is_err());
    let output = coalesced(vec![
        segment(0, 500, 1, &"a".repeat(32768)),
        segment(450, 900, 1, &"b".repeat(32768)),
    ])
    .unwrap();
    assert_eq!(output.segments.len(), 2);
    assert_eq!(output.unknown_segments, 1);
    assert!(output.segments.iter().all(|s| s.text.len() <= 65536));
    let output = coalesced(vec![segment(0, 500, 1, "a"), segment(500, 900, 1, "b")]).unwrap();
    assert_eq!(output.segments.len(), 2);
}

#[test]
fn joint_response_preserves_real_overlap_and_text_without_changing_the_whisper_contract() {
    let source = response(vec![
        segment(0, 600, 1, "hello 世界"),
        segment(300, 900, 2, "yes"),
    ]);
    let adapted = source.adapt(&request()).unwrap();
    assert_eq!(adapted.segments.len(), 2);
    assert_eq!(adapted.segments[1].start_ms, 300);
    assert_eq!(adapted.segments[0].end_ms, 600);
    assert_eq!(adapted.segments[0].text, "hello 世界");
    assert_eq!(adapted.unknown_segments, 0);
    assert_eq!(
        decode_response(&encode_response(&source, &request()).unwrap(), &request()).unwrap(),
        source
    );
    assert_eq!(
        decode_request(&encode_request(&request()).unwrap()).unwrap(),
        request()
    );
}

#[test]
fn conflicting_same_speaker_segment_is_unknown_and_tail_adjustment_is_accounted_for() {
    let source = response(vec![
        segment(0, 500, 1, "first"),
        segment(400, 600, 1, "保留这段"),
        segment(650, 1_084, 2, "tail"),
    ]);
    let adapted = source.adapt(&request()).unwrap();
    assert_eq!(adapted.unknown_segments, 1);
    assert_eq!(adapted.conflicting_speaker_segments, 1);
    assert_eq!(adapted.clipped_tail_ms, 84);
    assert_eq!(adapted.segments[1].speaker_id, None);
    assert_eq!(adapted.segments[1].start_ms, 400);
    assert_eq!(adapted.segments[2].end_ms, 1_000);
    let expected: Vec<_> = source.segments.iter().map(|s| &s.text).collect();
    assert_eq!(
        adapted.segments.iter().map(|s| &s.text).collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn timing_policy_rejects_large_or_nonfinal_overruns_regressions_and_colliding_unknowns() {
    for segments in [
        vec![segment(0, 1_101, 1, "overshoot")],
        vec![
            segment(0, 1_010, 1, "nonfinal"),
            segment(500, 900, 2, "other"),
        ],
        vec![
            segment(10, 500, 1, "first"),
            segment(5, 900, 2, "regression"),
        ],
        vec![segment(-1, 500, 1, "negative")],
        vec![segment(100, 100, 1, "zero duration")],
        vec![segment(1_000, 1_050, 1, "entirely outside")],
        vec![
            segment(0, 500, 0, "unknown"),
            segment(400, 600, 0, "unknown"),
        ],
    ] {
        assert!(response(segments).adapt(&request()).is_err());
    }
}

#[test]
fn identities_completion_and_anonymous_slots_fail_closed() {
    let original = response(vec![segment(0, 900, 1, "fabricated")]);
    let mut candidate = original.clone();
    candidate.complete = false;
    assert!(candidate.adapt(&request()).is_err());
    candidate = original.clone();
    candidate.audio_sha256 = "b".repeat(64);
    assert!(candidate.adapt(&request()).is_err());
    candidate = original.clone();
    candidate.model_sha256 = "b".repeat(64);
    assert!(candidate.adapt(&request()).is_err());
    candidate = original.clone();
    candidate.timing_policy = "unversioned".into();
    assert!(candidate.adapt(&request()).is_err());
    candidate = original.clone();
    candidate.segments[0].speaker_id = 17;
    assert!(candidate.adapt(&request()).is_err());
    candidate = original;
    candidate.segments[0].text.push('\0');
    assert!(candidate.adapt(&request()).is_err());
}

#[test]
fn request_paths_are_recording_bound_and_models_are_pinned() {
    for path in [
        "/outside.wav",
        "../outside.wav",
        "inbox/018f92d8-6ad4-7dc1-8e28-8b020d2942cb/../other.wav",
        "inbox/018f92d8-6ad4-7dc1-8e28-8b020d2942cb//other.wav",
        "inbox/018f92d8-6ad4-7dc1-8e28-8b020d2942cb/./other.wav",
        "inbox/028f92d8-6ad4-7dc1-8e28-8b020d2942cb/tracks/other.wav",
    ] {
        let mut r = request();
        r.audio_relative_path = path.into();
        assert!(r.validate().is_err());
    }
    let mut r = request();
    r.model_revision = "main".into();
    assert!(r.validate().is_err());
    let mut r = request();
    r.language = Some("inferred-from-text".into());
    assert!(r.validate().is_err());
}

#[test]
fn frames_and_segment_text_are_bounded_and_unknown_fields_are_rejected() {
    assert!(decode_request(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
    assert!(decode_response(&vec![b' '; MAX_RESPONSE_BYTES + 1], &request()).is_err());
    let mut r = serde_json::to_value(request()).unwrap();
    r["url"] = "forbidden".into();
    assert!(decode_request(&serde_json::to_vec(&r).unwrap()).is_err());
    let mut r = serde_json::to_value(response(vec![segment(0, 900, 1, "text")])).unwrap();
    r["segments"][0]["confidence"] = 1.into();
    assert!(decode_response(&serde_json::to_vec(&r).unwrap(), &request()).is_err());
    assert!(
        response(vec![segment(0, 900, 1, &"x".repeat(64 * 1024 + 1))])
            .adapt(&request())
            .is_err()
    );
    assert!(response(vec![segment(0, 900, 1, "x"); MAX_SEGMENTS + 1])
        .adapt(&request())
        .is_err());
}
