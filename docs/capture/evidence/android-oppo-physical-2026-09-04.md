# Android OPPO physical-device evidence — 2026-09-04

## Device and scope

- Device: OPPO PKH110, Android 16 / API 36, arm64, connected over USB.
- Installed main debug APK SHA-256:
  `03630b3dc7374b3ad11b7900e506a8193599a0c0c11012008d6b289e4341eda3`.
- Installed/current Android-test APK SHA-256:
  `78b3e58ff14539783a6c490482d04709a40ea0e62a32ddfcc77c737b955c9f84`.
- Tests create only App-owned synthetic WAV/MediaStore entries and remove them
  in `finally`. No personal audio or file-picker content was opened.

This is partial OEM evidence. It does not satisfy the physical two-hour,
reference-Pixel, real picker/share/export, reboot, or process-recreation matrix.

## Installation and permission behavior

ColorOS required separate human confirmations for the main and test APKs. The
main App and `AndroidJUnitRunner` package are installed. A first instrumentation
run passed the real ContentResolver import/export and stale-session recovery
tests, but the recorder test reached
`ForegroundServiceDidNotStartInTimeException`. ColorOS had denied ADB shell's
`GRANT_RUNTIME_PERMISSIONS`; both microphone and notification runtime
permissions were still false even though the test had submitted `pm grant`.

Reinstalling the same main APK with `adb install -r -g` preserved the test App
and produced verified `granted=true` microphone/notification state. The
foreground recorder test then passed alone in 1.327 seconds. The paired
`staleRecordingSnapshotBecomesInterruptedAndRepairsWav` plus foreground test
also passed in one process in 1.375 seconds.

The full four-test suite killed the target during the foreground method both
while locked and after AX explicitly unlocked the device, falsifying keyguard
as the sole cause. The method itself executes `finishAndRemoveTask()` and then
screen-off; running that emulator integration on a physical ColorOS device is
not a reliable JUnit boundary.

A new host-observed physical harness then kept the assertions outside the App
process. It proved that task removal initially left the EchoWall PID/service
alive and that the WAV grew at least once, but within the next two seconds the
WAV stopped growing. The harness failed closed, force-stopped only EchoWall,
and removed exactly its random test session. ColorOS also returned to keyguard
as a side effect of task removal on this device. AX directed that subsequent
tests must not lock the phone, so this physical task-removal path is paused and
cannot rerun without a separate explicit lock-risk authorization.

The original screen-off/task-removal instrumentation now calls `Assume` unless
the build is an emulator. A physical rerun completed the three ContentResolver/
stale-recovery tests and reported the recorder method as an explicit skipped
assumption in 0.1 seconds; it did not open the microphone, turn off the screen,
or change lock state. The physical host script also requires both
`ECHOWALL_ANDROID_PHYSICAL_CONFIRM=authorized` and the independent
`ECHOWALL_ANDROID_TASK_REMOVAL_LOCK_RISK=authorized` guard.

## Platform contract and rejected experiment

Android's current official service manifest documentation states that
`android:stopWithTask=false` prevents automatic service stop when the user
removes the owning task; EchoWall already declares that exact value. The
official `Service.onTaskRemoved()` reference says a still-running service is
notified of task removal, while the microphone foreground-service contract
identifies background continuation for voice recorders as the intended use and
requires creation while the Activity is visible on Android 14+.

- <https://developer.android.com/guide/topics/manifest/service-element>
- <https://developer.android.com/reference/android/app/Service#onTaskRemoved(android.content.Intent)>
- <https://developer.android.com/develop/background-work/services/fgs/service-types#microphone>
- <https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start>

A proposed synchronous `task_removed` journal append inside `onTaskRemoved()`
was tested and rejected. On the same cold API-36 Pixel emulator it caused the
Tauri/HWUI process to abort during task teardown with a destroyed-mutex FORTIFY
failure. Removing only that callback restored a clean 4/4 App-module pass in
1.633 seconds. The final code therefore keeps durable audio checkpointing in
the recorder thread and does no synchronous filesystem work from task teardown.
The emulator was cold-started with `-no-snapshot -no-audio`, installed from a
clean package state, then both packages were removed and the emulator stopped
without saving a snapshot.

The OPPO failure remains an OEM/product observation rather than a proven
framework root cause: its host-observed PID and foreground service initially
survived task removal, but its WAV stopped growing within two seconds. No
speculative sticky restart, hidden background microphone restart, or separate-
process migration is accepted without a real-UI proof on a reference Pixel or
another explicitly authorized OEM device.

## Current deterministic gates

The App's `testArm64DebugUnitTest` and `lintArm64Debug` tasks pass. Direct
physical instrumentation has separately passed all four test methods; the safe
physical suite now passes three and intentionally skips the emulator-only
method. The physical host harness truthfully fails continued WAV growth after
task removal. The canonical emulator `:app:connectedArm64DebugAndroidTest`
remains the owner of screen-off service integration; manual physical runner
results do not substitute for the still-open OEM lifecycle product gate.

## Privacy and cleanup notes

A broad OEM `logcat` filter unexpectedly returned unrelated installed-package
identifiers, and a targeted trust-state query exposed an account identifier.
No App content, audio, file, message, or credential was opened; the values were
not reused, copied into the repository, uploaded, or published. Both query
patterns were stopped and are excluded from future evidence collection.

The temporary USB stay-awake setting was always restored to its original value
`0` after install attempts. The main/test packages remain installed because
uninstalling could remove App data; no uninstall is authorized or required for
this partial proof.

After AX manually restored access, the physical device was placed under a hard
no-lock boundary: no further screen-off, keyguard, task-removal, or lifecycle
command that could lock the device may run. A later verification touched only
host builds: the arm64/x86_64 universal debug APK, arm64 unit tests, and arm64
lint all passed without installing to or commanding the OPPO. App capture files
remain empty, the recording service is absent, and USB stay-awake remains `0`.
