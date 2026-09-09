// Filesystem boundary shared by explicit local QA preparation/evidence tools.
// Reject pre-existing redirections, not a sandbox against hostile same-uid races.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';

function plainDirectory(directory) {
  const metadata = fs.lstatSync(directory);
  assert.ok(metadata.isDirectory() && !metadata.isSymbolicLink(), 'QA directory must be plain');
  assert.equal(fs.realpathSync(directory), directory, 'QA directory must be canonical');
}

export function createQaRoot(root) {
  assert.equal(path.resolve(root), root, 'absolute canonical QA root spelling');
  assert.equal(path.basename(root), 'ai.ax.watch-transcriber.qa.moss');
  const parent = path.dirname(root);
  plainDirectory(parent); // Must precede the very first mkdir or marker write.
  fs.mkdirSync(root, {mode: 0o700});
  plainDirectory(root);
  assert.equal(fs.lstatSync(root).mode & 0o077, 0, 'private QA root');
}

export function validateQaTree(root) {
  plainDirectory(root);
  assert.equal(fs.lstatSync(root).mode & 0o077, 0, 'private QA root');
  const pending = [[root, 0]];
  let count = 0;
  while (pending.length) {
    const [directory, depth] = pending.pop();
    assert.ok(depth <= 64, 'bounded QA depth');
    for (const entry of fs.readdirSync(directory, {withFileTypes: true})) {
      assert.ok(++count <= 100000, 'bounded QA entries');
      if (entry.isSymbolicLink()) {
        const file = path.join(directory, entry.name);
        const parts = path.relative(root, file).split(path.sep);
        assert.ok(parts.length === 4 && parts[0] === 'data' && parts[1] === 'by-topic', 'QA child must not redirect through a symlink');
        // Preserve existing publisher-created by-topic leaf aliases only.
        const target = fs.realpathSync(file);
        assert.ok(target.startsWith(path.join(root, 'data') + path.sep) && fs.lstatSync(target).isFile(), 'topic alias must remain a regular file inside this QA archive');
        continue;
      }
      if (entry.isDirectory()) pending.push([path.join(directory, entry.name), depth + 1]);
      else assert.ok(entry.isFile(), 'QA child must not be a special file');
    }
  }
}

export function ensureQaDirectory(root, relative) {
  plainDirectory(root);
  const components = relative.split('/');
  assert.ok(components.length && components.every(value => /^[a-zA-Z0-9_.-]+$/.test(value) && !['.', '..'].includes(value)));
  let directory = root;
  for (const component of components) {
    directory = path.join(directory, component);
    try { fs.mkdirSync(directory, {mode: 0o700}); }
    catch (error) { if (error.code !== 'EEXIST') throw error; }
    plainDirectory(directory);
  }
  return directory;
}
