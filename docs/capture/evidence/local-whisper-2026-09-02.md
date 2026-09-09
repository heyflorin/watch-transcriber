# Local Whisper worker evidence — 2026-09-02

This evidence is intentionally aggregate-only. It contains no recording title,
transcript text, disagreement span, speaker name, credential, signed URL, or
provider response. The user explicitly authorized the most recent local archive
recording for this one comparison; the copied audio, downloaded models, and raw
worker responses were kept in one temporary local directory and removed after
the aggregate results were recorded.

## Boundary and build

- Host: Apple Silicon arm64, 16 logical CPUs, 128 GiB unified memory.
- Protocol: independent `echowall-local-whisper-protocol` crate; no Tauri,
  network, credential, provider, queue, or persistence dependency.
- Worker: `echowall-whisper-worker` pinned to `whisper-rs = 0.16.0` with Metal.
- Worker normal dependency scan contains no `reqwest`, `hyper`, `tokio`,
  `tauri`, `axum`, TOS SDK, or keyring crate.
- Release worker size: 3,309,840 bytes on arm64. The universal desktop build
  also produces a 417,816-byte x86_64 stub which exits unsupported; Intel Macs
  receive no local-STT product claim.
- The explicit macOS Tauri config ran its pre-build step and placed the arm64
  worker beside the App executable at
  `EchoWall.app/Contents/MacOS/echowall-whisper-worker`. A direct invocation of
  that bundled copy produced byte-identical output to the standalone release
  worker.
- The original debug bundle passed strict deep verification after an explicit
  ad-hoc nested-code signing pass. On 2026-09-03 the current universal App and
  DMG also passed Developer ID signing, hardened runtime, independent
  notarization/stapling, worker privacy/network scans, and Gatekeeper. See
  `macos-distribution-bundle-2026-09-03.md`.

The worker accepts exactly one bounded JSON request on stdin, resolves a fixed
model path and recording-relative audio path below one canonical App-data root,
rejects symlinks, verifies file identity/hash/size before use and identity after
use, decodes and streams downmix/resampling in Rust, runs Metal inference, emits
one bounded identity-bound response on stdout, and exits. The App launcher uses
a fixed sibling executable, clears inherited environment variables, bounds both
output pipes, enforces a duration-derived timeout, and kills the child when its
future is dropped. Native zero-duration boundary text is merged into an adjacent
real segment without inventing a time interval.

## Private A/B against the existing Miaoji transcript

The source was one user-authorized 1,183.68-second English M4A already present
in the private archive. Its existing Miaoji transcript was treated as a
comparison reference, not ground truth. Normalization lowercased alphanumeric
tokens and removed punctuation; the aggregate comparison tool never prints
source or candidate text.

| Model | Model bytes | Wall time | Real-time factor | Peak RSS | Segments | Adjacent duplicate segments | WER vs Miaoji | CER vs Miaoji |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `tiny-q5_1` | 32,152,673 | 10.01 s | 0.0085 | 411,779,072 B (392.7 MiB) | 222 | 2 | 22.58% | 14.80% |
| `large-v3-turbo-q5_0` | 574,041,195 | 27.85 s | 0.0235 | 1,118,306,304 B (1.04 GiB) | 151 | 0 | 15.03% | 8.22% |
| `large-v3-turbo` | 1,624,555,275 | 27.37 s | 0.0231 | 2,200,010,752 B (2.05 GiB) | 152 | 0 | 14.91% | 7.94% |
| `large-v3-q5_0` | 1,081,140,203 | 122.32 s | 0.1033 | 2,362,425,344 B (2.20 GiB) | 414 | 7 | 31.03% | 21.87% |

The Miaoji reference contained 3,113 normalized words. Tiny produced 2,988;
Turbo Q5 produced 3,089. Full-precision Turbo improved only 0.12 absolute WER
points while using about 2.8x the model bytes and 2x peak RSS. Non-Turbo Large
V3 regressed sharply on this long-form sample. Turbo Q5 is therefore the
Release 1 default candidate; Tiny remains smoke-only and the other two are not
default options. This single sample is not human ground truth, Mandarin and
two-hour quality remain unproven, and a separate default-on Core ML
diarization stage is required for the full Miaoji-similar deliverable.

## Two-hour offline stability — 2026-09-03

A deterministic 7,200-second fixture repeated four turns from two macOS system
voices. This is a stability/resource fixture, not linguistic ground truth. It
ran on a physical Apple M4 Max MacBook Pro with 128 GiB unified memory and
macOS 26.6.2, using the exact 574,041,195-byte `large-v3-turbo-q5_0` catalog
artifact. The process ran under `sandbox-exec` with `(deny network*)`.

- Whisper completed in 184.05 seconds: 39.1x real time (RTF 0.0256),
  2,190,524,416-byte peak RSS, zero swap, and no macOS thermal/performance
  warning. The closed validator accepted 1,173 segments and 124,402 transcript
  bytes; the framed response was 180,617 bytes.
- The intentionally repetitive fixture produced 25 exact adjacent duplicate
  segments (2.13%). That is a long-form quality risk to score on nonrepetitive
  independent ground truth; it is not silently deleted by a heuristic.
- The paired offline diarization result gave all 1,173 Whisper segments an
  accepted anonymous speaker label after the confidence/overlap gates. This is
  100% structural coverage, not DER.
- A separate forced-kill run terminated the worker with signal 9 after five
  seconds. It emitted zero stdout and the source audio still matched its bound
  SHA-256, demonstrating that cancel/crash does not corrupt the recording.

## Reproduction boundaries

The public helper at
`desktop/whisper-worker/examples/quality_compare.rs` accepts an EchoWall
Markdown note and worker response and emits aggregate counts/error rates only.
It is an example target, not an App binary or processing dependency. Official
model artifacts came from `ggerganov/whisper.cpp` on Hugging Face and were
checked against the repository's published SHA-1 before EchoWall's own SHA-256
identity was calculated. No Hugging Face token was used. The later two-hour
run used the catalog's commit-pinned URL plus exact size/SHA-256 directly.
