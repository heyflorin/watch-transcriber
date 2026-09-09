//! Aggregate-only, offline quality evaluation for `EchoWall` full-local models.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use rapidfuzz::distance::levenshtein;
use serde::{Deserialize, Serialize};

mod coverage;
pub use coverage::{evaluate_speaker_coverage, SpeakerCoverageReport};

const MANIFEST_VERSION: u32 = 1;
const TRANSCRIPT_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_TRANSCRIPT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TRANSCRIPT_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CASES: usize = 256;
const MAX_SEGMENTS: usize = 50_000;
const MAX_DURATION_MS: u64 = 5 * 60 * 60 * 1_000;
const MAX_SPEAKERS: usize = 16;
const MAX_TERMS: usize = 256;
const FRAME_MS: u64 = 10;
const UNKNOWN_SPEAKER: &str = "local_unknown";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum Stratum {
    English,
    Mandarin,
    Mixed,
    Overlap,
    LongForm,
}

impl Stratum {
    const ALL: [Self; 5] = [
        Self::English,
        Self::Mandarin,
        Self::Mixed,
        Self::Overlap,
        Self::LongForm,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::English => "english",
            Self::Mandarin => "mandarin",
            Self::Mixed => "mixed",
            Self::Overlap => "overlap",
            Self::LongForm => "long_form",
        }
    }

    const fn minimums(self) -> (usize, u64) {
        match self {
            Self::English | Self::Mandarin | Self::Mixed => (10, 2 * 60 * 60 * 1_000),
            Self::Overlap => (10, 30 * 60 * 1_000),
            Self::LongForm => (3, 6 * 60 * 60 * 1_000),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MatrixManifest {
    schema_version: u32,
    cases: Vec<MatrixCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MatrixCase {
    case_id: String,
    stratum: Stratum,
    duration_ms: u64,
    ground_truth: String,
    miaoji: String,
    local: String,
    named_terms: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalTranscript {
    schema_version: u32,
    segments: Vec<CanonicalSegment>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalSegment {
    start_ms: u64,
    end_ms: u64,
    speaker: String,
    text: String,
}

#[derive(Clone, Copy, Debug, Default)]
struct RawMetrics {
    word_errors: u64,
    reference_words: u64,
    character_errors: u64,
    reference_characters: u64,
    diarization_errors: u64,
    reference_speaker_frames: u64,
    jer_sum: f64,
    reference_speakers: u64,
    exact_speaker_count: u64,
    term_hits: u64,
    term_total: u64,
    covered_speech_frames: u64,
    reference_speech_frames: u64,
    adjacent_duplicates: u64,
    adjacent_pairs: u64,
}

impl RawMetrics {
    fn add(&mut self, other: Self) {
        self.word_errors = self.word_errors.saturating_add(other.word_errors);
        self.reference_words = self.reference_words.saturating_add(other.reference_words);
        self.character_errors = self.character_errors.saturating_add(other.character_errors);
        self.reference_characters = self
            .reference_characters
            .saturating_add(other.reference_characters);
        self.diarization_errors = self
            .diarization_errors
            .saturating_add(other.diarization_errors);
        self.reference_speaker_frames = self
            .reference_speaker_frames
            .saturating_add(other.reference_speaker_frames);
        self.jer_sum += other.jer_sum;
        self.reference_speakers = self
            .reference_speakers
            .saturating_add(other.reference_speakers);
        self.exact_speaker_count = self
            .exact_speaker_count
            .saturating_add(other.exact_speaker_count);
        self.term_hits = self.term_hits.saturating_add(other.term_hits);
        self.term_total = self.term_total.saturating_add(other.term_total);
        self.covered_speech_frames = self
            .covered_speech_frames
            .saturating_add(other.covered_speech_frames);
        self.reference_speech_frames = self
            .reference_speech_frames
            .saturating_add(other.reference_speech_frames);
        self.adjacent_duplicates = self
            .adjacent_duplicates
            .saturating_add(other.adjacent_duplicates);
        self.adjacent_pairs = self.adjacent_pairs.saturating_add(other.adjacent_pairs);
    }
}

#[derive(Clone, Debug, Default)]
struct StratumTotals {
    cases: usize,
    duration_ms: u64,
    shortest_case_ms: u64,
    miaoji: RawMetrics,
    local: RawMetrics,
}

/// Versioned acceptance rules. Metric computation and input schemas are shared.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AcceptancePolicy {
    /// Original report schema and absolute 95% exact-count release gate.
    #[default]
    LegacyV1,
    /// Per-stratum Miaoji count comparison, with 95% reported as stretch only.
    MiaojiRelativeV2,
}

#[derive(Debug, Serialize)]
pub struct EvaluationReport {
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_policy: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'static str>,
    status: &'static str,
    strata: BTreeMap<&'static str, StratumReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stretch: Option<StretchReport>,
}

#[derive(Debug, Serialize)]
struct StretchReport {
    target: &'static str,
    minimum_accuracy: f64,
    status: &'static str,
    // null means insufficient evidence; never a vacuous success.
    strata: BTreeMap<&'static str, Option<bool>>,
}

#[derive(Debug, Serialize)]
struct StratumReport {
    sufficient: bool,
    passed: bool,
    failure_codes: Vec<&'static str>,
    cases: usize,
    duration_ms: u64,
    miaoji: MetricReport,
    local: MetricReport,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct MetricReport {
    wer: f64,
    cer: f64,
    der: f64,
    jer: f64,
    exact_speaker_count: f64,
    named_term_recall: f64,
    timestamp_coverage: f64,
    adjacent_duplicate_rate: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvalError {
    code: &'static str,
}

impl EvalError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local quality evaluation failed")
    }
}

impl std::error::Error for EvalError {}

/// Evaluate one closed quality matrix without exposing its content in output.
///
/// # Errors
///
/// Returns a closed [`EvalError`] code when the manifest, a referenced file,
/// canonical transcript invariants, or ground truth is invalid.
pub fn evaluate_manifest(path: impl AsRef<OsStr>) -> Result<EvaluationReport, EvalError> {
    evaluate_manifest_with_policy(path, AcceptancePolicy::LegacyV1)
}

/// Evaluate the same metrics under an explicitly selected acceptance policy.
///
/// # Errors
/// Returns the same closed input/ground-truth errors as [`evaluate_manifest`].
pub fn evaluate_manifest_with_policy(
    path: impl AsRef<OsStr>,
    policy: AcceptancePolicy,
) -> Result<EvaluationReport, EvalError> {
    let (manifest, root) = load_manifest(path)?;
    let mut totals = BTreeMap::<Stratum, StratumTotals>::new();
    for case in &manifest.cases {
        let truth = read_transcript(&root, &case.ground_truth, case.duration_ms)?;
        let miaoji = read_transcript(&root, &case.miaoji, case.duration_ms)?;
        let local = read_transcript(&root, &case.local, case.duration_ms)?;
        let miaoji_metrics = score_candidate(&truth, &miaoji, &case.named_terms, case.duration_ms)?;
        let local_metrics = score_candidate(&truth, &local, &case.named_terms, case.duration_ms)?;
        let entry = totals.entry(case.stratum).or_default();
        entry.cases = entry.cases.saturating_add(1);
        entry.duration_ms = entry.duration_ms.saturating_add(case.duration_ms);
        entry.shortest_case_ms = if entry.shortest_case_ms == 0 {
            case.duration_ms
        } else {
            entry.shortest_case_ms.min(case.duration_ms)
        };
        entry.miaoji.add(miaoji_metrics);
        entry.local.add(local_metrics);
    }
    Ok(build_report(&totals, policy))
}

fn load_manifest(path: impl AsRef<OsStr>) -> Result<(MatrixManifest, PathBuf), EvalError> {
    let supplied = PathBuf::from(path.as_ref());
    let metadata =
        fs::symlink_metadata(&supplied).map_err(|_| EvalError::new("manifest_unavailable"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err(EvalError::new("manifest_rejected"));
    }
    let manifest_path =
        fs::canonicalize(&supplied).map_err(|_| EvalError::new("manifest_unavailable"))?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| EvalError::new("manifest_rejected"))?;
    let manifest: MatrixManifest = serde_json::from_slice(
        &fs::read(&manifest_path).map_err(|_| EvalError::new("manifest_unavailable"))?,
    )
    .map_err(|_| EvalError::new("manifest_invalid"))?;
    validate_manifest(&manifest)?;
    Ok((manifest, root.to_path_buf()))
}

fn validate_manifest(manifest: &MatrixManifest) -> Result<(), EvalError> {
    if manifest.schema_version != MANIFEST_VERSION
        || manifest.cases.is_empty()
        || manifest.cases.len() > MAX_CASES
    {
        return Err(EvalError::new("manifest_invalid"));
    }
    let mut ids = HashSet::new();
    for case in &manifest.cases {
        if !valid_identifier(&case.case_id)
            || !ids.insert(&case.case_id)
            || case.duration_ms == 0
            || case.duration_ms > MAX_DURATION_MS
            || case.named_terms.len() > MAX_TERMS
            || !valid_relative_json(&case.ground_truth)
            || !valid_relative_json(&case.miaoji)
            || !valid_relative_json(&case.local)
        {
            return Err(EvalError::new("manifest_invalid"));
        }
        let mut terms = HashSet::new();
        for term in &case.named_terms {
            let normalized = normalized_characters(term);
            if term.len() > 200 || normalized.is_empty() || !terms.insert(normalized) {
                return Err(EvalError::new("manifest_invalid"));
            }
        }
    }
    Ok(())
}

fn read_transcript(
    root: &Path,
    relative: &str,
    duration_ms: u64,
) -> Result<CanonicalTranscript, EvalError> {
    let path = resolve_regular_file(root, relative)?;
    let metadata = fs::metadata(&path).map_err(|_| EvalError::new("transcript_unavailable"))?;
    if metadata.len() == 0 || metadata.len() > MAX_TRANSCRIPT_BYTES {
        return Err(EvalError::new("transcript_rejected"));
    }
    let transcript: CanonicalTranscript = serde_json::from_slice(
        &fs::read(path).map_err(|_| EvalError::new("transcript_unavailable"))?,
    )
    .map_err(|_| EvalError::new("transcript_invalid"))?;
    validate_transcript(&transcript, duration_ms)?;
    Ok(transcript)
}

fn validate_transcript(
    transcript: &CanonicalTranscript,
    duration_ms: u64,
) -> Result<(), EvalError> {
    if transcript.schema_version != TRANSCRIPT_VERSION
        || transcript.segments.is_empty()
        || transcript.segments.len() > MAX_SEGMENTS
    {
        return Err(EvalError::new("transcript_invalid"));
    }
    let mut speakers = BTreeSet::new();
    let mut last_start = 0;
    let mut last_end_by_speaker = BTreeMap::<&str, u64>::new();
    let mut text_bytes = 0_usize;
    for (index, segment) in transcript.segments.iter().enumerate() {
        if segment.end_ms <= segment.start_ms
            || segment.end_ms > duration_ms
            || index > 0 && segment.start_ms < last_start
            || !valid_speaker(&segment.speaker)
            || segment.text.trim().is_empty()
            || segment.text.len() > 65_536
            || segment.text.contains('\0')
            || last_end_by_speaker
                .get(segment.speaker.as_str())
                .is_some_and(|last_end| segment.start_ms < *last_end)
        {
            return Err(EvalError::new("transcript_invalid"));
        }
        last_start = segment.start_ms;
        last_end_by_speaker.insert(&segment.speaker, segment.end_ms);
        speakers.insert(&segment.speaker);
        text_bytes = text_bytes
            .checked_add(segment.text.len())
            .ok_or_else(|| EvalError::new("transcript_rejected"))?;
        if text_bytes > MAX_TRANSCRIPT_TEXT_BYTES || speakers.len() > MAX_SPEAKERS {
            return Err(EvalError::new("transcript_rejected"));
        }
    }
    Ok(())
}

fn score_candidate(
    truth: &CanonicalTranscript,
    candidate: &CanonicalTranscript,
    terms: &[String],
    duration_ms: u64,
) -> Result<RawMetrics, EvalError> {
    let truth_text = joined_text(truth);
    let candidate_text = joined_text(candidate);
    let truth_words = word_tokens(&truth_text);
    let candidate_words = word_tokens(&candidate_text);
    let truth_characters = normalized_characters(&truth_text);
    let candidate_characters = normalized_characters(&candidate_text);
    if truth_words.is_empty() || truth_characters.is_empty() {
        return Err(EvalError::new("ground_truth_invalid"));
    }
    let diarization = diarization_metrics(truth, candidate, duration_ms)?;
    let mut term_hits = 0_u64;
    for term in terms {
        let normalized = normalized_characters(term);
        if !truth_characters.contains(&normalized) {
            return Err(EvalError::new("ground_truth_invalid"));
        }
        term_hits = term_hits.saturating_add(u64::from(candidate_characters.contains(&normalized)));
    }
    let normalized_segments: Vec<_> = candidate
        .segments
        .iter()
        .map(|segment| normalized_characters(&segment.text))
        .collect();
    let truth_normalized_segments: Vec<_> = truth
        .segments
        .iter()
        .map(|segment| normalized_characters(&segment.text))
        .collect();
    let adjacent_duplicates = adjacent_duplicate_count(&normalized_segments)
        .saturating_sub(adjacent_duplicate_count(&truth_normalized_segments));
    Ok(RawMetrics {
        word_errors: word_distance(&truth_words, &candidate_words) as u64,
        reference_words: truth_words.len() as u64,
        character_errors: levenshtein::distance(
            truth_characters.chars(),
            candidate_characters.chars(),
        ) as u64,
        reference_characters: truth_characters.chars().count() as u64,
        diarization_errors: diarization.errors,
        reference_speaker_frames: diarization.reference_speaker_frames,
        jer_sum: diarization.jer_sum,
        reference_speakers: diarization.reference_speakers,
        exact_speaker_count: u64::from(diarization.exact_speaker_count),
        term_hits,
        term_total: terms.len() as u64,
        covered_speech_frames: diarization.covered_speech_frames,
        reference_speech_frames: diarization.reference_speech_frames,
        adjacent_duplicates,
        adjacent_pairs: normalized_segments.len().saturating_sub(1) as u64,
    })
}

fn adjacent_duplicate_count(segments: &[String]) -> u64 {
    segments
        .windows(2)
        .filter(|pair| !pair[0].is_empty() && pair[0] == pair[1])
        .count() as u64
}

#[derive(Clone, Copy, Debug)]
struct DiarizationRaw {
    errors: u64,
    reference_speaker_frames: u64,
    jer_sum: f64,
    reference_speakers: u64,
    exact_speaker_count: bool,
    covered_speech_frames: u64,
    reference_speech_frames: u64,
}

fn diarization_metrics(
    truth: &CanonicalTranscript,
    candidate: &CanonicalTranscript,
    duration_ms: u64,
) -> Result<DiarizationRaw, EvalError> {
    if truth
        .segments
        .iter()
        .any(|segment| segment.speaker == UNKNOWN_SPEAKER)
    {
        return Err(EvalError::new("ground_truth_invalid"));
    }
    let truth_speakers = speaker_index(truth);
    let candidate_speakers = speaker_index(candidate);
    let truth_frames = timeline(truth, &truth_speakers, duration_ms)?;
    let candidate_frames = timeline(candidate, &candidate_speakers, duration_ms)?;
    let candidate_activity = activity_timeline(candidate, duration_ms)?;
    let mapping = optimal_mapping(
        &truth_frames,
        &candidate_frames,
        truth_speakers.len(),
        candidate_speakers.len(),
    );
    let mut errors = 0_u64;
    let mut reference_speaker_frames = 0_u64;
    let mut covered_speech_frames = 0_u64;
    let mut reference_speech_frames = 0_u64;
    for ((&reference, &hypothesis), &candidate_active) in truth_frames
        .iter()
        .zip(&candidate_frames)
        .zip(&candidate_activity)
    {
        let mapped = mapped_mask(hypothesis, &mapping);
        let reference_count = u64::from(reference.count_ones());
        let hypothesis_count = u64::from(hypothesis.count_ones());
        let correct = u64::from((reference & mapped).count_ones());
        errors = errors
            .saturating_add(reference_count.saturating_sub(hypothesis_count))
            .saturating_add(hypothesis_count.saturating_sub(reference_count))
            .saturating_add(
                reference_count
                    .min(hypothesis_count)
                    .saturating_sub(correct),
            );
        reference_speaker_frames = reference_speaker_frames.saturating_add(reference_count);
        if reference != 0 {
            reference_speech_frames = reference_speech_frames.saturating_add(1);
            covered_speech_frames =
                covered_speech_frames.saturating_add(u64::from(candidate_active));
        }
    }
    if reference_speaker_frames == 0 || reference_speech_frames == 0 {
        return Err(EvalError::new("ground_truth_invalid"));
    }

    let mut jer_sum = 0.0;
    for reference_speaker in 0..truth_speakers.len() {
        let hypothesis_speaker = mapping
            .iter()
            .position(|mapped| *mapped == Some(reference_speaker));
        let mut intersection = 0_u64;
        let mut union = 0_u64;
        for (&reference, &hypothesis) in truth_frames.iter().zip(&candidate_frames) {
            let reference_active = reference & (1_u16 << reference_speaker) != 0;
            let hypothesis_active =
                hypothesis_speaker.is_some_and(|speaker| hypothesis & (1_u16 << speaker) != 0);
            intersection =
                intersection.saturating_add(u64::from(reference_active && hypothesis_active));
            union = union.saturating_add(u64::from(reference_active || hypothesis_active));
        }
        if union == 0 {
            return Err(EvalError::new("ground_truth_invalid"));
        }
        jer_sum += 1.0 - ratio(intersection, union);
    }
    Ok(DiarizationRaw {
        errors,
        reference_speaker_frames,
        jer_sum,
        reference_speakers: truth_speakers.len() as u64,
        exact_speaker_count: truth_speakers.len() == candidate_speakers.len(),
        covered_speech_frames,
        reference_speech_frames,
    })
}

fn speaker_index(transcript: &CanonicalTranscript) -> BTreeMap<&str, usize> {
    transcript
        .segments
        .iter()
        .map(|segment| segment.speaker.as_str())
        .filter(|speaker| *speaker != UNKNOWN_SPEAKER)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .enumerate()
        .map(|(index, speaker)| (speaker, index))
        .collect()
}

fn timeline(
    transcript: &CanonicalTranscript,
    speakers: &BTreeMap<&str, usize>,
    duration_ms: u64,
) -> Result<Vec<u16>, EvalError> {
    let frame_count = usize::try_from(duration_ms.div_ceil(FRAME_MS))
        .map_err(|_| EvalError::new("duration_rejected"))?;
    let mut frames = vec![0_u16; frame_count];
    for segment in &transcript.segments {
        let Some(&speaker) = speakers.get(segment.speaker.as_str()) else {
            if segment.speaker == UNKNOWN_SPEAKER {
                continue;
            }
            return Err(EvalError::new("transcript_invalid"));
        };
        let frame_count_u64 =
            u64::try_from(frame_count).map_err(|_| EvalError::new("duration_rejected"))?;
        let start = usize::try_from(
            ((segment.start_ms.saturating_add(FRAME_MS / 2 - 1)) / FRAME_MS).min(frame_count_u64),
        )
        .map_err(|_| EvalError::new("duration_rejected"))?;
        let end = usize::try_from(
            ((segment.end_ms.saturating_add(FRAME_MS / 2 - 1)) / FRAME_MS).min(frame_count_u64),
        )
        .map_err(|_| EvalError::new("duration_rejected"))?;
        for frame in &mut frames[start..end] {
            *frame |= 1_u16 << speaker;
        }
    }
    Ok(frames)
}

fn activity_timeline(
    transcript: &CanonicalTranscript,
    duration_ms: u64,
) -> Result<Vec<bool>, EvalError> {
    let frame_count = usize::try_from(duration_ms.div_ceil(FRAME_MS))
        .map_err(|_| EvalError::new("duration_rejected"))?;
    let mut frames = vec![false; frame_count];
    for segment in &transcript.segments {
        let frame_count_u64 =
            u64::try_from(frame_count).map_err(|_| EvalError::new("duration_rejected"))?;
        let start = usize::try_from(
            ((segment.start_ms.saturating_add(FRAME_MS / 2 - 1)) / FRAME_MS).min(frame_count_u64),
        )
        .map_err(|_| EvalError::new("duration_rejected"))?;
        let end = usize::try_from(
            ((segment.end_ms.saturating_add(FRAME_MS / 2 - 1)) / FRAME_MS).min(frame_count_u64),
        )
        .map_err(|_| EvalError::new("duration_rejected"))?;
        frames[start..end].fill(true);
    }
    Ok(frames)
}

fn optimal_mapping(
    truth_frames: &[u16],
    candidate_frames: &[u16],
    truth_speakers: usize,
    candidate_speakers: usize,
) -> Vec<Option<usize>> {
    let size = truth_speakers.max(candidate_speakers);
    let mut weights = vec![vec![0_i64; size]; size];
    for (&truth, &candidate) in truth_frames.iter().zip(candidate_frames) {
        for (hypothesis, row) in weights.iter_mut().enumerate().take(candidate_speakers) {
            if candidate & (1_u16 << hypothesis) == 0 {
                continue;
            }
            for (reference, value) in row.iter_mut().enumerate().take(truth_speakers) {
                if truth & (1_u16 << reference) != 0 {
                    *value += 1;
                }
            }
        }
    }
    let maximum = weights.iter().flatten().copied().max().unwrap_or(0);
    let costs: Vec<Vec<_>> = weights
        .iter()
        .map(|row| row.iter().map(|weight| maximum - weight).collect())
        .collect();
    let assignment = hungarian_min_assignment(&costs);
    assignment
        .into_iter()
        .take(candidate_speakers)
        .map(|column| (column < truth_speakers).then_some(column))
        .collect()
}

fn hungarian_min_assignment(costs: &[Vec<i64>]) -> Vec<usize> {
    let size = costs.len();
    if size == 0 {
        return Vec::new();
    }
    let mut row_potential = vec![0_i64; size + 1];
    let mut column_potential = vec![0_i64; size + 1];
    let mut matching = vec![0_usize; size + 1];
    let mut way = vec![0_usize; size + 1];
    for row in 1..=size {
        matching[0] = row;
        let mut column = 0;
        let mut minimum = vec![i64::MAX; size + 1];
        let mut used = vec![false; size + 1];
        loop {
            used[column] = true;
            let matched_row = matching[column];
            let mut delta = i64::MAX;
            let mut next_column = 0;
            for candidate_column in 1..=size {
                if used[candidate_column] {
                    continue;
                }
                let current = costs[matched_row - 1][candidate_column - 1]
                    - row_potential[matched_row]
                    - column_potential[candidate_column];
                if current < minimum[candidate_column] {
                    minimum[candidate_column] = current;
                    way[candidate_column] = column;
                }
                if minimum[candidate_column] < delta {
                    delta = minimum[candidate_column];
                    next_column = candidate_column;
                }
            }
            for candidate_column in 0..=size {
                if used[candidate_column] {
                    row_potential[matching[candidate_column]] += delta;
                    column_potential[candidate_column] -= delta;
                } else {
                    minimum[candidate_column] -= delta;
                }
            }
            column = next_column;
            if matching[column] == 0 {
                break;
            }
        }
        loop {
            let previous = way[column];
            matching[column] = matching[previous];
            column = previous;
            if column == 0 {
                break;
            }
        }
    }
    let mut assignment = vec![0_usize; size];
    for column in 1..=size {
        assignment[matching[column] - 1] = column - 1;
    }
    assignment
}

fn mapped_mask(hypothesis: u16, mapping: &[Option<usize>]) -> u16 {
    let mut mapped = 0_u16;
    for (speaker, reference) in mapping.iter().enumerate() {
        if hypothesis & (1_u16 << speaker) != 0 {
            if let Some(reference) = reference {
                mapped |= 1_u16 << reference;
            }
        }
    }
    mapped
}

fn build_report(
    totals: &BTreeMap<Stratum, StratumTotals>,
    policy: AcceptancePolicy,
) -> EvaluationReport {
    let mut strata = BTreeMap::new();
    let mut stretch_strata = BTreeMap::new();
    let mut any_insufficient = false;
    let mut all_passed = true;
    let mut all_stretch_passed = true;
    for stratum in Stratum::ALL {
        let totals = totals.get(&stratum).cloned().unwrap_or_default();
        let (minimum_cases, minimum_duration) = stratum.minimums();
        let long_enough =
            stratum != Stratum::LongForm || totals.shortest_case_ms >= 60 * 60 * 1_000;
        let sufficient = totals.cases >= minimum_cases
            && totals.duration_ms >= minimum_duration
            && totals.local.term_total >= 10
            && totals.miaoji.term_total >= 10
            && long_enough;
        let miaoji = metric_report(totals.miaoji, totals.cases);
        let local = metric_report(totals.local, totals.cases);
        let mut failures = Vec::new();
        if sufficient {
            if !noninferior(local.wer, miaoji.wer, 0.02) {
                failures.push("wer_noninferiority");
            }
            if !noninferior(local.cer, miaoji.cer, 0.02) {
                failures.push("cer_noninferiority");
            }
            if !noninferior(local.der, miaoji.der, 0.03) {
                failures.push("der_noninferiority");
            }
            if !noninferior(local.jer, miaoji.jer, 0.03) {
                failures.push("jer_noninferiority");
            }
            match policy {
                AcceptancePolicy::LegacyV1 => {
                    if local.exact_speaker_count + f64::EPSILON < 0.95 {
                        failures.push("speaker_count_accuracy");
                    }
                }
                AcceptancePolicy::MiaojiRelativeV2 => {
                    if totals.local.exact_speaker_count < totals.miaoji.exact_speaker_count {
                        failures.push("speaker_count_noninferiority");
                    }
                }
            }
            if local.named_term_recall + 0.05 + f64::EPSILON < miaoji.named_term_recall {
                failures.push("named_term_recall");
            }
            if local.timestamp_coverage + 0.02 + f64::EPSILON < miaoji.timestamp_coverage {
                failures.push("timestamp_coverage");
            }
            if local.adjacent_duplicate_rate > miaoji.adjacent_duplicate_rate + 0.01 + f64::EPSILON
            {
                failures.push("adjacent_duplicate_rate");
            }
        } else {
            failures.push("insufficient_corpus");
            any_insufficient = true;
        }
        let passed = sufficient && failures.is_empty();
        all_passed &= passed;
        let stretch_passed = sufficient.then_some(local.exact_speaker_count + f64::EPSILON >= 0.95);
        all_stretch_passed &= stretch_passed == Some(true);
        stretch_strata.insert(stratum.name(), stretch_passed);
        strata.insert(
            stratum.name(),
            StratumReport {
                sufficient,
                passed,
                failure_codes: failures,
                cases: totals.cases,
                duration_ms: totals.duration_ms,
                miaoji,
                local,
            },
        );
    }
    let relative = policy == AcceptancePolicy::MiaojiRelativeV2;
    EvaluationReport {
        schema_version: if relative { 2 } else { 1 },
        acceptance_policy: relative.then_some("miaoji-relative-v2"),
        scope: relative.then_some("transcript_metrics_only"),
        status: report_status(any_insufficient, all_passed),
        strata,
        stretch: relative.then_some(StretchReport {
            target: "exact_speaker_count_per_stratum",
            minimum_accuracy: 0.95,
            status: report_status(any_insufficient, all_stretch_passed),
            strata: stretch_strata,
        }),
    }
}

const fn report_status(any_insufficient: bool, all_passed: bool) -> &'static str {
    if any_insufficient {
        "insufficient_corpus"
    } else if all_passed {
        "pass"
    } else {
        "fail"
    }
}

#[allow(clippy::cast_precision_loss)]
fn metric_report(raw: RawMetrics, cases: usize) -> MetricReport {
    MetricReport {
        wer: ratio(raw.word_errors, raw.reference_words),
        cer: ratio(raw.character_errors, raw.reference_characters),
        der: ratio(raw.diarization_errors, raw.reference_speaker_frames),
        jer: if raw.reference_speakers == 0 {
            0.0
        } else {
            raw.jer_sum / raw.reference_speakers as f64
        },
        exact_speaker_count: ratio(
            raw.exact_speaker_count,
            u64::try_from(cases).unwrap_or(u64::MAX),
        ),
        named_term_recall: ratio(raw.term_hits, raw.term_total),
        timestamp_coverage: ratio(raw.covered_speech_frames, raw.reference_speech_frames),
        adjacent_duplicate_rate: ratio(raw.adjacent_duplicates, raw.adjacent_pairs),
    }
}

fn noninferior(local: f64, miaoji: f64, absolute_margin: f64) -> bool {
    local <= miaoji + absolute_margin + f64::EPSILON && local <= miaoji.mul_add(1.2, f64::EPSILON)
}

fn resolve_regular_file(root: &Path, relative: &str) -> Result<PathBuf, EvalError> {
    if !valid_relative_json(relative) {
        return Err(EvalError::new("path_rejected"));
    }
    let mut candidate = root.to_path_buf();
    let components: Vec<_> = Path::new(relative).components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(EvalError::new("path_rejected"));
        };
        candidate.push(name);
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|_| EvalError::new("transcript_unavailable"))?;
        if metadata.file_type().is_symlink()
            || index + 1 < components.len() && !metadata.is_dir()
            || index + 1 == components.len() && !metadata.is_file()
        {
            return Err(EvalError::new("path_rejected"));
        }
    }
    let canonical = fs::canonicalize(&candidate).map_err(|_| EvalError::new("path_rejected"))?;
    if !canonical.starts_with(root) {
        return Err(EvalError::new("path_rejected"));
    }
    Ok(canonical)
}

fn valid_relative_json(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2_048
        && !value.contains('\\')
        && Path::new(value).extension() == Some(OsStr::new("json"))
        && !Path::new(value).is_absolute()
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn valid_speaker(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn joined_text(transcript: &CanonicalTranscript) -> String {
    transcript
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn word_tokens(value: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = String::new();
    for character in value.chars().flat_map(char::to_lowercase) {
        if is_han(character) {
            if !current.is_empty() {
                output.push(std::mem::take(&mut current));
            }
            output.push(character.to_string());
        } else if character.is_alphanumeric() {
            current.push(character);
        } else if !current.is_empty() {
            output.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        output.push(current);
    }
    output
}

fn normalized_characters(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric() || is_han(*character))
        .collect()
}

const fn is_han(character: char) -> bool {
    matches!(character as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

fn word_distance(left: &[String], right: &[String]) -> usize {
    let mut vocabulary = BTreeMap::<&str, u64>::new();
    for token in left.iter().chain(right) {
        if !vocabulary.contains_key(token.as_str()) {
            vocabulary.insert(token, vocabulary.len() as u64);
        }
    }
    let left = left.iter().map(|token| vocabulary[token.as_str()]);
    let right = right.iter().map(|token| vocabulary[token.as_str()]);
    levenshtein::distance(left, right)
}

#[allow(clippy::cast_precision_loss)]
fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    pub fn segment(start_ms: u64, end_ms: u64, speaker: &str, text: &str) -> CanonicalSegment {
        CanonicalSegment {
            start_ms,
            end_ms,
            speaker: speaker.to_owned(),
            text: text.to_owned(),
        }
    }

    pub fn transcript(segments: Vec<CanonicalSegment>) -> CanonicalTranscript {
        CanonicalTranscript {
            schema_version: TRANSCRIPT_VERSION,
            segments,
        }
    }

    #[test]
    fn perfect_permuted_speakers_have_zero_error() {
        let truth = transcript(vec![
            segment(0, 1_000, "A", "hello alpha"),
            segment(500, 1_500, "B", "世界 beta"),
        ]);
        let candidate = transcript(vec![
            segment(0, 1_000, "speaker_2", "hello alpha"),
            segment(500, 1_500, "speaker_1", "世界 beta"),
        ]);
        validate_transcript(&truth, 2_000).unwrap();
        validate_transcript(&candidate, 2_000).unwrap();
        let metrics = score_candidate(
            &truth,
            &candidate,
            &["alpha".to_owned(), "世界".to_owned()],
            2_000,
        )
        .unwrap();
        assert_eq!(metrics.word_errors, 0);
        assert_eq!(metrics.character_errors, 0);
        assert_eq!(metrics.diarization_errors, 0);
        assert!(metrics.jer_sum.abs() < f64::EPSILON);
        assert_eq!(
            metrics.covered_speech_frames,
            metrics.reference_speech_frames
        );
        assert_eq!(metrics.term_hits, 2);
        assert_eq!(metrics.exact_speaker_count, 1);
    }

    #[test]
    fn missing_speech_text_term_and_speaker_worsen_metrics() {
        let truth = transcript(vec![
            segment(0, 1_000, "A", "hello alpha"),
            segment(1_000, 2_000, "B", "world beta"),
        ]);
        let candidate = transcript(vec![segment(0, 500, "only", "hello")]);
        let metrics = score_candidate(
            &truth,
            &candidate,
            &["alpha".to_owned(), "beta".to_owned()],
            2_000,
        )
        .unwrap();
        assert!(metrics.word_errors > 0);
        assert!(metrics.character_errors > 0);
        assert!(metrics.diarization_errors > 0);
        assert!(metrics.jer_sum > 0.0);
        assert!(metrics.covered_speech_frames < metrics.reference_speech_frames);
        assert_eq!(metrics.term_hits, 0);
        assert_eq!(metrics.exact_speaker_count, 0);
    }

    #[test]
    fn duplicate_and_unknown_fields_are_detected() {
        let truth = transcript(vec![segment(0, 1_000, "A", "repeat")]);
        let candidate = transcript(vec![
            segment(0, 500, "A", "repeat"),
            segment(500, 1_000, "A", "repeat"),
        ]);
        let metrics = score_candidate(&truth, &candidate, &[], 1_000).unwrap();
        assert_eq!(metrics.adjacent_duplicates, 1);
        assert_eq!(metrics.adjacent_pairs, 1);
        assert!(serde_json::from_str::<CanonicalTranscript>(
            r#"{"schema_version":1,"segments":[],"private":"forbidden"}"#
        )
        .is_err());
    }

    #[test]
    fn reference_repetition_is_not_counted_as_candidate_hallucination() {
        let truth = transcript(vec![
            segment(0, 500, "A", "repeat"),
            segment(500, 1_000, "A", "repeat"),
        ]);
        let candidate = truth.clone();
        let metrics = score_candidate(&truth, &candidate, &[], 1_000).unwrap();
        assert_eq!(metrics.adjacent_duplicates, 0);
        assert_eq!(metrics.adjacent_pairs, 1);
    }

    #[test]
    fn local_unknown_is_a_diarization_miss_not_a_speaker_cluster() {
        let truth = transcript(vec![segment(0, 1_000, "A", "hello")]);
        let candidate = transcript(vec![segment(0, 1_000, UNKNOWN_SPEAKER, "hello")]);
        let metrics = score_candidate(&truth, &candidate, &[], 1_000).unwrap();
        assert_eq!(metrics.exact_speaker_count, 0);
        assert_eq!(metrics.diarization_errors, metrics.reference_speaker_frames);
        assert_eq!(
            metrics.covered_speech_frames,
            metrics.reference_speech_frames
        );
    }

    #[test]
    fn report_never_passes_an_incomplete_matrix() {
        let report = build_report(&BTreeMap::new(), AcceptancePolicy::LegacyV1);
        assert_eq!(report.status, "insufficient_corpus");
        assert!(report
            .strata
            .values()
            .all(|stratum| !stratum.sufficient && !stratum.passed));
    }

    fn passing_metric_totals() -> BTreeMap<Stratum, StratumTotals> {
        let metrics = RawMetrics {
            reference_words: 100,
            reference_characters: 100,
            reference_speaker_frames: 100,
            reference_speakers: 40,
            exact_speaker_count: 20,
            term_hits: 20,
            term_total: 20,
            covered_speech_frames: 100,
            reference_speech_frames: 100,
            adjacent_pairs: 20,
            ..RawMetrics::default()
        };
        Stratum::ALL
            .into_iter()
            .map(|stratum| {
                (
                    stratum,
                    StratumTotals {
                        cases: 20,
                        duration_ms: 20 * 90 * 60 * 1_000,
                        shortest_case_ms: 90 * 60 * 1_000,
                        miaoji: metrics,
                        local: metrics,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn relative_release_can_pass_while_absolute_stretch_fails_without_changing_metrics() {
        let mut totals = passing_metric_totals();
        for stratum in totals.values_mut() {
            stratum.local.exact_speaker_count = 16;
            stratum.miaoji.exact_speaker_count = 6;
        }
        let legacy = build_report(&totals, AcceptancePolicy::LegacyV1);
        let relative = build_report(&totals, AcceptancePolicy::MiaojiRelativeV2);
        assert_eq!(legacy.status, "fail");
        assert_eq!(relative.status, "pass");
        assert_eq!(relative.stretch.as_ref().unwrap().status, "fail");
        let legacy_json = serde_json::to_value(legacy).unwrap();
        let relative_json = serde_json::to_value(relative).unwrap();
        assert_eq!(legacy_json["schema_version"], 1);
        for field in ["acceptance_policy", "scope", "stretch"] {
            assert!(legacy_json.get(field).is_none());
        }
        assert_eq!(relative_json["schema_version"], 2);
        assert_eq!(relative_json["acceptance_policy"], "miaoji-relative-v2");
        for stratum in Stratum::ALL {
            for metric_source in ["local", "miaoji"] {
                assert_eq!(
                    legacy_json["strata"][stratum.name()][metric_source],
                    relative_json["strata"][stratum.name()][metric_source]
                );
            }
        }
    }

    #[test]
    fn relative_count_is_gated_per_stratum_and_accepts_an_exact_tie() {
        let mut totals = passing_metric_totals();
        let mixed = totals.get_mut(&Stratum::Mixed).unwrap();
        mixed.local.exact_speaker_count = 8;
        mixed.miaoji.exact_speaker_count = 16;
        let report = build_report(&totals, AcceptancePolicy::MiaojiRelativeV2);
        assert_eq!(report.status, "fail");
        assert_eq!(
            report.strata["mixed"].failure_codes,
            ["speaker_count_noninferiority"]
        );
        assert!(report.strata["english"].passed);
        totals
            .get_mut(&Stratum::Mixed)
            .unwrap()
            .local
            .exact_speaker_count = 16;
        assert_eq!(
            build_report(&totals, AcceptancePolicy::MiaojiRelativeV2).status,
            "pass"
        );
    }

    #[test]
    fn relative_policy_preserves_other_failures_and_missing_corpus_is_not_stretch_success() {
        let mut totals = passing_metric_totals();
        totals
            .get_mut(&Stratum::Mandarin)
            .unwrap()
            .local
            .word_errors = 10;
        let report = build_report(&totals, AcceptancePolicy::MiaojiRelativeV2);
        assert_eq!(report.status, "fail");
        assert!(report.strata["mandarin"]
            .failure_codes
            .contains(&"wer_noninferiority"));
        assert_eq!(report.stretch.as_ref().unwrap().status, "pass");
        totals.remove(&Stratum::LongForm);
        let report = build_report(&totals, AcceptancePolicy::MiaojiRelativeV2);
        assert_eq!(report.status, "insufficient_corpus");
        let stretch = report.stretch.as_ref().unwrap();
        assert_eq!(stretch.status, "insufficient_corpus");
        assert_eq!(stretch.strata["long_form"], None);
        assert!(!report.strata["long_form"].passed);
    }

    #[test]
    fn normalization_is_deterministic_for_mixed_text() {
        assert_eq!(
            word_tokens("Hello，世界 ROI-7"),
            ["hello", "世", "界", "roi", "7"]
        );
        assert_eq!(normalized_characters(" Hello，世界! "), "hello世界");
    }

    #[test]
    fn manifest_run_is_aggregate_only_and_rejects_symlinked_inputs() {
        let root = TempDir::new().unwrap();
        let transcript = serde_json::json!({
            "schema_version": 1,
            "segments": [{
                "start_ms": 0,
                "end_ms": 1000,
                "speaker": "private_speaker",
                "text": "private private_term_qzx phrase"
            }]
        });
        for name in ["truth.json", "miaoji.json", "local.json"] {
            fs::write(
                root.path().join(name),
                serde_json::to_vec(&transcript).unwrap(),
            )
            .unwrap();
        }
        let manifest = serde_json::json!({
            "schema_version": 1,
            "cases": [{
                "case_id": "private_case",
                "stratum": "english",
                "duration_ms": 1000,
                "ground_truth": "truth.json",
                "miaoji": "miaoji.json",
                "local": "local.json",
                "named_terms": ["private_term_qzx"]
            }]
        });
        let manifest_path = root.path().join("matrix.json");
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let report = evaluate_manifest(manifest_path.as_os_str()).unwrap();
        assert_eq!(report.status, "insufficient_corpus");
        let output = serde_json::to_string(&report).unwrap();
        let coverage = evaluate_speaker_coverage(manifest_path.as_os_str()).unwrap();
        let coverage_output = serde_json::to_string(&coverage).unwrap();
        for private_value in [
            "private_case",
            "private_speaker",
            "private private_term_qzx phrase",
            "truth.json",
            "private_term_qzx",
        ] {
            assert!(!output.contains(private_value));
            assert!(!coverage_output.contains(private_value));
        }

        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("outside.json"), b"{}").unwrap();
        symlink(
            outside.path().join("outside.json"),
            root.path().join("linked.json"),
        )
        .unwrap();
        assert_eq!(
            resolve_regular_file(root.path(), "linked.json")
                .unwrap_err()
                .code(),
            "path_rejected"
        );
    }
}
