import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { verifyLocalSidecarContract } from './verify_local_sidecar_contract.mjs';

const DESKTOP = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WORKERS = ['whisper', 'diarization', 'summary', 'qwen', 'moss'].map(name => `echowall-${name}-worker`);
const METADATA_FILES = [
  'src-tauri/tauri.local.conf.json', 'src-tauri/tauri.conf.json',
  'moss-worker/Cargo.toml', 'moss-worker/Cargo.lock',
  'local-whisper-protocol/src/lib.rs',
  'local-worker-support/parent_guard.rs', 'moss-worker/src/lib.rs', 'summary-worker/src/lib.rs',
  'src-tauri/src/processing/moss_worker.rs',
  'diarization-worker/Sources/EchoWallDiarizationWorker/ParentLifetimeGuard.swift',
  'diarization-worker/Sources/EchoWallDiarizationWorker/main.swift',
  'diarization-worker/Sources/EchoWallDiarizationWorker/SpeakerKitWorkerPreset.swift',
  'moss-worker/ThirdPartyLicenses/transcribe.cpp-LICENSE',
  'moss-worker/ThirdPartyLicenses/transcribe.cpp-THIRD-PARTY-LICENSES.md',
  'scripts/build_local_sidecars.sh', 'scripts/verify_local_sidecar_contract.mjs',
];

function temporary(t) {
  const path = mkdtempSync(join(tmpdir(), 'echowall-macos-sidecar-test-'));
  t.after(() => rmSync(path, { recursive: true, force: true }));
  return path;
}

function metadataFixture(t) {
  const root = temporary(t);
  for (const relative of METADATA_FILES) {
    const destination = join(root, relative);
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(join(DESKTOP, relative), destination);
  }
  return root;
}

test('closed native lock, five fixed sidecars and explicit signing policy agree', () => {
  assert.deepEqual(verifyLocalSidecarContract(), {
    workerCount: 5, nativeRevision: '63a44d9239d610b3908e8a66b384924cd4a77217',
    arm64InferenceOnly: true, additionalEntitlements: false,
    diarizationProtocolVersion: 2,
  });
});

test('native pin, Metal feature, license, inventory and Windows config drift fail closed', t => {
  const mutations = [
    ['moss-worker/Cargo.toml', text => text.replace('63a44d9239d610b3908e8a66b384924cd4a77217', '0'.repeat(40))],
    ['moss-worker/Cargo.toml', text => text.replace('features = ["metal"]', 'features = ["metal", "cuda"]')],
    ['moss-worker/Cargo.toml', text => text.replace('target_arch = "aarch64"', 'target_arch = "x86_64"')],
    ['moss-worker/Cargo.lock', text => text.replace('#63a44d9239d610b3908e8a66b384924cd4a77217', `#${'0'.repeat(40)}`)],
    ['moss-worker/ThirdPartyLicenses/transcribe.cpp-LICENSE', text => `${text}\naltered\n`],
    ['src-tauri/tauri.local.conf.json', text => text.replace('binaries/echowall-moss-worker', 'binaries/echowall-other-worker')],
    ['src-tauri/tauri.local.conf.json', text => text.replace('"hardenedRuntime": true', '"hardenedRuntime": false')],
    ['src-tauri/tauri.conf.json', text => { const data = JSON.parse(text); data.bundle.externalBin = ['binaries/echowall-moss-worker']; return JSON.stringify(data); }],
    ['diarization-worker/Sources/EchoWallDiarizationWorker/main.swift', text => text.replace('private let protocolVersion = 2', 'private let protocolVersion = 1')],
    ['diarization-worker/Sources/EchoWallDiarizationWorker/SpeakerKitWorkerPreset.swift', text => text.replace('speakerkit-pyannote-v3-exclusive-v1', 'old-preset')],
    ['diarization-worker/Sources/EchoWallDiarizationWorker/SpeakerKitWorkerPreset.swift', text => text.replace('speakerkit-pyannote-v3-exclusive-tail-context-v2', 'old-tail-preset')],
    ['diarization-worker/Sources/EchoWallDiarizationWorker/ParentLifetimeGuard.swift', text => text.replace('ECHOWALL_WORKER_PARENT_PID', 'WRONG_OWNER')],
    ['summary-worker/src/lib.rs', text => text.replace('parent_guard::bind_from_environment()', 'missing_owner()')],
  ];
  for (const [file, mutate] of mutations) {
    const root = metadataFixture(t);
    const path = join(root, file);
    writeFileSync(path, mutate(readFileSync(path, 'utf8')));
    assert.throws(() => verifyLocalSidecarContract(root), file);
  }
});

test('dry-run lists both MOSS slices and universal output without invoking any build or filesystem command', t => {
  const root = metadataFixture(t);
  const guards = join(root, 'guard-tools');
  mkdirSync(guards);
  for (const command of ['cargo', 'swift', 'xcrun', 'install', 'mkdir', 'mv', 'chmod', 'strings', 'otool', 'nm', 'cmp']) {
    const path = join(guards, command);
    writeFileSync(path, '#!/bin/sh\necho "unexpected mutating/build command" >&2\nexit 99\n');
    chmodSync(path, 0o755);
  }
  const result = spawnSync('/bin/sh', [join(root, 'scripts/build_local_sidecars.sh'), '--dry-run'], {
    encoding: 'utf8', timeout: 10_000,
    env: { PATH: `${guards}:${dirname(process.execPath)}:/usr/bin:/bin` },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal((result.stdout.match(/cargo build --locked[^\n]+moss-worker\/Cargo.toml/g) ?? []).length, 2);
  assert.match(result.stdout, /echowall-moss-worker-universal-apple-darwin -verify_arch arm64 x86_64/);
  assert.match(result.stdout, /require explicit unsupported_platform marker in Intel MOSS stub/);
  assert.match(result.stdout, /dry-run: cmp -s[^\n]+echowall-diarization-worker-aarch64-apple-darwin/);
  assert.equal((result.stdout.match(/verify_diarization_capabilities\.sh/g) ?? []).length, 3);
  assert.match(result.stdout, /-Xswiftc -gnone -Xcc -g0 -Xcxx -g0/, 'release Swift, C and C++ objects must omit builder debug paths');
  assert.equal((result.stdout.match(/dry-run: xcrun lipo -create/g) ?? []).length, 5);
  assert.equal(existsSync(join(root, 'src-tauri/binaries')), false);
});

// Fake tools operate on text-only disposable bundles. They never compile,
// sign, notarize, launch a real Mach-O, or consult a signing keychain.
const TOOL_FIXTURE = `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const tool = path.basename(process.argv[1]);
const args = process.argv.slice(2);
const input = args.at(-1);
const isMoss = path.basename(input ?? '') === 'echowall-moss-worker';
const fault = process.env.ECHOWALL_FIXTURE_FAULT;
if (tool === 'strings') process.stdout.write(fs.readFileSync(input));
else if (tool === 'otool') {
  if (isMoss && fault === 'inspection') process.exit(1);
  process.stdout.write(isMoss && fault === 'network-framework' ? '/System/Library/Frameworks/Network.framework/Network\\n' : '/usr/lib/libSystem.B.dylib\\n');
} else if (tool === 'nm') process.stdout.write(isMoss && fault === 'network-symbol' ? '_connect\\n' : '_malloc\\n');
else if (tool === 'xcrun') {
  if (args[0] !== 'lipo' || args[1] !== '-archs') process.exit(2);
  process.stdout.write(isMoss && fault === 'architecture' ? 'x86_64\\n' : 'arm64 x86_64\\n');
} else if (tool === 'codesign') {
  if (isMoss && fault === 'signature' && args.includes('--verify')) process.exit(1);
  if (args.includes('--verbose=4')) process.stdout.write(isMoss && fault === 'runtime' ? 'flags=0x0(none)\\n' : 'flags=0x10000(runtime)\\n');
  if (isMoss && fault === 'entitlement' && args.includes('--entitlements')) process.stdout.write('<plist version="1.0"><dict><key>com.apple.security.cs.allow-jit</key><true/></dict></plist>');
} else process.exit(2);
`;

function bundleFixture(t) {
  const root = temporary(t);
  const app = join(root, 'EchoWall.app');
  const macos = join(app, 'Contents/MacOS');
  const licenses = join(app, 'Contents/Resources/licenses');
  const tools = join(root, 'tools');
  mkdirSync(macos, { recursive: true }); mkdirSync(licenses, { recursive: true }); mkdirSync(tools);
  for (const name of ['desktop', ...WORKERS]) {
    writeFileSync(join(macos, name), name === 'echowall-diarization-worker'
      ? 'schema_version quality_preset speakerkit-pyannote-v3-exclusive-v1 speakerkit-pyannote-v3-exclusive-tail-context-v2 fluid-step015-embed040-v1 fluid-community-v1 ECHOWALL_WORKER_PARENT_PID\n'
      : 'fabricated binary marker ECHOWALL_WORKER_PARENT_PID\n');
    chmodSync(join(macos, name), 0o755);
  }
  writeFileSync(join(app, 'Contents/Info.plist'), '<plist version="1.0"><dict><key>CFBundleExecutable</key><string>desktop</string></dict></plist>');
  for (const name of ['transcribe.cpp-LICENSE', 'transcribe.cpp-THIRD-PARTY-LICENSES.md']) {
    copyFileSync(join(DESKTOP, 'moss-worker/ThirdPartyLicenses', name), join(licenses, name));
  }
  for (const name of ['strings', 'otool', 'nm', 'xcrun', 'codesign']) {
    writeFileSync(join(tools, name), TOOL_FIXTURE);
    chmodSync(join(tools, name), 0o755);
  }
  return { app, macos, licenses, tools };
}

function verifyBundle(fixture, fault = '') {
  return spawnSync('/bin/sh', [join(DESKTOP, 'scripts/verify_macos_bundle_privacy.sh'), fixture.app], {
    encoding: 'utf8', timeout: 20_000,
    env: { PATH: `${fixture.tools}:${dirname(process.execPath)}:/usr/bin:/bin`, ECHOWALL_FIXTURE_FAULT: fault },
  });
}

test('privacy/signing gate accepts exact fixture and rejects MOSS network, architecture and signing failures', { skip: process.platform !== 'darwin' }, t => {
  const fixture = bundleFixture(t);
  let result = verifyBundle(fixture);
  assert.equal(result.status, 0, result.stderr);
  for (const fault of ['network-framework', 'network-symbol', 'architecture', 'signature', 'runtime', 'entitlement', 'inspection']) {
    result = verifyBundle(fixture, fault);
    assert.notEqual(result.status, 0, fault);
    assert.doesNotMatch(result.stdout, /verification passed/, fault);
  }
});

test('privacy gate rejects count-preserving substitution, symlinks, models, missing license and builder paths', { skip: process.platform !== 'darwin' }, t => {
  const mutations = [
    fixture => { rmSync(join(fixture.macos, 'echowall-moss-worker')); writeFileSync(join(fixture.macos, 'echowall-other-worker'), 'replacement'); },
    fixture => writeFileSync(join(fixture.macos, 'echowall-extra-worker'), 'unexpected'),
    fixture => { rmSync(join(fixture.macos, 'echowall-moss-worker')); symlinkSync('echowall-qwen-worker', join(fixture.macos, 'echowall-moss-worker')); },
    fixture => writeFileSync(join(fixture.app, 'Contents/Resources/model.gguf'), 'forbidden'),
    fixture => rmSync(join(fixture.licenses, 'transcribe.cpp-LICENSE')),
    fixture => writeFileSync(join(fixture.licenses, 'transcribe.cpp-LICENSE'), 'wrong notice'),
    fixture => writeFileSync(join(fixture.macos, 'echowall-diarization-worker'), 'old signed diarization fixture'),
    fixture => writeFileSync(join(fixture.macos, 'echowall-moss-worker'), '/Users/fabricated-builder/private-source'),
    fixture => writeFileSync(join(fixture.macos, 'desktop'), 'echowall-isolated-qa-v1'),
  ];
  for (const mutate of mutations) {
    const fixture = bundleFixture(t);
    mutate(fixture);
    const result = verifyBundle(fixture);
    assert.notEqual(result.status, 0, basename(fixture.app));
    assert.doesNotMatch(result.stdout, /verification passed/);
  }
});
