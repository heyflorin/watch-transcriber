#!/usr/bin/env node
// Score a frozen snapshot of completed public App-matrix cases. Does not touch
// the live ledger, choose inference parameters, retry workers, or read credentials.
// Source/reference text stays on disk; stdout contains aggregate grader output.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';

function main() {
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const matrix = fs.realpathSync(path.join(repo, 'local-eval/matrix'));
const [run, stratum = 'all', kind = 'uniform'] = process.argv.slice(2);
assert.ok(process.argv.length >= 3 && process.argv.length <= 5);
const choices = {
  uniform: {directory: 'moss-uniform-app-v1', policy: 'original-aac-app-quiet12-coalesced2-graph3-v1'},
  'window-aware': {directory: 'moss-window-aware-app-v1', policy: 'original-aac-app-quiet12-chronological3-window-aware-tail2-v1'},
};
assert.ok(Object.hasOwn(choices, kind));
const {directory, policy} = choices[kind];
assert.match(run ?? '', /^run-[A-Za-z0-9_-]{1,40}$/);
assert.ok(['all','english','mandarin','mixed','overlap','long_form'].includes(stratum));
const root = path.join(matrix, 'outputs', directory, run);
assert.equal(fs.realpathSync(root), root);
function file(relative, limit = 32 * 1024 * 1024) {
  assert.ok(typeof relative === 'string' && !path.isAbsolute(relative) && !relative.includes('\\'));
  let result = matrix;
  for (const component of relative.split('/')) {
    assert.ok(component && !['.', '..'].includes(component));
    result = path.join(result, component);
    assert.ok(!fs.lstatSync(result).isSymbolicLink());
  }
  const metadata = fs.lstatSync(result);
  assert.ok(metadata.isFile() && metadata.size <= limit);
  return result;
}
function bytes(relative, limit) { return fs.readFileSync(file(relative, limit)); }
function json(relative, limit) { return JSON.parse(bytes(relative, limit)); }
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const encode = value => Buffer.from(JSON.stringify(value, null, 2) + '\n');
function publish(destination, value) {
  const contents = encode(value);
  const temporary = path.join(path.dirname(destination), `.snapshot-${crypto.randomUUID()}.tmp`);
  const fd = fs.openSync(temporary, 'wx', 0o600);
  try { fs.writeFileSync(fd, contents); fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
  try {
    try { fs.linkSync(temporary, destination); }
    catch (error) {
      if (error.code !== 'EEXIST') throw error;
      assert.equal(fs.realpathSync(destination), destination);
      assert.equal(sha(fs.readFileSync(destination)), sha(contents), 'conflicting snapshot is never overwritten');
    }
    const directory = fs.openSync(path.dirname(destination), 'r');
    try { fs.fsyncSync(directory); } finally { fs.closeSync(directory); }
  } finally { fs.unlinkSync(temporary); }
}

const manifestBytes = bytes('manifest.json', 1024 * 1024);
assert.equal(sha(manifestBytes), 'c6615a07f8e80fbe75d3da65b75898423a716749750a1033bca0db70b4506280');
const manifest = JSON.parse(manifestBytes);
const declaration = json(`outputs/${directory}/${run}/started.json`, 1024 * 1024);
assert.equal(declaration.policy, policy);
assert.equal(declaration.manifest_sha256, sha(manifestBytes));
const proofs = new Map();
let failedAttempts = 0;
for (const name of fs.readdirSync(path.join(root, 'case-results')).sort()) {
  if (!name.endsWith('.json')) continue;
  const proof = json(`outputs/${directory}/${run}/case-results/${name}`, 1024 * 1024);
  const entry = manifest.cases.find(entry => entry.case_id === proof.case_id);
  assert.ok(entry && proof.policy === declaration.policy);
  if (stratum !== 'all' && entry.stratum !== stratum) continue;
  if (!proof.successful) { failedAttempts++; continue; }
  assert.equal(proof.state, 'summarizing');
  assert.equal(proof.summary_or_archive_executed, false);
  const relative = `outputs/${directory}/${run}/canonical/${proof.case_id}.json`;
  assert.equal(sha(bytes(relative)), proof.canonical_sha256);
  const ledger = json(`outputs/${directory}/${run}/processing/jobs/${proof.recording_id}.json`);
  assert.equal(ledger.state, 'summarizing');
  assert.equal(ledger.transcription_backend, 'moss_local');
  assert.equal(ledger.normalized.sha256, proof.source_sha256);
  assert.equal(ledger.local_moss.generation, proof.generation);
  const previous = proofs.get(proof.case_id);
  if (previous) assert.equal(previous.canonical_sha256, proof.canonical_sha256);
  proofs.set(proof.case_id, {...proof, local: relative});
}
assert.ok(proofs.size > 0, 'no completed public cases');
const selected = manifest.cases.filter(entry => proofs.has(entry.case_id));
const provenance = selected.map(entry => ({case_id: entry.case_id,
  source_sha256: proofs.get(entry.case_id).source_sha256,
  canonical_sha256: proofs.get(entry.case_id).canonical_sha256,
  ground_truth_sha256: sha(bytes(entry.ground_truth)), miaoji_sha256: sha(bytes(entry.miaoji))}));
const snapshotId = sha(encode({policy: declaration.policy, provenance})).slice(0, 24);
const manifestName = `manifest-moss-app-snapshot-${snapshotId}.json`;
publish(path.join(matrix, manifestName), {...manifest,
  cases: selected.map(entry => ({...entry, local: proofs.get(entry.case_id).local}))});
const scores = path.join(root, 'scores');
if (!fs.existsSync(scores)) fs.mkdirSync(scores, {mode: 0o700});
assert.equal(fs.realpathSync(scores), scores);
const binaries = path.join(repo, 'desktop/local-quality-eval/target/release');
function grade(name, extra) {
  const binary = path.join(binaries, name);
  const result = spawnSync('/usr/bin/sandbox-exec', ['-p', '(version 1) (allow default) (deny network*)',
    binary, path.join(matrix, manifestName), ...extra],
    {env: {PATH: '/usr/bin:/bin'}, encoding: 'utf8', timeout: 120000, maxBuffer: 1024 * 1024});
  assert.equal(result.status, 0, 'grader execution failed; exit zero alone never means a passing quality verdict');
  return {binary_sha256: sha(fs.readFileSync(binary)), report: JSON.parse(result.stdout)};
}
const comparison = grade('echowall-local-quality-eval', ['--policy', 'miaoji-relative-v2']);
const coverage = grade('echowall-speaker-coverage', []);
const report = {schema_version: 1, scope: 'completed-case-snapshot-not-whole-batch-or-release-acceptance',
  selected_stratum: stratum, selected_cases: selected.length, failed_attempts_in_scope: failedAttempts,
  all44_included: selected.length === 44, manifest: manifestName,
  policy: declaration.policy, worker_sha256: declaration.worker_sha256,
  provenance, comparison, lexical_assignment: coverage};
publish(path.join(scores, `${snapshotId}-${stratum}.json`), report);
// No per-case identifiers, paths, terms, text, or model outputs in stdout.
const {provenance: omitted, manifest: omittedManifest, ...aggregate} = report;
console.log(JSON.stringify(aggregate));
}

try { main(); }
catch {
  // Match the grader's aggregate-only boundary on failures too; never dump
  // Node assertion diffs, reference values, source paths or private stack traces.
  console.error(JSON.stringify({state: 'failed', code: 'snapshot_input_or_grader_rejected'}));
  process.exitCode = 1;
}
