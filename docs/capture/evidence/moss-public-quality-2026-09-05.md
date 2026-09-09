# MOSS public-corpus quality evidence — 2026-09-05

## Current checkpoint

- Keep the pre-raw-guard whole-file44 failure frozen. It is not the selected
  App backend and not evidence for the later parser or window policies.
- The new raw-corrected40 artifact set fixes leaked closing-time markers in
  five English cases. English WER/CER is16.71%/11.94% vs Miaoji25.28%/18.30%.
  Mixed exact count and named terms remain below the relative gate.
- All32 quiet-boundary native windows across four90-minute files complete
  with the unchanged100ms tail gate. Six-hour ASR WER/CER is23.21%/16.65%
  vs34.49%/26.45%; window-local speaker IDs are not global identity evidence.
- Preserve injective anchoring v2's added 65 unknown segments as counterevidence.
  Overlap-constrained v3 avoids those additions. Optional producer coalescing
  v2 then merges six adjacent same-ID overlap fragments without changing joined
  transcript bytes/order or clipped interval union. The unchanged v3 mapping
  leaves one 440 ms unknown segment. Long-form relative metrics pass with
  DER/JER 23.28%/33.26%; a separate Rust WER-tokenizer check passes ≥99%
  lexical assignment in every long case. Human review, aligned-word timing,
  Mixed regressions, and full release acceptance remain separate.
- A real public12-minute AAC input completes through the Rust decoder and
  native worker under network denial. The later source-engine route and
  distinct-ID ad-hoc QA App also complete local summary/archive and crash/reopen
  proof. Release-installed UI, whole-App OS-denied reopen and device acceptance
  remain open; those runtime successes do not close the corpus-quality gates.
- A later source-duration audit finds old importer AAC padding in the frozen
  evaluation timelines (four cases over100ms, maximum127.25ms). All44 AAC
  hashes match Miaoji's retained import copies and their effective frame counts
  match the WAVs. The duration producer is corrected without modifying audio,
  old manifests or the100ms gate. A real2.691-second public tail then completes
  natively; this is not a new full quality comparison.

## Decision and scope

MOSS-Transcribe-Diarize 0.9B Q8 through `transcribe.cpp`/Metal is the leading
short-form candidate for the next local transcription implementation. The completed
40-case comparison shows lower WER/CER and DER/JER than Miaoji in each of the
four measured strata. By itself it does not establish long-form quality, a completed
EchoWall integration, or full-local release readiness. New native long-form
evidence below includes a severe regression on `long_form_02`; whole-file
90-minute inference is not ready for promotion. The now-complete pre-raw-guard
44-case baseline (40 CLI + four native cases) fails relative acceptance.

This supersedes the blanket MOSS rejection based on the earlier 60-second
Miaoji-reference probe and different runtime. Preserve that probe as historical
evidence; do not use it to exclude this measured model/runtime combination.
Qwen/ForcedAligner and SpeakerKit remain comparison and replay paths. An
ensemble is not selected automatically: later long-form anchoring is measured,
but blanket short-form fusion has Mandarin/Overlap counterevidence.

All inputs are the existing public AMI/AISHELL-4/ASCEND matrix: 40 recordings,
6.50768 hours, ten each for English, Mandarin, mixed Chinese/English, and
overlapping speech. No personal audio or audio playback was used. Development
checkouts, models, raw outputs, and canonical fixtures stay in ignored
`local-eval/`. Python and CLI experiments are not App runtime dependencies.

## Reproducible identity

The model was open-sourced on 2026-07-09 according to the
[official release record](https://github.com/OpenMOSS/MOSS-Transcribe-Diarize#news).
The tested runtime v0.2.3 was released on 2026-08-30; these are separate model
and runtime release dates.

- Runtime: `handy-computer/transcribe.cpp` v0.2.3,
  commit `63a44d9239d610b3908e8a66b384924cd4a77217`; MIT.
- Model: `handy-computer/moss-transcribe-diarize-gguf`, revision
  `6fdfa33aed776bbb0ac11a1a9835634fe6d75dd7`,
  `MOSS-Transcribe-Diarize-Q8_0.gguf`; 986,899,616 bytes;
  SHA-256 `64ec654dc6ffcfdfe180422dffce1d33422b0c30959b7edfd131bad77ee35039`.
- Model license: Apache-2.0. Primary references are the
  [upstream model card](https://huggingface.co/OpenMOSS-Team/MOSS-Transcribe-Diarize),
  [pinned runtime](https://github.com/handy-computer/transcribe.cpp/tree/63a44d9239d610b3908e8a66b384924cd4a77217),
  and [pinned model pack](https://huggingface.co/handy-computer/moss-transcribe-diarize-gguf/tree/6fdfa33aed776bbb0ac11a1a9835634fe6d75dd7).
- Inference: whole-file 16 kHz mono PCM; `--backend metal --diarize
  --timestamps segment --batch-jsonl`; no known-speaker-count hint, transcript
  rewriting, or per-case parameter selection.
- The runtime's 37 CTest checks passed. Its development binary links Metal
  and has no observed networking imports, but embedded shader source includes
  URL strings. EchoWall packaging/privacy gates have not passed for MOSS.

## Measured results

All error values are percentages; lower is better. Speaker counts compare
against independent corpus annotations, not against Miaoji labels.

| Stratum | MOSS WER / CER | Miaoji WER / CER | MOSS DER / JER | Miaoji DER / JER | MOSS exact count | Miaoji exact count |
|---|---:|---:|---:|---:|---:|---:|
| English | 21.96 / 15.33 | 25.28 / 18.30 | 20.59 / 26.11 | 37.26 / 52.22 | 8/10 | 3/10 |
| Mandarin | 15.61 / 15.61 | 16.38 / 16.41 | 9.58 / 20.25 | 20.35 / 42.52 | 6/10 | 1/10 |
| Mixed | 10.26 / 8.02 | 11.62 / 9.30 | 13.93 / 15.50 | 20.26 / 24.34 | 4/10 | 8/10 |
| Overlap | 36.32 / 30.06 | 47.01 / 39.86 | 45.59 / 49.05 | 70.82 / 81.19 | 7/10 | 0/10 |

MOSS exact count totals 25/40 (62.5%); Miaoji totals 12/40 (30%). The aggregate
does not excuse the mixed-stratum regression, 4/10 versus 8/10. Speaker
attribution error and exact speaker count are different measurements: lower
DER/JER does not imply that every participant is discovered or counted once.
The previous Qwen/SpeakerKit route also counted 25/40 correctly on these same
four strata (27/44 including its completed long-form cases).

Automatic mixed named-term recall is 90% versus Miaoji's 100%, exceeding the
allowed five-point deficit; these term targets still require human confirmation.
Short-turn recall and two-reviewer blinded summary quality remain unmeasured.
The other reported automatic metrics meet their existing margins in these four
strata. These are corpus results, not a promise for every recording.

The 30 Mandarin/mixed/overlap files took 702.57 seconds; ten English files took
392.60 seconds. Short-file RSS observations were approximately 5.1 GiB. These
batch observations are not a minimum-memory or long-form performance guarantee.

## Output adaptation and limits of proof

The 40 raw records contain 4,607 segments and no reported inference errors.
The diagnostic conversion preserves segment text and order. Four same-speaker
overlapping segments were relabeled `local_unknown`; one final segment's end
was clipped by 84 ms to the verified audio duration. Cross-speaker overlaps
remain in the evaluator input. The scores above include these adaptations.

Consequently these are not scores for an untouched production response. The
new bounded `desktop/local-moss-protocol` adapter now explicitly validates and
accounts for these cases. Its opt-in replay reproduced all 40 canonical
outputs exactly: 4,607 segments, four unknown adaptations, and 84 ms clipped,
without inference or text changes. Six unit tests plus the replay pass. Do not silently
weaken the current non-overlap worker protocol or drop troublesome segments.
Joint speaker-aware output must still meet the coverage and human short-turn
requirements; a model-generated speaker ID is not a calibrated confidence score.

Raw batches: `local-eval/matrix/outputs/moss-transcribe-q8-mmo/results.jsonl`
and `local-eval/matrix/outputs/moss-transcribe-q8-english/results.jsonl`.
Canonical comparison: `local-eval/matrix/manifest-moss-transcribe-q8-40-hybrid.json`.
That manifest's four **long_form rows are Qwen/SpeakerKit placeholders**, not
MOSS output. Never quote its long-form score or call it a 44-file MOSS pass.
Reproduce the four measured aggregates from the repository root:

```sh
desktop/local-quality-eval/target/release/echowall-local-quality-eval \
  local-eval/matrix/manifest-moss-transcribe-q8-40-hybrid.json \
  | jq '{status, strata: (.strata | del(.long_form))}'
```

The legacy grader still returns `fail`, including its absolute 95%-count gate.
Do not relabel old reports after changing the acceptance policy below.

## Native one-shot worker proof

`desktop/moss-worker` now builds a standalone arm64 Rust executable against the
same pinned transcribe.cpp Rust binding and Metal runtime. It is not linked
into Tauri and does not call a Python process, CLI transcriber, provider API,
or downloader. Model and audio identities are verified from App-shaped local
paths; the joint response is accepted only after the native session completes
without abort/truncation and passes the bounded protocol.

The first network-denied native test used public `english_01` (720,064 ms).
It completed in 43.805 seconds with 184 segments, no unknown adaptation or end
clipping, and zero stderr bytes. Canonical text, times, and speaker values
exactly match the previous CLI output (JSON key order is immaterial). The
worker was run beneath `sandbox-exec` with `(deny network*)` and a cleared
environment. It requested Metal and verified the model's classified device
kind was `metal`; no CPU fallback was accepted. A live RSS sample was about
5.03 GiB, not a measured peak.

Checkpoint:
`local-eval/matrix/outputs/moss-native-worker-v1/english_01-1788600435403454000/`.
Worker SHA-256:
`8c60b445dbc59d6fff1760807d82cf5eafd574605b0ec047489c99972846b83d`.
Each run retains request, raw response, canonical transcript, bounded stderr,
and a terminal aggregate report in its own directory; no prior case is
overwritten. The test enforces a 30-minute child deadline and kills/reaps the
child before cleaning its temporary App-shaped tree on early errors.

Two load-boundary problems were caught before success: macOS `/dev/fd/N`
reopens share the file cursor, so a magic-byte sniff consumed the next GGUF
open's header; and the native backend display name is not its lowercase kind.
Inference now uses a new private 0700 directory containing a read-only,
rehashed model snapshot copied from the verified descriptor, and checks the
typed device kind. Regression tests cover independent snapshot cursors,
source replacement, permissions, and cleanup. Six worker unit tests and
Clippy with warnings denied pass.

This is a **candidate worker proof**, not App integration or packaging proof.
The initial proof used a 16 kHz mono-only decoder. The App
must implement model catalog/download/remove, launcher deadlines/cancellation,
crash-leftover scratch recovery, durable ledger/archive integration, signing,
and bundle verification before promotion. Model snapshots require temporary
disk space; the current test is not a resource-budget pass for all machines.
No current result changes the selected App backend.

Reproduce a single public case from `desktop/moss-worker`:

```sh
ECHOWALL_MOSS_NATIVE_CONFIRM=public-corpus-native-worker-authorized \
ECHOWALL_MOSS_NATIVE_CASE=english_01 \
cargo test --locked --release --test public_native -- --ignored --nocapture
```

Only IDs in the existing public manifest are accepted. The first long-form
result and subsequent decoder changes are recorded below; inspect each
attempt's own worker hash rather than treating every build as identical.

### First completed native long-form case

Public `long_form_01` (5,400,064 ms, five truth speakers) completed under
network denial in 1,179.631 seconds (19 min 40 s; RTF approximately 0.218).
It produced 932 segments, one unknown adaptation, no end clipping, and zero
stderr bytes. Worker hash is the same `8c60b445…b83d` as the first short pilot.
The exact checkpoint is
`local-eval/matrix/outputs/moss-native-worker-v1/long_form_01-1788600506137770000/`.

| Metric | Native MOSS | Miaoji |
|---|---:|---:|
| WER / CER | 14.77% / 8.91% | 23.78% / 16.34% |
| DER / JER | 23.73% / 41.10% | 39.11% / 51.09% |
| Exact speaker count | 5, correct | 5, correct |
| Reference speech timestamp coverage | 96.76% | 82.11% |

The pure `manifest-moss-native-41.json` combines the original 40 CLI cases
with this one native long-form output, not Qwen placeholders. It still reports
`insufficient_corpus`: one 90-minute case cannot satisfy the six-hour long-form
requirement. Mixed count/term failures and human/summary gates remain. The
stopped older batch remains incomplete historical evidence; this new result
does not retroactively make it a successful run.

Live `sample` reported physical footprint peak `20.9G`; sampled system swap
stayed at `103.56M`. The sampled stack showed active Metal command submission
and waits. These observations describe this machine/run, not a minimum-memory
promise. Remaining long-form cases use individual checkpoints without
concurrent GPU inference.

### Second long-form case — quality regression, do not promote

`long_form_02` completes the native protocol in 551.251 seconds (9 min 11 s),
with 432 segments, zero unknowns/clips/stderr, and worker hash `d6e87d54…c0c38`.
Its quality is substantially worse: WER/CER **60.64%/55.57%** versus Miaoji's
33.69%/25.86%; DER/JER **41.34%/79.59%** versus 36.68%/52.03%. It predicts
three speakers for eight truth speakers (Miaoji predicts six). Returned times
reach the file's end and cover 97.35% of reference speech, showing why native
EOS/valid timestamps are not proof that the text or attribution is correct.

Checkpoint:
`local-eval/matrix/outputs/moss-native-worker-v1/long_form_02-1788601861255147000/`.
The two completed long cases aggregate to MOSS WER/CER **36.47%/31.24%**
versus Miaoji **28.47%/20.89%**, DER/JER **32.80%/64.79%** versus
37.85%/51.67%, with exact count 1/2 for both. The pure 42-case manifest remains
insufficient (three of the required six long-form hours), and its measured
long-form WER/CER/JER are regressions. A protocol test's `state:pass` is not a
quality pass. Preserve this failure; do not cite only the successful first case.

`long_form_03` was next launched. Complete the whole-file baseline before
comparing a fixed-window long-audio strategy with explicit cross-window
speaker reconciliation. Windowing is a hypothesis, not an already-selected
solution; any MOSS/SpeakerKit combination must be evaluated independently.

### Bounded general audio conversion

The decoder now accepts 8–192 kHz, up to eight channels, with finite-sample
validation, full-scale normalization, mono averaging, and pinned Rubato 0.16.2
FFT anti-aliasing conversion. Source/decoded packet sizes, source frames,
duration, output growth, filter delay removal, and final flush are bounded.
16 kHz mono PCM bypasses filtering and remains bit-identical.

Thirteen unit tests plus two subprocess boundary tests pass with warnings denied.
Pure-memory tests verify exact resampled length, chunk independence, impulse
time-origin preservation, speech-band amplitude, and above-Nyquist attenuation;
no waveform was played. The initial sine test's float32 phase arithmetic
introduced noise; using float64 to construct the test signal fixed the fixture
without relaxing its attenuation threshold.

An opt-in, synthetic-file-only AAC/MP3 encode/decode fixture now passes too.
It caught missing MP4 edit-list trimming: Symphonia 0.6.1 AAC ignores the
gapless option and its MP4 path does not apply movie edits. The worker now
uses explicit packet trimming for codecs that expose it and Mozilla's pinned
[mp4parse 0.17.0 timing metadata](https://docs.rs/mp4parse/0.17.0/mp4parse/struct.Track.html)
for simple MP4 priming/padding. A bounded box-shape guard rejects complex,
looped, negative-time, or non-unit-rate edits that the permissive parser would
otherwise ignore. Truncated media cannot be filled with invented samples.
The fixture verifies duration within 16 ms and speech-band signal scale; its
FFmpeg invocation exists only under development test configuration and never
plays audio. A targeted review also found that duplicate `edts`/`elst` siblings
could replace the timeline silently; a red/green regression now proves those
forms are rejected before the metadata parser loses the earlier value.
Public AAC inference and App import acceptance still remain.

After this change, a fresh network-denied `english_01` worker run again
returned 184 segments in 50.315 seconds with zero stderr; every canonical value
matches the prior CLI output. Worker SHA-256:
`d6e87d5425c00572541a74468999715a4f5c9ba2c678db4ecb5747b0ab6c0c38`.
The harness now freezes a private executable copy and hashes that exact copy
before launch, so concurrent rebuilds cannot misattribute the running binary.

### Third completed long-form case

`long_form_03` completes the native protocol in 561.766 seconds (9 min 22 s),
with 425 segments, four predicted speakers for eight truth speakers, and zero
unknowns/clips/stderr. Its output reaches 5,400,000 ms. Checkpoint:
`local-eval/matrix/outputs/moss-native-worker-v1/long_form_03-1788602775617882000/`;
worker SHA-256 `74ff7002b09f6c53483db7bf472016c45483951457469a59c20cfde995cb4adc`.

Across the first three long cases, MOSS WER/CER is **48.61%/43.94%** versus
Miaoji **32.20%/24.35%**; DER/JER is **44.26%/72.88%** versus
38.30%/48.82%. Exact count is 1/3 for both, and named-term recall is 88.89%
versus 100%. The native protocol succeeds but whole-file quality does not.
`manifest-moss-native-43.json` remains a retained partial report at4.5 hours.

### Frozen44-case whole-file result

The fourth90-minute native case completed in1,603.481 seconds (26 min43 s),
with1,767 segments,31 unknown adaptations, zero end clips/stderr. Checkpoint:
`local-eval/matrix/outputs/moss-native-worker-v1/long_form_04-1788603484439887000/`;
worker SHA-256 `ffc2f826087be37a9eb185f9fb74d06f62b5b40591434e7cb1c5b682b1a771b9`.

`manifest-moss-native-44.json` now contains all40 earlier CLI cases and four
native long cases. All five strata meet corpus size/duration requirements;
the relative-policy report is **fail**, not merely insufficient.

| Long-form aggregate,4 cases /6 hours | MOSS | Miaoji |
|---|---:|---:|
| WER / CER |43.73% /39.04%|34.49% /26.45%|
| DER / JER |41.32% /67.30%|41.43% /49.95%|
| Exact count |1/4|1/4|
| Named-term recall |91.67%|100%|

Long-form failures are WER, CER, JER, and named-term recall. Mixed still fails
relative count and named-term recall. English, Mandarin, and Overlap retain
their earlier passing automatic relative comparisons. These results remain
separate from human short-turn/summary and App release acceptance.

### Raw timestamp provenance correction

Source inspection found that upstream `arch/moss/diarize.cpp` fills unknown
start/end times, including extending the final turn to the file's end. The
second/third long outputs include single segments spanning approximately44/55
minutes. Their raw model strings were not retained, so the exact contribution
of that repair to those cases is unproven; do not attribute their high
timestamp coverage to verified model timing. Their text metrics remain valid
as measurements of the retained output, but those old runs do **not** prove
the new raw-boundary integrity check.

The worker now uses its own bounded parser over `Transcript.raw_text` under
`explicit-model-boundaries-v1`. It accepts explicit end/start and shared
boundaries, preserves genuine overlap and literal text, handles inter-turn
whitespace, and fails on missing start/end/speaker metadata. It has no audio
duration argument from which it could invent a final boundary. Five new unit
tests cover this boundary; worker totals are now18 unit +2 subprocess tests,
plus the sample-exact window test and opt-in synthetic codec test.

A fresh network-denied `english_01` run passes this raw policy in44.851 s,
retains184 segments with zero stderr/unknowns/clips, and exactly matches all
prior canonical values. Checkpoint `english_01-1788605540337868000`; worker
SHA-256 `1d24b77b3b7a305d1ceadbc4bf2a8d26802fa8b96de05e15ca3c60cc37695a2d`.
New harness reports include `raw_timing_policy`; absence in old reports must
not be silently upgraded into raw-timing proof.

The official [model card](https://huggingface.co/OpenMOSS-Team/MOSS-Transcribe-Diarize)
describes90-minute single-pass support, while the pinned port's published
numerical/ASR validation is on short JFK/LibriSpeech inputs. Model behavior,
quantization, and long-context port behavior are not yet separated causes.
The next controlled experiment uses fixed12-minute PCM windows on the same
failing `long_form_02`, with the new raw policy and no oracle speaker count.
Its initial scope is ASR only; per-window speaker slots are not global speaker
identities. Cross-window attribution must be separately verified.

## Long-form and alternative-counter state

The four 90-minute MOSS inputs were launched as one batch. Live sampling during
that run showed Metal computation and approximately 20.8 GiB peak physical
footprint; observed system swap stayed at 103.56 MiB during sampled intervals.
No completed recording result was retained. On the subsequent status check,
the process and session handle were absent and the JSONL contained only a load
record. The stopping cause is unknown; this is an incomplete run, not a
successful long-form test or a verified model crash. The 3D-Speaker pilot also
has no retained RTTM result. Revalidate both before any new experiment.

SpeakerKit's full 44-case 0.45/0.75 threshold diagnostics did not solve counting:
0.45 stayed at 27/44; 0.75 fell to 26/44. Retain 0.6 as the existing replay
preset; further blind threshold sweeps have no demonstrated benefit.

## Acceptance clarification and next execution

AX confirmed that the product goal is a deliverable comparable to Miaoji.
The agent-added absolute 95% exact-count target is a separate stretch target,
not an observed Miaoji capability or the definition of relative parity. The
Release 1 comparison requires exact-count accuracy at least as high as Miaoji
in **each** stratum, alongside unchanged ASR, DER/JER, coverage, term, summary,
runtime, privacy, and physical-device gates. Mixed count therefore remains open.
The separate 95% human short-turn recall requirement is unchanged.

The grader now exposes `--policy miaoji-relative-v2`, preserves default legacy
output byte-for-byte, and passes eleven library plus one CLI tests. The new
`local-eval/matrix/manifest-moss-transcribe-q8-40.json` excludes Qwen long-form
placeholders and reports `insufficient_corpus`; Mixed retains relative-count
and named-term failures. Its scope is transcript metrics only.

Next: retain the completed whole-file failure and raw-timing boundary evidence,
evaluate fixed-window ASR and cross-window attribution, resolve
mixed count and human term/short-turn/summary review; then integrate the winning
native runtime into the App's durable ledger and rerun packaging/physical proof.
Qwen MLX porting is an alternative if MOSS fails; it is not a prerequisite to
finishing a better MOSS implementation. No current result changes the App's
selected backend or proves end-to-end offline MOSS archive completion.

## Raw-corrected40 replay

`desktop/local-moss-protocol/src/raw.rs` is now the shared parser used by the
worker and the independent replay. All40 retained CLI records include raw
strings. `tests/public_raw_adapter.rs` creates new artifacts only, validates
bounded paths/inputs, preserves reference metadata, and refuses conflicting
existing bytes. Repeated generation verified42 artifacts with zero rewrites.

The corrected set has4,607 segments, four unknown/conflicting adaptations,
84ms tail clipping, and841 overlapping segment pairs. Only
`english_02/03/04/06/08` change:851 end-time fields and853 text fields lose
misparsed metadata, not speech. English metrics become WER16.71%, CER11.94%,
DER21.89%, JER26.51%, exact count8/10; Miaoji is25.28%,18.30%,37.26%,52.22%,
3/10 respectively. Other short strata retain their previous values.

Artifacts: `manifest-moss-raw-explicit-40.json` and
`outputs/moss-raw-explicit-v1/` beneath `local-eval/matrix/`. The generation
record hashes retained raw inputs and actual public WAVs. Old CLI rows do not
attest their model/binary identity, so expected fixture identity is explicitly
distinguished from cryptographic attestation. No new inference was used.

## Quiet-boundary long-audio experiment

The initial fixed12-minute experiment stopped at `long_form_02` window05:
the final explicit turn was719,690–720,110ms against a720,000ms window. A
separate public CLI diagnostic reproduced the110ms overrun. The100ms limit
was not relaxed. The cut's300ms RMS was0.008875 full scale; a nearby quiet
region measured0.000080, motivating a deterministic cut-boundary experiment.

`quiet12m-energy300-back5s-v1` limits windows to12 minutes. It searches the
last5 seconds for the lowest300ms RMS, chooses the latest tied minimum, and
uses it only below164/32768 full scale; otherwise it retains the fixed cut.
Every sample belongs to exactly one contiguous window. The final window uses
real PCM duration, not the original AAC container's extra64ms. Tests cover
sample-exact splitting, no overwrite/padding, deterministic quiet selection,
and fixed-cut fallback when no quiet region exists.

All32 native windows completed, with the same worker hash
`4a317dd821b08048ef80d89a515073c75ab737319378cbd6119a8e0a8191a500`,
raw-boundary validation, network denial, and no playback. The four cases
contain5,594 segments; inference-time sums are343.852,318.140,392.918,353.026s
(excluding harness preparation). `assemble_moss_windows.mjs` checks contiguous
coverage, unique window IDs, source/worker hashes, and unchanged local text/time
before adding global time offsets. It publishes ASR-only artifacts; it does
not claim per-window speaker slots are global identities.

Six-hour ASR WER/CER is23.21%/16.65% versus Miaoji34.49%/26.45%.
`long_form_02` alone is19.75%/12.94% vs33.69%/25.86%. This combines bounded
windowing with raw-marker correction; it is not an isolated causal estimate
of window size alone.

Artifacts: `outputs/moss-quiet12-asr-long{01,02,03,04}-v1/` and
`manifest-moss-quiet12-asr-long4-v1.json`. Do not report the latter's speaker
metrics as global identity quality. Reproduce assembly with:

```sh
node local-eval/assemble_moss_windows.mjs long_form_01 long_form_02 long_form_03 long_form_04
```

## Identity-anchor experiments and open coverage

V1 uses a fixed duration-overlap vote from each window's MOSS slot to the
existing whole-file SpeakerKit anonymous slots. It never reads truth or edits
text/time/order. It improves the long02 pilot (DER17.21%, JER33.10%) but is
not safe as a blanket policy: short Mandarin and overlap regress, and
`overlap_01`/`long_form_04` encounter overlapping unknowns after merges.
Those cases fail explicitly; no fake speaker or dropped segment was emitted.

V2 instead maximizes total overlap under injective per-window assignment to
existing global slots, using bounded exact subset DP. Zero-support/dummy
assignments stay unknown. Five tests include exhaustive512 binary3x3 matrices.
All four long cases map; their unchanged relative-policy stratum passes:

| Metric | Quiet MOSS + injective anchors | Miaoji |
|---|---:|---:|
| WER / CER |23.21% /16.65%|34.49% /26.45%|
| DER / JER |23.55% /33.59%|41.43% /49.95%|
| Exact count |2/4|1/4|
| Named-term recall |100%|100%|
| Reference timestamp coverage |94.03%|76.10%|

The tradeoff is65 new unknown segments:72 total /238,330ms, with no collisions.
An English alphanumeric token diagnostic finds assigned ratios99.335%,98.870%,
98.725%,98.651% (all four outputs contain zero Han characters). This is not
≥99% per case, nor human short-turn validation. A graph-constrained v3 is being
tested to preserve overlapping local distinctions without unnecessarily
forcing distinct identities for non-overlapping slots; no gate is weakened.

V1 artifacts remain in `outputs/moss-speakerkit-identity-map-v1/` and
`outputs/moss-speakerkit-identity-map-long4-v1/`. V2 lives in
`outputs/moss-speakerkit-identity-map-long4-v2/` with
`manifest-moss-quiet12-speakerkit-long4-v2.json`. These are development replay
tools, not Node dependencies in the App processing route.

## Real AAC native pilot

Public `english_01` was also adopted directly from its original `.m4a`, not
the pre-decoded WAV. The Rust/native route completed in43.186s with184 segments
and zero unknowns/clips/stderr under network denial. WER/CER17.32%/12.29%,
DER/JER22.82%/26.59%, exact count correct; Miaoji for this same single case is
27.40%/19.20%,36.71%/66.74%, count incorrect. The AAC and WAV native results
differ in two text fields and two time fields, not speaker labels, so do not
claim bit-identical codec output. This single pilot is not a full corpus gate.

Checkpoint: `english_01-m4a-1788611885948676000` under native outputs, same
`4a317dd8…1a500` worker hash. Reproduce with
`ECHOWALL_MOSS_NATIVE_SOURCE=m4a` and the existing public native confirmation.

## Overlap-constrained mapping and producer coalescing

The fixed development mapper
`local-eval/reconcile_moss_speakers_overlap_constrained.mjs` (v3) maximizes
acoustic duration overlap while forbidding a shared global anchor only for
window-local slots that actually overlap. Non-overlapping local slots may
reuse an existing acoustic anchor. Explicit unknown spans remain fixed;
zero support cannot become a guessed speaker. The bounded exact solver uses
at most 16 slots and 1,000,000 search nodes per connected component, with
deterministic lexical ties. It consumes no transcript words, reference labels,
named terms, or evaluation scores. Its six tests include 4,096 exhaustive
small-graph comparisons. Raw v3 adds no unknowns, but the producer's six
unknown fragments still leave `long_form_04` below the lexical coverage gate.

Inspection of retained native responses found that five of that file's six
unknown fragments were immediately adjacent, same-model-ID overlaps. One
52.56-second turn had become entirely unknown because its beginning overlapped
the preceding same-speaker turn by 460 ms. The new optional shared Rust policy
`joint-adjacent-union-unknown-tail100-v2` joins only such adjacent known-ID
fragments. It keeps every source text in order separated by one space and
preserves their time-interval union. It never bridges a positive gap, merges
across another speaker, deduplicates words, changes IDs, or raises the existing
100 ms tail limit. Legacy `joint-overlap-unknown-tail100-v1` stays unchanged.

The opt-in `public_coalesced_windows` replay validates all 32 original
request/response/canonical receipts and hashes, then changes only the explicit
adaptation-policy identifier for re-adaptation. Across the four files it
changes 5,594 source segments into 5,588 canonical segments, with six merges
(one in `long_form_02`, five in `long_form_04`). Joined transcript bytes/order
and clipped interval unions are exact per window and for the assembled files.
The one remaining unknown is non-adjacent (440 ms); it is deliberately retained.
This is producer canonicalization evidence, not fresh inference or model
attestation. A focused review found no concrete invariant defect.

One new caller then imports the unchanged v3 mapper. All four long files map
successfully with no new unknowns:

| Six-hour long-form metric | Coalesced MOSS + v3 anchors | Miaoji |
|---|---:|---:|
| WER / CER | 23.21% / 16.65% | 34.49% / 26.45% |
| DER / JER | 23.28% / 33.26% | 41.43% / 49.95% |
| Exact speaker count | 2/4 | 1/4 |
| Named-term recall | 100% | 100% |
| Timestamp coverage | 94.03% | 76.10% |

The long-form relative stratum is sufficient and passes. The report as a whole
is `insufficient_corpus` because it contains only that stratum. WER/CER, term
recall and timestamp coverage are unchanged from the pre-coalescing window
replay. Two existing adjacent duplicate pairs remain; their denominator changes
from 5,590 to 5,584. Human speaker/short-turn adjudication and summary review
are still absent; the Mixed count/term regressions are not resolved by this run.

Replay artifacts are `outputs/moss-quiet12-coalesced-v2/`,
`manifest-moss-quiet12-coalesced-v2-long4.json` (ASR only), and
`manifest-moss-quiet12-coalesced-v2-speakerkit-v3-long4.json` (mapped).
`comparison.json` records source hashes and the earlier ASCII-token diagnostic.
Repeat replay verifies 42 producer artifacts and ten mapped artifacts with no
writes. These development helpers are not App runtime dependencies.

## Formal lexical assignment check without report migration

The separate Rust `echowall-speaker-coverage` executable now measures the
existing ≥99% per-case lexical-assignment requirement with the exact WER
Unicode/Han tokenizer. Unknown segments stay in the denominator, punctuation
does not create words, zero-word output fails, and a high stratum average
cannot hide a failing case. The result is aggregate-only and explicitly scoped
to attribution presence, not speaker correctness, ASR completeness, aligned
word timestamps, or human short-turn recall. The legacy and relative transcript
reports remain byte-identical to their pre-change hashes.

All four long cases pass: 61,787/61,788 tokens are assigned, minimum per-case
99.992937%. The corrected short 40 also pass this *separate* check: per-stratum
minimum case fractions are English 100%, Mandarin 99.425676%, Mixed 100%,
Overlap 99.6875%. Mixed exact count and terms still fail the other grader.
Neither partial manifest becomes a full release pass. Fifteen library tests,
the existing CLI policy test, formatting, clippy and release build pass.

`local-eval/verify_moss_coverage.mjs` checks frozen legacy/relative report hashes
and creates-or-verifies `speaker-coverage-wer-v1.json` in the corrected40 and
coalesced32 output directories. No audio, model inference, or new provider call
is involved.

## Shared Rust planning and speaker mapping

`desktop/local-moss-protocol/src/speakers.rs` and its index/solver modules now
implement the fixed v3 mapping as a pure timing-only API. Known anonymous IDs
are bounded ASCII; unknown is `None`. Indexed anchor-duration queries avoid
quadratic full-transcript scans. Each component has the unchanged1,000,000-node
search bound; the whole call additionally has16,000,000 deterministic work
units and cancellation polling. Failure/cancel returns no partial mapping.
Nine regression tests include exhaustive small graphs, actual overlap/fixed
unknown constraints, malformed inputs, limits/cancel, and20,000 intervals.

The read-only `public_speaker_mapping` replay matches all5,588 labels across32
windows, plus support matrices, conflict graphs, constraints, components and
820 search nodes. It uses597,033 work units and retains one unknown. It reads
the immutable coalesced inputs and JavaScript v3 outputs, writes nothing, and
does not inspect transcript words for mapping. Parent readback confirms parity.

`src/windows.rs` implements the measured quiet policy over a caller-supplied
verified16kHz S16 mono source. Only84,800 samples are buffered/read per interior
cut, not the full recording; integer energy, latest-tie selection, threshold
and fixed-cut fallback match the original experiment. Plans preserve exact
sample coordinates including sub-millisecond final tails, never container
padding. Three tests exercise contiguity, tiny tails, extrema, threshold/ties,
bounded reads and cancel/failure. `moss-worker/tests/public_window_plan.rs`
verifies all32 boundaries against both the original independent helper and
retained native receipts, with public-source SHA-256 before/after and zero
artifact writes. Short/tiny-tail *inference quality* is not proved by planning.

Adding source modules legitimately changes the current source hash. The new
coalesced read-only verifier therefore compares41 canonical/receipt artifacts
byte-for-byte plus every stable field of the42nd summary. It preserves and
reports historical and current verifier/adapter hashes separately and does not
re-attest historical code. A separate audit finds all56 retained artifact
hashes/mtimes unchanged. The original generation mode remains hash-strict;
use the read-only mode to verify existing evidence with later source builds.

```sh
ECHOWALL_MOSS_COALESCED_VERIFY_CONFIRM=public-coalesced-window-verification-authorized \
cargo test --manifest-path desktop/local-moss-protocol/Cargo.toml \
  --test public_coalesced_windows retained_coalesced_windows_verify_read_only_with_source_drift \
  -- --ignored --nocapture

ECHOWALL_MOSS_SPEAKER_MAPPING_REPLAY_CONFIRM=public-speaker-mapping-replay-authorized \
cargo test --manifest-path desktop/local-moss-protocol/Cargo.toml \
  --test public_speaker_mapping -- --ignored --nocapture

ECHOWALL_MOSS_WINDOW_REPLAY_CONFIRM=public-window-plan-replay-authorized \
cargo test --manifest-path desktop/moss-worker/Cargo.toml \
  --test public_window_plan -- --ignored --nocapture
```

These are shared domain pieces, not App integration. A focused consumer review
confirms `LocalWhisperResponse` rejects genuine cross-speaker overlap; its
contract must stay unchanged. MOSS needs dedicated per-window checkpoints and
finalization, with one local-operation owner, generation/hash binding,
crash/retry reuse, and cancel/late-response fencing. Existing ordered canonical
summary/archive consumers can then be reused without flattening overlap. Archive
IDs are limited to32 ASCII bytes, stricter than the standalone mapper's64-byte
input boundary; App finalization must enforce its consumer contract.

## Shared decoder extraction for App preparation

The decoder and its MP4/resampling modules now live in `desktop/local-audio`.
The worker delegates its verified file to this Rust-only crate; the App has
the same path dependency for upcoming preparation. No transcribe.cpp/model
inference dependency enters Tauri. Input path/hash ownership stays with each
caller. At the initial extraction checkpoint, the MP4 and resampler files retained SHA-256
`64f7346d1890b6e4f027162e5f54858dca69431aef378dbdb28d13a4d260b5d1`
and `91178a4efd29e28b8e1c34e397926a2156c74e3801585a156c415265d305c65e`;
the decoder only loses its MOSS-request convenience wrapper and exposes the
existing `(File, format_hint, duration_ms)` function. Ten audio tests, the
separate explicit synthetic AAC/MP3 test, worker boundary/tests and clippy pass.
There was no fresh inference claim at that extraction checkpoint; the later
short-tail proof below has its own exact worker identity.
At that extraction checkpoint, App PCM materialization and durable window
execution were not yet complete. The later runtime checkpoint below supersedes
that implementation status without upgrading the earlier proof's scope.

## App format boundary and immutable sidecars

`processing/local_moss.rs` now owns immutable validated plan/request and
response-bundle types. Original audio and prepared windows have separate byte
identities. Plans enforce source/PCM duration agreement within100ms, retained
final sample frames with explicit ceil-ms representation, nonfinal715–720s
windows on the measured10ms grid, unique recording-owned window paths, and
no original-source alias. Responses bind plan/request/body hashes and ordered
complete window sets. Finalization uses the MOSS adapter and timing-only mapper
directly, never the non-overlap Whisper response type. Output remains at most
10,000 entries/16MiB, with archive-compatible anonymous IDs and explicit language.

Eleven synthetic tests pass, including unchanged overlapping canonical
timestamps/text order through the actual existing summary reader and archive
formatter. Negative tests reject partial/misbound/reordered output and a short
PCM declaration for a long source. This is App code-boundary evidence, not an
executed App job or native model attestation.

`processing/moss_artifacts.rs` creates private recording/generation-namespaced
sidecars. Publication is create-only with file/directory sync and exact
hash/size readback; a conflicting result never replaces an existing result.
References bind recording, generation, artifact kind and content. Reads are
bounded; symbolic links and nonprivate Unix directories fail. Recording leases
exclude another process across generations and are released by the OS after
crash. A focused review found that the first `write` API made locking optional;
it now requires an unforgeable canonical-root/recording/generation-bound lease.
Wrong recording, generation or App root fails before publication. Five tests
pass, including separate-process exclusion and child exit without Rust Drop.

Each lease acquisition now also gets a fresh session UUID for the next durable
claim layer. A lease alone is not a persisted late-response fence. At that
checkpoint the engine was explicitly unavailable pending claim/runtime wiring.
The later sections implement those layers; neither uses remote fallback nor
the legacy overlap-losing contract.

## Durable MOSS ledger boundary

`processing/moss_ledger.rs` now stores sealed, validated metadata while bodies
remain in immutable sidecars. The exact source projection is
`inbox/{recording_id}/{ledger.normalized.relative_path}`; accepting both package-
and App-relative names would create ambiguity, so the projection is strict.
Plan/request/body hashes, recording/generation identity and completed ordered
prefixes are revalidated. A reference alone is insufficient: each completed
sidecar is read back and validated before the ledger advances.

Claims persist a fresh token and OS-owner session before any dispatch. Duplicate
claims under the same owner fail; a fresh exclusive owner may replace a dead
claim while preserving completed windows. Window/anchor/finalization/failure
checkpoints require the exact active claim and `LocalTranscribing`; stale,
canceled and retried claims fail without changing revision or data. Explicit
failure/retry retains the completed prefix and never reuses a token. Normal
selection cannot bypass accepted/ambiguous Miaoji submit state; takeover is a
separate explicit operation preserving the superseded task receipt.

Sealed finalization compares all retained receipts, writes the canonical
overlapping transcript unchanged, binds the existing Qwen summary input digest,
and transitions to `Summarizing`. Missing `local_moss` is omitted during legacy
serialization, so old jobs do not gain an unknown null field. Seven focused
ledger tests cover these boundaries, with a separate legacy byte-roundtrip test.
The full App library run passes243 tests with24 explicitly ignored; formatting
and strict library clippy also pass. This is durable-state proof, not actual
worker dispatch, model installation, UI, signed bundle, or full offline App
completion. At that checkpoint the engine's MOSS arm was an explicit
unavailable placeholder, subsequently replaced by the leased runtime below.

Runtime integration must also arbitrate against in-flight TOS upload during
mode selection, preserve any unresolved upload/cleanup obligation, and supervise
child cancellation/reaping; an artifact lease only coordinates local MOSS work.

## Effective AAC duration correction and native short tail

Extending the same quiet-window test to `mixed_10` initially failed
`window_source_duration_mismatch` before workspace creation or inference.
The frozen manifest says720,384ms, while current AAC and WAV both contain
11,524,167 effective16kHz frames (720,260.4375ms). FFprobe's MP4 trace confirms
movie timescale16,000, edit duration11,524,167 and media start1,024. The old
importer counted all11,526,144 decoded AAC frames, including priming/padding,
and the old quality harness copied that padded duration into the manifest.
It was a duration-producer defect, not a changed audio file or model failure.

`local-eval/audit_source_durations.mjs` validates all44 original AAC hashes
against retained Miaoji import metadata and verifies equal AAC/WAV effective
frame counts. Four files (`mixed_02/07/08/10`) exceed100ms of metadata padding;
the maximum is127.25ms and the total is3.477625 seconds across the44 files.
Its new immutable artifact is `matrix/outputs/source-duration-audit-v1.json`.
It does not overwrite source media, manifests, model output or old reports.

Shared `ContainerTimeline`/`DecodedFrameTimeline` now select validated MP4 edits
or packet trims exactly once, with decoder gapless mode explicitly disabled.
Metadata is parsed and rewound before Symphonia receives a shared descriptor.
The importer still fully decodes, but only counts effective frames; it does
not resample or buffer the full file. `mixed_10` AAC and WAV now both inspect
as720,261ms, preserving all11,524,167 samples. Fractional AAC/MP3 fixtures,
126ms priming/padding overcount, and the effective-near-five-hour boundary are
covered. Unsupported complex edits remain explicit failures, not approximations.

The quality harness's former duration synchronizer is now read-only validation
of frozen evaluation timelines; its regression proves no in-memory or on-disk
manifest rewrite. The unchanged historical timeline and current physical
decode duration are distinct data, not interchangeable gate inputs. New native
tests use the hash-bound duration audit and copy exact PCM frames, including
sub-millisecond final tails. Neither the worker's100ms duration check nor its
100ms final-segment overrun rule was relaxed.

`mixed_10-quiet12-window01-1788647640848154000` completes its2.691-second
tail in4.5227 seconds under network denial: one segment, zero unknowns, zero
end clips and zero stderr. Frames11,481,120→11,524,167 are retained exactly;
the timestamp ceil is recorded as9 representational frames, not inserted audio.
Worker SHA-256 is `e88a947bd5e694d9a32acb2c99b43b51107db78955463db05690db7b518cc570`.
This proves that one real short tail can execute, not all tiny tails, global
speaker quality, or a fully integrated App route. No playback or personal
recording was used.

## Leased App engine, preparation and command integration

`engine/moss.rs` now holds one recording/generation OS lease across preparation,
ASR windows, acoustic anchors, reconciliation, local summary and publication.
Value-returning worker tasks retain ownership after caller abortion until
cancellation/reaping finishes. Preparation may publish only claim-fenced local
receipts; stale/canceled jobs cannot checkpoint late output. A crashed local
summary is retryable under that exclusive owner without reopening a Gemini
charge fence. Concurrent resume cannot dispatch twice or label a live local
summary ambiguous.

The Queued-only local-selection CAS precedes decoding or model-byte checks.
If remote resume already changed Queued to Uploading, selection fails with an
explicit `remote_effect_already_started` result. The declaration uses compiled
model pins, not an assertion that models are installed; the workers still
verify actual bytes. New `PreparingLocalMoss` checkpoints retain the exact
source/pins/language, owner-session/token and ordered prepared-file prefix.
Current effective-frame inspection, cancelable decode, the shared quiet
planner, and versioned `pcm16-round-nearest-clamp-fullscale-v1` preserve every
sample. File publication is private, immutable and fsynced. A32,017-frame WAV
with a deliberately stale duration hint still produces32,017 frames/2002ms.
Decoder failure and retry stay local and never enqueue an upload.

A focused cancellation review found archive effects outside task ownership.
They now retain the lease too. The ledger's atomic target `Started` checkpoint
is a non-cancelable commit boundary: cancellation before it prevents dispatch;
afterward the command returns `publication_commit_in_progress`, keeps the job
nonterminal and requires finishing/reconciling the archive. This avoids false
canceled success with an untracked late archive. It applies to later explicit
cloud backup as well. Reprocess retains an old verified publication; canceling
the new summary preserves that archive and its cleanup history without enabling
retention for a non-Complete job.

The real Swift diarization request contract exposed another integration issue:
the worker prepends `inbox/<recording_id>/`, while the original private App
plan incorrectly supplied an App-relative anchor path. New plan schema2 stores
the package-relative path and validates its exact reconstruction to the source
identity. The worker protocol is unchanged. Schema1 is readable byte-for-byte,
including original plan/request hashes, but cannot create claims or execute
ASR, finalized summary, reprocess or remote backup. There is no silent
dispatch-time translation of a retained request.

Thirteen engine tests,13 plan/finalizer tests,10 preparation tests and the
existing7 ledger tests cover these boundaries. Before the later snapshot
addition, the full App library suite passed289 tests with27 explicit ignores;
the MOSS-filtered suite passed67 with2 ignores. A real file-only PCM test runs
the complete preparation-to-archive orchestration with fabricated effects,
while the explicit native test below is separate model/runtime evidence.

The new `ECHOWALL_MOSS_CANDIDATE_ENABLED` switch defaults off and does not
enable Qwen or SpeakerKit candidate routes. Four explicit commands expose
model status/install/remove and Queued-only local selection; permissions are
limited to the existing main webview/authenticated loopback capability. There
is no new origin, server, download-on-selection, provider fallback or packaged
credential. Optional worker initialization is macOS-arm64-only; Intel compile
and command/feature/model tests pass. UI and installed/signed App proof remain
separate unfinished work.

## Snapshot cleanup after kill and restart

The shared std/libc-only `moss-worker/src/scratch.rs` now manages four fixed
private slots under `processing/moss-snapshots-v1`. Stable kernel-lock inodes
are never unlinked. A fsynced random-nonce journal precedes model creation, and
the model inode is persisted before any model bytes are copied. Only exact,
single-link, uid/private/layout-validated files are eligible for unlink after
the same slot lock is acquired. Live workers are protected by their kernel
leases, including orphaned workers; cleanup does not infer liveness from PID.

Normal model/session teardown precedes snapshot cleanup. The App reclaims
after confirmed child reap or at the next safe MOSS start. Malformed, replaced,
symlinked, hardlinked or unknown state fails closed; all old unmarked
`.moss-model-*` paths remain untouched. Ordinary interrupted initialization and
copy states are recoverable. If exit cannot be confirmed within the bounded
five-second abort reaper, cleanup waits for a future safe probe. Hostile
same-uid mutation is outside the process-isolation boundary.

Worker tests pass9 with one subprocess helper ignored; App launcher tests
pass15 with one helper ignored. They use synthetic files/processes, covering
normal exit, kill/restart, explicit cancel, future abortion and preservation of
another live slot. Worker/App clippy and scoped formatting pass. A focused
read-only deletion review found no reproducible actionable defect. The rebuilt
release worker, SHA-256
`d85e99c4f34049f4633b9fb49c4f40b1e0d0d88530d5396b63ebfb66493852cc`,
also passes the actual public `mixed_10`2.691-second final window in4.5314s,
with one segment, no unknowns/end clips and zero stderr under network denial.
That proves the changed snapshot path can execute natively, not signed bundle
acceptance. The full source-engine pilot below uses its separately recorded
older ASR worker identity.

## First native source-engine attempt and retained retry

The opt-in `engine/moss/live.rs` test uses only public AMI `english_01` AAC,
SHA-256 `22c340e2193a29a14ecfcda5d6e7c0010b9d7efbbc3e4d0fc9ac374a2969fc8c`.
The test process and all three workers deny network; loopback bind/connect
must fail with policy errors before the test starts. Credentials are an empty
ephemeral store. There is no playback, device capture, personal audio, download
or provider call. A fresh retained output root holds per-attempt identities,
the same durable ledger, immutable ASR output and local archive artifacts.

`matrix/outputs/moss-app-engine-v1/english_01-PgTUqu` initially reached one
completed MOSS window, then durably failed at acoustic anchors after193.80s
(including initial model hashing/setup). The copied `src-tauri/binaries`
diarizer was stale:969,752bytes, built September4 22:06, with no SpeakerKit
preset. It returned closed `invalid_request`. The current Swift release build
is1,909,480bytes, built September5 00:30, and contains the requested preset.
The retained test resumed with that current worker and passed in65.1326s,
reusing the existing ASR receipt rather than rerunning or rewriting it. It
completed184 canonical segments,7 summary fields, verified local publication
and canonical audio backup; source and archived audio both match the allowed
SHA-256. A second resume returns the identical Complete ledger without further
effects. `completed.json` was read back; `failed.json` and original
`started.json` identities remain intact, with a separate resumed-attempt record.

ASR worker SHA-256 is
`e88a947bd5e694d9a32acb2c99b43b51107db78955463db05690db7b518cc570`;
the successful SpeakerKit worker is
`46c99e023ecca604d77049a1cbe00232078611acef819c6d06aa80b2688e5cc6`;
summary worker is
`bbc7609ed20ad8d160d613b70bc1a1d7eb1e58426a856e17a5fa82f55a14cb43`.
The stale diarizer's original hash is
`e93bb67f97a8b18785921b71b464463ea5b75d1307a96af5f6ba2079e5a1f125`.
No original source, retained ASR response or earlier quality artifact changed.

Latest integrated App library validation passes296 tests/28 explicit ignores;
formatting and strict `clippy --lib --tests` pass. This is real source-engine
offline/retry/archive proof, not webview/installed App, coherent signed bundle,
uniform quality policy, human speaker/summary review, or release acceptance.

## Fifth-sidecar packaging contract (source/static proof)

macOS configuration/build scripts now include the fixed fifth MOSS sidecar,
the arm64 native implementation and Intel unsupported stub, exact native
license notices, locked transcribe.cpp0.2.3 revision, and five-name architecture/
signature/hardened-runtime/privacy checks. Release MOSS remains explicitly off;
Windows Release2 packaging is unchanged. The normal wrapper rebuilds Swift
from current source, copies that exact result and compares bytes, then checks
diarization protocol/preset markers again in arm64/universal artifacts and the
final bundle. The read-only gate accepts the current Swift build and rejects
the stale copied diarizer found above. Five fake-tool/static tests and script/
configuration syntax checks pass. No new universal App bundle, signing,
notarization, upload or installed-App mutation has been performed.

Remaining runtime proof includes killing the actual App with a live worker and
verifying recovery/resource reclamation, then the enabled UI and installed
bundle. Synthetic caller-abort/slot-lock tests are not those proofs. Do not lock
or sleep the user's machine to obtain them.

## Candidate UI and combined model lifecycle

The default-off source UI now has a separate MOSS candidate model panel and
explicit queued/import processing action. It preserves the existing graphite/
light styles, readable single-line action labels in the170px sidebar, and
clear states for missing dependencies, progress, cancel/retry, unsupported or
low-memory devices, removal, remote-selection rejection and archive commit.
Unknown IPC selection outcomes trigger only a ledger read, never another route.
MOSS failures cannot expose the legacy Whisper/Qwen transcript-only fallback.
The existing default/offline preference remains unchanged.

`local_models/moss_pipeline.rs` composes exactly31 files from the existing
pinned MOSS, SpeakerKit and summary catalogs. It reuses installed shared bytes,
requires all three components for readiness, retains download partials for
retry, and caches verification against file identity/mtime/ctime while native
workers still hash their actual inputs. A dropped operation's owned I/O keeps
the global model mutation lease until it stops. Removal keeps shared files by
default; explicit shared removal names its effect on other local routes.

Seven small-file, no-network tests cover catalog composition, shared reuse,
cache invalidation/forced proof, missing dependencies, partial adoption, cancel/
ownership, removal isolation and symlink rejection. An empty `.partial` left
before the first downloaded byte is now reopenable instead of repeatedly
failing create-new; unverified retained bytes expose removal rather than a
dead-end retry. This is not a fresh17GiB live download attestation.

The headless MOSS UI suite and existing capture/import suite pass using
fabricated data and IPC. Chrome is explicitly muted; no native capture/model
commands run in these tests. Parent visual inspection covers dark model-panel,
light170px sidebar and archive-commit state. Screenshots:
`/tmp/echowall-moss-ui-main-20260905`; regression fixtures/screenshots:
`/tmp/echowall-moss-regression.fb5fVM`. The isolated test venv uses cached
Playwright1.61.0/Python3.12.9 and installed Chrome, not a new browser download.
The source model commands are registered with three narrow existing-window/
loopback permissions; no new origin or server is added.

Testing side effect: an initial legacy viewer-generation command used the
native App's `WATCH_TRANSCRIBER_DATA` variable, but that Python helper actually
uses `LOCAL_ARCHIVE_DIR`. It therefore regenerated local `data/index.html` and
copied `marked.min.js`; original audio, notes and manifest were not modified
or uploaded. AX was informed. The corrected test used only the fabricated
directory; `make_demo_data.py --render` now binds the correct variable itself.
Do not repeat the ambiguous manual invocation.

## Worker owner loss, not only caller cancellation

The MOSS launcher adds only `ECHOWALL_WORKER_PARENT_PID` to its cleared
environment, set to the launching App's own PID. MOSS and summary share
`local-worker-support/parent_guard.rs`; Swift has the equivalent narrow
`ParentLifetimeGuard`. Each validates canonical PID syntax and the actual
direct parent before model work, then checks kernel parent identity on a
monotonic100ms cadence. A lost parent causes only the worker itself to exit74;
no supplied PID is ever signaled or treated as proof of liveness via reuse.
Normal shutdown disarms/joins the monitor. Standalone callers may omit this
optional binding and retain their previous one-shot contract.

Process-tree tests reproduce an unguarded orphan after host SIGKILL, then prove
Rust and Swift guarded workers exit; the Swift probe also uses the real
deny-network sandbox. Both worker crates pass these3 focused tests plus one
explicit helper ignore. The Swift executable was rebuilt with its async@main
entrypoint explicitly parsed as a library now that it has multiple source
files. Packaging checks require owner support in all three MOSS-route workers,
so a valid but stale sibling cannot satisfy the new bundle gate.

Rebuilt MOSS worker SHA-256
`f868d73941f6a28bae49fc4150f738f5fc5e6ded1ed4a281ce26ad9b66196d41`
completes the public2.691s tail in4.2035s with parent binding active, one segment,
no unknowns/clips and zero stderr under network denial. Earlier native/source-
engine identities remain historical evidence; no installed bundle was updated.

Parallel tests exposed a safe-but-transient snapshot cleanup skip when a
shared/inherited pre-exec descriptor still retains a flock. The regression now
checks that no file is removed during the skip, waits boundedly for release,
then requires rejection of the unknown layout. A deterministic shared-descriptor
test covers the same ownership behavior. This did not justify deleting or
ignoring unknown files. Latest full App suite:304 passed/28 explicit ignores,
formatting and strict library/test clippy pass. Actual installed-App death and
the response-file-before-ledger-receipt cut remain separate required proofs.

## Response intent and publication/receipt crash recovery

A failing-behavior characterization reproduced a real gap: the old engine
could write `window-00.json`, die before recording its receipt, then rerun ASR.
If the new model response differed even with the same request, immutable-file
conflict put the job in ProviderFailed again on every retry. This was not
covered by the earlier completed-prefix or caller-abort tests.

`MossCheckpoint.pending_response` now records the exact validated response
hash, size, plan/request binding, recording/generation and effect kind before
canonical file publication. It is omitted when absent for legacy serialized
compatibility. A fresh exclusive owner/claim reads and validates a matching
published body, commits its receipt and clears the intent without dispatching
the worker. If the file is proven absent, only that missing local effect may
repeat. A symlink, hash mismatch or invalid binding is not treated as absence;
the intent and data remain intact and no native/archive effect runs.

Old unreceipted bodies have no durable hash provenance. They are preserved
under `.unattested-<kind>-<sha>` in the same private generation directory before
fresh inference, not adopted as trusted text or overwritten. Preservation is
lease-bound, bounded, create-only and byte-verified; a durable retained copy
precedes removal of the old canonical name. Plans, committed prefixes and
original recording paths are not eligible. Collision/symlink/owner-mismatch
tests prove neither target is overwritten or removed.

Four actual child-process exits without Rust Drop cover ASR and acoustic
anchors both after intent/before file publication and after file publication/
before receipt. Parent recovery proves OS lease release, exact saved-body
adoption and correct worker-call counts. Additional tests cover missing-body
retry, tampered-body rejection, old-claim rejection, cancellation and legacy
unreceipted preservation. All tests use synthetic recording packages/fake
model effects; this is process/filesystem crash proof, not installed-App or
live-GPU crash acceptance. Latest full App suite:313 passed/29 explicit
ignores, strict library/test clippy and formatting pass.

## First coherent five-worker unsigned universal App

The canonical remapping/build wrapper now completes an actual universal `.app`
with the current main program and all five sidecars. Both arm64 and x86_64
slices are present for each executable; all six executable scans report no
personal builder path. The three MOSS-route workers include the parent-lifetime
marker, and the current diarizer passes the preset/protocol check. Exact
identities and the build command are in
`moss-unsigned-bundle-2026-09-05.json`.

The build deliberately used `--no-sign --ci`, did not supply signing credentials,
launch/install an App, or upload anything. The strict release gate correctly
fails at the first unsigned worker; no signing/notarization success is claimed.
An exact local copy is retained at
`local-eval/bundles/EchoWall-unsigned-be2a1cae.app` before later rebuilds/signing.
The main binary is
`be2a1cae5543c3d469fea96d33314a4ea5fd9afa2b2ea7f1e91c668a66efbf48`.
Signed/installed isolation, live model/App-death replay and the complete quality
matrix/human gates remain open. MOSS still defaults off.

## Isolated QA App model-active crash and cold-reopen proof

The opt-in macOS-only `isolated-qa` build keeps the real importer, Rust ledger,
native workers, secure-store implementation, summary and archive. It substitutes
only dedicated App-data/archive/Keychain names and denies App HTTP at connection
establishment. An explicit hash-allowlisted public import/retry trigger reuses
the real use cases; ordinary launch uses the unchanged startup resume path.
The release bundle gate rejects the QA marker. No production App, credentials,
personal audio or existing archive was changed; no playback or lock action ran.

Retain the failed V2 attempt. A loopback-allowing outer App sandbox caused the
worker's stricter sandbox application to fail with `sandbox_apply: Operation
not permitted` (exit71), leaving ProviderFailed and zero completed windows.
The same worker/request works under its ordinary sandbox alone. A `/usr/bin/true`
probe reproduces failure with differing nested profiles and success with the
identical deny-all profile. V3 does not weaken/skip the worker sandbox: the QA
App instead has a deny-only reqwest connector on all five client constructors,
plus an early guard before the provider's explicit DNS resolution. Two focused
tests prove the inner connector is never polled/called and a real loopback
listener receives no connection. This is a QA HTTP boundary, not a process-wide
OS network sandbox; neither empty logs nor missing remote checkpoints replace
the latter acceptance gate.

The retained locally ad-hoc-signed arm64 V3 bundle passes strict deep code-sign
verification. `run_moss_qa_crash.mjs` explicitly retries the existing failed
public AMI `english_01` task, observes the exact child executable/PID with
1,788,752KiB RSS (above512MiB for3.02s), journals the state, and sends SIGKILL
only to the just-launched QA App PID13558. Worker13562 disappears in109ms.
The ledger remains LocalTranscribing with its old claim and no ASR receipt;
there is no fake success, manual ledger edit or killed worker cleanup shortcut.

An ordinary V3 launch (PID14054) resumes the **same** recording/generation,
reclaims the dead owner's work and completes MOSS → SpeakerKit → local summary
→ local archive:184 canonical segments,7 summary fields, one manifest entry,
and byte-identical imported/original/archived public audio. ASR/anchor receipt
files are retained. TOS/Miaoji checkpoints remain absent; the bounded private
App log contains zero QA HTTP-denial markers. A further cold launch (PID16068)
passes a10-second readback with identical ledger, manifest, note, plan, ASR and
anchor hashes and no child process. This proves App-death recovery in the QA
flavor, not a clicked UI workflow or release-installed acceptance.

Exact identities/results are in `moss-qa-app-recovery-2026-09-05.json`. Detailed
before-launch/before-kill/after-kill/completed/reopened receipts remain under
the QA App-data root at
`qa-evidence/model-active-crash-bb8a27a1-9faa-4b5e-a3e9-363391f4f3bd`.
The retained V1/V2 bundles, failed attempt and earlier unsigned universal bundle
are not overwritten. The main V3 hash is
`6e356002115ec21f3d20332160c8c40c676e12cd2b5a8b0760699804db552e5d`.

Verification: normal App suite315 passed/29 explicitly ignored; three focused
QA tests pass; normal and QA library/test strict clippy plus formatting pass;
five packaging/static tests pass. CUA still returns `Sky Computer Use native
pipe startup failed` for the reopened App, so no UI screenshot/click acceptance
is claimed. Release signing/installation, whole-App OS-denied reopen, uniform
44-case policy and human quality gates remain open. MOSS stays default-off.

Post-review correction: the initial QA validation checked only the root. A
pre-existing symlinked archive/inbox/nested-model directory could redirect
writes when adapters canonicalized it. The bootstrap now validates a bounded
existing child tree before any adapter runs, checks canonical parents for an
absent root, and creates direct writable roots one component at a time. Shared
JS guards cover preparation and evidence tools, including the initial root
creation before writing its marker. External-sentinel tests reject archive,
inbox, nested-model, processing/evidence and absent-root-parent redirection.
The existing publisher-created `data/by-topic/<category>/<leaf>` aliases are
allowed only if their resolved target is a regular file inside this same QA
archive. No legitimate alias or evidence was deleted to obtain a pass.

Five focused Rust QA tests, four JS path tests, the normal App317-pass/29-ignore
suite, both strict clippy variants, formatting and the real-tree guard pass.
The rebuilt/ad-hoc-signed V4 main hash is
`7d1023bad10503c32bc81fc968a7e98d68c586e6b122385cf714784c743d1833`;
all five worker hashes match V3. Its cold launch PID24705 passes the same
10-second data/idempotence check with unchanged ledger/manifest/note/plan/ASR/
anchor hashes. This is an updated-main reopen check, not a rerun of V3's
model-active crash. Both bundle/receipt identities remain explicit in the
machine-readable evidence. Whole-App OS-offline/UI/release gates are unchanged.

## Original-AAC uniform App policy, first20 cases

The old short40 and windowed-long4 results do not prove one deployable pipeline.
In particular, all ten Mixed files have effective duration slightly over12
minutes. App preparation therefore creates two windows for each; the historical
whole-file40 path did not. The new opt-in
`engine/moss/matrix.rs::public_uniform_aac44_transcript_matrix` uses the actual
Rust importer, preparation, immutable window ledger, ordinary native worker
supervisor, coalescing-v2 adapter and graph-v3 finalization on the44 original AACs.
Each source matches the fixed duration audit and retained Miaoji-import hash.
No reference labels, terms, count hint, language hint or corpus stratum selects
a model parameter. Mixed-first order is test scheduling only.

The test process and workers use the same OS deny-network profile. Credentials
are empty/ephemeral; models and all three worker binaries are snapshotted from
existing verified local bytes. No download, playback or personal audio occurs.
Each job stops at `Summarizing` before local summary or archive. That is a
deliberate transcript-quality isolation, never claimed as local completion.
Cases keep ordinary job generations and per-window receipts; a failure is
retained without an automatic retry or a fabricated output. A missing case
stays visible in the batch outcome and cannot become a full-matrix pass.

The new `score_moss_matrix_snapshot.mjs` snapshots only already-completed,
hash-checked canonical results. It preserves source/reference manifest fields,
records both grader binary hashes and every input hash, writes create-only
snapshots, and reports only aggregate JSON (including on validation failure).
Repeating a snapshot verifies identical bytes; an invalid run path fails without
an assertion dump. The graders still emit a JSON report on exit0 even when the
quality status fails; the wrapper does not confuse those meanings.

At2026-09-06 05:28:58UTC,20/44 cases complete with no runtime failure:

| Fresh original-AAC App policy | English10 | Mixed10 |
|---|---:|---:|
| WER local / Miaoji |16.71% /25.28%|10.26% /11.62%|
| CER local / Miaoji |11.94% /18.30%|8.02% /9.30%|
| DER local / Miaoji |21.76% /37.26%|16.06% /20.26%|
| JER local / Miaoji |25.98% /52.22%|17.13% /24.34%|
| Exact count local / Miaoji |9/10 /3/10|7/10 /8/10|
| Candidate-term recall local / Miaoji |90% /50%|90% /100%|
| Minimum per-case lexical assignment |100%|100%|
| Relative automatic stratum verdict |pass|fail: count and terms|

All three remaining Mixed count errors are one extra speaker. Their predicted
counts match the previously measured standalone SpeakerKit counts. Re-reading
the existing0.45/0.75 diagnostics confirms both remain7/10 on Mixed; no new
threshold sweep was run. The tentative terms come from the existing frequency-
based extractor and still need human adjudication; no target or reference was
changed to make this run pass. Lower WER/DER does not erase the count failure.

The live batch is `run-7RKXpm`, runner PID37712/tool session87891 at this
checkpoint, not a terminal result. Full paths, frozen runner/worker hashes and
snapshot locations are in `moss-uniform-app-matrix-2026-09-05.json`. Keep checking
the same actual process/handle through the remaining24 cases. Default App
backend, prior reports, human gates and release criteria are unchanged.
Verification: normal App318 passed/30 explicitly ignored (the new corpus test
is opt-in), strict library/test clippy/formatting pass; grader15 library plus
one CLI tests pass. This is substantive partial evidence, not whole-matrix or
full-local quality acceptance.

## Terminal uniform App attempt and resumed diagnosis — 2026-09-06 UTC

The original `run-7RKXpm` attempt finished at 06:00:33 UTC with 44 attempted,
42 successful transcript-stage cases and two `effect_Temporary` failures.
`long_form_04` failed at zero-based window 5 after retaining five receipts;
`overlap_02` failed at window 0. No summary/archive effect ran. The original
outcome remains immutable and all 44 source hashes were unchanged.
On resumption, PID 37712 is absent and the snapshot command reverified the
existing 42-case score bytes. This is a terminal failed batch, not a live wait.

English and Mandarin meet relative transcript metrics. Mixed remains at 7/10
exact counts versus Miaoji 8/10 and 90% tentative terms versus 100%.
The separate lexical-assignment gate reveals additional App-route regressions:
two Mandarin cases fall below 99% (minimum 93.7228%) and three of nine completed
overlap cases fall below 99% (minimum 89.4198%). English, Mixed and the three
completed long cases have 100% assignment. Long-form/overlap corpus verdicts
remain insufficient. Older whole-file or windowed results cannot fill the two
missing cases or replace these fresh coverage failures.

Outcome SHA-256: `6c03330c8489820680aff7b7e1c70ee85d88db21a5e4a9663a16d7397fcb85ab`.
Completed-case snapshot `scores/107030a3cf1ee4f889d4ac82-all.json` SHA-256:
`0359c42ec190a821732e34f7a0daab314e0ffc218f480bd4987ccb93e9806c49`.
Exact identities, per-stratum metrics and failure coordinates are retained in
`moss-uniform-app-matrix-2026-09-05.json`; the earlier 20-case checkpoint remains
historical evidence there. Diagnosis will use the exact failed requests and
read-only mapping provenance, with new artifacts separate from this attempt.

## Exact mapping replay and single-window policy challenger — 2026-09-06

A fresh offline Rust diagnostic reuses the actual protocol adapter and graph-v3
mapper and exactly reconstructs all 42 completed App canonicals, including text,
timing, order and speaker labels. All 50 newly unknown segments originate in
graph reconciliation, not MOSS attribution: 12 in Mandarin02, 16 in Mandarin06,
11 in Overlap01, 8 in Overlap07 and 3 in Overlap09. Every affected job has one
physical window. Overlap constraints prevent some joint-model slots from sharing
available whole-file acoustic anchors, even though those slots have substantial
positive acoustic support. The original output is correctly preserved as failure
evidence; mapping has not been changed behind any retained plan/hash.

One separately named challenger retains the adapted joint model's own anonymous
slots when the physical plan contains exactly one window and uses unchanged v3
for multiple windows. It uses no stratum, reference, term, count hint or score to
select behavior. It preserves every text, interval and ordering value. This
removes all 50 mapping-created unknowns: the 42 completed cases all meet the
99% per-case lexical requirement (Mandarin minimum99.7973%, other groups100%).
Mandarin DER/JER improve to9.5570%/20.2376% and count6/10 (Miaoji1/10); completed
Overlap9 DER/JER improve to46.0830%/49.8418%, count6/9 (Miaoji0/9). English
count drops from9/10 to8/10 but remains above Miaoji3/10 and still passes all
relative transcript gates. Multiwindow Mixed and Long3 results are unchanged.
The two missing runtime cases, Mixed count/terms and human gates remain open.
This is an output replay challenger, not App integration or a complete matrix.

Separately, reproduced timing rejections exposed a launcher classification
omission: fixed worker codes `invalid_timing`, `invalid_segment` and
`ambiguous_unknown_timing` now map to Verification rather than Temporary.
The bounded stderr remains private; no arbitrary diagnostic escapes. All ten
worker-supervision subprocess tests, including the new rejection/redaction
regression, and formatting pass. Timing limits and transcript acceptance are
unchanged. `moss-uniform-diagnosis-2026-09-06.json` records exact diagnostic
artifacts, hashes, comparisons and the original graph-failure provenance.

## Mixed tail split: isolated acoustic cause and context probe — 2026-09-06

Read-only annotation comparison isolates two substantial identity splits in
Mixed01/02 and one5.8-second tail split in Mixed05. The tentative missing term is
`badminton` in Mixed05:12 reference occurrences, absent in both fresh App and
historical MOSS outputs, present in Miaoji. It predates App windowing. The three
count errors remain identical under retained SpeakerKit baseline/0.45/0.75 runs;
no threshold sweep or reference edits were performed.

A copied, instrumented unchanged SpeakerKit worker reproduced Mixed05's219
segments and6 speakers exactly. Its last30-second model chunk starts714s but
contains only12.1726s real audio and17.8274s zero padding. Four tail embeddings
seed a separate AHC/VBx cluster; cosine to the preceding speaker centroid is
only0.0537–0.0625. The copied model preprocessor centers features over the full
padded chunk. This observation motivated one different-context experiment,
not a threshold change.

Using the exact original last480,000 decoded frames, with no artificial tail
padding, reduces the result to5 speakers/218 segments. All21 final-chunk
embeddings join the preceding speaker. Tail-to-previous-individual cosine is
0.8896–0.9471. Independent diarization-only scoring improves DER12.6499% to
12.0160% and JER12.3819% to12.1055%; Miaoji remains9.3228% DER. The exact source
slice is[11,138,761,11,618,761) at16kHz. All35 chunks together cover every source
frame without gaps; the first34 geometries and labeled10ms prefix through
696.170s are unchanged. Fractional absolute offsets are carried explicitly.

This supports final partial-chunk context as the owning cause for this split;
it does not isolate mean normalization from every other context-dependent
model effect or prove all-corpus quality. Both probes are retained separately,
with unchanged input/model/old-anchor hashes and no audio playback/network.
A separately versioned native quality preset is being implemented; v1 requests
retain their old behavior and App/catalog defaults remain v1 until further
proof. The Rust protocol accepts only the new fixed preset with speakerkit-v1
and rejects cross-preset response substitution;13 protocol tests pass.
See `speakerkit-tail-context-2026-09-06.json` for exact aggregate evidence and
reproduction artifacts. This single diarization case does not close Mixed ASR
terms, full-matrix, short-turn, human or release acceptance.

## Versioned candidate App source-engine proof — 2026-09-06

The mapping fix now persists preparation schema2 and explicit plan schema3;
legacy preparation1/plan2 stays graph3 and schema1 remains non-executable.
All327 normal App tests pass (31 explicitly opt-in), strict library/test/example
clippy passes, and a focused persistence review finds no High/Critical production
issue. An authentic legacy fixture was corrected to omit the new field.
The actual App-finalizer example replays all42 retained plan2 outputs exactly
and all42 plan3 candidate outputs exactly, totaling8,440 segments. There are29
single-window cases; unknowns fall51→1. It changes no artifact or model output.

A fresh `english_01-O0oHVl` source-engine run then uses real current workers,
plan3 and the composed policy under OS network denial/empty credentials.
It completes184 transcript segments,7 summary fields and one local archive with
an exact source-audio hash; a repeated engine resume is identical. Total337.61s
includes the debug runner's model hashing and local summary, so it is not a
release-performance benchmark. Worker identities differ from the original44
matrix and are recorded explicitly in the completed receipt. The proof is not
an installed/signed App, full matrix, human-quality or release claim.

See `moss-uniform-diagnosis-2026-09-06.json` for the pilot root, all three worker
hashes, plan hash, completed receipt hash, runtime and preservation scope.
