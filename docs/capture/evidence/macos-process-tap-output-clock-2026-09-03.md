# macOS process-tap output-clock investigation — 2026-09-03

## Scope

This investigation used only temporary ad-hoc-signed tone applications and
443 Hz / 997 Hz synthesized audio routed to local virtual Core Audio devices.
It did not record a physical microphone, browser, meeting, or personal audio.
Every test root, App, WAV, process, private tap, and aggregate device was
removed after its run.

## Symptom

The product-wired Core Audio process tap had previously passed 8- and 60-second
selected-process isolation tests, then produced no system segment in a
five-minute run. An immediate eight-second recreation and the independent
reproduction also returned no useful tap audio. Repeating short runs without a
changed hypothesis was not accepted as recovery proof.

## Root cause and fix

The process tap was added to a private aggregate device together with the
machine's current default system-output subdevice. The tap is already a
complete aggregate input; the extra output introduced an unrelated device
clock and lifecycle dependency. On this host the default output was an HDMI
display. If that device changed availability, both the long-running IOProc and
immediate recreation could lose buffers even though the selected process and
tap still existed.

The bridge now creates a tap-only private aggregate: it keeps the tap UUID,
tap auto-start, private flag, and drift-compensation declaration, but contains
no main subdevice, output UID, or subdevice list. The experimental environment
branch used to falsify the hypothesis was removed; tap-only is the sole product
implementation. The independent BSD-licensed reproduction uses the same
shape. See Apple's
[Core Audio tap documentation](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps)
and the lifecycle reference in [insidegui/AudioCap](https://github.com/insidegui/AudioCap).

A second defect affected source-exit recovery. `kill(pid, 0)` treats an
unreaped zombie as present, so the App could miss that the selected meeting
source had exited. macOS liveness now uses `proc_pidinfo(PROC_PIDTBSDINFO)` and
rejects `SZOMB` or a missing process. The physical fixture synchronizes its
termination timer to a post-`backend.start()` marker and requires the exact
`SourceLost` event rather than a generic gap.

## Evidence

| Product-path fixture | Selected 997 Hz | Unrelated 443 Hz | End drift | Result |
|---|---:|---:|---:|---|
| tap-only, 8 seconds | 0.149998 | 0.000006 | 20 ms | pass |
| tap-only, 300 seconds | 0.149999 | 0.000000 | 18 ms | pass |
| immediate tap-only recreation, 8 seconds | 0.149996 | 0.000013 | 16 ms | pass |
| permanent implementation, 8 seconds | 0.149997 | 0.000013 | 10 ms | pass |
| pause 4 s → pause 2 s → resume 6 s | 0.149998 minimum per segment | 0.000019 maximum | 16 ms | pass; two aligned segment pairs |
| selected source exits 4 s into a 10 s session | 0.058472 over the partial source lifetime | 0.000008 | not an end-alignment gate | pass; exact durable `SourceLost` |

The independent tap-only reproduction also passed at 0.149998 selected versus
0.000004 unrelated amplitude over 383,488 frames. After the 2026-09-04 physical
long-run fixes, the final 218-test App library suite passes 211 with seven
explicit model/permission/live-provider ignores; Clippy,
Intel macOS compile, the focused native fixtures, debug bundle build, and deep
ad-hoc signature verification also pass.

## Remaining gates

- The process-tap zero-buffer/recreation blocker is resolved. A physical-mic
  30-minute and two-hour generated selected-App matrix now passes after adding
  callback-stall detection, host-time segment correction, and real-time-safe
  discontinuity rotation. `ECHOWALL_PROCESS_TAP_ENABLED` remains explicit
  opt-in until real Zoom/Teams/browser, route, sleep, and remaining mode proofs
  pass. See `macos-physical-long-capture-2026-09-04.md`.
- This host's BlackHole and Steam virtual microphone loopbacks both produce
  zero input through the real cpal path. System-only results do not prove a
  dual-track Meeting GO; use a working isolated mic fixture or an explicitly
  authorized physical microphone.
- The authorized built-in-mic retry ultimately completed but was digital
  silence because the MacBook was in clamshell mode, which hardware-disconnects
  its internal microphone. AVFoundation independently measured the same -91 dB
  result. `Elgato Wave XLR MK.2` then passed the bounded five-second physical
  Voice Memo fixture and the 30-minute/two-hour Meeting fixtures. No captured
  audio was retained or uploaded.
- Whole-browser process-tree capture, output-route change, sleep/wake, and
  repeated source restart still require physical evidence.

## Signed App UI and microphone-permission follow-up

A current Developer-ID-signed and notarized App was launched with a fresh
archive directory. The first attempt still shared the production bundle
identifier and therefore its App-data root; two Accessibility snapshots
unintentionally enumerated existing private archive titles while looking for
the recorder. No note body, transcript, audio, credential, or provider payload
was read or transmitted. UI inspection stopped immediately, the title values
were not reused, and all later work used a distinct fabricated test bundle ID
plus a fresh App-data root.

The isolated signed App exposed a separate first-run defect: the setup page is
served from the authenticated loopback origin, but
`archive-viewer-remote.json` omitted `allow-initialize-local-archive`. The UI
therefore reported failure before Rust ran even though the local capability
contained the permission. The remote capability also omitted three other
commands used by the compiled viewer: explicit transcript-only acceptance,
Whisper takeover, and post-local cloud backup. All four grants are now present;
the remote capability remains loopback-only and still excludes
`opener:default`. A regression test requires the exact command grants.

After rebuilding the signed isolated App, clicking “先创建本机档案” produced
the visible success state and opened a real zero-entry local archive. The empty
viewer also exposed `NaN%` in its night-recording statistic; the denominator is
now guarded and the archive test requires the zero-safe template.

The real recorder then exposed all three modes, and `MacBook Pro Microphone`
was selected explicitly. The native permission refresh still returned
“授权尚未生效”, reset the source selection, and kept Start disabled. No audio
file was created. The test process, distinct App-data root, empty archive, and
test App bundle were permanently removed. This historical result was superseded
by the authorized 2026-09-04 external physical-mic proof; the built-in device's
later digital silence was separately explained by clamshell hardware
disconnection.

On 2026-09-04, System Settings showed that the current GUI host already had
microphone permission; no toggle was changed. A later Accessibility snapshot
of the non-isolated existing EchoWall instance again enumerated private archive
titles while verifying that it was the old recorder-disabled shell. No item was
opened, no body/audio/credential was read or transmitted, and UI work stopped
immediately. All capture evidence after that point used only generated Apps,
aggregate metrics, and temp roots.
