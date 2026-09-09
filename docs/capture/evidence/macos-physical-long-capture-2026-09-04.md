# macOS physical-microphone long-capture evidence — 2026-09-04

## Scope and privacy boundary

AX explicitly authorized the bounded physical-microphone and Release 1 matrix
work. The successful Meeting fixtures selected a generated local application
that emitted only a 997 Hz tone and used `Elgato Wave XLR MK.2` as the physical
microphone. Only aggregate duration, level, frequency-isolation, timing, signal
count, process memory, and cleanup results were printed. No captured audio was
played back, transcribed, uploaded, published, or retained. However,
the selected-App 997 Hz output was initially routed through `Steam Streaming
Speakers`; Wave Link monitored that virtual route into AX's normal output, so
the tone was audibly leaked even though the harness itself did not open a
playback path. AX reported the beep during a later System Capture attempt. The
attempt was terminated immediately, counted as no evidence, and its processes
and temp root were verified absent. Every completed generated App, raw
track, capture directory, tap, aggregate device, and process was removed after
each attempt.

Both generated-audio harnesses now require the additional exact guard
`ECHOWALL_TEST_TONE_SINK_CONFIRM=non_monitored` and otherwise refuse to start.
The main fixture defaults to `BlackHole 2ch`, but a virtual-device name alone
is never treated as proof that the user's monitoring graph is isolated.

This evidence proves the product Core Audio process-tap path with a real
microphone. It does not substitute for real Zoom/Teams, selected-browser,
route-change, sleep/wake, or the separate two-hour Voice Memo and System
Capture acceptance runs.

## Physical microphone diagnosis

The first authorized five-second `MacBook Pro Microphone` retry completed the
cpal start/record/stop lifecycle but contained digital silence. An independent
AVFoundation five-second capture also completed and measured exactly -91 dB
mean/max. `ioreg` then reported `AppleClamshellState = Yes`: the MacBook lid was
closed, so the built-in microphone was hardware-disconnected. This ruled out
the App capture implementation.

The same five-second Rust fixture on `Elgato Wave XLR MK.2` passed at 48 kHz,
two channels, 5,002 ms, RMS 0.00008248, and peak 0.00051881. The temp WAV was
deleted immediately.

## Long-run failures found and fixed

The physical-mic Meeting harness was extended to 7,200 seconds. Physical mode
does not launch the 443 Hz virtual-microphone tone App; it requires non-zero
physical mic RMS/peak while preserving the selected-App 997 Hz isolation gate.

Long runs then exposed three independent defects:

1. A first 30-minute attempt received no system callbacks even though tap
   creation returned success; only the mic segment existed at Stop. The App now
   tracks callback activity. After three seconds without a callback it records
   an explicit gap and restarts the process tap once; after another three
   seconds it emits `macos_process_tap_no_audio_callbacks` instead of silently
   recording an empty Meeting source. The harness polls native signals during
   the run, matching the App session loop.
2. A complete two-track 30-minute attempt ended at 103 ms drift, three
   milliseconds above the frozen 100 ms gate. Track metadata had discarded
   callback host-time and derived its end solely from frame count and nominal
   sample rate. It now retains the shared host-time boundary. The normalized
   provider copy resamples each segment to that bounded timeline; raw PCM is
   unchanged.
3. A subsequent run received a real callback discontinuity at about 16
   minutes and crashed with `EXC_BAD_ACCESS`/`SIGILL`. The macOS crash report
   placed the audio IO thread in `TrackRecorder::close → hash_file`: the first
   discontinuity implementation synchronously hashed a large WAV from the
   real-time callback and its 1 MiB stack buffer crossed that thread's guard
   page. A callback now only rotates an `OpenSegment` into an in-memory draft.
   WAV header finalization, sync, and heap-buffered hashing run later on the
   Pause/Stop control thread. A discontinuity also closes the timing segment so
   the normalized mix cannot stretch across a real missing interval. A format
   change is rejected before rotation and can never be hidden as a new segment.

Deterministic tests cover watchdog restart/failure/recovery, host-time frame
correction without raw-metadata mutation, discontinuity draft finalization,
and format-change precedence. After the later long-provider/cancel-race work,
the current App library suite contains 227 tests: 216 pass and eleven explicitly
ignored model/permission/live-provider tests.
Clippy passes with warnings denied.

## Passing physical results

| Fixture | End drift | Physical mic | Selected 997 Hz | Unrelated 443 Hz | Result |
|---|---:|---:|---:|---:|---|
| 8 s regression | 31 ms | RMS 0.00061635 | 0.149997 | 0.000009 | pass |
| 60 s regression | 25 ms | RMS 0.00656007, peak 0.11407819 | 0.106849 | 0.000002 | pass |
| 30 min uninterrupted | 0 ms | RMS 0.00578808, peak 0.31199072 | 0.075683 | 0.000000 | pass |
| 2 h uninterrupted | 2 ms | RMS 0.01220827, peak 0.79357280 | 0.096084 | 0.000001 | pass |

A separate physical Voice Memo run passed 7,220.754 seconds across nine
explicit discontinuity segments at 48 kHz stereo, RMS 0.00275160, peak
0.37586596, and 84 recorded signals. It completed in 7,269.21 seconds including
control-thread finalization and aggregate validation. Its temp root was absent
after the pass.

The two-hour test remained one process/session for 7,200 seconds and completed
in 7,361.23 seconds including control-thread finalization and aggregate scans.
It emitted 56 explicit startup/discontinuity signals and rotated both roles at
the same observed boundaries; no `SourceLost`, callback-stall failure, crash,
or hidden system-only downgrade occurred. Peak observed recorder RSS stayed
about 124 MiB while temporary audio grew beyond 1.7 GiB at the 81-minute
checkpoint. The final temp root was absent and free disk space returned after
the pass.

## Remaining macOS acceptance

- Repeat selected-process capture with current native Zoom and Teams and prove
  exclusion of a simultaneous unrelated application.
- Run selected Chrome and Edge application capture with the exact other-tabs
  warning and a distinct non-browser exclusion signal.
- Exercise output-route change, microphone route change, sleep/wake, selected
  source restart, and the recovery UI on the signed App.
- The separate two-hour Voice Memo mode now passes. Whole-system mode remains
  open and may not restart until its generated source is proven to use a truly
  non-monitored sink; the stopped audible-tone attempt is no evidence.
