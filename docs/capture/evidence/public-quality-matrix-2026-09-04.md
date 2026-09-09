# Public full-local quality matrix evidence — 2026-09-04

## Corpus boundary

The private, Git-ignored evaluation root uses only public, independently
annotated corpora. No personal recording is part of this matrix.

| Stratum | Source | License / independent truth |
|---|---|---|
| English | AMI Meeting Corpus | CC BY 4.0; manual orthographic transcripts with per-speaker word timing |
| Mandarin | AISHELL-4 test split | CC BY-SA 4.0; accurate transcription, speaker activity, TextGrid and RTTM |
| Mixed | ASCEND | CC BY-SA 4.0; expert-generated spontaneous Mandarin/English code-switch transcription and speaker/session metadata |
| Overlap | AMI windows selected by annotated overlap | CC BY 4.0; distinct speakers remain overlapping in canonical truth |
| Long form | four nonrepetitive 90-minute AMI composites | CC BY 4.0; source transcript intervals are offset without repetition |

Primary source records:

- AMI overview and license: <https://groups.inf.ed.ac.uk/ami/corpus/overview.shtml>
- AMI downloads and manual annotation release:
  <https://groups.inf.ed.ac.uk/ami/download>
- AISHELL-4 SLR111: <https://www.openslr.org/111>
- ASCEND source repository: <https://github.com/HLTCHKUST/ASCEND>
- ASCEND public dataset revision:
  <https://huggingface.co/datasets/CAiRE/ASCEND>

Downloaded source identities include:

- AMI manual annotations v1.6.2 SHA-256
  `b56e5babb2496b8795deeeda7e71178d7fbc9963f94276cf2a3f4b56ebbc9f9d`;
- AISHELL-4 test archive SHA-256
  `7e5d306b5f18ab66fcd7e0380c90979b47fd9576bfa8e67e6353bdec7c14a35a`;
- all five ASCEND parquet files matched the SHA-256 identities published by
  Hugging Face revision `737e9800ae31be9932ba8464c80366559bd28424`.

Historical duration note: the App importer supplied these evaluation timelines.
A 2026-09-05 audit found that it counted AAC encoder priming/padding, up to
127.25ms extra, rather than effective decoded media duration. All44 original
AAC hashes still match the retained Miaoji import copies. The frozen manifest
and historical scores remain unchanged; current decoding/import now uses
effective frames. See the MOSS evidence's duration correction below.
The final private manifest SHA-256 is
`c6615a07f8e80fbe75d3da65b75898423a716749750a1033bca0db70b4506280`.
Candidate named terms were selected from reference text but remain explicitly
marked for human review; they cannot close the adjudication gate by themselves.

## Frozen matrix size

| Stratum | Cases | Duration |
|---|---:|---:|
| English | 10 | 2.000 h |
| Mandarin | 10 | 2.000 h |
| Mixed | 10 | 2.007 h |
| Overlap | 10 | 0.500 h |
| Long form | 4 | 6.000 h |
| **Total** | **44** | **12.507 h** |

A structure-only run copied each ground truth to both candidate slots in a
temporary directory. The Rust evaluator accepted every closed schema,
duration, path, speaker, overlap, and minimum-corpus invariant. That identity
run was only a validator check and was never represented as model quality.

## Complete Miaoji/Gemini baseline

The Rust App path processed all 44 same-audio cases. It used an ephemeral
in-memory credential adapter instead of the real App Keychain, deterministic
request fences, the current `volc.lark.minutes` API, and 31-second per-task
polling. The batch covered 45,024.427 seconds and completed in 1,026.58 seconds.

- 44/44 ledgers reached `complete`;
- 44/44 exact TOS versions were deleted and checkpointed;
- 44 canonical Miaoji transcripts and 44 Gemini summaries were written only
  under the ignored matrix root;
- no GitHub or R2 archive effect ran;
- no transcript, summary, case identifier, signed URL, credential, or object
  identity was printed.

Aggregate human-reference baseline:

| Stratum | Miaoji WER | CER | DER | JER | Exact speaker count | Timestamp coverage |
|---|---:|---:|---:|---:|---:|---:|
| English | 25.28% | 18.30% | 37.26% | 52.22% | 30% | 83.42% |
| Mandarin | 16.38% | 16.41% | 20.35% | 42.52% | 10% | 92.24% |
| Mixed | 11.62% | 9.30% | 20.26% | 24.34% | 80% | 92.58% |
| Overlap | 47.01% | 39.86% | 70.82% | 81.19% | 0% | 72.69% |
| Long form | 34.49% | 26.45% | 41.43% | 49.95% | 25% | 76.10% |

These figures also show why Miaoji is a comparison candidate, not truth.

## Evaluator correction

The first baseline sanity check incorrectly failed when the candidate was an
exact copy of ground truth. Real adjacent repeated turns in AMI/AISHELL truth
were being counted as candidate hallucinations. The metric now reports only
candidate duplicate count in excess of the independently annotated reference,
divided by candidate pairs. WER/CER still penalize deletion. A new regression
test proves that exact reference repetition has zero duplicate error, while an
extra repeated candidate segment is still detected. A second correction keeps
`local_unknown` out of the detected-speaker set while still counting its time
coverage and DER miss. All eight evaluator tests pass, and the identity baseline
now passes all five strata.

## Current local candidate — MOSS, 2026-09-05

MOSS Q8/Metal has completed 40 public cases, with lower WER/CER and DER/JER
than Miaoji in English, Mandarin, Mixed, and Overlap. Counts are 25/40 versus
12/40 overall, while Mixed count regresses 4/10 versus 8/10 and term recall is
90% versus 100% pending human confirmation. All four new native long cases
complete, with aggregate WER/CER43.73%/39.04% vs34.49%/26.45% and
DER/JER41.32%/67.30% vs41.43%/49.95%; relative long acceptance fails. The
older stopped batch is not a quality result. Its historical hybrid manifest
retains Qwen/SpeakerKit long-form rows, which must never be called MOSS scores.
See [the exact identities, metrics, adaptations, and open gates](moss-public-quality-2026-09-05.md).

Release count acceptance now compares with Miaoji in each stratum; the
agent-added absolute 95% target is separate stretch work. The Rust grader now
supports `--policy miaoji-relative-v2` and preserves the default legacy output.
The pure 40-case manifest excludes Qwen long-form placeholders and reports
insufficient corpus plus Mixed count/term failures. The frozen44 manifest adds
four actual native long results and fails relative acceptance. A subsequent
raw-marker correction has a separate pure40 manifest: English WER/CER improves
to16.71%/11.94% vs25.28%/18.30%, with no speech rewriting or new inference.
It does not promote old long results into raw-provenance proof. Rust adaptation replay
matches all 40 canonical cases; the first standalone native 12-minute worker
run also matches its prior CLI output under network denial. App integration
and full-local acceptance are not claimed from these partial results.

Subsequent quiet-boundary32 windows of four long files, optional adjacent
same-ID coalescing v2, and overlap-constrained SpeakerKit anchoring v3 pass the
six-hour long-form relative comparison: WER/CER23.21%/16.65%, DER/JER
23.28%/33.26%, count2/4 versus1/4, terms100% both. The separate Rust
WER-tokenizer lexical-assignment check passes all four (minimum99.993%) and
the corrected40 short cases. This does not erase whole-file failures or Mixed
count/term regressions, and does not establish a unified App policy or human
acceptance. Exact immutable artifacts are listed in the MOSS evidence.

## Historical Qwen/SpeakerKit signal

The first local result exposed two evaluator/harness defects: `local_unknown`
was incorrectly counted as a detected speaker, and individual aligned words
were not regrouped into consecutive speaker turns. Those old speaker-count and
65.04%/80.45% DER/JER figures are superseded and must not be reused.

The corrected full 44-file run uses MLX Qwen3-ASR,
official ForcedAligner timestamps, and the new Argmax SpeakerKit replacement
candidate. English, Mandarin, and overlap beat or meet Miaoji on WER/CER and
DER/JER; the four 90-minute files also beat Miaoji at 25.07%/17.60% local
WER/CER versus 34.49%/26.45%. Mixed speaker error improves, but mixed WER/CER
remains materially worse. Exact speaker count is only 27/44, and one overlap
case has 98.66% assigned-word coverage rather than the required 99%. The
complete metrics, alternate runtime/model results, source identities, and
caveats are in `local-quality-challengers-2026-09-04.md`.

Natural AMI inference also falsified the shared Whisper-derived Qwen timeout:
the first 12-minute worker was killed at the old 480-second bound. Qwen now has
its own cancelable bounded timeout of `2 × audio duration + 5 minutes`, capped
at eight hours; the original ledger is explicitly retried rather than replaced.

The production-native SpeakerKit offline subset, exact candidate catalog,
install/proof/remove flow, and closed-protocol public pilot pass. Further native
Qwen MLX porting is now conditional on MOSS failing the complete deliverable.
The current MOSS work, relative mixed count, human short-turn/term/summary
review, and crash/signing/bundle proof remain open. No complete local route can
yet be advertised as matching Miaoji.
