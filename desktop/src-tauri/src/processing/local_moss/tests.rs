use super::*;
use crate::processing::{local_whisper::*, transcript_for_summary};
use uuid::Uuid;

mod policy;
mod timing;

fn plan_spec(frame_lengths: &[u64]) -> LocalMossPlanSpec {
    let recording_id = Uuid::from_u128(42);
    let frames = frame_lengths.iter().sum::<u64>();
    let source = SourceAudioIdentity {
        relative_path: format!("inbox/{recording_id}/tracks/original.m4a"),
        sha256: digest(b"synthetic original source"),
        size_bytes: 4096,
        duration_ms: frames.div_ceil(FRAMES_PER_MS) + 64,
    };
    let mut start = 0;
    let windows = frame_lengths
        .iter()
        .enumerate()
        .map(|(index, frames)| {
            let window = MossWindowRequestSpec {
                index,
                start_frame: start,
                end_frame: start + frames,
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
                        "inbox/{recording_id}/derived/moss_generation1_{index}.wav"
                    ),
                    audio_sha256: digest(format!("synthetic PCM window {index}").as_bytes()),
                    audio_size_bytes: frames * 2 + 44,
                    audio_duration_ms: frames.div_ceil(FRAMES_PER_MS),
                    language: None,
                },
            };
            start += frames;
            window
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
        audio_relative_path: "tracks/original.m4a".into(),
        audio_sha256: source.sha256.clone(),
        audio_size_bytes: source.size_bytes,
        audio_duration_ms: source.duration_ms,
        expected_speaker_count: None,
    };
    LocalMossPlanSpec {
        schema_version: LOCAL_MOSS_PLAN_VERSION,
        mapping_policy: Some(speakers::POLICY.into()),
        recording_id,
        source,
        pcm_sample_rate: moss::windows::SAMPLE_RATE,
        pcm_source_frames: frames,
        pcm_quantization_policy: None,
        window_policy: moss::windows::QUIET_WINDOW_POLICY.into(),
        windows,
        diarization_request,
    }
}

fn segment(start_ms: i64, end_ms: i64, speaker_id: u32, text: &str) -> moss::MossSegment {
    moss::MossSegment {
        start_ms,
        end_ms,
        speaker_id,
        text: text.into(),
    }
}

fn response(
    plan: &LocalMossPlan,
    index: usize,
    segments: Vec<moss::MossSegment>,
) -> moss::MossResponse {
    let request = plan.windows()[index].request();
    moss::MossResponse {
        schema_version: moss::PROTOCOL_VERSION,
        recording_id: request.recording_id,
        runtime_id: request.runtime_id.clone(),
        model_id: request.model_id.clone(),
        model_sha256: request.model_sha256.clone(),
        audio_sha256: request.audio_sha256.clone(),
        timing_policy: request.timing_policy.clone(),
        complete: true,
        segments,
    }
}

fn binding(plan: &LocalMossPlan, request_hash: &str, bytes: &[u8]) -> ResponseBinding {
    ResponseBinding {
        plan_sha256: plan.plan_sha256().into(),
        request_sha256: request_hash.into(),
        response_sha256: digest(bytes),
    }
}

fn window_response(
    plan: &LocalMossPlan,
    index: usize,
    segments: Vec<moss::MossSegment>,
) -> ValidatedMossWindowResponse {
    let bytes = serde_json::to_vec(&response(plan, index, segments)).unwrap();
    ValidatedMossWindowResponse::decode(
        plan,
        index,
        binding(plan, plan.windows()[index].request_sha256(), &bytes),
        &bytes,
    )
    .unwrap()
}

fn anchor_response(
    plan: &LocalMossPlan,
    intervals: &[(u64, u64, u32)],
) -> LocalDiarizationResponse {
    let request = plan.diarization_request();
    LocalDiarizationResponse {
        schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
        recording_id: request.recording_id,
        pack_id: request.pack_id.clone(),
        quality_preset: request.quality_preset.clone(),
        audio_sha256: request.audio_sha256.clone(),
        speaker_count: intervals.iter().map(|entry| entry.2).max().unwrap(),
        segments: intervals
            .iter()
            .map(
                |&(start_ms, end_ms, speaker_slot)| LocalDiarizationSegment {
                    start_ms,
                    end_ms,
                    speaker_slot,
                    confidence_milli: 0,
                },
            )
            .collect(),
    }
}

fn anchors(plan: &LocalMossPlan, intervals: &[(u64, u64, u32)]) -> ValidatedSpeakerKitResponse {
    let bytes = serde_json::to_vec(&anchor_response(plan, intervals)).unwrap();
    ValidatedSpeakerKitResponse::decode(
        plan,
        binding(plan, plan.diarization_request_sha256(), &bytes),
        &bytes,
    )
    .unwrap()
}

#[test]
fn overlapping_moss_speakers_reach_canonical_archive_and_summary_without_retiming() {
    let plan = LocalMossPlan::new(plan_spec(&[3000 * FRAMES_PER_MS])).unwrap();
    let responses = CompleteMossResponses::new(
        &plan,
        vec![window_response(
            &plan,
            0,
            vec![
                segment(0, 1800, 1, "hello 世界"),
                segment(1000, 3000, 2, "yes"),
            ],
        )],
    )
    .unwrap();
    let result = finalize(
        &plan,
        &responses,
        &anchors(&plan, &[(0, 1500, 1), (1500, 3000, 2)]),
    )
    .unwrap();
    let json = result.transcript_json();
    assert_eq!(json[0]["start_time"], 0);
    assert_eq!(json[0]["end_time"], 1800);
    assert_eq!(json[1]["start_time"], 1000);
    assert_eq!(json[1]["end_time"], 3000);
    assert_eq!(json[0]["speaker"]["id"], "local_speaker_01");
    assert_eq!(json[1]["speaker"]["id"], "local_speaker_02");
    assert_eq!(json[0]["stt_backend"], "moss_local");
    assert_eq!(json[0]["model_id"], moss::MODEL_ID);
    assert_eq!(json[0]["language"], "auto");
    assert_eq!(
        transcript_for_summary(json).unwrap(),
        "SPEAKER_local_speaker_01: hello 世界\nSPEAKER_local_speaker_02: yes\n"
    );
    let before_archive_formatting = json.clone();
    assert_eq!(
        crate::processing::archive::moss_transcript_text_for_test(json).unwrap(),
        "[00:00:00 - 00:00:01] SPEAKER_local_speaker_01: hello 世界\n[00:00:01 - 00:00:03] SPEAKER_local_speaker_02: yes"
    );
    assert_eq!(*json, before_archive_formatting);
    assert_eq!(
        serde_json::from_slice::<Value>(result.json_bytes()).unwrap(),
        *json
    );
    assert!(json
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["speaker"]["id"]
            .as_str()
            .is_some_and(|id| id.len() <= 32 && id.is_ascii())));
}

#[test]
fn adjacent_coalescing_preserves_joined_text_and_reports_source_counts() {
    let plan = LocalMossPlan::new(plan_spec(&[3000 * FRAMES_PER_MS])).unwrap();
    let responses = CompleteMossResponses::new(
        &plan,
        vec![window_response(
            &plan,
            0,
            vec![
                segment(0, 1200, 1, " one "),
                segment(1000, 2000, 1, "\ttwo"),
                segment(2500, 3000, 2, "three"),
            ],
        )],
    )
    .unwrap();
    let result = finalize(
        &plan,
        &responses,
        &anchors(&plan, &[(0, 2000, 1), (2000, 3000, 2)]),
    )
    .unwrap();
    assert_eq!(result.transcript_json().as_array().unwrap().len(), 2);
    assert_eq!(result.transcript_json()[0]["content"], " one  \ttwo");
    assert_eq!(result.transcript_json()[0]["end_time"], 2000);
    assert_eq!(result.evidence().source_segments, 3);
    assert_eq!(result.evidence().coalesced_source_segments, 1);
    assert_eq!(
        transcript_for_summary(result.transcript_json()).unwrap(),
        "SPEAKER_local_speaker_01:  one  \ttwo\nSPEAKER_local_speaker_02: three\n"
    );
}

#[test]
fn repeated_local_slots_receive_global_anchors_per_window_and_unknown_stays_unknown() {
    let plan = LocalMossPlan::new(plan_spec(&[
        moss::windows::MAX_WINDOW_FRAMES,
        2000 * FRAMES_PER_MS,
    ]))
    .unwrap();
    let responses = CompleteMossResponses::new(
        &plan,
        vec![
            window_response(&plan, 0, vec![segment(0, 1500, 1, "first")]),
            window_response(
                &plan,
                1,
                vec![
                    segment(0, 300, 0, "unattributed"),
                    segment(500, 2000, 1, "second"),
                ],
            ),
        ],
    )
    .unwrap();
    let result = finalize(
        &plan,
        &responses,
        &anchors(&plan, &[(0, 720_000, 1), (720_000, 722_000, 2)]),
    )
    .unwrap();
    let rows = result.transcript_json();
    assert_eq!(rows[0]["speaker"]["id"], "local_speaker_01");
    assert_eq!(rows[1]["speaker"]["id"], "local_unknown");
    assert_eq!(rows[1]["start_time"], 720_000);
    assert_eq!(rows[1]["end_time"], 720_300);
    assert_eq!(rows[2]["speaker"]["id"], "local_speaker_02");
    assert_eq!(rows[2]["start_time"], 720_500);
    assert_eq!(rows[2]["end_time"], 722_000);
    assert_eq!(result.evidence().mapping.output_unknown_segments(), 1);
    assert_eq!(transcript_for_summary(rows).unwrap(), "SPEAKER_local_speaker_01: first\nSPEAKER_local_unknown: unattributed\nSPEAKER_local_speaker_02: second\n");
}

#[test]
fn final_partial_pcm_frame_is_retained_with_explicit_ceil_metadata() {
    let plan = LocalMossPlan::new(plan_spec(&[16_001])).unwrap();
    assert_eq!(plan.windows()[0].end_frame(), 16_001);
    assert_eq!(plan.windows()[0].request().audio_duration_ms, 1001);
    let responses = CompleteMossResponses::new(
        &plan,
        vec![window_response(&plan, 0, vec![segment(0, 1001, 1, "tail")])],
    )
    .unwrap();
    let result = finalize(&plan, &responses, &anchors(&plan, &[(0, 1001, 1)])).unwrap();
    assert_eq!(result.evidence().pcm_source_frames, 16_001);
    assert_eq!(result.evidence().final_timestamp_round_up_frames, 15);
    assert_eq!(result.evidence().pcm_timestamp_policy, PCM_TIMESTAMP_POLICY);
    assert_eq!(result.transcript_json()[0]["end_time"], 1001);
    let spec = plan_spec(&[moss::windows::MAX_WINDOW_FRAMES - 1, 16_000]);
    assert!(LocalMossPlan::new(spec).is_err());
}

#[test]
fn optional_pcm_quantization_policy_preserves_legacy_plan_bytes_and_is_evidenced() {
    // The old DTO's field order and shape define existing immutable plan hashes.
    #[derive(Serialize)]
    struct LegacyPlan<'a> {
        schema_version: u32,
        recording_id: Uuid,
        source: &'a SourceAudioIdentity,
        pcm_sample_rate: u64,
        pcm_source_frames: u64,
        window_policy: &'a str,
        windows: &'a [MossWindowRequestSpec],
        diarization_request: &'a LocalDiarizationRequest,
    }
    let mut spec = plan_spec(&[16_001]);
    spec.schema_version = 2;
    spec.mapping_policy = None;
    let legacy = LegacyPlan {
        schema_version: spec.schema_version,
        recording_id: spec.recording_id,
        source: &spec.source,
        pcm_sample_rate: spec.pcm_sample_rate,
        pcm_source_frames: spec.pcm_source_frames,
        window_policy: &spec.window_policy,
        windows: &spec.windows,
        diarization_request: &spec.diarization_request,
    };
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
    let restored = LocalMossPlan::from_json(&legacy_bytes).unwrap();
    assert_eq!(restored.plan_bytes(), legacy_bytes);
    assert_eq!(restored.plan_sha256(), digest(&legacy_bytes));
    assert!(restored.spec().pcm_quantization_policy.is_none());

    let mut explicit = spec;
    explicit.pcm_quantization_policy = Some(echowall_local_audio::PCM_QUANTIZATION_POLICY.into());
    let plan = LocalMossPlan::new_for_preparation(explicit.clone()).unwrap();
    assert_ne!(plan.plan_sha256(), restored.plan_sha256());
    let responses = CompleteMossResponses::new(
        &plan,
        vec![window_response(&plan, 0, vec![segment(0, 1001, 1, "tail")])],
    )
    .unwrap();
    let result = finalize(&plan, &responses, &anchors(&plan, &[(0, 1001, 1)])).unwrap();
    assert_eq!(
        result.evidence().pcm_quantization_policy.as_deref(),
        Some(echowall_local_audio::PCM_QUANTIZATION_POLICY)
    );
    explicit.pcm_quantization_policy = Some("unversioned-cast".into());
    assert_eq!(
        LocalMossPlan::new_for_preparation(explicit)
            .unwrap_err()
            .code,
        "local_moss_plan_invalid"
    );
}

#[test]
fn floored_container_duration_uses_declared_bound_without_stretching_anchors() {
    let mut spec = plan_spec(&[32_001]);
    spec.source.duration_ms = 2000;
    spec.diarization_request.audio_duration_ms = 2000;
    let plan = LocalMossPlan::new(spec).unwrap();
    assert_eq!(plan.pcm_duration_ms_ceil(), 2001);
    assert_eq!(plan.timeline_duration_ms(), 2001);
    let responses = CompleteMossResponses::new(
        &plan,
        vec![window_response(
            &plan,
            0,
            vec![segment(0, 2001, 1, "fractional end")],
        )],
    )
    .unwrap();
    let anchor = anchors(&plan, &[(0, 2000, 1)]);
    let result = finalize(&plan, &responses, &anchor).unwrap();
    assert_eq!(anchor.response.segments[0].end_ms, 2000);
    assert_eq!(result.transcript_json()[0]["end_time"], 2001);
    assert_eq!(result.evidence().pcm_source_frames, 32_001);
    assert_eq!(result.evidence().source_container_duration_ms, 2000);
    assert_eq!(result.evidence().validated_timeline_duration_ms, 2001);
    assert_eq!(result.evidence().final_timestamp_round_up_frames, 15);
    for source_duration in [1899, 2102, 5_400_000] {
        let mut spec = plan.spec().clone();
        spec.source.duration_ms = source_duration;
        spec.diarization_request.audio_duration_ms = source_duration;
        assert!(LocalMossPlan::new(spec).is_err());
    }
}

#[test]
fn quiet_window_policy_and_original_source_cannot_be_mislabeled() {
    assert!(LocalMossPlan::new(plan_spec(&[1000 * FRAMES_PER_MS, 1000 * FRAMES_PER_MS])).is_err());
    let minimum = moss::windows::MAX_WINDOW_FRAMES - moss::windows::QUIET_LOOKBACK_FRAMES;
    assert!(LocalMossPlan::new(plan_spec(&[minimum, 16_000])).is_ok());
    assert!(LocalMossPlan::new(plan_spec(&[
        minimum - moss::windows::QUIET_CUT_GRID_FRAMES as u64,
        16_000
    ]))
    .is_err());
    assert!(LocalMossPlan::new(plan_spec(&[minimum + FRAMES_PER_MS, 16_000])).is_err());
    let mut alias = plan_spec(&[moss::windows::MAX_WINDOW_FRAMES, 16_000]);
    alias.source.relative_path = alias.windows[0].request.audio_relative_path.clone();
    alias.diarization_request.audio_relative_path = alias.source.relative_path.clone();
    assert!(LocalMossPlan::new(alias).is_err());
    let mut alias = plan_spec(&[moss::windows::MAX_WINDOW_FRAMES, 16_000]);
    alias.source.relative_path = alias.windows[0]
        .request
        .audio_relative_path
        .replace("moss_generation", "MOSS_GENERATION");
    alias.diarization_request.audio_relative_path = alias.source.relative_path.clone();
    assert!(LocalMossPlan::new(alias).is_err());
}

#[test]
fn validated_plan_roundtrip_is_immutable_and_languages_are_consistent() {
    for language in [None, Some("en"), Some("zh")] {
        let mut spec = plan_spec(&[moss::windows::MAX_WINDOW_FRAMES, 16_000]);
        for window in &mut spec.windows {
            window.request.language = language.map(str::to_owned);
        }
        let plan = LocalMossPlan::new(spec).unwrap();
        let roundtrip = LocalMossPlan::from_json(plan.plan_bytes()).unwrap();
        assert_eq!(plan.plan_sha256(), roundtrip.plan_sha256());
        assert_eq!(plan.requested_language(), language);
        assert_eq!(
            digest(plan.windows()[0].request_bytes()),
            plan.windows()[0].request_sha256()
        );
        assert_eq!(
            digest(plan.diarization_request_bytes()),
            plan.diarization_request_sha256()
        );
        let mut changed = plan.spec().clone();
        changed.windows[1].request.language = Some("other".into());
        assert!(LocalMossPlan::new(changed).is_err());
        assert_eq!(plan.plan_sha256(), roundtrip.plan_sha256());
    }
    let mut mixed = plan_spec(&[moss::windows::MAX_WINDOW_FRAMES, 16_000]);
    mixed.windows[1].request.language = Some("zh".into());
    assert!(LocalMossPlan::new(mixed).is_err());
}

#[test]
fn plans_reject_identity_coordinate_model_and_ownership_mismatches() {
    let base = plan_spec(&[moss::windows::MAX_WINDOW_FRAMES, 16_000]);
    let mut invalid = Vec::new();
    let mut spec = base.clone();
    spec.recording_id = Uuid::nil();
    invalid.push(spec);
    let mut spec = base.clone();
    spec.windows[0].request.recording_id = Uuid::from_u128(99);
    invalid.push(spec);
    let mut spec = base.clone();
    spec.windows[0].request.model_sha256 = digest(b"other model");
    invalid.push(spec);
    let mut spec = base.clone();
    spec.windows[0].request.timing_policy = moss::TIMING_POLICY.into();
    invalid.push(spec);
    let mut spec = base.clone();
    spec.windows[0].request.audio_duration_ms += 1;
    invalid.push(spec);
    let mut spec = base.clone();
    spec.windows[1].start_frame += 16;
    invalid.push(spec);
    let mut spec = base.clone();
    spec.windows[1].end_frame -= 16;
    spec.windows[1].request.audio_duration_ms -= 1;
    invalid.push(spec);
    let mut spec = base.clone();
    spec.windows[1].index = 0;
    invalid.push(spec);
    let mut spec = base.clone();
    spec.windows[1].request.audio_relative_path =
        spec.windows[0].request.audio_relative_path.clone();
    invalid.push(spec);
    let mut spec = base.clone();
    spec.source.relative_path = "inbox/other/source.m4a".into();
    spec.diarization_request.audio_relative_path = spec.source.relative_path.clone();
    invalid.push(spec);
    let mut spec = base.clone();
    spec.diarization_request.recording_id = Uuid::from_u128(99);
    invalid.push(spec);
    let mut spec = base.clone();
    spec.diarization_request.audio_sha256 = digest(b"other source");
    invalid.push(spec);
    let mut spec = base.clone();
    spec.diarization_request.pack_id = "fluid-v1".into();
    spec.diarization_request.quality_preset = LOCAL_DIARIZATION_QUALITY_PRESET.into();
    invalid.push(spec);
    let mut spec = base.clone();
    spec.diarization_request.model_files.pop();
    invalid.push(spec);
    let mut spec = base;
    spec.pcm_sample_rate = 48000;
    invalid.push(spec);
    assert!(invalid
        .into_iter()
        .all(|spec| LocalMossPlan::new(spec).is_err()));
}

#[test]
fn response_bindings_prevent_cross_plan_window_recording_and_model_reuse() {
    let plan = LocalMossPlan::new(plan_spec(&[moss::windows::MAX_WINDOW_FRAMES, 16_000])).unwrap();
    let response = response(&plan, 0, vec![segment(0, 1000, 1, "one")]);
    let bytes = serde_json::to_vec(&response).unwrap();
    let correct = binding(&plan, plan.windows()[0].request_sha256(), &bytes);
    let mut wrong = correct.clone();
    wrong.plan_sha256 = digest(b"other plan");
    assert!(ValidatedMossWindowResponse::decode(&plan, 0, wrong, &bytes).is_err());
    assert!(ValidatedMossWindowResponse::decode(&plan, 1, correct.clone(), &bytes).is_err());
    let mut wrong = correct;
    wrong.response_sha256 = digest(b"different bytes");
    assert!(ValidatedMossWindowResponse::decode(&plan, 0, wrong, &bytes).is_err());
    for mutate in [0, 1, 2, 3, 4] {
        let mut broken = response.clone();
        match mutate {
            0 => broken.recording_id = Uuid::from_u128(99),
            1 => broken.model_sha256 = digest(b"other model"),
            2 => broken.audio_sha256 = digest(b"other audio"),
            3 => broken.complete = false,
            _ => {
                broken.segments[0].end_ms =
                    plan.windows()[0].request().audio_duration_ms as i64 + 101
            }
        }
        let bytes = serde_json::to_vec(&broken).unwrap();
        assert!(ValidatedMossWindowResponse::decode(
            &plan,
            0,
            binding(&plan, plan.windows()[0].request_sha256(), &bytes),
            &bytes
        )
        .is_err());
    }
    let mut diarization = anchor_response(&plan, &[(0, 2000, 1)]);
    diarization.audio_sha256 = digest(b"other source");
    let bytes = serde_json::to_vec(&diarization).unwrap();
    assert!(ValidatedSpeakerKitResponse::decode(
        &plan,
        binding(&plan, plan.diarization_request_sha256(), &bytes),
        &bytes
    )
    .is_err());
}

#[test]
fn v2_uses_exact_package_relative_diarization_and_v1_bytes_are_read_only() {
    let mut spec = plan_spec(&[16_000]);
    spec.schema_version = 2;
    spec.mapping_policy = None;
    let plan = LocalMossPlan::from_json(&serde_json::to_vec(&spec).unwrap()).unwrap();
    plan.require_executable().unwrap();
    assert_eq!(
        plan.diarization_request().audio_relative_path,
        "tracks/original.m4a"
    );
    assert_eq!(
        format!(
            "inbox/{}/{}",
            spec.recording_id,
            plan.diarization_request().audio_relative_path
        ),
        spec.source.relative_path
    );
    let mut wrong_v2 = spec.clone();
    wrong_v2.diarization_request.audio_relative_path = spec.source.relative_path.clone();
    assert!(LocalMossPlan::new_for_preparation(wrong_v2).is_err());

    let mut legacy = spec;
    legacy.schema_version = 1;
    legacy.diarization_request.audio_relative_path = legacy.source.relative_path.clone();
    assert_eq!(
        LocalMossPlan::new(legacy.clone()).unwrap_err().code,
        "legacy_moss_plan_requires_reprepare"
    );
    let bytes = serde_json::to_vec_pretty(&legacy).unwrap();
    let retained = LocalMossPlan::from_json(&bytes).unwrap();
    assert_eq!(retained.plan_bytes(), bytes);
    assert_eq!(retained.plan_sha256(), digest(&bytes));
    assert_eq!(
        retained.diarization_request_bytes(),
        encode_diarization_request(&legacy.diarization_request).unwrap()
    );
    assert_eq!(
        retained.require_executable().unwrap_err().code,
        "legacy_moss_plan_requires_reprepare"
    );
}

#[test]
fn only_complete_ordered_response_sets_can_finalize() {
    let plan = LocalMossPlan::new(plan_spec(&[moss::windows::MAX_WINDOW_FRAMES, 16_000])).unwrap();
    let first = window_response(&plan, 0, vec![segment(0, 1000, 1, "first")]);
    let second = window_response(&plan, 1, vec![segment(0, 1000, 1, "second")]);
    assert!(CompleteMossResponses::new(&plan, vec![first.clone()]).is_err());
    assert!(CompleteMossResponses::new(&plan, vec![first.clone(), first.clone()]).is_err());
    assert!(CompleteMossResponses::new(&plan, vec![second.clone(), first.clone()]).is_err());
    assert!(
        CompleteMossResponses::new(&plan, vec![first.clone(), second.clone(), second.clone()])
            .is_err()
    );
    let complete = CompleteMossResponses::try_collect(&plan, [Ok(first), Ok(second)]).unwrap();
    let mut changed = plan.spec().clone();
    changed.diarization_request.model_files[0].sha256 = digest(b"other model identity");
    let other = LocalMossPlan::new(changed).unwrap();
    assert!(finalize(&other, &complete, &anchors(&plan, &[(0, 2000, 1)])).is_err());
    assert!(
        finalize_with_cancel(&plan, &complete, &anchors(&plan, &[(0, 2000, 1)]), || true).is_err()
    );
}

#[test]
fn archive_segment_and_serialized_json_limits_fail_closed() {
    let plan = LocalMossPlan::new(plan_spec(&[
        moss::windows::MAX_WINDOW_FRAMES,
        6000 * FRAMES_PER_MS,
    ]))
    .unwrap();
    let segments = || {
        (0..5001)
            .map(|index| segment(index, index + 1, 1, "x"))
            .collect()
    };
    let first = window_response(&plan, 0, segments());
    let second = window_response(&plan, 1, segments());
    assert!(CompleteMossResponses::new(&plan, vec![first, second]).is_err());
    assert!(bounded_json(
        &json!("x".repeat(MAX_ARCHIVE_JSON_BYTES + 1)),
        MAX_ARCHIVE_JSON_BYTES
    )
    .is_err());
    assert!(LocalMossPlan::from_json(&vec![b' '; MAX_PLAN_BYTES + 1]).is_err());
}
