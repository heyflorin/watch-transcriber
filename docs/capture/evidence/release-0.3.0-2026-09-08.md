# EchoWall 0.3.0 release closeout

AX authorized completing code, build and distribution work on September 8,
with physical phone testing excluded. The existing Watch automation remains
independent of App recording preferences.

## Delivered state

- The complete native capture/import/local-processing implementation is committed
  and pushed to the repository's main branch. No model weights, private archive,
  credentials, temporary research data or generated build directories are included.
- Version0.3.0 uses matched iOS App/Share build3. Canonical iOS packaging requires
  a fresh archive and exact version-verified export; stale/fallback outputs are
  rejected. Manual CI dispatch builds only by default. TestFlight requires an
  explicit upload choice; tag publication has one owner and never clobbers assets.
- Rust1.96.0 is pinned for local and hosted builds. The catalog unit test checks
  actual memory detection without requiring the CI host to meet the32GiB model
  installation requirement. Runtime installation/inference guards are unchanged.
- All native speaker tests use XCTest. The one disk-PCM cleanup test was moved
  from Swift Testing with the same assertions; running only XCTest avoids the
  second runner entering the worker executable's main entrypoint. The complete
  suite passes25 tests with one deliberately opt-in diagnostic skipped.
- The signed/notarized local Mac0.3.0 App is installed at `/Applications/EchoWall.app`.
  The older bundle is retained under the private release evidence directory.
  No personal recording, archive or stored credential was altered. The production
  App was not launched automatically.
- iOS0.3.0(3) passed Apple validation, uploaded successfully, finished processing
  as VALID and is IN_BETA_TESTING in both existing internal groups. See
  [the supported iOS release and TestFlight receipt](ios-supported-release-path-2026-09-08.md).
- The signed Android0.3.0/code3000 universal APK contains exactly arm64-v8a and
  x86_64; its release signer matches the previous release. No phone was installed
  to, launched, unlocked or recorded for this closeout.

## Original Watch automation

Read-only service inspection found `com.watch-transcriber` loaded, configured
with the original Voice Memos WatchPaths trigger, correct Python/script paths,
and no disable override. Its most recent recorded exit was0. An idle/not-running
state is expected for this directory-triggered service, not evidence it stopped
watching. The new App's manual/remote/local preference does not affect this path.

The existing doctor's21 configuration checks passed with no warning/failure
when executed under the background service's actual PATH. Transcription and
summary credentials, TOS SDK, original directory, local destinations, Feishu CLI
and service configuration were available. No credential value or personal title
was printed. The doctor's temporary write probe was removed. No actual personal
recording was processed and this is not a fresh provider end-to-end test.

## Retained validation and scope

The core337 tests and strict all-target Clippy pass. The remaining10 Rust crates'
all-target tests/Clippy, sidecar contract/packaging tests, fresh-IPA regression
suite and the corrected Swift suite pass locally on the pinned compiler.
Existing actual isolated-App English/Mandarin/Mixed processing, source/archived
hash equality, original export and cold-reopen proof is retained; no new model
research or all44 campaign was needed.

The first hosted failure was a test's host-memory assumption, then a moving
compiler introduced new lints. Both causes were corrected without weakening
runtime checks. Historical failing runs remain evidence; current verification
is the corrected run rather than an obsolete failure card.

Physical phone tests remain deferred as requested, including OPPO's known
recording-stop-after-task-removal behavior. The future iOS scene migration is
an SDK27 upgrade requirement, not a current Xcode26 release blocker.

Mac live system/meeting-source tests retain their documented limits. The initial
read-only routing check found Wave Link monitoring all audio and Chrome into
the user's normal output. A virtual-device name did not establish a silent
test sink. Existing long Meeting and Voice Memo results stay valid, and are
not described as System Capture or all-source-restart proof.

### September 9 Mac test attempt and audio recovery

AX subsequently authorized Mac testing and temporary monitoring mute. The
attempt muted Wave Link's All audio channel, but the short harness exited at
its microphone/screen-recording permission preflight before recording began.
The mute was not promptly restored, and AX reported lost sound. This attempt
is failed preflight evidence, not a capture pass.

At recovery, the complete Wave Link configuration was compared with the private
pre-test backup: it matched exactly, All audio was unmuted, and no generated
tone or capture-test process remained. AX confirmed sound returned before
requesting continued closeout. The permission-only recheck still returned
screen recording unavailable and microphone authorization not determined for
the current test process. A permission-capable launch context and a verified
non-monitored test source are prerequisites to another capture attempt.
Do not mute the normal output while preparing builds or resolving permissions.

No new long System Capture, real Zoom/Teams/browser exclusion, source/route
restart or signed-App recovery result is claimed. Sleep/wake remains held by
the existing no-lock/no-sleep instruction; phone tests remain deferred.

Local receipts, exact binaries, private logs and previous installed App backups
are under `local-eval/release-20260908/` and are excluded from Git.

## Hosted build and public downloads

[EchoWall v0.3.0 preview release](https://github.com/xingfanxia/watch-transcriber/releases/tag/v0.3.0)
is published. Its Mac DMG, Android APK and SHA256SUMS.txt are publicly downloadable;
anonymous metadata and download requests all succeeded. Uploaded asset sizes and
server SHA-256 digests exactly match the inspected hosted artifacts.

- [Final macOS run](https://github.com/xingfanxia/watch-transcriber/actions/runs/34312122844):
  all tests, strict Clippy, fresh universal bundle, privacy, App/DMG notarization,
  Gatekeeper and artifact upload pass at `cd109bd`.
- [Pinned multi-platform run](https://github.com/xingfanxia/watch-transcriber/actions/runs/34311235827):
  Windows, Android and iOS lanes pass at `a1918ae`; its Mac-only Swift runner
  failure is superseded by the final run. The difference to the release commit
  is limited to that native test, its runner/workflow selection and README;
  the Android/iOS/Windows product source is unchanged.
- Release tag `v0.3.0` points exactly to `cd109bd116dd07e75e0f66e65206e12d104ed690`.
  Publishing the inspected artifacts also created a redundant tag-triggered run;
  that run was canceled deliberately to avoid rebuilding/replacing an already
  verified published release. The completed runs above own the evidence.

Public package SHA-256:

```text
104b49b91a22d6f741692308b3b33e0769961c69902172346e8b51c1a9a22960  EchoWall_0.3.0_universal.dmg
cce048d7d1728b0d4b2ef62e2e53dc05e2d545dc22d50f17a9562186fe16ff61  EchoWall_0.3.0_universal.apk
```

The installed local Mac build and the separately verified local Android build
remain valid, with their own hashes in local receipts; the public downloads are
the hosted build artifacts. This distinction is explicit, not a claim that
independently signed/compiler-environment artifacts are byte-identical.
