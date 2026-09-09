# Local ASR and diarization challenger evidence — 2026-09-04

> Historical Qwen/SpeakerKit checkpoint. The subsequent
> [MOSS 40-case evidence](moss-public-quality-2026-09-05.md) changes the leading
> candidate to MOSS Q8/Metal. Its four measured strata beat Miaoji on both ASR
> and attribution errors; its long-form output and App integration remain
> incomplete. The absolute 95%-count failures below use the legacy policy;
> current relative-release and stretch requirements are separate in
> `../../evals/full-local-quality.md`. Do not rewrite historical scores.

## Boundary and status

This evaluation uses only the public AMI, AISHELL-4, and ASCEND corpus matrix
described in `public-quality-matrix-2026-09-04.md`. No personal recording,
microphone, audio output device, provider credential, or remote transcription
API was used. All model inference was file-only. Python/MLX runners and complete
upstream checkouts live below the Git-ignored `local-eval/` root and are
development oracles only; none is an App runtime or release dependency.

This checkpoint changed the diarization candidate and narrowed the ASR failures,
but it does not close the full-local parity gate. Human named-term review,
short-turn adjudication, blinded summary review, the exact-speaker-count gate,
and the final production-native implementation remain open.

## Pinned candidates

| Candidate | Pinned identity | Boundary |
|---|---|---|
| Qwen3-ASR MLX oracle | `moona3k/mlx-qwen3-asr` `d1a035514e1d6ac31da7658b273482656eacba61`; official Qwen3-ASR-1.7B and ForcedAligner revisions already recorded in the Qwen evidence | Python/MLX development oracle only |
| Existing FluidAudio worker | `FluidInference/FluidAudio` `5c19d5e12320e22bbfb7a1877b089d2665a69add`; EchoWall's source-tracked no-network subset | Current App candidate |
| Argmax SpeakerKit | `argmaxinc/argmax-oss-swift` `ea872ffd35705aa757f33033500b9b0d40bd38df`; Core ML model revision `86ec9c929b52208b6656eb6a6361ed0d822a1f78`, exact W8/W32 pack 29 files / 17,187,070 bytes | Implemented replacement candidate; upstream package itself is not release-safe |
| FluidAudio Sortformer / LS-EEND | FluidAudio revision above; cached official Core ML assets | Rejected after same-corpus pilot |
| Fun-ASR-Nano | official `QwenAudio/Fun-ASR` runtime `v0.2.1`; macOS archive SHA-256 `bc63c4d4b96f2465f1d258600668a971f4f600d661f1859b03797cefaa417167`; three GGUF files / 1,275,804,800 bytes | Rejected ASR challenger |
| MiMo-V2.5-ASR | `ailuntx/mlx-audio` `6241c57d61663725bb8a0ca1e1695c89ab6c09c0`; MLX 4-bit revision `284d42f8d404cc97b625edf9c9c7ed898c05b101`; tokenizer revision `6d451ed9a73024b4d33b87afa69e0dfd40d8f306`; 22 files / 7,103,262,268 bytes | Strongest mixed-ASR challenger, but still below the CER gate and has no accepted native App runtime |

Primary source records:

- [Qwen3-ASR](https://github.com/QwenLM/Qwen3-ASR)
- [Argmax OSS / SpeakerKit](https://github.com/argmaxinc/argmax-oss-swift)
- [SpeakerKit Core ML assets](https://huggingface.co/argmaxinc/speakerkit-coreml)
- [Pyannote Community-1 model card and benchmark](https://huggingface.co/pyannote/speaker-diarization-community-1)
- [FluidAudio](https://github.com/FluidInference/FluidAudio)
- [Fun-ASR](https://github.com/QwenAudio/Fun-ASR)
- [MiMo-V2.5-ASR](https://huggingface.co/XiaomiMiMo/MiMo-V2.5-ASR)
- [MiMo MLX 4-bit conversion](https://huggingface.co/mlx-community/MiMo-V2.5-ASR-MLX-4bit)

The SpeakerKit code is MIT. Its local model bundle attributes Pyannote
segmentation-3.0 (MIT), WeSpeaker weights (the corresponding VoxCeleb model
license), and VBx (Apache-2.0). These notices must be retained if it is promoted.

## Full raw-diarization comparison

Both diarizers processed the same 44 files / 12.507 hours. The candidate
transcript for this isolated score used each diarizer's time intervals and a
constant placeholder word. Therefore only DER, JER, speaker count, and timestamp
coverage are meaningful in this table; its WER/CER and named-term columns are
deliberately ignored.

| Stratum | Miaoji DER / JER | Current FluidAudio DER / JER | SpeakerKit DER / JER | Fluid exact count | SpeakerKit exact count |
|---|---:|---:|---:|---:|---:|
| English | 37.26% / 52.22% | 19.82% / 29.18% | **18.64% / 23.09%** | 6/10 | **10/10** |
| Mandarin | 20.35% / 42.52% | 13.25% / 35.68% | **12.01% / 33.00%** | **4/10** | 3/10 |
| Mixed | 20.26% / 24.34% | 21.31% / 28.22% | **19.23% / 21.88%** | 7/10 | 7/10 |
| Overlap | 70.82% / 81.19% | 55.20% / 65.80% | **51.62% / 58.26%** | 2/10 | **5/10** |
| Long form | 41.43% / 49.95% | 22.15% / 35.20% | **21.04% / 33.20%** | 0/4 | **2/4** |

SpeakerKit is better in every DER/JER stratum and materially improves speaker
count in English, overlap, and long form. Threshold sweeps at 0.55 and 0.65 did
not repair the count gate: 0.55 was unchanged outside a worse overlap result;
0.65 improved overlap from 5/10 to 6/10 but left all other strata unchanged.
Truth-count-forced K-means made the Mandarin/mixed pilot DER worse, so forcing a
count is not a general quality fix.

An oracle that could choose the correct count between SpeakerKit and FluidAudio
would still reach only 32/44 files (72.7%). Adding Miaoji raises that impossible
runtime oracle to only 33/44 (75.0%). The required 95% count accuracy therefore
cannot be obtained by selecting between the existing engines.

## Qwen + SpeakerKit merged result

The MLX oracle's aligned words were assigned by the existing conservative Rust
maximum-overlap, strict point-containment, and same-speaker bridge rules. Words
with irreducible zero duration are not given fabricated milliseconds: for this
diagnostic only, their text is attached to an adjacent real interval and that
interval is downgraded to `local_unknown`. Production still requires the owning
Qwen chunk boundary and uses whole-chunk unknown fallback.

All 44 files completed, including four 90-minute long-form cases:

| Stratum | Miaoji WER / CER | Local WER / CER | Miaoji DER / JER | Local DER / JER | Exact count | Min assigned-word coverage |
|---|---:|---:|---:|---:|---:|---:|
| English | 25.28% / 18.30% | **20.77% / 14.59%** | 37.26% / 52.22% | **22.49% / 26.86%** | **10/10** | 99.23% |
| Mandarin | 16.38% / 16.41% | 16.59% / 16.59% | 20.35% / 42.52% | **14.50% / 34.55%** | 3/10 | 99.82% |
| Mixed | **11.62% / 9.30%** | 15.63% / 15.03% | 20.26% / 24.34% | **17.27% / 19.01%** | 7/10 | 99.73% |
| Overlap | 47.01% / 39.86% | **41.68% / 34.03%** | 70.82% / 81.19% | **54.63% / 59.90%** | 5/10 | 98.66% |
| Long form | 34.49% / 26.45% | **25.07% / 17.60%** | 41.43% / 49.95% | **27.41% / 36.91%** | 2/4 | 99.30% |

English fails only the still-unconfirmed named-term gate in the automated
report (40% local versus 50% Miaoji). Mandarin passes ASR and speaker-error
non-inferiority. Mixed fails WER/CER. Overlap beats Miaoji on ASR and speaker
error, but one of ten cases falls below 99% assigned-word coverage. Exact
speaker count fails every non-English stratum. Long form materially beats
Miaoji on ASR and speaker error, but exact count is only 2/4 and the automated
named-term recall is 91.67% local versus 100% Miaoji pending human review.

Across all 44 files, exact speaker count is 27/44 (61.4%), far below the 95%
gate. The only sub-99% word-assignment case is in the overlap stratum; long-form
minimum coverage is 99.30%. The full five-stratum result therefore confirms
that SpeakerKit is the better diarizer candidate but does not support a local
parity claim.

## Closed worker and catalog proof

EchoWall now builds an arm64 one-shot worker from 18 source-tracked SpeakerKit
inference files plus offline compatibility shims. The resulting 1.6 MB release
binary has no CFNetwork, Network, URLSession, socket, URL, model-hub, or
downloader linkage/markers. It loads only the caller-supplied model directory;
the downloader stub always fails.

The App's Rust model manager embeds a separate default-off catalog for the exact
29-file / 17,187,070-byte pack. It pins every path, byte count, SHA-256, source
revision, license disclosure, and the `speakerkit-pyannote-v3-exclusive-v1`
preset. A live HTTPS install downloaded and verified all 29 files, produced a
closed proof, then removed only catalog-owned files while preserving an
unrelated file. The current full-local default catalog remains FluidAudio
because the quality gates above are still red.

The same public Mixed pilot was then imported into an App-owned root and sent
through the production Rust launcher and bounded protocol. The worker completed
in 5.09 seconds with 29 verified model files, 8 speakers, 246 exclusive
segments, minimum confidence 500/1,000, and zero network effects. No audio was
played; every evaluation in this document was file-only.

## ASR challenger results

All rows use the same first public Mixed case and independent ground truth. The
single-file result is a falsifying pilot, not a replacement for the full matrix.

| ASR route | WER | CER | Runtime / notes | Decision |
|---|---:|---:|---|---|
| Miaoji | 12.28% | 9.04% | Remote baseline | Comparison |
| MLX Qwen, whole file | 15.24% | 15.33% | 82.7 s ASR+alignment in the initial run | Best Qwen policy, still fails |
| MLX Qwen, exact v2 30 s VAD policy | 19.24% | 16.09% | 36 chunks, 154.6 s; 16 Chinese / 20 English detections | Rejected; fragmentation regresses ASR |
| MLX Qwen, fixed 300 s | 20.28% | 17.85% | Three chunks, 154.5 s | Rejected |
| Fun-ASR-Nano q8 | 14.66% | 14.61% | 44.3 s; official C++/GGUF runtime, CPU main layers | Rejected overall; Mandarin was 41.97%/42.40% |
| MiMo-V2.5-ASR MLX 4-bit, 30 s | 14.88% | 12.57% | 25 chunks, 86.8 s | Better CER, still fails |
| MiMo-V2.5-ASR MLX 4-bit, 60 s | **13.17%** | **12.02%** | 13 chunks, 87.1 s | Passes WER margin, fails CER; no native release path |

MiMo is the strongest mixed-language ASR challenger found, but its 60-second
result still trails Miaoji by 2.98 absolute CER points and about 33% relative.
Per the predeclared stop condition, no additional window tuning or App
integration was performed.

## Rejected diarization pilots

| Candidate | Mandarin result | Mixed result | Reason rejected |
|---|---|---|---|
| FluidAudio offline Sortformer | DER 68.21%, JER 82.28%, 4 speakers | DER 69.79%, JER 86.96%, 4 speakers | Very fast (about 2 s / 12 min) but materially wrong |
| LS-EEND AMI | 100% DER/JER, almost all speech missed | DER 49.45%, JER 73.06%, 4 speakers | Domain failure on Mandarin; worse than VBx on mixed |
| LS-EEND DIHARD3 | DER 66.34%, JER 84.57%, 5 speakers | DER 54.16%, JER 82.89%, 3 speakers | Worse boundaries/counts and sub-99% coverage |
| SpeakerKit non-exclusive timeline | 89.24% assigned words | 98.72% assigned words | Fails the 99% transcript-assignment gate; exclusive reconciliation retained |

## Defects found and enforced decisions

1. The Rust Qwen v2 splitter advertised a hard 30-second chunk duration but
   searched up to three seconds after the target for a quiet point. A natural
   Mixed file produced a chunk longer than 30 seconds. `quiet_split_point` now
   searches only before the ceiling; the exact file produces 36 contiguous
   ranges, maximum 29.95 seconds, with complete one-time sample coverage. A new
   Rust regression test covers a lower-energy region after the ceiling.
2. Grouping adjacent same-speaker aligned words is part of the production
   deliverable. The initial diagnostic left every word as a separate canonical
   segment and understated timestamp coverage, inflating DER. The scorer input
   now uses consecutive speaker turns without moving word timestamps.
3. The full Argmax package is not eligible as a worker dependency. Both the
   3.5 MB minimal-link spike and the full CLI import CFNetwork, Network,
   URLSession, and URLRequest even with `download=false` and a local model path.
   Promotion requires a source-tracked offline-only subset with the downloader,
   model hub, transcript-merging extras, and network symbols absent.
4. SpeakerKit is the selected replacement candidate because it wins all five
   raw DER/JER strata. Its minimal offline port, exact candidate catalog, closed
   protocol, no-network binary scan, real one-shot pilot, and full 44-file merge
   now pass. The current FluidAudio worker remains the selected App default
   until short-turn/count gates, crash isolation, universal signing, and bundle
   verification pass.
5. MLX Qwen remains the best same-model runtime oracle. The App may adopt only a
   parity-preserving Rust MLX-C or Swift/MLX one-shot implementation. No Python,
   CLI, localhost service, implicit downloader, or long-running helper is
   permitted.
