# Practical closeout — 2026-09-06

Current local builds are ready for inspection. The macOS local-processing user
path passes real isolated-App UI checks at AX's practical Feishu/Miaoji standard.
Release 1 still has the physical-platform acceptance items below; no public
release, TestFlight upload or store submission was performed.

## Artifacts

All paths are relative to the repository. Exact receipts are retained under
`local-eval/closeout-20260906/`; the compact public-safe record is
[practical-closeout-2026-09-06.json](practical-closeout-2026-09-06.json).

| Platform | Current artifact | Verification |
|---|---|---|
| macOS | `local-eval/closeout-20260906/artifacts/EchoWall-0.2.0-macOS-universal-20260906.dmg` | 24,667,130 bytes; Developer ID, hardened runtime; App and DMG notarized/stapled and accepted by Gatekeeper; privacy gate passes; mounted DMG contains the exact six verified App binaries |
| Android | `local-eval/closeout-20260906/artifacts/EchoWall_0.2.0_android_universal.apk` | 44,390,454 bytes; version0.2.0/code2000; arm64-v8a + x86_64; release signature verified and matches the earlier release signer |
| iOS | `local-eval/closeout-20260906/artifacts/ios-export/EchoWall.ipa` | 9,099,750 bytes; fresh Apple Distribution archive/export; App + Share identities, arm64 slices, profiles, App Group and strict signatures verified |

SHA-256:

```text
macOS   1de724932c0e73ca0d96b359c834bc5004ab9b7ae0fbec2f2ca53cd7cdb4ec01
Android 6cc1002471c7fd9aa3e5cf87bda53a20692c376dad42dded41815e4a41f1d8ef
iOS     37da77e4cb9433fbf770e1cd4b9188d309c4eaa65bf862131f721f79f795e0c7
```

The iOS IPA is an App Store export for an authorized distribution step; it is
not a development IPA that can be directly sideloaded. Android used installed
NDK27.1.12297006, while CI specifies27.2.12479018. Earlier build outputs and the
initial mobile `libc` compile failures are preserved. Moving the existing
`libc` dependency from macOS-only to Unix fixed both mobile builds; the macOS
dependency graph and behavior are unchanged.

## Use on Mac

Open the DMG and copy EchoWall to Applications. On Apple Silicon with macOS14+
and at least32GiB unified memory, open **MOSS 本地** and explicitly install the
model pack. Import an audio file, confirm its time/title, then choose
**MOSS 本地处理**. Whisper and cloud processing remain separate choices.

The processing settings now save a new-recording preference in App storage.
It survives the changing localhost port and cold launches. An unset, unreadable
or unavailable local choice leaves recordings on the Mac until an explicit
choice; it never silently uploads them. Model download remains explicit.
Existing jobs retain their original route and persisted model policies.

Local completion produces transcript, anonymous speakers, summary and a
reopenable archive without provider or cloud-backup calls. A difficult isolated
tail can still fail visibly; the original audio and retry/export remain.

## Actual App evidence

The retained `EchoWall-QA-closeout-20260906-final.app` uses the current native
importer, Rust engine, bundled workers and viewer, with separate App-data and
Keychain namespaces and a deny-only QA HTTP connector. Native Accessibility
clicks exercised its actual WebKit UI and file dialogs; there was no fake IPC
in these checks, no audio playback and no production-data access.

- Mandarin01: native picker → explicit MOSS selection →202 transcript segments,
  anonymous speakers,7 summary fields and local publication. Summary and
  transcript opened in the App.
- Mixed01: the same path with2 processing windows →100 segments,7 summary fields
  and local publication. Summary opened; native export reproduced the exact
  source SHA-256.
- English01: the retained184-segment/7-field generation opened in the current
  App. Its earlier generation was preserved; this was not a fresh English
  inference run. Current English worker/source-engine quality proof is reused.
- All3 original/inbox/archive audio hashes match. There are exactly3 archive
  entries, no remote task/object checkpoints and no QA HTTP-denial markers.
- A same-bundle cold launch kept all3 ledgers, manifest, notes, audio and the
  MOSS preference unchanged over11.4seconds, with no child worker. The result
  opened again through the native UI. The QA App was then stopped.

These are real isolated-App UI and artifact proofs. They do not claim that the
production installation was launched or that a whole-App OS network trace was
captured. Production worker network denial and package linkage checks remain
intact. Older model-active crash/recovery proof remains applicable to its
recorded generation; it was not silently re-labelled as a new crash run.

## Relevant checks

- Rust App:337 passed,31 opt-in ignored;3 additional physical-capture tests remain
  explicitly ignored. Strict lib/test clippy and formatting pass.
- Packaging/QA-path tests:9 passed; final actual macOS privacy/signature checks
  also pass.
- Python:44 tests and16 subtests passed; ruff passed.
- Expanded fabricated MOSS UI and full capture/import UI suites passed, covering
  preference races, unavailable-model holds, explicit routes and radio layout.

## Held scope

- macOS capture: remaining real-app/browser exclusion, route/source-restart and
  signed-App capture recovery checks. System long capture needs a proven
  non-monitored source; no audible fixture or Mac lock was used.
- iOS: real Files/Voice Memos Share Sheet, physical capture/lifecycle and the
  supported UIScene cold-launch matrix. A signed IPA does not close these.
- Android: real SAF/share/export and physical Pixel/OEM lifecycle checks. No
  physical Pixel is available. The earlier OPPO task-removal failure remains
  recorded; lock/task-removal testing is prohibited under current authority.
- Public distribution and production installation/launch have not occurred.
  Windows remains Release2.

No model-selection, fine-window, all44, perfect-score or blind-review campaign
is scheduled. The next work is limited to the named physical/distribution scope.

### Read-only physical preflight after delivery

The next goal turn confirmed OPPO's ADB connection is authorized but the device
is Dozing with keyguard showing. The named iPhone responds to the developer
lock-state query (`passcodeRequired=false`, `unlockedSinceBoot=true`); that does
not prove its current UI state or any Share Sheet journey. No device was woken,
unlocked, installed to, launched, recorded or otherwise changed. The three
retained artifacts and completed App receipts remain present. Real phone UI
acceptance awaits user coordination; this does not reopen model/package work.

The same bounded preflight checked the [official Tauri releases](https://github.com/tauri-apps/tauri/releases):
stable Tauri remains2.11.5 and runtime-wry2.11.4. The
[released runtime manifest](https://raw.githubusercontent.com/tauri-apps/tauri/tauri-runtime-wry-v2.11.4/crates/tauri-runtime-wry/Cargo.toml)
still requires tao0.35.0 (this repository locks0.35.3), so it does not supply the
plan's tao0.37 UIScene migration. No applicable released upgrade was found;
no dependency was changed and no finished artifact was rebuilt for this check.
