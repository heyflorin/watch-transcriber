import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {validateQaTree, ensureQaDirectory, createQaRoot} from './moss_qa_paths.mjs';

function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'echowall-qa-path-test-')));
  t.after(() => fs.rmSync(root, {recursive: true}));
  return root;
}

test('QA first creation rejects a redirected parent before creating root or marker', t => {
  const parent = fixture(t);
  const outside = path.join(parent, 'external'); fs.mkdirSync(outside);
  const sentinel = path.join(outside, 'sentinel'); fs.writeFileSync(sentinel, 'preserved');
  const redirectedParent = path.join(parent, 'redirected'); fs.symlinkSync(outside, redirectedParent);
  const name = 'ai.ax.watch-transcriber.qa.moss';
  assert.throws(() => createQaRoot(path.join(redirectedParent, name)));
  assert.deepEqual(fs.readdirSync(outside), ['sentinel']);
  assert.equal(fs.readFileSync(sentinel, 'utf8'), 'preserved');
  const root = path.join(parent, name); createQaRoot(root); validateQaTree(root);
  assert.equal(fs.statSync(root).mode & 0o077, 0);
});

test('QA path boundary creates only canonical private child components', t => {
  const root = fixture(t);
  const child = ensureQaDirectory(root, 'models/summary/pinned-model');
  assert.equal(fs.statSync(child).mode & 0o077, 0);
  validateQaTree(root);
  for (const relative of ['../outside', '/tmp', 'data/../outside', 'data//outside', './data']) {
    assert.throws(() => ensureQaDirectory(root, relative));
  }
});

test('QA tree and immediate writer reject archive/inbox/model/evidence redirection without changing sentinel', t => {
  for (const relative of ['data', 'inbox', 'models/summary', 'processing/jobs', 'qa-evidence']) {
    const parent = fixture(t);
    const root = path.join(parent, 'qa-root'); fs.mkdirSync(root, {mode: 0o700});
    const external = path.join(parent, 'external'); fs.mkdirSync(external, {mode: 0o700});
    const sentinel = path.join(external, 'sentinel'); fs.writeFileSync(sentinel, 'preserved');
    const link = path.join(root, relative);
    fs.mkdirSync(path.dirname(link), {recursive: true});
    fs.symlinkSync(external, link);
    assert.throws(() => validateQaTree(root), relative);
    assert.throws(() => ensureQaDirectory(root, relative + '/new-child'), relative);
    assert.deepEqual(fs.readdirSync(external), ['sentinel']);
    assert.equal(fs.readFileSync(sentinel, 'utf8'), 'preserved');
  }
});

test('QA permits only same-archive regular-file topic aliases, not external files or directories', t => {
  const parent = fixture(t);
  const root = path.join(parent, 'qa-root'); fs.mkdirSync(root, {mode: 0o700});
  const topic = ensureQaDirectory(root, 'data/by-topic/topic');
  const canonical = ensureQaDirectory(root, 'data/2026-09-05');
  fs.writeFileSync(path.join(canonical, 'note.md'), 'retained note');
  const alias = path.join(topic, 'note.md');
  fs.symlinkSync('../../2026-09-05/note.md', alias);
  validateQaTree(root);
  fs.unlinkSync(alias); fs.symlinkSync(canonical, alias);
  assert.throws(() => validateQaTree(root));
  fs.unlinkSync(alias);
  const sentinel = path.join(parent, 'external.md'); fs.writeFileSync(sentinel, 'external sentinel');
  fs.symlinkSync(sentinel, alias);
  assert.throws(() => validateQaTree(root));
  assert.equal(fs.readFileSync(sentinel, 'utf8'), 'external sentinel');
  assert.equal(fs.readFileSync(path.join(canonical, 'note.md'), 'utf8'), 'retained note');
});
