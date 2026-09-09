# macOS native import UI evidence — 2026-09-03

## Scope

This proof used a distinct test bundle identifier, a fresh App-data directory,
a fresh local archive, and one generated two-second 523 Hz mono PCM WAV. It did
not read a recording or archive, configure credentials, or contact TOS, Miaoji,
Gemini, GitHub, or R2.

Opening the native macOS file picker caused its Accessibility tree to enumerate
some filenames from the current Documents directory and the Go-to-folder field
to expose a previously entered local path. None of those files or paths was
opened, read, selected, or transmitted. The test immediately used
Go to Folder with the exact generated fixture path and did not browse any user
directory.

## First-run correction

The signed isolated-App pass immediately before this import found that the
authenticated loopback page lacked the remote capability grant for
`initialize_local_archive`. The same capability also lacked three commands
used by the compiled viewer: transcript-only acceptance, Whisper takeover, and
post-local cloud backup. The four exact grants were added without enabling
external opening, and a regression test now requires them.

After rebuilding, the visible first-run action completed and opened an empty
local archive. The empty archive rendered `0%` for the night-recording statistic
after a separate divide-by-zero fix, rather than the prior `NaN%`.

## Import and durable recovery

The real `NSOpenPanel` path selected `fabricated-import.wav`. The App review row
reported PCM signed 16-bit little-endian, two seconds, and approximately 63 KB,
kept the original display filename, and accepted an explicit fabricated title
and one-speaker hint. Starting processing created one Rust inbox package and one
ledger entry.

The source and managed `tracks/imported.wav` copies both had SHA-256
`b52e560f56b9710dace263d4316e9f2a0b501a16eb42f57fb7b04f36f7557023`.
The envelope recorded `file_import`, macOS, 2,000 ms, the original display
filename, and the managed normalized path. The source remained unchanged.

With no processing credentials, upload was rejected before dispatch and no
TOS or Miaoji checkpoint existed. This exposed a state-machine defect: the
ledger remained `uploading`, so UI and relaunch recovery appeared perpetually
in progress. The engine now moves only a proven `Rejected`/`NotDispatched`
upload to durable `provider_failed`; transient or ambiguous network failures
remain resumable in `uploading`. Retry returns to `uploading` when no TOS
receipt exists, while an existing receipt still returns to Miaoji submission.

A focused state-machine test proves reject, durable failure, retry, upload,
submit, and poll ordering. A rebuilt App relaunched the original synthetic
checkpoint, converted it to revision 3 `provider_failed`, and visibly showed
the recovered-task row with Retry, Install local models, Export original, and
Cancel. Final readback still showed no TOS, Miaoji, or transcript checkpoint.

The test App process, distinct App-data directory, local archive, source WAV,
managed WAV, and generated test bundle were permanently removed. This closes
the macOS native picker/review/copy/relaunch slice for a supported WAV. It does
not substitute for `.m4a`/`.mp3` UI repetition, mobile picker/share UI, or a
physical provider-complete pilot.
