#!/usr/bin/env node
// Read-only ledger/archive verification for the exact public QA crash test.
// Writes only new evidence receipts; no IPC, audio output or ledger mutation.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {setTimeout as delay} from 'node:timers/promises';
import {fileURLToPath} from 'node:url';
import {validateQaTree} from './moss_qa_paths.mjs';

const [mode, pidText, evidenceInput, updatedBundle] = process.argv.slice(2);
assert.ok(['--complete', '--reopen', '--reopen-updated-main'].includes(mode));
assert.equal(process.argv.length, mode === '--reopen-updated-main' ? 6 : 5,
  'usage: verify_moss_qa_recovery.mjs --complete|--reopen <QA-pid> <crash-evidence-dir>; --reopen-updated-main additionally requires the retained QA bundle');
assert.match(pidText, /^[1-9][0-9]*$/);
const pid = Number(pidText);
const root = path.join(os.homedir(), 'Library/Application Support/ai.ax.watch-transcriber.qa.moss');
validateQaTree(root);
const evidence = fs.realpathSync(evidenceInput);
assert.equal(path.dirname(evidence), path.join(root, 'qa-evidence'));
assert.ok(path.basename(evidence).startsWith('model-active-crash-'));
function read(file) { return JSON.parse(fs.readFileSync(file, 'utf8')); }
function hash(file) {
  assert.equal(fs.realpathSync(file), file);
  assert.ok(fs.lstatSync(file).isFile());
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}
const before = read(path.join(evidence, 'before-launch.json'));
const killed = read(path.join(evidence, 'after-kill.json'));
assert.equal(killed.worker_exited, true);
assert.notEqual(pid, killed.app_pid);
const bundle = updatedBundle ? path.resolve(updatedBundle) : before.bundle;
if (updatedBundle) {
  assert.equal(fs.realpathSync(bundle), bundle);
  assert.equal(path.dirname(bundle), path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../local-eval/bundles'));
  const identity = spawnSync('/usr/bin/plutil', ['-extract', 'CFBundleIdentifier', 'raw', '-o', '-', path.join(bundle, 'Contents/Info.plist')], {encoding: 'utf8'});
  assert.equal(identity.status, 0); assert.equal(identity.stdout.trim(), 'ai.ax.watch-transcriber.qa.moss');
}
const sourceHash = '22c340e2193a29a14ecfcda5d6e7c0010b9d7efbbc3e4d0fc9ac374a2969fc8c';
assert.equal(before.source_sha256, sourceHash);
const ledgerPath = path.join(root, 'processing/jobs', `${killed.recording_id}.json`);
function liveApp() {
  const result = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'comm='], {encoding: 'utf8'});
  assert.equal(result.status, 0, 'QA App exited');
  assert.equal(result.stdout.trim(), path.join(bundle, 'Contents/MacOS/desktop'));
}
const started = performance.now();
let ledger, priorState;
do {
  liveApp();
  ledger = read(ledgerPath);
  assert.equal(ledger.normalized.sha256, sourceHash);
  assert.equal(ledger.local_moss.generation, killed.generation);
  if (ledger.state !== priorState) {
    console.log(JSON.stringify({qa_state: ledger.state, completed_windows: ledger.local_moss.completed_windows.length}));
    priorState = ledger.state;
  }
  if (ledger.state === 'complete') break;
  assert.notEqual(ledger.state, 'provider_failed', 'retained failure; no automatic retry');
  assert.equal(mode, '--complete', 'reopened job must remain complete');
  assert.ok(performance.now() - started < 600000, 'bounded completion deadline');
  await delay(1000);
} while (true);

assert.equal(ledger.transcription_backend, 'moss_local');
assert.equal(ledger.publication_backend, 'local_archive');
assert.equal(ledger.tos_object, null); assert.equal(ledger.miaoji, null);
assert.equal(ledger.local_moss.active_claim, null);
assert.equal(ledger.local_moss.pending_response ?? null, null);
assert.ok(ledger.canonical_backup.locator.startsWith('local:'));
assert.ok(ledger.publication.targets.every(target => target.state === 'verified'));
assert.equal(ledger.local_moss.completed_windows.length, 1);
assert.equal(ledger.transcript_json.length, 184);
assert.equal(Object.keys(ledger.summary_json).length, 7);
const archive = path.join(root, 'data');
const manifestPath = path.join(archive, 'manifest.json');
const entries = Object.values(read(manifestPath));
assert.equal(entries.length, 1, 'no duplicate archive entry');
const entry = entries[0];
assert.equal(entry.r2_key, undefined);
function archivePath(relative) {
  assert.equal(typeof relative, 'string');
  const file = path.resolve(archive, relative);
  assert.ok(file.startsWith(archive + path.sep));
  return file;
}
assert.equal(hash(archivePath(entry.audio)), sourceHash);
assert.equal(hash(path.join(root, 'public-english_01.m4a')), sourceHash);
assert.equal(hash(path.join(root, 'inbox', ledger.recording_id, ledger.normalized.relative_path)), sourceHash);
assert.ok(fs.statSync(archivePath(entry.note)).size > 0);
assert.ok(fs.statSync(path.join(archive, 'index.html')).size > 0);
const artifactRoot = path.join(root, 'processing/moss', ledger.recording_id, killed.generation);
const artifacts = [];
for (const name of fs.readdirSync(artifactRoot).sort()) {
  assert.ok(['plan.json', 'window-00.json', 'anchors.json'].includes(name), 'unexpected output retained for review');
  artifacts.push({name, sha256: hash(path.join(artifactRoot, name))});
}
assert.equal(artifacts.length, 3);
assert.equal(artifacts.find(file => file.name === 'plan.json').sha256, before.ledger.local_moss.plan.sha256);
const logFiles = fs.readdirSync(path.join(root, 'qa-evidence')).filter(name => name.startsWith(`app-${pid}-`) && name.endsWith('.log'));
assert.equal(logFiles.length, 1);
const log = fs.readFileSync(path.join(root, 'qa-evidence', logFiles[0]), 'utf8');
const blockedHttpAttempts = (log.match(/echowall-isolated-qa-http-denied-v1/g) ?? []).length;
assert.equal(blockedHttpAttempts, 0, 'QA route unexpectedly attempted HTTP');
const signature = spawnSync('/usr/bin/codesign', ['--verify', '--deep', '--strict', bundle], {encoding: 'utf8'});
assert.equal(signature.status, 0, 'bundle signature changed');
const binaries = ['desktop', ...['whisper', 'diarization', 'summary', 'qwen', 'moss'].map(name => `echowall-${name}-worker`)]
  .map(name => ({name, sha256: hash(path.join(bundle, 'Contents/MacOS', name))}));
const result = {scope: 'ad-hoc-QA-App-crash-recovery-not-UI-release-or-OS-offline-proof',
  verified_at: new Date().toISOString(), app_pid: pid, bundle, recording_id: ledger.recording_id,
  generation: killed.generation, source_sha256: sourceHash, state: ledger.state,
  windows: ledger.local_moss.completed_windows.length, segments: ledger.transcript_json.length,
  summary_fields: Object.keys(ledger.summary_json).length, archive_entries: entries.length,
  original_and_archived_audio_match: true, remote_checkpoints: false,
  qa_http_denials_in_private_log: blockedHttpAttempts, os_app_network_trace: false,
  ledger_sha256: hash(ledgerPath), manifest_sha256: hash(manifestPath),
  note_sha256: hash(archivePath(entry.note)), artifacts, binaries};
if (mode !== '--complete') {
  const completed = read(path.join(evidence, 'completed.json'));
  for (const key of ['ledger_sha256', 'manifest_sha256', 'note_sha256', 'artifacts']) {
    assert.deepEqual(result[key], completed[key], `reopen changed ${key}`);
  }
  if (mode === '--reopen-updated-main') {
    assert.deepEqual(result.binaries.slice(1), completed.binaries.slice(1), 'worker identities must remain unchanged');
    assert.notEqual(result.binaries[0].sha256, completed.binaries[0].sha256);
    result.previous_main_sha256 = completed.binaries[0].sha256;
    result.scope = 'updated-QA-main-reopen-not-a-repeat-of-the-earlier-model-active-crash';
  } else {
    assert.deepEqual(result.binaries, completed.binaries, 'same-bundle reopen requires all six identities');
  }
  // Observe beyond startup, not just before the normal resume task runs.
  for (let i = 0; i < 20; i++) {
    await delay(500); liveApp();
    assert.equal(hash(ledgerPath), completed.ledger_sha256);
    assert.equal(spawnSync('/usr/bin/pgrep', ['-P', String(pid)]).status, 1, 'unexpected child after completed-job reopen');
  }
  assert.equal(hash(manifestPath), completed.manifest_sha256);
  assert.equal(hash(archivePath(entry.note)), completed.note_sha256);
  for (const artifact of completed.artifacts) assert.equal(hash(path.join(artifactRoot, artifact.name)), artifact.sha256);
  result.idempotent_reopen_observed_seconds = 10;
}
const output = path.join(evidence, mode === '--complete' ? 'completed.json' : `reopened-${pid}.json`);
const fd = fs.openSync(output, 'wx', 0o600);
try { fs.writeFileSync(fd, JSON.stringify(result, null, 2) + '\n'); fs.fsyncSync(fd); }
finally { fs.closeSync(fd); }
console.log(JSON.stringify(result));
