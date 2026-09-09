//! Aggregate-only diagnostic for Qwen aligned-word + FluidAudio merging.
//!
//! The tool never prints token text, timestamps, paths, recording IDs, or
//! speaker labels. It is not bundled with the App.

use std::env;
use std::fs;

use echowall_local_qwen_protocol::{LocalQwenRequest, LocalQwenResponse};
use echowall_local_whisper_protocol::{
    merge_aligned_words_with_diarization, LocalAlignedWord, LocalDiarizationRequest,
    LocalDiarizationResponse,
};
use serde_json::json;

const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;

fn read_json<T: serde::de::DeserializeOwned>(path: &std::ffi::OsStr) -> Result<T, String> {
    let metadata = fs::metadata(path).map_err(|_| "input unavailable".to_owned())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_INPUT_BYTES {
        return Err("input rejected".to_owned());
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| "input unavailable".to_owned())?)
        .map_err(|_| "input invalid".to_owned())
}

fn main() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let qwen_request: LocalQwenRequest =
        read_json(&arguments.next().ok_or("four inputs required")?)?;
    let qwen_response: LocalQwenResponse =
        read_json(&arguments.next().ok_or("four inputs required")?)?;
    let diarization_request: LocalDiarizationRequest =
        read_json(&arguments.next().ok_or("four inputs required")?)?;
    let diarization_response: LocalDiarizationResponse =
        read_json(&arguments.next().ok_or("four inputs required")?)?;
    if arguments.next().is_some() {
        return Err("four inputs required".to_owned());
    }
    qwen_response
        .validate_against(&qwen_request)
        .map_err(|_| "Qwen identity mismatch".to_owned())?;
    let mut words: Vec<LocalAlignedWord> = qwen_response
        .segments
        .iter()
        .flat_map(|segment| {
            segment.words.iter().map(|word| LocalAlignedWord {
                start_ms: word.start_ms,
                end_ms: word.end_ms,
                text: word.text.clone(),
                speaker_id: None,
            })
        })
        .collect();
    let zero_before = words
        .iter()
        .filter(|word| word.start_ms == word.end_ms)
        .count();
    let mut zero_runs = 0_usize;
    let mut unbounded_zero_words = 0_usize;
    let mut reversed_anchor_zero_words = 0_usize;
    let mut zero_span_words = 0_usize;
    let mut positive_span_words = 0_usize;
    let mut position = 0_usize;
    while position < words.len() {
        if words[position].end_ms > words[position].start_ms {
            position += 1;
            continue;
        }
        let start = position;
        while position < words.len() && words[position].end_ms == words[position].start_ms {
            position += 1;
        }
        zero_runs += 1;
        let count = position - start;
        if start == 0 || position == words.len() {
            unbounded_zero_words += count;
        } else if words[start - 1].end_ms > words[position].start_ms {
            reversed_anchor_zero_words += count;
        } else if words[start - 1].end_ms == words[position].start_ms {
            zero_span_words += count;
        } else {
            positive_span_words += count;
        }
    }
    let stats = merge_aligned_words_with_diarization(
        &mut words,
        &diarization_response,
        &diarization_request,
    )
    .map_err(|_| "merge rejected".to_owned())?;
    let mut chunk_offset = 0_usize;
    let mut zero_duration_fallback_chunks = 0_usize;
    let mut text_mismatch_fallback_chunks = 0_usize;
    let mut speaker_boundary_overlap_fallback_chunks = 0_usize;
    let mut publishable_chunks = 0_usize;
    for chunk in &qwen_response.segments {
        let end = chunk_offset
            .checked_add(chunk.words.len())
            .ok_or_else(|| "chunk bounds rejected".to_owned())?;
        let chunk_words = words
            .get(chunk_offset..end)
            .ok_or_else(|| "chunk bounds rejected".to_owned())?;
        if chunk_words.iter().any(|word| word.end_ms <= word.start_ms) {
            zero_duration_fallback_chunks += 1;
        } else if normalized_text(&chunk.text)
            != normalized_text(
                &chunk_words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        {
            text_mismatch_fallback_chunks += 1;
        } else if has_speaker_boundary_overlap(chunk_words) {
            speaker_boundary_overlap_fallback_chunks += 1;
        } else {
            publishable_chunks += 1;
        }
        chunk_offset = end;
    }
    if chunk_offset != words.len() {
        return Err("chunk bounds rejected".to_owned());
    }
    let zero_after = words
        .iter()
        .filter(|word| word.start_ms == word.end_ms)
        .count();
    let point_containment_candidates = words
        .iter()
        .filter(|word| word.start_ms == word.end_ms)
        .filter(|word| {
            let mut speaker = None;
            for interval in diarization_response.segments.iter().filter(|interval| {
                interval.confidence_milli >= 500
                    && interval.start_ms < word.start_ms
                    && word.start_ms < interval.end_ms
            }) {
                match speaker {
                    None => speaker = Some(interval.speaker_slot),
                    Some(existing) if existing == interval.speaker_slot => {}
                    Some(_) => return false,
                }
            }
            speaker.is_some()
        })
        .count();
    let assigned = stats
        .lexical_words
        .saturating_sub(stats.unknown_lexical_words);
    let mut unknown_runs_after = 0_usize;
    let mut word_bridge_candidate_words = 0_usize;
    let mut unbounded_unknown_words = 0_usize;
    let mut long_unknown_words = 0_usize;
    let mut different_speaker_unknown_words = 0_usize;
    let mut index = 0_usize;
    while index < words.len() {
        if words[index].speaker_id.is_some() {
            index += 1;
            continue;
        }
        let start = index;
        while index < words.len() && words[index].speaker_id.is_none() {
            index += 1;
        }
        unknown_runs_after += 1;
        let count = index - start;
        if start == 0 || index == words.len() {
            unbounded_unknown_words += count;
            continue;
        }
        let before = &words[start - 1];
        let after = &words[index];
        if before.speaker_id != after.speaker_id {
            different_speaker_unknown_words += count;
            continue;
        }
        if after.start_ms.saturating_sub(before.end_ms) > 1_000 {
            long_unknown_words += count;
            continue;
        }
        let speaker_slot = before
            .speaker_id
            .as_deref()
            .and_then(|speaker| speaker.strip_prefix("local_speaker_"))
            .and_then(|slot| slot.parse::<u32>().ok());
        let conflicting_interval = diarization_response.segments.iter().any(|interval| {
            interval.confidence_milli >= 500
                && interval.end_ms > before.end_ms
                && interval.start_ms < after.start_ms
                && Some(interval.speaker_slot) != speaker_slot
        });
        if !conflicting_interval {
            word_bridge_candidate_words += count;
        }
    }
    println!(
        "{}",
        json!({
            "lexicalWords": stats.lexical_words,
            "assignedWords": assigned,
            "unknownWords": stats.unknown_lexical_words,
            "coverageBasisPoints": assigned.saturating_mul(10_000) / stats.lexical_words.max(1),
            "overlapAssignedWords": stats.overlap_assigned_words,
            "pointAssignedWords": stats.point_assigned_words,
            "bridgeAssignedWords": stats.bridge_assigned_words,
            "interpolatedZeroDurationWords": stats.interpolated_zero_duration_words,
            "zeroDurationBefore": zero_before,
            "zeroDurationAfter": zero_after,
            "pointContainmentCandidates": point_containment_candidates,
            "zeroRuns": zero_runs,
            "unboundedZeroWords": unbounded_zero_words,
            "reversedAnchorZeroWords": reversed_anchor_zero_words,
            "zeroSpanWords": zero_span_words,
            "positiveSpanWords": positive_span_words,
            "punctuationAttached": stats.attached_punctuation_tokens,
            "unknownRunsAfter": unknown_runs_after,
            "wordBridgeCandidateWords": word_bridge_candidate_words,
            "unboundedUnknownWords": unbounded_unknown_words,
            "longUnknownWords": long_unknown_words,
            "differentSpeakerUnknownWords": different_speaker_unknown_words,
            "sourceChunks": qwen_response.segments.len(),
            "zeroDurationFallbackChunks": zero_duration_fallback_chunks,
            "textMismatchFallbackChunks": text_mismatch_fallback_chunks,
            "speakerBoundaryOverlapFallbackChunks": speaker_boundary_overlap_fallback_chunks,
            "publishableChunks": publishable_chunks,
        })
    );
    Ok(())
}

fn normalized_text(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric() || *character == '\'')
        .collect()
}

fn has_speaker_boundary_overlap(words: &[LocalAlignedWord]) -> bool {
    let mut prior_speaker: Option<&str> = None;
    let mut prior_turn_end = 0_u64;
    let mut has_prior_turn = false;
    for word in words {
        let speaker = word.speaker_id.as_deref();
        if has_prior_turn && prior_speaker == speaker {
            prior_turn_end = prior_turn_end.max(word.end_ms);
            continue;
        }
        if has_prior_turn && word.start_ms < prior_turn_end {
            return true;
        }
        prior_speaker = speaker;
        prior_turn_end = word.end_ms;
        has_prior_turn = true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start_ms: u64, end_ms: u64, speaker: Option<&str>) -> LocalAlignedWord {
        LocalAlignedWord {
            start_ms,
            end_ms,
            text: "fabricated".to_owned(),
            speaker_id: speaker.map(str::to_owned),
        }
    }

    #[test]
    fn aggregate_fallback_classifier_detects_only_cross_turn_overlap() {
        assert!(!has_speaker_boundary_overlap(&[
            word(0, 20, Some("local_speaker_01")),
            word(10, 30, Some("local_speaker_01")),
            word(30, 40, None),
        ]));
        assert!(has_speaker_boundary_overlap(&[
            word(0, 20, Some("local_speaker_01")),
            word(10, 30, Some("local_speaker_02")),
        ]));
        assert!(has_speaker_boundary_overlap(&[
            word(0, 20, None),
            word(10, 30, Some("local_speaker_01")),
        ]));
        assert_eq!(
            normalized_text("Hello, 世界!"),
            normalized_text("Hello 世界")
        );
    }
}
