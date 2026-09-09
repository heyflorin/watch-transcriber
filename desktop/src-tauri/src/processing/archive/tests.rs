use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::json;

use super::*;

#[test]
fn legacy_structured_action_items_are_strictly_normalized_without_reprompting() {
    let normalized = action_item_strings(Some(&json!([
        "Canonical string",
        {"task": "Verify archive", "owner": "Synthetic owner", "due_date": "Friday"}
    ])))
    .unwrap();
    assert_eq!(
        normalized,
        [
            "Canonical string",
            "Verify archive (owner: Synthetic owner; due: Friday)"
        ]
    );
    for invalid in [
        json!([{"task": ""}]),
        json!([{"task": "x", "url": "https://untrusted.example"}]),
        json!([{"task": ["wrong"]}]),
    ] {
        assert!(action_item_strings(Some(&invalid)).is_err());
    }
}

#[derive(Default)]
struct FakeGitState {
    head: String,
    commits: HashMap<String, (String, String)>,
    blobs: HashMap<String, Vec<u8>>,
    manifest_blobs: HashMap<String, String>,
    tree_entries: HashMap<String, Vec<(String, String)>>,
    counter: usize,
    patch_count: usize,
    commit_count: usize,
    conflict_once: bool,
    lose_patch_response_once: bool,
    requests: Vec<String>,
    tree_included_index: bool,
    concurrent_manifest: Option<BTreeMap<String, Value>>,
    deleted_paths: Vec<String>,
}

struct FakeGit {
    state: Mutex<FakeGitState>,
}

impl FakeGit {
    fn new() -> Self {
        let mut state = FakeGitState {
            head: "head-0".to_owned(),
            ..FakeGitState::default()
        };
        state.commits.insert(
            "head-0".to_owned(),
            ("tree-0".to_owned(), "prior".to_owned()),
        );
        Self {
            state: Mutex::new(state),
        }
    }

    fn response(status: u16, value: Value) -> HttpFuture {
        Box::pin(async move {
            Ok(ArchiveHttpResponse {
                status,
                body: serde_json::to_vec(&value).unwrap(),
            })
        })
    }
}

impl ArchiveHttpTransport for FakeGit {
    fn execute(&self, request: ArchiveHttpRequest) -> HttpFuture {
        let mut state = self.state.lock().unwrap();
        assert!(request
            .url
            .starts_with("https://api.github.com/repos/owner/private/"));
        assert!(request.response_limit <= MAX_GITHUB_RESPONSE_BYTES);
        assert!(!format!("{request:?}").contains("test-secret"));
        let path = request
            .url
            .split("/owner/private/")
            .nth(1)
            .unwrap()
            .to_owned();
        state.requests.push(path.clone());
        match (request.method, path.as_str()) {
            (ArchiveMethod::Get, "git/ref/heads/main") => {
                Self::response(200, json!({"object": {"sha": state.head}}))
            }
            (ArchiveMethod::Get, path) if path.starts_with("git/commits/") => {
                let sha = path.trim_start_matches("git/commits/");
                let (tree, message) = state.commits.get(sha).unwrap().clone();
                Self::response(200, json!({"tree": {"sha": tree}, "message": message}))
            }
            (ArchiveMethod::Get, path) if path.starts_with("git/trees/") => {
                let tree = path
                    .trim_start_matches("git/trees/")
                    .trim_end_matches("?recursive=1");
                let mut entries = state
                    .manifest_blobs
                    .get(tree)
                    .map(|sha| vec![json!({"path": "manifest.json", "type": "blob", "mode": "100644", "sha": sha})])
                    .unwrap_or_default();
                entries.extend(
                    state
                        .tree_entries
                        .get(tree)
                        .into_iter()
                        .flatten()
                        .map(|(path, sha)| json!({"path": path, "type": "blob", "mode": "100644", "sha": sha})),
                );
                Self::response(200, json!({"tree": entries}))
            }
            (ArchiveMethod::Get, path) if path.starts_with("git/blobs/") => {
                let sha = path.trim_start_matches("git/blobs/");
                let content = state.blobs.get(sha).unwrap();
                Self::response(
                    200,
                    json!({"encoding": "base64", "content": base64(content)}),
                )
            }
            (ArchiveMethod::Post, "git/blobs") => {
                let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
                let bytes = decode_base64(body["content"].as_str().unwrap()).unwrap();
                let sha = git_blob_sha(&bytes);
                state.blobs.insert(sha.clone(), bytes);
                Self::response(201, json!({"sha": sha}))
            }
            (ArchiveMethod::Post, "git/trees") => {
                let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
                let base_tree = body["base_tree"].as_str().unwrap();
                state.tree_included_index = body["tree"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["path"] == "index.html");
                state.deleted_paths.extend(
                    body["tree"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|entry| entry["sha"].is_null())
                        .filter_map(|entry| entry["path"].as_str().map(str::to_owned)),
                );
                let manifest_blob = body["tree"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["path"] == "manifest.json")
                    .and_then(|entry| entry["sha"].as_str())
                    .unwrap()
                    .to_owned();
                let mut entries: HashMap<String, String> = state
                    .tree_entries
                    .get(base_tree)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect();
                for entry in body["tree"].as_array().unwrap() {
                    let path = entry["path"].as_str().unwrap();
                    if path == "manifest.json" {
                        continue;
                    }
                    if let Some(sha) = entry["sha"].as_str() {
                        entries.insert(path.to_owned(), sha.to_owned());
                    } else {
                        entries.remove(path);
                    }
                }
                let mut entries: Vec<_> = entries.into_iter().collect();
                entries.sort();
                state.counter += 1;
                let tree = format!("tree-{}", state.counter);
                state.manifest_blobs.insert(tree.clone(), manifest_blob);
                state.tree_entries.insert(tree.clone(), entries);
                Self::response(201, json!({"sha": tree}))
            }
            (ArchiveMethod::Post, "git/commits") => {
                let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
                let message = body["message"].as_str().unwrap().to_owned();
                let tree = body["tree"].as_str().unwrap().to_owned();
                let parent = body["parents"][0].as_str().unwrap();
                assert_eq!(parent, state.head);
                state.counter += 1;
                state.commit_count += 1;
                let sha = format!("commit-{}", state.counter);
                state.commits.insert(sha.clone(), (tree, message));
                Self::response(201, json!({"sha": sha}))
            }
            (ArchiveMethod::Patch, "git/refs/heads/main") => {
                let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
                assert_eq!(body["force"], false);
                state.patch_count += 1;
                if state.conflict_once {
                    state.conflict_once = false;
                    if let Some(manifest) = state.concurrent_manifest.take() {
                        let manifest_bytes = serialize_manifest(&manifest).unwrap();
                        let blob = git_blob_sha(&manifest_bytes);
                        state.blobs.insert(blob.clone(), manifest_bytes);
                        state
                            .manifest_blobs
                            .insert("external-tree".to_owned(), blob);
                        let prior_tree = state.commits[&state.head].0.clone();
                        let mut entries: HashMap<String, String> = state
                            .tree_entries
                            .get(&prior_tree)
                            .into_iter()
                            .flatten()
                            .cloned()
                            .collect();
                        for note in manifest
                            .values()
                            .filter_map(|entry| entry.get("note").and_then(Value::as_str))
                        {
                            if !entries.contains_key(note) {
                                let bytes = b"# Remote\n\n## Summary\n\nremote\n".to_vec();
                                let note_blob = git_blob_sha(&bytes);
                                state.blobs.insert(note_blob.clone(), bytes);
                                entries.insert(note.to_owned(), note_blob);
                            }
                        }
                        let mut entries: Vec<_> = entries.into_iter().collect();
                        entries.sort();
                        state
                            .tree_entries
                            .insert("external-tree".to_owned(), entries);
                    }
                    state.head = "external-head".to_owned();
                    state.commits.insert(
                        "external-head".to_owned(),
                        ("external-tree".to_owned(), "concurrent".to_owned()),
                    );
                    return Self::response(409, json!({}));
                }
                state.head = body["sha"].as_str().unwrap().to_owned();
                if state.lose_patch_response_once {
                    state.lose_patch_response_once = false;
                    return Box::pin(async { Err(ArchiveError::network()) });
                }
                Self::response(200, json!({"object": {"sha": state.head}}))
            }
            _ => panic!("unexpected GitHub request: {:?} {path}", request.method),
        }
    }
}

#[derive(Default)]
struct FakeR2State {
    objects: HashMap<String, R2PutProof>,
    put_calls: usize,
    corrupt_put: bool,
    corrupt_head: bool,
    delete_calls: usize,
    fail_delete_once: bool,
}

#[derive(Default)]
struct FakeR2 {
    state: Mutex<FakeR2State>,
}

struct HangingR2;

impl ArchiveR2 for HangingR2 {
    fn destination_identity(&self) -> String {
        "fake/hanging".to_owned()
    }

    fn put_verified(&self, _: String, _: String, _: PathBuf, _: String, _: u64) -> R2Future {
        Box::pin(std::future::pending())
    }

    fn head_verified(&self, _: String, _: String, _: String, _: u64) -> R2Future {
        Box::pin(std::future::pending())
    }

    fn delete_owned(&self, _: String, _: String, _: u64, _: String, _: u64) -> R2DeleteFuture {
        Box::pin(std::future::pending())
    }
}

impl ArchiveR2 for FakeR2 {
    fn destination_identity(&self) -> String {
        "fake/audio".to_owned()
    }

    fn put_verified(
        &self,
        key: String,
        recording_id: String,
        source: PathBuf,
        sha256: String,
        size_bytes: u64,
    ) -> R2Future {
        let actual = hash_file(&source).unwrap();
        assert_eq!(actual, (sha256.clone(), size_bytes));
        let mut state = self.state.lock().unwrap();
        if let Some(existing) = state.objects.get(&key) {
            if existing.recording_id == recording_id
                && existing.sha256 == sha256
                && existing.size_bytes == size_bytes
            {
                let existing = existing.clone();
                return Box::pin(async move { Ok(existing) });
            }
            return Box::pin(async { Err(ArchiveError::conflict()) });
        }
        state.put_calls += 1;
        let proof = R2PutProof {
            key: key.clone(),
            recording_id,
            version_id: format!("version-{}", state.put_calls),
            etag: format!("etag-{}", state.put_calls),
            sha256,
            size_bytes,
        };
        let mut returned = proof.clone();
        if state.corrupt_put {
            returned.sha256 = "e".repeat(64);
        }
        state.objects.insert(key, proof.clone());
        Box::pin(async move { Ok(returned) })
    }

    fn head_verified(
        &self,
        key: String,
        recording_id: String,
        sha256: String,
        size_bytes: u64,
    ) -> R2Future {
        let state = self.state.lock().unwrap();
        let Some(mut proof) = state.objects.get(&key).cloned() else {
            return Box::pin(async { Err(ArchiveError::verification()) });
        };
        if proof.recording_id != recording_id
            || proof.sha256 != sha256
            || proof.size_bytes != size_bytes
        {
            return Box::pin(async { Err(ArchiveError::verification()) });
        }
        if state.corrupt_head {
            proof.sha256 = "f".repeat(64);
        }
        Box::pin(async move { Ok(proof) })
    }

    fn delete_owned(
        &self,
        key: String,
        recording_id: String,
        _: u64,
        sha256: String,
        size_bytes: u64,
    ) -> R2DeleteFuture {
        let mut state = self.state.lock().unwrap();
        state.delete_calls += 1;
        if state.fail_delete_once {
            state.fail_delete_once = false;
            return Box::pin(async { Err(ArchiveError::network()) });
        }
        let outcome = match state.objects.get(&key) {
            Some(proof)
                if proof.recording_id == recording_id
                    && proof.sha256 == sha256
                    && proof.size_bytes == size_bytes =>
            {
                state.objects.remove(&key);
                r2::R2DeleteOutcome::Deleted
            }
            Some(_) => return Box::pin(async { Err(ArchiveError::verification()) }),
            None => r2::R2DeleteOutcome::AlreadyMissing,
        };
        Box::pin(async move { Ok(outcome) })
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    archive: PathBuf,
    inbox: PathBuf,
    git: Arc<FakeGit>,
    r2: Arc<FakeR2>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("archive");
        let inbox = temp.path().join("inbox");
        fs::create_dir_all(&archive).unwrap();
        fs::create_dir_all(&inbox).unwrap();
        Self {
            _temp: temp,
            archive,
            inbox,
            git: Arc::new(FakeGit::new()),
            r2: Arc::new(FakeR2::default()),
        }
    }

    fn adapter(&self) -> ArchiveAdapter<FakeGit, FakeR2> {
        ArchiveAdapter::with_components(
            self.archive.clone(),
            self.inbox.clone(),
            "owner/private".to_owned(),
            "test-secret".to_owned(),
            Arc::clone(&self.git),
            Arc::clone(&self.r2),
        )
        .unwrap()
    }

    fn recording(&self, recording_id: Uuid) -> (RecordingEnvelope, NormalizedArtifactCheckpoint) {
        let bytes = b"RIFFfabricated-audio-only";
        let sha256 = sha256_hex(bytes);
        let directory = self.inbox.join(recording_id.to_string()).join("derived");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("mixed.wav"), bytes).unwrap();
        let envelope: RecordingEnvelope = serde_json::from_value(json!({
            "schema_version": 1,
            "recording_id": recording_id,
            "source": {
                "kind": "desktop_voice_memo",
                "platform": "macos",
                "label": "Synthetic",
                "capture_scope": "microphone"
            },
            "captured_at": "2026-09-02T12:00:00.987-07:00",
            "ended_at": "2026-09-02T12:00:01.987-07:00",
            "duration_ms": 1000,
            "tracks": [{
                "role": "microphone",
                "relative_path": "derived/mixed.wav",
                "codec": "wav_pcm_s16le",
                "sample_rate": 16000,
                "channels": 1,
                "duration_ms": 1000,
                "clock_start_ns": 0,
                "sha256": sha256
            }],
            "normalized_audio": "derived/mixed.wav",
            "normalized_sha256": sha256,
            "imported_name": null,
            "capture_warnings": [],
            "job": {"state": "ready", "attempt": 0, "remote_job_id": null, "last_error": null}
        }))
        .unwrap();
        let artifact = NormalizedArtifactCheckpoint {
            relative_path: "derived/mixed.wav".to_owned(),
            sha256,
            size_bytes: bytes.len() as u64,
        };
        (envelope, artifact)
    }

    fn summary(title: &str) -> Value {
        json!({
            "title": title,
            "category": "工作商务",
            "summary": "合成摘要",
            "key_points": ["One"],
            "todos": ["Test"]
        })
    }
}

#[tokio::test]
async fn local_archive_completion_is_hash_verified_and_makes_no_remote_effect() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    let plan = adapter
        .plan(&envelope, PublicationBackend::LocalArchive)
        .unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].id, LOCAL_TARGET_ID);

    let proof = adapter
        .publish(
            LOCAL_TARGET_ID,
            1,
            &envelope,
            &json!([{
                "speaker": {"id": "local_speaker_01"},
                "content": "fabricated local transcript",
                "start_time": 0,
                "end_time": 1000
            }]),
            &Fixture::summary("Offline"),
            &artifact,
        )
        .await
        .unwrap();
    assert!(proof.locator.starts_with("local:"));
    let backup = adapter
        .verify_backup(PublicationBackend::LocalArchive, 1, &envelope, &artifact)
        .await
        .unwrap();
    assert!(backup.locator.starts_with("local:"));
    assert_eq!(backup.sha256, artifact.sha256);
    assert_eq!(backup.size_bytes, artifact.size_bytes);

    let manifest = load_manifest(&fixture.archive).unwrap();
    let entry = manifest
        .values()
        .find(|entry| entry["recording_id"] == envelope.recording_id.to_string())
        .unwrap();
    assert!(entry.get("r2_key").is_none());
    assert!(entry.get("r2_generation").is_none());
    assert!(entry["audio"]
        .as_str()
        .is_some_and(|relative| { fixture.archive.join(relative).is_file() }));
    assert!(fixture.git.state.lock().unwrap().requests.is_empty());
    assert_eq!(fixture.r2.state.lock().unwrap().put_calls, 0);
}

#[tokio::test]
async fn offline_first_run_initializes_an_empty_viewer_without_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("archive");
    let inbox = temp.path().join("inbox");
    let sync = temp.path().join("sync");
    fs::create_dir_all(&archive).unwrap();
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&sync).unwrap();
    let deferred = DeferredArchive::new(archive.clone(), inbox, sync).unwrap();
    deferred.initialize_local().await.unwrap();
    assert_eq!(
        fs::read_to_string(archive.join("manifest.json")).unwrap(),
        "{}\n"
    );
    let index = fs::read_to_string(archive.join("index.html")).unwrap();
    assert!(!index.is_empty());
    assert!(index.contains(
        "const nightPercent = DATA.entries.length ? Math.round(night / DATA.entries.length * 100) : 0;"
    ));
    assert!(index.contains("<b>${nightPercent}%</b> 录于凌晨"));
    assert!(fs::metadata(archive.join("marked.min.js")).unwrap().len() > 0);
    deferred.initialize_local().await.unwrap();
    assert_eq!(
        fs::read_to_string(archive.join("manifest.json")).unwrap(),
        "{}\n"
    );
}

#[tokio::test]
async fn same_second_collision_advances_only_archive_key_and_sanitizes_paths() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (first, first_artifact) = fixture.recording(Uuid::new_v4());
    let (second, second_artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &first,
            &json!({"text": "fixture"}),
            &Fixture::summary("First / unsafe:* title"),
            &first_artifact,
        )
        .await
        .unwrap();
    adapter
        .publish(
            TARGET_ID,
            1,
            &second,
            &json!({"text": "fixture"}),
            &Fixture::summary("Second"),
            &second_artifact,
        )
        .await
        .unwrap();

    let manifest = load_manifest(&fixture.archive).unwrap();
    assert!(manifest.contains_key("2026-09-02 120000"));
    assert!(manifest.contains_key("2026-09-02 120001"));
    assert_eq!(
        manifest["2026-09-02 120001"]["captured_at"],
        "2026-09-02T12:00:00.987-07:00"
    );
    let note = manifest["2026-09-02 120000"]["note"].as_str().unwrap();
    let filename = Path::new(note).file_name().unwrap().to_str().unwrap();
    assert!(!filename.contains(['/', ':', '*']));
    assert!(fixture.archive.join("index.html").is_file());
}

#[tokio::test]
async fn reprocess_preserves_user_fields_and_removes_owned_stale_paths() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "one"}),
            &Fixture::summary("Old title"),
            &artifact,
        )
        .await
        .unwrap();
    let mut manifest = load_manifest(&fixture.archive).unwrap();
    let entry = manifest
        .values_mut()
        .next()
        .unwrap()
        .as_object_mut()
        .unwrap();
    let old_note = entry["note"].as_str().unwrap().to_owned();
    let old_r2_key = entry["r2_key"].as_str().unwrap().to_owned();
    let attachment = "2026-09-02/120000-attachments/user.md";
    fs::create_dir_all(fixture.archive.join("2026-09-02/120000-attachments")).unwrap();
    fs::write(fixture.archive.join(attachment), "user context").unwrap();
    entry.insert("speakers".to_owned(), json!({"S1": "Alice"}));
    entry.insert("speakers_applied".to_owned(), json!(true));
    entry.insert("attachments".to_owned(), json!([attachment]));
    atomic_write(
        &fixture.archive.join("manifest.json"),
        &serialize_manifest(&manifest).unwrap(),
    )
    .unwrap();

    adapter
        .publish(
            TARGET_ID,
            2,
            &envelope,
            &json!({"text": "two"}),
            &Fixture::summary("New title"),
            &artifact,
        )
        .await
        .unwrap();
    let manifest = load_manifest(&fixture.archive).unwrap();
    let entry = manifest.values().next().unwrap();
    assert_eq!(entry["speakers"]["S1"], "Alice");
    assert_eq!(entry["speakers_applied"], true);
    assert_eq!(entry["attachments"], json!([attachment]));
    assert_eq!(entry["publish_generation"], 2);
    assert_eq!(entry["r2_generation"], 1);
    assert_eq!(entry["r2_key"], old_r2_key);
    assert_eq!(fixture.r2.state.lock().unwrap().put_calls, 1);
    assert!(!fixture.archive.join(old_note).exists());
}

#[tokio::test]
async fn reprocess_replaces_a_missing_prior_backup_without_reuploading_to_tos() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "one"}),
            &Fixture::summary("Old title"),
            &artifact,
        )
        .await
        .unwrap();
    let old_key = load_manifest(&fixture.archive).unwrap()["2026-09-02 120000"]["r2_key"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.r2.state.lock().unwrap().objects.remove(&old_key);

    adapter
        .publish(
            TARGET_ID,
            2,
            &envelope,
            &json!({"text": "one"}),
            &Fixture::summary("New title"),
            &artifact,
        )
        .await
        .unwrap();

    let manifest = load_manifest(&fixture.archive).unwrap();
    let entry = &manifest["2026-09-02 120000"];
    assert_eq!(entry["publish_generation"], 2);
    assert_eq!(entry["r2_generation"], 2);
    assert_ne!(entry["r2_key"], old_key);
    assert_eq!(fixture.r2.state.lock().unwrap().put_calls, 2);
}

#[tokio::test]
async fn github_ref_conflict_rebases_with_non_force_cas() {
    let fixture = Fixture::new();
    let remote_id = Uuid::new_v4();
    let mut remote_manifest = BTreeMap::new();
    remote_manifest.insert(
        "2026-09-02 120000".to_owned(),
        json!({
            "recording_id": remote_id,
            "title": "Remote",
            "note": "2026-09-02/120000-remote.md",
            "audio": "2026-09-02/120000-remote.wav",
            "captured_at": "2026-09-02T12:00:00-07:00",
            "speakers": {"SPEAKER_1": "Remote User"}
        }),
    );
    {
        let mut state = fixture.git.state.lock().unwrap();
        state.conflict_once = true;
        state.concurrent_manifest = Some(remote_manifest);
    }
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!([{
                "speaker": {"id": "1"},
                "content": "fixture",
                "start_time": 0,
                "end_time": 1000
            }]),
            &Fixture::summary("Conflict"),
            &artifact,
        )
        .await
        .unwrap();
    let state = fixture.git.state.lock().unwrap();
    assert_eq!(state.patch_count, 2);
    assert_eq!(state.commit_count, 2);
    assert!(!state.tree_included_index);
    let manifest_blob = state
        .manifest_blobs
        .get(&state.commits.get(&state.head).unwrap().0)
        .unwrap();
    let published: BTreeMap<String, Value> =
        serde_json::from_slice(state.blobs.get(manifest_blob).unwrap()).unwrap();
    let remote = published
        .values()
        .find(|entry| entry["recording_id"] == remote_id.to_string())
        .unwrap();
    assert_eq!(remote["speakers"]["SPEAKER_1"], "Remote User");
    let (local_key, local) = published
        .iter()
        .find(|(_, entry)| entry["recording_id"] == envelope.recording_id.to_string())
        .unwrap();
    let audio = local["audio"].as_str().unwrap();
    let r2_key = local["r2_key"].as_str().unwrap();
    assert_eq!(local_key, "2026-09-02 120001");
    assert_eq!(audio, "2026-09-02/120001-Conflict.wav");
    assert!(r2_key.starts_with("2026-09-02/120000-recording-"));
    assert!(r2_key.ends_with(&format!("-g1-{}.wav", envelope.recording_id)));
    let tree = &state.commits[&state.head].0;
    assert!(state.tree_entries[tree]
        .iter()
        .any(|(path, _)| path == ".gitignore"));
    assert!(!state.tree_entries[tree]
        .iter()
        .any(|(path, _)| path == "index.html"));
    assert!(!state
        .deleted_paths
        .contains(&"2026-09-02/120000-remote.md".to_owned()));
    drop(state);
    let r2 = fixture.r2.state.lock().unwrap();
    assert_eq!(r2.put_calls, 1);
    assert_eq!(r2.objects.len(), 1);
    drop(r2);
    assert!(fs::read_to_string(fixture.archive.join("index.html"))
        .unwrap()
        .contains(&remote_id.to_string()));
}

#[tokio::test]
async fn occupied_r2_key_checkpoints_a_deterministic_fallback_without_overwrite() {
    let fixture = Fixture::new();
    let recording_id = Uuid::new_v4();
    let other_id = Uuid::new_v4();
    let (envelope, artifact) = fixture.recording(recording_id);
    let occupied_key = format!(
        "2026-09-02/120000-recording-{}-g1-{recording_id}.wav",
        &artifact.sha256[..16]
    );
    fixture.r2.state.lock().unwrap().objects.insert(
        occupied_key.clone(),
        R2PutProof {
            key: occupied_key.clone(),
            recording_id: other_id.to_string(),
            version_id: "other-version".to_owned(),
            etag: "other-etag".to_owned(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        },
    );

    fixture
        .adapter()
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Occupied"),
            &artifact,
        )
        .await
        .unwrap();

    let state = fixture.r2.state.lock().unwrap();
    assert_eq!(
        state.objects[&occupied_key].recording_id,
        other_id.to_string()
    );
    let own = state
        .objects
        .values()
        .find(|proof| proof.recording_id == recording_id.to_string())
        .unwrap();
    assert!(own.key.contains(&recording_id.to_string()));
    assert_ne!(own.key, occupied_key);
    assert_eq!(state.put_calls, 1);
    assert_eq!(state.objects.len(), 2);
}

#[tokio::test]
async fn restart_reconciles_lost_ref_response_without_duplicate_commit() {
    let fixture = Fixture::new();
    fixture.git.state.lock().unwrap().lose_patch_response_once = true;
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    let first = fixture
        .adapter()
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Restart"),
            &artifact,
        )
        .await;
    assert_eq!(first.unwrap_err().kind, EffectErrorKind::Temporary);
    let commits = fixture.git.state.lock().unwrap().commit_count;

    fixture
        .adapter()
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Restart"),
            &artifact,
        )
        .await
        .unwrap();
    assert_eq!(fixture.git.state.lock().unwrap().commit_count, commits);
}

#[tokio::test]
async fn lost_ref_response_reconciles_after_a_descendant_commit() {
    let fixture = Fixture::new();
    fixture.git.state.lock().unwrap().lose_patch_response_once = true;
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    let first = fixture
        .adapter()
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Interleaved"),
            &artifact,
        )
        .await;
    assert_eq!(first.unwrap_err().kind, EffectErrorKind::Temporary);
    let (commits, puts) = {
        let mut state = fixture.git.state.lock().unwrap();
        let published_head = state.head.clone();
        let published_tree = state.commits[&published_head].0.clone();
        state.commits.insert(
            "descendant-head".to_owned(),
            (published_tree, "another recording".to_owned()),
        );
        state.head = "descendant-head".to_owned();
        (
            state.commit_count,
            fixture.r2.state.lock().unwrap().put_calls,
        )
    };

    let proof = fixture
        .adapter()
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Interleaved"),
            &artifact,
        )
        .await
        .unwrap();
    assert_eq!(proof.version, "descendant-head");
    assert_eq!(fixture.git.state.lock().unwrap().commit_count, commits);
    assert_eq!(fixture.r2.state.lock().unwrap().put_calls, puts);
}

#[tokio::test]
async fn native_user_edit_uses_github_cas_without_git_or_r2_reupload() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!([{
                "speaker": {"id": "1"},
                "content": "fixture",
                "start_time": 0,
                "end_time": 1000
            }]),
            &Fixture::summary("Native Edit"),
            &artifact,
        )
        .await
        .unwrap();
    let key = load_manifest(&fixture.archive)
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let mut manifest = load_manifest(&fixture.archive).unwrap();
    manifest
        .get_mut(&key)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("speakers".to_owned(), json!({"SPEAKER_1": "Alice"}));
    atomic_write(
        &fixture.archive.join("manifest.json"),
        &serialize_manifest(&manifest).unwrap(),
    )
    .unwrap();
    let r2_puts = fixture.r2.state.lock().unwrap().put_calls;

    adapter
        .publish_native_edit(NativeArchiveEdit::UserFields {
            deltas: vec![ArchiveEntryDelta {
                key: key.clone(),
                recording_id: Some(envelope.recording_id.to_string()),
                base_entry_sha256: None,
                speaker_slots: vec![SpeakerSlotDelta {
                    slot: "SPEAKER_1".to_owned(),
                    expected: None,
                    desired: Some("Alice".to_owned()),
                }],
                add_attachments: Vec::new(),
            }],
        })
        .await
        .unwrap();
    assert_eq!(fixture.r2.state.lock().unwrap().put_calls, r2_puts);
    {
        let state = fixture.git.state.lock().unwrap();
        let tree = &state.commits[&state.head].0;
        let manifest_blob = &state.manifest_blobs[tree];
        let remote: BTreeMap<String, Value> =
            serde_json::from_slice(&state.blobs[manifest_blob]).unwrap();
        assert_eq!(
            remote.values().next().unwrap()["speakers"]["SPEAKER_1"],
            "Alice"
        );
        let note_blob = state.tree_entries[tree]
            .iter()
            .find(|(path, _)| path.ends_with(".md"))
            .map(|(_, sha)| sha)
            .unwrap();
        assert!(std::str::from_utf8(&state.blobs[note_blob])
            .unwrap()
            .contains("] Alice:"));
    }

    let mut manifest = load_manifest(&fixture.archive).unwrap();
    manifest
        .get_mut(&key)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("speakers");
    atomic_write(
        &fixture.archive.join("manifest.json"),
        &serialize_manifest(&manifest).unwrap(),
    )
    .unwrap();
    apply_speaker_labels(&fixture.archive, std::slice::from_ref(&key)).unwrap();
    adapter
        .publish_native_edit(NativeArchiveEdit::UserFields {
            deltas: vec![ArchiveEntryDelta {
                key,
                recording_id: Some(envelope.recording_id.to_string()),
                base_entry_sha256: None,
                speaker_slots: vec![SpeakerSlotDelta {
                    slot: "SPEAKER_1".to_owned(),
                    expected: Some("Alice".to_owned()),
                    desired: None,
                }],
                add_attachments: Vec::new(),
            }],
        })
        .await
        .unwrap();
    let state = fixture.git.state.lock().unwrap();
    let tree = &state.commits[&state.head].0;
    let manifest_blob = &state.manifest_blobs[tree];
    let remote: BTreeMap<String, Value> =
        serde_json::from_slice(&state.blobs[manifest_blob]).unwrap();
    let entry = remote.values().next().unwrap();
    assert!(entry.get("speakers").is_none());
    assert!(entry.get("speakers_applied").is_none());
    let note_blob = state.tree_entries[tree]
        .iter()
        .find(|(path, _)| path.ends_with(".md"))
        .map(|(_, sha)| sha)
        .unwrap();
    assert!(std::str::from_utf8(&state.blobs[note_blob])
        .unwrap()
        .contains("] SPEAKER_1:"));
}

#[tokio::test]
async fn native_edit_lost_response_reconciles_after_descendant_commit() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Edit Retry"),
            &artifact,
        )
        .await
        .unwrap();
    fs::write(
        fixture.archive.join("speakers.json"),
        br##"{"Alice":"#123456"}"##,
    )
    .unwrap();
    fixture.git.state.lock().unwrap().lose_patch_response_once = true;
    let first = adapter
        .publish_native_edit(NativeArchiveEdit::SpeakerColors {
            name: "Alice".to_owned(),
            expected: None,
            desired: "#123456".to_owned(),
        })
        .await;
    assert_eq!(first.unwrap_err().kind, ArchiveErrorKind::Network);
    let commits = {
        let mut state = fixture.git.state.lock().unwrap();
        let published_head = state.head.clone();
        let published_tree = state.commits[&published_head].0.clone();
        state.commits.insert(
            "edit-descendant".to_owned(),
            (published_tree, "other edit".to_owned()),
        );
        state.head = "edit-descendant".to_owned();
        state.commit_count
    };

    let proof = adapter
        .publish_native_edit(NativeArchiveEdit::SpeakerColors {
            name: "Alice".to_owned(),
            expected: None,
            desired: "#123456".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(proof.version, "edit-descendant");
    assert_eq!(fixture.git.state.lock().unwrap().commit_count, commits);
}

#[tokio::test]
async fn native_delete_publishes_manifest_and_tree_removal_with_cas() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Native Delete"),
            &artifact,
        )
        .await
        .unwrap();
    let key = load_manifest(&fixture.archive)
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let deleted = delete_local_recording(&fixture.archive, &key).unwrap();
    adapter
        .publish_native_edit(NativeArchiveEdit::Delete {
            key,
            recording_id: deleted.recording_id,
            base_entry_sha256: Some(deleted.base_entry_sha256),
            deleted_paths: deleted.deleted,
            r2_delete: None,
        })
        .await
        .unwrap();

    let state = fixture.git.state.lock().unwrap();
    let tree = &state.commits[&state.head].0;
    let manifest_blob = &state.manifest_blobs[tree];
    let remote: BTreeMap<String, Value> =
        serde_json::from_slice(&state.blobs[manifest_blob]).unwrap();
    assert!(remote.is_empty());
    assert_eq!(state.deleted_paths.len(), 1);
    assert!(state.deleted_paths[0].ends_with("Native-Delete.md"));
    assert!(!state.tree_entries[tree]
        .iter()
        .any(|(path, _)| path.ends_with("Native-Delete.md")));
}

#[tokio::test]
async fn delete_cas_conflict_restores_local_manifest_viewer_and_tombstone() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Delete Conflict"),
            &artifact,
        )
        .await
        .unwrap();
    let manifest = load_manifest(&fixture.archive).unwrap();
    let key = manifest.keys().next().unwrap().clone();
    let planned = plan_local_recording_delete(&fixture.archive, &key).unwrap();
    let before_manifest = fs::read(fixture.archive.join("manifest.json")).unwrap();
    let before_viewer = fs::read(fixture.archive.join("index.html")).unwrap();
    let mut concurrent = manifest;
    concurrent
        .get_mut(&key)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("category".to_owned(), Value::String("学习认知".to_owned()));
    {
        let mut state = fixture.git.state.lock().unwrap();
        state.conflict_once = true;
        state.concurrent_manifest = Some(concurrent);
    }
    let error = adapter
        .publish_native_edit(NativeArchiveEdit::Delete {
            key,
            recording_id: planned.recording_id,
            base_entry_sha256: Some(planned.base_entry_sha256),
            deleted_paths: planned.deleted,
            r2_delete: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, ArchiveErrorKind::Conflict);
    assert_eq!(
        fs::read(fixture.archive.join("manifest.json")).unwrap(),
        before_manifest
    );
    assert_eq!(
        fs::read(fixture.archive.join("index.html")).unwrap(),
        before_viewer
    );
    assert!(!fixture.archive.join(DELETED_RECORDINGS_PATH).exists());
    assert_eq!(fixture.r2.state.lock().unwrap().delete_calls, 0);
}

#[tokio::test]
async fn delete_prepare_is_side_effect_free_before_remote_cas() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Delete Crash"),
            &artifact,
        )
        .await
        .unwrap();
    let manifest = load_manifest(&fixture.archive).unwrap();
    let key = manifest.keys().next().unwrap().clone();
    let planned = plan_local_recording_delete(&fixture.archive, &key).unwrap();
    let manifest_before = fs::read(fixture.archive.join("manifest.json")).unwrap();
    let viewer_before = fs::read(fixture.archive.join("index.html")).unwrap();
    let ids = std::collections::HashSet::from([envelope.recording_id.to_string()]);
    let head = adapter
        .fetch_github_head(&ids, &std::collections::HashSet::new())
        .await
        .unwrap();
    let prepared = adapter
        .prepare_native_edit(
            &NativeArchiveEdit::Delete {
                key,
                recording_id: planned.recording_id,
                base_entry_sha256: Some(planned.base_entry_sha256),
                deleted_paths: planned.deleted,
                r2_delete: None,
            },
            &head.remote,
        )
        .unwrap();
    let prepared_manifest: BTreeMap<String, Value> =
        serde_json::from_slice(&prepared.manifest).unwrap();
    assert!(prepared_manifest.is_empty());
    assert_eq!(
        fs::read(fixture.archive.join("manifest.json")).unwrap(),
        manifest_before
    );
    assert_eq!(
        fs::read(fixture.archive.join("index.html")).unwrap(),
        viewer_before
    );
    assert!(!fixture.archive.join(DELETED_RECORDINGS_PATH).exists());
}

#[tokio::test]
async fn native_speaker_colors_merge_distinct_names_and_conflict_on_same_name() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Colors"),
            &artifact,
        )
        .await
        .unwrap();

    fs::write(
        fixture.archive.join("speakers.json"),
        br##"{"Bob":"#112233"}"##,
    )
    .unwrap();
    adapter
        .publish_native_edit(NativeArchiveEdit::SpeakerColors {
            name: "Bob".to_owned(),
            expected: None,
            desired: "#112233".to_owned(),
        })
        .await
        .unwrap();

    fs::write(
        fixture.archive.join("speakers.json"),
        br##"{"Alice":"#445566"}"##,
    )
    .unwrap();
    adapter
        .publish_native_edit(NativeArchiveEdit::SpeakerColors {
            name: "Alice".to_owned(),
            expected: None,
            desired: "#445566".to_owned(),
        })
        .await
        .unwrap();

    {
        let state = fixture.git.state.lock().unwrap();
        let tree = &state.commits[&state.head].0;
        let speaker_sha = state.tree_entries[tree]
            .iter()
            .find(|(path, _)| path == "speakers.json")
            .map(|(_, sha)| sha)
            .unwrap();
        let colors: Value = serde_json::from_slice(&state.blobs[speaker_sha]).unwrap();
        assert_eq!(colors["Bob"], "#112233");
        assert_eq!(colors["Alice"], "#445566");
    }

    fs::write(
        fixture.archive.join("speakers.json"),
        br##"{"Alice":"#abcdef"}"##,
    )
    .unwrap();
    let error = adapter
        .publish_native_edit(NativeArchiveEdit::SpeakerColors {
            name: "Alice".to_owned(),
            expected: None,
            desired: "#abcdef".to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, ArchiveErrorKind::Conflict);
}

#[tokio::test]
async fn legacy_native_edit_reconciles_a_lost_patch_after_descendant_commit() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Legacy Edit"),
            &artifact,
        )
        .await
        .unwrap();
    let mut local = load_manifest(&fixture.archive).unwrap();
    let key = local.keys().next().unwrap().clone();
    let local_entry = local.get_mut(&key).unwrap().as_object_mut().unwrap();
    for field in [
        "recording_id",
        "publish_generation",
        "r2_generation",
        "r2_key",
        "audio_sha256",
        "audio_size_bytes",
    ] {
        local_entry.remove(field);
    }
    let base = archive_entry_sha256(local.get(&key).unwrap()).unwrap();
    atomic_write(
        &fixture.archive.join("manifest.json"),
        &serialize_manifest(&local).unwrap(),
    )
    .unwrap();
    {
        let mut state = fixture.git.state.lock().unwrap();
        let tree = state.commits[&state.head].0.clone();
        let prior_sha = state.manifest_blobs[&tree].clone();
        let mut remote: BTreeMap<String, Value> =
            serde_json::from_slice(&state.blobs[&prior_sha]).unwrap();
        let remote_entry = remote.get_mut(&key).unwrap().as_object_mut().unwrap();
        for field in [
            "recording_id",
            "publish_generation",
            "r2_generation",
            "r2_key",
            "audio_sha256",
            "audio_size_bytes",
        ] {
            remote_entry.remove(field);
        }
        let bytes = serialize_manifest(&remote).unwrap();
        let sha = git_blob_sha(&bytes);
        state.blobs.insert(sha.clone(), bytes);
        state.manifest_blobs.insert(tree, sha);
        state.lose_patch_response_once = true;
    }
    let edit = NativeArchiveEdit::UserFields {
        deltas: vec![ArchiveEntryDelta {
            key: key.clone(),
            recording_id: None,
            base_entry_sha256: Some(base),
            speaker_slots: vec![SpeakerSlotDelta {
                slot: "SPEAKER_1".to_owned(),
                expected: None,
                desired: Some("Alice".to_owned()),
            }],
            add_attachments: Vec::new(),
        }],
    };
    assert_eq!(
        adapter
            .publish_native_edit(edit.clone())
            .await
            .unwrap_err()
            .kind,
        ArchiveErrorKind::Network
    );
    {
        let mut state = fixture.git.state.lock().unwrap();
        let published_head = state.head.clone();
        let published_tree = state.commits[&published_head].0.clone();
        state.commits.insert(
            "legacy-descendant".to_owned(),
            (published_tree, "other edit".to_owned()),
        );
        state.head = "legacy-descendant".to_owned();
    }
    let proof = adapter.publish_native_edit(edit).await.unwrap();
    assert_eq!(proof.version, "legacy-descendant");
}

#[tokio::test]
async fn content_addressed_attachments_merge_without_same_name_overwrite() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Attachments"),
            &artifact,
        )
        .await
        .unwrap();
    let key = load_manifest(&fixture.archive)
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let (first_path, first) =
        prepare_local_attachment(&fixture.archive, &key, "context", "first").unwrap();
    let (second_path, second) =
        prepare_local_attachment(&fixture.archive, &key, "context", "second").unwrap();
    assert_ne!(first_path, second_path);
    adapter
        .publish_native_edit(NativeArchiveEdit::UserFields {
            deltas: vec![first],
        })
        .await
        .unwrap();
    adapter
        .publish_native_edit(NativeArchiveEdit::UserFields {
            deltas: vec![second],
        })
        .await
        .unwrap();
    let state = fixture.git.state.lock().unwrap();
    let tree = &state.commits[&state.head].0;
    let manifest_sha = &state.manifest_blobs[tree];
    let remote: BTreeMap<String, Value> =
        serde_json::from_slice(&state.blobs[manifest_sha]).unwrap();
    let attachments = remote[&key]["attachments"].as_array().unwrap();
    assert!(attachments.iter().any(|value| value == &first_path));
    assert!(attachments.iter().any(|value| value == &second_path));
    let first_sha = state.tree_entries[tree]
        .iter()
        .find(|(path, _)| path == &first_path)
        .map(|(_, sha)| sha)
        .unwrap();
    let second_sha = state.tree_entries[tree]
        .iter()
        .find(|(path, _)| path == &second_path)
        .map(|(_, sha)| sha)
        .unwrap();
    assert_eq!(state.blobs[first_sha], b"first");
    assert_eq!(state.blobs[second_sha], b"second");
}

#[tokio::test]
async fn native_edit_outbox_is_ordered_and_replays_dependent_deltas() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Outbox"),
            &artifact,
        )
        .await
        .unwrap();
    let key = load_manifest(&fixture.archive)
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let base =
        archive_entry_sha256(load_manifest(&fixture.archive).unwrap().get(&key).unwrap()).unwrap();
    let deferred = DeferredArchive::new(
        fixture.archive.clone(),
        fixture.inbox.clone(),
        fixture._temp.path().join("app-data"),
    )
    .unwrap();
    let transaction = begin_native_archive_transaction(&fixture.archive)
        .await
        .unwrap();
    let binding = ArchiveDestinationBinding {
        schema_version: 1,
        github_repo: "owner/private".to_owned(),
        r2_account_id: "0123456789abcdef0123456789abcdef".to_owned(),
        r2_bucket: "audio-a".to_owned(),
    };
    let first = NativeArchiveEdit::UserFields {
        deltas: vec![ArchiveEntryDelta {
            key: key.clone(),
            recording_id: Some(envelope.recording_id.to_string()),
            base_entry_sha256: Some(base.clone()),
            speaker_slots: vec![SpeakerSlotDelta {
                slot: "SPEAKER_1".to_owned(),
                expected: None,
                desired: Some("Alice".to_owned()),
            }],
            add_attachments: Vec::new(),
        }],
    };
    let first_ticket = deferred
        .stage_native_edit_with_binding(&transaction, first.clone(), binding.clone())
        .unwrap();
    assert!(load_manifest(&fixture.archive).unwrap()[&key]
        .get("speakers")
        .is_none());
    apply_native_edit_locally(&fixture.archive, &first).unwrap();
    let second = NativeArchiveEdit::UserFields {
        deltas: vec![ArchiveEntryDelta {
            key: key.clone(),
            recording_id: Some(envelope.recording_id.to_string()),
            base_entry_sha256: Some(base),
            speaker_slots: vec![SpeakerSlotDelta {
                slot: "SPEAKER_1".to_owned(),
                expected: Some("Alice".to_owned()),
                desired: Some("Bob".to_owned()),
            }],
            add_attachments: Vec::new(),
        }],
    };
    let second_ticket = deferred
        .stage_native_edit_with_binding(&transaction, second.clone(), binding)
        .unwrap();
    apply_native_edit_locally(&fixture.archive, &second).unwrap();
    drop(transaction);

    let tickets = load_native_edit_tickets(&deferred.native_edit_outbox).unwrap();
    assert_eq!(tickets.len(), 2);
    assert_eq!(tickets[0].record.edit_id, first_ticket.record.edit_id);
    assert_eq!(tickets[1].record.edit_id, second_ticket.record.edit_id);
    for ticket in tickets {
        adapter
            .publish_native_edit(ticket.record.edit)
            .await
            .unwrap();
    }
    let state = fixture.git.state.lock().unwrap();
    let tree = &state.commits[&state.head].0;
    let manifest_sha = &state.manifest_blobs[tree];
    let remote: BTreeMap<String, Value> =
        serde_json::from_slice(&state.blobs[manifest_sha]).unwrap();
    assert_eq!(remote[&key]["speakers"]["SPEAKER_1"], "Bob");
}

#[tokio::test]
async fn delete_tombstone_retries_r2_cleanup_and_blocks_republication() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Delete Fence"),
            &artifact,
        )
        .await
        .unwrap();
    let key = load_manifest(&fixture.archive)
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let planned = plan_local_recording_delete(&fixture.archive, &key).unwrap();
    let audio = planned.audio_relative.clone().unwrap();
    let edit = NativeArchiveEdit::Delete {
        key,
        recording_id: planned.recording_id.clone(),
        base_entry_sha256: Some(planned.base_entry_sha256),
        deleted_paths: planned.deleted,
        r2_delete: Some(R2DeleteIntent {
            key: audio.clone(),
            recording_id: envelope.recording_id.to_string(),
            generation: 1,
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        }),
    };
    fixture.r2.state.lock().unwrap().fail_delete_once = true;
    assert_eq!(
        adapter
            .publish_native_edit(edit.clone())
            .await
            .unwrap_err()
            .kind,
        ArchiveErrorKind::Network
    );
    adapter.publish_native_edit(edit).await.unwrap();
    {
        let r2 = fixture.r2.state.lock().unwrap();
        assert!(!r2.objects.contains_key(&audio));
        assert_eq!(r2.delete_calls, 2);
    }
    let error = adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Delete Fence"),
            &artifact,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, EffectErrorKind::PublicationConflict);
}

#[tokio::test]
async fn persisted_r2_attempt_cleans_put_after_crash_and_manifest_overlay() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Crash Cleanup"),
            &artifact,
        )
        .await
        .unwrap();
    let manifest = load_manifest(&fixture.archive).unwrap();
    let audio = manifest.values().next().unwrap()["r2_key"]
        .as_str()
        .unwrap()
        .to_owned();
    adapter
        .persist_r2_attempt(envelope.recording_id, 1, &audio, &artifact)
        .unwrap();

    {
        let mut state = fixture.git.state.lock().unwrap();
        let tombstones =
            serialize_deleted_recordings(&std::collections::BTreeSet::from([envelope
                .recording_id
                .to_string()]))
            .unwrap();
        let tombstone_sha = git_blob_sha(&tombstones);
        state.blobs.insert(tombstone_sha.clone(), tombstones);
        let empty_manifest = serialize_manifest(&BTreeMap::new()).unwrap();
        let manifest_sha = git_blob_sha(&empty_manifest);
        state.blobs.insert(manifest_sha.clone(), empty_manifest);
        let prior_tree = state.commits[&state.head].0.clone();
        let mut entries = state.tree_entries[&prior_tree].clone();
        entries.push((DELETED_RECORDINGS_PATH.to_owned(), tombstone_sha));
        state
            .tree_entries
            .insert("tombstone-tree".to_owned(), entries);
        state
            .manifest_blobs
            .insert("tombstone-tree".to_owned(), manifest_sha);
        state.commits.insert(
            "tombstone-head".to_owned(),
            ("tombstone-tree".to_owned(), "delete elsewhere".to_owned()),
        );
        state.head = "tombstone-head".to_owned();
    }
    atomic_write(
        &fixture.archive.join("manifest.json"),
        &serialize_manifest(&BTreeMap::new()).unwrap(),
    )
    .unwrap();

    let error = adapter
        .publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Crash Cleanup"),
            &artifact,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, EffectErrorKind::PublicationConflict);
    assert!(!fixture
        .r2
        .state
        .lock()
        .unwrap()
        .objects
        .contains_key(&audio));
    assert!(!adapter.r2_attempt_path(envelope.recording_id, 1).exists());
}

#[tokio::test]
async fn changed_r2_attempt_key_cleans_prior_owned_object_before_replacement() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    let first = format!(
        "2026-09-02/120000-recording-aaaaaaaaaaaaaaaa-g1-{}.wav",
        envelope.recording_id
    );
    let second = format!(
        "2026-09-02/120000-recording-bbbbbbbbbbbbbbbb-g1-{}.wav",
        envelope.recording_id
    );
    fixture.r2.state.lock().unwrap().objects.insert(
        first.clone(),
        R2PutProof {
            key: first.clone(),
            recording_id: envelope.recording_id.to_string(),
            version_id: "attempt-version".to_owned(),
            etag: "attempt-etag".to_owned(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        },
    );
    adapter
        .persist_r2_attempt(envelope.recording_id, 1, &first, &artifact)
        .unwrap();
    adapter
        .begin_r2_attempt(envelope.recording_id, 1, &second, &artifact)
        .await
        .unwrap();
    let state = fixture.r2.state.lock().unwrap();
    assert!(!state.objects.contains_key(&first));
    assert_eq!(state.delete_calls, 1);
    drop(state);
    assert_eq!(
        adapter
            .load_r2_attempt(envelope.recording_id, 1)
            .unwrap()
            .unwrap()
            .key,
        second
    );
}

#[test]
fn pending_native_edit_destination_binding_never_crosses_repositories_or_buckets() {
    let original = ArchiveDestinationBinding {
        schema_version: 1,
        github_repo: "owner/archive-a".to_owned(),
        r2_account_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        r2_bucket: "audio-a".to_owned(),
    };
    assert!(verify_destination_binding(&original, &original).is_ok());
    for changed in [
        ArchiveDestinationBinding {
            github_repo: "owner/archive-b".to_owned(),
            ..original.clone()
        },
        ArchiveDestinationBinding {
            r2_account_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            ..original.clone()
        },
        ArchiveDestinationBinding {
            r2_bucket: "audio-b".to_owned(),
            ..original.clone()
        },
    ] {
        assert_eq!(
            verify_destination_binding(&original, &changed)
                .unwrap_err()
                .kind,
            EffectErrorKind::PublicationConflict
        );
    }
}

#[test]
fn archive_paths_reject_control_files_symlink_ancestors_and_allow_null_audio() {
    let directory = tempfile::tempdir().unwrap();
    let day = directory.path().join("2026-09-02");
    fs::create_dir(&day).unwrap();
    fs::write(day.join("120000-safe.md"), "safe").unwrap();
    fs::write(
        directory.path().join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "2026-09-02 120000": {
                "note": "2026-09-02/120000-safe.md",
                "audio": null
            }
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(load_manifest(directory.path()).is_ok());

    fs::write(
        directory.path().join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "2026-09-02 120000": {"note": "manifest.json", "audio": null}
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        load_manifest(directory.path()).unwrap_err().kind,
        ArchiveErrorKind::Verification
    );

    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        fs::remove_dir_all(&day).unwrap();
        std::os::unix::fs::symlink(outside.path(), &day).unwrap();
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "2026-09-02 120000": {
                    "note": "2026-09-02/120000-safe.md",
                    "audio": null
                }
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            load_manifest(directory.path()).unwrap_err().kind,
            ArchiveErrorKind::Verification
        );
    }
}

#[test]
fn repository_archive_manifest_remains_read_compatible() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    if root.join("manifest.json").is_file() {
        load_manifest(&root).expect("the existing archive must remain readable");
    }
}

#[tokio::test]
async fn backup_is_independently_head_verified_and_corrupt_proof_is_rejected() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            7,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Backup"),
            &artifact,
        )
        .await
        .unwrap();
    let proof = adapter
        .verify_backup(PublicationBackend::RemoteArchive, 7, &envelope, &artifact)
        .await
        .unwrap();
    assert_eq!(proof.sha256, artifact.sha256);
    assert_eq!(proof.size_bytes, artifact.size_bytes);
    fixture.r2.state.lock().unwrap().corrupt_head = true;
    assert_eq!(
        adapter
            .verify_backup(PublicationBackend::RemoteArchive, 7, &envelope, &artifact)
            .await
            .unwrap_err()
            .kind,
        EffectErrorKind::Verification
    );
}

#[tokio::test]
async fn corrupt_put_receipt_stops_before_github_publication() {
    let fixture = Fixture::new();
    fixture.r2.state.lock().unwrap().corrupt_put = true;
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    assert_eq!(
        adapter
            .publish(
                TARGET_ID,
                1,
                &envelope,
                &json!({"text": "fixture"}),
                &Fixture::summary("Corrupt"),
                &artifact,
            )
            .await
            .unwrap_err()
            .kind,
        EffectErrorKind::Verification
    );
    assert_eq!(fixture.git.state.lock().unwrap().commit_count, 0);
}

#[test]
fn confirmed_import_title_source_contract_and_current_provider_fields_are_formatted() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    let mut value = serde_json::to_value(envelope).unwrap();
    value["source"] = json!({
        "kind": "file_import",
        "platform": "macos",
        "label": "Imported Label",
        "capture_scope": "imported_file"
    });
    value["tracks"][0]["role"] = json!("imported");
    value["imported_name"] = json!("fixture.wav");
    value["import_review"] = json!({
        "timestamp_source": "user",
        "timestamp_confidence": "user_confirmed",
        "display_title": "Confirmed / Import Title",
        "speaker_count": 2,
        "confirmed_at": "2026-09-02T12:05:00-07:00"
    });
    let envelope: RecordingEnvelope = serde_json::from_value(value).unwrap();
    let prepared = adapter
        .prepare_local(
            1,
            &envelope,
            &json!([{
                "speaker": {"id": "2"},
                "content": "fabricated speech",
                "start_time": 1000,
                "end_time": 2500
            }]),
            &json!({
                "title": "AI title must not win",
                "category": "工作商务",
                "summary_en": "English summary.",
                "summary_zh": "中文摘要。",
                "key_points_en": ["English point"],
                "key_points_zh": ["中文要点"],
                "action_items": ["Follow up"]
            }),
            &artifact,
            ArchivePreparation::Remote(&RemoteArchive {
                manifest: BTreeMap::new(),
                notes: Vec::new(),
                deleted_recording_ids: std::collections::BTreeSet::new(),
            }),
        )
        .unwrap();
    assert!(prepared.note.contains("Confirmed / Import Title"));
    assert!(prepared.note.contains("English summary.\n\n中文摘要。"));
    assert!(prepared.note.contains("- English point\n\n- 中文要点"));
    assert!(prepared.note.contains("- [ ] Follow up"));
    assert!(prepared
        .note
        .contains("[00:00:01 - 00:00:02] SPEAKER_2: fabricated speech"));
    let manifest: BTreeMap<String, Value> = serde_json::from_slice(&prepared.manifest).unwrap();
    let entry = manifest.values().next().unwrap();
    assert!(entry["title"]
        .as_str()
        .unwrap()
        .ends_with("Confirmed / Import Title"));
    assert_eq!(entry["source"], "Imported Label");
    assert_eq!(entry["source_kind"], "file_import");
    assert!(prepared
        .note
        .contains("**Recorded:** 2026-09-02T12:00:00.987-07:00"));
    assert!(prepared.note.contains("**Source:** Imported Label"));
    assert!(prepared.note.contains("**File:** `fixture.wav`"));
    assert!(prepared.note.contains("\n---\n\n## Transcript\n\n```\n"));
    assert!(!prepared.note.contains("```text"));
}

#[tokio::test]
async fn publisher_rebuilds_daily_topic_and_trusted_viewer_derivatives() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let (first, first_artifact) = fixture.recording(Uuid::new_v4());
    adapter
        .publish(
            TARGET_ID,
            1,
            &first,
            &json!([{
                "speaker": {"id": "1"},
                "content": "first",
                "start_time": 0,
                "end_time": 2_000
            }]),
            &Fixture::summary("First"),
            &first_artifact,
        )
        .await
        .unwrap();
    let (second, second_artifact) = fixture.recording(Uuid::new_v4());
    let mut second_summary = Fixture::summary("Second");
    second_summary["category"] = Value::String("学习认知".to_owned());
    adapter
        .publish(
            TARGET_ID,
            1,
            &second,
            &json!([{
                "speaker": {"id": "2"},
                "content": "second",
                "start_time": 0,
                "end_time": 3_000
            }]),
            &second_summary,
            &second_artifact,
        )
        .await
        .unwrap();

    let daily = fs::read_to_string(fixture.archive.join("2026-09-02/daily.md")).unwrap();
    assert!(daily.starts_with("# 2026-09-02\n\n_2 recording(s)_\n"));
    assert!(daily.contains("# 2026-09-02 12:00 First"));
    assert!(daily.contains("# 2026-09-02 12:00 Second"));
    let daily_html = fs::read_to_string(fixture.archive.join("2026-09-02/daily.html")).unwrap();
    assert!(daily_html.contains("<title>2026-09-02</title>"));
    assert!(daily_html.contains("<div class=\"transcript\">"));
    assert_eq!(
        fs::read(fixture.archive.join("marked.min.js")).unwrap(),
        VIEWER_MARKED_JS
    );

    #[cfg(unix)]
    {
        let manifest = load_manifest(&fixture.archive).unwrap();
        for entry in manifest.values() {
            let category = entry["category"].as_str().unwrap();
            let title = safe_topic_filename(entry["title"].as_str().unwrap());
            for field in ["note", "audio"] {
                let relative = entry[field].as_str().unwrap();
                let extension = Path::new(relative)
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap();
                let link = fixture
                    .archive
                    .join("by-topic")
                    .join(category)
                    .join(format!("{title}.{extension}"));
                assert!(fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink());
                assert_eq!(
                    link.canonicalize().unwrap(),
                    fixture.archive.join(relative).canonicalize().unwrap()
                );
            }
        }
    }

    let index = fs::read_to_string(fixture.archive.join("index.html")).unwrap();
    assert!(index.contains("\"duration\":2"));
    assert!(index.contains("\"duration\":3"));
}

#[test]
fn viewer_span_is_chronological_across_years() {
    let directory = tempfile::tempdir().unwrap();
    for (key, note) in [
        ("2025-12-31 235959", "2025-12-31/235959-old.md"),
        ("2026-01-01 000001", "2026-01-01/000001-new.md"),
    ] {
        fs::create_dir_all(directory.path().join(&key[..10])).unwrap();
        fs::write(
            directory.path().join(note),
            format!(
                "# {key}\n\n## Transcript\n\n```\n[00:00:01 - 00:00:02] SPEAKER_1: fixture\n```\n"
            ),
        )
        .unwrap();
    }
    fs::write(
        directory.path().join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "2025-12-31 235959": {
                "title": "2025-12-31 23:59 Old",
                "category": "其他",
                "note": "2025-12-31/235959-old.md",
                "audio": null
            },
            "2026-01-01 000001": {
                "title": "2026-01-01 00:00 New",
                "category": "其他",
                "note": "2026-01-01/000001-new.md",
                "audio": null
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let html = render_existing_viewer(directory.path()).unwrap();
    let html = std::str::from_utf8(&html).unwrap();
    assert!(html.contains("\"span\":\"2025–2026\""));
    assert_eq!(html.matches("\"duration\":2").count(), 2);
}

#[test]
fn archive_process_lock_is_exclusive_and_released_by_raii() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("archive.lock");
    let first = open_archive_process_lock(&path).unwrap();
    let second = open_archive_process_lock(&path).unwrap();

    let guard = ArchiveFileLockGuard::acquire(&first).unwrap();
    let busy = match ArchiveFileLockGuard::acquire(&second) {
        Ok(_) => panic!("a second publisher must not enter the archive critical section"),
        Err(error) => error,
    };
    assert_eq!(busy.kind, ArchiveErrorKind::Network);

    drop(guard);
    ArchiveFileLockGuard::acquire(&second).unwrap();
}

#[tokio::test]
async fn canceled_network_future_releases_archive_operation_and_file_locks() {
    let fixture = Fixture::new();
    let adapter = ArchiveAdapter::with_components(
        fixture.archive.clone(),
        fixture.inbox.clone(),
        "owner/private".to_owned(),
        "test-secret".to_owned(),
        Arc::clone(&fixture.git),
        Arc::new(HangingR2),
    )
    .unwrap();
    let (envelope, artifact) = fixture.recording(Uuid::new_v4());
    let timed_out = tokio::time::timeout(
        std::time::Duration::from_millis(25),
        adapter.publish(
            TARGET_ID,
            1,
            &envelope,
            &json!({"text": "fixture"}),
            &Fixture::summary("Hanging"),
            &artifact,
        ),
    )
    .await;
    assert!(timed_out.is_err());
    assert!(adapter.operation_lock.try_lock().is_ok());
    let contender =
        open_archive_process_lock(&archive_process_lock_path(&fixture.archive).unwrap()).unwrap();
    ArchiveFileLockGuard::acquire(&contender).unwrap();
}

#[tokio::test]
async fn native_edit_transaction_serializes_local_mutation_through_remote_cas() {
    let fixture = Fixture::new();
    let first = begin_native_archive_transaction(&fixture.archive)
        .await
        .unwrap();
    let blocked = tokio::time::timeout(
        std::time::Duration::from_millis(25),
        begin_native_archive_transaction(&fixture.archive),
    )
    .await;
    assert!(blocked.is_err());
    drop(first);
    begin_native_archive_transaction(&fixture.archive)
        .await
        .unwrap();
}

#[test]
fn rust_archive_edit_rewrites_and_clears_speaker_labels_without_python() {
    let directory = tempfile::tempdir().unwrap();
    let day = directory.path().join("2026-09-02");
    fs::create_dir(&day).unwrap();
    let note_relative = "2026-09-02/120000-fixture.md";
    fs::write(
        directory.path().join(note_relative),
        "# Fixture\n\n## Transcript\n\n```text\n[00:00:01 - 00:00:02] Old Name: fabricated speech\n```\n",
    )
    .unwrap();
    let manifest_path = directory.path().join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&json!({
            "2026-09-02 120000": {
                "title": "2026-09-02 12:00 Fixture",
                "category": "其他",
                "note": note_relative,
                "speakers": {"SPEAKER_1": "Alice"},
                "speakers_applied": {"SPEAKER_1": "Old Name"}
            }
        }))
        .unwrap(),
    )
    .unwrap();

    apply_speaker_labels(directory.path(), &["2026-09-02 120000".to_owned()]).unwrap();
    let renamed = fs::read_to_string(directory.path().join(note_relative)).unwrap();
    assert!(renamed.contains("] Alice: fabricated speech"));
    assert!(directory.path().join("index.html").is_file());

    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["2026-09-02 120000"]
        .as_object_mut()
        .unwrap()
        .remove("speakers");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    apply_speaker_labels(directory.path(), &["2026-09-02 120000".to_owned()]).unwrap();
    let cleared = fs::read_to_string(directory.path().join(note_relative)).unwrap();
    assert!(cleared.contains("] SPEAKER_1: fabricated speech"));
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert!(manifest["2026-09-02 120000"]
        .get("speakers_applied")
        .is_none());
}

#[test]
fn native_local_delete_removes_only_owned_artifacts_and_rebuilds_viewer() {
    let directory = tempfile::tempdir().unwrap();
    let day = directory.path().join("2026-09-02");
    fs::create_dir(&day).unwrap();
    let note = day.join("120000-Delete.md");
    let audio = day.join("120000-Delete.wav");
    let attachments = day.join("120000-attachments");
    fs::create_dir(&attachments).unwrap();
    fs::write(&note, "# 2026-09-02 12:00 Delete\n").unwrap();
    fs::write(&audio, b"fabricated").unwrap();
    fs::write(attachments.join("context.md"), "context").unwrap();
    fs::write(day.join("daily.md"), "stale").unwrap();
    fs::write(day.join("daily.html"), "stale").unwrap();
    let recording_id = Uuid::new_v4();
    fs::write(
        directory.path().join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "2026-09-02 120000": {
                "recording_id": recording_id,
                "title": "2026-09-02 12:00 Delete",
                "category": "其他",
                "note": "2026-09-02/120000-Delete.md",
                "audio": "2026-09-02/120000-Delete.wav",
                "attachments": ["2026-09-02/120000-attachments/context.md"]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let topic = directory.path().join("by-topic/其他");
        fs::create_dir_all(&topic).unwrap();
        std::os::unix::fs::symlink("../../2026-09-02/120000-Delete.md", topic.join("Delete.md"))
            .unwrap();
    }

    let result = delete_local_recording(directory.path(), "2026-09-02 120000").unwrap();
    assert_eq!(result.recording_id, Some(recording_id.to_string()));
    assert_eq!(
        result.audio_relative.as_deref(),
        Some("2026-09-02/120000-Delete.wav")
    );
    assert!(!note.exists());
    assert!(!audio.exists());
    assert!(!attachments.exists());
    assert!(!day.exists());
    assert_eq!(load_manifest(directory.path()).unwrap().len(), 0);
    assert!(fs::read_to_string(directory.path().join("index.html"))
        .unwrap()
        .contains("safeMarkdownFragment"));
    #[cfg(unix)]
    assert!(!directory.path().join("by-topic").exists());
}
