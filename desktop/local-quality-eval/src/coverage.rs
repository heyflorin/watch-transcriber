//! Separate lexical attribution gate; it never changes historical score reports.
use std::{collections::BTreeMap, ffi::OsStr};

use serde::Serialize;

use crate::{
    load_manifest, ratio, read_transcript, report_status, word_tokens, CanonicalTranscript,
    EvalError, Stratum, UNKNOWN_SPEAKER,
};

#[derive(Debug, Serialize)]
pub struct SpeakerCoverageReport {
    schema_version: u32,
    scope: &'static str,
    tokenizer: &'static str,
    minimum_per_case: f64,
    status: &'static str,
    strata: BTreeMap<&'static str, CoverageStratum>,
}

#[derive(Debug, Serialize)]
struct CoverageStratum {
    sufficient: bool,
    passed: bool,
    failure_codes: Vec<&'static str>,
    cases: usize,
    duration_ms: u64,
    local: CoverageMetric,
    miaoji: CoverageMetric,
}

#[derive(Clone, Debug, Default, Serialize)]
struct CoverageMetric {
    lexical_tokens: u64,
    assigned_tokens: u64,
    assigned_fraction: f64,
    minimum_case_fraction: Option<f64>,
    cases_below_minimum: usize,
    cases_without_lexical_tokens: usize,
}

#[derive(Default)]
struct Totals {
    cases: usize,
    duration_ms: u64,
    shortest_case_ms: u64,
    local: CoverageMetric,
    miaoji: CoverageMetric,
}

impl CoverageMetric {
    fn add(&mut self, transcript: &CanonicalTranscript) {
        let (mut total, mut assigned) = (0_u64, 0_u64);
        for segment in &transcript.segments {
            // Same tokenizer and segment-joining separators as WER. No ASCII-only
            // shortcut: accented words and mixed-language Han remain in scope.
            let tokens = word_tokens(&segment.text).len() as u64;
            total += tokens;
            if segment.speaker != UNKNOWN_SPEAKER {
                assigned += tokens;
            }
        }
        let fraction = ratio(assigned, total);
        self.lexical_tokens += total;
        self.assigned_tokens += assigned;
        self.assigned_fraction = ratio(self.assigned_tokens, self.lexical_tokens);
        self.minimum_case_fraction = Some(
            self.minimum_case_fraction
                .map_or(fraction, |prior| prior.min(fraction)),
        );
        self.cases_without_lexical_tokens += usize::from(total == 0);
        // Exact integer comparison, with no rounding tolerance or vacuous pass.
        self.cases_below_minimum += usize::from(total == 0 || assigned * 100 < total * 99);
    }
}

/// Evaluate the existing ≥99% assigned-word gate using the WER tokenizer.
///
/// This measures attribution presence, not speaker correctness, ASR recall,
/// aligned-word timestamp accuracy, or human short-turn recall. No reference
/// transcript is read or used to assign a label. Existing report modes are unchanged.
///
/// # Errors
/// Returns closed manifest/canonical-input errors without disclosing content.
pub fn evaluate_speaker_coverage(
    path: impl AsRef<OsStr>,
) -> Result<SpeakerCoverageReport, EvalError> {
    let (manifest, root) = load_manifest(path)?;
    let mut totals = BTreeMap::<Stratum, Totals>::new();
    for case in &manifest.cases {
        let local = read_transcript(&root, &case.local, case.duration_ms)?;
        let miaoji = read_transcript(&root, &case.miaoji, case.duration_ms)?;
        let entry = totals.entry(case.stratum).or_default();
        entry.cases += 1;
        entry.duration_ms += case.duration_ms;
        entry.shortest_case_ms = if entry.shortest_case_ms == 0 {
            case.duration_ms
        } else {
            entry.shortest_case_ms.min(case.duration_ms)
        };
        entry.local.add(&local);
        entry.miaoji.add(&miaoji);
    }
    Ok(build_report(totals))
}

fn build_report(mut totals: BTreeMap<Stratum, Totals>) -> SpeakerCoverageReport {
    let mut strata = BTreeMap::new();
    let mut any_insufficient = false;
    let mut all_passed = true;
    for stratum in Stratum::ALL {
        let total = totals.remove(&stratum).unwrap_or_default();
        let (minimum_cases, minimum_duration) = stratum.minimums();
        // Named-term count is a separate truth-dependent gate, not a measure of
        // lexical assignment corpus coverage. The same case/duration floors apply.
        let sufficient = total.cases >= minimum_cases
            && total.duration_ms >= minimum_duration
            && (stratum != Stratum::LongForm || total.shortest_case_ms >= 60 * 60 * 1_000);
        let mut failure_codes = Vec::new();
        if !sufficient {
            any_insufficient = true;
            failure_codes.push("insufficient_corpus");
        }
        if total.local.cases_below_minimum > 0 {
            failure_codes.push("speaker_lexical_coverage");
        }
        if total.local.cases_without_lexical_tokens > 0 {
            failure_codes.push("no_lexical_tokens");
        }
        let passed = failure_codes.is_empty();
        all_passed &= passed;
        strata.insert(
            stratum.name(),
            CoverageStratum {
                sufficient,
                passed,
                failure_codes,
                cases: total.cases,
                duration_ms: total.duration_ms,
                local: total.local,
                miaoji: total.miaoji,
            },
        );
    }
    SpeakerCoverageReport {
        schema_version: 1,
        scope: "speaker_lexical_assignment_only",
        tokenizer: "wer-unicode-alphanumeric-han-v1",
        minimum_per_case: 0.99,
        status: report_status(any_insufficient, all_passed),
        strata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{segment, transcript};

    #[test]
    fn same_unicode_tokenizer_counts_unknown_han_accented_words_and_overlap() {
        let input = transcript(vec![
            segment(0, 1_000, "known", "Hello，世界 ROI-7 café"),
            segment(500, 1_500, UNKNOWN_SPEAKER, "Élan 世界"),
            segment(1_500, 2_000, "known", "？！"),
        ]);
        let mut metric = CoverageMetric::default();
        metric.add(&input);
        assert_eq!(metric.lexical_tokens, 9);
        assert_eq!(metric.assigned_tokens, 6);
        assert_eq!(metric.cases_below_minimum, 1);
        assert_eq!(metric.cases_without_lexical_tokens, 0);
        assert_eq!(word_tokens(&crate::joined_text(&input)).len(), 9);
    }

    #[test]
    fn exact_99_passes_but_aggregate_cannot_hide_one_failing_case() {
        let mut metric = CoverageMetric::default();
        metric.add(&transcript(vec![
            segment(0, 1_000, "known", &"word ".repeat(99)),
            segment(1_000, 2_000, UNKNOWN_SPEAKER, "word"),
        ]));
        assert_eq!(metric.cases_below_minimum, 0);
        metric.add(&transcript(vec![segment(
            0,
            1_000,
            "known",
            &"word ".repeat(10_000),
        )]));
        metric.add(&transcript(vec![segment(
            0,
            1_000,
            UNKNOWN_SPEAKER,
            "word",
        )]));
        assert!(metric.assigned_fraction > 0.99);
        assert_eq!(metric.cases_below_minimum, 1);
        assert_eq!(metric.minimum_case_fraction, Some(0.0));
    }

    #[test]
    fn punctuation_only_and_missing_corpus_never_pass() {
        let mut metric = CoverageMetric::default();
        metric.add(&transcript(vec![segment(0, 1_000, "known", "？！")]));
        assert_eq!(metric.cases_below_minimum, 1);
        assert_eq!(metric.cases_without_lexical_tokens, 1);
        let report = build_report(BTreeMap::new());
        assert_eq!(report.status, "insufficient_corpus");
        assert!(report
            .strata
            .values()
            .all(|s| !s.passed && s.local.minimum_case_fraction.is_none()));
    }

    #[test]
    fn coverage_failure_is_per_case_and_miaoji_is_comparison_only() {
        let mut totals = BTreeMap::new();
        for stratum in Stratum::ALL {
            let (cases, duration_ms) = stratum.minimums();
            totals.insert(
                stratum,
                Totals {
                    cases,
                    duration_ms,
                    shortest_case_ms: 60 * 60 * 1_000,
                    local: CoverageMetric::default(),
                    miaoji: CoverageMetric {
                        cases_below_minimum: 10,
                        ..CoverageMetric::default()
                    },
                },
            );
        }
        totals
            .get_mut(&Stratum::Mixed)
            .unwrap()
            .local
            .cases_below_minimum = 1;
        let report = build_report(totals);
        assert_eq!(report.status, "fail");
        assert!(report.strata["english"].passed);
        assert_eq!(
            report.strata["mixed"].failure_codes,
            ["speaker_lexical_coverage"]
        );
    }
}
