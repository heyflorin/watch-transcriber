# Goal: Finish EchoWall Release 1 to practical Feishu-comparable usability

Execution status: **v0.3.0 engineering and distribution delivered**.
AX's September8 instruction authorized commit/push, versioned release and
TestFlight, excluding physical phone tests. Those release actions are complete:
read `docs/capture/evidence/release-0.3.0-2026-09-08.md` first when resuming.
The remaining non-phone live acceptance work requires a proven silent Mac
audio route and a process with microphone/system-recording permissions.
AX authorized Mac testing and temporary monitoring mute; the September 9
attempt stopped at permission preflight before recording. Audio settings were
restored to the pre-test backup and AX confirmed sound returned. No new live
capture pass is claimed. No production change, model study or artifact rebuild
is needed merely to resume. Keep the earlier no-audible-tone/no-lock restrictions.

## Intent and acceptance

AX's confirmed standard on 2026-09-06 is **“表现能和飞书差不多就可以接受了”**.
This is an execution/closeout goal. Finish the existing implementation into a
usable, inspectable App artifact and honest platform readiness report.
Do not continue open-ended model, diarization or window-policy research.

`docs/capture/PLAN.md` owns the product/architecture/platform specification.
Its “Acceptance and closeout” and Phase 1A sections own current quality
acceptance. Historical evidence and old evaluator documents do not add work or
reinstate superseded gates. Follow system/developer instructions, applicable
AGENTS.md and the user's current authorization throughout.

Practical quality means representative English, Mandarin and mixed-language
meeting notes preserve the main content, useful anonymous speaker attribution
and a usable summary at roughly Feishu/Miaoji's level. Wording, punctuation,
segmentation, isolated terms and occasional speaker-count differences can vary.
Do not require every metric/stratum to pass, exact matching, 95%/99% thresholds,
a fresh all-44 run, or blinded/two-reviewer quality approval.

Substantial repeated omissions, unusable speakers/summaries, crashes, hangs,
corrupted/lost audio, duplicate charges or privacy violations remain real
blockers. An isolated difficult fixture with a clear error, intact source and
recovery/export can be documented as a limitation. Never turn failed processing
into a fabricated success or alter historical scores to satisfy acceptance.

## Grounded context

- Repository: the current `watch-transcriber` checkout.
  It is public; personal `data/`, `.env`, `state/` and nested private history
  must stay private and untouched.
- Linear: AX-237 is the umbrella; AX-256 owns current local-processing work.
  Existing capture, import, UX and release issues remain the work breakdown.
- Current implementation to finish: MOSS Q8 / pinned transcribe.cpp Metal,
  existing quiet12 windows, explicit chronological timing v3, versioned
  SpeakerKit tail-context v2 where used, and single-window native / multiwindow
  graph mapping. Pinned Qwen3.8-27B UD-Q4_K_XL provides local summary.
- Preparation/plan policies and model/audio hashes are explicit and immutable.
  Old plans/requests retain their original behavior after upgrade. Keep raw
  model provenance and the canonical ordering/conservation fixes.
- Current evidence: 44 native diarization cases and 42 retained-ASR actual
  App-finalizer replays support practical Miaoji-comparable quality. The latest
  bundled-worker source-engine run completed 184 segments, 7 summary fields,
  exact-hash local archive and idempotent resume under OS network denial.
  Recorded App suite: 337 passed / 31 opt-in; focused protocol/worker and
  packaging evidence is retained. Reuse it while the relevant code is unchanged.
- Source/runtime proof does not prove final installed-App UI or release state.
  Verify those during closeout; do not restart model selection instead.
- Fine-window production work was stopped before edits. The fixed120+60-second
  probe is retained feasibility evidence only, not a delivery requirement.
  No new fine-window profile, threshold sweep, model port or precision study is
  scheduled.

Read evidence progressively, only for the next concrete delivery question:

- `docs/capture/evidence/moss-uniform-diagnosis-2026-09-06.json`
- `docs/capture/evidence/speakerkit-tail-context-2026-09-06.json`
- Relevant capture/provider/signing evidence linked by the plan's readiness table.

## Historical local closeout on 2026-09-06 — superseded by the September8 release

Read `docs/capture/evidence/practical-closeout-2026-09-06.md` for current artifact
paths/hashes, setup, exact proof and held platform scope. The corresponding JSON
and `local-eval/closeout-20260906/` receipts preserve the detailed evidence.

- Current macOS universal DMG is Developer-ID-signed, hardened, notarized,
  stapled and Gatekeeper accepted. The actual DMG contains the verified App
  binaries and passes the privacy gate. Android's new universal release APK
  and iOS's new App/Share-signed App Store IPA/archive are also retained.
- MOSS is exposed on supported Macs with explicit installation. App-owned
  remote/Whisper/MOSS preference survives the random localhost origin and cold
  launches. Missing choices/storage/models hold recordings locally; unselected
  Queued jobs do not auto-upload at startup or from UI timers.
- Actual isolated-QA native UI import and full processing passed for Mandarin01
  and Mixed01 (202/100 segments,7 summary fields each). The retained English
  generation opened. Original export and all3 source/archive hashes match;
  same-bundle cold reopen preserves all3 ledgers, notes, manifest and preference.
  No audio playback, production-data access or QA HTTP attempt occurred.
- Rust337 tests, strict clippy/formatting, packaging/QA-path9 tests,
  Python44+16 subtests, ruff and both fabricated UI suites pass. The Unix libc
  dependency correction is verified by successful Android and iOS builds.
- Bilingual user docs are updated. No test App or model worker remains active.

Do not redo completed UI/model/package work merely to resume. Only the plan's
named physical capture/import/lifecycle and authorized distribution scope
remains. At that historical closeout, production App installation/launch and
public/TestFlight/store upload had not been performed; isolated QA was not
that proof. The September 8 release subsequently completed Mac installation,
public Mac/Android downloads and internal TestFlight distribution, as recorded
above; production App launch remains a separate claim. The iOS App Store IPA is not a
directly sideloadable development IPA. No physical Pixel is available, and
OPPO lock/task-removal remains prohibited. Report these precise held claims;
do not label all Release1 platforms complete or restart quality research.

Keep old failures identifiable. The original all-44 ASR attempt was incomplete;
one long-window failure was subsequently repaired and an isolated tail case
remains a known edge. Do not describe a component/replay result as all-44 current
App success. These facts do not mandate another research campaign.

## Done state

1. The final App exposes a coherent local model choice and completes ordinary
   import/record selection → transcription/speakers → summary → archive/result
   opening and reopen. Finish MOSS's actual UI/native wiring where needed;
   retain existing Whisper fallback and explicit remote/local choice.
2. Practical quality above is met using existing evidence plus proportionate
   assembled-App checks. Minor isolated metric deficits are documented rather
   than pursued indefinitely.
3. A coherent local installable artifact is produced from current source and
   current sidecars, with exact path/hash, setup instructions and applicable
   signing/privacy/architecture checks. Bare worker binaries are not the App
   deliverable. Public upload remains a separately authorized action.
4. Shared Rust ownership, policy-compatible retry/restart, source integrity,
   explicit failures/recovery and one winning archive generation remain intact.
   Full local completion needs no TOS/Miaoji/Gemini/GitHub/R2 call or hosted
   processing service. Optional later sync does not redefine completion.
5. macOS/iOS/Android functionality and readiness match the plan. Preserve required
   capture consent, honest source scope, native import and supported lifecycle
   behavior. Do not call a platform/recorder ready from mocks or simulator-only
   evidence. Windows remains Release 2.
6. User-facing changes are reflected in both READMEs and relevant setup/readiness
   docs. Known limits and any held capability/device/authorization step are
   explicit; Linear mirrors the actual delivered state.

A complete macOS artifact is useful delivery evidence, not permission to call
all Release 1 platforms complete. If a required physical or external step
remains unavailable, finish independent work and report the exact partial/held
scope instead of replacing that work with more model research.

## Remaining work, in order

Apply this sequence only to unfinished items in the closeout status above.
Completed evidence stays valid until a relevant change or concrete defect.

1. Inspect current source/artifacts and the plan's delivery/readiness list.
   Revalidate any saved live PID/session before acting; do not restart a run
   because an old note says it was active or an observation timed out.
2. Finish the selected model's user path in the App: explicit install/status,
   import/record choice, truthful progress, retry/cancel/export, result opening,
   summary and reopen. Avoid technical model/policy details in ordinary UI.
3. Fix a reproducible ordinary-use blocker with the smallest coherent patch.
   Record its trigger, effect and focused verification. A red research score
   alone is not a blocker and does not authorize a new experiment.
4. Build the current coherent App using the canonical build scripts; check the
   actual resulting sidecars and artifact. Preserve earlier artifacts/evidence
   where their identity is referenced.
5. Run the necessary assembled-App smoke/recovery checks and relevant final
   project contracts, reusing passing evidence. Finish the remaining authorized
   platform work; hold only the genuinely blocked capability.
6. Update bilingual user docs, artifact/readiness notes, this plan state and
   Linear. Deliver the concrete result and any remaining external ask.
   Do not invent another acceptance phase after this result passes.

## Proof

- **User path:** A representative available English/Mandarin/mixed sample set
  runs through the assembled App and produces readable text, useful speakers,
  a usable summary and a reopenable archive. Reuse source/corpus evidence for
  quality; the final check catches integration defects. Use already retained
  recordings/fixtures, not another dataset collection or all-44 campaign.
- **Integrity/recovery:** Source/audio hash matches, interrupted/retried work
  resumes from durable checkpoints, no duplicate archive/provider effect occurs,
  and failure leaves usable recovery/export. Apply existing ownership, path,
  bounded-input, policy and archive tests to changed code.
- **Privacy:** Full-local job execution makes no provider/archive request;
  native workers have no network/credential/downloader role. No real credentials,
  personal recordings, model weights or builder-home paths enter shipped
  artifacts. Do not weaken codec/timing/identity checks to improve a score.
- **Artifact:** Inspect the actual current bundle, model setup, sidecar protocols,
  architecture, applicable signing/privacy checks and result opening/reopen.
  Keep source-only, ad-hoc QA, signed/installed and published evidence distinct.
- **Platforms:** Use the plan's existing relevant native lifecycle/isolation
  evidence; repeat only when the affected implementation or an unresolved
  ordinary-use risk changed. Do not redo two-hour captures just to refresh a
  date. Never substitute a simulator for a required physical claim.
- **Code contracts:** Use the repository's documented commands as applicable:
  `cargo check && cargo test` in `desktop/src-tauri`, and relevant formatting /
  clippy / sidecar checks. For legacy watcher changes, use
  `venv/bin/python3 -m pytest tests/ -q`,
  `ruff check deliveries/ scripts/ tests/ transcribe.py`, and
  `venv/bin/python3 transcribe.py --doctor`.
  Run focused checks during fixes and the final relevant gate once after the
  diff stabilizes. Do not rerun unchanged suites as a completion ritual.

Keep evaluator/report semantics unchanged. Missing or failed measurements stay
missing/failed; they are diagnostics with their original scope, not mandatory
release scorecards. No additional blinded study or specialist review panel is
required solely for model quality.

## Scope and authority

- May read/change this repository's required source, tests, docs, native projects
  and build/release configuration; produce reversible local build artifacts and
  use ordinary development dependencies.
- May use available simulators/emulators, already available test devices, public
  corpus/fabricated/demo fixtures and existing development credentials only
  through their intended secure consumers within existing session authority.
- May update/claim existing AX-237 work and sync concise Git-owned docs/evidence
  through the shared Linear workflow. Git remains authoritative.
- Preserve all unrelated/user-owned edits, personal `data/`, `.env`, `state/`,
  private archive history, existing jobs, original audio, credentials and frozen
  measurements. New model policies never reinterpret saved requests silently.
- Use private credentials from `~/creds/<provider>/` only through intended tools
  and the App's explicit secure-store setup. Never print them, copy them into
  source/webviews/binaries or dump inherited environments. Diagnostics and
  worker/test subprocesses use explicit minimal environments; do not repeat the
  `xctest -help` environment-dump incident. Credential rotation is not authorized
  by this goal.
- Use public or fabricated inputs for tests. A named personal recording requires
  explicit authorization. Never play generated tones/speech through AX's normal
  outputs; capture needs a proven non-monitored source. Do not lock/sleep the
  Mac or lock, screen-off, task-remove, or alter keyguard on AX's OPPO under the
  current instruction. Never repeat/persist an unlock credential.
- Commit/push/merge, deployment, paid-resource/DNS creation, public release,
  App Store/TestFlight/browser-store upload, credential mint/rotation and
  destructive data changes require explicit applicable session authorization.
  This file does not add it. Honor authorization already supplied; prepare the
  concrete artifact and all independent work before asking at an affected gate.

## Non-goals and invalid shortcuts

- No new model/precision/diarizer/window research, Qwen MLX port, threshold sweep,
  benchmark expansion or chasing exact words/counts solely for scorecards.
- No hidden recording, watchOS, mobile system/call capture, exact browser-tab
  extension, video/live transcription, billing or multi-user expansion.
- No Python/CLI model runtime, local model server, hosted EchoWall broker or
  second queue. Rust remains the job owner; native adapters/workers stay narrow.
- No implicit model download, hidden cloud fallback, extra network call in full
  local mode, made-up speaker identity, altered evidence or concealed failure.
- No platform-ready claim without its relevant proof. Do not delete the legacy
  Voice Memos watcher or alter its ordering/manifest/user-field contracts.

## Priorities and decision rules

1. Preserve audio, consent, credentials, durable recovery and one publication.
2. Deliver the working App and honest platform readiness.
3. Meet practical Feishu-comparable quality; retain small known limitations.
4. Apply modest polish that helps actual use.

Resolve routine reversible implementation choices autonomously. If an ordinary
use action fails, establish the symptom and make a bounded correction; after
repeated failure change the hypothesis, not the acceptance target. Do not
replace a packaging/UI/device task with a new model investigation.

Reopen model or segmentation research only for a demonstrated substantial
ordinary-use blocker that a focused fix cannot address; state the blocker and
smallest discriminating check first. Missing human-review/metric perfection is
not such a blocker. Unavailable hardware or missing external authorization holds
only the affected step while independent delivery work continues.

## Control loop and resumption

- When AX explicitly starts/resumes execution, use `autonomous-grind` and the
  existing native goal controls. Editing this file alone does not start or
  resume a paused goal.
- Work unit: one concrete closeout item or reproducible user-path defect.
  Use the existing AX-256 / AX-237 binding when applicable; otherwise bind the
  concrete eligible issue with `linear-workflow bind AX-N --umbrella AX-237`.
- State lives in this contract, the canonical plan, source/artifacts/tests and
  Linear. Do not add a parallel research ledger or routinely reread every
  historical experiment.
- Verify actual live processes before waits/restarts. Preserve completed work;
  a phase boundary is not a reason to restart discovery or model selection.
- Finish when the done state and practical proof pass. Stop if AX pauses/stops,
  or report a genuine remaining hardware/authority/external blocker after all
  safe independent work is complete. Native complete/blocked status follows the
  runtime's actual semantics; never mark incomplete work complete to exit.

## Delivery

Return the installable artifact path/hash, concise setup/use instructions,
relevant validation, platform readiness and known/held limits, with the Linear
links. Keep the final report short and focused on what AX can use.

No new research phase, perfect-metric target or review ceremony follows a usable,
verified delivery. A held external release action remains an explicit ask on a
prepared artifact; it is neither an excuse for further research nor a claim
that the entire release has already happened.
