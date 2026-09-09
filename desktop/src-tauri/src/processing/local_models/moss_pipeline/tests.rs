use super::*;
use std::{fs, time::Duration};

struct Fixture {
    _temp: tempfile::TempDir,
    manager: Arc<LocalModelPackManager>,
    catalog: PipelineCatalog,
}
impl Fixture {
    fn new(installed: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let manager = Arc::new(LocalModelPackManager::open(temp.path()).unwrap());
        let components = ["moss", "speakerkit", "summary"].into_iter().map(|id| {
            let bytes = format!("synthetic-{id}");
            let path = format!("models/{id}/fixture/model.bin");
            if installed {
                let destination = temp.path().join(&path);
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                fs::write(destination, &bytes).unwrap();
            }
            ComponentPack { id, display_name: id.into(), prefix: format!("models/{id}/fixture/"), files: vec![PackFile {
                file: CatalogFile { source_id: "fixture".into(), source_path: "model.bin".into(), install_path: path, sha256: hex::encode(Sha256::digest(bytes.as_bytes())), size_bytes: bytes.len() as u64 },
                url: reqwest::Url::parse("https://huggingface.co/fixture/models/resolve/0000000000000000000000000000000000000000/model.bin").unwrap(),
            }] }
        }).collect();
        Self {
            _temp: temp,
            manager,
            catalog: PipelineCatalog {
                components,
                licenses: vec!["synthetic".into()],
            },
        }
    }
    fn path(&self, index: usize) -> PathBuf {
        self.manager
            .app_data_root
            .join(&self.catalog.components[index].files[0].file.install_path)
    }
    async fn run(&self, kind: OperationKind) -> Result<(), LocalModelError> {
        self.manager
            .run_pipeline(kind, self.catalog.clone(), true)
            .await
    }
    fn status(&self) -> MossPipelineModelStatus {
        pipeline_snapshot(&self.manager.moss_pipeline, &self.catalog, true, false).unwrap()
    }
}

#[test]
fn pipeline_is_exactly_the_three_existing_components_not_whisper() {
    let catalog = PipelineCatalog::load().unwrap();
    assert_eq!(
        catalog
            .components
            .iter()
            .map(|component| component.id)
            .collect::<Vec<_>>(),
        ["moss", "speakerkit", "summary"]
    );
    assert_eq!(
        catalog
            .components
            .iter()
            .map(|component| component.files.len())
            .collect::<Vec<_>>(),
        [1, 29, 1]
    );
    assert!(catalog
        .components
        .iter()
        .flat_map(ComponentPack::plain_files)
        .all(|file| !file.install_path.contains("whisper")
            && !file.install_path.contains("fluid-v1")));
    assert!(catalog.total_bytes() > 1024 * 1024 * 1024);
    assert!(require_pack("moss-candidate-v1").is_err());
}

#[test]
fn shared_removal_requires_explicit_closed_request() {
    let request: RemoveMossPipelineRequest =
        serde_json::from_value(serde_json::json!({"packId":MOSS_PIPELINE_PACK_ID})).unwrap();
    assert!(!request.remove_shared);
    assert!(serde_json::from_value::<RemoveMossPipelineRequest>(
        serde_json::json!({"packId":MOSS_PIPELINE_PACK_ID, "path":"/tmp"})
    )
    .is_err());
}

#[tokio::test]
async fn matching_shared_components_are_reused_and_idle_cache_is_invalidated_by_replacement() {
    let fixture = Fixture::new(true);
    fixture.run(OperationKind::Install).await.unwrap(); // Network is forbidden.
    assert!(fixture.status().installed);
    assert_eq!(
        fixture.manager.moss_pipeline.lock().unwrap().hashed_files,
        3
    );
    fixture.run(OperationKind::Inspect).await.unwrap();
    assert_eq!(
        fixture.manager.moss_pipeline.lock().unwrap().hashed_files,
        3
    );
    fixture.run(OperationKind::Proof).await.unwrap();
    assert_eq!(
        fixture.manager.moss_pipeline.lock().unwrap().hashed_files,
        6
    );
    let path = fixture.path(2);
    let replacement = path.with_extension("replacement");
    fs::write(
        &replacement,
        vec![b'x'; fixture.catalog.components[2].files[0].file.size_bytes as usize],
    )
    .unwrap();
    fs::rename(replacement, &path).unwrap();
    fixture.run(OperationKind::Inspect).await.unwrap();
    let status = fixture.status();
    assert!(!status.installed && !status.components[2].installed);
    assert!(status.components[0].installed && status.components[1].installed);
    assert_eq!(status.downloaded_bytes, fixture.catalog.total_bytes());
    assert_eq!(
        fixture.manager.moss_pipeline.lock().unwrap().hashed_files,
        7
    );
    assert_eq!(
        fixture.run(OperationKind::Install).await.unwrap_err().code,
        "model_destination_conflict"
    );
}

#[tokio::test]
async fn missing_one_dependency_never_counts_as_ready_and_removal_preserves_shared_and_unknown_files(
) {
    let fixture = Fixture::new(true);
    fs::remove_file(fixture.path(2)).unwrap();
    fixture.run(OperationKind::Inspect).await.unwrap();
    assert!(!fixture.status().installed);
    assert!(!fixture.status().components[2].installed);
    let unknown = fixture.path(0).with_file_name("keep.txt");
    fs::write(&unknown, b"not owned by catalog").unwrap();
    fixture.run(OperationKind::Remove(false)).await.unwrap();
    assert!(!fixture.path(0).exists());
    assert!(fixture.path(1).is_file());
    assert_eq!(fs::read(&unknown).unwrap(), b"not owned by catalog");
    fixture.run(OperationKind::Remove(true)).await.unwrap();
    assert!(!fixture.path(1).exists());
    assert!(unknown.is_file());
}

#[tokio::test]
async fn full_partial_files_resume_without_network_and_empty_partial_is_retryable() {
    let fixture = Fixture::new(false);
    for (index, component) in fixture.catalog.components.iter().enumerate() {
        let destination = fixture.path(index);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(
            partial_path(&destination).unwrap(),
            format!("synthetic-{}", component.id),
        )
        .unwrap();
    }
    fixture.run(OperationKind::Install).await.unwrap();
    assert!(fixture.status().installed);
    for index in 0..3 {
        assert!(!partial_path(&fixture.path(index)).unwrap().exists());
    }
    let empty = fixture.manager.app_data_root.join("empty.partial");
    fs::write(&empty, b"").unwrap();
    let mut file = download::open_partial_output(&empty, 0).await.unwrap();
    file.write_all(b"abc").await.unwrap();
    file.flush().await.unwrap();
    drop(file);
    assert_eq!(fs::read(&empty).unwrap(), b"abc");
    assert!(download::open_partial_output(&empty, 0).await.is_err());
}

#[tokio::test]
async fn cancel_is_scoped_and_busy_lease_outlives_dropped_operation_owner() {
    let fixture = Fixture::new(false);
    let work = fixture
        .manager
        .start_pipeline(OperationKind::Install, fixture.catalog.clone(), true)
        .unwrap();
    let state = pipeline_snapshot(
        &fixture.manager.moss_pipeline,
        &fixture.catalog,
        true,
        false,
    )
    .unwrap();
    assert!(state.installing && !state.installed);
    let pending = work.clone();
    let reader = tokio::spawn(async move {
        pending
            .await_download(std::future::pending::<Result<(), LocalModelError>>())
            .await
    });
    fixture
        .manager
        .cancel_install(MOSS_PIPELINE_PACK_ID)
        .unwrap();
    assert!(!fixture.manager.cancel.load(Ordering::Acquire));
    assert!(!fixture.manager.qwen_cancel.load(Ordering::Acquire));
    assert!(!fixture.manager.speakerkit_cancel.load(Ordering::Acquire));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        "model_install_canceled"
    );
    let held = work.clone();
    drop(work);
    assert!(fixture.manager.acquire_busy().is_err());
    drop(held);
    assert!(fixture.manager.acquire_busy().is_ok());
    assert!(!fixture.status().installing);
    assert_eq!(fixture.status().error_code, Some("model_install_canceled"));
    fixture.run(OperationKind::Inspect).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_model_and_partial_fail_without_touching_target() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(true);
    let target = fixture.manager.app_data_root.join("outside-model");
    fs::write(&target, b"keep").unwrap();
    fs::remove_file(fixture.path(0)).unwrap();
    symlink(&target, fixture.path(0)).unwrap();
    assert!(fixture.run(OperationKind::Inspect).await.is_err());
    assert!(fixture.run(OperationKind::Remove(true)).await.is_err());
    let partial = fixture.manager.app_data_root.join("symlink.partial");
    symlink(&target, &partial).unwrap();
    assert!(download::open_partial_output(&partial, 4).await.is_err());
    assert_eq!(fs::read(target).unwrap(), b"keep");
}
