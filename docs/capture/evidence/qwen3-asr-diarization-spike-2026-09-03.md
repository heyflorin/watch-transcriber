# Qwen3-ASR + local diarization spike — 2026-09-03

## Status and privacy boundary

> Superseded decision notice (2026-09-05): retain the single-recording and
> CPU/MOSS rejection checkpoints below as historical evidence. The
> [MOSS 40-case public comparison](moss-public-quality-2026-09-05.md) now leads
> replacement work, with lower ASR and attribution errors than Miaoji across
> all four measured strata; long-form and App integration are incomplete.
> Qwen/ForcedAligner remains an alternative with independent MLX output as its
> quality reference. SpeakerKit remains a separate-diarizer candidate. The
> current plan separates relative release acceptance from 95% exact-count
> stretch work; no historical result is retroactively a pass.

The first half of this document is an isolated development spike and decision
record, not proof that the shipping App path is complete. It used one
explicitly authorized, locally held
1,183.68-second recording. No audio, transcript text, title, local path, model
credential, or provider request was written to this repository or sent to a
new remote service. The measurements below are aggregate only. All inference
ran from an isolated temporary directory; this spike did not modify the App,
ledger, model catalog, or native worker sources. The later App implementation
and runtime comparison checkpoints below are separately identified. The later
candidate-pack, durable-ledger, and full-App checkpoints are real App-path
proofs, but the candidate remains default-off because the human quality matrix
is still open.

## Decision

Adopt the following as the implementation candidate for full-local meeting
transcription on Apple Silicon:

```text
Qwen3-ASR-1.7B
  → Qwen3-ForcedAligner-0.6B word timestamps
  → FluidAudio Pyannote/WeSpeaker/PLDA/VBx diarization
  → deterministic Rust word-to-speaker assignment
  → explicit local_unknown for unresolved words
```

Qwen3-ASR has no official native speaker-diarization model or speaker-labelled
output. Its official family contains ASR models and a forced aligner. The open
speaker-diarization request in the official toolkit proposes integrating an
external diarizer. Third-party “Qwen3-ASR diarization” projects similarly add
CAM++ or pyannote outside Qwen rather than exposing a hidden Qwen capability.

Keep FluidAudio as the selected diarizer. CAM++ may be evaluated against the
same ground truth later, but it is not preferred merely because a third-party
service packages it with Qwen.

Primary sources:

- [Qwen3-ASR official repository](https://github.com/QwenLM/Qwen3-ASR)
- [Qwen3-ASR-1.7B model](https://huggingface.co/Qwen/Qwen3-ASR-1.7B)
- [Qwen3-ForcedAligner-0.6B model](https://huggingface.co/Qwen/Qwen3-ForcedAligner-0.6B)
- [Open official-toolkit diarization request](https://github.com/QwenLM/Qwen3-ASR-Toolkit/issues/13)
- [Development-only pure-Rust QwenASR implementation](https://github.com/huanglizhuo/QwenASR)

## End-to-end spike result

`qwen-asr-cli` 0.9.1, wrapping the pure-Rust `qwen-asr` 0.11.0 library from
the same `d39208de340a8b0c3a28f53ac1fd160a7118666f` source revision, was used
as a quality baseline, not as the final acceleration decision. The correct
official aligner revision was downloaded explicitly because the CLI downloader incorrectly requested
`Qwen3-ASR-ForcedAligner-0.6B` and received HTTP 401; the official repository
is `Qwen3-ForcedAligner-0.6B`.

| Stage | Result |
|---|---:|
| Qwen3-ASR transcription only | 33.07 s |
| Qwen3-ASR + ForcedAligner | 63.81 s |
| Aligned words | 3,123 |
| Aligner peak RSS | 19,817,431,040 bytes |
| Default FluidAudio diarization | 4.75 s; 2 speakers; 149 intervals |
| Selected FluidAudio quality preset, three repeats | 5.48 / 5.53 / 5.86 s |
| Selected preset repeat stability | identical 2 speakers / 195 intervals / 6 zero-confidence intervals |
| Full selected pipeline wall time | about 69.3 s, or 17.1× realtime |

Miaoji labels are not ground truth. They are used below only as a regression
reference to reject obvious speaker drift while tuning; they cannot establish
DER or parity.

| Diarization/merge policy | Assigned-word coverage | Anonymous-speaker agreement on Miaoji-comparable words | Known speaker switches | Diarization time |
|---|---:|---:|---:|---:|
| Current default (`stepRatio=0.20`, `minEmbedding=1.0s`) | 96.51% | 98.46% | 36 | 4.75 s |
| Candidate (`stepRatio=0.15`, `minEmbedding=0.4s`) | 98.24% | 98.38% | 38 | median 5.53 s |
| Candidate + safe same-speaker gap bridge | **99.04%** | **98.39%** | 38 | no material inference cost |
| More aggressive `minEmbedding=0.2s` | 98.43% before bridging | 98.34% | 40 | 6.08 s |

The 0.2-second embedding threshold is rejected. It recovered only 0.19% more
words before bridging while increasing speaker flips, interval fragmentation,
and zero-confidence output (15 intervals versus 6).

Enabling FluidAudio's existing 0.4-second `zeroVoteReembed` pass was also
tested and rejected as the fix for this recording. It produced no meaningful
boundary/speaker change because the missing spans were segmentation/VAD gaps,
not speech-active frames with zero cluster votes.

## Definitive merge contract

Rust owns the final speaker assignment. It must implement these rules in order:

1. Validate monotonic Qwen word timestamps and FluidAudio intervals against the
   same recording/hash/duration identity.
2. Attach punctuation-only tokens to the preceding lexical word. Interpolate a
   monotonic interval for zero-duration lexical timestamp runs only between
   valid neighbouring aligner anchors; otherwise keep their original point. A
   zero point may receive a one-millisecond interval only when it lies strictly
   inside exactly one confidence-qualified diarization interval; points on an
   interval boundary or in a gap remain unresolved.
3. Assign a word to the unique speaker with maximum positive temporal overlap.
   Keep the existing confidence floor and reject tied maxima.
4. For an unassigned gap no longer than 1.0 second, bridge it only when the
   immediately preceding and following accepted diarization intervals belong
   to the same speaker. This rule alone raised the selected preset from 98.24%
   to 99.04% coverage without reducing reference agreement.
5. Do not assign by nearest speaker when the two sides differ, when only a
   weak/distant neighbour exists, or when an overlap is ambiguous. Preserve
   `local_unknown` instead of fabricating certainty.
6. Regroup adjacent words with the same assigned speaker into user-visible
   turns without changing transcript text or word timestamps. If dense aligned
   points cannot become positive, monotonic canonical turns, retain the exact
   Qwen chunk text and time range as `local_unknown`; never shift evidence to
   make the validator pass.

The current spike leaves 30 of 3,123 words explicit unknown (0.96%). That is an
acceptable fail-safe intermediate result, not permission to hide them.

## App implementation checkpoint

The App diarization protocol is now v2 and binds every request and response to
one of two explicit presets:

- `fluid-community-v1` preserves the previous `stepRatio=0.20` and
  `minEmbedding=1.0s` behavior for old-ledger replay;
- `fluid-step015-embed040-v1` selects `stepRatio=0.15` and
  `minEmbedding=0.4s` for newly selected full-local jobs.

Rust now owns the specified punctuation attachment, bounded zero-duration
anchor interpolation, strict single-interval point containment, unique positive
maximum-overlap assignment, confidence floor, and at-most-one-second
same-speaker bridge. Ambiguous, one-sided, boundary, and long gaps remain
`local_unknown`; nearest-speaker guessing is absent. Eleven protocol tests
include focused safe-bridge, punctuation, ambiguous-gap, dense-quantization,
point-boundary, and zero-duration-anchor cases.

As a private aggregate-only boundary check, the rebuilt App Swift worker ran
the selected preset on a previously authorized local recording under `(deny
network*)`. It completed in 6.50 seconds at 573,915,136-byte peak RSS with zero
swap, returned two speakers and 184 intervals, and two immediate repeats
produced byte-identical response SHA-256 values and identical interval/speaker
counts. Four intervals had zero confidence and the accepted output contained
42 speaker switches. The legacy preset on the same retained input completed in
4.42 seconds with 142 intervals, zero zero-confidence intervals, and 34
switches.

These counts intentionally are not presented as a reproduction of the
independent spike's 195 intervals and 38 switches: its normalized artifact and
build provenance were not retained as an App fixture. No human ground truth or
Miaoji-reference agreement was recalculated for this boundary check. The
private scratch copies were securely unlinked after aggregate measurements.

## Transcript-quality and runtime decision checkpoint

The same authorized 1,183.68-second English recording was rerun from the exact
official Qwen model revisions under `(deny network*)`. The existing Miaoji
transcript is only a regression reference, not ground truth. EchoWall's same
aggregate-only Rust normalizer and Levenshtein implementation used for the
Whisper comparison produced:

| Route | WER vs Miaoji | CER vs Miaoji | Segments | Aligned words | Adjacent duplicate segments |
|---|---:|---:|---:|---:|---:|
| Existing Whisper Turbo Q5 | 15.03% | 8.22% | 151 | n/a | 0 |
| Qwen auto language, independent worker baseline | **10.89%** | **6.12%** | 38 | 3,123 | 0 |
| Qwen forced English diagnostic | 11.11% | 6.38% | 39 | 3,138 | 0 |

On this single diagnostic recording, Qwen auto improved 3.92 absolute WER
points and 2.10 absolute CER points over the current Whisper route. This is the
missing evidence for promoting Qwen as the replacement candidate; it is not a
substitute for the frozen English/Mandarin/mixed/overlap/long-form human-truth
matrix.

The App candidate now has a separate closed v1 Qwen protocol and a 2.0MB arm64
one-shot Rust worker. It pins `qwen-asr` 0.11.0 exactly and includes neither the
0.9.1 CLI nor its `ureq` downloader/live-capture surface. The built worker links
only Accelerate, libSystem, and libiconv; `otool`/`nm` find no CFNetwork,
Network, URLSession, socket, connect, getaddrinfo, or curl dependency/symbol.
Four protocol tests and five worker tests cover closed identity framing,
model-file order/path/size/hash bounds, output timestamps/languages, exact
regular-file model sets, symlink rejection, and bounded audio resampling.
The rebuilt 100MB debug App contains all four local workers and the qwen-asr
MIT notice, passes the worker network-symbol gate, and passes explicit deep
ad-hoc signature verification. A later current-source universal App and DMG
passed Developer ID signing, independent notarization/stapling, worker
privacy/network scans, and Gatekeeper; see
`macos-distribution-bundle-2026-09-03.md`.

The five exact source files total 6,539,619,722 bytes and are bound to the
official ASR revision `7278e1e70fe206f11671096ffdd38061171dd6e5` and aligner
revision `c7cbfc2048c462b0d63a45797104fc9db3ad62b7`. The full App-shaped worker
run completed in 114.77 seconds at 20,115,685,376-byte peak RSS with zero swap,
returned 38 chunks and 3,123 words, and produced chunk-text arrays identical to
the independent auto-language CLI baseline. Its response passed the separate
protocol validator. No transcript, title, path, or disagreement was printed.

Two integration defects were found and contained before any ledger wiring:

1. The aligner's 80ms output grid put eight final word ends and seven starts
   just beyond their owning quiet-split chunk. The worker clamps both endpoints
   only to the verified chunk boundary; resulting zero-duration tokens remain
   available for the Rust between-anchor interpolation rule.
2. `qwen-asr` normally writes a derived `qwen-asr-int8.sidecar` startup cache
   next to the BF16 source model. EchoWall forces the library's documented
   `QWEN_ASR_SIDECAR=0` path internally. The successful run left the immutable
   model directories unchanged. The decoder still uses its in-memory INT8
   kernels, so evidence records BF16 source identity and INT8 execution policy
   separately.

The initial implementation incorrectly assumed the library applied detected
language per quiet-split chunk before alignment. In reality, one-shot
`transcribe_full` detects once per invocation and exposes only the final
file-level language; the worker therefore left v1 auto chunk language unset
rather than copying a false value. The later source-grounded v2 policy invokes
the model independently on bounded audio-derived utterance ranges and now
exports truthful per-range labels. Within-turn code switching remains a
required quality gate.

## App candidate-pack and durable-route checkpoint

The App now implements the candidate rather than merely describing it:

- `desktop/model-catalog/qwen-asr-candidate-v1.json` is a separate,
  default-off 6,539,619,722-byte catalog. It pins the two official revisions,
  five exact paths, sizes, and SHA-256 values, and requires an explicit
  development feature plus at least 32 GiB unified memory. It neither enlarges
  nor changes the existing Whisper/FluidAudio/Qwen-summary pack.
- The real catalog install → exact proof → exact removal lifecycle completed in
  217.64 seconds. HTTPS allowlisting, Range resume, partial-file bounds,
  per-file hash/size verification, atomic rename, cancellation, symlink
  rejection, and pack-isolated removal use the same App-owned model manager.
- `qwen_local` is a first-class durable transcription backend. Its ledger
  checkpoint binds the exact model-set digest and survives reopen/retry. An
  explicit takeover supersedes an accepted Miaoji task and rejects a late
  remote transcript, preserving exactly one publication. The emitted archive
  JSON now records `stt_backend=qwen_local`, rather than inheriting the old
  Whisper label.
- The candidate UI exposes install, progress, cancel, removal, candidate
  processing, and takeover. It states that Whisper remains the default. No
  model is implicitly downloaded and the five Qwen files are never required
  for users who do not opt in.

An independent replay against the current App build found an important
non-reproduction of the original 99.04% spike result. On the same authorized
3,123-word response, the aligner's 80 ms grid produced 1,543 zero-duration
words. Safe between-anchor interpolation recovered 1,329, and strict
single-interval point containment recovered another 209. The current merge
therefore assigns 3,079 words and leaves 44 explicit unknown, or **98.59%**
assigned-word coverage:

| Current App merge source | Assigned words |
|---|---:|
| Unique positive overlap | 2,847 |
| Strict single-interval point containment | 209 |
| ≤1 s same-speaker interval bridge | 23 |
| Explicit unknown | 44 |

Of the 44 unknown words, 19 sit between different speakers, 15 span more than
one second, and 10 are possible same-speaker word-neighbour rescues. Those ten
are not assigned because the original rule was calibrated from adjacent
accepted diarization intervals, not word labels. Adding a word-neighbour rule
without human speaker truth would turn a coverage target into guessed labels.

The original 99.04% result remains valid only for the independently generated
spike artifacts and merge implementation used for that table. Its normalized
audio/build artifacts were not retained, so it is not presented as reproduced
by the App. The 98.59% App result is the current product evidence and stays
below the ≥99% promotion gate.

The final current-build test imported the authorized 1,183.68-second recording
under macOS `(deny network*)`, proved both model packs, ran Qwen ASR/alignment,
FluidAudio diarization, Qwen3.8-27B summary, and wrote the verified local
archive through the real Rust ledger. It completed in 398.39 seconds at
21,851,602,944-byte peak RSS with zero swap. The final ledger was
`qwen_local` transcription, `qwen_local` summary, and `local_archive`
publication; TOS and Miaoji checkpoints were absent, the backup locator was
local, and the manifest contained no R2 field. The transcript published 50
canonical segments with three anonymous IDs including `local_unknown`; 31
segments were unresolved because a failed dense-word canonicalization falls
back at the whole Qwen chunk boundary. This is strong execution/privacy proof
and explicit negative quality evidence. It is not a default-parity result.

The 101 MB debug App bundles the four arm64 one-shot workers and passes a fresh
deep ad-hoc signature verification after signing. The later current-source
universal App and independently notarized DMG pass the distribution gate. The
App suite now has 214 tests: 207 pass by default and seven explicit
model/permission/live-provider proofs are ignored. The closed Qwen protocol
passes four tests and its worker passes five; the aligned-word/Whisper protocol passes 11;
the local quality evaluator passes six; and the Swift worker's private-PCM
unlink test passes. The authorized temporary audio, model copies, worker JSON,
ledger, and archive were securely unlinked after aggregate verification.
The same source then passed `x86_64-pc-windows-gnu` compile, an unsigned arm64
iOS simulator archive, and an Android arm64/x86_64 universal debug APK build.
Those targets retain an unavailable/default-off local-Qwen surface and do not
bundle or download the macOS worker/model pack.

## Fabricated mixed-language and short-overlap truth diagnostic

A later aggregate-only run used no personal audio and made no provider call. It
generated a 48.486-second fixture from two macOS system voices with English,
Mandarin, within-turn mixed-language speech, two sub-1.3-second turns, and two
deliberate overlaps. The source script and speaker schedule were exact synthetic
truth, not a Miaoji transcript. The retained public Qwen files matched all five
catalog hashes before use; the cloned test root, audio, requests, and responses
were removed after recording these aggregate results.

Under an OS network deny, the initial App worker completed in 10.65 seconds at
12,645,187,584-byte maximum resident size. It returned two 30-second quiet-
split chunks, 88 aligned words, and 54 zero-duration word timestamps. Against
the exact source script, the frozen evaluator reported 67.29% WER and 82.63%
CER. Inspection of the pinned `qwen-asr` 0.11.0 source found the cause: one-shot
`transcribe_full` detects language once per invocation, while the crate's
`set_multilingual(true)` re-detection contract applies only to its incremental
streaming path. The worker's claim that it already detected per quiet-split
chunk was false; preceding English dragged later Mandarin into translation.

The request now defaults to the versioned
`vad-utterance-30s-language-per-chunk-v2` policy. It finds bounded 600 ms
silence boundaries from samples, caps every range at 30 seconds using a bounded
quiet search, and invokes the same one-shot model independently per range after
clearing the public language-header state. It never infers language from text.
The legacy v1 policy remains accepted and byte-behavior-compatible for durable
job replay. Pure tests prove full sample coverage, long-silence splitting,
short-pause preservation, and the 30-second ceiling.

On the byte-identical audio (same SHA-256), v2 completed under the same network
deny in 28.53 seconds at 11,678,121,984-byte maximum resident size with zero
swap. It returned eight ordered chunks, truthful `en` and `zh` chunk labels,
118 aligned tokens (110 lexical), and preserved the standalone Mandarin turns.
WER fell to 22.43% and CER to 19.69%. The cost is about 2.7x more wall time on
this short alternating fixture; the route still runs faster than real time.
Within-turn code switching remained poor because a single uninterrupted range
still receives one detected language. The evaluator's comparison slot was
populated with exact truth only to exercise the scorer; no Miaoji score is
claimed.

FluidAudio completed in 1.13 seconds on the v2 rerun, found the expected two
speakers, returned the same 12 intervals, and had no zero-confidence interval.
The current Rust merge assigned 91/110 lexical words (82.72%), left 19 explicit
unknown, and improved from zero to three publishable chunks. Three chunks still
fell back for unresolved point runs and two for cross-speaker timing overlap;
the short overlapping voice was not recovered reliably.

This is mixed evidence: v2 fixes a real, source-proven language-state defect,
but the remaining 22.43% WER, 19.69% CER, within-turn mixed-language failure,
82.72% speaker coverage, and short-overlap miss still prohibit promotion. One
synthetic TTS case cannot reject Qwen on natural speech. Qwen remains an
explicit default-off candidate, Whisper remains the default, and timestamp-only
re-embedding rescue must first be calibrated against the independent
human/RTTM matrix. Unknown speaker output is safer than turning this failure
into confident wrong labels.

## Alternative model/runtime exploration

The replacement decision was reopened rather than accepting the first spike:

- The selected FluidAudio revision was re-read after the App reproduction
  missed 99%. Its own current model-choice guide still calls the complete-file
  [Offline VBx pipeline](https://github.com/FluidInference/FluidAudio/blob/main/Documentation/Diarization/GettingStarted.md)
  the best offline-quality option. The same revision also contains LS-EEND,
  which supports up to ten speakers and is stronger on overlap/whispers, but
  the maintainers describe it as more prone to false alarms and less stable
  than Sortformer outside heavy-overlap conditions. LS-EEND is therefore added
  as a human-truth overlap/short-turn challenger, not substituted into the
  default pipeline from README claims alone.
- Pyannote Community-1's official model card exposes an exclusive-speaker
  timeline specifically to simplify reconciliation with imprecise ASR
  timestamps. EchoWall's vendored FluidAudio `PostProcessing.community`
  already sets `exclusiveSegments=true` and trims later overlaps before worker
  output. The 31 unknown canonical chunks are therefore not fixed by merely
  “turning on exclusive diarization”; the next aggregate diagnostic must split
  forced-aligner zero/overlap timing failures from word-to-chunk text
  reconstruction failures before changing the merge. The aggregate-only
  `qwen_merge_stats` tool now reports those three mutually exclusive fallback
  classes plus publishable chunk count and has a focused no-content fixture;
  it will be applied on the next authorized quality run.
- The Apache-2.0 [`soniqo/speech-swift`](https://github.com/soniqo/speech-swift)
  v0.0.27 source at `a2ef1dd159b1c1b3cfbdb41b228437c3c29a44f1` is the most
  concrete Apple acceleration challenger found. It contains native Swift MLX
  Qwen3-ASR plus MLX/Core ML ForcedAligner paths and reports 30.5× realtime for
  Qwen3-ASR-1.7B 8-bit on an M5 Pro; its Core ML INT8 aligner report is 69×
  realtime and 697 MB peak RSS on a 20-second clip. Those are project-reported
  numbers on converted third-party weights, not output parity against the
  official BF16 identities or EchoWall's corpus. Per the quality-first rule,
  record this source as the first post-gate acceleration spike; do not replace
  the CPU oracle or candidate pack yet.
- MOSS-Transcribe-Diarize 0.9B was the strongest architectural alternative
  because it emits transcript, timestamps, and anonymous speakers in one pass.
  On the same authorized first 60 seconds, both official-port q8 and FP16
  outputs scored 31.48% WER, 20.72% CER, two speakers, 89.52% anonymous-speaker
  agreement, and 90.51% comparable reference coverage against Miaoji. q8 took
  11.62 seconds at 1,734,164,480-byte peak RSS; FP16 took 13.92 seconds at
  2,731,933,696 bytes. The q8 Metal route on this M4 Max hit a GPU command
  buffer recovery error and then aborted in ggml cleanup with no stdout.
  Quantization was not the quality cause, and MOSS is rejected for this route.
- Fun-ASR-Nano is attractive for Chinese/dialect robustness and has a native
  GGUF runtime, but its [official repository](https://github.com/QwenAudio/Fun-ASR)
  explicitly says the released checkpoint lacks trained timestamp-head weights
  and that speaker labels come from a composed FSMN-VAD + CAM++ pipeline, not
  the ASR model. GLM-ASR-Nano has promising meeting/noise benchmarks but no
  official timestamp/diarization contract or maintained no-Python Apple runtime
  plus matching forced aligner. Neither
  displaces Qwen for the current bilingual, speaker-attributed deliverable;
  both may enter the frozen corpus as model challengers later.
- OpenASR, CrispASR, transcribe.cpp, upstream llama.cpp/mtmd, and native Swift
  MLX ports show that Qwen itself is not tied to the CPU library. They currently
  introduce some combination of quantized-output risk, experimental audio,
  extra downloader/server linkage, missing timestamps, or an immature source
  surface. They remain acceleration challengers. A challenger must reproduce
  the accepted transcript/alignment metrics using the same model and corpus;
  runtime speed alone cannot choose the product model.

The resulting decision is two-dimensional: Qwen3-ASR-1.7B remains the model
candidate, while `qwen-asr` 0.11.0 CPU is the frozen quality oracle and first
App worker. MLX/Metal/ggml is a later replaceable execution policy, not part of
the model-quality decision.

## Short-turn and remaining-gap rescue

The selected FluidAudio source already contains the necessary primitives:
`OfflineEmbeddingExtractor.embedSpan`, global speaker centroids, PLDA/VBx
features, and frame-level segmentation/activation data. Do not add another
model or export raw embeddings to the Tauri process.

If the natural-speech transcript gate keeps Qwen viable, add a versioned
diarization request field containing bounded forced-aligned speech spans as
timestamps only—never transcript text. After normal global clustering, the
Swift worker should:

1. group aligned word spans that have no accepted diarization coverage;
2. ignore punctuation, invalid spans, overlap regions, and spans crossing an
   already accepted different-speaker boundary;
3. re-embed an eligible exact audio span with the existing WeSpeaker model;
4. compare it with the recording-local global centroids using the existing
   PLDA/centroid representation;
5. accept only when the best score clears both an absolute floor and a
   best-versus-second margin calibrated from high-confidence spans in the same
   recording; otherwise return it unresolved;
6. feed accepted rescue intervals through the same Rust overlap/bridge rules.

For true short turns that were smoothed into a long neighbouring turn, apply a
second pass before final interval merging. Inspect frame-level alternate-
speaker posterior runs around candidate transitions, re-embed the exact span,
and retain a 0.25–1.0-second turn only when the posterior evidence and centroid
margin agree. Textual alternation heuristics such as “uh-huh probably belongs
to the other person” are forbidden.

This rescue stays inside the signed one-shot diarization worker. The worker
still receives only App-owned audio/model identities plus bounded timestamps,
has no network/credentials/listener/persistence, and returns bounded anonymous
intervals. Rust remains the ledger and publication owner.

## Required proof before changing the default

Create human speaker-turn ground truth for at least 5–10 minutes of material
that intentionally includes sub-second backchannels, interruptions, silence,
overlap, low-volume speech, and both speakers' long turns. Miaoji must not be
treated as truth.

The Qwen route may replace Whisper as the default only when all of these pass:

- Qwen transcript quality beats the current Whisper Turbo route on the frozen
  English, Mandarin, mixed-language, overlap, and long-form strata;
- speaker-assigned word coverage is at least 99% per case, with every remainder
  explicitly `local_unknown`;
- short-turn recall for 0.25–2.0-second annotated turns is at least 95%;
- DER/JER and speaker-attributed WER meet the existing non-inferiority margins;
- no material increase in false speaker flips or overlap collapse;
- repeated runs preserve speaker count and materially equivalent boundaries;
- kill, timeout, malformed timestamp, oversized span list, identity mismatch,
  and network-denied tests fail closed without losing the recording;
- the mixed-language route detects language per bounded VAD/alignment chunk,
  because a direct Chinese→English concatenation test caused file-level Qwen
  language detection to omit the Chinese portion.

CPU is the frozen quality baseline. MLX/Metal, quantization, batching, and other
acceleration work begins only after the same-model output passes the quality
matrix; an acceleration candidate must not weaken transcript, alignment, or
speaker metrics.

## Implementation order for the main agent

1. **Complete:** add Qwen3-ASR and ForcedAligner closed one-shot protocols,
   separate exact model catalog, durable ledger model identities, and explicit
   UI; keep Whisper selectable as the default fallback.
2. **Complete as a candidate, not a quality pass:** add the FluidAudio quality
   preset behind a development flag and implement/test the deterministic Rust
   overlap, point-containment, and same-speaker bridge rules. Preserve
   irreducible chunks as `local_unknown`.
3. **Reordered after the exact synthetic failure:** establish natural Mandarin,
   mixed-language, and human/RTTM speaker truth before spending more
   implementation on rescue. The current ASR failure cannot be repaired by a
   diarization post-pass.
4. If Qwen remains viable, extend the diarization protocol with bounded
   timestamp-only candidate speech spans, implement margin-gated span rescue
   using existing FluidAudio embeddings/centroids, then add the frame-level
   short-turn pass. Keep both disabled until the same truth matrix shows no
   overlap collapse or false-speaker regression.
5. Run transcript, alignment, diarization, code-switch, two-hour stability,
   crash, privacy, binary network-linkage, signing, and bundle-size gates.
6. Only after quality passes, benchmark MLX/Metal or other acceleration against
   the CPU baseline and choose the fastest output-preserving runtime.
