# macOS universal distribution bundle evidence — 2026-09-03

This proof used only repository source, public build dependencies, the existing
Developer ID identity, and the existing App Store Connect notarization key. It
did not open or process a recording, access an archive, or publish a GitHub
release.

## Failures found before acceptance

The first arm64 release App was built without release-signing environment
variables. Its executable therefore had only a linker ad-hoc signature and the
bundle had no sealed resources. Strict `codesign` correctly rejected it. A
second build with the real Developer ID and notarization inputs produced a
signed, hardened-runtime, notarized, and stapled App.

That build exposed two independent release-path defects:

1. Tauri notarized and stapled the App before creating the DMG, then only signed
   the newly created disk image. Gatekeeper rejected that DMG as
   `Unnotarized Developer ID`.
2. The checked-in sidecar builder produced arm64 and x86_64 files but not the
   `*-universal-apple-darwin` artifacts that Tauri resolves for a universal
   bundle. Both main-program slices compiled, but packaging stopped before it
   could copy the workers.

No failed artifact was published or uploaded to a release.

## Corrections

- `desktop/scripts/build_local_sidecars.sh` now creates every universal worker
  explicitly with `lipo`, verifies both `arm64` and `x86_64`, and repeats the
  no-network linkage check on the merged artifact.
- `desktop/scripts/notarize_macos_dmg.sh` rejects a missing or non-Developer-ID
  disk image, submits the signed DMG with `notarytool`, staples and validates
  the ticket, and requires Gatekeeper to accept the final file.
- The macOS release workflow resolves the versioned DMG before verification,
  runs the App privacy/signing gate, independently notarizes the exact DMG,
  and only then reaches either upload step.

## Final universal proof

The canonical macOS build completed with
`--target universal-apple-darwin`. The main executable and all four fixed
one-shot workers report both `x86_64` and `arm64` slices:

- `desktop`
- `echowall-whisper-worker`
- `echowall-diarization-worker`
- `echowall-summary-worker`
- `echowall-qwen-worker`

The final current-source App is 50,312 KiB after retiring the vulnerable TOS
SDK graph. Its identifier is
`ai.ax.watch-transcriber`; the effective signature uses Developer ID
Application, hardened runtime, a secure timestamp, sealed resources, and a
stapled notarization ticket. These checks all passed on the final App:

- `desktop/scripts/verify_macos_bundle_privacy.sh`
- `codesign --verify --deep --strict`
- `xcrun stapler validate`
- `spctl -a -t exec`, returning `source=Notarized Developer ID`

The independently notarized and stapled final disk image is 20,361,133 bytes
with SHA-256
`ef8f9ee2497aa3d6b02920c2602d31c49d01ae8bbcc5136c1e7905ece246010c`.
`stapler validate` passed and `spctl -a -t open` returned
`source=Notarized Developer ID`.

This closes distribution signing, notarization, universal worker placement,
worker network-linkage inspection, and Gatekeeper acceptance for the current
macOS build. It does not replace the open physical microphone, selected-browser,
sleep/route-change, or long-duration capture matrices, and it does not promote
Qwen over Whisper without the separate human-ground-truth quality gate.

## Current-source rebuild — 2026-09-04

The macOS capture callback-watchdog, shared host-time, discontinuity, and
real-time-safety fixes landed after the first distribution proof above, so the
earlier App and DMG were no longer current. The canonical universal command was
rerun from the resulting worktree without launching any capture fixture or
generating audio.

The final rebuild also includes the later durable upload-cancel reconciliation
fix. The App is 50,332 KiB. The main executable and all four fixed workers again
report both `x86_64` and `arm64`. The App passed the checked-in privacy gate,
strict deep code-sign verification, stapler validation, and Gatekeeper as
`Notarized Developer ID` after Apple returned `Accepted`.

The rebuilt independently notarized and stapled DMG is 20,357,258 bytes with
SHA-256
`fed828fc60ca78deac2a89a1a5fb9105e9c08c82c7004e812404235001088bfb`.
Its stapler validation and Gatekeeper open assessment also passed as
`Notarized Developer ID`. No release or public artifact was uploaded.
