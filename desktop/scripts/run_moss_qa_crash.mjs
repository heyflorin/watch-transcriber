#!/usr/bin/env node
// Explicit destructive test of ONE newly launched QA App process. Public AMI
// input only; no audio output, keyguard, UI automation, or production process.
// Keep the receipt/failure and original ledger. Run normal QA launch afterward
// to test recovery; this script never edits/reset/replaces processing state.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import {spawn, spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {setTimeout as delay} from 'node:timers/promises';
import {validateQaTree, ensureQaDirectory} from './moss_qa_paths.mjs';

assert.equal(process.argv.length, 3, 'usage: node scripts/run_moss_qa_crash.mjs <retained-QA.app>');
const bundle = path.resolve(process.argv[2]);
const scripts = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(os.homedir(), 'Library/Application Support/ai.ax.watch-transcriber.qa.moss');
validateQaTree(root);
ensureQaDirectory(root, 'qa-evidence');
const sourceHash = '22c340e2193a29a14ecfcda5d6e7c0010b9d7efbbc3e4d0fc9ac374a2969fc8c';
const jobs = path.join(root, 'processing/jobs');
const files = fs.readdirSync(jobs).filter(file => file.endsWith('.json'));
assert.equal(files.length, 1, 'exactly one QA recording, never choose an arbitrary job');
const ledgerPath = path.join(jobs, files[0]);
function readLedger() {
  const ledger = JSON.parse(fs.readFileSync(ledgerPath, 'utf8'));
  assert.equal(ledger.normalized.sha256, sourceHash);
  assert.equal(ledger.transcription_backend, 'moss_local');
  return ledger;
}
const original = readLedger();
assert.equal(original.state, 'provider_failed', 'only explicit Retry from the retained failed QA job');
assert.equal(original.local_moss.completed_windows.length, 0);
const evidence = path.join(root, 'qa-evidence', `model-active-crash-${crypto.randomUUID()}`);
fs.mkdirSync(evidence, {mode: 0o700});
function write(name, value) {
  const fd = fs.openSync(path.join(evidence, name), 'wx', 0o600);
  try { fs.writeFileSync(fd, JSON.stringify(value, null, 2) + '\n'); fs.fsyncSync(fd); }
  finally { fs.closeSync(fd); }
  const directory = fs.openSync(evidence, 'r');
  try { fs.fsyncSync(directory); } finally { fs.closeSync(directory); }
}
function processInfo(pid) {
  const result = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'pid=,ppid=,rss=,stat=,comm='], {encoding: 'utf8'});
  if (result.status === 1 && !result.stdout.trim()) return null;
  assert.equal(result.status, 0, 'process inspection failed');
  const row = result.stdout.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(\S+)\s+(.+)$/);
  assert.ok(row, 'unrecognized process identity');
  return {pid: Number(row[1]), ppid: Number(row[2]), rss_kib: Number(row[3]), state: row[4], executable: row[5]};
}
write('before-launch.json', {bundle, source_sha256: sourceHash, ledger: original, scope: 'QA-App-death-not-UI-or-OS-offline-proof'});
const launcher = spawn(process.execPath, [path.join(scripts, 'run_moss_qa.mjs'), '--launch-public-retry', bundle], {stdio: ['ignore', 'pipe', 'pipe']});
let appPid, output = '', launcherExited = false;
launcher.stdout.on('data', chunk => {
  output += chunk.toString();
  assert.ok(output.length < 65536);
  let end;
  while ((end = output.indexOf('\n')) >= 0) {
    const line = output.slice(0, end); output = output.slice(end + 1);
    const event = JSON.parse(line);
    if (event.launched) {
      assert.equal(event.bundle, bundle); assert.equal(event.root, root);
      appPid = event.pid;
      console.log(JSON.stringify({qa_app_started: appPid, evidence}));
    }
  }
});
// No child diagnostic content goes to the console. The launcher keeps its
// bounded private App log; startup failure is reported by exit/timeout here.
launcher.stderr.resume();
launcher.on('exit', () => { launcherExited = true; });
const deadline = performance.now() + 180000;
let activeSince, killed = false;
try {
  while (performance.now() < deadline) {
    if (launcherExited) throw Error('QA launcher exited before active-model kill');
    if (!appPid) { await delay(200); continue; }
    const app = processInfo(appPid);
    assert.equal(app?.executable, path.join(bundle, 'Contents/MacOS/desktop'));
    const children = spawnSync('/usr/bin/pgrep', ['-P', String(appPid)], {encoding: 'utf8'});
    assert.ok(children.status === 0 || children.status === 1);
    const workers = children.stdout.trim().split(/\s+/).filter(Boolean).map(Number).map(processInfo)
      .filter(info => info?.executable === path.join(bundle, 'Contents/MacOS/echowall-moss-worker'));
    assert.ok(workers.length <= 1, 'unexpected duplicate MOSS worker');
    const worker = workers[0];
    const ledger = readLedger();
    assert.equal(ledger.local_moss.generation, original.local_moss.generation);
    if (ledger.state === 'provider_failed' && worker == null) { await delay(200); continue; }
    if (ledger.local_moss.completed_windows.length) throw Error('ASR completed before kill; do not claim model-active crash proof');
    if (!worker || worker.rss_kib < 512 * 1024 || ledger.state !== 'local_transcribing') {
      activeSince = undefined; await delay(200); continue;
    }
    assert.equal(worker.ppid, appPid);
    activeSince ??= performance.now();
    if (performance.now() - activeSince < 3000) { await delay(200); continue; }
    write('before-kill.json', {time: new Date().toISOString(), app, worker, active_for_ms: performance.now() - activeSince, ledger});
    // Recheck the exact target immediately before the only destructive action.
    assert.equal(processInfo(appPid)?.executable, app.executable);
    const start = performance.now();
    process.kill(appPid, 'SIGKILL'); killed = true;
    let last;
    do {
      await delay(50);
      last = processInfo(worker.pid);
      if (!last) break;
      assert.equal(last.executable, worker.executable, 'PID identity changed; do not signal it');
    } while (performance.now() - start < 5000);
    const result = {app_pid: appPid, worker_pid: worker.pid, worker_exited: last === null,
      observed_exit_ms: performance.now() - start, source_sha256: sourceHash,
      recording_id: ledger.recording_id, generation: ledger.local_moss.generation,
      completed_windows_before: ledger.local_moss.completed_windows.length,
      ledger_after: readLedger(), scope: 'QA-App-death-not-UI-or-OS-offline-proof'};
    write('after-kill.json', result);
    console.log(JSON.stringify({app_killed: appPid, worker_exited: result.worker_exited, observed_exit_ms: result.observed_exit_ms, evidence}));
    assert.ok(result.worker_exited, 'worker did not disappear within 5 seconds; preserve state for diagnosis');
    break;
  }
  assert.ok(killed, 'no active model observed within the bounded test');
} catch (error) {
  write('failed.json', {error: error.message, app_pid: appPid ?? null, killed});
  // Never kill an unexpected or still-running worker to manufacture a pass.
  console.error(JSON.stringify({crash_test_failed: true, evidence, app_pid: appPid ?? null, killed}));
  process.exitCode = 1;
}
// Do not keep this monitor alive when no kill happened. The App/launcher may
// still be useful for diagnosis; no automatic broad cleanup follows a failure.
launcher.stdout.destroy(); launcher.stderr.destroy(); launcher.unref();
