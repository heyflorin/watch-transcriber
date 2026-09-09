# CAPTURE-0: Recording and file ingestion release roadmap

- Status: v0.3.0 preview code and distribution delivered; physical phone tests deferred by AX. Remaining live Mac audio checks require a safe monitoring setup. Windows remains Release 2.
- Owner: AX
- Linear umbrella: [AX-237](https://linear.app/ax-agent-swarm/issue/AX-237)
- Plan issue: [AX-238](https://linear.app/ax-agent-swarm/issue/AX-238)

## Release delivered — 2026-09-08

Code is committed/pushed, [v0.3.0 downloads are published](https://github.com/xingfanxia/watch-transcriber/releases/tag/v0.3.0),
and iOS0.3.0(3) is available to existing internal TestFlight groups. Remote builds
and the corrected full native test suite pass; see the
[release evidence](evidence/release-0.3.0-2026-09-08.md). The original Watch
recording automation is still loaded/configured and its actual-environment
health check passes. App-only preferences do not change that automation.

AX explicitly deferred physical phone tests. The earlier requirement to wait
for tao0.37 is superseded by the [supported current iOS release decision](evidence/ios-supported-release-path-2026-09-08.md):
Xcode26 is accepted now; scene migration belongs to a future SDK27 upgrade.
The remaining live Mac system/source audio tests still need an independently
safe non-monitored setup or the pending authorization to temporarily mute
monitoring. They have not been run or quietly labelled passed.

## Acceptance and closeout — AX decision, 2026-09-06

AX's acceptance standard is **“表现能和飞书差不多就可以接受了”**.
This is a delivery task. Finish a usable EchoWall with representative
Feishu/Miaoji-comparable transcription, speaker attribution and summaries.
The existing measurements support ending model-selection research and moving
to application integration, packaging and ordinary-use verification.

Quality acceptance is a practical judgment about the deliverable:

- Representative English, Mandarin and mixed-language meeting notes preserve
  the main content, useful anonymous speaker attribution and a usable summary.
- Differences in wording, punctuation, segment boundaries, occasional names or
  terms, and anonymous speaker counts are acceptable when the result remains
  useful and broadly comparable to Miaoji.
- WER/CER, DER/JER, exact counts, lexical coverage and term recall remain honest
  diagnostic evidence. No fixed 95%/99% score, per-stratum scorecard, isolated
  term deficit, fresh all-44 pass, or blinded/two-reviewer study is a release
  prerequisite. Do not edit frozen reports or label unmeasured results passed.
- Widespread omissions, unusable speaker attribution, misleading summaries,
  crashes, hangs, data loss, duplicate charges and privacy violations remain
  concrete defects. Fix a demonstrated ordinary-use blocker with the smallest
  coherent patch and a focused check; do not reopen general model research.
- An isolated difficult fixture with a clear error, preserved source and usable
  recovery/export can remain a named limitation. It does not automatically
  block delivery or authorize a new segmentation architecture.

This section and Phase 1A define current quality acceptance. Historical reports
and `docs/evals/full-local-quality.md` describe measurements and earlier targets;
they do not reinstate a research backlog or additional shipment gates.
Capture consent/scope, audio integrity, credential isolation, durable recovery,
platform readiness and external-action authorization retain their requirements.

## Current implementation and retained evidence

Finish the existing MOSS route; keep Whisper available as the existing fallback
and Qwen ASR/ForcedAligner as retained alternatives. No new model port, precision
comparison, threshold sweep or fine-window profile is required for closeout.

| Concern | Current source choice / evidence |
|---|---|
| Local ASR | MOSS 0.9B Q8, pinned transcribe.cpp v0.2.3 / Metal |
| Window policy | Existing quiet12 policy; no fine-window production edits landed |
| Timing | Explicit chronological v3; immutable raw emission order and permutation provenance; old policies retain their behavior |
| Speakers | Single-window adapted MOSS identities; multiwindow graph-v3 reconciliation with explicit SpeakerKit tail-context-v2 |
| Summary/archive | Pinned Qwen3.8-27B UD-Q4_K_XL / llama.cpp; Rust-owned validated local publication |
| Compatibility | Preparation/plan pins survive restart; old schemas/policies are not silently rewritten |
| Source checks | 337 App tests passed, 31 opt-in tests not run as part of that suite; strict clippy/formatting, 9 packaging/QA-path checks, Python44+16 subtests, ruff and both UI suites pass |
| Practical end-to-end proof | Current bundled release workers completed 184 transcript segments, 7 summary fields, source-hash-matching local archive and idempotent resume under OS network denial |
| Quality evidence | 44 native diarization cases completed; 42 retained-ASR App-finalizer replays remain comparable to Miaoji, including Mixed count 8/10 vs 8/10 |
| Known limits | Original all-44 ASR attempt had two failures. Timing v3 repaired the failing long window; the 180 ms tail case and the unselected finer-window feasibility probe stay recorded. No full-current-44 ASR success is claimed |

Evidence lives in:

- `evidence/moss-uniform-diagnosis-2026-09-06.json` — current source checks,
  raw/canonical timing proof, release worker identities and latest end-to-end run.
- `evidence/speakerkit-tail-context-2026-09-06.json` — native 44-case speaker
  result, actual App-finalizer replay and fixed aggregation/ordering defects.
- `evidence/moss-public-quality-2026-09-05.md` and linked artifacts — historical
  experiments, retained failures and provenance; consult only for a concrete
  delivery question.

Current artifact and real isolated-App UI proof is recorded in
[practical-closeout-2026-09-06.md](evidence/practical-closeout-2026-09-06.md).
Supported Macs now expose MOSS; native storage preserves the explicit
new-recording choice across cold launches. Unselected queued recordings no
longer upload on startup or UI timers. Existing jobs keep their route/policies.

## Completed closeout and remaining work

Completed on 2026-09-06:

- The current isolated QA App imported Mandarin01 and Mixed01 through the real
  native picker and completed MOSS/speakers/summary/local archive;202 and100
  segments respectively,7 summary fields each. The retained English184-segment
  generation also opened in the current App; current English source proof is
  reused. This is not a new all44 run.
- Summary/transcript viewing, original export and same-bundle cold reopen pass.
  All3 archive entries and source/inbox/archive hashes match; ledgers, notes,
  manifest and native preference remain unchanged after cold launch, with no
  worker or QA HTTP attempt. Actual WebKit radio layout was fixed and checked.
- A signed/notarized universal macOS DMG, signed Android universal APK and fresh
  App Store iOS IPA/archive are retained with exact hashes and setup notes in
  the linked closeout evidence. Both READMEs describe the final user path.

Remaining work is limited to the readiness table's physical capture/import/
lifecycle checks and explicitly authorized distribution steps. No test App or
model worker remains running. Production installation/launch and public upload
were not performed. The iOS App Store IPA is not directly sideloadable.

Reuse these passing checks while the relevant code is unchanged. Missing
hardware or prohibited OPPO lock/task-removal holds the affected claim; it does
not create another model, fine-window, threshold or review phase. Keep AX-237
and its concrete issues aligned with this partial/held platform state.

## Outcome

EchoWall becomes the capture and ingestion front end for the existing archive:

```text
native recording or file import
  → durable local recording package
    → Rust core embedded in this App process
      → direct TOS upload → 妙记 submit/poll
        → Gemini title/summary
          → GitHub/R2 archive → EchoWall refresh
```

The transcription stage is deliberately simple: after the App has durably
adopted a new recording or imported file, it uploads that artifact directly to
TOS and submits/polls 妙记. Gemini summary and GitHub/R2 publication are
downstream App-owned stages, not another transcription backend or an
EchoWall-operated service.

The same recording contract is shared by macOS, Windows, iOS, Android, a Voice
Memos share action, and ordinary audio files. Delivery is intentionally staged:
Release 1 ships macOS, iOS, and Android; Windows is the next release and does
not block Release 1. Platform code owns capture and permissions. One shared Rust
engine inside the Tauri app owns upload, provider calls, retries, idempotency,
archive publication, and delivery ordering. EchoWall does not require a
self-hosted broker or processing server, and this roadmap contains no server
component to build, deploy, operate, or test as an App dependency.

The authenticated in-process loopback transport used by the existing viewer may
continue to serve App UI/assets and narrowly scoped mutations. It owns no job,
queue, provider credential, upload, transcription, summary, or publication work
and is not the processing layer.

## Locked product decisions

1. macOS ships the following three user-visible recording modes in Release 1.
   Windows targets exact product parity in Release 2, after the same behavior is
   proven on Windows 11 hardware:

   | Mode | Captured tracks | Intended use |
   |---|---|---|
   | Voice Memo | microphone | thoughts, interviews in the room |
   | Meeting | microphone + one selected meeting source | Zoom, Teams, native meeting apps, or one selected browser application |
   | System Capture | microphone + all system output | demos or sessions spanning several apps |

2. Desktop Meeting captures the narrowest practical Release 1/2 source. Native
   meeting apps are captured by application/process. Browser meetings capture
   the entire explicitly selected Chrome/Edge application and warn that other
   audible tabs may be included; exact-tab capture is deferred beyond these
   release gates. Selected-browser capture remains narrower than System
   Capture, which intentionally records every application.
3. iOS and Android v1 record the microphone only. Mobile system/call capture is
   not required for launch.
4. Every platform supports direct audio-file import. Desktop also supports
   drag-and-drop and batch selection. iOS supports Files plus a Voice Memos
   share extension. Android supports the Storage Access Framework plus a share
   target.
5. Recording always starts from an explicit user action and always shows an
   unambiguous recording indicator. There is no hidden meeting detection or
   silent automatic recording.
6. The current Voice Memos → Mac `launchd` path remains operational until the
   new path has production parity. It is an input adapter, not the future state
   authority. New iOS work does not depend on background discovery of Apple's
   Voice Memos library: users either record inside EchoWall or explicitly
   share/import an audio file.
7. The default STT route is the current simple route: upload the normalized
   recording to TOS, submit/poll 妙记, and consume its transcript and speaker
   labels. Senko, pyannote, and chunked Gemini/OpenAI transcription are not in
   the default path.
8. Windows is explicitly the next release target. Its existing shared contracts,
   WASAPI implementation, Credential Manager backend, and CI lane stay in the
   tree and must keep compiling, but Windows packaging and physical acceptance
   are not Release 1 completion gates. Cross-compilation on macOS is evidence of
   code shape only, never evidence of Windows runtime behavior.
9. The product remains Rust + Tauri. Swift on iOS and Kotlin on Android are
   narrow native capture/import adapters; shared ingest and processing stay in
   Rust. Go + Wails is not part of either release train.
10. The new App path has no project-owned server. TOS, 妙记, Gemini, GitHub, and
    R2 remain external APIs. Users provide their own credentials during setup;
    the app accepts them through a write-only setup command, stores them only in
    the platform secure store, and never embeds them in a public binary. The
    legacy Python watcher remains a compatibility path, not part of the new App
    processing engine. A new App job must not call an EchoWall-owned HTTP
    endpoint, start a Python/FastAPI worker, or depend on a localhost/LAN/cloud
    processing sidecar. Suspension recovery belongs to the app-owned durable
    Rust ledger and resumes on the next permitted background opportunity or app
    launch.
    The default App route is exactly `new local package → direct TOS upload →
    妙记 submit/poll → Gemini summary → native GitHub/R2 archive publish`.
    Its STT sub-route is `new local package → TOS → 妙记`; the later
    stages consume the stored transcript. No separately deployed EchoWall
    runtime sits between any of those steps.
11. Android Release 1 is 64-bit: `arm64-v8a` for physical devices and `x86_64`
    for emulators, matching the current reference hardware and verified native
    toolchains. The retired TOS SDK's 32-bit CRC build failure is no longer an
    architectural constraint, but `armv7` and `i686` remain unadvertised and
    outside the Release 1 gate until a named supported device requires them and
    the complete capture/secure-store/processing path is proved there.
12. The legacy Voice Memos watcher keeps its existing delivery order. The new
    embedded App publisher uses a stricter cross-service commit order: stage
    local derivatives, PUT/HEAD-verify immutable R2 audio, then publish the
    GitHub manifest with non-force CAS, then clean the temporary TOS object.
    This intentional App-only safety difference prevents an authoritative
    manifest from pointing at audio that never became durable. Explicit
    reprocess reuses the already verified R2 object; if that object is missing,
    a journaled replacement generation is uploaded before the new Git CAS.
13. App file imports do not invoke ffmpeg or pretend compressed bytes are WAV.
    Rust/Symphonia parses and fully decode-validates `.m4a`, `.mp3`, and `.wav`
    within the provider's `<5h`/`<512MB` limits, then preserves that verified
    container as the canonical provider artifact. The TOS key and Content-Type
    match its real format. Native desktop/mobile capture still finalizes a
    canonical WAV. The legacy watcher may keep making its 16kHz mono MP3 copy.
14. Recording, direct import, embedded processing, and selected-browser capture
    have independent release/runtime kill switches. Defaults are on;
    `ECHOWALL_RECORDING_ENABLED`, `ECHOWALL_IMPORT_ENABLED`,
    `ECHOWALL_PROCESSING_ENABLED`, `ECHOWALL_BROWSER_CAPTURE_ENABLED`, and
    `ECHOWALL_LOCAL_STT_ENABLED` may
    be set to `0`/`false`/`off`/`no` in the build or desktop launch environment.
    Native commands enforce the switches; disabling processing never blocks
    cancel cleanup, export, discard replay, or archive viewing.
    The macOS process-tap Meeting backend remains opt-in until its remaining
    physical matrix passes; only an explicit
    `ECHOWALL_PROCESS_TAP_ENABLED=1|true|on|yes` enables it after the OS 14.2
    gate. Its former five-minute zero-buffer blocker is fixed by a tap-only
    private aggregate. A physical external mic plus generated selected-App now
    passes uninterrupted 30-minute and two-hour Meeting runs at 0 ms and 2 ms
    end drift. Those runs exposed and fixed no-callback stalls, nominal-frame
    clock drift, and real-time-thread hashing on discontinuity. Real Zoom/Teams,
    browser, route, sleep, and the separate two-hour Voice Memo/System Capture
    proofs remain. See
    `evidence/macos-physical-long-capture-2026-09-04.md`.
15. Preserve the existing pinned `whisper-rs` / `whisper.cpp` Metal worker as
    the selectable Apple-Silicon fallback. No new Candle, MLX, Core ML speed
    comparison or Python runtime is required for this delivery.
16. Finish full local processing with MOSS Q8 / transcribe.cpp, the implemented
    versioned mapping/timing policies and the offline SpeakerKit subset where
    used. Apply the practical quality standard above. Existing evidence is
    sufficient to end model-selection research; final UI/installed-App and
    package checks determine readiness. Anonymous labels are not personal
    identity claims. Do not guess names or silently turn a failed speaker stage
    into a successful full-local result; transcript-only recovery is explicit.
17. Full local mode includes the pinned Qwen3.8-27B Unsloth Dynamic
    `UD-Q4_K_XL` summary worker through llama.cpp/Metal and a verified App-private
    audio/note/manifest archive. The quality tier retains its 32 GiB memory
    requirement and explicit model installation. It makes no TOS, Miaoji,
    Gemini, GitHub or R2 call to complete; optional backup/sync is a later,
    separately enabled effect. Keep the existing summary schema and identities.
18. Qwen3-ASR/ForcedAligner and FluidAudio remain preserved alternatives and
    compatibility paths. Their old short-turn, coverage, MLX-port and comparison
    work is not a prerequisite for finishing MOSS. Reopen an alternative only
    for a demonstrated ordinary-use blocker that a smaller fix cannot address.
19. Keep model bytes, runtime policies and saved job identities distinct.
    Existing plans/requests retain their policy and hashes after upgrades.
    New policy choices are explicit before dispatch. Do not alter historical
    measurements, make implicit model downloads, or reopen model-selection
    research to chase perfect scorecards.

## Assumptions

- Initial deployment is single-user and private, but multiple devices may
  submit concurrently.
- Release 1 is single-user/private. Each installation is configured with the
  user's own least-privilege provider and archive credentials; no shared vendor
  secret is compiled into or distributed with the app.
- The existing generated archive, `manifest.json`, private notes repository,
  R2 audio archive, and ordered delivery chain remain authoritative outputs.
- English and Chinese UI copy continue to live in the same product; the capture
  UX reuses EchoWall's existing graphite visual language and mobile navigation.

The Rust engine is one shared implementation compiled into every platform; it is
not four independent pipelines. Each device owns only its durable local queue.
Remote archive publication uses optimistic generation/ref checks so concurrent
devices either serialize successfully or surface a recoverable conflict.

## What already exists

- `transcribe.py` owns discovery, state, provider routing, summarization, note
  formatting, and the delivery handoff.
- `volc_lark.py` owns full-recording MP3 preparation, TOS upload/presign,
  妙记 submit/poll/result parsing, cleanup, and timestamp formatting.
- `deliveries/` owns the load-bearing archive order. `manifest.json` is the
  archive authority and user-authored fields survive reprocessing.
- `desktop/src-tauri` already builds the EchoWall Tauri shell for macOS, iOS,
  and Android. Mobile already has secure token storage, direct-pull sync, R2
  streaming, cache, and offline pinning.
- `deliveries/viewer_template.html` is the one desktop/mobile viewer and its
  existing list/detail, player, sync pill, light/dark, and responsive patterns
  should be extended rather than forked.
- `docs/mobile/PLAN.md` records the current read-only mobile implementation and
  its platform landmines.
- No repository `DESIGN.md` exists. The viewer template's tokens and shipped
  screenshots are the visual source of truth for this milestone.

## Current gaps and platform readiness

The remaining work is integration and delivery, not an open model-selection
program. Keep the shared Rust engine, compatibility fixtures and completed
quality work; repair only a concrete affected behavior.

| Area | Retained evidence | Remaining closeout |
|---|---|---|
| Shared ingest/providers/archive | Rust ledger, import, dedup, recovery and bounded self-cleaning live provider runs exist | Verify the final App uses this path, run affected checks and fix regressions |
| Full local | Final model-choice UI, durable explicit preference, actual isolated-App Mandarin/Mixed import→result/export/reopen, current signed Mac artifact and practical quality evidence pass | Production-installation claim remains separate; no further model research is scheduled |
| macOS capture | Physical Voice Memo and long Meeting core have passing evidence | Authorized real-app/browser exclusion, route/source-restart and signed-App recovery checks; System Capture long run only with a proven non-monitored source |
| iOS | App/Share signing, physical install/cold launch and staged-import evidence exist | Real Files/Voice Memos Share Sheet and required physical lifecycle checks; supported release-toolchain cold launch |
| Android | Emulator recovery/import/export checks and bounded OPPO physical checks exist | Remaining real picker/share/export and supported lifecycle proof; do not repeat OPPO task-removal/lock under current authority |
| Distribution/docs | Current signed/notarized Mac DMG, signed Android APK, App/Share-signed iOS IPA/archive, exact hashes and bilingual setup/readiness docs are retained | Physical-platform acceptance and explicitly authorized public/TestFlight/store distribution remain open |

Physical evidence is not replaced by a UI mock, emulator, compilation or
CoreDevice injection. AX's quality clarification does not waive capture safety
or imply device-lock/audio-playback authorization. Reuse passing long-run
results while their relevant source is unchanged. If a required physical step
is unavailable or prohibited, finish independent delivery work, identify the
held capability and needed external step, and do not restart model research.

Source-specific historical evidence:
`evidence/macos-physical-long-capture-2026-09-04.md`,
`evidence/macos-process-tap-output-clock-2026-09-03.md`,
`evidence/android-oppo-physical-2026-09-04.md`,
`evidence/ios-app-group-signing-2026-09-03.md`,
`evidence/macos-import-ui-2026-09-03.md`,
`evidence/live-provider-chain-2026-09-03.md`,
`evidence/long-miaoji-provider-2026-09-04.md`, and
`evidence/macos-distribution-bundle-2026-09-03.md`.

## Capability matrix

| Capability | macOS | Windows 11 | iOS | Android |
|---|---|---|---|---|
| App-owned mic recording | required | required | required | required |
| Continue while app not frontmost | normal desktop process | normal desktop process | `UIBackgroundModes=audio` | microphone foreground service |
| Native meeting-app output | default-off Core Audio process-tap experiment on macOS 14.2+ | WASAPI process loopback | not v1 | not reliable; not v1 |
| Browser meeting output | selected Chrome/Edge application, with other-tabs warning | selected Chrome/Edge process tree, with other-tabs warning | not v1 | not v1 |
| Whole-system output | ScreenCaptureKit display audio | WASAPI endpoint loopback | not v1 | not v1 |
| Separate mic/system tracks | required | required | mic only | mic only |
| File picker import | required | required | required | required |
| Drag-and-drop batch import | required | required | optional tablet enhancement | not applicable |
| Share target/import extension | optional | optional | required | required |
| Crash-recoverable recording | required | required | required | required |
| Release CPU targets | universal macOS | x86_64/arm64 in Release 2 | arm64 | arm64-v8a + x86_64 emulator |

Windows 11 is the desktop baseline for Meeting mode. Whole-system WASAPI
loopback works on older Windows, but process-loopback requires build 20348 or
later; a single product contract is clearer than a partially disabled Windows
10 Meeting mode.

## Architecture

### Ownership boundaries

```text
┌──────────────────────────────────────────────────────────────────────┐
│ Platform capture adapters                                            │
│ macOS CoreAudio/SCK    │ Windows WASAPI │ iOS AVFAudio │ Android     │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ track segments + metadata
┌──────────────────────────────▼───────────────────────────────────────┐
│ Shared in-app Rust core                                              │
│ RecordingEnvelope │ inbox │ mix │ durable job/effect checkpoints    │
│ TOS upload │ 妙记 poll │ Gemini summary │ archive CAS/publish       │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ direct external API calls
┌──────────────────────────────▼───────────────────────────────────────┐
│ User-configured external services                                    │
│ TOS │ 妙记 │ Gemini │ private GitHub archive │ private R2           │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ immutable archive/result
┌──────────────────────────────▼───────────────────────────────────────┐
│ Existing archive + EchoWall viewer                                   │
└──────────────────────────────────────────────────────────────────────┘
```

### Why processing is embedded

The selected design ports the active TOS → 妙记 → Gemini → archive path once
into the shared Rust crate and runs it in the app. It matches the single-user,
private deployment, removes an always-on operational dependency, and lets an
iPhone or Android device finish work without waiting for a Mac-owned worker.

Mobile operating systems may suspend or kill the app. The design does not
pretend otherwise: every effect is checkpointed locally before and after the
network call, provider work continues remotely, and the app resumes upload,
polling, summary, publication, or cleanup when background time is granted or on
the next launch. Completion latency may increase while suspended; durability
must not decrease.

Public builds contain no project/vendor secrets. Setup imports the user's own
TOS, 妙记, Gemini, GitHub, and R2 credentials into Keychain, Android
Keystore-backed storage, or Windows Credential Manager. Credentials are scoped
to the smallest practical bucket/repository permissions. Values cross IPC only
as input to an explicit write-only setup command, are never returned to the
webview or exposed to a browser extension, and can be replaced or deleted
locally.

The legacy Python watcher and delivery code remain operational while parity is
proved. They are compatibility/reference implementations only: a new App job
must not invoke Python, FastAPI, a localhost broker, or a remote EchoWall
control plane.

### Runtime topology invariant

The shipped path has exactly one processing owner: the Rust core compiled into
the Tauri application. The release artifact contains no EchoWall service
binary, service launcher, broker client, or server deployment configuration.

| Responsibility | Runtime owner |
|---|---|
| Record, import, normalize, queue, retry, and recover | Tauri App + narrow Swift/Kotlin/native capture adapters |
| TOS upload, 妙记 submit/poll, Gemini summary, archive publication, cleanup | embedded Rust processing engine |
| Job/effect state | app-private durable local files |
| Secrets | platform Keychain/Keystore/Credential Manager, reachable only through native write-only setup commands |
| Remote data plane | the user's TOS, 妙记, Gemini, private GitHub, and private R2 accounts |
| Existing Voice Memos compatibility | legacy Mac Python watcher only; never called by a new App job |

No processing behavior depends on a browser extension, EchoWall-hosted queue,
broker, processing API, daemon, or always-on Mac. The authenticated in-process
loopback viewer transport is allowed only as a presentation/asset boundary; it
cannot own processing state or receive provider credentials.

### Boundary artifact: `RecordingEnvelope` v1

Each capture or import creates a directory in the app-owned inbox:

```text
inbox/<recording-id>/
  recording.json
  tracks/
    mic-0001.m4a
    system-0001.m4a
  derived/
    mixed.m4a
  events.ndjson
```

`recording.json` has a versioned schema:

```json
{
  "schema_version": 1,
  "recording_id": "uuidv7",
  "source": {
    "kind": "desktop_meeting",
    "platform": "windows",
    "label": "Microsoft Teams",
    "capture_scope": "process_tree"
  },
  "captured_at": "2026-09-02T09:00:00-07:00",
  "ended_at": "2026-09-02T10:00:00-07:00",
  "duration_ms": 3600000,
  "tracks": [
    {
      "role": "microphone",
      "relative_path": "tracks/mic-0001.m4a",
      "codec": "aac",
      "sample_rate": 48000,
      "channels": 1,
      "sha256": "..."
    },
    {
      "role": "system",
      "relative_path": "tracks/system-0001.m4a",
      "codec": "aac",
      "sample_rate": 48000,
      "channels": 2,
      "sha256": "..."
    }
  ],
  "normalized_audio": "derived/mixed.m4a",
  "normalized_sha256": "...",
  "imported_name": null,
  "capture_warnings": [],
  "job": {
    "state": "ready",
    "attempt": 0,
    "remote_job_id": null,
    "last_error": null
  }
}
```

Rules:

- Paths are relative and canonicalized inside the recording directory.
- `recording_id` identifies the ingest job. `normalized_sha256` provides
  content deduplication across devices and renamed imports.
- `events.ndjson` records device changes, pauses, interruptions, recovered
  segments, upload attempts, and provider transitions without rewriting history.
- The public schema and Rust validator both cap an envelope at 32 tracks and
  128 capture warnings. `recording.json` is read/written through a 1MiB bound;
  each inbox event is at most 64KiB and the shared `events.ndjson` is at most
  8MiB. Oversized persisted input is rejected before unbounded allocation.
- Provider-facing and AI-facing inputs are retained long enough to replay a
  failed stage without recapturing or reuploading.
- Existing `manifest.json` keeps its current external key format and gains
  additive App ownership/proof fields: `recording_id`, `publish_generation`,
  `r2_key`, `r2_generation`, `audio_sha256`, and `audio_size_bytes`. Legacy
  readers ignore them; the Python compatibility path preserves them when it
  re-renders an App-owned entry. The publisher serializes writes. A same-second
  key collision advances only the archive key to the next unused second while
  preserving the true `captured_at` in the entry.

### Job state machine

```text
draft
  → recording/importing
  → finalizing
  → ready
  → uploading
  → queued
  → transcribing
  → summarizing
  → publishing
  → complete
```

Recoverable side states:

```text
interrupted │ offline │ upload_failed │ provider_failed │ publish_failed
recovered   │ canceled_before_upload  │ canceled_after_upload
```

Contract:

- Every transition is durable before its side effect begins.
- `recording_id` is the idempotency key for create/submit/publish.
- Retries resume from the last verified artifact; they do not repeat capture or
  regenerate a valid 妙记 transcript.
- Cancel stops further processing but preserves the app-owned local recording
  for Export Original or a separate explicit discard. Cancel after upload first
  persists the canceled state, then deletes the exact temporary TOS version;
  failed cleanup resumes on the next launch.
- Discard is a separate destructive action available only before upload or
  after cancellation and verified TOS cleanup. It durably enters `discarding`
  before removing the app-owned package, never follows nested symlinks, and
  resumes to `discarded` after a crash.
- The app may disappear at any point. Already-submitted 妙记 work continues at
  the provider; every other stage pauses safely and resumes from its durable
  local checkpoint by `recording_id` on the next background opportunity or
  launch.
- Terminal provider failures retain the local recording and offer Retry and
  Export Original; they never mark the recording processed silently.

### In-app processing engine

The Rust core exposes typed local commands and Rust interfaces, not an HTTP
processing control plane:

```text
enqueue(recording_id)
status(recording_id)
run_pending()
retry(recording_id)
reprocess(recording_id)
cancel(recording_id)
export_original(recording_id)
resume_all_on_launch()
```

Requirements:

- The webview can pass only `recording_id` and bounded user choices. The Rust
  engine derives package paths and TOS object keys from validated local state;
  JavaScript cannot supply arbitrary paths, URLs, credentials, or object keys.
- Mobile WebViews cannot invoke the Swift/Kotlin capture plugin directly.
  App-owned Rust `mobile_*` commands are the only granted capability surface:
  recording permission/preflight/start recheck the recording kill switch,
  picker open rechecks the import kill switch, while status/stop/drain/ack stay
  available so a held rollout cannot strand existing local work.
- Secrets are read only inside Rust/native secure-store adapters after setup.
  They cross Tauri IPC only as input to an explicit write-only save command,
  are never returned to JavaScript, and are never compile-time seeded.
- Imports are fully decode-validated with byte, `<5h`, path, packet-count, and
  wall-time limits before provider submission. The provider object extension
  and MIME type are derived from the validated canonical artifact in Rust.
- TOS uploads stream directly from the verified package and are rechecked by
  metadata/hash before submission. Signed GET URLs are short-lived and redacted.
- TOS temporary objects are deleted only after the transcript and canonical
  audio backup are durable. Cleanup is itself checkpointed and resumes on launch.
- 妙记 request IDs, returned task IDs, transcripts, summaries, archive
  generations, per-target publication proofs, and cleanup proofs are durable.
- Archive writes use a remote compare-and-swap/ref precondition. A conflict is
  fetched, merged by stable `recording_id`, and retried within a bound; an
  unresolved conflict is visible and never overwrites another device's work.
- Viewer edits and deletes enter an ordered, destination-bound Rust outbox
  before authoritative local mutation. Relaunch replays them in sequence;
  replacing the configured GitHub repository, R2 account, or bucket quarantines
  old intents instead of applying them to the new destination.
- Canonical R2 audio keys are derived from recording identity, publication
  generation, normalized content hash, and captured time, never a mutable title
  or locally collided archive slot. An R2-attempt journal is durable before PUT
  and is cleared only after Git publication or owner/hash/size-verified cleanup.
- Explicit reprocess verifies the local normalized audio, reuses the stored
  妙记 transcript, regenerates only the Gemini summary, and publishes the next
  archive generation. A still-valid canonical R2 object is reused and carries
  its own `r2_generation`; a missing object falls back to a new journaled key.
- Recording deletion uses an exact entry-version CAS plus a durable
  `recording_id` tombstone. The tombstone prevents stale processing from
  resurrecting deleted audio; legacy objects without ownership metadata require
  a separate explicit partial-delete confirmation and are never reported as
  remotely deleted.
- Rate, duration, and byte limits are configuration surfaced before upload and
  enforced again in Rust immediately before each external effect.

### macOS on-device STT fallback

This is a second implementation of the STT provider port, not a second job
engine. The same Rust ledger remains authoritative and records the selected
backend (`miaoji_remote`, `whisper_local`, `moss_local`, or retained `qwen_local`), model
identity/digest, attempt, transcript proof, summary backend/model/prompt
identity, archive disposition, and one winning result. The local route removes
every remote stage from its completion dependency; optional cloud sync is a
separately replayable effect.

Fallback eligibility is fail-safe:

- TOS fails before any upload dispatch, or 妙记 is proven unavailable before
  submit: offer the installed local model immediately.
- TOS upload or 妙记 submit has an ambiguous/lost response: reconcile first.
  Never start local transcription automatically while a chargeable remote task
  may exist.
- 妙记 accepted a task but polling is unavailable: continue bounded polling and
  offer an explicit user takeover. A takeover durably marks the remote result
  superseded; a late remote result may be stored for diagnosis but cannot
  publish a second archive generation.
- The full local result stores transcript text, segment timestamps, and
  recording-local anonymous speaker labels by default, then runs the local Qwen
  summary worker and publishes the same user-visible fields to the local
  archive. “Similar deliverable”
  means comparable usable structure and measured quality, not byte-identical
  wording, segmentation, or speaker numbering. A user may explicitly turn
  diarization off or accept transcript-only output after a visible diarization
  failure; otherwise missing labels are not a successful full-local result.
  A queued Gemini call or remote archive publication is never labeled complete
  local processing.

Model installation is an explicit, cancelable and HTTP-range-resumable user action with model name,
download size, expected disk/RAM use, license/source, SHA-256, and removal UI.
No model or Hugging Face token is bundled. Once installed, transcription works
with network disabled and never sends audio off-device. Model files live in the
App-owned data area, are regular-file/hash verified before mmap/load, and are
covered by an independent local-STT kill switch.

Each imported file exposes an explicit pre-upload local action. The installed
model card also offers a non-secret, opt-in preference that routes newly
completed App recordings to full local processing before any TOS/妙记 request;
the default remains direct TOS → 妙记. This preference never changes mobile or
unsupported-Mac behavior.

`whisper.cpp` is native C/C++ code. The selected Rust wrapper documents that
certain foreign exceptions can abort the Rust runtime, so inference must not run
inside the Tauri main process. EchoWall ships a signed arm64 one-shot worker
inside the App bundle. Rust launches it for one validated job over bounded
stdin/stdout framing (or inherited file descriptors), supplies only App-owned
model/audio identities, verifies the returned transcript, and terminates it.
The worker has no listener, network client, provider/archive credential, queue,
relaunch agent, or persistence authority; it is not a server or daemon. A crash
marks the durable job retryable while the main App and local recording survive.

Default local diarization runs in a second signed one-shot Swift/Core ML worker
on Apple Silicon macOS 14+. The currently implemented pipeline is pinned
FluidAudio offline Pyannote + WeSpeaker + PLDA/VBx; the selected replacement
candidate is an offline-only source subset of Argmax SpeakerKit with its exact
Core ML assets. It receives only verified App-owned 16kHz mono audio/model
identities and returns bounded anonymous speaker intervals; it has no listener,
downloader, credentials, queue, or persistence. Rust validates the response,
merges speaker slots into Whisper
segments only when confidence is at least 500/1,000 and maximum temporal
overlap is unambiguous, and remains the only job/checkpoint/
publication owner. The full local pack installs diarization assets and the
local summary model by default.

Local summarization runs in a third signed arm64 one-shot Rust worker built from
pinned `llama-cpp-2`/`llama.cpp` with Metal and no network/common downloader
surface. The closed request binds the recording, transcript digest, prompt
version, model digest, and size. Output is schema-directed and then strictly
validated by Rust against the title/category/bilingual summaries,
key-points, and action-items contract. Long transcripts are split only at
deterministic UTF-8/line boundaries, summarized independently, and recursively
reduced under the same prompt/schema. Intermediate text never enters logs or
stdout. A crash, timeout, invalid JSON, or identity mismatch leaves the stored
transcript authoritative and retryable.

Implemented local routes and closeout obligations:

1. Keep the existing Whisper/FluidAudio and Qwen compatibility paths intact.
   Finish MOSS with the already implemented pinned native workers and policies;
   do not add a model port, acceleration study or another diarizer comparison.
2. Preserve bounded protocols, model/audio identities, cancellation/reaping,
   generation ownership, raw provenance, truthful unknowns and policy-compatible
   restart. Canonical text/timing/order must survive summary/archive unchanged.
3. Keep model installation explicit, resumable and removable, with size/license
   disclosure and verified local files. Do not require installing unused model
   families to use the selected route.
4. Run local summary and atomic hash-verified archive publication through the
   same Rust owner. Confirm result opening and reopen in the assembled App;
   optional sync must not become a local-completion dependency.
5. Use existing quality evidence and a representative ordinary-use check under
   the acceptance section and Phase 1A. Retained alternatives and historical
   measurement documents are not additional implementation obligations.

```text
Tauri UI → Rust durable job owner → selected native MOSS worker
  → explicit timing/mapping + offline SpeakerKit where used
  → pinned local Qwen summary worker
  → validated, atomic local archive → result view/reopen
```

The existing remote and Whisper fallback paths remain available as explicit
choices. No new local route may silently take over an ambiguous/accepted
chargeable remote job. Historical details are in the linked evidence files;
this section does not require their experiments to be repeated.

## Capture specifications

### Shared recording behavior

- Start requires one explicit click/tap and a successful permission/source
  preflight.
- Start also checks free space before creating a session. Desktop reserves the
  calculated worst-case two-hour PCM tracks, normalized output, and 512MiB
  safety margin; iOS/Android require at least 1GiB. Capacity-check failure or a
  lower value blocks Start with an explicit storage message.
- The app writes crash-recoverable segments rather than trusting one long file.
  A five-minute segment is the initial target; the spike may tune this based on
  encoder behavior and gap measurements.
- Pause closes the current segment. Resume opens a new one and records the gap.
- Stop durably closes tracks, validates duration, builds `RecordingEnvelope`,
  creates the normalized mix, then enqueues upload.
- Local input and remote/system audio remain separate tracks. The normalized
  provider copy aligns them on a shared monotonic clock, applies conservative
  level normalization, and never uses acoustic echo cancellation to rewrite the
  source tracks.
- If the microphone hears speaker output, EchoWall warns that headphones give
  cleaner separation; it does not attempt to invent a clean track.
- Route changes, source silence, source exit, low disk, and encoder failures are
  first-class events and visible warnings.
- Active/recovered desktop status returns the exact mic/source labels, total gap
  duration, and at most the latest 16 validated native notices. The UI renders
  known codes as bounded user-facing recovery guidance; it never relies on a
  count alone or exposes unbounded backend diagnostics.
- Desktop recovery reads at most a 1MiB `capture-session.json` and an 8MiB
  `events.ndjson`. The snapshot admits at most 512 closed segments and 128
  combined gaps/notices, matching the final envelope warning limit; every
  snapshot write reserves 1MiB of event-journal capacity for recovery.
  Oversized files or collections fail before JSON parsing or snapshot mutation,
  and tail repair never truncates an over-limit journal.
- “Continue recording” is shown only while the same iOS coordinator still owns
  a resumable interrupted recorder. Desktop crash/source-loss recovery and
  Android terminal interruption have no native stream to resume: their control
  is disabled and the user stops/saves the safe segments before starting a new
  source.

### macOS

Implementation adapter: a default-off narrow Swift/C ABI over Core Audio
process taps for Meeting plus ScreenCaptureKit for whole-system audio, exposed to the Tauri Rust
ingest core. The Swift bridge owns only tap/aggregate/IOProc lifetime and emits
Float32 callbacks; Rust owns the shared clock, PCM segments, gaps, levels, and
recovery.

- Voice Memo: capture selected microphone.
- Meeting, native app: on macOS 14.2+, translate the selected PID and matching
  bundle/helper processes into a private `CATapDescription`; never fall back to
  whole-system audio.
- Meeting, browser: tap the explicitly selected browser process/bundle family
  and show the other-tabs warning.
- System Capture: capture display/system audio plus microphone; exclude
  EchoWall's own playback by default.
- Permissions: microphone plus Screen & System Audio Recording. Read-only
  preflight never prompts. After the user chooses a mode, a separate explicit
  “Authorize and refresh sources” action requests only the required permission;
  denied/restricted states expose a fixed native System Settings destination,
  never a webview-provided URL. Screen Recording changes may require App restart.
- Source picker displays the selected App's bounded, lazily loaded OS icon, App
  name, and exact capture scope. It does not claim current audio activity or
  open a capture device before Start; after explicit Start, the active source
  name and real system-audio meter expose activity. It never defaults to the
  previously selected App without showing it.

References: [Core Audio process-tap sample](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps),
[ScreenCaptureKit capture sample](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos),
[`capturesAudio`](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturesaudio),
and [`captureMicrophone`](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturemicrophone).

### Windows

Implementation adapter: Rust `windows` bindings over WASAPI, isolated behind
`cfg(target_os = "windows")`.

- Voice Memo: capture selected microphone endpoint.
- Meeting, native app: activate process loopback with
  `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` for the selected app and
  capture the microphone separately.
- Meeting, browser: capture the selected browser process tree and show the
  other-tabs warning.
- System Capture: capture the selected/default render endpoint in WASAPI
  loopback mode plus the microphone.
- Capture uses shared mode and monotonic/QPC timestamps. Device invalidation
  closes the segment, re-enumerates the endpoint, and resumes only after
  recording a warning event.
- Permissions: handle Windows microphone privacy denial and deep-link to
  `ms-settings:privacy-microphone`.
- Protected/DRM audio is not promised. Native Zoom/Teams and explicit
  selected-browser fixtures are the release targets.

References: [WASAPI loopback](https://learn.microsoft.com/windows/win32/coreaudio/loopback-recording),
[process-loopback mode](https://learn.microsoft.com/windows/win32/api/audioclientactivationparams/ne-audioclientactivationparams-process_loopback_mode),
and Microsoft's [ApplicationLoopback sample](https://github.com/microsoft/Windows-classic-samples/tree/main/Samples/ApplicationLoopback).

### Browser Meeting scope

Release 1 and Release 2 use the selected-browser application/process capture
defined above. The source picker names the browser and requires confirmation of
the warning that all audible tabs in that browser may be captured. Exact-tab
capture, browser extensions, Native Messaging, and extension-store packaging
are deferred beyond both release gates.

### iOS

Implementation adapter: Swift AVFAudio bridge plus a share extension/App Group.

- Voice Memo: microphone recording through `AVAudioSession` and
  `AVAudioRecorder` or `AVAudioEngine`; the spike chooses the simpler API that
  passes segmentation and interruption tests.
- Declare background audio so an active recording continues after lock or app
  switch. Handle phone/alarm interruptions and route changes explicitly.
- File import: EchoWall's native `UIDocumentPickerViewController` opens the
  security-scoped URL only inside its callback, copies each accepted file to a
  stable App-owned staging ID, then releases the URL. Picker and Share items use
  the same replay/ack path into the Rust inbox.
- Voice Memos import: Share → EchoWall copies the `.m4a` into the shared App
  Group inbox. Opening EchoWall alone still cannot inspect Voice Memos.
- The main App and Share Extension declare the same
  `group.ai.ax.watch-transcriber` entitlement. Their development/distribution
  provisioning profiles must both contain that App Group; CI rejects profiles
  that do not before attempting the archive.
- Share handoff is two-phase: Swift keeps the App Group original and exposes a
  stable import ID until Rust has durably adopted or deduplicated the file.
  Only then may a bounded native acknowledgement delete staging copies.
- Share and picker items persist a bounded per-import sidecar containing the
  sanitized original display filename. Crash replay passes that name through to
  Rust review; acknowledgement removes both audio and sidecar. Missing/corrupt
  metadata falls back to the stable staging filename without losing audio.
- Direct TOS/妙记/Gemini/archive work runs while iOS grants the App execution
  time. The product does not promise a permanently running uploader after
  suspension or force termination: every step is durable and resumes on the
  next permitted background opportunity or App launch.
- Native preflight and Start both require at least 1GiB free using the volume's
  important-usage capacity, so a two-hour capture is not knowingly started
  without room for source, normalization, and handoff copies.
- Export Original uses a native document exporter with a real verified
  App-owned staging file, not an empty save-dialog placeholder. The picker
  callback reopens the selected destination under its security scope and
  verifies exact size/SHA before reporting success; abandoned export staging is
  cleaned on launch.
- Under Tauri's iOS App Data directory (`Application Support/<bundle-id>`),
  native recorder sessions live at `native-capture/sessions/<recording-id>` and
  the Rust inbox lives at `inbox/<recording-id>`. Swift capture, Share
  staging, and export explicitly use that same bundle-scoped root; they never
  write a handoff into the bare Application Support directory. Launch migrates
  only legacy unfinished native sessions that contain `capture.json` but no
  Rust `recording.json`; Rust cleanup mechanically rejects either-direction
  overlap with its inbox.
- iOS 27 ScreenCaptureKit cross-app audio is an optional later spike, not a v1
  dependency.

References: [background recording category](https://developer.apple.com/documentation/avfaudio/avaudiosession/category-swift.struct/record),
[audio interruptions](https://developer.apple.com/documentation/avfaudio/handling-audio-interruptions),
[document picker](https://developer.apple.com/documentation/uikit/uidocumentpickerviewcontroller).

### Android

Implementation adapter: Kotlin microphone foreground service exposed to the
Tauri Rust ingest core.

- Voice Memo: start while the activity is visible, then continue through a
  `microphone` foreground service with a persistent recording notification.
- File import: Storage Access Framework `ACTION_OPEN_DOCUMENT`; copy into the
  app-owned inbox before returning control.
- Share import: accept `audio/*` send intents, validate, and enqueue.
- Picker/share copies remain in the native inbox under a stable import ID until
  Rust durably adopts or deduplicates them. A bounded acknowledgement ledger
  hides and cleans only those App-owned copies after adoption; task/process
  death before acknowledgement safely replays the item.
- After native recording segments are hash-verified into the Rust package and
  the normalized recording reaches `Ready`, the redundant Swift/Kotlin session
  directory is removed with a bounded symlink-rejecting walk. The copied source
  tracks inside the Rust package remain subject to the 30-day retention rule;
  failed native-staging cleanup retries from `Ready` without decoding again.
- Persist job state so task removal, process death, reboot, or offline periods
  never lose a completed local recording.
- Native preflight, plugin Start, and the foreground service independently
  require at least 1GiB free from `StatFs`; a runtime write failure reports a
  storage-specific interruption if capacity has crossed below that floor.
- When no recorder service is active, status/preflight/start first reconciles a
  persisted nonterminal native session: mark it `INTERRUPTED`, repair the WAV
  header from durable PCM length, expose it to Rust recovery, and block a new
  recording until the prior session is finalized. After Rust reaches `Ready`,
  remove the session directory and its matching terminal active-session
  snapshot; never delete a snapshot belonging to another session.
- Export Original uses Android's `ACTION_CREATE_DOCUMENT`. Rust derives and
  hash-verifies the App-owned source from `recording_id`; the native adapter
  writes the selected `content://` URI and reopens it to verify size/hash.
  JavaScript never supplies a source path or destination URI.
- App-module integration creates a valid fabricated WAV through MediaStore,
  passes its real `content://` URI through the picker-copy adapter, verifies
  bytes/name/size/hash, replays the same URI to the same stable import, and
  removes only the App-owned copy after acknowledgement. Actual system-picker
  interaction remains a physical-device acceptance item.
- Android playback capture is not a Meeting-mode fallback. Android excludes
  voice-communication usage and capture depends on the source app's policy.

References: [microphone foreground service](https://developer.android.com/develop/background-work/services/fgs/service-types),
[background-start restrictions](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start),
and [AudioPlaybackCapture restrictions](https://developer.android.com/reference/android/media/AudioPlaybackCaptureConfiguration).

## Direct file import

### Entry points

| Platform | Entry points |
|---|---|
| macOS | toolbar Import, File menu, drag-and-drop, multi-select Open panel |
| Windows | toolbar Import, File menu, drag-and-drop, multi-select picker |
| iOS | Files picker, Share → EchoWall |
| Android | Storage Access Framework, Share → EchoWall |

### Supported v1 inputs

- Audio only: `.m4a`, `.mp3`, and `.wav`, fully decode-validated locally before
  enqueue. `.aac`, `.flac`, and `.ogg` remain future additions and must not be
  advertised until the Rust decoder and the configured 妙记 route pass the same
  validation and end-to-end matrix.
- Files are identified by content, not extension alone.
- Video-container extraction, playlists, URLs, folders, and archives are out of
  scope for v1.
- Imports never modify or delete the source file.
- Desktop supports batch import. Mobile share actions may enqueue several
  provider-supported items but process them independently.

### Import review

Before enqueue, show:

- filename, duration, size, and detected codec;
- proposed recording time from embedded metadata, filename, or file mtime, with
  an editable fallback when confidence is low;
- optional display title; and
- speaker count defaulting to auto.

Duplicate policy:

- Same normalized hash already complete: show the existing recording and do not
  upload again.
- Same hash in progress: attach to the existing job.
- Same source filename but different hash: treat as a distinct recording.
- Explicit Reprocess reuses stored audio and transcript when possible; it does
  not pretend a duplicate is a new capture.

Validation failures identify the concrete reason: unsupported codec, corrupt or
zero-byte file, encrypted/protected media, provider size/duration limit, decode
timeout, or insufficient local storage.

## Product experience

### Information architecture

Reuse the existing archive list/detail model; do not add a dashboard.

```text
EchoWall
├─ top bar
│  ├─ Record
│  │  ├─ Voice Memo
│  │  ├─ Meeting          desktop only
│  │  └─ System Capture   desktop only
│  └─ Import
├─ active recording strip / mobile recording screen
├─ Processing section at the top of the timeline
│  └─ local, uploading, transcribing, publishing, failed jobs
└─ existing archive timeline and detail view
```

Primary hierarchy during recording:

1. elapsed time and unmistakable red recording state;
2. selected mic/source plus separate live meters;
3. Pause and Stop; settings are unavailable while recording unless safe.

Desktop keeps a tray/menu-bar control with timer, source name, Pause, and Stop.
iOS exposes a lock-screen/Live Activity status when the chosen APIs permit it.
Android uses the required foreground-service notification. Closing the main
window never implies Stop; Quit explains and confirms if recording is active.

### User journey

| Step | User action | Intended feeling | Product response |
|---|---|---|---|
| 1. Choose source | taps Record or Import | confident about what will be captured | shows only supported modes and states the capture scope before permission prompts |
| 2. Preflight | selects mic, app/browser, or file | in control, not surveilled | identifies every source and permission state, labels the meters that will activate only after explicit Start, and calls out whole-browser or whole-system scope |
| 3. Record/import | starts recording or confirms import | able to leave it alone | persistent timer/progress, safe backgrounding copy, Pause/Stop, no settings noise |
| 4. Finalize | taps Stop or finishes import | certain the audio is safe | saves locally first, reports duration/gaps, then queues upload |
| 5. Process | closes or keeps using EchoWall | trust that work will finish | timeline row names the current stage, preserves local audio, and offers bounded retry |
| 6. Recover | returns after interruption/offline/app restart | reassured nothing was fabricated or lost silently | shows recovered duration and explicit gaps, then offers Continue, Export, or Discard |
| 7. Review | opens completed archive item | immediate payoff | lands in the existing detail view with playback, summary, speaker labels, and source metadata |
| 8. Long-term use | repeats the flow across devices | muscle memory | identical mode names and processing states; platform differences appear only where capabilities differ |

Time-horizon target:

- First 5 seconds: the user can distinguish Voice Memo, Meeting, System
  Capture, and Import without reading help text.
- First 5 minutes: source selection, active recording, safe close, and pending
  processing are self-explanatory.
- Long-term: every device produces the same archive shape and every failure is
  recoverable without remembering platform-specific rituals.

### Visual-system constraints

- Extend the existing graphite token set in `viewer_template.html`; do not add a
  second capture theme or a generic SaaS dashboard.
- Record and Import are compact top-bar actions on desktop and primary actions
  in the existing mobile list header. Processing jobs are timeline rows, not a
  grid of status cards.
- Reserve the established blue accent for selection/action. Recording red is
  used only for live capture and destructive stop/discard confirmation.
- Reuse existing typography, day grouping, pills, player controls, toast style,
  mobile full-screen push, and bottom-sheet patterns.
- Live meters are thin functional traces with text labels; they do not become
  decorative visualizers competing with elapsed time.
- Preflight never opens an audio device merely to animate a meter. Metering
  begins only after the same explicit Start action that begins the durable
  recording, and stops or decays to zero when capture pauses or ends.
- The recording surface removes search/filter/archive controls while active.
  Stop, Pause/Resume, elapsed time, and source identity are the only primary
  elements.

### Interaction state table

| Surface | Ready/empty | Active/loading | Recoverable problem | Terminal problem | Success |
|---|---|---|---|---|---|
| Permission preflight | explains why each permission is needed | system prompt open | denied with Open Settings | platform/API unsupported | source picker unlocked |
| Voice Memo | mic name + meter label; no audio device open | red timer + level | interruption/route change banner | encoder or disk failure; original segments retained | finalizing → queued |
| Desktop Meeting | mic + native app or warned selected-browser scope | two meters + source/scope name | source silent, device change | selected source ended; user chooses a new source or stops | finalizing → queued |
| System Capture | mic + output scope stated plainly | two meters + “all system audio” | endpoint/display change | permission revoked | finalizing → queued |
| File import | warm CTA: “Choose an audio file” | inspecting metadata/hash | missing metadata is editable | corrupt/unsupported/too large | review sheet → queued |
| Upload | queued/offline copy remains local | bytes + percentage | offline/backoff with Retry Now | auth or repeated integrity failure | remote object integrity verified |
| Processing | waiting position and safe close message | transcribing/summarizing/publishing | provider retry/backoff | stage-specific error with Retry/Export | archive item opens automatically |
| Duplicate | existing title/date preview | checking hash | remote status unavailable | none | Open Existing or Reprocess |
| Recovered capture | recovered duration/source explanation | validating segments | gaps listed honestly | unrecoverable final segment isolated | Continue upload or discard |

No state uses an indefinite spinner without stage text, elapsed time, a safe
close explanation, and a recovery action.

### Accessibility and platform fit

- Minimum 44pt touch targets on mobile and logical keyboard order on desktop.
- Space toggles Pause/Resume only when focus is not in a text field; a distinct
  shortcut stops recording after confirmation.
- All source names, meters, states, and errors have screen-reader labels that do
  not rely on color.
- Recording status uses icon, text, and color. Reduced-motion disables pulsing
  animation without hiding the active state.
- Desktop source lists are searchable and fully keyboard navigable.
- Long filenames and app/tab titles truncate visually but remain available to
  assistive technology and tooltips.

## Privacy and trust boundary

Assets: raw voice/meeting audio, separate mic/system tracks, transcripts,
speaker labels, presigned upload URLs, provider credentials, archive credentials,
and deletion authority.

Actors: the user, EchoWall app, TOS, 妙记, Gemini, private
GitHub archive, and R2. There is no EchoWall-operated processing service.

Material failure scenarios and controls:

| Failure/abuse | Required control |
|---|---|
| Accidental or hidden recording | explicit start, persistent indicator, source name, no auto-start |
| Wrong app/browser captured | preflight identity + meter; explicit selected-browser confirmation; source scope recorded in envelope |
| Provider key exposed or bundled | users provide their own keys; native secure store only; no key in binaries, webview state, logs, fixtures, or crash reports; least-privilege TOS/archive scope |
| Synced archive replaces executable UI or invokes native commands | executable viewer/markdown runtime compiled into the App; synced HTML/JS ignored; restrictive CSP; per-process capability path; exact navigation and same-origin mutation checks; explicit Tauri command ACL |
| Mobile WebView bypasses a release kill switch through the native plugin | remove all raw `echowall-capture:*` grants; expose only Rust `mobile_*` wrappers with native-operation policy checks and recovery exceptions |
| Signed URL leaks in logs | structured redaction and negative log tests |
| Duplicate uploads or charges | hash + `recording_id` idempotency at create, submit, and publish |
| Corrupt/untrusted import | magic-byte validation, decode sandbox, resource/time limits, canonical paths |
| Raw audio retained forever accidentally | visible retention setting, cleanup ledger, explicit source-track policy |
| Processing succeeds but publish fails | replay from durable transcript; never retranscribe automatically |
| Concurrent App/Python archive writers lose data | one canonical cross-process archive lock shared by Rust publisher, desktop mutators, sync overlay, and the legacy watcher |
| Viewer edit is lost on crash or replayed into a newly configured archive | ordered local outbox written before mutation; exact non-secret destination binding; field-level CAS/merge; conflicts remain visible |
| Delete races a reprocess or leaves private R2 audio orphaned | immutable generation-owned R2 key, pre-PUT attempt journal, exact manifest-version CAS, durable recording tombstone, and owner/hash/size-verified cleanup |
| Delete removes the only surviving audio | confirm durable archive before temp cleanup; preserve existing deletion path; legacy unverified R2 audio needs explicit partial-delete confirmation |

Retention keeps the canonical mixed audio in the existing private R2 archive.
Release 1 uses the locked default shown before the first Meeting/System
recording: separate mic/system source tracks stay local until successful
publication and are deleted after 30 days; imported originals and the canonical
mix are not pruned. Permanent source-track backup remains an explicit future
opt-in.

## Phased implementation

These sections retain the component contracts and original work breakdown.
Execute the remaining delivery list first; do not restart completed Phase 0
spikes or use old model experiments as a new closeout backlog.

### Phase 0 — Feasibility spikes and ADRs

Linear: [AX-248](https://linear.app/ax-agent-swarm/issue/AX-248).

Spikes:

1. macOS: 30-minute native Zoom/Teams capture with separate Core Audio process-
   tap/mic tracks and no timestamp drift beyond 100ms at the end.
2. Windows 11 (Release 2 gate): the same with WASAPI process loopback; include
   output-device switch and source-process restart on Windows hardware.
3. Browser: validate selected-browser process/app capture in current Chrome and
   Edge, including one-hour stability, other-tabs warning, and exclusion of a
   distinct non-browser application signal.
4. iOS: two-hour mic recording across lock, app switch, incoming call, AirPods
   route change, offline stop, and relaunch recovery.
5. Android: two-hour foreground-service mic recording across screen-off, task
   removal, offline stop, and process recreation on at least Pixel and one OEM.
6. Import: one fixture per supported codec plus corrupt, zero-byte, duplicate,
   3.5-hour, and oversized samples through the in-app Rust decoder.
7. Processing: direct TOS upload, app disappearance after upload/submit, 妙记
   poll, transcript replay, archive CAS/publish, and TOS cleanup.

Exit criteria:

- Release 1 macOS Meeting capture is GO. Browser Meeting records the explicitly
  selected browser application, excludes non-browser applications, and shows
  the exact other-tabs warning. Windows reaches the same exit criterion in
  Release 2.
- Mobile long-recording artifacts survive the tested lifecycle interruptions.
- The embedded-processing/credential ADR is recorded with suspension, rotation,
  retention, concurrent-writer, and rollback behavior.
- Any failed platform promise is removed or explicitly deferred before shared UI
  implementation begins.

### Phase 1 — Shared ingest and processing job core

Linear: [AX-239](https://linear.app/ax-agent-swarm/issue/AX-239).

- Add versioned `RecordingEnvelope` schema and validation.
- Add app-owned inbox, append-only events, hash/dedup, and state transitions.
- Define `CaptureAdapter`, `ProcessingEngine`, provider, secure-store, and
  archive-publisher ports in Rust.
- Make current Voice Memos discovery emit the same envelope.
- Port the active TOS → 妙记 → Gemini text summary path and required archive
  publisher behavior into Rust while preserving the legacy note/audio/viewer
  shape. Keep the Python watcher order unchanged; use locked decision 12's
  safer R2-before-Git order only in the App publisher.
- Add secure credential setup, a local durable effect ledger, streaming upload,
  provider reconciliation, optimistic archive publication, idempotency, and
  resumable cleanup. Do not add FastAPI or any self-hosted broker.

Acceptance:

- A fixture envelope can be replayed from every nonterminal state.
- Duplicate enqueue/resume requests create one provider job and one archive
  publish.
- A valid stored transcript can republish without uploading or calling 妙记.
- Existing watcher output remains compatible. App-owned entries use the same
  friendly note/audio paths, note structure, daily rollups, topic views, and
  viewer payload, with only the additive ownership/proof fields listed above.
- The same fixture completes with no Python/FastAPI process and no EchoWall
  processing endpoint available. Excluding the authenticated `127.0.0.1`
  viewer asset transport, only the documented external provider/archive hosts
  are contacted; no job payload or provider credential is sent to that viewer
  transport.

### Phase 1A — macOS local processing closeout

Linear: AX-256 under AX-237; shared ownership remains in AX-239's Rust engine.
The native MOSS, SpeakerKit and local-summary implementation exists. Finish its
user path and package; keep existing alternatives working without expanding
research. This phase does not block independent iOS/Android delivery work.

Deliverables:

- A coherent App with the selected local model visible and usable through
  native commands and UI; explicit installation and local/remote selection,
  progress, cancel/retry/remove and useful failure/recovery states.
- Current App-bundled workers built through the canonical scripts. Preserve
  native-only inference, offline boundaries, model/audio hashes, immutable
  requests, policy-compatible replay and exactly one winning publication.
- Validated transcript, timestamps, anonymous speakers, structured local
  summary and a reopenable audio/note/manifest archive. Unknown attribution is
  honest; a missing/failed stage is never silently declared full local success.
- User-facing model/setup/fallback and known-limit documentation, updated in
  both READMEs when their content changes.

Acceptance:

- A representative English, Mandarin and mixed-language ordinary-use sample
  set is readable and useful at a broadly Feishu/Miaoji-comparable level. Reuse
  the current corpus evidence; the purpose of the final sample check is to
  catch App integration failures, not to launch another benchmark campaign.
- The assembled App completes local import → transcription/speakers → summary
  → archive/result opening and reopen after explicit model installation.
  Network-disabled execution retains full local completion. Check a longer
  example only if relevant existing evidence does not cover changed behavior.
- No crash/hang, silent loss, corrupt source/archive, duplicate publication or
  hidden provider call occurs in the checked user path. Failure preserves the
  recording and offers a truthful recovery/export path.
- Model install/cancel/remove, pre-dispatch local selection and ambiguous-remote
  takeover retain their existing integrity/privacy tests. Existing validated
  long-run/resource evidence is reusable; a session boundary does not require
  another two-hour run.
- Current signing, architecture, sidecar protocol and privacy checks pass for
  the artifact being delivered. Do not call a source test an installed-App or
  release proof.

### Quality evidence is diagnostic, not a research gate

The release target is useful Feishu-comparable performance, not exact output
matching or a fully green scorecard. Do not require per-stratum numerical
non-inferiority, exact speaker-count equality, 95% short-turn/count targets,
99% coverage, isolated named-term thresholds, all-44 completion, or blinded /
two-reviewer quality approval to deliver an otherwise usable implementation.
A persistent, substantial language/speaker/summary failure in ordinary use is a
real defect and must not be hidden by an average; an individual term, count or
edge-case deficit alone is a known limitation/diagnostic.

`docs/evals/full-local-quality.md`, `desktop/local-quality-eval` and
`schemas/local-quality-matrix-v1.schema.json` remain the measurement harness.
Keep existing reports and grader semantics unchanged. Old `legacy-v1` and
`miaoji-relative-v2` verdicts are evidence with their original scope, not the
current release decision. Do not fabricate unmeasured human or runtime proof.

### Phase 2 — Direct import vertical slice

Linear: import portions of AX-240 through AX-243; shared behavior owned by
AX-239.

- Implement validation, metadata review, copy-to-inbox, and dedup once.
- Wire macOS pickers and drag-and-drop for Release 1; keep the shared desktop
  boundary Windows-compatible and activate the Windows picker in Release 2.
- Wire iOS document picker/share extension and Android SAF/share target.
- Send imported recordings through the complete processing and archive path.

Acceptance:

- macOS, iOS, and Android import `.m4a`, `.mp3`, and `.wav` end to end in
  Release 1; Windows passes the same matrix in Release 2.
- Desktop batch import preserves independent progress/retry.
- Voice Memos Share → EchoWall works on a physical iPhone.
- The signed main App and `EchoWallShare.appex` both carry the exact App Group
  entitlement and move one fabricated share into the App-owned review queue.
- Termination after native staging and termination after Rust adoption but
  before native acknowledgement both replay to one deduplicated review item;
  acknowledged staging files are removed without touching the source Voice
  Memos recording.
- The review row preserves the sanitized Voice Memos/Files display filename
  across staging and relaunch instead of exposing an internal UUID.
- Duplicate import performs no second upload or provider call.
- Source files remain unchanged.
- Mobile termination between native copy, Rust adoption, and native cleanup
  replays one stable item and eventually removes only App-owned staging bytes.
- Successful mobile capture finalization leaves one managed source-track copy,
  not an indefinitely retained second native-session copy.

### Phase 3 — macOS three-mode recorder

Linear: [AX-240](https://linear.app/ax-agent-swarm/issue/AX-240).

- Native capture adapter and permission preflight.
- Voice Memo, native-app Meeting, and System Capture.
- Separate-track segment writer, normalization, recovery, tray/menu-bar controls.
- Integrate the explicit selected-browser Meeting source from Phase 5.

Acceptance:

- All three modes pass a 2-hour physical-device recording.
- Native Zoom and Teams remote audio is isolated from unrelated apps.
- Mic/system tracks stay aligned within the spike-approved tolerance.
- A native source whose first callback arrives more than 100ms after the shared
  recording clock produces an explicit startup gap; raw tracks are never
  backfilled with invented samples to make their starts look aligned.
- Permission denial, app exit, source silence, and device changes match the state
  table.
- Insufficient storage blocks Start before a capture package or native session
  is created and leaves existing recoverable sessions untouched.

### Phase 4 — Windows three-mode recorder

Linear: [AX-241](https://linear.app/ax-agent-swarm/issue/AX-241).

Release target: Release 2. This phase is deliberately non-blocking for Release
1. Before Windows hardware is available, preserve the implementation and require
native/Windows-target compile, clippy, contract tests, and an unsigned CI
artifact only; do not call those checks runtime acceptance.

- WASAPI mic, endpoint loopback, and process-loopback adapters.
- Windows microphone privacy handling and endpoint recovery.
- Voice Memo, native-app Meeting, and System Capture with the same product
  semantics as macOS.
- Windows Credential Manager support, CI build, signing, and installer lane.

Acceptance:

- All three modes pass a 2-hour Windows 11 recording.
- Native Zoom and Teams process trees are isolated from unrelated apps.
- Output-device and microphone changes produce recoverable segments and visible
  warnings, not silent gaps.
- Release installer launches without a console, stores no secret in plaintext,
  and completes import/upload on a clean machine.

### Phase 5 — Browser Meeting selected-source capture

Linear: [AX-246](https://linear.app/ax-agent-swarm/issue/AX-246).

- Ship selected-browser application/process capture with scope text and an
  unrelated-tab warning on macOS. Ship the same behavior on Windows in Release
  2.
- Keep browser Meeting distinct from System Capture by excluding non-browser
  applications and recording the selected browser identity in the envelope.

Acceptance:

- On each platform when it enters its release train, browser Meeting captures
  the selected browser and excludes non-browser apps.
- The UI states plainly that other audible tabs in that browser are included
  and requires the user to confirm that scope before Start.

### Phase 6 — iOS recorder

Linear: [AX-242](https://linear.app/ax-agent-swarm/issue/AX-242).

- Native mic recording, segmentation, interruptions, background recording
  continuation, lock-screen status where supported, resumable direct processing,
  and recovery UI.
- Preserve existing read-only archive sync and playback.

Acceptance:

- Two-hour locked-screen recording completes and processes without keeping the
  EchoWall screen open.
- Incoming-call and route-change outcomes are represented honestly in the
  artifact and UI.
- Force-terminated/relaunched app recovers every closed segment and never marks
  missing time as captured.
- Recovery works from native `capture.json` even if WebView localStorage is
  absent. A persisted `process_restarted` state routes Stop to the Rust pending
  finalizer instead of calling a recorder object from the prior process.
- A successful iOS export matches the source size and SHA-256; cancel or failed
  verification preserves the App-owned original.

### Phase 7 — Android recorder

Linear: [AX-243](https://linear.app/ax-agent-swarm/issue/AX-243).

- Native microphone foreground service, persistent notification, segmentation,
  lifecycle recovery, resumable upload, and recovery UI.
- Preserve existing archive sync, R2 playback, cache, and offline pinning.

Acceptance:

- Two-hour screen-off recording passes on reference Pixel and one OEM device.
- Task removal and offline periods preserve the recording and upload queue.
- Less than 1GiB free blocks Start before the foreground service creates a
  session; crossing the floor during recording yields an explicit interrupted
  state rather than silent truncation.
- Android never labels playback/system capture as supported Meeting audio.
- A failed or cancelled SAF export keeps the App-owned recording and reports an
  explicit result; a successful export matches the source size and SHA-256.
- The Android application-module instrumentation test writes from the real Rust
  inbox root to a MediaStore `content://` destination, reopens the result, and
  removes both test artifacts. A paired import test copies a real MediaStore
  `content://` WAV into the native stable inbox, proves replay idempotency, and
  acknowledges its cleanup. Actual user picker/`ACTION_CREATE_DOCUMENT`
  interaction remains part of the physical-device acceptance matrix.
- With emulator audio explicitly disconnected from the host, the App-module
  foreground-service test grants only disposable runtime permissions, writes a
  real PCM WAV, proves continued growth after `finishAndRemoveTask()` and screen
  sleep, checkpoints pause/resume/stop, verifies its durable journal, restores
  the screen, and removes all test audio. This is service integration evidence,
  not the physical two-hour/OEM lifecycle matrix.
- A separate App-module test starts from a persisted `RECORDING` snapshot while
  the service is inactive, proves recovery changes it to `INTERRUPTED`, repairs
  the fabricated WAV length, and records the interruption event. In addition,
  `scripts/demo/test_android_process_death.sh` runs a host-orchestrated pair of
  instrumentation phases on a clean emulator: it confirms the foreground
  recorder process and durable `RECORDING` snapshot, force-stops the package,
  then launches the real Tauri Activity in a newly created process and proves
  its Rust `mobile_status` wrapper/native plugin path recovers the same WAV. A
  final clean-App pair creates a real native ContentResolver import, force-stops
  the process before Rust adoption, then launches the Tauri Activity in a new
  process and waits for the Rust inbox envelope/track plus native
  acknowledgement cleanup. The script refuses physical devices; Pixel/OEM
  force-kill/relaunch and user picker interaction remain in the physical
  acceptance matrix.

### Phase 8 — Unified UX and archive integration

Linear: [AX-244](https://linear.app/ax-agent-swarm/issue/AX-244).

- Extend the shared viewer with Record, Import, active recording, and Processing
  surfaces while preserving the current archive IA.
- Implement the interaction state table, keyboard/touch/a11y behavior, and
  platform capability copy.
- Refresh the archive automatically when a job reaches complete.
- Add reprocess, retry, cancel, export-original, and recovered-recording actions.

Acceptance:

- Every state in the table has deterministic fixtures and visual coverage.
- Desktop 1440px archive behavior remains unchanged outside the new controls.
- iPhone, Pixel, and macOS flows pass one-hand/keyboard/screen-reader smoke
  tests appropriate to the platform. Windows joins this matrix in Release 2.

### Phase 9 — Privacy, recovery, release, and rollout

Linear: [AX-245](https://linear.app/ax-agent-swarm/issue/AX-245).

- Focused trust-boundary review and negative tests.
- Resumable temp-object sweeper, retention ledger, secure credential
  replace/delete, rate limits, redaction, concurrent-publication conflict tests,
  and restore drills.
- macOS, iOS, and Android Release 1 lanes. The Windows lane remains an unsigned
  compile artifact until Release 2 physical acceptance.
- iOS release validation checks the main-App and Share-Extension provisioning
  profiles for the shared App Group before archive/export.
  On 2026-09-03 the Share bundle ID and shared App Group were registered and
  explicitly associated with both targets. Fresh `EchoWall App Store` and
  `EchoWall Share App Store` profiles contain the group; a distribution-signed
  archive and locally exported IPA expose the same effective entitlements for
  both `.app` and `.appex`. GitHub's two profile secrets were rotated from the
  verified assets. A current-Team development certificate plus App/Share device
  profiles also produced a signed debug archive installed on the named iPhone;
  a fabricated M4A/receipt injected into the real App Group was staged, adopted
  by Rust, acknowledged, and removed. See
  `evidence/ios-app-group-signing-2026-09-03.md`. Direct Files/Voice Memos Share
  Sheet invocation and lifecycle proof remain open and must not be inferred
  from distribution signing or CoreDevice injection. The current signed
  physical archive also includes a fix for the 16-item drain: supported audio
  is filtered before `prefix(16)`, so receipt files cannot consume half the
  batch bound, and the main App creates the shared inbox idempotently. A
  CoreDevice 32-file pre/post readback was not reliable enough to count as UI
  or batch runtime proof.
- Bilingual docs, permission screenshots, privacy disclosures, migration and
  rollback instructions.
- The macOS canonical build uses a composable rustc wrapper plus Clang
  file/debug/macro prefix maps and release symbol stripping. Run
  `desktop/scripts/verify_macos_bundle_privacy.sh` on the final `.app`; CI runs
  it after signed/notarized universal build and before upload. It rejects
  `/Users/` builder paths, credential/Python/model artifacts, private-key/key-ID
  markers, a worker-count mismatch, worker network linkage, or invalid deep
  signing. The sidecar builder creates and verifies explicit arm64+x86_64
  workers for the universal target. Because Tauri creates the DMG after it
  notarizes the App, CI then runs `desktop/scripts/notarize_macos_dmg.sh` on the
  exact versioned disk image and requires a stapled ticket plus Gatekeeper
  acceptance before upload. The capture-fix source was rebuilt on 2026-09-04;
  the resulting universal App and independently notarized DMG pass every gate,
  with no release upload. See
  `evidence/macos-distribution-bundle-2026-09-03.md`.

Acceptance:

- No provider/write credential is present in app binaries, plaintext files,
  webview/extension storage, logs, fixtures, screenshots, or crash reports;
  runtime credentials exist only in the platform secure store.
- Leak scan, offline tests, platform builds, native capture fixtures, and
  opt-in live-provider tests pass.
- A failed rollout can disable new job creation without affecting the existing
  Voice Memos watcher or archive viewing.
- Pilot users complete one recording and one import on macOS, iOS, and Android
  before the old path is considered optional. Windows must pass the same pilot
  before its Release 2 rollout.

## Test strategy

### Offline/default

- JSON schema compatibility and unknown-version rejection.
- State-transition property tests, retry bounds, idempotency, and cancel races.
- Hash dedup across names/devices and same-name/different-content handling.
- Path traversal, symlink, extension spoofing, malformed media, decompression
  bombs, oversized input, decoder timeout, and signed-URL log redaction.
- Mix alignment fixtures with distinct mic/system tones and intentional gaps.
- Provider transcript fixture replay and publisher failure recovery.
- Viewer state fixtures for every interaction-table cell.

### Native integration

- macOS ScreenCaptureKit permissions, app exit, display/output route changes.
- Windows WASAPI process tree, endpoint invalidation, communications device,
  mic privacy denial.
- Selected-browser scope/warning and unrelated-app exclusion.
- macOS local Whisper model integrity, offline inference, memory/swap/thermal
  bounds, local summary schema/chunking, verified local archive, process
  restart, and remote-ambiguity takeover.
- iOS lock/interruption/route/force-quit recovery and security-scoped import.
- Android foreground-service lifecycle, task removal, reboot, SAF/share import.

### Opt-in live

- Direct in-app TOS upload/presign/cleanup.
- 妙记 submit/poll/result with one short fixture and one long fixture.
- Explicit local-model installation from an allowlisted source, followed by a
  network-disabled macOS fallback run; model downloads are never a default test.
- Network-denied local Qwen summary replay from the stored transcript and local
  archive reopen/hash verification.
- Gemini summary replay from the stored transcript.
- Archive commit/push, R2 durable audio, viewer refresh, and configured
  deliveries.

Live tests use fabricated speech and demo archives only. Personal recordings,
presigned URLs, and credentials never enter fixtures, screenshots, CI logs, or
the public repository.

## Rollout

1. Release 1 internal desktop alpha: import + macOS modes behind a feature flag.
2. Release 1 iOS TestFlight and Android sideload beta for mic recording/import.
3. Release 1 selected-browser Meeting beta with explicit other-tabs warning.
4. Release 1 Apple Silicon Mac pilot enables the local Whisper fallback only
   after model disclosure/install and the outage/takeover matrix passes.
5. Enable the in-app Rust processing engine for all Release 1 clients while
   keeping the Mac watcher available as fallback.
6. After 30 days with no lost recordings and successful cleanup/replay drills,
   document the new path as primary. Do not delete the old watcher in this
   milestone.
7. Release 2 Windows alpha starts only on Windows 11 hardware after process
   isolation, device recovery, Credential Manager, and clean-install checks pass;
   promote it independently without reopening Release 1 claims.

Kill switches are independent: new recording, file import, selected-browser
capture, and direct provider processing can each be disabled without breaking
archive viewing.

## Estimates and dependency order

Original planning estimates below are historical; they do not restart completed work.

These are engineering ranges after Phase 0, not calendar promises:

| Work | Estimate | Depends on |
|---|---:|---|
| Phase 0 spikes/ADRs | 4–7 days | none |
| Shared ingest + embedded Rust processing | 10–16 days | Phase 0 |
| macOS local Whisper fallback | 8–15 days | shared processing core |
| Cross-platform import | 5–8 days | shared Rust core |
| macOS recorder | 6–10 days | shared core |
| Windows recorder + release lane | 7–12 days | shared core |
| Browser-app fallback | 2–4 days | desktop capture adapters |
| iOS recorder | 5–8 days | shared Rust core |
| Android recorder | 5–8 days | shared Rust core |
| Unified UX/archive integration | 5–9 days | platform vertical slices |
| Hardening/release/pilot | 6–10 days | all above |

Expected total with selected-browser capture and the local Mac fallback is
roughly 63–107 engineering days
for one person. Parallel desktop, mobile, and browser work can reduce calendar
time only after the envelope, provider, and UX contracts stabilize.

## Decision and reversal conditions

Recommendation confidence: high for mic recording/import on all platforms and
all three desktop modes, including explicit selected-browser capture.

Preserved decisions; revisit only for a demonstrated delivery blocker:

| Decision | Recommended default | Evidence needed to reverse it |
|---|---|---|
| Processing topology | embedded Rust engine; no self-hosted EchoWall service | only revisit if a named platform cannot durably resume the direct provider/archive path and an explicit new server decision is approved |
| macOS local STT fallback | keep pinned Whisper available | change only for a concrete ordinary-use or compatibility defect; no new acceleration benchmark is required |
| macOS local ASR closeout | finish current MOSS Q8 / transcribe.cpp route | reopen model choice only for a demonstrated substantial ordinary-use failure that a bounded fix cannot address; isolated scores do not trigger research |
| macOS local diarization | implemented joint MOSS attribution plus versioned SpeakerKit where used | preserve honest labels and practical usability; fix integration, recovery or packaging defects without a new count/short-turn benchmark gate |
| macOS local summary | existing pinned Qwen3.8-27B UD-Q4_K_XL / llama.cpp worker, 32 GiB tier, closed schema | investigate only repeated missing/fabricated key content or an ordinary-use runtime blocker; no model/runtime comparison is scheduled |
| Exact browser tab | deferred beyond Release 1 and Release 2 | a separately approved future milestone accepts extension/bridge scope and maintenance |
| Separate source-track retention | keep locally for 30 days; canonical mixed audio remains in private R2; permanent source-track backup is opt-in | AX chooses permanent raw-track preservation and accepts roughly doubled storage |
| Windows baseline | Windows 11 for the full three-mode contract | a supported Windows 10 process-loopback path passes the same native-app isolation tests |
| Android ABI | arm64-v8a devices + x86_64 emulator | a named supported 32-bit device and an upstream-compatible TOS/CRC path justify armv7/i686 |
| Import timestamp | embedded recording date → filename parse → file mtime → user confirmation | a source family proves those metadata fields systematically misleading |
| iOS scene lifecycle | Current Xcode26 legacy lifecycle is supported and Apple accepted0.3.0(3); no scene plist workaround on tao0.35.3 | Before SDK27 adoption, migrate through a supported Tauri/tao release and verify cold launch; current phone tests remain deferred |

Revisit the architecture if any of these occur:

- Exact-tab capture stays outside this roadmap. A future extension/bridge may be
  reconsidered only as a separately approved milestone; Release 1 and Release 2
  ship selected-browser capture with explicit other-tabs disclosure.
- A platform cannot make safe progress with the embedded engine: first narrow
  background expectations and prove launch-time recovery. Adding an EchoWall
  server is a new architecture decision and may not happen implicitly.
- Separate desktop tracks drift beyond the approved tolerance: move mixing to a
  single native clock domain or keep segment-level correction metadata.
- 妙记 provider limits reject normalized mobile/import formats: centralize all
  transcoding in the Rust processing engine and narrow accepted artifacts.
- A platform cannot recover a two-hour recording through its lifecycle tests:
  ship import first and hold that platform's recorder rather than claiming
  unreliable capture.

## Explicitly not in scope

- Silent automatic recording or calendar-triggered meeting surveillance.
- Replacing Apple Voice Memos or building a watchOS recorder in this milestone.
- Android meeting/call/system-audio recording.
- iOS cross-app/system-audio recording for v1.
- Python/CLI `whispermlx`, an embedded Python runtime, automatic model download,
  or local fallback outside Apple Silicon macOS.
- Exact browser-tab capture or a browser extension.
- Video/screen recording, live transcription, live diarization, persistent
  speaker enrollment/identification, or live meeting bots.
- Multi-user accounts, sharing, billing, or organization administration.
- Removing Gemini/OpenAI fallback code or migrating `manifest.json` to a new
  top-level schema.
- Deleting the existing Mac watcher before the new path completes its pilot.

## Release acceptance criteria

Release 1 is complete only when the delivered capabilities below are verified.
Model quality uses the practical acceptance section and Phase 1A, with existing
quality evidence reused. Remaining device/authority limits must be named and
held explicitly; they are not a reason to resume model research.

- local transcription, attribution and summaries are useful and broadly
  comparable to Feishu on representative ordinary use; numerical research
  scorecards and extra human-review studies are not required;

- macOS ships Voice Memo, selected-source Meeting, and System Capture with
  separate mic/system tracks;
- browser Meeting clearly labels selected-browser scope, warns that other
  audible tabs are included, and excludes unrelated applications;
- iOS and Android record mic audio reliably through their supported background
  lifecycle;
- macOS, iOS, and Android import supported audio files, deduplicate, and recover
  from interruption;
- every source produces the same validated, replayable `RecordingEnvelope`;
- provider and archive credentials are never bundled or exposed to the webview
  or extension; user-provided credentials remain only in native secure storage;
- every new App job completes through embedded Rust without a project-owned
  broker/server, Python worker, always-on Mac, or EchoWall processing endpoint;
- an Apple Silicon Mac with an explicitly installed local model can finish a
  confirmed TOS/妙记 outage through the same Rust ledger, with network-disabled
  transcription, diarization, summary, and a reopenable verified local archive,
  with one winning transcript/publication and zero provider/archive requests;
- one interrupted job can resume from every stage without recapture or duplicate
  provider charges;
- viewer edits, attachments, colors, and deletes survive process termination,
  preserve concurrent changes from other devices, and never replay across a
  changed GitHub/R2 destination;
- a delete/reprocess race cannot resurrect a tombstoned recording or leave an
  App-created R2 generation without a durable publication or cleanup intent;
- existing archives, mobile sync/playback, and the Voice Memos watcher continue
  to work; and
- Release 1 artifacts, docs, privacy disclosures, and end-to-end proof exist for
  macOS, iOS, and Android.

Windows Release 2 is complete only when the same shared envelope, processing,
privacy, import, browser-scope, and archive invariants remain true and all Phase
4 acceptance criteria pass on named Windows 11 hardware. Until then Windows is
an explicitly unsupported technical preview: its code and unsigned CI artifact
may be inspected, but it must not appear in user-facing Release 1 availability
copy or be treated as shipped.
