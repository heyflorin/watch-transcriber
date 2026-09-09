# EVAL DEFINITION: full-local quality parity

## Decision

Policy clarification — 2026-09-05: the product target is quality comparable to
Miaoji. The former agent-added 95% exact-speaker-count gate is a separate
stretch target. Relative release acceptance requires exact-count accuracy at
least as high as Miaoji in each stratum; all other gates below are unchanged.
The versioned evaluator is implemented; neither policy can establish human
review, App integration, or physical release acceptance by itself.

EchoWall may describe the Apple-Silicon model path as comparable in quality to
飞书妙记 only after this matrix passes. Miaoji output is a candidate, never the
ground truth. Every file is independently transcribed and speaker-annotated,
with a second human adjudicating disagreements before either system is scored.

The evaluator is `desktop/local-quality-eval`. It is a Rust development tool,
not an App dependency. It performs no network access and emits only aggregate
JSON; transcript text, terms, paths, case IDs, timestamps, and speaker names are
never printed. Private manifests and canonical transcripts remain untracked.

## Corpus contract

Each case supplies one closed canonical segment JSON for ground truth, Miaoji,
and full local output. Segments contain millisecond start/end, an anonymous
speaker ID, and text. Ground truth may contain overlapping speakers. All files
are relative regular files under the manifest directory; symlinks, traversal,
unknown fields, oversized inputs, more than 16 speakers, and durations over five
hours are rejected.

The five strata are independently gated:

| Stratum | Minimum files | Minimum duration | Additional requirement |
|---|---:|---:|---|
| English | 10 | 2 hours | conversational and meeting speech |
| Mandarin | 10 | 2 hours | native Mandarin, punctuation ignored |
| Mixed | 10 | 2 hours | within-turn Chinese/English code switching |
| Overlap | 10 | 30 minutes | human RTTM-style overlapping speaker intervals |
| Long form | 3 | 6 hours | every file at least 60 minutes; nonrepetitive content |

Each stratum must contain at least ten adjudicated named-term targets. Public
corpora may satisfy most cases; any private case requires explicit authorization
and stays outside Git. The prior user-authorized recording and repeated two-hour
fixture remain diagnostic evidence only.

## Deterministic metrics

- WER uses lowercase Unicode alphanumeric tokens; each Han character is a token
  so Mandarin and mixed text do not depend on an external segmenter.
- CER uses lowercase Unicode alphanumeric characters with whitespace and
  punctuation removed.
- DER uses 10 ms frames, optimal anonymous-speaker mapping, and no forgiveness
  collar. Miss, false alarm, and speaker confusion are divided by reference
  speaker-time; overlapping reference speakers count independently.
- JER is the mean mapped per-reference-speaker Jaccard error.
- Timestamp coverage is reference speech time overlapped by any candidate
  speech interval.
- Named-term recall uses normalized literal term presence.
- Exact speaker count is scored per file.
- `local_unknown` is an explicit unresolved label, not a speaker cluster. It is
  excluded from exact speaker count, counts as missed speaker time in DER/JER,
  and still counts toward timestamp coverage because the ASR interval exists.
- Adjacent duplicate rate is excess exact normalized candidate duplication
  beyond the independently annotated reference count, divided by candidate
  segment pairs. This preserves legitimate repeated turns in ground truth while
  retaining the repeated two-hour artifact as an explicit long-form regression
  metric.

## Per-stratum non-inferiority gates

All conditions must pass in every stratum; an aggregate average cannot rescue a
weak stratum.

- Local WER and CER are each no more than 2 absolute percentage points and 20%
  relative worse than Miaoji.
- Local DER and JER are each no more than 3 absolute percentage points and 20%
  relative worse than Miaoji.
- Local exact-speaker-count accuracy is at least Miaoji's on the same files
  in each stratum. Report both rates; an aggregate gain cannot cancel a
  regression in mixed-language or another stratum.
- Local named-term recall is no more than 5 percentage points below Miaoji.
- Local timestamp coverage is no more than 2 percentage points below Miaoji.
- Local adjacent-duplicate rate is no more than 1 percentage point above
  Miaoji.

## Separate stretch target and grader migration

The ≥99% per-case speaker-assigned lexical-word requirement is checked by the
separate `echowall-speaker-coverage` binary. It uses the same lowercase
Unicode-alphanumeric / Han tokenizer as WER, counts every `local_unknown` word
in the denominator, and fails zero-word cases. It reports aggregate counts,
minimum case fraction, and number of failing cases per stratum; averages cannot
hide a failing case. Existing case/duration/long-form floors apply. Named terms
and ground-truth-dependent metrics remain the other grader's responsibility.
No reference text is read to assign labels. A lexical-assignment pass does not
prove label correctness, ASR recall, aligned-word timestamp accuracy, or the
95% human short-turn requirement. CLI success means report generation only.

```sh
desktop/local-quality-eval/target/release/echowall-speaker-coverage \
  local-eval/matrix/manifest-moss-quiet12-coalesced-v2-speakerkit-v3-long4.json
```

The absolute target of at least 95% exact-count accuracy remains a reported
stretch target and is not required to claim relative Miaoji parity. It is
unrelated to the separate 95% human short-turn recall and bilingual-summary
consistency requirements, which remain release gates.

The default command and `--policy legacy-v1` retain the original report schema
and absolute-count failure condition. `--policy miaoji-relative-v2` selects
report schema 2: top-level `status` reflects the relative transcript metrics,
`scope` is explicitly `transcript_metrics_only`, and `stretch` reports the 95%
exact-count target independently. Insufficient strata have `null` stretch
verdicts and prevent either aggregate verdict from passing. CLI exit zero means
a report was generated, not that its status passed; callers must inspect JSON.

```sh
desktop/local-quality-eval/target/release/echowall-local-quality-eval \
  local-eval/matrix/manifest-moss-transcribe-q8-40.json --policy miaoji-relative-v2
```

The input schema and metric calculations are unchanged. Eleven library tests
and one CLI test pass, including per-stratum count regression, an exact tie,
stretch failure with relative success, preserved ASR failure, and insufficient
corpus. The existing hybrid-matrix legacy output was compared before/after and
is byte-identical. The new pure 40-case MOSS manifest excludes the Qwen
long-form placeholders; it reports `insufficient_corpus`, with English,
Mandarin, and Overlap machine comparisons passing and Mixed count/terms
failing. Missing long-form or human evidence still prevents completion.

Do not confuse transcription errors, speaker-attribution errors (DER/JER),
and exact speaker count. On the completed 40 MOSS cases, both error families
are lower than Miaoji in every measured stratum, but count regresses in Mixed
(4/10 versus 8/10) despite the overall gain (25/40 versus 12/40).

## Summary non-inferiority gate

Summary quality is evaluated twice so ASR errors are not confused with model
quality:

1. summary-only: local Qwen and Gemini receive the same adjudicated transcript;
2. end-to-end: each summary receives its own local or Miaoji transcript.

Two reviewers first create a transcript-grounded inventory of atomic facts,
decisions, named terms, and action items with owner/deadline fields. Candidate
summaries are randomized as A/B and reviewed without model names. A second
reviewer adjudicates every disagreement before the assignment is unsealed.
Model-based judging may be retained as diagnostic evidence only and cannot pass
this gate.

Every stratum must pass all of these conditions:

- importance-weighted fact/decision recall trails Gemini by no more than 5
  percentage points;
- unsupported-claim rate is no more than 1 point above Gemini and no critical
  unsupported claim is allowed;
- action-item precision and recall each trail by no more than 5 points;
- named-term recall trails by no more than 5 points;
- English/Chinese summaries are judged semantically consistent in at least 95%
  of cases;
- title and category are both acceptable in at least 90% of cases;
- no more than 5% of cases omit a critical decision, owner, or deadline that
  Gemini retained.

The report contains counts and rates only. Transcript text, atomic facts,
candidate summaries, titles, terms, reviewer comments, and case IDs remain in
the untracked evaluation root.

## Capability evals

Uniform App-policy run (defined2026-09-05): process all44 original public AAC
files through the current embedded Rust importer, effective-frame preparation,
quiet12m windowing, coalescing-v2 native MOSS adapter and graph-v3 SpeakerKit
mapping. Language stays automatic; no count hint, case-specific parameter,
truth/stratum routing or earlier transcript reuse is allowed. Execution order
may prioritize Mixed for diagnosis, but cannot change model decisions. Every
source must match the frozen source-duration audit and Miaoji import hash.
The ten Mixed files all exceed12 minutes in effective frames; their old
whole-file outputs cannot stand in for this two-window App policy.

Follow-up diagnostic, defined2026-09-05: the uniform Mixed10 has three
one-speaker overcounts. Reference-overlap analysis finds one person split into
two substantial clusters in two cases (roughly90–176 seconds per cluster),
and a5.8-second extra cluster in the third. A duration-only filter is not an
acceptable fix. Measure the existing SpeakerKit raw-space centroid cosine
distances across all44 public inputs using the unchanged0.6/no-count-hint
inference configuration. Keep pair distances and anonymous intervals only;
never persist raw voice embedding vectors or add them to the App protocol.
Use reference labels only afterward to diagnose true-split and different-speaker
pairs. This is not a universal same-person threshold, an automatic merge rule,
or quality acceptance. Do not run this second inference experiment concurrently
with the still-active original-AAC44 baseline.

The opt-in source-engine harness uses the real per-window ledger and ordinary
workers under OS network denial, retains failures and immutable source/request/
response identities, and deliberately stops each job at `Summarizing`. This
isolates the transcript-quality comparison; it must not be reported as full
local completion, summary acceptance, UI, installed-App or release evidence.
Any failed case remains in the batch outcome, even if a successful-subset
manifest is also emitted for diagnosis. The frozen baseline/ground truth and
old reports remain unchanged. Evaluate all existing per-stratum gates and the
separate per-case lexical-assignment checker; do not infer success from a
native process exit or a generated report.

1. The same manifest deterministically produces the same aggregate metrics.
2. Perfect local/Miaoji candidates score zero WER/CER/DER/JER, full timestamp
   and term coverage, and exact speaker count.
3. Speaker label permutations do not change DER/JER.
4. Missed speech, false alarms, speaker confusion, term loss, and duplicated
   adjacent segments each worsen the intended metric.
5. Overlapping reference speech is counted as speaker-time, not flattened.
6. Any undersized stratum yields `insufficient_corpus`, never a passing result.
7. No successful or failed run emits content, paths, case IDs, timestamps, or
   named terms.

## Regression evals

- App, protocol, Whisper worker, Swift worker, browser UI, macOS bundle,
  iOS/Windows cross-checks, and Android dual-ABI build gates remain green.
- The quality evaluator is absent from Tauri dependencies and release bundles.
- Live Miaoji calls are opt-in paid tests only; the evaluator itself is offline.

## Current report

- Capability/build/resource boundary: passing for implemented deterministic
  tests, private A/B, network-denied short inference, network-denied two-hour
  speech inference, and a network-denied end-to-end local summary/archive
  fixture.
- Quality parity: **in progress, not passed**. A 44-case public matrix now
  supplies independent AMI/AISHELL-4/ASCEND transcript and speaker truth for
  all five strata, and the same-audio 12.507-hour Miaoji/Gemini baseline has
  completed with exact temporary-TOS cleanup. The leading MOSS Q8/Metal
  candidate completes 40 public cases and beats Miaoji on WER/CER and DER/JER
  in each measured stratum. Mixed WER/CER is 10.26%/8.02% versus 11.62%/9.30%.
  Relative mixed count (4/10 versus 8/10), mixed named-term recall (90% versus
  100%), human short turns, and blinded summary remain open. The pre-raw-guard
  whole-file four native90-minute cases complete, but aggregate long WER/CER
  is43.73%/39.04% vs34.49%/26.45%, and DER/JER41.32%/67.30% vs41.43%/49.95%.
  That frozen full44 relative report fails. Later 32 quiet-boundary windows,
  source-preserving adjacent coalescing v2, and overlap-constrained SpeakerKit
  identity anchoring v3 pass the six-hour long-form relative metrics:
  WER/CER23.21%/16.65%, DER/JER23.28%/33.26%, count2/4 versus1/4, terms100%
  for both. The separate WER-tokenizer assignment check also passes all four
  cases (minimum99.993%). This does not establish a deployable policy across
  all five strata or human acceptance. The standalone Rust MOSS worker reproduces a 12-minute public case
  under network denial. Later source-engine and distinct-ID ad-hoc QA App
  ledger/summary/archive plus actual App-death/reopen evidence now exist;
  release-installed/UI/whole-App OS-offline and quality acceptance remain open.
  The bounded Rust adapter exactly replays all 40 diagnostic outputs, including
  four timing-conflicting segments marked unknown and one 84 ms end clip,
  without rewriting speech. A separate raw-marker correction of retained40
  outputs fixes five English cases and improves English WER/CER to16.71%/11.94%
  versus25.28%/18.30%; those artifacts do not make old long results raw-validated.
  Qwen/SpeakerKit's
  earlier 44-file results are retained independently; the hybrid manifest's
  long-form rows are not MOSS evidence. A 48.486-second two-system-voice English,
  Mandarin, mixed, short-turn, and overlap fixture now provides exact synthetic
  text/speaker truth. The initial Qwen policy scored 67.29% WER and 82.63% CER.
  A source-grounded VAD-utterance language reset improved the byte-identical
  rerun to 22.43% WER, 19.69% CER, and 82.72% speaker-assigned lexical words,
  but within-turn mixed speech and short overlap still fail. This is diagnostic
  evidence rather than a corpus pass. The user-authorized
  latest-recording local summary scored directionally comparable to the
  existing summary under a same-model local judge; that is also diagnostic,
  not parity evidence.
- Current release status: local mode is an experimental candidate, not yet a
  proven Miaoji-quality replacement.

See `../capture/evidence/public-quality-matrix-2026-09-04.md` for source
licenses, aggregate Miaoji metrics, current local pilot findings, and the
aggregate-only privacy boundary.
Current MOSS measurements and adaptation limitations are in
`../capture/evidence/moss-public-quality-2026-09-05.md`.
