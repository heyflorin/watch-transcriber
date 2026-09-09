//! Aggregate-only comparison of a local worker response with an existing
//! EchoWall Markdown transcript. This tool intentionally never prints either
//! transcript or any differing span.

use std::env;
use std::fs;

use echowall_local_whisper_protocol::LocalWhisperResponse;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let reference_path = arguments.next().ok_or("reference path is required")?;
    let candidate_path = arguments.next().ok_or("candidate path is required")?;
    if arguments.next().is_some() {
        return Err("exactly two paths are required".into());
    }
    let reference = extract_reference(&fs::read_to_string(reference_path)?)?;
    let candidate: LocalWhisperResponse = serde_json::from_slice(&fs::read(candidate_path)?)?;
    let candidate_text = candidate
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    let reference_words = normalized_words(&reference);
    let candidate_words = normalized_words(&candidate_text);
    let word_distance = levenshtein(&reference_words, &candidate_words);
    let reference_characters = normalized_characters(&reference);
    let candidate_characters = normalized_characters(&candidate_text);
    let character_distance = levenshtein(&reference_characters, &candidate_characters);
    let adjacent_duplicate_segments = candidate
        .segments
        .windows(2)
        .filter(|pair| normalize_text(&pair[0].text) == normalize_text(&pair[1].text))
        .count();

    println!(
        "{{\"reference_words\":{},\"candidate_words\":{},\"word_distance\":{},\"word_error_rate\":{:.4},\"reference_characters\":{},\"candidate_characters\":{},\"character_distance\":{},\"character_error_rate\":{:.4},\"segments\":{},\"adjacent_duplicate_segments\":{}}}",
        reference_words.len(),
        candidate_words.len(),
        word_distance,
        ratio(word_distance, reference_words.len()),
        reference_characters.len(),
        candidate_characters.len(),
        character_distance,
        ratio(character_distance, reference_characters.len()),
        candidate.segments.len(),
        adjacent_duplicate_segments,
    );
    Ok(())
}

fn extract_reference(markdown: &str) -> Result<String, &'static str> {
    let (_, transcript) = markdown
        .split_once("## Transcript")
        .ok_or("reference transcript section is missing")?;
    let mut output = String::new();
    for line in transcript.lines() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }
        let (_, after_timestamp) = line
            .split_once(']')
            .ok_or("reference timestamp is invalid")?;
        let after_timestamp = after_timestamp.trim();
        let content = after_timestamp
            .split_once(": ")
            .map(|(_, content)| content)
            .unwrap_or(after_timestamp);
        if !content.is_empty() {
            if !output.is_empty() {
                output.push(' ');
            }
            output.push_str(content);
        }
    }
    if output.is_empty() {
        Err("reference transcript is empty")
    } else {
        Ok(output)
    }
}

fn normalize_text(value: &str) -> String {
    normalized_words(value).join(" ")
}

fn normalized_words(value: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(value.len());
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '\'' {
            normalized.push(character);
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().map(str::to_owned).collect()
}

fn normalized_characters(value: &str) -> Vec<char> {
    normalized_words(value).join("").chars().collect()
}

fn ratio(distance: usize, reference_length: usize) -> f64 {
    if reference_length == 0 {
        0.0
    } else {
        distance as f64 / reference_length as f64
    }
}

fn levenshtein<T: Eq>(left: &[T], right: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_value) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_value) in right.iter().enumerate() {
            current[right_index + 1] = if left_value == right_value {
                previous[right_index]
            } else {
                1 + previous[right_index]
                    .min(current[right_index])
                    .min(previous[right_index + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_helpers_do_not_need_transcript_output() {
        let reference = extract_reference(
            "# Fixture\n\n## Transcript\n\n```\n[00:00:00] Speaker 1: Hello, world.\n```\n",
        )
        .unwrap();
        assert_eq!(normalized_words(&reference), ["hello", "world"]);
        assert_eq!(levenshtein(&["a", "b"], &["a", "c", "b"]), 1);
    }

    #[test]
    fn missing_transcript_is_rejected() {
        assert!(extract_reference("# Fixture").is_err());
    }
}
