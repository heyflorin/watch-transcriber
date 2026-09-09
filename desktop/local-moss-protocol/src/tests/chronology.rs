use super::*;

fn chronological(segments: Vec<MossSegment>) -> (MossRequest, MossResponse) {
    let mut request = request();
    request.timing_policy = CHRONOLOGICAL_TIMING_POLICY_V3.into();
    let mut source = response(segments);
    source.timing_policy = request.timing_policy.clone();
    (request, source)
}

#[test]
fn chronological_policy_sorts_cross_speakers_with_lossless_union_provenance() {
    let raw = vec![
        segment(500, 900, 1, " later "),
        segment(0, 300, 2, "早\t"),
        segment(250, 450, 2, "second"),
        segment(900, 1000, 0, "unknown"),
    ];
    let (request, source) = chronological(raw.clone());
    let before = serde_json::to_vec(&source).unwrap();
    let adapted = source.adapt(&request).unwrap();
    assert_eq!(source.segments, raw);
    assert_eq!(serde_json::to_vec(&source).unwrap(), before);
    let provenance = adapted.chronology.as_ref().unwrap();
    assert_eq!(provenance.source_index_permutation, [1, 2, 0, 3]);
    assert_eq!(
        provenance.output_source_indices,
        [vec![1, 2], vec![0], vec![3]]
    );
    assert_eq!(adapted.coalesced_source_segments, 1);
    assert_eq!(
        adapted.segments[0],
        ValidatedSegment {
            start_ms: 0,
            end_ms: 450,
            speaker_id: Some(2),
            text: "早\t second".into()
        }
    );
    assert_eq!(adapted.segments[1].text, " later ");
    assert_eq!(adapted.segments[1].speaker_id, Some(1));
    assert_eq!(adapted.segments[2].speaker_id, None);
    assert_eq!(adapted.unknown_segments, 1);
    let emitted = encode_response(&source, &request).unwrap();
    let decoded = decode_response(&emitted, &request).unwrap();
    assert_eq!(decoded.segments, raw);
    assert_eq!(decoded, source);
    assert_eq!(
        decode_request(&encode_request(&request).unwrap()).unwrap(),
        request
    );
    for (group, output) in provenance
        .output_source_indices
        .iter()
        .zip(&adapted.segments)
    {
        assert_eq!(
            group
                .iter()
                .map(|&index| raw[index].text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            output.text
        );
        assert_eq!(
            output.start_ms,
            group
                .iter()
                .map(|&index| raw[index].start_ms as u64)
                .min()
                .unwrap()
        );
        assert_eq!(
            output.end_ms,
            group
                .iter()
                .map(|&index| raw[index].end_ms as u64)
                .max()
                .unwrap()
        );
        assert!(group
            .iter()
            .all(|&index| raw[index].speaker_id == raw[group[0]].speaker_id));
    }
}

#[test]
fn chronological_policy_never_reorders_a_speaker_or_unknown_subsequence() {
    for slot in [0, 1, MAX_SPEAKERS] {
        let (request, source) = chronological(vec![
            segment(500, 600, slot, "first emitted"),
            segment(0, 100, 2, "other"),
            segment(200, 300, slot, "second emitted"),
        ]);
        assert_eq!(source.adapt(&request).unwrap_err().0, "invalid_timing");
    }
}

#[test]
fn chronological_equal_starts_are_stable_including_unknown_slots() {
    let (request, source) = chronological(vec![
        segment(500, 600, 1, "a"),
        segment(0, 100, 2, "earlier"),
        segment(500, 650, 0, "unknown"),
        segment(500, 700, 3, "b"),
    ]);
    let adapted = source.adapt(&request).unwrap();
    assert_eq!(
        adapted.chronology.unwrap().source_index_permutation,
        [1, 0, 2, 3]
    );
    assert_eq!(
        adapted
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>(),
        ["earlier", "a", "unknown", "b"]
    );
}

#[test]
fn chronological_tail_limit_requires_original_and_chronological_final_position() {
    let (request, source) = chronological(vec![
        segment(500, 700, 1, "later"),
        segment(0, 100, 2, "earlier"),
        segment(900, 1100, 3, "tail"),
    ]);
    let adapted = source.adapt(&request).unwrap();
    assert_eq!(adapted.clipped_tail_ms, 100);
    assert_eq!(adapted.segments.last().unwrap().end_ms, 1000);
    for raw in [
        vec![
            segment(900, 1084, 1, "original nonfinal"),
            segment(0, 500, 2, "original final"),
        ],
        vec![
            segment(800, 900, 1, "chronological final"),
            segment(0, 1084, 2, "original final"),
        ],
        vec![segment(0, 1101, 1, "101ms")],
        vec![segment(0, 1180, 1, "180ms")],
    ] {
        let (request, source) = chronological(raw);
        assert_eq!(source.adapt(&request).unwrap_err().0, "invalid_timing");
    }
}

#[test]
fn chronological_policy_keeps_original_field_completeness_and_overlap_gates() {
    for (raw, code) in [
        (vec![segment(-1, 100, 1, "negative")], "invalid_timing"),
        (
            vec![segment(500, 500, 1, "empty interval")],
            "invalid_timing",
        ),
        (vec![segment(1000, 1084, 1, "outside")], "invalid_timing"),
        (vec![segment(0, 100, 17, "invalid slot")], "invalid_segment"),
        (vec![segment(0, 100, 1, "\0")], "invalid_segment"),
        (vec![segment(0, 100, 1, "  ")], "invalid_segment"),
        (
            vec![
                segment(0, 600, 0, "unknown1"),
                segment(500, 900, 0, "unknown2"),
            ],
            "ambiguous_unknown_timing",
        ),
    ] {
        let (request, source) = chronological(raw);
        assert_eq!(source.adapt(&request).unwrap_err().0, code);
    }
    let (request, mut source) = chronological(vec![segment(0, 100, 1, "content")]);
    source.complete = false;
    assert_eq!(source.adapt(&request).unwrap_err().0, "invalid_response");
    source.complete = true;
    source.segments.clear();
    assert_eq!(source.adapt(&request).unwrap_err().0, "invalid_response");
}

#[test]
fn retained_v1_v2_bytes_results_and_rejections_are_unchanged() {
    let raw = vec![segment(0, 500, 1, "a"), segment(400, 900, 1, "b")];
    let v1 = response(raw.clone());
    let expected_v1 = serde_json::json!({"segments":[
        {"start_ms":0,"end_ms":500,"speaker_id":1,"text":"a"},
        {"start_ms":400,"end_ms":900,"speaker_id":null,"text":"b"}],
        "unknown_segments":1,"conflicting_speaker_segments":1,"clipped_tail_ms":0,"coalesced_source_segments":0});
    assert_eq!(
        serde_json::to_value(v1.adapt(&request()).unwrap()).unwrap(),
        expected_v1
    );
    let mut v2 = v1.clone();
    let mut v2_request = request();
    v2_request.timing_policy = COALESCING_TIMING_POLICY_V2.into();
    v2.timing_policy = v2_request.timing_policy.clone();
    let expected_v2_bytes = br#"{"segments":[{"start_ms":0,"end_ms":900,"speaker_id":1,"text":"a b"}],"unknown_segments":0,"conflicting_speaker_segments":0,"clipped_tail_ms":0,"coalesced_source_segments":1}"#;
    let adapted_v2 = v2.adapt(&v2_request).unwrap();
    assert_eq!(serde_json::to_vec(&adapted_v2).unwrap(), expected_v2_bytes);
    for (request, source) in [(request(), v1), (v2_request, v2)] {
        let mut expected = serde_json::to_vec(&source).unwrap();
        expected.push(b'\n');
        assert_eq!(encode_response(&source, &request).unwrap(), expected);
        let mut reordered = source;
        reordered.segments = vec![segment(500, 900, 1, "later"), segment(0, 400, 2, "earlier")];
        assert_eq!(reordered.adapt(&request).unwrap_err().0, "invalid_timing");
    }
    let (request, source) = chronological(raw);
    let mut v3 = source.adapt(&request).unwrap();
    assert_eq!(
        v3.chronology.take().unwrap().source_index_permutation,
        [0, 1]
    );
    assert_eq!(v3, adapted_v2);
}

#[test]
#[ignore = "read-only, hash-pinned retained public diagnostics; run explicitly without inference"]
fn retained_public_diagnostic_outputs_prove_v3_acceptance_and_tail_rejection() {
    use sha2::{Digest, Sha256};
    use std::{fs, path::Path};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("local-eval/matrix/diagnostics/moss-failed-window-replay-20260906");
    let hash = |bytes: &[u8]| {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let mut reports = Vec::new();
    for (case, request_hash, raw_hash) in [
        (
            "long_form_04",
            "0055b6d0cfe9ee327fb57dad061978a3aa4c67345b0ea8aa71f973eaa78d8818",
            "6eb1d2846195c0df1be929aa759d7c6eb6fdb66dc51e3b1f4fd6ba475b10c3fe",
        ),
        (
            "overlap_02",
            "3ac6fc6a24d32f1d269fdca79bef134fbef2a00938ec5813874179c7e1bb013c",
            "995b68274df74e9f469ebf391d2ab8b60518d098a8e1dbf014236331799deccf",
        ),
    ] {
        let request_path = root.join(format!("{case}-request.json"));
        let raw_path = root.join(format!("{case}-instrumented-stdout.bin"));
        for path in [&request_path, &raw_path] {
            assert!(fs::symlink_metadata(path).unwrap().is_file());
            assert!(fs::metadata(path).unwrap().len() <= MAX_RESPONSE_BYTES as u64);
        }
        let request_bytes = fs::read(&request_path).unwrap();
        let raw_bytes = fs::read(&raw_path).unwrap();
        assert_eq!(hash(&request_bytes), request_hash);
        assert_eq!(hash(&raw_bytes), raw_hash);
        let request = decode_request(&request_bytes).unwrap();
        let diagnostic: serde_json::Value = serde_json::from_slice(&raw_bytes).unwrap();
        let source: MossResponse =
            serde_json::from_value(diagnostic["explicit_response"].clone()).unwrap();
        assert_eq!(source.adapt(&request).unwrap_err().0, "invalid_timing");
        let mut candidate_request = request.clone();
        candidate_request.timing_policy = CHRONOLOGICAL_TIMING_POLICY_V3.into();
        let mut candidate_source = source.clone();
        candidate_source.timing_policy = candidate_request.timing_policy.clone();
        let result = candidate_source.adapt(&candidate_request);
        if case == "long_form_04" {
            let adapted = result.unwrap();
            assert_eq!(source.segments.len(), 153);
            assert_eq!(adapted.segments.len(), 151);
            assert_eq!(adapted.coalesced_source_segments, 2);
            assert_eq!(adapted.unknown_segments, 0);
            let provenance = adapted.chronology.as_ref().unwrap();
            assert_eq!(
                provenance
                    .source_index_permutation
                    .iter()
                    .enumerate()
                    .filter(|(index, original)| *index != **original)
                    .count(),
                4
            );
            assert_eq!(
                decode_response(
                    &encode_response(&candidate_source, &candidate_request).unwrap(),
                    &candidate_request
                )
                .unwrap()
                .segments,
                source.segments
            );
            reports.push(serde_json::json!({"case_id":case,"old_error":"invalid_timing","raw_segments":153,"v3_segments":151,"coalesced":2,"unknown":0,"moved_positions":4,"input_sha256":raw_hash}));
        } else {
            assert_eq!(result.unwrap_err().0, "invalid_timing");
            assert_eq!(
                source.segments.last().unwrap().end_ms - request.audio_duration_ms as i64,
                180
            );
            reports.push(serde_json::json!({"case_id":case,"old_error":"invalid_timing","v3_error":"invalid_timing","tail_overrun_ms":180,"input_sha256":raw_hash}));
        }
        assert_eq!(candidate_source.segments, source.segments);
        assert_eq!(fs::read(request_path).unwrap(), request_bytes);
        assert_eq!(fs::read(raw_path).unwrap(), raw_bytes);
    }
    println!(
        "{}",
        serde_json::json!({"scope":"read-only-retained-public-diagnostics-no-inference-or-migration","cases":reports})
    );
}
