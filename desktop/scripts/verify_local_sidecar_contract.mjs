#!/usr/bin/env node
// Static packaging metadata only: no Cargo invocation, model I/O or download.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const DESKTOP = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REVISION = '63a44d9239d610b3908e8a66b384924cd4a77217';
const REPOSITORY = 'https://github.com/handy-computer/transcribe.cpp';
const WORKERS = ['whisper', 'diarization', 'summary', 'qwen', 'moss'];
const LICENSES = new Map([
  ['transcribe.cpp-LICENSE', '86a53633b56f6b029d3cb42158bcc7aac0cdff898aceb13e83b93e368bbc4ac6'],
  ['transcribe.cpp-THIRD-PARTY-LICENSES.md', '6b55185de37d7a24be30e2db7f2fd41270f73d5609eefc073592ecb2a527c85c'],
]);

export function verifyLocalSidecarContract(desktop = DESKTOP) {
  const read = relative => readFileSync(resolve(desktop, relative), 'utf8');
  const local = JSON.parse(read('src-tauri/tauri.local.conf.json'));
  const base = JSON.parse(read('src-tauri/tauri.conf.json'));
  assert.deepEqual(local.bundle.externalBin, WORKERS.map(name => `binaries/echowall-${name}-worker`), 'fixed five-worker inventory');
  assert.equal(local.bundle.macOS.hardenedRuntime, true, 'hardened runtime');
  assert.equal(local.bundle.macOS.entitlements ?? null, null, 'no additional worker entitlements');
  assert.equal(base.bundle.externalBin?.includes('binaries/echowall-moss-worker') ?? false, false, 'MOSS stays out of Windows/mobile base config');
  assert.equal(local.build.beforeBuildCommand, './scripts/build_local_sidecars.sh');
  for (const [name, hash] of LICENSES) {
    const source = `../moss-worker/ThirdPartyLicenses/${name}`;
    assert.equal(local.bundle.resources[source], `licenses/${name}`, 'native license resource');
    assert.equal(createHash('sha256').update(read(`moss-worker/ThirdPartyLicenses/${name}`)).digest('hex'), hash, 'pinned native license text');
  }

  const manifest = read('moss-worker/Cargo.toml');
  const dependency = [...manifest.matchAll(/^transcribe-cpp\s*=\s*(\{[^\n]+\})\s*$/gm)];
  assert.equal(dependency.length, 1, 'single closed native dependency');
  // The owned dependency is an inline table; reject unsupported syntax instead
  // of guessing at TOML. Its strings, bool and array are JSON-compatible values.
  const native = JSON.parse(dependency[0][1].replace(/([a-z][a-z-]*)\s*=/g, '"$1":'));
  assert.deepEqual(native, { git: REPOSITORY, rev: REVISION, 'default-features': false, features: ['metal'] }, 'exact native revision and backend');
  const nativeSection = manifest.slice(0, dependency[0].index).match(/^\[[^\n]+\]$/gm)?.at(-1);
  assert.equal(nativeSection, '[target.\'cfg(all(target_os = "macos", target_arch = "aarch64"))\'.dependencies]', 'native dependency is arm64-macOS only');
  const packages = read('moss-worker/Cargo.lock').split('[[package]]').map(block => ({
    name: block.match(/^name = "([^"]+)"$/m)?.[1],
    version: block.match(/^version = "([^"]+)"$/m)?.[1],
    source: block.match(/^source = "([^"]+)"$/m)?.[1],
  }));
  for (const name of ['transcribe-cpp', 'transcribe-cpp-sys']) {
    const matches = packages.filter(value => value.name === name);
    assert.equal(matches.length, 1, 'single locked native package');
    assert.deepEqual(matches[0], { name, version: '0.2.3', source: `git+${REPOSITORY}?rev=${REVISION}#${REVISION}` }, 'native lock identity');
  }
  const rustProtocol = read('local-whisper-protocol/src/lib.rs');
  const swiftProtocol = read('diarization-worker/Sources/EchoWallDiarizationWorker/main.swift');
  const swiftSpeakerPresets = read('diarization-worker/Sources/EchoWallDiarizationWorker/SpeakerKitWorkerPreset.swift');
  const ownerEnv = 'ECHOWALL_WORKER_PARENT_PID';
  assert.ok(read('local-worker-support/parent_guard.rs').includes(`pub const APP_PARENT_ENV: &str = "${ownerEnv}"`), 'shared Rust App-owner contract');
  assert.ok(read('diarization-worker/Sources/EchoWallDiarizationWorker/ParentLifetimeGuard.swift').includes(`["${ownerEnv}"]`), 'Swift App-owner contract');
  assert.match(read('src-tauri/src/processing/moss_worker.rs'), /\.env\("ECHOWALL_WORKER_PARENT_PID", std::process::id\(\)\.to_string\(\)\)/, 'launcher binds its own PID after clearing environment');
  assert.ok(read('moss-worker/src/lib.rs').includes('parent_guard::bind_from_environment()'), 'MOSS lifetime monitor');
  assert.ok(read('summary-worker/src/lib.rs').includes('parent_guard::bind_from_environment()'), 'summary lifetime monitor');
  assert.ok(swiftProtocol.includes('ParentLifetimeGuard.bindFromEnvironment()'), 'diarizer lifetime monitor');
  const protocolVersion = rustProtocol.match(/^pub const LOCAL_DIARIZATION_PROTOCOL_VERSION: u32 = (\d+);$/m)?.[1];
  assert.equal(protocolVersion, '2', 'review diarization protocol changes before packaging');
  assert.equal(swiftProtocol.match(/^private let protocolVersion = (\d+)$/m)?.[1], protocolVersion, 'Swift/Rust diarization protocol agreement');
  for (const [rustName, swiftName, expected] of [
    ['LOCAL_DIARIZATION_QUALITY_PRESET', 'selectedQualityPreset', 'fluid-step015-embed040-v1'],
    ['LOCAL_DIARIZATION_LEGACY_PRESET', 'legacyQualityPreset', 'fluid-community-v1'],
  ]) {
    assert.equal(rustProtocol.match(new RegExp(`^pub const ${rustName}: &str = "([^"]+)";$`, 'm'))?.[1], expected, 'Rust diarization preset');
    assert.equal(swiftProtocol.match(new RegExp(`^private let ${swiftName} = "([^"]+)"$`, 'm'))?.[1], expected, 'Swift diarization preset');
  }
  for (const [rustName, swiftCase, expected] of [
    ['LOCAL_DIARIZATION_SPEAKERKIT_PRESET', 'exclusiveV1', 'speakerkit-pyannote-v3-exclusive-v1'],
    ['LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET', 'tailContextV2', 'speakerkit-pyannote-v3-exclusive-tail-context-v2'],
  ]) {
    assert.equal(rustProtocol.match(new RegExp(`^pub const ${rustName}: &str =\\s*"([^"]+)";$`, 'm'))?.[1], expected, 'Rust SpeakerKit preset');
    assert.equal(swiftSpeakerPresets.match(new RegExp(`^\\s*case ${swiftCase} = "([^"]+)"$`, 'm'))?.[1], expected, 'Swift SpeakerKit preset');
  }
  return { workerCount: WORKERS.length, nativeRevision: REVISION, arm64InferenceOnly: true, additionalEntitlements: false, diarizationProtocolVersion: Number(protocolVersion) };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  if (process.argv.length !== 2) {
    process.stderr.write('usage: verify_local_sidecar_contract.mjs\n');
    process.exitCode = 2;
  } else {
    try {
      process.stdout.write(`${JSON.stringify(verifyLocalSidecarContract())}\n`);
    } catch {
      process.stderr.write('local sidecar catalog, lock, platform or signing contract drifted\n');
      process.exitCode = 1;
    }
  }
}
