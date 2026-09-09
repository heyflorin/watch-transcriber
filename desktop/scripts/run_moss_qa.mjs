#!/usr/bin/env node
// Real App launcher/preparation for one public recording. No production
// credentials, recording directory, automatic model download or App install.
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import {spawn, spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
import {validateQaTree, ensureQaDirectory, createQaRoot} from './moss_qa_paths.mjs';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const id = 'ai.ax.watch-transcriber.qa.moss';
const root = path.join(os.homedir(), 'Library/Application Support', id);
const marker = 'echowall-isolated-qa-fixture-v1';
const audioHash = '22c340e2193a29a14ecfcda5d6e7c0010b9d7efbbc3e4d0fc9ac374a2969fc8c';
const audio = path.join(repo, 'local-eval/matrix/audio/english_01.m4a');
// Fixed retained public fixtures for the assembled-App language smoke. These
// extend the isolated UI launcher, not the production import allowlist.
const uiFixtures = [
  {name:'english_01',sha256:audioHash},
  {name:'mandarin_01',sha256:'5de1c7b124943fa1b87088e688066c68e5746b431f70c76c737b3d781e17ac04'},
  {name:'mixed_01',sha256:'b4315116d932b569c4d8871e5ceeb2c138eb0add8d08521d0627b233348d439d'},
];
const allowedAudioHashes = new Set(uiFixtures.map(fixture=>fixture.sha256));
const args = process.argv.slice(2);
if (args[0] === '--help' || !args.length) {
  console.log('usage: node scripts/run_moss_qa.mjs --prepare | --check | --launch <local-QA.app> | --launch-public <local-QA.app> | --launch-public-retry <local-QA.app>');
  process.exit(args.length ? 0 : 2);
}
const mode = args[0];
assert.ok(['--prepare','--check','--launch','--launch-public','--launch-public-retry'].includes(mode));
assert.equal(args.length, mode.startsWith('--launch') ? 2 : 1);

function hash(file) {
  const digest = crypto.createHash('sha256');
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try { const buffer = Buffer.alloc(1024*1024); let size;
    while ((size=fs.readSync(fd,buffer)) > 0) digest.update(buffer.subarray(0,size));
  } finally { fs.closeSync(fd); }
  return digest.digest('hex');
}
function checked(file) {
  const resolved = path.resolve(file);
  assert.equal(fs.realpathSync(resolved),resolved,'symlinks are not QA inputs');
  assert.ok(fs.lstatSync(resolved).isFile());
  return resolved;
}
function json(relative) { return JSON.parse(fs.readFileSync(path.join(repo,relative),'utf8')); }
function writeNew(file, value) {
  const fd = fs.openSync(file,'wx',0o600);
  try { fs.writeFileSync(fd,JSON.stringify(value,null,2)+'\n'); fs.fsyncSync(fd); }
  finally { fs.closeSync(fd); }
}
function validateRoot() {
  validateQaTree(root);
  const claim = JSON.parse(fs.readFileSync(checked(path.join(root,'qa-fixture.json')),'utf8'));
  assert.equal(claim.marker,marker); assert.equal(claim.identifier,id); assert.equal(claim.source_sha256,audioHash);
}
function validateRecordings() {
  const inbox = path.join(root,'inbox');
  if (!fs.existsSync(inbox)) return;
  for (const entry of fs.readdirSync(inbox,{withFileTypes:true})) {
    assert.ok(!entry.isSymbolicLink());
    if (!entry.isDirectory()) continue;
    const pkg=path.join(inbox,entry.name);
    const recording=JSON.parse(fs.readFileSync(checked(path.join(pkg,'recording.json')),'utf8'));
    assert.ok(allowedAudioHashes.has(recording.normalized_sha256),'only allowlisted public recordings may resume');
    const input=path.resolve(pkg,recording.normalized_audio);
    assert.ok(input.startsWith(pkg+path.sep));
    assert.equal(hash(checked(input)),recording.normalized_sha256);
  }
}

if (mode === '--prepare') {
  assert.equal(hash(checked(audio)),audioHash);
  if (fs.existsSync(root)) validateRoot();
  else {
    createQaRoot(root);
    writeNew(path.join(root,'qa-fixture.json'),{marker,identifier:id,source_sha256:audioHash});
  }
  validateRecordings();
  for (const fixture of uiFixtures) {
    const source=checked(path.join(repo,'local-eval/matrix/audio',`${fixture.name}.m4a`));
    assert.equal(hash(source),fixture.sha256);
    const publicCopy=path.join(root,`public-${fixture.name}.m4a`);
    if (!fs.existsSync(publicCopy)) fs.linkSync(source,publicCopy);
    assert.equal(hash(checked(publicCopy)),fixture.sha256);
  }
  ensureQaDirectory(root,'data');
  const manifest=path.join(root,'data/manifest.json');
  if (!fs.existsSync(manifest)) writeNew(manifest,{});
  const moss=json('desktop/model-catalog/moss-candidate-v1.json');
  const speakers=json('desktop/model-catalog/speakerkit-candidate-v1.json');
  const full=json('desktop/model-catalog/full-local-v1.json');
  const files=[
    ...moss.files.map(file=>({...file,source:'local-eval/models/moss-transcribe-diarize-q8/MOSS-Transcribe-Diarize-Q8_0.gguf'})),
    ...speakers.files.map(file=>({...file,source:'local-eval/speakerkit-worker-root/'+file.install_path})),
    ...full.files.filter(file=>file.install_path.startsWith('models/summary/')).map(file=>({...file,source:'local-eval/model-root/'+file.install_path})),
  ];
  assert.equal(files.length,31);
  for (const file of files) {
    assert.ok(file.install_path.startsWith('models/') && !file.install_path.includes('..'));
    const source=checked(path.join(repo,file.source));
    assert.equal(fs.statSync(source).size,file.size_bytes);
    assert.equal(hash(source),file.sha256,'pinned public model identity');
    const destination=path.join(root,file.install_path);
    if (fs.existsSync(destination)) {
      assert.equal(hash(checked(destination)),file.sha256,'never replace an existing conflicting model');
    } else {
      ensureQaDirectory(root,path.posix.dirname(file.install_path));
      // Read-only public model bytes; never chmod or modify either hard link.
      fs.linkSync(source,destination);
    }
  }
  console.log(JSON.stringify({prepared:true,root,model_files:files.length,downloaded:false,credentials_seeded:false}));
  process.exit(0);
}

validateRoot(); validateRecordings();
// A loopback-allowing outer App sandbox prevents its worker from applying the
// different production deny-all profile (sandbox_apply EPERM). Do not weaken
// the worker profile or claim an application-level HTTP guard is an OS trace.
// Dedicated build namespaces, a hash-allowlisted input, and empty environment
// protect this runtime test; the QA binary denies App HTTP before connection.
console.log(JSON.stringify({fixture_verified:true,root,os_sandbox_for_app:false,
  isolation:'build-only-data-and-keychain-namespaces',http:'QA-connector-deny',
  worker_sandbox:'unchanged-production-deny-network',release_offline_proof:false}));
if (mode === '--check') process.exit(0);

const bundle=path.resolve(args[1]);
assert.ok(bundle.startsWith(path.join(repo,'local-eval')+path.sep),'launch only a retained local QA copy');
assert.equal(fs.realpathSync(bundle),bundle);
const plist=spawnSync('/usr/bin/plutil',['-extract','CFBundleIdentifier','raw','-o','-',path.join(bundle,'Contents/Info.plist')],{encoding:'utf8'});
assert.equal(plist.status,0); assert.equal(plist.stdout.trim(),id);
const executable=checked(path.join(bundle,'Contents/MacOS/desktop'));
const strings=spawnSync('/usr/bin/strings',['-a',executable],{encoding:'utf8',maxBuffer:32*1024*1024});
assert.equal(strings.status,0); assert.ok(strings.stdout.includes('echowall-isolated-qa-v1'));
assert.ok(strings.stdout.includes('echowall-isolated-qa-http-denied-v1'),'refuse old QA builds without HTTP guard');
assert.equal(spawnSync('/usr/bin/codesign',['--verify','--deep','--strict',bundle],{stdio:'ignore'}).status,0);
const env={PATH:'/usr/bin:/bin:/usr/sbin:/sbin',OS_ACTIVITY_MODE:'disable',
  ECHOWALL_MOSS_CANDIDATE_ENABLED:'1',ECHOWALL_RECORDING_ENABLED:'0',ECHOWALL_LOCAL_STT_ENABLED:'1'};
if (mode === '--launch-public') env.ECHOWALL_QA_RUN_PUBLIC='english_01';
if (mode === '--launch-public-retry') env.ECHOWALL_QA_RUN_PUBLIC='english_01_retry';
// Preserve the actual OS environment locations, never substitute a fake home.
for (const key of ['HOME','TMPDIR','LANG']) if(process.env[key]) env[key]=process.env[key];
const child=spawn(executable,[],{env,stdio:['ignore','pipe','pipe']});
const evidence=ensureQaDirectory(root,'qa-evidence');
const logFile=path.join(evidence,`app-${child.pid}-${crypto.randomUUID()}.log`);
const log=fs.openSync(logFile,'wx',0o600); let logged=0;
const capture=bytes=>{const part=bytes.subarray(0,Math.max(0,65536-logged));if(part.length){fs.writeSync(log,part);logged+=part.length;}};
child.stdout.on('data',capture); child.stderr.on('data',capture); // Bounded private evidence, never console content.
child.once('close',()=>fs.closeSync(log));
console.log(JSON.stringify({launched:true,pid:child.pid,bundle,root,log_file:logFile,scope:'isolated-real-app-not-release'}));
process.on('SIGINT',()=>child.kill('SIGTERM'));
process.on('SIGTERM',()=>child.kill('SIGTERM'));
child.on('exit',(code,signal)=>{console.log(JSON.stringify({app_exited:true,code,signal}));process.exitCode=code??1;});
