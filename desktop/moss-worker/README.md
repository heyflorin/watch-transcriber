# MOSS candidate one-shot worker

Apple-Silicon-only Rust executable using pinned transcribe.cpp v0.2.3 / Metal.
No Python, CLI transcription subprocess, network client, downloader, queue,
credentials, Tauri dependency, or selected App route. Other targets fail with
`unsupported_platform`.

One bounded JSON request on stdin, one complete validated JSON response on
stdout, then exit. Errors are closed codes on stderr; native logging is disabled.
`../local-moss-protocol` owns pinned identities, recording-relative paths,
output limits, and joint-speaker timing adaptation. Aborted/token-truncated
generation must fail instead of publishing a partial transcript as complete.
The `explicit-model-boundaries-v1` raw parser uses the model's own markers,
not transcribe.cpp's unknown-time repair. Missing timing fails; no last turn
is stretched to the audio duration. Old benchmark reports without
`raw_timing_policy` are not evidence for this guard.

`ECHOWALL_APP_DATA_ROOT` must be an absolute non-symlink App data directory.
The model is at `models/moss/<MODEL_ID>/model.gguf`; the source is below the
request's `inbox/<recording_id>/`. Both are regular files verified by size and
SHA-256. The native model parser receives a private 0700 temporary directory's
read-only, rehashed snapshot, not a mutable shared model pathname. macOS
`/dev/fd/N` aliases cannot be used here because repeated native opens share
their cursor. Normal exit removes only this worker's temporary snapshot.

The previously private decoder now lives in `../local-audio`, shared with App
preprocessing. Model inference and verified-file/snapshot ownership remain
isolated here; Tauri never depends on this worker crate.

Current candidate limitations:

- WAV decoding and 8–192 kHz / up to eight-channel normalization have bounded
  tests. Rubato 0.16.2 FFT conversion removes filter delay and preserves exact
  duration; 16 kHz mono PCM bypasses filtering. A synthetic AAC/MP3 padding
  fixture passes with explicit packet/simple MP4 edit trimming. Complex MP4
  edits fail explicitly. A public AAC pilot also completes under network
  denial; App import integration remains open for MOSS.
- Model snapshotting needs additional temporary disk space. App-owned crash
  recovery must clean orphaned scratch by ownership, not a broad deletion.
- The caller must enforce deadlines, cancellation/reaping, and resource limits.
- App model catalog/lifecycle, launch/retry/ledger/archive integration and
  signing/bundle validation are not implemented for MOSS yet.

```sh
cargo test --locked --release
cargo clippy --locked --all-targets -- -D warnings

# Development-only FFmpeg encoding to temporary files; no playback.
ECHOWALL_CODEC_TEST_CONFIRM=synthetic-codec-files-authorized \
cargo test --manifest-path ../local-audio/Cargo.toml --locked --lib \
  synthetic_aac_and_mp3_preserve_duration_and_signal \
  -- --ignored --nocapture

# Opt-in PUBLIC corpus only; no playback, uploads, or real App data.
# First generate/verify the immutable source-duration audit from repo root:
# node local-eval/audit_source_durations.mjs
ECHOWALL_MOSS_NATIVE_CONFIRM=public-corpus-native-worker-authorized \
ECHOWALL_MOSS_NATIVE_CASE=english_01 \
cargo test --locked --release --test public_native -- --ignored --nocapture
```

For the fixed-window ASR experiment, additionally set
`ECHOWALL_MOSS_NATIVE_WINDOW_INDEX=0` (zero-based; eight windows for a
90-minute case) with an allowlisted public case ID. Windows are fixed at12 minutes,
except the last remainder, and copy exact PCM samples using test-only Hound.
The explicit `ECHOWALL_MOSS_NATIVE_WINDOW_POLICY=quiet12m` experiment instead
chooses the lowest-energy 300 ms span in the previous five seconds, if its RMS
is at most 164/32768; otherwise it keeps the fixed cut. All32 such windows of
four public90-minute files complete. No human labels or playback are involved.
Artifacts are labeled `windowNN` and record original PCM/container durations,
offsets, and source hash. This is **not cross-window speaker reconciliation**:
do not treat per-window speaker slots as global identities or claim a complete
quality pass. The last window follows actual PCM length rather than inventing
the AAC container's64ms padding.
Current native probes bind both original AAC and WAV hashes to the separate
source-duration audit. Historical matrix timelines include old importer codec
padding (up to127.25ms); they are not used as physical decode durations. Frame
coordinates and final ceil accounting preserve sub-millisecond tails. A public
2.691-second Mixed tail completes; this is not a full tiny-tail quality gate.

Shared adaptation supports optional
`joint-adjacent-union-unknown-tail100-v2`: adjacent, known same-ID overlaps can
coalesce while preserving joined text and interval union. Legacy v1 remains
the default test request policy. A retained-response replay proves six joins
across32 windows; it is not fresh inference. Measured global anchor mapping
and lexical coverage remain separate from native worker proof and App selection.
The shared `local-moss-protocol::windows` streaming planner independently
reproduces all32 retained cut boundaries; `::speakers` reproduces all5,588
measured v3 assignments using only timing/anonymous slots. Both expose bounded
cancellation-aware pure APIs; neither creates App jobs or selects a backend.

The integration harness runs the worker with a cleared environment under a
network-denying macOS sandbox, adopts public input/model into its own temporary
App-shaped tree, enforces a 30-minute timeout and bounded files, and retains
per-attempt evidence under ignored
`local-eval/matrix/outputs/moss-native-worker-v1/`. Existing evidence is never
overwritten. The exact executable is copied/hashed before launch; a worker
guard kills/reaps on errors before temporary-tree cleanup.
See `../../docs/capture/evidence/moss-public-quality-2026-09-05.md` for results,
model identity/license, and remaining product gates.
