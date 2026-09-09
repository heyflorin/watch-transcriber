# Shared local audio decoding

Rust-only Symphonia decoding, simple MP4 edit/priming handling and Rubato
anti-aliasing conversion to16kHz mono. Extracted from the measured MOSS worker;
App preprocessing and the isolated worker can use the same implementation
without linking model inference into Tauri. No model, network client, audio
device, queue or Python dependency.

The caller passes a verified regular `File`, a format-hint name and bounded
expected duration. It owns path/size/SHA-256 identity checks before and after.
`ContainerTimeline` is read before descriptor handoff; `DecodedFrameTimeline`
selects validated edits or packet trims exactly once. Import inspection shares
this selector and counts effective frames without full-file buffering or
resampling. Padded historical evaluation durations are not physical decode
durations. Final duration is ceil-ms with original sample frames retained.
Decoding handles8–192kHz and up to8channels, rejects non-finite/format-changing
input, trims only supported simple edits, and enforces100ms duration tolerance.
16kHz mono PCM bypasses filtering. The current API returns bounded full-file
samples; streaming App-owned PCM materialization is a separate integration step.

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings

# Generated temporary codec files only; never opens an audio output.
ECHOWALL_CODEC_TEST_CONFIRM=synthetic-codec-files-authorized \
cargo test --locked synthetic_aac_and_mp3_preserve_duration_and_signal \
  -- --ignored --nocapture
```

Native worker input/model verification, model snapshot lifetime and inference
remain in `../moss-worker`; this crate must not take over those responsibilities.
