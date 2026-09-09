# iOS App Group signing and export proof — 2026-09-03

## Scope

This proof covers Apple resource registration, App Group association,
distribution profiles, a signed archive, and a locally exported App Store IPA.
It did not upload to App Store Connect or TestFlight and did not use personal
audio.

## Apple resources

- Registered the universal Share Extension bundle ID
  `ai.ax.watch-transcriber.share`.
- Registered App Group `group.ai.ax.watch-transcriber` and associated it with
  both `ai.ax.watch-transcriber` and
  `ai.ax.watch-transcriber.share` in Apple Developer Certificates,
  Identifiers & Profiles.
- Enabling the `APP_GROUPS` capability through App Store Connect API alone was
  insufficient: profiles generated before the explicit association contained
  an empty `com.apple.security.application-groups` array. Those temporary
  profiles were not installed or promoted.
- After the explicit association, fresh verification profiles contained exactly
  `group.ai.ax.watch-transcriber`. The temporary remote profiles were deleted
  after verification.

The final active App Store profiles are:

| Target | Profile | Application identifier |
|---|---|---|
| Main App | EchoWall App Store | 2T5DG4GZBN.ai.ax.watch-transcriber |
| Share Extension | EchoWall Share App Store | 2T5DG4GZBN.ai.ax.watch-transcriber.share |

Exact profile identifiers are retained in the private signing receipts.

Both profiles expire on 2027-07-29, use the existing Apple Distribution
certificate, contain the shared App Group, and set `get-task-allow=false`. The
superseded main profile remains in the local credential vault under an explicit
`pre-app-group` backup name; it cannot be used for future EchoWall builds.
GitHub secrets `APPLE_PROVISIONING_PROFILE` and
`APPLE_SHARE_PROVISIONING_PROFILE` were installed from the verified final
profiles at 2026-09-03T20:34Z. No credential bytes or base64 were logged.

## Signed artifact evidence

`npm run tauri -- ios build --export-method app-store-connect --ci` produced a
distribution-signed archive. The archive and the final IPA both passed strict
signature inspection with these effective entitlements:

| Signed item | Application identifier | App Groups | Team | Debuggable |
|---|---|---|---|---|
| `EchoWall.app` | `2T5DG4GZBN.ai.ax.watch-transcriber` | `group.ai.ax.watch-transcriber` | `2T5DG4GZBN` | no |
| `EchoWallShare.appex` | `2T5DG4GZBN.ai.ax.watch-transcriber.share` | `group.ai.ax.watch-transcriber` | `2T5DG4GZBN` | no |

The exported `EchoWall.ipa` SHA-256 was
`011e40fd7814537b1cca1f7c101153392ef0736835cec1a6c9916991971e3838`.
This digest is evidence for this local build only, not a published release.

## Export failure root cause and fix

The first Tauri export correctly selected both profiles after the App Group
association, then failed with `exportArchive Copy failed`. Xcode's distribution
log showed `/usr/bin/rsync -E` launching a local rsync server resolved from
`PATH`. The Apple client is openrsync-compatible 2.6.9; the child resolved to
Homebrew rsync 3.4.4, which rejected Apple's `--extended-attributes` option.

The minimal reproduction failed under the normal PATH and passed when PATH was
restricted to Apple system directories. `desktop/scripts/export_ios_archive.sh`
now enforces that environment for export, refuses a pre-existing output path,
and consumes the checked-in `ExportOptions.plist`. That plist now maps both the
main and extension bundle IDs, and release CI uses the same script/source of
truth instead of generating a second plist inline. Re-exporting the same
archive through the script succeeded.

## Remaining gate

App Store profiles are intentionally not installable for local debugging, so a
separate Apple Development certificate was created through the ASC API. The
private key/CSR/certificate and Apple-compatible legacy-PKCS#12 backup live only
in the local credential vault. Certificate `9MVHF76864` expires on 2027-09-03.
The two active development profiles are:

| Target | Profile |
|---|---|
| Main App | EchoWall Development |
| Share Extension | EchoWall Share Development |

Both profiles contain the shared App Group, set `get-task-allow=true`, and
contain only the authorized test iPhone; its device identifier is retained
privately. XcodeGen now assigns those profiles to the debug
configurations. A debug archive built through a temporary signing keychain,
passed strict App/appex signature and effective-entitlement inspection, and
installed on the physical phone. The temporary keychain/search-list entry and
temporary p12 were deleted after restoring the original login-Keychain search
list.

A 6.674-second, 59,660-byte synthesized Mandarin/English M4A plus its exact
Share receipt were copied into the physical App Group container. After a cold
EchoWall launch, neither the audio nor receipt remained in `share-inbox`, and
the App process remained alive. Native code can remove both group originals
only after the Rust inbox durably adopts or deduplicates the item, so this is
physical App Group → native staging → Rust adoption → acknowledgement evidence.
The local fabricated audio and readback roots were deleted after their
SHA-256/size/duration were verified.

A later current-source rebuild after the native TOS security migration and
mobile import review again produced a strict-signature-valid physical debug
archive. Both the App and Share extension still expose the exact App Group.
The App installed over the existing test build, cold-launched on the same
iPhone, and remained present in the live process inventory. The corresponding
arm64 simulator archive also installed and visibly rendered the current setup
screen before the simulator was shut down and its screenshot removed.

That review found a real sixteen-item boundary defect in the main-App drain:
the directory was truncated to 16 entries before receipt JSON was filtered, so
16 audio+receipt pairs could stage only about eight audio files per pass. Swift
now filters supported audio entries first and applies `prefix(16)` afterward;
the main App also idempotently creates the App Group inbox when drain begins.
A source-contract regression test fixes the ordering and the current simulator
and physical archives compile the new Swift.

A planned 16-item CoreDevice injection did not produce acceptable runtime
proof. CoreDevice could report a top-level directory copy as successful while
subsequent exact list/copy operations could neither see nor address its files;
exact per-file writes stopped before App launch. No personal Voice Memos item
was used, and the generated local batch fixture was deleted. The code fix and
current signed builds are evidence, but direct 1–16-item Share Sheet runtime
acceptance remains open.

The system Files/Voice Memos Share Sheet itself was not invoked by this
CoreDevice injection, so that UI journey, interruption/background behavior,
and the two-hour lifecycle matrix remain open. This proof must not be described
as direct Share Extension invocation.
