#!/usr/bin/env python3
"""Headless MOSS UI checks with fabricated IPC and every HTTP request intercepted.

No native worker, microphone, playback, model download, provider, or user file is
accessed. Screenshots show synthetic state only. Requires Python Playwright and
an installed Chromium/Chrome; never installs either dependency.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

from playwright.sync_api import expect, sync_playwright


STUB = r"""
window.__calls = [];
window.__rows = window.__config.rows || [];
window.__preference = window.__config.preference || null;
if(window.__config.legacyWhisper) localStorage.setItem('echowall-prefer-local-model','1');
window.__localModel = {supported:true,enabled:true,installed:false,installing:false,
  memorySufficient:true,totalBytes:18000000000,licenses:[],...(window.__config.localModel||{})};
window.__model = {
  supported: true, enabled: true, packId: 'moss-full-local-v1', installed: true,
  installing: false, memorySufficient: true, minimumMemoryBytes: 34359738368,
  totalBytes: 18000000000, downloadedBytes: 18000000000, errorCode: null,
  components: ['moss','speakerkit','summary'].map(id => ({id,displayName:id,installed:true,totalBytes:6000000000})),
  licenses: ['Apache-2.0 · MOSS', 'CC-BY-4.0 · SpeakerKit', 'Apache-2.0 · Qwen'],
  ...(window.__config.model || {}),
};
const row = (id, state, backend='moss_local', errorCode=null) => ({
  recordingId:id, status: {state,transcriptionBackend:backend,summaryBackend:'qwen_local',
    publicationBackend:'local_archive',diarizationSelected:true,transcriptOnlyAccepted:false,
    remoteTaskAccepted:false,remoteTaskSuperseded:false,transcriptAvailable:false,
    summaryAvailable:false,temporaryCleanupPending:false,revision:2},errorCode,errorMessage:null,
});
const remember = result => {
  window.__rows = window.__rows.filter(r => r.recordingId !== result.recordingId).concat(result);
  return structuredClone(result);
};
window.__finishInstall = (success=true) => {
  window.__model.installing=false;
  window.__model.installed=success;
  window.__model.errorCode=success ? null : 'download_incomplete';
  window.__model.components.forEach(c => c.installed=success);
  if (window.__installResolve) {
    const finish=success ? window.__installResolve : window.__installReject;
    window.__installResolve=null;
    finish(success ? structuredClone(window.__model) : new Error('fabricated failure'));
  }
};
HTMLMediaElement.prototype.play = () => { throw new Error('playback forbidden in fabricated UI test'); };
window.__TAURI__ = { core: { invoke: async (command,args) => {
  window.__calls.push({command,args});
  if(command==='runtime_features') return {recording:false,audioImport:true,directProcessing:true,
    browserCapture:false,localStt:window.__config.localStt!==false,localQwenCandidate:false,
    ...(window.__config.featureOmitted ? {} : {localMossCandidate:window.__config.enabled!==false})};
  if(command==='processing_credentials_status') return {configured:true,archiveConfigured:true};
  if(command==='get_processing_preference') {
    if(window.__config.preferenceFailure || window.__preferenceFailure) throw new Error('fabricated preference failure');
    if(window.__holdPreference) return await new Promise(resolve=>{window.__preferenceResolve=resolve;});
    return {newRecordingRoute:window.__preference};
  }
  if(command==='set_processing_preference') {
    if(window.__savePreferenceFailure) throw new Error('fabricated preference save failure');
    if(window.__holdPreferenceSave) await new Promise(resolve=>{window.__preferenceSaveResolve=resolve;});
    window.__preference=args.request.newRecordingRoute;
    return {newRecordingRoute:window.__preference};
  }
  if(command==='local_model_pack_status') return structuredClone(window.__localModel);
  if(command==='moss_pipeline_model_status') {
    if(window.__statusFailure) throw new Error('fabricated status failure');
    return structuredClone(window.__model);
  }
  if(command==='list_processing_recordings') {
    if(window.__ledgerFailure) throw new Error('fabricated ledger failure');
    return {results:structuredClone(window.__rows)};
  }
  if(command==='confirm_import_review') return null;
  if(command==='mobile_list_pending') return {items:[]};
  if(command==='install_moss_pipeline_model_pack') {
    window.__model.installing=true;
    window.__model.downloadedBytes=9000000000;
    return await new Promise((resolve,reject)=>{window.__installResolve=resolve;window.__installReject=reject;});
  }
  if(command==='cancel_local_model_install') { window.__finishInstall(false); return null; }
  if(command==='remove_moss_pipeline_model_pack') {
    window.__model.components.forEach(c=>{if(c.id==='moss'||args.request.removeShared)c.installed=false;});
    window.__model.installed=false;
    return structuredClone(window.__model);
  }
  if(command==='process_recording_with_moss_candidate') {
    if(window.__selectionFailure) throw new Error('fabricated uncertain selection');
    if(window.__remoteRace) {
      const rejected=row(args.request.recordingId,'polling','miaoji_remote','remote_effect_already_started');
      rejected.status.remoteTaskAccepted=true;
      return remember(rejected);
    }
    const selected=remember(row(args.request.recordingId,'preparing_local_moss'));
    if(window.__holdSelection) return await new Promise(resolve=>{window.__selectionResolve=resolve;});
    return selected;
  }
  if(command==='process_recording_with_local_models') return remember(row(args.request.recordingId,'local_transcribing','whisper_local'));
  if(command==='retry_processing_recording') return remember(row(args.request.recordingId,'preparing_local_moss'));
  if(command==='cancel_processing_recording') {
    if(window.__commitFence) return remember(row(args.request.recordingId,'publishing','moss_local','publication_commit_in_progress'));
    const canceled=remember(row(args.request.recordingId,'canceled_before_upload'));
    if(window.__selectionResolve) window.__selectionResolve(canceled);
    return canceled;
  }
  if(command==='process_recordings') return {results:args.request.recordingIds.map(id=>remember(row(id,'provider_failed')))};
  throw new Error('unexpected fabricated IPC: '+command);
}}, webviewWindow:{getCurrentWebviewWindow:()=>({onDragDropEvent:async()=>()=>{}})}};
"""


def result(identifier="queued-fixture", state="queued", backend="miaoji_remote", **overrides):
    return {
        "recordingId": identifier,
        "status": {"state": state, "transcriptionBackend": backend,
                   "remoteTaskAccepted": False, "remoteTaskSuperseded": False,
                   "transcriptAvailable": False, "summaryAvailable": False,
                   "diarizationSelected": True, "transcriptOnlyAccepted": False,
                   "temporaryCleanupPending": False, "revision": 1, **overrides},
        "errorCode": None, "errorMessage": None,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--browser", default="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    repository = Path(__file__).resolve().parents[2]
    payload = {"span": "合成测试", "entries": [], "categories": [], "speaker_colors": {}}
    html = (repository / "deliveries/viewer_template.html").read_text().replace("__PAYLOAD__", json.dumps(payload))
    errors = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True, executable_path=args.browser, args=["--mute-audio"])

        def page_for(config=None, width=1440, theme="dark", mobile=False):
            mobile_options={"user_agent":"Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148"} if mobile else {}
            page = browser.new_page(viewport={"width": width, "height": 960}, color_scheme=theme, **mobile_options)
            page.clock.install()
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.route("**/*", lambda route: route.fulfill(
                status=200, content_type="text/html" if route.request.resource_type=="document" else "application/json",
                body=html if route.request.resource_type=="document" else "{}"))
            page.add_init_script("window.__config="+json.dumps(config or {})+";"+STUB)
            page.goto("https://moss-ui.invalid/index.html"+("?m" if mobile else ""))
            page.wait_for_load_state("networkidle")
            page.wait_for_function("window.__calls.some(c=>c.command==='list_processing_recordings')")
            return page

        def calls(page, command):
            return page.evaluate("command=>window.__calls.filter(c=>c.command===command)", command)

        def no_processing(page):
            assert not page.evaluate("window.__calls.some(c=>/^(process_|install_|take_over_)/.test(c.command))")

        def no_wrong_route(page):
            assert not page.evaluate("window.__calls.some(c=>/process_recording_with_(local_models|qwen_candidate)|take_over_|accept_local_transcript_only|back_up_local/.test(c.command))")

        def model_dialog(page):
            page.locator("#mossModelBtn").click()
            expect(page.locator("#mossCandidatePack")).to_be_visible()

        def add_import(page, identifier="new-fixture"):
            page.evaluate("id=>document.querySelector('#importQueue').prepend(renderImportResult({recordingId:id,sourcePath:'/fabricated/example.wav',status:'imported',durationMs:1000,sizeBytes:44,codec:'wav'}))", identifier)
            return page.locator(f'.import-item[data-recording-id="{identifier}"]')

        def capture(page, identifier="captured-fixture"):
            page.evaluate("id=>{void queueCapturedRecording({recordingId:id,sourcePath:'native-capture',status:'native_ready',durationMs:1000,sizeBytes:44,codec:'wav'});}", identifier)
            return page.locator(f'.import-item[data-recording-id="{identifier}"]')

        for config, mobile, name in [({"enabled": False}, False, "feature-off"),
                                     ({"featureOmitted": True}, False, "missing-feature"),
                                     ({}, True, "mobile")]:
            page = page_for(config, width=390 if mobile else 1440, mobile=mobile)
            assert page.locator("#mossModelBtn").is_hidden()
            assert page.locator("#mossCandidatePack").is_hidden()
            assert not calls(page,"moss_pipeline_model_status")
            assert add_import(page).get_by_role("button",name="MOSS 本地处理").count()==0
            no_processing(page)
            page.screenshot(path=args.output/f"moss-{name}.png",full_page=True)
            page.close()

        for theme in ["dark","light"]:
            for width in [1440,1000]:
                page=page_for({"rows":[result()]},width=width,theme=theme)
                no_processing(page)
                queue=page.locator('.import-item[data-recording-id="queued-fixture"]')
                expect(queue.get_by_role("button",name="MOSS 本地处理")).to_be_visible()
                # Existing sidebar is170px internally: labels stay on one line
                # and action groups wrap within their own bounds.
                assert page.evaluate("""()=>[...document.querySelectorAll('.capture-connect,.processing-row-actions button')]
                  .filter(b=>!b.hidden&&b.getBoundingClientRect().width).every(b=>{
                    const box=b.getBoundingClientRect(),parent=b.parentElement.getBoundingClientRect();
                    const range=document.createRange();range.selectNodeContents(b);
                    return range.getClientRects().length===1&&box.left>=parent.left&&box.right<=parent.right+1;
                  })""")
                page.locator("#captureTools").screenshot(path=args.output/f"moss-sidebar-{theme}-{width}.png")
                model_dialog(page)
                assert page.evaluate("""()=>[...document.querySelectorAll('.local-pack-default')]
                  .filter(label=>label.getBoundingClientRect().width>0).every(label=>{
                    const input=label.querySelector('input').getBoundingClientRect();
                    const text=label.querySelector('span').getBoundingClientRect();
                    return input.width<=24&&text.width>=Math.min(180,label.getBoundingClientRect().width*.7);
                  })"""), "native radio/checkbox sizing must not squeeze preference text"
                assert page.locator("#mossCandidateComponents li").count()==3
                assert page.locator("#mossCandidatePack").inner_text().count("Whisper")==0
                assert page.locator("#mossCandidatePack input[type=checkbox]").count()==1
                page.screenshot(path=args.output/f"moss-models-{theme}-{width}.png",full_page=True)
                page.close()

        missing={"installed":False,"components":[{"id":id,"installed":id!="speakerkit"} for id in ["moss","speakerkit","summary"]]}
        page=page_for({"model":missing})
        imported=add_import(page)
        imported.get_by_role("button",name="MOSS 本地处理").click()
        expect(page.locator("#mossCandidatePack")).to_be_visible()
        assert not calls(page,"process_recording_with_moss_candidate")
        no_processing(page)
        expect(page.locator("#mossCandidateComponents")).to_contain_text("待安装")
        page.locator("#mossCandidateInstall").click()
        expect(page.locator("#mossCandidateProgress")).to_be_visible()
        assert calls(page,"install_moss_pipeline_model_pack")[-1]["args"]["request"]=={"packId":"moss-full-local-v1"}
        page.screenshot(path=args.output/"moss-install-progress.png",full_page=True)
        page.locator("#mossCandidateCancel").click()
        expect(page.locator("#mossCandidateInstall")).to_be_visible()
        assert calls(page,"cancel_local_model_install")[-1]["args"]["request"]=={"packId":"moss-full-local-v1"}
        page.locator("#mossCandidateInstall").click()
        page.wait_for_function("window.__model.installing")
        page.evaluate("window.__finishInstall(true)")
        expect(page.locator("#mossCandidateState")).to_contain_text("已安装")
        page.once("dialog",lambda dialog:dialog.accept())
        page.locator("#mossCandidateRemove").click()
        page.wait_for_function("window.__calls.some(c=>c.command==='remove_moss_pipeline_model_pack')")
        assert calls(page,"remove_moss_pipeline_model_pack")[-1]["args"]["request"]=={"packId":"moss-full-local-v1","removeShared":False}
        page.evaluate("window.__model.installed=true;window.__model.components.forEach(c=>c.installed=true);renderMossCandidateModelStatus(window.__model)")
        page.locator("#mossRemoveShared").check()
        dialogs=[]
        def reject_shared(dialog):
            dialogs.append(dialog.message)
            dialog.dismiss()
        page.once("dialog",reject_shared)
        page.locator("#mossCandidateRemove").click()
        assert len(calls(page,"remove_moss_pipeline_model_pack"))==1
        assert "其他本地路线" in dialogs[0] and "SpeakerKit" in dialogs[0]
        page.once("dialog",lambda dialog:dialog.accept())
        page.locator("#mossCandidateRemove").click()
        page.wait_for_function("window.__calls.filter(c=>c.command==='remove_moss_pipeline_model_pack').length===2")
        assert calls(page,"remove_moss_pipeline_model_pack")[-1]["args"]["request"]["removeShared"] is True
        no_wrong_route(page)
        page.close()

        for model in [{"supported":False}, {"enabled":False}, {"memorySufficient":False},
                      {"installed":True,"components":[{"id":"moss","installed":True}]}]:
            page=page_for({"model":model})
            add_import(page).get_by_role("button",name="MOSS 本地处理").click()
            expect(page.locator("#mossCandidatePack")).to_be_visible()
            assert not calls(page,"process_recording_with_moss_candidate")
            if not all(model.get(key,True) for key in ["supported","enabled","memorySufficient"]):
                expect(page.locator("#mossCandidateInstall")).to_be_disabled()
            no_processing(page)
            page.close()

        page=page_for()
        page.evaluate("window.__statusFailure=true")
        model_dialog(page)
        expect(page.locator("#mossCandidateRefresh")).to_be_visible()
        expect(page.locator("#mossCandidateInstall")).to_be_disabled()
        page.evaluate("window.__statusFailure=false")
        page.locator("#mossCandidateRefresh").click()
        expect(page.locator("#mossCandidateState")).to_contain_text("已安装")
        no_processing(page)
        page.close()

        page=page_for()
        imported=add_import(page)
        page.evaluate("window.__selectionFailure=true;window.__ledgerFailure=true")
        imported.get_by_role("button",name="MOSS 本地处理").click()
        expect(imported.locator(".import-status")).to_contain_text("状态待确认")
        assert imported.get_by_role("button",name="云端处理",exact=True).count()==0
        assert not calls(page,"process_recordings")
        no_wrong_route(page)
        page.close()

        page=page_for({"rows":[result()]})
        queue=page.locator('.import-item[data-recording-id="queued-fixture"]')
        page.evaluate("window.__holdSelection=true")
        queue.get_by_role("button",name="MOSS 本地处理").click()
        expect(queue.locator(".import-status")).to_have_text("MOSS 本地准备中")
        assert calls(page,"process_recording_with_moss_candidate")[-1]["args"]["request"]=={"recordingId":"queued-fixture","language":None}
        # Whisper is deliberately missing in every fixture; MOSS dispatch still
        # depends only on its own three verified components.
        assert queue.get_by_role("button",name="MOSS 本地处理").count()==0
        page.once("dialog",lambda dialog:dialog.accept())
        queue.get_by_role("button",name="取消处理").click()
        expect(queue.locator(".import-status")).to_have_text("MOSS · 已取消")
        page.evaluate("updateProcessingRow({recordingId:'queued-fixture',status:{state:'preparing_local_moss',transcriptionBackend:'moss_local',revision:1}})")
        expect(queue.locator(".import-status")).to_have_text("MOSS · 已取消")
        assert queue.get_by_role("button",name="继续 MOSS 处理").count()==0
        assert queue.get_by_role("button",name="重试",exact=True).count()==0
        assert not calls(page,"process_recordings")
        no_wrong_route(page)
        page.close()

        page=page_for({"rows":[result()]})
        page.evaluate("window.__remoteRace=true")
        queue=page.locator('.import-item[data-recording-id="queued-fixture"]')
        queue.get_by_role("button",name="MOSS 本地处理").click()
        expect(queue.locator(".import-status")).to_have_text("云端任务已开始 · 未切换 MOSS")
        page.clock.fast_forward(11000)
        assert queue.get_by_role("button",name="MOSS 本地处理").count()==0
        assert not calls(page,"process_recordings")
        page.screenshot(path=args.output/"moss-remote-start-rejected.png",full_page=True)
        no_wrong_route(page)
        page.close()

        page=page_for({"rows":[result("failed-fixture","provider_failed","moss_local")]})
        queue=page.locator('.import-item[data-recording-id="failed-fixture"]')
        assert queue.get_by_role("button",name="跳过说话人分离并重试").count()==0
        assert queue.get_by_role("button",name="安装本地模型",exact=True).count()==0
        queue.get_by_role("button",name="重试",exact=True).click()
        expect(queue.locator(".import-status")).to_have_text("MOSS 本地准备中")
        page.evaluate("window.__commitFence=true")
        page.once("dialog",lambda dialog:dialog.accept())
        queue.get_by_role("button",name="取消处理").click()
        expect(queue.locator(".import-status")).to_have_text("档案提交中 · 暂不能取消")
        expect(queue.get_by_role("button",name="继续核验档案")).to_be_visible()
        assert queue.get_by_role("button",name="取消处理").count()==0
        assert "已取消" not in queue.inner_text()
        page.screenshot(path=args.output/"moss-commit-fence.png",full_page=True)
        no_wrong_route(page)
        page.close()

        # Native-persisted preference is the only new-recording auto-route.
        # Old queued/Whisper jobs are not reinterpreted when MOSS is selected.
        page=page_for({"preference":"moss_local","rows":[result(),result("old-whisper","provider_failed","whisper_local")]})
        no_processing(page)
        assert page.locator("#mossPackDefault").is_checked()
        capture(page)
        page.wait_for_function("window.__calls.some(c=>c.command==='process_recording_with_moss_candidate')")
        assert calls(page,"process_recording_with_moss_candidate")[-1]["args"]["request"]["recordingId"]=="captured-fixture"
        assert not calls(page,"process_recordings")
        assert not calls(page,"process_recording_with_local_models")
        assert not calls(page,"install_moss_pipeline_model_pack")
        page.close()

        # Missing models still permit Rust's durable MOSS selection; the UI
        # cannot replace that choice with download, Whisper or remote dispatch.
        page=page_for({"preference":"moss_local","model":missing})
        capture(page)
        page.wait_for_function("window.__calls.some(c=>c.command==='process_recording_with_moss_candidate')")
        assert not calls(page,"install_moss_pipeline_model_pack")
        assert not calls(page,"process_recordings")
        page.close()

        for config, label in [
            ({}, "请选择处理方式"),
            ({"preferenceFailure":True}, "请选择处理方式"),
            ({"preference":"moss_local","enabled":False}, "MOSS 暂不可用"),
            ({"preference":"moss_local","localStt":False}, "MOSS 暂不可用"),
            ({"preference":"whisper_local"}, "Whisper 模型未就绪"),
        ]:
            page=page_for(config)
            captured=capture(page)
            expect(captured.locator(".import-status")).to_contain_text(label)
            page.evaluate("processRecordingIds(pendingRecordingIds()); processRecordingIds(['captured-fixture']);")
            page.clock.fast_forward(11000)
            no_processing(page)
            captured.get_by_role("button",name="云端处理",exact=True).click()
            page.wait_for_function("window.__calls.some(c=>c.command==='process_recordings')")
            assert calls(page,"process_recordings")[-1]["args"]["request"]["recordingIds"]==["captured-fixture"]
            page.close()

        # Cold UI restoration of a queued row also stays manual when local
        # features are disabled; started work remains a native-ledger concern.
        page=page_for({"enabled":False,"preference":"moss_local","rows":[result()]})
        page.clock.fast_forward(11000)
        no_processing(page)
        expect(page.get_by_role("button",name="云端处理",exact=True)).to_be_visible()
        page.close()

        page=page_for({"preference":"whisper_local","localModel":{"installed":True}})
        capture(page)
        page.wait_for_function("window.__calls.some(c=>c.command==='process_recording_with_local_models')")
        assert not calls(page,"process_recording_with_moss_candidate")
        assert not calls(page,"process_recordings")
        page.close()

        page=page_for({"preference":"remote"})
        capture(page)
        page.wait_for_function("window.__calls.some(c=>c.command==='process_recordings')")
        assert not calls(page,"process_recording_with_moss_candidate")
        page.close()

        page=page_for({"legacyWhisper":True,"localModel":{"installed":True}})
        assert page.locator("#localPackDefault").is_checked()
        assert not page.locator("#mossPackDefault").is_checked()
        assert not calls(page,"set_processing_preference")
        model_dialog(page)
        page.locator("#processingPreferenceSaveWhisper").click()
        page.wait_for_function("window.__preference==='whisper_local'")
        assert calls(page,"set_processing_preference")[-1]["args"]=={"request":{"newRecordingRoute":"whisper_local"}}
        page.locator("#mossPackDefault").check()
        page.wait_for_function("window.__preference==='moss_local'")
        assert not page.locator("#localPackDefault").is_checked()
        assert page.evaluate("localStorage.getItem('echowall-prefer-local-model')") is None
        page.locator("#remoteProcessingDefault").check()
        page.wait_for_function("window.__preference==='remote'")
        no_processing(page)
        page.close()

        page=page_for({"preference":"remote"})
        page.evaluate("window.__savePreferenceFailure=true; void saveProcessingPreference('moss_local');")
        page.wait_for_function("!PROCESSING_PREFERENCE_BUSY")
        capture(page)
        page.clock.fast_forward(11000)
        no_processing(page)
        page.close()

        # A recording that awaited an earlier GET must also await a later SET.
        # Stale GET completion cannot restore the old remote preference.
        page=page_for({"preference":"remote"})
        page.evaluate("window.__holdPreference=true; void (PROCESSING_PREFERENCE_PENDING=loadProcessingPreference());")
        captured=capture(page)
        page.evaluate("window.__holdPreferenceSave=true; void saveProcessingPreference('moss_local');")
        page.evaluate("window.__preferenceResolve({newRecordingRoute:'remote'});")
        page.wait_for_function("!!window.__preferenceSaveResolve")
        assert not calls(page,"process_recordings")
        assert not calls(page,"process_recording_with_moss_candidate")
        page.evaluate("window.__preferenceSaveResolve();")
        expect(captured.locator(".import-status")).to_contain_text("MOSS")
        assert calls(page,"process_recording_with_moss_candidate")
        assert not calls(page,"process_recordings")
        page.close()

        page=page_for({"preference":"moss_local"})
        page.evaluate("window.__selectionFailure=true; window.__ledgerFailure=true;")
        captured=capture(page)
        expect(captured.locator(".import-status")).to_contain_text("状态待确认")
        page.evaluate("processRecordingIds(['captured-fixture'], true);")
        assert not calls(page,"process_recordings")
        page.close()

        for state in ["uploading","transcribing","provider_failed","complete"]:
            page=page_for({"rows":[result(state=state)]})
            assert page.get_by_role("button",name="MOSS 本地处理").count()==0
            page.close()
        browser.close()
    assert not errors, errors
    print("MOSS UI synthetic smoke passed: feature gates, native preference load/save/races, manual and unavailable-local holds, legacy Whisper choice, explicit remote routing, dark/light layouts, model lifecycle, queued selection, remote rejection, cancellation and commit fence; no native/audio/network dispatch.")


if __name__ == "__main__":
    main()
