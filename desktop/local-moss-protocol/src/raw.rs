//! Parse the model's explicit turn boundaries, never the upstream parser's
//! repaired timestamps. Missing metadata is a failed run, not audio-end fill.
use crate::{MossSegment, MAX_SEGMENTS, MAX_TEXT_BYTES};

struct Turn {
    start: i64,
    speaker: u32,
    text: String,
}

fn time(raw: &str, offset: usize) -> Option<(i64, usize)> {
    let rest = raw.get(offset..)?.strip_prefix('[')?;
    let length = rest.as_bytes().iter().take(32).position(|b| *b == b']')?;
    let body = &rest[..length];
    let (whole, fraction) = body.split_once('.').map_or((body, ""), |v| v);
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (body.contains('.') && fraction.is_empty())
    {
        return None;
    }
    let seconds = whole.parse::<i64>().ok()?;
    let mut millis = 0_i64;
    let mut digits = fraction.bytes();
    for _ in 0..3 {
        millis = millis * 10 + i64::from(digits.next().unwrap_or(b'0') - b'0');
    }
    if digits.next().is_some_and(|b| b >= b'5') {
        millis += 1;
    }
    Some((
        seconds.checked_mul(1000)?.checked_add(millis)?,
        offset + length + 2,
    ))
}

fn speaker(raw: &str, offset: usize) -> Option<(u32, usize)> {
    let rest = raw.get(offset..)?.strip_prefix("[S")?;
    let length = rest.as_bytes().iter().take(16).position(|b| *b == b']')?;
    let digits = &rest[..length];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = digits.parse::<u32>().ok()?;
    (id > 0).then_some((id, offset + length + 3))
}

fn close(
    open: &mut Option<Turn>,
    end: i64,
    output: &mut Vec<MossSegment>,
) -> Result<(), &'static str> {
    let turn = open.take().ok_or("raw_timing_incomplete")?;
    let text = turn.text.trim_matches([' ', '\t', '\r', '\n']);
    if text.is_empty() || text.len() > 64 * 1024 || output.len() >= MAX_SEGMENTS {
        return Err("raw_output_rejected");
    }
    output.push(MossSegment {
        start_ms: turn.start,
        end_ms: end,
        speaker_id: turn.speaker,
        text: text.into(),
    });
    Ok(())
}

pub fn parse_raw_segments(raw: &str) -> Result<Vec<MossSegment>, &'static str> {
    if raw.is_empty() || raw.len() > MAX_TEXT_BYTES || raw.contains('\0') {
        return Err("raw_output_rejected");
    }
    let raw = raw.trim_matches([' ', '\t', '\r', '\n']);
    let mut open = None;
    let mut output = Vec::new();
    let mut cursor = 0;
    while cursor < raw.len() {
        if let Some((boundary, next)) = time(raw, cursor) {
            let after_space = skip_space(raw, next);
            if let Some((id, after)) = speaker(raw, after_space) {
                // An explicit shared timestamp can close one turn and start
                // the next. It must not be synthesized from file duration.
                if open.is_some() {
                    close(&mut open, boundary, &mut output)?;
                }
                open = Some(Turn {
                    start: boundary,
                    speaker: id,
                    text: String::new(),
                });
                cursor = after;
                continue;
            }
            if let Some((start, after_time)) = time(raw, after_space) {
                if let Some((id, after)) = speaker(raw, skip_space(raw, after_time)) {
                    close(&mut open, boundary, &mut output)?;
                    open = Some(Turn {
                        start,
                        speaker: id,
                        text: String::new(),
                    });
                    cursor = after;
                    continue;
                }
            }
            if after_space == raw.len() {
                close(&mut open, boundary, &mut output)?;
                cursor = after_space;
                continue;
            }
            // A bracketed number followed by normal text is literal content.
            open.as_mut()
                .ok_or("raw_timing_incomplete")?
                .text
                .push_str(&raw[cursor..next]);
            cursor = next;
            continue;
        }
        if speaker(raw, cursor).is_some() {
            return Err("raw_timing_incomplete");
        }
        let character = raw[cursor..].chars().next().ok_or("raw_output_rejected")?;
        let turn = open.as_mut().ok_or("raw_timing_incomplete")?;
        if turn.text.len() + character.len_utf8() > 64 * 1024 {
            return Err("raw_output_rejected");
        }
        turn.text.push(character);
        cursor += character.len_utf8();
    }
    if open.is_some() || output.is_empty() {
        return Err("raw_timing_incomplete");
    }
    Ok(output)
}

fn skip_space(raw: &str, mut position: usize) -> usize {
    while raw
        .as_bytes()
        .get(position)
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
    {
        position += 1;
    }
    position
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_overlap_and_shared_boundaries_preserve_model_text() {
        let segments = parse_raw_segments(
            "[0.080][S01]hello 世界[1.2346][1.100][S02]yes [2026] version[2.000][S01]end[2.900] \n",
        )
        .unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!((segments[0].start_ms, segments[0].end_ms), (80, 1235));
        assert_eq!((segments[1].start_ms, segments[1].end_ms), (1100, 2000));
        assert_eq!(segments[0].text, "hello 世界");
        assert_eq!(segments[1].text, "yes [2026] version");
        assert_eq!(segments[2].text, "end");
    }

    #[test]
    fn missing_start_end_or_speaker_metadata_is_not_repaired() {
        for raw in [
            "plain text",
            "[0][S01]missing end",
            "[S01]missing start[4]",
            "[0][S01]one[S02]two[4]",
            "[0][1][S01]orphan[2]",
            "[0]no speaker[2]",
        ] {
            assert!(parse_raw_segments(raw).is_err());
        }
    }

    #[test]
    fn final_explicit_time_is_kept_even_with_trailing_whitespace() {
        let segments = parse_raw_segments("[2757.64][S01]last audible words[2758.92]\n").unwrap();
        assert_eq!(segments[0].end_ms, 2_758_920);
        // This API deliberately has no audio_duration argument: it cannot
        // stretch a missing final boundary to a5400-second file's end.
        assert_eq!(
            parse_raw_segments("[2757.64][S01]last audible words").unwrap_err(),
            "raw_timing_incomplete"
        );
    }

    #[test]
    fn raw_text_and_turn_sizes_are_bounded() {
        assert!(parse_raw_segments(&format!("[0][S01]{}[1]", "x".repeat(65_537))).is_err());
        assert!(parse_raw_segments("[0][S01]\0[1]").is_err());
        assert!(parse_raw_segments("[18446744073709551615][S01]x[1]").is_err());
        assert!(parse_raw_segments(&format!("[0][S01]{}", "[".repeat(65_537))).is_err());
    }

    #[test]
    fn line_separated_explicit_ends_are_not_replaced_by_the_next_start() {
        let segments = parse_raw_segments("[0] [S01]first[1]\n[1.5]\t[S02]second[2]\n").unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].end_ms, segments[1].start_ms), (1000, 1500));
        assert_eq!(segments[0].text, "first");
        assert_eq!(segments[1].text, "second");
    }

    #[test]
    #[ignore = "read-only audit of40 retained public raw strings; no inference/playback"]
    fn retained_public_raw_output_audit() -> Result<(), &'static str> {
        use serde_json::{json, Value};
        use std::{fs, path::Path};
        if std::env::var("ECHOWALL_MOSS_RAW_REPLAY_CONFIRM").as_deref()
            != Ok("public-raw-moss-replay-authorized")
        {
            return Err("confirmation_required");
        }
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or("repository_missing")?;
        let mut cases = 0;
        let mut accepted = 0;
        let mut changed = Vec::new();
        let mut rejected = Vec::new();
        for relative in [
            "local-eval/matrix/outputs/moss-transcribe-q8-mmo/results.jsonl",
            "local-eval/matrix/outputs/moss-transcribe-q8-english/results.jsonl",
        ] {
            let path = repository.join(relative);
            let metadata = fs::symlink_metadata(&path).map_err(|_| "fixture_missing")?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 32 * 1024 * 1024
            {
                return Err("fixture_rejected");
            }
            let text = fs::read_to_string(path).map_err(|_| "fixture_read_failed")?;
            for line in text.lines() {
                let row: Value = serde_json::from_str(line).map_err(|_| "fixture_invalid")?;
                if row["type"] == "batch_header" {
                    continue;
                }
                let id = Path::new(row["file"].as_str().ok_or("fixture_invalid")?)
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .ok_or("fixture_invalid")?;
                let raw = row["raw_text"].as_str().ok_or("raw_unavailable")?;
                cases += 1;
                match parse_raw_segments(raw) {
                    Err(code) => rejected.push(json!({"case_id":id,"code":code})),
                    Ok(parsed) => {
                        accepted += 1;
                        let previous = row["segments"].as_array().ok_or("fixture_invalid")?;
                        let mut time_changes = 0;
                        let mut text_changes = 0;
                        for (new, old) in parsed.iter().zip(previous) {
                            if Some(new.start_ms) != old["t0_ms"].as_i64()
                                || Some(new.end_ms) != old["t1_ms"].as_i64()
                            {
                                time_changes += 1;
                            }
                            if Some(new.text.as_str()) != old["text"].as_str() {
                                text_changes += 1;
                            }
                        }
                        if time_changes != 0 || text_changes != 0 || parsed.len() != previous.len()
                        {
                            changed.push(json!({"case_id":id,"time_changes":time_changes,"text_changes":text_changes,
                                "parsed_segments":parsed.len(),"previous_segments":previous.len()}));
                        }
                    }
                }
            }
        }
        if cases != 40 {
            return Err("incomplete_fixture");
        }
        println!(
            "{}",
            json!({"scope":"raw-grammar-audit-only","cases":cases,"accepted":accepted,
            "changed":changed,"rejected":rejected,"inference_run":false})
        );
        Ok(())
    }
}
