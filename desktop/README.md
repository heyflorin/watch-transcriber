# 回音壁 EchoWall (desktop + mobile App)

Rust + Tauri v2 application for capture, import, embedded processing, and the
private archive. An authenticated in-process loopback transport serves the
resolved archive directory (HTTP Range → audio seeking) plus the manager APIs
(`/api/speakers`, `/api/attachments`, `/api/delete`, `/api/speaker-colors`);
the webview renders validated archive data with the viewer template compiled
into the App. Synced HTML/JavaScript is never executed.

The desktop shell now also owns the first cross-platform ingest boundary:

- macOS/Windows native dialog and drag/drop import for `.m4a`, `.mp3`, and
  `.wav`;
- an app-data inbox with atomic copy, streaming hash dedupe, media probing,
  append-only recovery events, and one validated `RecordingEnvelope` per item;
- an embedded Rust processing engine that checkpoints every direct
  TOS/妙记/Gemini/archive effect and resumes after app restart without a broker;
- MOSS local processing on Apple Silicon/macOS 14+ with at least 32 GiB unified
  memory. **MOSS 本地** opens explicit installation/status for its MOSS,
  SpeakerKit and Qwen summary components; Whisper is a separate fallback, not
  an installation prerequisite. Imports expose separate MOSS, Whisper and cloud
  actions. The model workers and Rust produce a verified local archive without
  provider/archive network calls; optional cloud backup remains a separate
  explicit action. Shared-model removal asks for confirmation.
  New-recording preferences are saved in App-owned storage and survive cold
  launch. An unset or unreadable preference leaves recordings queued; missing
  local models never cause an automatic download or cloud fallback. Selecting
  MOSS persists the local job before model verification. Existing jobs keep
  their saved policies, and a prior page-local Whisper preference has an
  explicit save action rather than being converted to MOSS.
  The existing `ECHOWALL_MOSS_CANDIDATE_ENABLED` runtime/compiled kill switch
  and `localMossCandidate` IPC field remain compatible; supported Macs expose
  MOSS unless disabled. `ECHOWALL_LOCAL_STT_ENABLED=0` also holds the route.
  The practical quality target is useful Feishu/Miaoji-comparable meeting notes,
  supported by retained corpus and full local source-engine evidence. Isolated
  difficult tails may fail explicitly with original audio available for
  recovery/export. Source, ad-hoc QA, installed UI and signed release evidence
  remain distinct; see [current readiness](../docs/capture/PLAN.md).
  Current artifact paths, hashes and installation limits are in the
  [delivery notes](../docs/capture/evidence/practical-closeout-2026-09-06.md).
- the shared Voice Memo / Meeting / System Capture consent, segment, gap, and
  crash-recovery model. Native record buttons remain unavailable until their
  platform backend passes the physical-device matrix.

macOS selected-App Meeting audio has a narrow Core Audio process-tap bridge.
Its private aggregate is tap-only so HDMI/AirPlay/default-output changes cannot
remove the tap's clock; 8 → 300 → immediate 8-second isolation, pause/resume,
and selected-source-exit fixtures pass. `ECHOWALL_PROCESS_TAP_ENABLED` remains
explicit opt-in until the 30-minute/two-hour, browser, route, sleep, and working
microphone matrix passes. Voice Memo and ScreenCaptureKit System Capture are
unaffected.

The same crate builds the **iOS / Android** companion
(`#[cfg(mobile)]`): `src/sync.rs` pulls the private notes repo as a GitHub
tarball into the app sandbox, `src/r2.rs` streams audio from R2 (SigV4,
Range, 500MB LRU cache, offline pin), `src/secrets.rs` keeps the two
archive and user-supplied provider credentials in
Keychain/Keystore/Credential Manager, and the viewer runs in sync mode via the
`/index.html?m=1` landing URL. Recording/import entrypoints are added only as
their native adapters become real; mobile never claims system/call capture.
Token setup guide + install paths:
repo README `## Mobile`. Agent landmines: repo `CLAUDE.md` `## Mobile`.

```bash
npm install
npm run tauri:dev:macos      # run against ../data with local-model sidecars
npm run tauri:build:macos    # local bundle; release signing uses CI credentials
```

`rust-toolchain.toml` pins desktop and worker builds to Rust 1.96.0, the
compiler used to verify the shipping artifacts. All four release CI lanes use
the same version, including Clippy and rustfmt. Run Cargo commands from
`desktop/` or a crate directory so rustup selects this toolchain. Upgrade the
pin and CI together, then rerun the release checks; a moving stable compiler
must not silently change the release lint or build requirements.

The macOS build command is the canonical privacy-preserving wrapper: it remaps
Rust and native C/C++ builder paths, strips release symbols, and builds explicit
arm64+x86_64 universal workers when requested. Release CI runs
`scripts/verify_macos_bundle_privacy.sh` on the signed/notarized `.app`, then
`scripts/notarize_macos_dmg.sh` on the exact final disk image before upload.
A local build without the release-signing environment is intentionally not
release evidence.

The macOS-only `--features isolated-qa --config src-tauri/tauri.qa.conf.json`
flavor is for public-fixture runtime tests, never distribution. It uses
`ai.ax.watch-transcriber.qa.moss` App-data and separate Keychain services,
ignores repository archive discovery/`WATCH_TRANSCRIBER_DATA`, and rejects App
HTTP before connection while keeping the production workers' OS network deny.
This HTTP guard is not a whole-App OS network trace. The release verifier
rejects QA binaries even if signed. Writable roots reject symlink redirection;
only existing by-topic leaf aliases to files inside that QA archive are allowed.

`node scripts/run_moss_qa.mjs --prepare` requires the already available,
hash-pinned public `english_01` fixture and31 public model files; it does not
download models or seed credentials. `--launch`, `--launch-public` and
`--launch-public-retry` accept only a retained, signed QA bundle beneath
`local-eval/`. Public import/retry calls the real Rust use cases, not a fake
processor or UI click. `run_moss_qa_crash.mjs` explicitly kills only the newly
launched QA App after observing its live model worker; do not use it as an
ordinary launcher. The recovery verifier checks the original ledger/archive
without rewriting them. See the
[bounded crash/reopen evidence](../docs/capture/evidence/moss-qa-app-recovery-2026-09-05.json).

App recording, import, first-run archive sync, processing, archive editing, and deletion do not use Python,
Git/wrangler CLIs, FastAPI, a localhost/LAN processing endpoint, or an EchoWall
processing service. Provider values enter only through a write-only setup
command, remain in the platform secure store, and never ship in a binary or
return to the webview. Windows CI builds unsigned MSI and NSIS artifacts only.
Authenticode signing and release attachment remain explicit release gates.

The loopback listener is an asset/media transport, not a processing service.
Every route is scoped by a per-process capability path; mutations additionally
require the exact Origin. The compiled viewer and markdown runtime are served
with a restrictive CSP, while Tauri grants the `main` window only the explicit
commands it needs.

The development-only `local-quality-eval` Rust binary scores canonical private
ground truth, Miaoji, and full-local outputs without network access or content
output. Its frozen corpus/metric/non-inferiority contract is
`../docs/evals/full-local-quality.md`; it is not a Tauri dependency or bundled
resource.

Prebuilt universal dmg on [GitHub Releases](https://github.com/xingfanxia/watch-transcriber/releases)
(`v*` tags auto-build via `.github/workflows/release.yml`; Developer ID
signed + notarized).

`WATCH_TRANSCRIBER_DATA` overrides the archive location; otherwise the app
walks up from its executable to find an enclosing source checkout's `data/`.
A standalone install with no checkout uses its platform app-data directory and
the in-App GitHub/R2 setup + Rust sync path; it does not require a Python
restore command.

### Native speaker worker tests

Run `swift test --package-path diarization-worker --configuration release --disable-swift-testing`
from `desktop/`. All native tests use XCTest, including private PCM cleanup.
The separate Swift Testing runner is disabled because the preset tests import
an executable target: launching that second runner enters the worker command
parser. This selects the correct test runner without skipping the tests.
