# Local speaker diarization evidence — 2026-09-02

This is aggregate-only evidence from the same explicitly user-authorized private
recording used by the local STT comparison. No audio, transcript text, speaker
name, timestamp sequence, or disagreement span is included. Temporary model,
source, result, and build artifacts were removed after these aggregates were
recorded.

## Question and decision

True diarization is feasible locally, but Whisper/tinydiarize alone is not
sufficient: it marks likely speaker turns and does not globally cluster stable
anonymous speakers. Three actual pipelines were tested on the same 1,183.68 s
two-speaker recording:

| Pipeline | Compute | Inference | End to end | Peak RSS | Speakers | Segments | Switches | Verdict |
|---|---|---:|---:|---:|---:|---:|---:|---|
| sherpa-onnx 1.13.7, Pyannote int8 + 3D-Speaker | CPU | 90.66 s | 91.92 s warm | 497,025,024 B | 2 | 184 | 57 | Correct count but unnecessarily slow; crates.io prebuilt Core ML request falls back to CPU |
| FluidAudio offline Sortformer v2.1 fp16 | Core ML `.all` | 3.11 s | 11.77 s cold | 558,088,192 B | 3 | 261 | 135 | Very fast but over-splits this two-speaker call; rejected as Release 1 default |
| FluidAudio offline Pyannote + WeSpeaker + PLDA/VBx | Core ML `.all` | 4.11 s | 4.3 s warm | 482,607,104 B | 2 | 149 | 36 | Selected default-on full-local diarization candidate |

The selected FluidAudio pipeline also returned two speakers when speaker count
was automatic. Supplying the known count of two produced 147 segments and the
same 36 switches. Against the existing Miaoji anonymous speaker labels, the
automatic result agreed on 94.44% of the 126 reference text segments that had a
non-zero overlap with a local speech interval. The reference contains 192 text
segments and 46 adjacent speaker switches. This overlap comparison is a useful
same-file signal, not diarization error rate (DER): Miaoji is not ground truth,
the two systems define speech intervals differently, and 66 reference segments
had no overlap to score.

## Selected boundary

- Keep Rust `whisper-rs`/whisper.cpp/Metal as the transcription worker.
- Add diarization as a separately installed, separately labeled capability pack
  backed by FluidAudio's offline Pyannote + WeSpeaker + PLDA/VBx Core ML path.
- Run that path in a second signed, one-shot Swift worker on macOS 14+ only.
  It receives the same verified 16 kHz mono samples through a bounded App-owned
  request, returns anonymous time intervals only, has no listener/network/
  credentials/queue/persistence, and exits.
- The Rust job owner verifies model/audio identities and response bounds, then
  assigns `local_speaker_01`, `local_speaker_02`, ... to Whisper segments by
  maximum temporal overlap. It never infers a person's name.
- Full local mode installs and enables this pack by default. If it is absent,
  disabled, unsupported, fails validation, or returns low-confidence/
  inconsistent coverage, the App requires an explicit choice to keep a
  transcript-only `local_unknown` result; it never calls that result full-local
  parity and never fabricates speaker labels.
- A confirmed import speaker count may constrain clustering. Automatic count is
  used for app-owned capture and passed this one two-speaker sample.

This is a narrow native adapter within the Rust/Tauri architecture, not a switch
to a Swift processing layer. FluidAudio source is Apache-2.0 and is pinned for
the spike at commit `5c19d5e12320e22bbfb7a1877b089d2665a69add`; the tested
Core ML model repository is CC-BY-4.0 at revision
`1ed7a662fdc7109e36d822db793ee6eebdaf8594`. The four compiled Core ML bundles
plus PLDA parameters total 21,599,417 bytes. Release integration must retain
the required attribution, pin every file SHA-256, avoid FluidAudio's automatic
downloader, and load only the App model manager's verified local directory.

## EchoWall-owned offline product proof — 2026-09-03

- The worker now compiles only an Apache-2.0 source subset of FluidAudio commit
  `5c19d5e12320e22bbfb7a1877b089d2665a69add`: offline Pyannote/WeSpeaker/
  PLDA/VBx inference, direct local Core ML loading, disk-backed audio, and the
  fastcluster bridge. FluidAudio's model registry, downloader, CLI, ASR, VAD,
  and TTS sources are not target dependencies. Upstream and retained
  fastcluster/VBx licenses are source-tracked beside the worker.
- The current arm64 release worker is 969,608 bytes after removal of the
  unused arbitrary embedding-export surface. `otool -L` contains neither
  CFNetwork nor Network.framework; `nm -u` contains no URLSession, NSURLSession,
  CFNetwork, or Network-framework symbol; `strings` contains no URL, Hugging
  Face, ModelHub, or model-download marker. The sidecar build script repeats
  these scans and fails packaging on a regression.
- A Tauri debug `.app` bundles the 969,608-byte diarization worker and the
  3,309,840-byte Rust/Metal Whisper worker beside the main executable, includes
  the Apache-2.0, fastcluster, and VBx license files under `Resources/licenses`,
  and passes strict deep verification after an ad-hoc nested-code signing pass.
  A later current-source universal App and DMG passed Developer ID signing,
  independent notarization/stapling, worker privacy/network scans, and
  Gatekeeper; see `macos-distribution-bundle-2026-09-03.md`.
- Disk-backed 16 kHz conversion now creates a mode-0600 `mkstemps` file, maps
  it, and immediately unlinks it before inference. The mapping remains usable,
  while normal exit, native abort, or forced termination cannot leave a named
  raw-PCM file behind. A real AVFAudio conversion test verifies the sample count
  and that the temporary-file set is unchanged after the factory returns.
- A 26.075-second, two-voice, locally synthesized English fixture completed
  inside `sandbox-exec` with `(deny network*)`. End-to-end time was 0.97 seconds,
  peak RSS was 251,805,696 bytes, and the closed Rust validator reported 21
  model files, two speakers, and four anonymous intervals. Interval confidence
  minimum/median/maximum were all 1,000/1,000; only intervals at or above the
  protocol's 500/1,000 confidence floor participate in Rust assignment. No
  source text or interval timestamp was logged. An invalid request exited 1
  with zero stdout and only the closed `invalid_request` error code.
- The final run observed zero named `echowall-diarization-*.raw` files both
  before and after inference, in addition to the focused unlink unit test.

## Two-hour offline stability — 2026-09-03

The same deterministic 7,200-second, two-system-voice fixture used by the
Whisper stability run completed under `(deny network*)` on the M4 Max host:

- 51.11 seconds end to end (140.9x real time), 1,383,776,256-byte peak RSS,
  zero swap, and no recorded macOS thermal/performance warning;
- two speakers and 1,105 anonymous intervals from 21 exact model files; the
  bounded response was 87,160 bytes;
- interval confidence minimum/median/maximum were 1,000/1,000/1,000, with zero
  intervals below the 500/1,000 Rust merge floor;
- Rust assigned all 1,173 Whisper segments, so structural label coverage was
  100%; the repeated synthetic fixture does not establish DER or speaker-turn
  quality; and
- a forced signal-9 termination after five seconds emitted zero stdout, left
  the source audio hash unchanged, and left zero named private PCM temp files.
- The Rust protocol now bounds and identity-binds the diarization request and
  response, merges only unambiguous temporal overlaps, durably selects default-
  on full local processing, and refuses to checkpoint a nominal full-local
  result when fewer than 95% of transcript segments have speaker labels. The
  explicit model manager pins all 22 pack files by size and SHA-256 and the UI
  exposes install, progress, cancel, remove, and pre-upload “完全离线转写”.
- The default-ignored live catalog test installed all 595,640,612 bytes through
  the actual Rust manager in 26.29 seconds, followed only allowlisted redirects,
  verified every file, produced the 1+21-file proof, removed the exact pack,
  and confirmed both model directories were gone. It used public artifacts and
  no token; normal test runs never download models.

## Remaining proof

- Run fabricated Mandarin/English, overlapping-speech, one-/two-/four-speaker,
  unknown-speaker-count, and two-hour tests with actual RTTM ground truth.
- Confirm cold Core ML compilation/cache behavior, ANE/GPU placement, and
  thermals on the physical capture matrix.
