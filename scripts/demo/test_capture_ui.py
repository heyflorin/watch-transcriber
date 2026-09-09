#!/usr/bin/env python3
"""Exercise the fabricated EchoWall import UI in a real browser.

The Tauri bridge is mocked before page load; no recording, provider, archive,
or personal files are accessed.  Use with the webapp-testing server helper.
"""

from __future__ import annotations

import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread

from playwright.sync_api import sync_playwright


TAURI_STUB = r"""
window.__TAURI__ = {
  core: {
    invoke: async (command, args) => {
      if (command === "get_processing_preference") return {newRecordingRoute:window.__newRecordingRoute || null};
      if (command === "set_processing_preference") {
        window.__newRecordingRoute = args.request.newRecordingRoute;
        return {newRecordingRoute:window.__newRecordingRoute};
      }
      if (command === "plugin:dialog|open") {
        window.__nativePickerOptions = args.options;
        if (window.__nativePickerFailure) throw new Error("fixture picker unavailable");
        return ["/fixtures/interview.m4a", "/fixtures/interview-copy.WAV"];
      }
      if (command === "runtime_features") {
        return window.__runtimeFeatures || {
          recording: true,
          audioImport: true,
          directProcessing: true,
          browserCapture: true,
          localStt: true,
          localQwenCandidate: true,
        };
      }
      if (command === "local_model_pack_status") {
        return {
          supported: true,
          enabled: true,
          packId: "full-local-v1",
          displayName: "EchoWall Full Local v1",
          whisperModelId: "large-v3-turbo-q5_0",
          diarizationDefault: true,
          summaryModelId: "qwen3.8-27b-ud-q4-k-xl",
          minimumMacosMajor: 14,
          minimumMemoryBytes: 34359738368,
          detectedMemoryBytes: 137438953472,
          memorySufficient: true,
          totalBytes: 18154818756,
          downloadedBytes: 18154818756,
          installed: true,
          installing: false,
          licenses: ["MIT", "CC-BY-4.0"],
          errorCode: null,
        };
      }
      if (command === "install_local_model_pack") {
        window.__localModelInstallRequest = args.request;
        return await window.__TAURI__.core.invoke("local_model_pack_status");
      }
      if (command === "qwen_candidate_model_pack_status") {
        return {
          supported: true,
          enabled: true,
          packId: "qwen-asr-candidate-v1",
          displayName: "Qwen3-ASR Candidate",
          runtimeId: "qwen-asr-rust-0.11.0",
          asrModelId: "qwen3-asr-1.7b",
          alignerModelId: "qwen3-forced-aligner-0.6b",
          minimumMacosMajor: 14,
          minimumMemoryBytes: 34359738368,
          detectedMemoryBytes: 137438953472,
          memorySufficient: true,
          totalBytes: 6539619722,
          downloadedBytes: 6539619722,
          installed: true,
          installing: false,
          licenses: ["Apache-2.0 · Official Qwen models"],
          errorCode: null,
        };
      }
      if (command === "install_qwen_candidate_model_pack") {
        window.__qwenCandidateInstallRequest = args.request;
        return await window.__TAURI__.core.invoke("qwen_candidate_model_pack_status");
      }
      if (command === "remove_qwen_candidate_model_pack") {
        window.__qwenCandidateRemoveRequest = args.request;
        return {
          ...(await window.__TAURI__.core.invoke("qwen_candidate_model_pack_status")),
          installed: false,
          downloadedBytes: 0,
        };
      }
      if (command === "cancel_local_model_install") {
        window.__localModelCancelRequest = args.request;
        return null;
      }
      if (command === "remove_local_model_pack") {
        window.__localModelRemoveRequest = args.request;
        return {
          ...(await window.__TAURI__.core.invoke("local_model_pack_status")),
          installed: false,
          downloadedBytes: 0,
        };
      }
      if (command === "process_recording_with_local_models") {
        window.__localProcessingRequest = args.request;
        return {
          recordingId: args.request.recordingId,
          status: {
            state: "local_transcribing",
            transcriptionBackend: "whisper_local",
            summaryBackend: "qwen_local",
            publicationBackend: "local_archive",
            diarizationSelected: true,
            transcriptOnlyAccepted: false,
            remoteTaskAccepted: false,
            remoteTaskSuperseded: false,
            transcriptAvailable: false,
          },
          errorCode: null,
          errorMessage: null,
        };
      }
      if (command === "process_recording_with_qwen_candidate") {
        window.__qwenCandidateProcessingRequest = args.request;
        return {
          recordingId: args.request.recordingId,
          status: {
            state: "local_transcribing",
            transcriptionBackend: "qwen_local",
            summaryBackend: "qwen_local",
            publicationBackend: "local_archive",
            diarizationSelected: true,
            transcriptOnlyAccepted: false,
            remoteTaskAccepted: false,
            remoteTaskSuperseded: false,
            transcriptAvailable: false,
          },
          errorCode: null,
          errorMessage: null,
        };
      }
      if (command === "back_up_local_recording_to_cloud") {
        window.__localCloudBackupRequest = args.request;
        return {
          recordingId: args.request.recordingId,
          status: {
            state: "complete",
            transcriptionBackend: "whisper_local",
            summaryBackend: "qwen_local",
            publicationBackend: "remote_archive",
            diarizationSelected: true,
            transcriptOnlyAccepted: false,
            remoteTaskAccepted: false,
            remoteTaskSuperseded: false,
            transcriptAvailable: true,
            summaryAvailable: true,
            temporaryCleanupPending: false,
          },
          errorCode: null,
          errorMessage: null,
        };
      }
      if (command === "take_over_processing_with_local_models") {
        window.__localTakeoverRequest = args.request;
        return {
          recordingId: args.request.recordingId,
          status: {
            state: "local_transcribing",
            transcriptionBackend: "whisper_local",
            diarizationSelected: true,
            transcriptOnlyAccepted: false,
            remoteTaskAccepted: true,
            remoteTaskSuperseded: true,
            transcriptAvailable: false,
          },
          errorCode: null,
          errorMessage: null,
        };
      }
      if (command === "take_over_processing_with_qwen_candidate") {
        window.__qwenCandidateTakeoverRequest = args.request;
        return {
          recordingId: args.request.recordingId,
          status: {
            state: "local_transcribing",
            transcriptionBackend: "qwen_local",
            diarizationSelected: true,
            transcriptOnlyAccepted: false,
            remoteTaskAccepted: true,
            remoteTaskSuperseded: true,
            transcriptAvailable: false,
          },
          errorCode: null,
          errorMessage: null,
        };
      }
      if (command === "accept_local_transcript_only") {
        window.__acceptedTranscriptOnly = args.request;
        return {
          recordingId: args.request.recordingId,
          status: {
            state: "local_transcribing",
            transcriptionBackend: "whisper_local",
            diarizationSelected: false,
            transcriptOnlyAccepted: true,
            remoteTaskAccepted: false,
            remoteTaskSuperseded: false,
            transcriptAvailable: false,
          },
          errorCode: null,
          errorMessage: null,
        };
      }
      if (command === "import_audio_files") {
        return {
          results: args.request.paths.map((path, index) => ({
            sourcePath: path,
            importedName: path.split("/").pop(),
            status: index === 0 ? "imported" : "duplicate",
            recordingId: index === 0
              ? (path.includes("dropped")
                ? "218f92d8-6ad4-7dc1-8e28-8b020d2942cb"
                : "118f92d8-6ad4-7dc1-8e28-8b020d2942cb")
              : "018f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            sha256: "a".repeat(64),
            sizeBytes: 4096,
            codec: "aac",
            durationMs: 125000,
            sampleRate: 48000,
            channels: 1,
            proposedCapturedAt: "2026-09-02T09:00:00-07:00",
            error: null,
          })),
        };
      }
      if (command === "confirm_import_review") {
        window.__confirmedImportReview = args.request;
        return { ...args.request, confirmedAt: "2026-09-02T16:01:00Z" };
      }
      if (command === "adopt_mobile_imports") {
        return { results: args.request.items.map(item => ({
          sourcePath: item.sourcePath,
          importedName: item.displayName || "shared-voice.m4a",
          status: "imported",
          recordingId: "418f92d8-6ad4-7dc1-8e28-8b020d2942cb",
          sha256: "c".repeat(64),
          sizeBytes: item.sizeBytes,
          codec: "aac",
          durationMs: 12000,
          sampleRate: 48000,
          channels: 1,
          proposedCapturedAt: "2026-09-02T09:00:00-07:00",
          error: null,
        })) };
      }
      if (command === "processing_credentials_status") {
        return { configured: !!window.__credentialsConfigured, archiveConfigured: !!window.__credentialsConfigured };
      }
      if (command === "save_sync_credentials") {
        window.__savedArchiveCredentials = args.setup;
        return null;
      }
      if (command === "save_processing_credentials") {
        window.__savedProcessingCredentials = args.credentials;
        window.__credentialsConfigured = true;
        return null;
      }
      if (command === "delete_processing_credentials") {
        window.__processingCredentialsDeleted = true;
        window.__credentialsConfigured = false;
        return null;
      }
      if (command === "delete_sync_credentials") {
        window.__archiveCredentialsDeleted = true;
        return null;
      }
      if (command === "process_recordings") {
        return {
          results: args.request.recordingIds.map(recordingId => ({
            recordingId,
            status: { state: "queued" },
            errorCode: null,
            errorMessage: null,
          })),
        };
      }
      if (command === "list_processing_recordings") {
        if (window.__emptyProcessingQueue) return { results: [] };
        return { results: [
          {
            recordingId: "318f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            status: {
              state: "publish_failed",
              revision: 9,
              transcriptionBackend: "miaoji_remote",
              diarizationSelected: false,
              transcriptOnlyAccepted: false,
              remoteTaskAccepted: true,
              remoteTaskSuperseded: false,
              transcriptAvailable: true,
              summaryAvailable: true,
              temporaryCleanupPending: false,
            },
            errorCode: null,
            errorMessage: null,
          },
          {
            recordingId: "c18f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            status: {
              state: "provider_failed",
              revision: 4,
              transcriptionBackend: "whisper_local",
              diarizationSelected: true,
              transcriptOnlyAccepted: false,
              remoteTaskAccepted: false,
              remoteTaskSuperseded: false,
              transcriptAvailable: false,
              summaryAvailable: false,
              temporaryCleanupPending: false,
            },
            errorCode: "processing_unavailable",
            errorMessage: "processing is temporarily unavailable",
          },
          {
            recordingId: "d18f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            status: {
              state: "polling",
              revision: 6,
              transcriptionBackend: "miaoji_remote",
              diarizationSelected: false,
              transcriptOnlyAccepted: false,
              remoteTaskAccepted: true,
              remoteTaskSuperseded: false,
              transcriptAvailable: false,
              summaryAvailable: false,
              temporaryCleanupPending: false,
            },
            errorCode: "processing_unavailable",
            errorMessage: "processing is temporarily unavailable",
          },
        ] };
      }
      if (command === "retry_processing_recording") {
        return { recordingId: args.request.recordingId, status: { state: "queued" }, errorCode: null, errorMessage: null };
      }
      if (command === "reprocess_recording") {
        window.__reprocessedRecordingId = args.request.recordingId;
        return { recordingId: args.request.recordingId, status: { state: "summarizing" }, errorCode: null, errorMessage: null };
      }
      if (command === "discard_processing_recording") {
        window.__discardedRecordingId = args.request.recordingId;
        return { recordingId: args.request.recordingId, status: { state: "discarded", temporaryCleanupPending: false }, errorCode: null, errorMessage: null };
      }
      if (command === "cancel_processing_recording") {
        return { recordingId: args.request.recordingId, status: { state: "canceled_before_upload" }, errorCode: null, errorMessage: null };
      }
      if (command === "export_recording_original") {
        window.__exportedRecordingId = args.request.recordingId;
        return { exported: true };
      }
      if (command === "capture_capabilities") {
        return window.__captureCapabilities || { backendAvailable: true, meetingAvailable: true, platform: "macos", modes: ["voice_memo", "meeting", "system_capture"] };
      }
      if (command === "capture_permissions") {
        return window.__capturePermissions || {
          microphone: "granted",
          screenAndSystemAudio: "granted",
        };
      }
      if (command === "capture_request_permissions") {
        window.__capturePermissionRequested = args.request;
        window.__capturePermissions = {
          microphone: "granted",
          screenAndSystemAudio: "granted",
        };
        return window.__capturePermissions;
      }
      if (command === "open_capture_permission_settings") {
        window.__openedCaptureSettings = args.request.kind;
        return null;
      }
      if (command === "capture_sources") {
        const permissions = window.__capturePermissions || {
          microphone: "granted", screenAndSystemAudio: "granted"
        };
        const sources = [
          { id: "mic-1", label: "Studio Mic", kind: "microphone", available: true, isDefault: true },
          { id: "app-1", label: "Zoom", kind: "native_application", available: true, isDefault: false },
          { id: "browser-1", label: "Google Chrome", kind: "browser_application", available: true, isDefault: false },
          { id: "system-1", label: "All system audio", kind: "system_output", available: true, isDefault: false },
        ];
        return { sources: permissions.screenAndSystemAudio === "granted"
          ? sources
          : sources.filter(source => !["native_application", "browser_application"].includes(source.kind)) };
      }
      if (command === "capture_source_icon") {
        window.__captureIconRequest = args.request.sourceId;
        const canvas = document.createElement("canvas");
        canvas.width = 32;
        canvas.height = 32;
        const context = canvas.getContext("2d");
        context.fillStyle = "#d7873d";
        context.fillRect(0, 0, 32, 32);
        context.fillStyle = "#fffaf2";
        context.font = "600 16px sans-serif";
        context.textAlign = "center";
        context.textBaseline = "middle";
        context.fillText("C", 16, 17);
        return { iconDataUrl: canvas.toDataURL("image/png") };
      }
      if (command === "capture_preflight") {
        return {
          backend_available: true,
          microphone_permission: "granted",
          system_audio_permission: "granted",
          selected_source: "available",
          warnings: [],
        };
      }
      if (command === "start_capture") {
        window.__desktopCaptureState = "recording";
        return {
          recordingId: "118f92d8-6ad4-7dc1-8e28-8b020d2942cb",
          phase: "recording",
          durationMs: 0,
          microphoneLevel: 0.25,
          systemLevel: 0.5,
        };
      }
      if (command === "capture_status") {
        if (window.__desktopRecovery) {
          return {
            recordingId: "a18f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            phase: "interrupted",
            mode: "meeting",
            durationMs: 12000,
            microphoneLabel: "Recovered Mic",
            sourceLabel: "Recovered Zoom",
            warnings: [{
              code: "crash_recovered",
              message: "Recovered only durably closed capture segments",
              atMs: 12000,
            }],
            warningCount: 1,
            gapDurationMs: 3000,
            microphoneLevel: null,
            systemLevel: null,
          };
        }
        return window.__desktopCaptureState
          ? {
            recordingId: "118f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            phase: window.__desktopCaptureState,
            durationMs: 1000,
            microphoneLevel: 0.25,
            systemLevel: 0.5,
            microphoneLabel: "Studio Mic",
            sourceLabel: "Google Chrome",
            warnings: [],
            warningCount: 0,
            gapDurationMs: 0,
          }
          : { recordingId: null, phase: null, durationMs: 0 };
      }
      if (command === "pause_capture" || command === "resume_capture") {
        window.__desktopCaptureState = command === "pause_capture" ? "paused" : "recording";
        return { recordingId: "118f92d8-6ad4-7dc1-8e28-8b020d2942cb", phase: window.__desktopCaptureState, durationMs: 1000 };
      }
      if (command === "stop_capture") {
        window.__desktopCaptureState = null;
        return {
          recordingId: "118f92d8-6ad4-7dc1-8e28-8b020d2942cb",
          envelope: {
            normalized_audio: "derived/mixed.wav",
            normalized_sha256: "b".repeat(64),
            duration_ms: 125000,
            tracks: [{ codec: "pcm_s16le" }],
          },
        };
      }
      if (command === "mobile_preflight") {
        return {
          supported: true,
          permission: "granted",
          mode: "voice_memo",
          storageReady: !window.__mobileStorageLow,
          storageAvailableBytes: window.__mobileStorageLow ? 128 * 1024 * 1024 : 4 * 1024 * 1024 * 1024,
        };
      }
      if (command === "mobile_start") {
        window.__mobileStartCalled = true;
        window.__nativeState = "recording";
        return { recordingId: args.recordingId, state: "recording", microphoneLevel: 0.2 };
      }
      if (command === "mobile_status") {
        return {
          recordingId: args.recordingId,
          state: window.__iosProcessRestarted ? "interrupted" : (window.__nativeState || "recording"),
          microphoneLevel: window.__iosProcessRestarted ? null : 0.2,
          warnings: window.__iosProcessRestarted ? ["process_restarted"] : [],
          imports: window.__androidShareReplayEnabled ? [{
            importId: "618f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            relativePath: "inbox/shared-voice.m4a",
            displayName: "android-shared-voice.m4a",
            sizeBytes: 4096,
            sha256: "d".repeat(64),
            state: "ready",
          }] : [],
        };
      }
      if (command === "mobile_pause") {
        window.__nativeState = "paused";
        return { recordingId: args.recordingId, state: "paused" };
      }
      if (command === "mobile_resume") {
        window.__nativeState = "recording";
        return { recordingId: args.recordingId, state: "recording" };
      }
      if (command === "mobile_stop") {
        window.__nativeState = "finalizing";
        return { recordingId: args.recordingId, state: "finalizing" };
      }
      if (command === "mobile_drain_shared_imports") {
        return { items: window.__shareReplayEnabled ? [{
          importId: "518f92d8-6ad4-7dc1-8e28-8b020d2942cb",
          path: "/fabricated/shared-voice.m4a",
          displayName: "Shared Voice Memo.m4a",
          sizeBytes: 4096,
        }] : [] };
      }
      if (command === "mobile_open_audio_picker") {
        return { items: [{
          importId: "718f92d8-6ad4-7dc1-8e28-8b020d2942cb",
          path: "/fabricated/picked-voice.m4a",
          displayName: "picked-voice.m4a",
          sizeBytes: 4096,
          state: "ready",
        }], rejected: 0 };
      }
      if (command === "mobile_acknowledge_shared_imports") {
        window.__acknowledgedSharedImports = args.importIds;
        return { removed: args.importIds.length * 2 };
      }
      if (command === "list_pending_mobile_recordings") {
        return { readyRecordingIds: [], nativeSessions: window.__iosPendingRecovery ? [{
          recordingId: "818f92d8-6ad4-7dc1-8e28-8b020d2942cb",
          state: "interrupted",
          platform: "ios",
        }] : [] };
      }
      if (command === "finalize_pending_mobile_capture") {
        window.__finalizedPendingMobileRecording = args.request.recordingId;
        return {
          recordingId: args.request.recordingId,
          durationMs: 125000,
          normalizedSha256: "e".repeat(64),
          status: "recovered",
        };
      }
      if (command === "finalize_mobile_capture") {
        return {
          recordingId: args.request.session.recordingId,
          durationMs: 125000,
          normalizedSha256: "a".repeat(64),
          status: "finalized",
        };
      }
      throw new Error("unexpected command");
    },
  },
  dialog: {
    open: async () => ["/fixtures/interview.m4a", "/fixtures/interview-copy.WAV"],
  },
  webviewWindow: {
    getCurrentWebviewWindow: () => ({
      onDragDropEvent: async callback => {
        window.__captureDragCallback = callback;
        return () => {};
      },
    }),
  },
};
"""


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("url", nargs="?")
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if bool(args.url) == bool(args.directory):
        parser.error("provide exactly one of url or --directory")
    args.output.mkdir(parents=True, exist_ok=True)

    server = None
    if args.directory:
        class QuietHandler(SimpleHTTPRequestHandler):
            def log_message(self, _format, *_args):
                pass

        server = ThreadingHTTPServer(
            ("127.0.0.1", 0),
            partial(QuietHandler, directory=str(args.directory.resolve())),
        )
        Thread(target=server.serve_forever, daemon=True).start()
        url = f"http://127.0.0.1:{server.server_port}/index.html"
    else:
        url = args.url

    console_errors: list[str] = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(
            headless=True,
            executable_path="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            args=["--mute-audio"],
        )
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        page.emulate_media(color_scheme="dark")
        page.add_init_script(TAURI_STUB)
        page.on("pageerror", lambda error: console_errors.append(str(error)))
        page.on(
            "console",
            lambda message: console_errors.append(message.text)
            if message.type == "error"
            else None,
        )
        page.goto(url)
        page.wait_for_load_state("networkidle")
        try:
            page.locator("#captureTools").wait_for(state="visible")
        except Exception as error:
            page.screenshot(path=args.output / "startup-failure.png", full_page=True)
            raise AssertionError(f"capture UI startup failed: {console_errors}") from error
        assert page.evaluate("processingStateLabel('local_transcribing')") == "本机转写中"
        page.locator("#localModelBtn").wait_for(state="visible")
        page.locator("#localModelBtn").click()
        page.locator("#localPack").wait_for(state="visible")
        assert page.locator("#localPackState").text_content() == (
            "已安装 · 说话人和本地摘要默认开启"
        )
        assert "完全离线运行" in page.locator("#localPackDescription").text_content()
        assert page.locator("#localPackRemove").is_visible()
        page.locator("#qwenCandidatePack").wait_for(state="visible")
        assert page.locator("#qwenCandidateState").text_content() == (
            "已安装 · 仍使用 Whisper 默认"
        )
        assert "不会替换 Whisper 默认" in page.locator(
            "#qwenCandidatePack"
        ).text_content()
        assert page.locator("#qwenCandidateRemove").is_visible()
        page.locator("#localPackDefault").check()
        page.wait_for_function("window.__newRecordingRoute === 'whisper_local'")
        page.locator("#processingCancel").click()
        page.locator("#processingDialog").wait_for(state="hidden")
        page.evaluate(
            "startQwenCandidateProcessing('e18f92d8-6ad4-7dc1-8e28-8b020d2942cb', null)"
        )
        qwen_candidate = page.locator(
            '.import-item[data-recording-id="e18f92d8-6ad4-7dc1-8e28-8b020d2942cb"]'
        )
        qwen_candidate.get_by_text("本机转写中", exact=True).wait_for()
        assert page.evaluate("window.__qwenCandidateProcessingRequest") == {
            "recordingId": "e18f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            "language": None,
        }
        transcript_only = page.locator(
            '.import-item[data-recording-id="c18f92d8-6ad4-7dc1-8e28-8b020d2942cb"]'
        )
        transcript_only.get_by_text("处理失败", exact=True).wait_for()
        page.once("dialog", lambda dialog: dialog.accept())
        transcript_only.get_by_role("button", name="跳过说话人分离并重试").click()
        transcript_only.get_by_text("本机转写中", exact=True).wait_for()
        assert page.evaluate("window.__acceptedTranscriptOnly") == {
            "recordingId": "c18f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        }
        takeover = page.locator(
            '.import-item[data-recording-id="d18f92d8-6ad4-7dc1-8e28-8b020d2942cb"]'
        )
        takeover.get_by_text("转写中", exact=True).wait_for()
        page.once("dialog", lambda dialog: dialog.accept())
        takeover.get_by_role("button", name="接管为完全离线模型").click()
        takeover.get_by_text("本机转写中", exact=True).wait_for()
        assert page.evaluate("window.__localTakeoverRequest") == {
            "recordingId": "d18f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            "language": None,
        }
        recovered = page.locator(
            '.import-item[data-recording-id="318f92d8-6ad4-7dc1-8e28-8b020d2942cb"]'
        )
        recovered.get_by_text("归档失败", exact=True).wait_for()
        assert recovered.get_by_role("button", name="重试").count() == 1
        assert recovered.get_by_role("button", name="导出原录音").count() == 1
        recovered.get_by_role("button", name="重试").click()
        recovered.get_by_text("等待转写", exact=True).wait_for()
        sanitized = page.evaluate(
            """() => {
              const host = document.createElement("div");
              host.append(safeMarkdownFragment(
                '<img src="https://attacker.invalid/pixel" onerror="window.pwned=1">'
                + '[bad](javascript:window.pwned=2) [good](https://example.com/path)'
              ));
              const links = [...host.querySelectorAll("a")].map(link => link.getAttribute("href"));
              return {
                images: host.querySelectorAll("img").length,
                eventAttributes: host.querySelectorAll("[onerror],[onclick]").length,
                links,
                pwned: !!window.pwned,
              };
            }"""
        )
        assert sanitized["images"] == 0
        assert sanitized["eventAttributes"] == 0
        assert sanitized["pwned"] is False
        assert all(link is None or not link.startswith("javascript:") for link in sanitized["links"])
        assert "https://example.com/path" in sanitized["links"]
        page.locator("#list .row").first.click()
        backup_button = page.get_by_role("button", name="备份到私有云")
        backup_button.wait_for(state="visible")
        page.evaluate("window.__savedSetTimeout = window.setTimeout; window.setTimeout = () => 0")
        page.once("dialog", lambda dialog: dialog.accept())
        backup_button.click()
        assert page.evaluate("window.__localCloudBackupRequest") == {
            "recordingId": "038f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        }
        page.evaluate("window.setTimeout = window.__savedSetTimeout")
        assert page.locator("#recordBtn").is_enabled()
        page.get_by_role("button", name="配置处理").click()
        page.get_by_label("TOS Access Key").fill("AKLTfabricated123")
        page.get_by_label("TOS Secret Key").fill("fabricated-tos-secret-123456")
        page.get_by_label("TOS Bucket").fill("echowall-private")
        page.get_by_label("妙记 API Key").fill("fabricated-volc-key-123456")
        page.get_by_label("Gemini API Key").fill("fabricated-gemini-key-123456")
        page.get_by_label("GitHub Archive PAT").fill("github_pat_fabricated-123456")
        page.get_by_label("GitHub Archive Repo").fill("owner/private-notes")
        page.get_by_label("R2 Account ID").fill("0123456789abcdef0123456789abcdef")
        page.get_by_label("R2 Access Key ID").fill("FABRICATEDACCESSKEY123456")
        page.get_by_label("R2 Secret Access Key").fill("fabricated-r2-secret-123456")
        page.get_by_label("R2 Archive Bucket").fill("private-audio")
        page.get_by_role("button", name="保存到系统安全存储").click()
        page.locator("#processingDialog").wait_for(state="hidden")
        saved = page.evaluate("window.__savedProcessingCredentials")
        archive_saved = page.evaluate("window.__savedArchiveCredentials")
        assert saved["tosBucket"] == "echowall-private"
        assert saved["geminiModel"] == "gemini-3.6-flash"
        assert archive_saved["r2AccountId"] == "0123456789abcdef0123456789abcdef"
        assert archive_saved["repo"] == "owner/private-notes"
        assert archive_saved["bucket"] == "private-audio"
        assert page.get_by_label("TOS Secret Key").input_value() == ""
        assert page.get_by_label("R2 Secret Access Key").input_value() == ""
        assert page.evaluate("Object.values(localStorage).join(' ')").find("fabricated-") == -1
        page.get_by_role("button", name="导入").click()
        page.get_by_text("interview.m4a", exact=True).wait_for()
        assert page.get_by_text("待确认", exact=True).count() == 1
        assert page.get_by_text("已存在", exact=True).count() == 1
        duplicate = page.locator(".import-item").filter(has_text="interview-copy.WAV")
        assert duplicate.get_by_role("button", name="打开已有录音").count() == 1
        page.once("dialog", lambda dialog: dialog.accept())
        duplicate.get_by_role("button", name="重新处理").click()
        assert page.evaluate("window.__reprocessedRecordingId") == (
            "018f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        )
        imported = page.locator(
            '.import-item[data-recording-id="118f92d8-6ad4-7dc1-8e28-8b020d2942cb"]'
        )
        imported.get_by_label("显示标题").fill("Synthetic interview")
        imported.get_by_label("说话人数").fill("2")
        imported.get_by_role("button", name="云端处理", exact=True).click()
        imported.get_by_text("等待转写", exact=True).wait_for()
        imported.get_by_role("button", name="导出原录音", exact=True).click()
        assert page.evaluate("window.__exportedRecordingId") == "118f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        confirmed = page.evaluate("window.__confirmedImportReview")
        assert confirmed["displayTitle"] == "Synthetic interview"
        assert confirmed["speakerCount"] == 2

        page.evaluate(
            """() => {
              document.querySelector('#importQueue').append(renderImportResult({
                sourcePath: '/fixtures/offline.m4a',
                importedName: 'offline.m4a',
                status: 'imported',
                recordingId: 'b18f92d8-6ad4-7dc1-8e28-8b020d2942cb',
                sha256: 'f'.repeat(64),
                sizeBytes: 8192,
                codec: 'aac',
                durationMs: 62000,
                sampleRate: 48000,
                channels: 1,
                proposedCapturedAt: '2026-09-02T10:00:00-07:00',
                error: null,
              }));
            }"""
        )
        offline = page.locator(
            '.import-item[data-recording-id="b18f92d8-6ad4-7dc1-8e28-8b020d2942cb"]'
        )
        offline.get_by_text("offline.m4a", exact=True).wait_for()
        offline.get_by_label("显示标题").fill("Offline interview")
        offline.get_by_label("说话人数").fill("2")
        offline.get_by_role("button", name="Whisper 离线处理").click()
        offline.get_by_text("本机转写中", exact=True).wait_for()
        local_request = page.evaluate("window.__localProcessingRequest")
        assert local_request == {
            "recordingId": "b18f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            "language": None,
        }
        local_confirmed = page.evaluate("window.__confirmedImportReview")
        assert local_confirmed["displayTitle"] == "Offline interview"
        assert local_confirmed["speakerCount"] == 2

        page.evaluate(
            """async () => {
              await window.__captureDragCallback({ payload: { type: "over", paths: [] } });
            }"""
        )
        assert page.locator("#dropMask").is_visible()
        page.evaluate(
            """async () => {
              await window.__captureDragCallback({
                payload: { type: "drop", paths: ["/fixtures/dropped.mp3"] },
              });
            }"""
        )
        page.get_by_text("dropped.mp3", exact=True).wait_for()
        dropped = page.locator(".import-item").filter(has_text="dropped.mp3")
        page.once("dialog", lambda dialog: dialog.accept())
        dropped.get_by_role("button", name="丢弃本机副本").click()
        assert page.evaluate("window.__discardedRecordingId") is not None
        assert dropped.count() == 0
        page.get_by_role("button", name="录音", exact=True).click()
        page.locator("#captureDialog").wait_for(state="visible")
        page.get_by_role("button", name="Meeting").click()
        page.locator("#captureSource").select_option(label="Google Chrome")
        preview = page.locator("#captureSourcePreview")
        assert preview.is_visible()
        assert page.locator("#captureSourcePreviewName").text_content() == "Google Chrome"
        assert "其他标签页可能被录入" in page.locator("#captureSourcePreviewMeta").text_content()
        assert "开始后检测" in page.locator("#captureSourcePreviewMeta").text_content()
        page.locator("#captureSourceIcon").wait_for(state="visible")
        assert page.locator("#captureSourceIcon").get_attribute("src").startswith(
            "data:image/png;base64,"
        )
        assert page.evaluate("window.__captureIconRequest") == "browser-1"
        page.screenshot(path=str(args.output / "desktop-source-picker-dark.png"))
        page.locator("#captureScopeCheck").check()
        page.get_by_role("button", name="开始录音").click()
        page.locator("#recordingSheet").wait_for(state="visible")
        assert "Studio Mic" in page.locator("#recordingSourceName").text_content()
        assert "Google Chrome" in page.locator("#recordingSourceName").text_content()
        assert int(page.get_by_role("meter", name="麦克风实时电平").get_attribute("aria-valuenow")) > 0
        assert int(page.get_by_role("meter", name="所选来源实时电平").get_attribute("aria-valuenow")) > 0
        page.screenshot(path=args.output / "desktop-recording-meters-dark.png", full_page=True)
        page.get_by_role("button", name="停止并保存").click()
        page.wait_for_timeout(2200)
        assert page.locator("#recordingSheet").is_hidden(), page.evaluate(
            """() => ({
              warning: document.querySelector("#recordingWarning").textContent,
              state: document.querySelector("#recordingState").textContent,
            })"""
        )
        assert page.evaluate("window.__localProcessingRequest.recordingId") == (
            "118f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        )
        page.screenshot(path=args.output / "desktop-import-dark.png", full_page=True)

        permission_page = browser.new_page(viewport={"width": 960, "height": 760})
        permission_page.add_init_script(TAURI_STUB)
        permission_page.add_init_script(
            "window.__capturePermissions = { microphone:'not_determined', "
            "screenAndSystemAudio:'denied' };"
        )
        permission_page.goto(url)
        permission_page.get_by_role("button", name="录音", exact=True).click()
        permission_page.get_by_role("button", name="授权并刷新来源").wait_for()
        assert permission_page.get_by_role("button", name="开始录音").is_disabled()
        permission_page.screenshot(path=args.output / "capture-permission-request.png", full_page=True)
        permission_page.get_by_role("button", name="授权并刷新来源").click()
        permission_page.get_by_text("权限已就绪，请确认来源后开始录音。", exact=True).wait_for()
        assert permission_page.get_by_role("button", name="开始录音").is_enabled()
        assert permission_page.evaluate("window.__capturePermissionRequested") == {
            "microphone": True,
            "screenAndSystemAudio": False,
        }
        permission_page.close()

        denied_page = browser.new_page(viewport={"width": 960, "height": 760})
        denied_page.add_init_script(TAURI_STUB)
        denied_page.add_init_script(
            "window.__capturePermissions = { microphone:'denied', "
            "screenAndSystemAudio:'denied' };"
        )
        denied_page.goto(url)
        denied_page.get_by_role("button", name="录音", exact=True).click()
        assert denied_page.get_by_role("button", name="开始录音").is_disabled()
        denied_page.screenshot(path=args.output / "capture-permission-denied.png", full_page=True)
        denied_page.get_by_role("button", name="打开系统设置").click()
        assert denied_page.evaluate("window.__openedCaptureSettings") == "microphone"
        denied_page.close()

        legacy_macos = browser.new_page(viewport={"width": 960, "height": 760})
        legacy_macos.add_init_script(TAURI_STUB)
        legacy_macos.add_init_script(
            "window.__captureCapabilities = { backendAvailable:true, "
            "meetingAvailable:false, platform:'macos', "
            "modes:['voice_memo','meeting','system_capture'] };"
        )
        legacy_macos.goto(url)
        legacy_macos.get_by_role("button", name="录音", exact=True).click()
        meeting_mode = legacy_macos.locator('.mode-option[data-mode="meeting"]')
        assert meeting_mode.is_disabled()
        assert "需要 macOS 14.2+" in meeting_mode.text_content()
        assert legacy_macos.locator('.mode-option[data-mode="voice_memo"]').is_enabled()
        assert legacy_macos.locator('.mode-option[data-mode="system_capture"]').is_enabled()
        legacy_macos.close()

        desktop_recovery = browser.new_page(viewport={"width": 960, "height": 760})
        desktop_recovery.add_init_script(TAURI_STUB)
        desktop_recovery.add_init_script("window.__desktopRecovery = true")
        desktop_recovery.goto(url)
        desktop_recovery.locator("#recordingSheet").wait_for(state="visible")
        desktop_recovery.get_by_text("录音已中断", exact=True).wait_for()
        assert desktop_recovery.get_by_role("button", name="无法继续").is_disabled()
        assert "Recovered Mic · Recovered Zoom" in desktop_recovery.locator(
            "#recordingSourceName"
        ).text_content()
        warning = desktop_recovery.locator("#recordingWarning").text_content()
        assert "安全落盘" in warning
        assert "00:03" in warning
        desktop_recovery.close()

        held_page = browser.new_page(viewport={"width": 960, "height": 760})
        held_page.add_init_script(
            TAURI_STUB
            + "\nwindow.__runtimeFeatures = { recording:false, audioImport:false, "
            "directProcessing:false, browserCapture:false, localStt:false, localQwenCandidate:false };"
        )
        held_page.goto(url)
        held_page.locator("#captureTools").wait_for(state="visible")
        assert held_page.locator("#recordBtn").is_disabled()
        assert held_page.locator("#importBtn").is_disabled()
        assert held_page.locator("#processingSetupBtn").is_hidden()
        assert held_page.locator("#localModelBtn").is_hidden()
        assert held_page.locator("#localPack").is_hidden()
        assert "直接处理已停用" in held_page.locator("#captureState").text_content()
        held_page.close()

        setup_page = browser.new_page(viewport={"width": 960, "height": 760})
        setup_page.add_init_script(TAURI_STUB)
        setup_page.route(
            "**/api/sync/refresh",
            lambda route: route.fulfill(status=200, content_type="application/json", body='{"ok":true}'),
        )
        setup_page.route(
            "**/api/sync/status",
            lambda route: route.fulfill(
                status=200,
                content_type="application/json",
                body='{"state":"ok","recordings":0}',
            ),
        )
        setup_page.goto(f"{url.rsplit('/', 1)[0]}/setup-placeholder?desktop=1")
        setup_html = (
            Path(__file__).resolve().parents[2]
            / "desktop/src-tauri/src/setup.html"
        ).read_text(encoding="utf-8")
        setup_page.set_content(setup_html)
        setup_page.locator("#github_pat").fill("github_pat_fabricated-restore")
        setup_page.locator("#r2_account_id").fill("0123456789abcdef0123456789abcdef")
        setup_page.locator("#r2_access_key_id").fill("FABRICATEDRESTOREKEY123")
        setup_page.locator("#r2_secret_access_key").fill("fabricated-restore-secret")
        setup_page.locator("#repo").fill("owner/empty-private-notes")
        setup_page.locator("#bucket").fill("empty-private-audio")
        setup_page.get_by_role("button", name="保存并同步").click()
        setup_page.get_by_text("✓ 已连接 · 0 条录音", exact=True).wait_for()
        assert setup_page.evaluate("window.__savedArchiveCredentials")["repo"] == (
            "owner/empty-private-notes"
        )
        assert setup_page.locator("#github_pat").input_value() == ""
        setup_page.wait_for_url("**/index.html")
        setup_page.close()

        mobile_context = browser.new_context(
            viewport={"width": 390, "height": 844},
            user_agent=(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) "
                "AppleWebKit/605.1.15 Mobile/15E148"
            ),
        )
        mobile = mobile_context.new_page()
        mobile.add_init_script(TAURI_STUB)
        mobile.add_init_script("window.__shareReplayEnabled = true")
        mobile.goto(f"{url}?m=1")
        mobile.wait_for_load_state("networkidle")
        mobile.locator("#captureTools").wait_for(state="visible")
        mobile.get_by_text("Shared Voice Memo.m4a", exact=True).wait_for()
        assert mobile.evaluate("window.__acknowledgedSharedImports") == [
            "518f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        ]
        mobile.get_by_role("button", name="导入", exact=True).click()
        mobile.get_by_text("picked-voice.m4a", exact=True).wait_for()
        assert mobile.evaluate("window.__acknowledgedSharedImports") == [
            "718f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        ]
        mobile.get_by_role("button", name="录音", exact=True).click()
        mobile.locator("#recordingSheet").wait_for(state="visible")
        assert int(mobile.get_by_role("meter", name="麦克风实时电平").get_attribute("aria-valuenow")) > 0
        assert mobile.get_by_role("meter", name="所选来源实时电平").is_hidden()
        mobile.screenshot(path=args.output / "mobile-recording-live.png", full_page=True)
        mobile.get_by_role("button", name="暂停").click()
        mobile.get_by_role("button", name="继续").wait_for()
        mobile.screenshot(path=args.output / "mobile-recording.png", full_page=True)
        mobile.get_by_role("button", name="继续").click()
        mobile.get_by_role("button", name="停止并保存").click()
        mobile.locator("#recordingSheet").wait_for(state="hidden")
        mobile_context.close()

        recovery_context = browser.new_context(
            viewport={"width": 390, "height": 844},
            user_agent=(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) "
                "AppleWebKit/605.1.15 Mobile/15E148"
            ),
        )
        recovery = recovery_context.new_page()
        recovery.add_init_script(TAURI_STUB)
        recovery.add_init_script("window.__iosPendingRecovery = true")
        recovery.goto(f"{url}?m=1")
        recovery.locator("#recordingSheet").wait_for(state="visible")
        recovery.get_by_text("录音已中断", exact=True).wait_for()
        assert recovery.get_by_role("button", name="无法继续").is_disabled()
        recovery.get_by_role("button", name="停止并保存").click()
        recovery.locator("#recordingSheet").wait_for(state="hidden")
        assert recovery.evaluate("window.__finalizedPendingMobileRecording") == (
            "818f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        )
        recovery_context.close()

        restarted_context = browser.new_context(
            viewport={"width": 390, "height": 844},
            user_agent=(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) "
                "AppleWebKit/605.1.15 Mobile/15E148"
            ),
        )
        restarted = restarted_context.new_page()
        restarted.add_init_script(TAURI_STUB)
        restarted.add_init_script(
            """
            window.__iosProcessRestarted = true;
            localStorage.setItem("echowall-native-recording", JSON.stringify({
              recordingId: "918f92d8-6ad4-7dc1-8e28-8b020d2942cb",
              startedAt: Date.now(), pausedAt: null, pausedTotal: 0
            }));
            """
        )
        restarted.goto(f"{url}?m=1")
        restarted.get_by_text("录音已中断", exact=True).wait_for()
        restarted.get_by_role("button", name="停止并保存").click()
        restarted.locator("#recordingSheet").wait_for(state="hidden")
        assert restarted.evaluate("window.__finalizedPendingMobileRecording") == (
            "918f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        )
        restarted_context.close()

        android_context = browser.new_context(
            viewport={"width": 412, "height": 915},
            user_agent=(
                "Mozilla/5.0 (Linux; Android 16; Pixel 8) "
                "AppleWebKit/537.36 Chrome/151 Mobile Safari/537.36"
            ),
        )
        android = android_context.new_page()
        android.add_init_script(TAURI_STUB)
        android.add_init_script("window.__androidShareReplayEnabled = true")
        android.goto(f"{url}?m=1")
        android.wait_for_load_state("networkidle")
        android.get_by_text("android-shared-voice.m4a", exact=True).wait_for()
        assert android.evaluate("window.__acknowledgedSharedImports") == [
            "618f92d8-6ad4-7dc1-8e28-8b020d2942cb"
        ]
        android_context.close()

        storage_context = browser.new_context(
            viewport={"width": 390, "height": 844},
            user_agent=(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) "
                "AppleWebKit/605.1.15 Mobile/15E148"
            ),
        )
        storage_page = storage_context.new_page()
        storage_page.add_init_script(TAURI_STUB)
        storage_page.add_init_script("window.__mobileStorageLow = true")
        storage_page.goto(f"{url}?m=1")
        storage_page.get_by_role("button", name="录音", exact=True).click()
        storage_page.get_by_text("可用存储空间不足 1 GB，无法开始两小时录音", exact=True).wait_for()
        assert storage_page.evaluate("window.__mobileStartCalled") is None
        assert storage_page.locator("#recordingSheet").is_hidden()
        storage_context.close()

        page.evaluate("document.querySelector('#processingDialog').showModal()")
        page.screenshot(path=args.output / "processing-credentials-dark.png", full_page=True)
        page.once("dialog", lambda dialog: dialog.accept())
        page.get_by_role("button", name="删除本机凭据").click()
        page.locator("#processingDialog").wait_for(state="hidden")
        assert page.evaluate("window.__processingCredentialsDeleted") is True
        assert page.evaluate("window.__archiveCredentialsDeleted") is True
        # Production's compiled viewer can have core.invoke without a dialog
        # global. It must still open the registered native picker, and the
        # narrow sidebar must keep settings labels intact.
        picker_page = browser.new_page(viewport={"width": 1440, "height": 900})
        picker_page.add_init_script(TAURI_STUB + """
          delete window.__TAURI__.dialog;
          window.__emptyProcessingQueue = true;
          window.__captureCapabilities = { backendAvailable: false, platform: 'macos' };
        """)
        picker_page.goto(url)
        picker_page.wait_for_load_state("networkidle")
        picker_page.locator("#captureState").get_by_text(
            "录音暂不可用 · 可导入文件", exact=True
        ).wait_for()
        assert picker_page.locator("#recordBtn").is_disabled()
        assert picker_page.locator("#importBtn").is_enabled()
        for theme in ("dark", "light"):
            picker_page.evaluate("theme => document.documentElement.dataset.theme = theme", theme)
            assert picker_page.evaluate("""() => {
              const head = document.querySelector('.capture-head').getBoundingClientRect();
              return head.width < 200 && [...document.querySelectorAll('.capture-connect')]
                .filter(button => !button.hidden).every(button => {
                  const box = button.getBoundingClientRect();
                  const range = document.createRange();
                  range.selectNodeContents(button);
                  return range.getClientRects().length === 1 &&
                    box.left >= head.left && box.right <= head.right &&
                    button.scrollWidth <= button.clientWidth;
                });
            }""")
            picker_page.locator("#captureTools").screenshot(
                path=args.output / f"capture-sidebar-{theme}.png"
            )
        picker_page.locator("#importBtn").click()
        picker_page.get_by_text("interview.m4a", exact=True).wait_for()
        assert picker_page.evaluate("window.__nativePickerOptions") == {
            "multiple": True, "directory": False, "title": "导入录音",
            "filters": [{"name": "录音", "extensions": ["m4a", "mp3", "wav"]}],
        }
        picker_page.evaluate("window.__nativePickerFailure = true")
        picker_page.locator("#importBtn").click()
        picker_page.locator("#captureState").get_by_text(
            "无法打开文件选择器，请重试或拖入文件", exact=True
        ).wait_for()
        assert picker_page.locator("#importBtn").is_enabled()
        picker_page.close()
        browser.close()

    if server is not None:
        server.shutdown()
        server.server_close()

    if console_errors:
        raise SystemExit("browser console errors: " + " | ".join(console_errors))
    print("capture UI smoke passed")


if __name__ == "__main__":
    main()
