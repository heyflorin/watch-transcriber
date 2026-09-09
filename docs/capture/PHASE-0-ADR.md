# CAPTURE-10: Feasibility evidence and architecture decisions

- Linear: [AX-248](https://linear.app/ax-agent-swarm/issue/AX-248)
- Governing plan: `docs/capture/PLAN.md`
- Goal contract: `docs/capture/EXECUTOR-GOAL.md`
- Evidence date: 2026-09-02

This document records decisions and reproducible evidence from Phase 0. API
availability is not physical-device acceptance; the evidence table keeps those
levels separate.

Current supersession (2026-09-05): MOSS-Transcribe-Diarize 0.9B Q8 through
transcribe.cpp v0.2.3/Metal is the leading short-form replacement candidate after
40 public cases with lower WER/CER and DER/JER than Miaoji in all four measured
strata. See `evidence/moss-public-quality-2026-09-05.md`. This supersedes the
blanket MOSS rejection and mandatory Qwen CPU-first/MLX-port sequence below.
Qwen/ForcedAligner remains an alternative; SpeakerKit's separate offline worker
and exact catalog remain replayable. MOSS is not yet App-integrated and its
stopped older long-form batch produced no completed recording output. A new
native per-file harness completed all four90-minute cases: aggregate long
WER/CER43.73%/39.04% versus34.49%/26.45%, DER/JER41.32%/67.30% versus
41.43%/49.95%, exact count1/4 both, terms91.67% versus100%. Whole-file MOSS
fails relative acceptance. Later all32 quiet-boundary windows complete;
adjacent-overlap coalescing v2 plus fixed overlap-constrained SpeakerKit
anchoring v3 passes the six-hour long-form relative metrics (WER/CER
23.21%/16.65%, DER/JER23.28%/33.26%, count2/4 versus1/4). A separate Rust
WER-tokenizer check passes ≥99% lexical assignment per long case. This does
not resolve Mixed or human-review gaps or establish one App-ready policy.
A shared
raw-marker parser removes upstream timestamp repair and leaked end markers;
corrected English WER/CER is16.71%/11.94% versus25.28%/18.30%. The new pure40
manifest preserves old evidence and does not count old long outputs as raw-guard proof.

The acceptance policy now separates relative Miaoji parity from the additional
95% exact-count stretch target. Release count accuracy must be at least
Miaoji's in each stratum. MOSS is 25/40 versus Miaoji's 12/40 overall, but Mixed
regresses 4/10 versus 8/10 and cannot be averaged away. Other quality, 95%
human short-turn recall, offline archive, signing, and device gates remain.
The grader now supports explicit `--policy miaoji-relative-v2` while preserving
the default legacy output byte-for-byte. The pure 40-case MOSS manifest still
reports missing long-form and Mixed count/term failures. The bounded Rust
adapter reproduces all 40 canonical cases, and a standalone native worker
reproduces public `english_01` under network denial. This does not establish
App integration or full-local acceptance. Bounded rate/channel conversion and
a synthetic AAC/MP3 edit-timing fixture now pass; a real public AAC input also
completes under network denial. App codec/ledger integration remains.

## Baseline evidence

| Surface | Evidence | Result |
|---|---|---|
| Host | `sw_vers`; `uname -m` | macOS 26.6.2, arm64 |
| Apple toolchain | `xcodebuild -version` | Xcode 26.6 (17F113) |
| Apple devices | `xcrun devicectl list devices` | paired iPhone 17 Pro Max and Watch Ultra 2 visible |
| iOS simulator | `xcrun simctl list devices available` | iOS 26.5 simulator set available |
| Android toolchain | `adb`, `emulator -list-avds`, Rust targets | Pixel 8 API 36 AVD exists; no Android device/emulator connected at baseline |
| Windows toolchain | `rustup target list --installed` | `x86_64-pc-windows-gnu` installed; no Windows runtime/device available |
| Browser/native apps | installed application inspection | Chrome 151 and Zoom available; Edge and Teams absent |
| Synthetic audio routes | `system_profiler SPAudioDataType` | BlackHole 2ch and several virtual Elgato routes available; fabricated capture can avoid personal mic input |
| Media tools | `ffmpeg -version` | ffmpeg/ffprobe 9.0.1 |
| Python baseline | `venv/bin/python3 -m pytest tests/ -q` | 22 passed |
| Rust baseline | `cargo check && cargo test` in `desktop/src-tauri` | check passed; 3 tests passed |

Android baseline was then exercised without microphone/audio access:

```text
npm run tauri android build -- --debug --apk
→ universal debug APK built for arm64, armv7, i686, and x86_64

adb install -r app-universal-debug.apk
adb shell am start -n ai.ax.watch_transcriber/.MainActivity
→ install succeeded; MainActivity displayed; app process remained alive
```

Emulator: Pixel 8, Android 16 / API 36. This proves the existing shell build and
startup path only; it does not prove foreground-service recording or physical
device lifecycle behavior.

## ADR-001: Capture is native; ingest and processing contracts are shared

Decision: keep permission, device, clock, and recording lifecycle code in a
native adapter for each platform. Adapters emit the same versioned
`RecordingEnvelope` into the Rust-owned durable inbox. Provider orchestration
and archive publication do not belong in capture adapters.

Consequences:

- macOS contains a default-off Core Audio process-tap experiment for selected-
  App Meeting audio,
  ScreenCaptureKit for whole-system audio/source enumeration, and CoreAudio mic
  capture.
- Windows uses WASAPI and Windows Credential Manager integration.
- iOS uses AVFAudio plus document/share extensions.
- Android uses a microphone foreground service plus SAF/share intent handling.
- Cross-platform tests target envelope/state behavior; native tests target
  clocks, devices, permissions, interruption, and recovery.
- Shared Rust ownership starts under
  `desktop/src-tauri/src/ingest/{envelope,inbox,state,import,gateway}.rs`.
  Recorder files live in the platform app-data inbox, never inside the
  Git-managed archive `data/` directory.
- The current app has no Apple capture bridge. The tracked iOS extension points
  are `desktop/src-tauri/gen/apple/project.yml` and
  `desktop/src-tauri/gen/apple/Sources/desktop/main.mm`; Phase 0 must decide the
  generator-safe native-plugin boundary instead of hand-maintaining derived
  Xcode output.
- Android integration begins at the tracked manifest, `MainActivity.kt`, and a
  dedicated service/plugin source. The Tauri-generated Gradle file is not a
  source-of-truth edit target.

Rejected: one generic cross-platform audio library as the owner of all capture.
It hides the exact app/process scope, platform permission states, background
lifecycle, and clock/discontinuity evidence required by the product contract.

Those initial declaration gaps are now implemented: iOS has microphone,
background-audio, App Group, and share-extension sources; Android has microphone
foreground-service and share/import sources; Windows has a Credential Manager
branch and unsigned CI lane. Physical lifecycle/signing evidence remains open,
and the secure store must be reshaped for the embedded provider engine.

## ADR-002: Windows three-mode capture uses WASAPI

Decision: Windows 11 is the supported baseline for the complete desktop
contract.

| Product mode | Windows API |
|---|---|
| Voice Memo | shared/event-driven capture on the selected microphone endpoint |
| Meeting | `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK` with `INCLUDE_TARGET_PROCESS_TREE`, plus a separate mic stream |
| System Capture | shared-mode render endpoint with `AUDCLNT_STREAMFLAGS_LOOPBACK`, plus a separate mic stream |

Implementation boundary:

```text
desktop/src-tauri/src/capture/windows/
  mod.rs
  endpoints.rs
  wasapi_stream.rs
  process_loopback.rs
  clock.rs
  recovery.rs
```

`IAudioCaptureClient::GetBuffer` device position/QPC timestamps and
discontinuity flags feed the shared gap/alignment model. On
`AUDCLNT_E_DEVICE_INVALIDATED`, release the old interfaces, record the gap,
re-enumerate defaults, and require explicit reselection for a no-longer-present
pinned device.

Credential storage uses Windows Credential Manager. User-provided TOS, 妙记,
Gemini, GitHub, and R2 credentials stay behind the Rust secure-store boundary;
they are never compiled into the app or returned to the webview.

Primary evidence:

- [WASAPI loopback](https://learn.microsoft.com/windows/win32/coreaudio/loopback-recording)
- [Process loopback modes](https://learn.microsoft.com/windows/win32/api/audioclientactivationparams/ne-audioclientactivationparams-process_loopback_mode)
- [Microsoft ApplicationLoopback sample](https://github.com/microsoft/Windows-classic-samples/tree/main/Samples/ApplicationLoopback)
- [`IAudioCaptureClient::GetBuffer`](https://learn.microsoft.com/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer)
- [Invalid-device recovery](https://learn.microsoft.com/windows/win32/coreaudio/recovering-from-an-invalid-device-error)

Physical proof still required on Windows 11:

- mic, native Zoom, native Teams, and whole-system fixtures;
- 30-minute drift at or below 100ms, then the two-hour matrix;
- unrelated-app tone absent from Meeting mode;
- mic/output switch and selected-process restart produce explicit gaps;
- microphone privacy denial and Credential Manager roundtrip;
- MSVC release build, signed installer, install/upgrade/uninstall.

Cross-compilation on macOS may falsify Rust/API shape but cannot satisfy these
acceptance criteria.

## ADR-003: Browser Meeting has a reliable accepted fallback

Decision: exact Chrome/Edge tab capture is NO-GO for Release 1 and Release 2.
The required browser Meeting path captures the explicitly selected browser
application/process tree and warns that other audible tabs may be included. It
must exclude non-browser applications and remains distinct from System Capture.

This follows the accepted product fallback and avoids making a browser
extension/native bridge a release dependency. Exact-tab capture may be revisited
as a later optimization, but no companion, store listing, or tab-only claim is
required by the current goal.

## ADR-004: Physical capture tests use fabricated sources

Decision: no Phase 0 test records ambient/user microphone audio without a new
explicit authorization. Prefer virtual devices, synthesized tones, demo calls,
and fabricated speech. The host has BlackHole 2ch and other virtual audio routes
available for macOS tests.

Required fixture pattern:

- mic/source tracks use distinct deterministic tones or fabricated speech;
- unrelated applications/tabs use a third sentinel tone;
- automated analysis proves expected inclusion/exclusion, drift, gaps, and
  duration;
- artifacts stay under task-local ignored test output and are never committed
if they contain anything other than generated fixtures.

## ADR-005: Processing is embedded in the Rust/Tauri app

Decision: EchoWall has no self-hosted broker or processing server. The shared
Rust crate directly executes TOS upload, 妙记 submit/poll, Gemini text summary,
archive publication, and temporary-object cleanup. Swift and Kotlin remain
narrow native capture/import adapters. The existing Python watcher remains only
as a compatibility/reference path for Apple Voice Memos.

Each installation uses credentials supplied by the user during explicit setup.
TOS, 妙记, Gemini, GitHub, and R2 secrets live only in Keychain,
Keystore-backed native storage, or Windows Credential Manager. They are never
compiled into a public binary, written to repo/plaintext configuration, returned
through Tauri IPC, exposed to a browser extension, or included in logs. The
only IPC exposure is user input to a write-only setup command.

The app-data inbox and durable effect ledger are the local control plane.
Required invariants:

- `recording_id` and normalized hash provide local idempotency and deduplication;
- every external effect checkpoints intent before dispatch and its result after
  confirmation;
- provider request/task IDs, transcript, summary, archive generation, per-target
  proof, and cleanup state survive process termination;
- an app suspended or killed during work pauses safely and resumes on the next
  background opportunity or launch;
- remote archive publication uses compare-and-swap/ref preconditions and merges
  by stable `recording_id`; it never silently overwrites another device;
- the processing path never invokes Python, FastAPI, a localhost/LAN processing
  endpoint, or an EchoWall-hosted queue. The existing in-process loopback asset
  transport used by the webview is unrelated and remains allowed only behind a
  per-process capability path, exact-origin mutation checks, exact main-frame
  navigation, compiled executable assets, a restrictive CSP, and an explicit
  Tauri command ACL;
- archive credentials and their GitHub/R2 destinations occupy one versioned
  secure-store record, so a crash cannot combine new keys with an old/default
  target; deletion revokes the in-memory handles immediately;
- Rust publication, viewer edits, sync overlay, and the legacy Python watcher
  share one canonical cross-process archive-writer lock.
- App-native edits use an ordered durable outbox written before authoritative
  mutation. Every ticket carries the non-secret GitHub repo and R2
  account/bucket identity from the same credential snapshot used to construct
  its publisher; credential replacement/deletion is serialized by the archive
  transaction and cannot redirect pending work.
- R2 archive keys are deterministic from captured time, normalized content,
  `recording_id`, and publication generation rather than title or a local
  archive-key collision. A durable attempt record precedes PUT and supports
  owner/hash/size-verified cleanup after a crash.
- Cross-service publication deliberately verifies R2 before the GitHub manifest
  CAS. This differs from the legacy watcher's `archive_git` then `r2_backup`
  order, which remains untouched. The App order prevents a newly authoritative
  manifest from referencing an R2 PUT that later fails.
- Explicit reprocess retains the verified upload and 妙记 transcript, runs only
  summary plus publication, and reuses the canonical R2 key with an additive
  `r2_generation` proof. If HEAD proves that object missing, the pre-PUT journal
  switches to a new generation-owned key before Git CAS.
- Delete is fenced to the exact manifest entry seen by the user and publishes a
  durable recording tombstone. Publishers re-read tombstones after R2 PUT and
  clean their precise attempt before returning conflict. Legacy objects without
  App ownership metadata support only an explicitly confirmed partial delete;
  the UI never claims their remote audio was deleted.

The stages remain independently replayable:

```text
verify → normalize → upload TOS → submit/poll 妙记 → persist transcript
  → Gemini summary → persist summary → archive CAS/publish
    → verify durable backup → clean temporary TOS object
```

A valid transcript is never regenerated because summary or publication failed.
The most important provider boundary remains submit ambiguity. The app commits
the deterministic `X-Api-Request-Id` before dispatch. If the connection becomes
uncertain after possible acceptance and provider idempotency has not been proven,
the job enters `submit_ambiguous`; it does not automatically create another
request or possible duplicate charge.

Current 妙记 material identifies `X-Api-Request-Id` as the submit/query identity
but does not explicitly guarantee repeat-submit idempotency. The conservative
ambiguous state remains until a bounded fabricated-audio test or provider
confirmation proves reconciliation.

The discarded broker/FastAPI prototype was removed rather than shipped after
its useful state-machine, fencing, redaction, cleanup, and publication cases
were ported to Rust.

Provider-facing import artifacts preserve their validated `.m4a`, `.mp3`, or
`.wav` container and use a matching TOS key/Content-Type; App code never invokes
ffmpeg and never labels compressed bytes as WAV. Rust/Symphonia fully decodes
the file for validation before durable adoption. The current official Volcano
recording-file documentation lists AAC/M4A, MP3, and WAV with `<5h` and `<512MB`
limits: https://www.volcengine.com/docs/6561/1354871?lang=zh. Native capture
continues to produce canonical PCM WAV; the legacy watcher retains its own MP3
conversion path.

## ADR-006: Windows ships in Release 2

Decision: Release 1 covers macOS, iOS, and Android. Windows stays on the same
shared contracts and retains its WASAPI, Credential Manager, and CI work, but it
is an unsupported technical preview until Release 2.

Reason: this macOS host can falsify Windows API and compile-time mistakes, but
cannot prove process-loopback isolation, device invalidation recovery,
microphone privacy behavior, two-hour stability, or installer lifecycle on a
Windows 11 machine. Treating cross-compilation as release evidence would weaken
the acceptance contract.

Release 2 starts from the existing implementation and requires the physical
matrix in ADR-002. The deferral changes sequencing, not Windows product scope or
the macOS/Windows three-mode parity target.

## ADR-007: iOS UIScene adoption waits for an upstream-owned runtime fix

Decision: keep the current iOS 26 launchable configuration and do not add a
hand-written `UIApplicationSceneManifest` while the app resolves to tao 0.35.3.
Before an iOS release is built with an SDK that enforces scene adoption, move to
a stable Tauri/tao path that supports the single-scene lifecycle and prove cold
launch on both simulator and physical hardware.

Current evidence:

- Rechecked 2026-09-04 from the official release/API sources: Tauri 2.11.5 is
  still the latest stable `tauri` release and its released runtime resolves tao
  0.35.x. tao 0.37.0 was released on 2026-08-21 with an upstream-owned
  application-delegate/scene-adoption fix that installs tao's scene delegate
  even without a static scene manifest. Tauri's current `dev` branch has moved
  through wry 0.56.0 to tao 0.37, but no coordinated stable Rust/npm Tauri
  release exposes that path yet. Do not pin the App to an unreleased framework
  branch merely to silence the warning;
- `cargo tree -i tao` resolves Tauri 2.11.5 through
  `tauri-runtime-wry` 2.11.4 to tao 0.35.3;
- the archived arm64 simulator App installs and cold-starts on an iPhone 17,
  iOS 26.5 simulator, rendering the in-App setup UI without a crash;
- UIKit logs the warning that UIScene lifecycle adoption will become required;
- Tauri issue [#15719](https://github.com/tauri-apps/tauri/issues/15719)
  reports a hard iOS 27 launch trap for an incomplete static scene manifest;
- tao issue [#1308](https://github.com/tauri-apps/tao/issues/1308) records that
  `UIApplicationSupportsMultipleScenes=false` does not currently attach tao's
  scene delegate, while the pre-0.36 `true` path has a separate release-launch
  crash history.

Consequence: current iOS 26 simulator evidence remains valid but is not iOS 27
release evidence. A local plist edit would trade a visible warning for an
unverified launch/lifecycle change. If the App Store toolchain begins enforcing
the new lifecycle before the upstream fix lands, hold the iOS artifact rather
than shipping a private runtime fork or silently enabling multi-window.

## ADR-008: macOS outage fallback uses Rust/native Whisper, not Python WhisperMLX

Decision: add an optional Apple-Silicon macOS local STT provider behind the
existing Rust effect ledger using pinned Codeberg
[`whisper-rs`](https://codeberg.org/tazz4843/whisper-rs) 0.16.x and its pinned
`whisper.cpp`. Metal is the baseline. Core ML encoder acceleration ships only
if its model-specific artifact, signing path, and measured benefit pass. Candle
and a new `mlx-rs` Whisper port are rejected for Release 1. The default provider
remains TOS → 妙记.

Evidence reviewed 2026-09-02:

- KalebJS [`whispermlx`](https://github.com/KalebJS/whispermlx) 3.13.1 is a
  Python 3.10–3.13 package depending on mlx-whisper, Torch, Transformers,
  pyannote, and Hugging Face; it exposes Python/CLI entrypoints, not a Swift or
  Rust package. Diarization requires a Hugging Face token and acceptance of a
  separate pyannote model agreement. Its ASR source explicitly mitigates
  long-recording MLX cache/swap growth by clearing cache every 20 VAD segments.
- Apple [`MLX`](https://github.com/ml-explore/mlx) provides C, C++, Swift, and
  Python APIs over Apple-Silicon CPU/GPU unified memory. MLX is not a separate
  alternative to Metal at the hardware layer, and the available
  [`mlx-rs`](https://github.com/oxiglade/mlx-rs) binding describes itself as
  unofficial and in active development; no maintained Rust Whisper port was
  found in its examples.
- [`whisper.cpp`](https://github.com/ggml-org/whisper.cpp) already supports
  macOS Intel/Arm, Metal, Core ML/ANE encoder acceleration, VAD, model
  quantization, and a C API with Rust bindings.
- The established `whisper-rs` Rust API exposes segment text/start/end
  timestamps, token-level DTW configuration, safe progress/abort hooks,
  `metal`/`coreml` feature flags, and an embedded Metal library. `cargo info`
  reports version 0.16.0, Rust 1.88, and the Codeberg repository; its source pins
  `whisper.cpp` commit `2eeeba56e9edd762b4b38467bab96c2517163158`. The GitHub
  mirror was archived after the maintainer moved active development to
  Codeberg. The wrapper also documents that some foreign C++ exceptions abort
  the Rust runtime, so inference requires process isolation.
- An isolated arm64 release spike on this host compiled and linked
  `whisper-rs` 0.16.0 first with Metal, then with Metal + Core ML. The resulting
  1,059,808-byte no-model Mach-O links only system libc++/Accelerate/Foundation/
  Metal/MetalKit/CoreML/CoreFoundation/objc libraries and runs without Python.
  This proves toolchain/link shape only; no model, audio, accuracy, memory,
  signing, sandbox, or two-hour inference claim follows from it.
- Hugging Face [`Candle`](https://github.com/huggingface/candle/tree/main/candle-examples/examples/whisper)
  contains a Rust Whisper implementation with multilingual recognition and
  timestamps plus a Metal backend, but its example still lists unfinished
  decoding filters/batching. EchoWall would own substantially more decoder code.

Consequences: `whispermlx` is used only as a development parity oracle. The
shipped App may not invoke Python, pip, a CLI, ffmpeg, or a local transcription
server. Models are explicit user downloads with source/license/size/hash UI and
are absent from the binary. After installation inference is network-free.
Ambiguous/accepted 妙记 submissions must reconcile or receive an explicit
takeover before local inference; the ledger permits one winning transcript and
one publication. Full local mode targets a similar user-facing deliverable to
妙记: transcript, timestamps, default-on anonymous speaker labels, local model
summary, and the same archive fields in a verified local archive. Different wording, breaks, and speaker
numbering are allowed, but the plan's ground-truth non-inferiority gates must
pass. Inference runs in a signed arm64 one-shot worker inside the App bundle because a
native abort must not take down the Tauri process. The worker has bounded local
IPC, no listener/network/credentials/persistence, and exits after one job; Rust
remains the sole job/effect owner. It is not a service or daemon.

Default-on speaker diarization is a second narrow native adapter on Apple
Silicon macOS 14+: a separately signed one-shot Swift worker containing only the
pinned FluidAudio offline Pyannote + WeSpeaker + PLDA/VBx Core ML source subset.
The model registry, downloader, CLI, ASR, VAD, and TTS are excluded from its
target, and the release build rejects CFNetwork/Network frameworks, URLSession
symbols, URLs, Hugging Face markers, and model-download strings. Its
model pack is an explicit CC-BY-4.0 download. It returns bounded anonymous time
intervals; Rust validates and performs the final temporal merge. A user must
explicitly choose transcript-only output after any diarization failure, and
that result is not labeled full-local parity.

Rejected alternatives for Release 1:

- Candle: technically viable and pure Rust, but its Whisper surface is still an
  example-level integration with more decoding behavior for EchoWall to own.
- `mlx-rs`: useful MLX primitives but no maintained Whisper implementation;
  porting WhisperMLX would recreate model, decoder, timestamps, VAD, and
  long-audio behavior before proving a user-visible advantage.
- Python WhisperMLX: good development oracle, but its interpreter and large
  Torch/Transformers/pyannote runtime violate the no-Python App boundary.

The shared App-owned boundary is implemented in the framework-free
`desktop/local-whisper-protocol` crate and re-exported from
`desktop/src-tauri/src/processing/local_whisper.rs`. Its closed JSON request is
limited to 64KiB and contains only recording/model identity, hashes, sizes,
duration, relative audio path, and optional language. The 16MiB response must
bind the same identities and contain bounded, non-overlapping monotonic
segments. Six protocol tests reject traversal, unknown path fields, oversized framing,
identity substitution, overlaps, segment bombs, total-text bombs, and excessive
segment counts. `desktop/whisper-worker` now implements the independent
no-network Rust/Metal worker, while `processing/local_worker.rs` owns the fixed
path, environment-cleared, bounded-output, timeout/kill-on-drop launcher. The
explicit macOS Tauri config places the worker beside the App executable. The
current universal Developer-ID App and all four two-slice workers pass strict
signing, notarization, privacy/network, staple, and Gatekeeper checks; its DMG
is independently notarized and checked before upload. See
`evidence/macos-distribution-bundle-2026-09-03.md`.

Aggregate-only user-authorized evidence is recorded in
`docs/capture/evidence/local-whisper-2026-09-02.md` and
`docs/capture/evidence/local-diarization-2026-09-02.md`. On the 1,183.68-second
sample, `large-v3-turbo-q5_0` completed in 27.85 seconds at about 1.04 GiB peak
RSS and differed from the existing Miaoji output by 15.03% WER / 8.22% CER.
For speakers, sherpa's macOS prebuilt Core ML request fell back to CPU and
Sortformer over-split; FluidAudio offline Pyannote/WeSpeaker/PLDA/VBx completed
warm inference in 4.11 seconds, automatically found two speakers, and agreed
with 94.44% of the Miaoji-labeled reference segments that had overlapping local
speech. These results choose implementation candidates; they are not
ground-truth parity or Release acceptance.

The v1 processing ledger remains backward compatible: missing route fields
default to `miaoji_remote`. It can now durably select `whisper_local` or the
default-off `qwen_local` candidate from a pre-task state, bind model identity
and audio duration, reconstruct the same worker request after restart,
checkpoint only an identity-valid typed response, and select the local-summary/
local-archive path. Store/engine tests prove zero
TOS/妙记/Gemini/GitHub/R2 effects and one publication, same-model retry after
local failure, and mechanical refusal from `submit_ambiguous`. Explicit
takeover of an accepted task durably marks it superseded and rejects a late
remote transcript.

## ADR-009: end-to-end local mode uses Qwen3.8-27B GGUF and a verified local archive

Decision: on Apple Silicon Macs with at least 32GiB unified memory, the
quality-first full-local summary model is Qwen3.8-27B using Unsloth Dynamic
`UD-Q4_K_XL` GGUF. The exact 17,559,178,144-byte artifact is pinned to SHA-256
`3f227079003add2511437e5b1e94812e363385225bf6a9b47b0054a72bc8b01e`
and source revision `4ca720788d1e01f1bff70c033e0d0028fd02e502`. It runs through
the signed one-shot Rust `llama-cpp-2`/Metal worker; Ollama is a development
oracle only and is not an App dependency or service.

Evidence reviewed and measured 2026-09-03:

- Qwen's official Apache-2.0 model card describes Qwen3.8-27B as a 27B dense
  multilingual model with native 262,144-token context and a `qwen3_5`
  architecture. The official organization publishes BF16 and FP8 weights but
  no official GGUF/INT4 artifact at the pinned revision.
- Unsloth's model card identifies `UD-Q4_K_XL` as its recommended 4-bit GGUF
  and traces it to `Qwen/Qwen3.8-27B`; it requires about 17–19GB total memory.
  Q4_K_M saves only about 1.1GB on disk, which is not a useful trade on the
  named 128GB M4 Max when summary quality is the reason to add a local LLM.
- NVIDIA's own support matrix requires compute capability 10.0+ Blackwell
  hardware for native NVFP4. A Mac Metal worker cannot use that acceleration;
  choosing an NVFP4 file would add conversion/emulation risk with no native
  kernel benefit.
- Ollama 0.33.1 loaded the exact `UD-Q4_K_XL` artifact on this M4 Max and
  completed a fabricated bilingual structured-summary smoke in 17.05 seconds:
  4.62 seconds load, 91.28 prompt tokens/s, and 21.59 generated tokens/s. It
  preserved the fabricated release date, acceptance-test owner/deadline,
  unchanged budget, bilingual summaries, key points, and action items.
- The App worker compiled to a 5.1MB arm64 Mach-O. `otool`/`nm` found only
  system Metal/MetalKit/Accelerate/Foundation/CoreFoundation/C/C++/Objective-C
  dependencies and no CFNetwork, Network, URLSession, socket, connect, or curl
  import. Under `(deny network*)`, the worker produced a closed identity-bound
  summary response in 21.81 seconds with 21,832,056,832-byte peak RSS and zero
  swap. An attempted llama.cpp GBNF sampler crossed the Rust FFI boundary with
  a foreign exception after grammar creation, so it was removed rather than
  shipping an abort path; the worker now uses an exact schema-directed prompt,
  two bounded deterministic attempts, typed `deny_unknown_fields` decoding,
  category/field/array bounds, and main-process identity validation.

Consequences: the complete model pack is about 16.91GiB and installation is
blocked below 32GiB unified memory. Full local completion means local Whisper,
local diarization, local Qwen summary, atomic local note/manifest/audio write,
audio hash verification, and reopenable archive state under network denial.
GitHub/R2 sync is optional later work and cannot block or redefine that state.
Apple Foundation Models remains an optional future optimization because its
availability depends on macOS 26, Apple Intelligence device/region/language
state, and OS-updated model versions. MLX 4-bit is a credible future runtime
alternative only if a same-model/same-transcript benchmark materially beats
the maintained pinned llama.cpp path without bringing a downloader, Python, or
long-running model server into the shipped App.

## ADR-010: Historical Qwen3-ASR candidate decision — superseded above

Decision: implement Qwen3-ASR-1.7B → official
Qwen3-ForcedAligner-0.6B → existing FluidAudio diarization → deterministic
Rust word-to-speaker merge as the next local transcription candidate. Qwen has
no native diarization output. Keep Whisper Turbo selectable until independent
human speaker/transcript truth, code-switch, short-turn, overlap, long-form,
crash, privacy, signing, and bundle gates pass. Freeze the pure-Rust QwenASR
`qwen-asr` 0.11.0 CPU output as the quality baseline; only then evaluate
MLX/Metal speedups. The 0.9.1 CLI is development evidence only and is not
shipped.

The independent aggregate-only evidence is
`docs/capture/evidence/qwen3-asr-diarization-spike-2026-09-03.md`. On the
authorized 1,183.68-second recording, Qwen transcription took 33.07 seconds,
transcription plus forced alignment took 63.81 seconds, and the selected full
pipeline took about 69.3 seconds. FluidAudio `stepRatio=0.15` plus
`minEmbedding=0.4s` and a ≤1-second same-speaker gap bridge assigned 99.04% of
3,123 aligned words while agreement against the non-ground-truth Miaoji
reference remained about 98.39%. The more aggressive 0.2-second embedding
threshold and zero-vote re-embedding were rejected.

Consequences: the next implementation versions the FluidAudio preset, adds
closed Qwen/aligner model and word-timestamp identities, and puts punctuation,
bounded anchor interpolation, unique-overlap assignment, same-speaker gap
bridging, and final regrouping in Rust. `local_unknown` remains explicit.
Timestamp-only candidate spans may later enter the signed Swift worker for
margin-gated short-turn rescue using its existing embedding/centroid
representation; transcript text and raw embeddings never cross that boundary.
Nearest-speaker assignment and textual speaker heuristics are forbidden.

Follow-up evidence closes the earlier transcript-quality omission without
changing the candidate-only status. The same authorized recording and exact
aggregate-only Rust comparator measured Qwen auto at 10.89% WER / 6.12% CER
against the non-ground-truth Miaoji transcript, versus Whisper Turbo Q5 at
15.03% / 8.22%. A separate v1 protocol and 2.0MB Rust one-shot worker now pin
the official model revisions and `qwen-asr` 0.11.0, reproduce all 38 Qwen chunk
texts under network denial, and leave the model directory unchanged. The
worker excludes the CLI/downloader/live surface and forces the library's
default persistent INT8 sidecar off; the in-memory INT8 execution policy and
official BF16 source identities remain separately auditable.

The candidate is now implemented behind a default-off feature. Its separate
6,539,619,722-byte exact model pack passed a real install/proof/remove lifecycle
in 217.64 seconds, and `qwen_local` is a durable ledger backend with retry and
accepted-Miaoji takeover fencing. The current App merge did not reproduce the
spike's 99.04% word-speaker coverage: strict interpolation, point containment,
overlap, and interval-bridge rules assign 3,079/3,123 words (98.59%) and retain
44 as unknown. A full current-build `(deny network*)` run completed the local
archive in 398.39 seconds at 21,851,602,944-byte peak RSS and zero swap, with
no remote checkpoint, but 31/50 canonical segments were unknown chunk
fallbacks. That result proves the offline route and simultaneously keeps the
quality/default gate closed.

At this historical checkpoint Qwen3-ASR-1.7B was the model candidate and CPU
was treated as its reference. The later public matrix supersedes both that
reference and the next-work priority. MOSS-Transcribe-Diarize was tested
as the strongest single-pass speaker/timestamp alternative and rejected: q8
Metal aborted on the M4 Max, while q8/FP16 CPU both measured 31.48% WER,
20.72% CER, and 89.52% Miaoji-reference speaker agreement on the same
authorized first 60 seconds. This evidence is detailed in the same spike file.

The pinned FluidAudio revision also contains LS-EEND, but its own model guide
still selects Offline VBx for best complete-file quality and describes LS-EEND
as more false-alarm-prone/less stable outside heavy overlap. EchoWall keeps
Offline VBx and adds LS-EEND only to the overlap/short-turn human-truth matrix.
The vendored Community-1 config already emits exclusive segments, so the
current Qwen unknown-chunk problem must be split into forced-aligner timing and
word-text reconstruction causes before changing speaker assignment. For later
Apple acceleration, Apache-2.0 `soniqo/speech-swift` v0.0.27 is the first named
challenger because it provides native MLX Qwen3-ASR and MLX/Core ML
ForcedAligner paths. Its project benchmarks use converted weights; adoption
would now require comparison against independent MLX Qwen output if Qwen work
regains priority. The current MOSS candidate is governed by the 2026-09-05
decision at the top of this ADR.

## Remaining Phase 0 gates

| Gate | Current evidence | Exit evidence |
|---|---|---|
| Embedded processing | Rust ledger, direct TOS/妙记/Gemini adapters, secure-store setup, archive CAS publisher, and Tauri commands pass offline; an 18.034-second two-voice fabricated route passes live TOS → 妙记 → Gemini → R2/GitHub → fresh refresh plus exact TOS/archive/Keychain cleanup; broker prototype removed | long-duration provider/rate proof and physical lifecycle recovery |
| macOS capture | selected-App ScreenCaptureKit audio was falsified; the opt-in macOS 14.2+ Core Audio process tap uses a tap-only private aggregate and passes short isolation, pause/resume, zombie-safe source exit, plus physical-mic 30-minute and two-hour generated selected-App runs at 0 ms/2 ms end drift after callback-stall, host-time, and real-time-safe discontinuity fixes | real Zoom/Teams and selected-browser exclusion, output/mic route, sleep/source-restart, signed-App recovery, and separate two-hour Voice Memo/System Capture; System Capture remains ScreenCaptureKit |
| Browser capture | exact-tab is NO-GO; selected-browser source contract and warning exist | physical selected-browser exclusion/stability proof |
| Windows capture (Release 2) | official API, implementation, Windows GNU compile, and unsigned CI lane | named Windows 11 physical matrix; does not block Release 1 |
| iOS capture | iOS 26.5 simulator handoff passes; Apple has the Share bundle and explicit shared-group associations; fresh App Store profiles, distribution archive, and exported IPA prove matching effective entitlements; current-Team development cert/profiles produce a debug archive that installs/cold-starts on the named iPhone; physical App Group injection completes staging → Rust adoption → ack cleanup; UIScene warning is tracked in ADR-007 | direct system Files/Voice Memos Share Sheet, physical export, upstream-supported UIScene path on an enforcing SDK, and fabricated-source or explicitly authorized physical two-hour lifecycle matrix |
| Android capture/import | API 36 Pixel 8 emulator build/install/cold-start and audio share-intent resolution pass with mic denied; four App-module connected tests pass real MediaStore import replay/ack, real MediaStore export, no-host-audio foreground recording/WAV/pause/resume/stop/journal, and inactive-service stale-snapshot interruption/WAV repair; a separate emulator-only host harness proves PID/snapshot → `am force-stop` → new-process recovery and native ContentResolver → Rust inbox → ack | Pixel/OEM physical two-hour, screen-off, task-removal, real process recreation, route, permission, and system picker/share matrix |
| Import/decoder | desktop/mobile Rust copy, Symphonia probe, review metadata, dedupe, recovery, and native entrypoints pass offline | physical mobile picker/share proof plus long/oversized fixture matrix |
| macOS local STT fallback | Codeberg whisper-rs 0.16.0/whisper.cpp is selected; independent arm64 Metal worker, protocol, fixed launcher, universal signed/notarized App placement, aggregate-only private A/B, model-pack UI, durable full-local route, accepted-task superseding takeover, and network-denied two-hour resource stability pass; no-Python/no-network dependency boundary is enforced | ground-truth parity/Mandarin/nonrepetitive long-form selection and live in-flight poll/takeover timing proof |
| macOS local diarization | FluidAudio stays selected; SpeakerKit remains replayable. MOSS improves four short-form strata, but Mixed count/terms and the completed six-hour whole-file baseline fail. Raw-marker correction has a separate40-case artifact set; bounded-window ASR/global attribution and App integration remain open | relative count/attribution, coverage/short-turn truth, native integration, crash isolation, signing/bundle proof;95% exact count is separate stretch work |
| Processing | 242 Rust App library tests (219 default-pass, 23 explicit model/permission/live-provider ignores) plus 12 Whisper/aligned-word-protocol, 4 Qwen-protocol, 6 Qwen-worker, 3 summary-protocol, 3 Whisper-worker, 2 summary-worker, 8 offline quality-evaluator, and 1 Swift-worker tests cover direct effects, restart fences, pre-dispatch upload rejection/retry, repeated HEAD-only canceled-upload reconciliation, ordered destination-bound edit outbox, field-level remote merge/CAS, immutable/reused R2 identity and attempt cleanup, transcript-reusing reprocess, exact TOS cleanup after cancel, durable symlink-safe local discard/replay, capture auto-enqueue recovery, schema-parity and bounded fail-closed capture/inbox journals, closed local inference framing/launcher, full-local zero-provider execution, verified local archive, resumable hash-bound model download, separate Qwen and SpeakerKit candidate catalogs/lifecycles, durable default-on Whisper and default-off candidates, accepted-task superseding takeover, explicit transcript-only acceptance, versioned diarization presets, fail-closed aligned-word speaker bridging/point containment, hash-bound no-network worker dispatch, replayable VAD-utterance language policy, native TOS4 signing against an official-SDK vector, zero-vulnerability audit, unlinked private diarization scratch audio, frozen aggregate-only quality grading, callback-stall detection, shared-host-time segment correction, real-time-safe discontinuity drafts, bounded source icons, desktop tray lifecycle, native release switches, three-format import limits, source retention, two-phase mobile acknowledgement, secure credential boundaries, one-host GitHub tarball redirects, current `volc.lark.minutes`, closed shared summary schema, R2-only delete handling, two exact-confirmation live full-chain proofs, a 7,140-second rate-limited Miaoji proof, and the private 44-case quality runner | physical suspend/relaunch recovery, local quality completion, and independent human adjudication |

## Prototype evidence and pivot status

The reusable Rust-side foundation exists:

- `schemas/recording-envelope-v1.json` is the closed boundary artifact;
- `desktop/src-tauri/src/ingest/` owns schema/state parity, atomic app-data
  inboxes, streaming hash/recovery, desktop/mobile import, and typed local job
  commands;
- `desktop/src-tauri/src/capture/` and the local Tauri plugin own desktop and
  mobile native capture boundaries;
- secure-store support now stores user-supplied TOS, 妙记, Gemini, GitHub, and
  R2 credentials behind write-only commands on macOS/iOS, Android, and the
  Release 2 Windows target; status returns booleans only and compile-time secret
  seeding is removed;
- `desktop/src-tauri/src/processing/` owns the atomic effect ledger, minimal
  native TOS4 adapter derived from the official Apache-2.0 signing source,
  妙记 submit/query/transcript, Gemini text summary,
  direct GitHub/R2 archive publication, multi-device manifest merge/CAS,
  Tauri commands, and launch-time resume;
- provider submit ambiguity, pre-dispatch rejection, exact TOS
  version/ETag/CRC/SHA proof, cleanup, publication conflict, same-second
  collision, user-field preservation, and redaction invariants now have Rust
  coverage.
- the authorized short live fixture also completes the direct chain against the
  existing private accounts and self-cleans. It fixed six live-only drifts:
  Rust-SDK file hash signing, missing TOS versioning/lifecycle, obsolete
  `auc_turbo` resource selection, Gemini/local-summary schema divergence,
  GitHub's private-tarball redirect, and deletion of an R2-only path from the
  Git tree. See `evidence/live-provider-chain-2026-09-03.md`.

The unshipped Python `broker/`, Python `processing/`, broker CLI/runtime,
requirements, and related tests were removed after their reusable failure cases
received Rust coverage. `ingest/gateway.rs` and the broker-token shim were also
removed; the shared UI command names now resolve to the embedded engine.

The legacy `transcribe.py`, `volc_lark.py`, and `deliveries/` remain because the
proven Voice Memos watcher still uses them. Their existence is expected; new
App jobs must have tests proving they do not spawn or import that Python path.

Current reproducible evidence:

```text
venv/bin/python3 -m pytest tests/ -q -W error
44 passed, 16 subtests passed

cd desktop/src-tauri
cargo check && cargo test && cargo clippy --all-targets -- -D warnings
The current App library suite has 227 tests (216 pass and 11 opt-in ignored),
plus 11 Whisper/aligned-word-protocol, 4 Qwen-protocol, 5 Qwen-worker,
3 summary-protocol, 3 Whisper-worker, 2 summary-worker, 7 offline
quality-evaluator, and 1 Swift-worker tests. The earlier public
568MiB speech/diarization manager install/removal test passed explicitly. The
complete 18,154,818,756-byte production catalog passed real download, all
23-file installation, a second exact `proof()`, and exact removal in 632.39
seconds at 1,046,495,232-byte peak RSS with zero swap; its isolated root was
absent afterward. The separate 6.09GiB Qwen
candidate pack completed its opt-in install/proof/remove test in 217.64
seconds. The real macOS source-icon test passed explicitly in 3.02 seconds;
source enumeration and the separate capture spike remain opt-in in the default
suite and run only under their native prerequisites
cargo check --target x86_64-pc-windows-gnu

cd desktop
npm run tauri:build:macos -- --debug --bundles app
The 101MB debug bundle contains four arm64 one-shot workers and passes an
explicit deep ad-hoc signature verification. A later universal release build
contains x86_64+arm64 slices for the App and all four workers and passes
Developer ID signing, App and DMG notarization/stapling, privacy/network scans,
and Gatekeeper acceptance; see
`evidence/macos-distribution-bundle-2026-09-03.md`.
npm run tauri ios build -- --debug --target aarch64-sim --no-sign --ci --archive-only
npm run tauri android build -- --debug --target aarch64 x86_64 --apk

LOCAL_ARCHIVE_DIR=<fabricated-demo> venv/bin/python3 -m deliveries.viewer
venv/bin/python3 scripts/demo/test_capture_ui.py --directory <fabricated-demo> --output <temporary-output>
capture UI smoke passed, including zero-recording first-run and write-only provider/archive setup
mobile smoke also proves iOS and Android stable-item acknowledgement after Rust adoption,
iOS native Files picker adoption, and native iOS recovery with or without WebView localStorage
Android unit/lint and the two-ABI APK compile the Rust-owned SAF routes;
the API 36 app-module instrumentation suite imports a valid fabricated WAV from
a real MediaStore `content://` URI with exact name/byte/hash preservation,
stable replay, and acknowledgement cleanup; it also writes 64 KiB of fabricated
bytes from the real `dataDir/inbox/<uuid>` boundary to a MediaStore URI,
reopens an exact byte/hash match, and removes both artifacts. User picker
selection remains in the physical device matrix
iOS device-target and simulator archive compile the native verified-document
export route and disjoint native-session root; physical picker proof remains open
```

The local Tauri 2.11.5 source defines iOS `app_data_dir` as the platform data
directory plus the bundle identifier. EchoWall's Swift bridge now mirrors that
contract explicitly as `Application Support/ai.ax.watch-transcriber`; capture
sessions under `native-capture/sessions`, Share/picker staging under
`shared-imports`, the Rust `inbox`, and export source validation
therefore agree on one bundle-scoped root. Bare-Application-Support locations
are migration inputs only, never new-write destinations.

Simulator runtime evidence used only fabricated data and no microphone/provider
access. The ordinary simulator archive's effective code-signing entitlements
were empty, and App Group lookup correctly failed; that build is not App Group
proof. A copy under `/tmp` was ad-hoc re-signed with the exact repository-owned
main/Share entitlements and installed over the same disposable simulator data.
With a stable staged 659Hz synthetic M4A plus its metadata sidecar, cold launch
produced exactly one additional Rust `inbox/<uuid>/recording.json` with
`platform=ios`, `kind=file_import`, `state=ready`, `confirmed_at=null`, and
`imported_name="Synthetic 659Hz Voice Memo.m4a"`. Both staged audio and sidecar
were removed by native acknowledgement. This proves App Group entitlement,
bundle-scoped path, command ACL, full decoder/adoption, metadata preservation,
and cleanup wiring in the simulator; it does not substitute for distribution
profiles, Share-sheet invocation, physical Files/Voice Memos, or lifecycle
acceptance.

A second run exercised the missing cross-container hop. The simulator's unique
App Group container was resolved from its container metadata with
`MCMMetadataIdentifier=group.ai.ax.watch-transcriber`; an 880Hz synthetic M4A
and per-item sidecar were placed under `share-inbox`. On launch, the Rust inbox
package count increased from three to four, the new package preserved
`imported_name="App Group Synthetic 880Hz.m4a"`, and App Group audio/sidecar plus
main-App staging audio/sidecar were all absent after acknowledgement. This adds
runtime evidence for App Group → bundle-scoped staging → Rust adoption → native
cleanup. Direct invocation of the system Share sheet and the signed physical
device path remain open gates.

The 2026-09-03 physical-device preflight found the paired iPhone 17 Pro Max
available over USB and three valid local signing identities. It first proved
that the old main profile lacked `group.ai.ax.watch-transcriber` and that the
Share bundle/profile did not exist. The subsequently authorized mutation
registered the Share bundle and App Group, associated both bundle IDs, and
generated fresh main/Share App Store profiles. A distribution-signed archive
and locally exported IPA now prove matching effective App Group entitlements
for both targets; the verified profiles were also installed in the two GitHub
release secrets. `evidence/ios-app-group-signing-2026-09-03.md` records the
aggregate proof and the Xcode/Homebrew rsync export fix. Physical Share Sheet,
Files/Voice Memos, and lifecycle evidence remained open at that point. A new
current-Team development certificate and two device profiles subsequently
produced a strict-signature-verified debug archive that installed and
cold-started on the named iPhone. A fabricated M4A/receipt injected into the
physical App Group completed native staging → Rust adoption → acknowledgement
with both source files removed. Direct system Share Sheet invocation and the
lifecycle matrix remain open.

After the mobile command surface was narrowed, both `main` capabilities were
inspected with no `echowall-capture:*` grant. The viewer invokes only App-owned
Rust `mobile_*` commands. A further App Group run with distinct 1047Hz synthetic
audio increased the simulator inbox from four to five packages, preserved
`imported_name="Rust Wrapper Synthetic 1047Hz.m4a"`, and cleared all staging.
This proves the Rust wrapper/ACL path still reaches native drain and ack; the
unit policy test separately proves recording/import-off rejects new native work
without disabling recovery helpers.

Android uses ABI product flavors, so only `:app:*` tasks are EchoWall App
evidence. Dependency modules such as `:tauri-android` and
`:tauri-plugin-dialog` do not count. Release CI runs the explicit arm64 App
unit/lint tasks and assembles its instrumentation APK. Tauri must build the
arm64 debug JNI/App first; the follow-up Gradle test command excludes only the
already-completed `:app:rustBuildArm64Debug` task because invoking it outside
Tauri's WebSocket orchestrator fails by design. Connected execution uses
`:app:connectedArm64DebugAndroidTest -x :app:rustBuildArm64Debug` on
`mio_api36_pixel8` / API 36.

That App task runs four tests and passes 4/4. Import writes a valid fabricated
WAV to MediaStore, copies the real `content://` source into the stable native
inbox, verifies its original display name/size/SHA/bytes, proves URI replay
returns the same import ID, then acknowledges and removes only the copied file.
Export writes 64 KiB of fabricated bytes from the real Rust inbox through a
MediaStore URI, reopens an exact match, and removes source/destination.

A 2026-09-03 clean rerun after the embedded-processing and macOS changes again
passed the arm64 Tauri APK build, exact `testArm64DebugUnitTest`,
`lintArm64Debug`, and 4/4 `connectedArm64DebugAndroidTest` tasks on
`mio_api36_pixel8` / API 36. The follow-up host harness passed both real
foreground-recording `am force-stop` → recreated-process recovery and stable
ContentResolver staging → process death → Rust inbox adoption → native
acknowledgement cleanup. The test packages were uninstalled and the emulator
was shut down without saving the test snapshot. Generic `testDebugUnitTest` is
not a valid command after ABI flavors; use the exact tasks above.
Recording runs the actual microphone foreground service while the emulator is
launched with `-no-audio`, so no host/personal input exists. The test proves the open WAV continues growing
after `finishAndRemoveTask()` and after screen sleep while durable state remains
`RECORDING`; it wakes the device, transitions through pause/resume/stop, verifies
durable events and the repaired WAV byte count, then removes the capture tree.
The stale-snapshot test starts from a fabricated persisted `RECORDING` snapshot
with no active service. Native reconciliation marks it `INTERRUPTED`, repairs the WAV
to its exact PCM-backed length, records the interruption event, and leaves it
available to the Rust pending-session path. Rust coverage then proves that
finalization to `Ready` removes both the native session directory and only the
matching terminal active-session snapshot, including the crash window where
the directory is already absent. This is deterministic stale-state recovery
evidence. A second, deliberately host-orchestrated harness is excluded from the
ordinary suite because its two methods must run in separate processes. On the
same clean API 36 emulator, `scripts/demo/test_android_process_death.sh`
confirmed the recorder PID and on-disk `RECORDING` snapshot while phase one was
still blocked, issued `am force-stop ai.ax.watch_transcriber`, confirmed the
PID disappeared, and invoked phase two in a newly created process. Phase two
launched the real Tauri Activity; its WebView called the App-owned Rust
`mobile_status` wrapper, whose native plugin reconciled the snapshot. The host
then observed exact session/WAV recovery to `INTERRUPTED` plus the durable event
without the test calling `recoverInterrupted()` directly. The harness refuses
non-emulator devices and restores a clean package state. A final clean-sandbox
pair writes a fabricated WAV through MediaStore, stages it with the native
picker-copy path, confirms the stable copy, and force-stops before Rust
adoption. A newly created process then launches the real Tauri Activity and
observes a Rust-owned `inbox/<recording-id>/recording.json` and hash-matching
normalized track before the native staging copy disappears by acknowledgement.
This proves both pre-adoption process-death replay and the Android native → Rust
wrapper → inbox → ack handoff on the emulator. It remains distinct from real
system-picker UI and the Pixel/OEM physical acceptance matrix.
The first App run exposed Android's legitimate
`/data/user/0` → `/data/data` canonical alias; validation now matches raw and
canonical allowed inbox roots before rejecting symlinks component-by-component.

The macOS generated-source harness is
`scripts/demo/test_macos_capture_spike.sh`; it refuses to prompt for TCC and
routes only 443Hz/997Hz fixtures to named BlackHole/Steam virtual devices. On
this host, 5–8 second runs exercised the real cpal microphone and
ScreenCaptureKit selected-application backend and produced separate PCM WAVs.
Mic/system end timestamps differed by 3–25ms, but first callbacks differed by
1.1–1.4s. The backend now emits a `macos_audio_startup_delay` gap from the
session clock to each delayed first callback instead of fabricating samples.
The spike remains intentionally failing: BlackHole's input returns silence,
and the selected-application system track contains both the selected 997Hz tone
and an unrelated 443Hz fixture at material amplitude. Moving the fixture
window on-screen and ad-hoc signing the two Apps did not change that result.
No raw probe artifact is retained. This evidence falsifies a Meeting GO on this
host; it proves only the native path, honest startup-gap handling, and end-clock
alignment until a working isolated fixture/device path passes the full matrix.

A 2026-09-03 rerun first fixed a harness-only cold-build race by compiling the
test binary before starting its duration-bounded fixture Apps. The actual
eight-second probe then reproduced the blocker: 1,435ms explicit start drift,
5ms end drift, selected 997Hz amplitude 0.149995, unrelated 443Hz amplitude
0.150000 in the selected-App system track, and zero mic input from BlackHole.
The temp App/audio root was removed. Apple's macOS 14.2+
[Core Audio process tap](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps)
then passed a separate 8-second spike at 0.149677 selected versus 0.000011
unrelated amplitude (~82.7dB isolation). The narrow Swift/C ABI was wired into
the existing Rust capture session for Meeting only. Product-path 8- and
60-second reruns produced 0.149683/0.000010 and 0.131288/0.000001 selected/
unrelated amplitudes, 9–15ms end drift, explicit startup-gap events, and no
remaining tap, aggregate device, temp App, or audio artifact. The bridge is
compiled with a macOS 13 deployment target and runtime-blocks Meeting below
14.2; Voice Memo and ScreenCaptureKit System Capture retain macOS 13 support.
The five-minute loss and failed immediate recreation were subsequently traced
to the private aggregate's unnecessary binding to the current default output
subdevice. On this host that was an HDMI display, adding an unrelated device
clock/lifecycle dependency to a tap that is already a complete aggregate input.
The production and independent bridges now create tap-only private aggregates.
Product runs pass 8 seconds, 300 seconds, immediate 8-second recreation, a
4s/pause/2s/resume/6s two-segment sequence, and selected-source termination.
The latter also replaced `kill(pid, 0)`, which treats zombies as alive, with
`proc_pidinfo(PROC_PIDTBSDINFO)` plus `SZOMB` rejection and produced the exact
durable `SourceLost` event. Detailed aggregate-only evidence is in
`docs/capture/evidence/macos-process-tap-output-clock-2026-09-03.md`.
`ECHOWALL_PROCESS_TAP_ENABLED` remains explicit opt-in even on macOS 14.2+
until the uninterrupted 30-minute/two-hour/browser/route/sleep matrix passes.
BlackHole and Steam virtual microphone inputs still return silence, so
system-only results do not establish a combined Meeting GO.
`scripts/demo/test_macos_process_tap_spike.sh` retains the independent
BSD-licensed reproduction; both harnesses use only temporary signed tone Apps
and remove their audio/device artifacts.

Live level telemetry now follows the consent boundary. No preflight path opens
an audio device; after explicit Start, `CaptureStatusDto` exposes normalized
mic/system RMS from macOS callbacks, iOS calls AVAudioRecorder metering on its
serialized coordinator queue, and Android computes bounded PCM16 RMS inside the
foreground service. Paused/stopped Android reads are rejected after the
blocking AudioRecord callback returns, which also closes a discovered race that
could append one post-pause buffer. Missing/recovered streams return no meter
instead of a made-up value, and macOS levels decay to zero after 750ms without a
callback. The shared viewer renders accessible thin dB traces plus the exact
mic/source labels; fabricated Playwright desktop/mobile journeys and visual
inspection pass. Physical non-silent iOS/Android meter movement remains part of
their device matrices.

Desktop permission UX is now closed at both the native and IPC boundaries.
`capture_permissions` remains read-only; the only prompting command accepts two
booleans, rejects an empty request and a disabled recording feature, and on
macOS calls AVFoundation/CGRequest only after the explicit dialog action. The
settings command accepts an enum and internally chooses the fixed macOS or
Windows settings URL, so the viewer cannot supply a URL. Both commands are
granted to the authenticated loopback viewer capability but no raw mobile
plugin surface; navigation remains pinned to the per-process capability path
and synced executable HTML is ignored. Fabricated browser journeys cover
not-determined → explicit request → refreshed sources and denied → fixed
settings routing. The real prompt was not invoked during automation.
The source macOS Info.plist and rebuilt App bundle contain explicit
`NSMicrophoneUsageDescription`, `NSScreenCaptureUsageDescription`, and
`NSAudioCaptureUsageDescription` copy naming the selected-app/whole-browser/all-
system scopes; no generic or hidden recording purpose string is used.

Storage preflight now matches the two-hour promise instead of relying on the
first failed write. Desktop computes the worst-case 48kHz stereo PCM footprint
from the requested one/two tracks, adds the normalized 16kHz output and a
512MiB reserve, and checks the real inbox volume before creating a journal.
iOS reports important-usage volume capacity and Android reports `StatFs`
capacity; both native preflight and Start enforce a 1GiB floor. Android's
foreground service rechecks independently and maps capacity loss during an I/O
failure to `recording_storage_full`. Unit thresholds, iOS archive compilation,
Android unit/lint, API 36 App tests 4/4, the host crash/import harness, and a
fabricated low-storage UI journey pass. Actual exhaustion during long physical
recording remains in each platform lifecycle matrix.

Desktop recovery no longer reduces durable evidence to `warningCount`.
`CaptureStatusDto` includes exact mic/source labels, total gap milliseconds, and
only the latest 16 validated notices with session-relative timestamps. The
viewer restores this state directly from Rust even with no WebView localStorage,
maps known interruption/startup/storage codes to bounded guidance, and displays
the recovered gap duration. Fabricated UI coverage proves an interrupted
Meeting restores `Recovered Mic · Recovered Zoom`, the durable recovery notice,
and a three-second explicit gap.
The recovery controls now follow the same state contract: only a live iOS
coordinator interruption exposes Resume. Desktop recovered/source-lost and
Android interrupted sessions display a disabled “cannot continue” control and
direct the user to stop/save safe segments before starting another source;
Playwright covers both desktop and mobile recovered states.

The macOS Meeting picker now loads only the selected catalog App's icon through
a closed Tauri command. AppKit converts the OS icon to PNG, a bounded decoder
resizes it to 32px, and Rust/JavaScript both accept only a small PNG data URL;
no arbitrary PID, path, or URL crosses the command. Source enumeration stays
prompt-free and avoids eagerly decoding every running App. A real TCC-granted
macOS smoke found a decodable bounded icon in 2.46 seconds without logging App
names; a fabricated Playwright journey visually verifies icon/name/whole-browser
scope and explicitly says audio activity is checked only after Start.

Desktop capture recovery now treats its persisted files as a bounded
private-data boundary. `capture-session.json` is limited to 1MiB, 512 segments,
and 128 combined gaps/notices; `events.ndjson` is limited to 8MiB, with 1MiB
reserved before each snapshot mutation. Recovery rejects oversized input before
parsing, preserves both snapshot and journal on failure, and never applies
incomplete-tail repair to an over-limit journal. The final inbox boundary also
caps `recording.json` at 1MiB, each event at 64KiB, and the shared journal at
8MiB. Rust validation now enforces the schema's existing 32-track/128-warning
limits, and mobile session adoption uses the same track limit. Seven negative
and schema-parity tests cover these cases.

The intentionally unsigned macOS debug App bundle builds with fabricated archive
data and no credentials. The iOS
simulator archive command exits zero with `--archive-only` and contains an arm64
`EchoWall.app` with `EchoWallShare.appex`. That App also installs and cold-starts
on the iPhone 17 / iOS 26.5 simulator and renders the in-App setup surface;
ADR-007 records the observed UIScene warning and future-SDK gate. IPA export
remains part of the later signed release gate. The 64-bit Android APK contains
arm64-v8a and x86_64, installs and cold-starts on the API 36 Pixel 8 emulator
with microphone denied;
single and multiple audio share intents resolve to `MainActivity`, and no fatal
app crash appears. After the two-phase mobile-import acknowledgement change,
the APK was rebuilt, reinstalled, and again completed a cold start with
microphone and notification permissions denied. No provider, signing, release,
personal microphone input, or personal-data action was performed.

A separate release-privacy pass staged all 6,536 Git-visible files
(800,168,315 bytes); gitleaks parsed 186,551,713 bytes and reported zero
findings, after which the temporary staging/report were removed. The first
release bundle still embedded the local builder's `/Users/...` source paths in
Rust and ggml/whisper.cpp `__FILE__` strings. The canonical npm build now
chains a dynamic rustc path-remap wrapper, Clang file/debug/macro prefix maps,
and release symbol stripping without replacing caller flags or an existing
rustc wrapper. A clean native-sys rebuild produced a 34 MB App whose main
executable and four workers contain zero `/Users/` paths, credential/private-
key markers, Python files, credentials, or model weights. The checked-in
`verify_macos_bundle_privacy.sh` repeats those checks, the four-worker/network
boundary, and deep signing in release CI after notarized build and before
upload.

This establishes the embedded processing, capture/import base, simulator build
paths, and current universal macOS distribution artifact. It does not
substitute for native physical capture matrices, physical mobile picker/share
paths, Windows Release 2 runtime, or the long provider/rate and mobile-resume
proofs.

Phase 0 closes only when every gate either has its exit evidence or the
governing plan is explicitly narrowed by AX. Missing hardware is a blocker for
that platform's physical acceptance, not permission to substitute compilation
or simulator evidence.
