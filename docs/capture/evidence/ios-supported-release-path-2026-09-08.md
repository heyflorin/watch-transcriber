# iOS supported release path — 2026-09-08

## Current release decision

EchoWall 0.3.0 can use the existing stable Tauri 2.11.5 /
tauri-runtime-wry 2.11.4 / tao 0.35.3 stack with Xcode 26.6 (17F113),
which supplies the iOS 26.5 SDK. Do not add a scene manifest to this stack
solely to silence a future SDK warning.

Apple's [current submission requirements](https://developer.apple.com/news/upcoming-requirements/)
require Xcode 26 or later and the iOS 26 SDK or later, effective April 28,
2026. Apple's [TN3187](https://developer.apple.com/documentation/technotes/tn3187-migrating-to-the-uikit-scene-based-life-cycle)
says scene adoption becomes a launch requirement when building with the
next major SDK after iOS 26. That is a future SDK upgrade requirement,
not a blocker for this Xcode 26 release.

The current main-App plist has no `UIApplicationSceneManifest`. The pinned
tao source registers the scene callback only when `multiple_scenes_enabled()`
is true and explicitly calls `on_app_ready()` on the existing non-scene
startup path otherwise. [Tauri's tao changelog](https://tauri.app/release/tao/all-versions/)
records a fix for a scene-configuration use-after-free in 0.36.0. Enabling
that scene path on 0.35.3 would introduce a known release-launch risk.

The retained September 6 App Store IPA independently reports Xcode 26.6,
SDK `iphoneos26.5`, minimum iOS 14.0, no scene manifest, and exempt-only
encryption. [September 3 launch evidence](ios-app-group-signing-2026-09-03.md)
records current-source simulator rendering and a signed physical cold launch.
These are existing observations, not a claim that the new release has passed
all physical-device scenarios.

Therefore the release gate is a fresh build/export, exact artifact validation,
and App Store Connect processing on the supported toolchain. Requiring a
particular future Tauri/tao version indefinitely is not an Apple requirement.
Before moving to SDK 27, adopt a supported scene lifecycle and verify its
launch/lifecycle behavior; keep that separate from this release.

## App Store Connect readback

Read-only authenticated calls to Apple's official API on September 8 verified
that the EchoWall app record already exists for `ai.ax.watch-transcriber`.
Its only existing builds were 0.2.0 and 0.2.1, uploaded July 29, both `VALID`
and `IN_BETA_TESTING` internally, with October 27 expiration dates. There
was no 0.3.0 build. Version 0.3.0, build 3 is available for a fresh upload.

Both existing internal groups have `hasAccessToAllBuilds=true`. A successfully
processed upload should therefore enter existing internal testing without
creating groups or sending new invitations; verify actual state afterward.
The separate App Store 1.0 draft remains `PREPARE_FOR_SUBMISSION` and is
outside this TestFlight release.

## Acceptance boundary

Physical-phone installation, launch, recording, locked-screen/background
capture, and real Files/Voice Memos Share Sheet scenarios are deferred under
the user's instruction. This release does not claim those tests are complete.
New release artifact and TestFlight processing evidence will be appended after
the corresponding actions succeed.
