//! Separate, opt-in MOSS model lifecycle. The 32 GiB floor belongs to the full
//! local pipeline (including diarization and summary), not this GGUF alone.
//! Downloads reuse the manager's allowlisted HTTPS client and bounded file
//! helpers; model bytes never include or load the native worker executable.

use super::*;
use echowall_local_moss_protocol::{
    MODEL_ID, MODEL_REVISION, MODEL_SHA256, MODEL_SIZE_BYTES, RUNTIME_ID,
};
use echowall_local_whisper_protocol::LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET;

pub const MOSS_CANDIDATE_PACK_ID: &str = "moss-candidate-v1";
const CATALOG_JSON: &str = include_str!("../../../../model-catalog/moss-candidate-v1.json");
const INSTALL_PREFIX: &str = "models/moss/moss-transcribe-diarize-0.9b-q8_0/";
const SOURCE_REPOSITORY: &str =
    "https://huggingface.co/handy-computer/moss-transcribe-diarize-gguf/resolve/";

fn require_moss_candidate(features: &RuntimeFeatures) -> Result<(), String> {
    if !features.local_moss_candidate || !crate::features::full_local_supported() {
        return Err("MOSS candidate mode is unavailable".to_owned());
    }
    Ok(())
}

#[tauri::command]
pub async fn moss_candidate_model_pack_status(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
) -> Result<MossCandidateModelPackStatus, String> {
    require_moss_candidate(&features)?;
    state
        .manager()
        .moss_status(features.local_moss_candidate)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn install_moss_candidate_model_pack(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
    request: LocalModelPackRequest,
) -> Result<MossCandidateModelPackStatus, String> {
    require_moss_candidate(&features)?;
    state
        .manager()
        .install_moss(&request.pack_id)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn remove_moss_candidate_model_pack(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
    request: LocalModelPackRequest,
) -> Result<MossCandidateModelPackStatus, String> {
    require_moss_candidate(&features)?;
    state
        .manager()
        .remove_moss(&request.pack_id)
        .await
        .map_err(safe_error)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MossCandidateCatalog {
    schema_version: u32,
    pack_id: String,
    display_name: String,
    runtime_id: String,
    runtime_license: String,
    runtime_attribution: String,
    model_id: String,
    model_revision: String,
    minimum_macos_major: u32,
    minimum_memory_bytes: u64,
    memory_requirement_scope: String,
    sources: Vec<CatalogSource>,
    files: Vec<CatalogFile>,
}

impl MossCandidateCatalog {
    pub(super) fn load() -> Result<&'static Self, LocalModelError> {
        static CATALOG: OnceLock<Result<MossCandidateCatalog, LocalModelError>> = OnceLock::new();
        CATALOG
            .get_or_init(|| {
                let catalog: Self = serde_json::from_str(CATALOG_JSON)
                    .map_err(|_| LocalModelError::new("invalid_catalog"))?;
                catalog.validate()?;
                Ok(catalog)
            })
            .as_ref()
            .map_err(|error| *error)
    }

    fn validate(&self) -> Result<(), LocalModelError> {
        if self.schema_version != 1
            || self.pack_id != MOSS_CANDIDATE_PACK_ID
            || self.display_name.is_empty()
            || self.display_name.len() > 160
            || self.runtime_id != RUNTIME_ID
            || self.runtime_license != "MIT"
            || self.runtime_attribution.is_empty()
            || self.runtime_attribution.len() > 512
            || self.model_id != MODEL_ID
            || self.model_revision != MODEL_REVISION
            || self.minimum_macos_major != 14
            || self.minimum_memory_bytes != 32 * 1024 * 1024 * 1024
            || self.memory_requirement_scope != "full-local-pipeline"
            || self.sources.len() != 1
            || self.files.len() != 1
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        let source = &self.sources[0];
        let file = &self.files[0];
        if source.id != "moss-gguf"
            || source.base_url != format!("{SOURCE_REPOSITORY}{MODEL_REVISION}")
            || source.license != "Apache-2.0"
            || source.attribution.is_empty()
            || source.attribution.len() > 512
            || file.source_id != source.id
            || file.source_path != "MOSS-Transcribe-Diarize-Q8_0.gguf"
            || file.install_path != format!("models/moss/{MODEL_ID}/model.gguf")
            || file.install_path != format!("{INSTALL_PREFIX}model.gguf")
            || file.sha256 != MODEL_SHA256
            || file.size_bytes != MODEL_SIZE_BYTES
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        self.source_url(file)?;
        Ok(())
    }

    fn source_url(&self, file: &CatalogFile) -> Result<reqwest::Url, LocalModelError> {
        let source = self
            .sources
            .iter()
            .find(|source| source.id == file.source_id)
            .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
        let base = reqwest::Url::parse(&source.base_url)
            .map_err(|_| LocalModelError::new("invalid_catalog"))?;
        if !pinned_huggingface_base(&base) || !valid_source_path(&file.source_path) {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        let url = reqwest::Url::parse(&format!("{}/{}", source.base_url, file.source_path))
            .map_err(|_| LocalModelError::new("invalid_catalog"))?;
        if !allowed_download_url(&url) || url.query().is_some() || url.fragment().is_some() {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(url)
    }

    fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size_bytes).sum()
    }

    pub(super) fn pipeline_files(
        &self,
    ) -> Result<Vec<(CatalogFile, reqwest::Url)>, LocalModelError> {
        self.files
            .iter()
            .map(|file| Ok((file.clone(), self.source_url(file)?)))
            .collect()
    }

    pub(super) fn pipeline_licenses(&self) -> Vec<String> {
        let mut licenses: Vec<_> = self
            .sources
            .iter()
            .map(|source| format!("{} · {}", source.license, source.attribution))
            .collect();
        licenses.push(format!(
            "{} · {}",
            self.runtime_license, self.runtime_attribution
        ));
        licenses
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MossCandidateModelPackStatus {
    pub supported: bool,
    pub enabled: bool,
    pub pack_id: String,
    pub display_name: String,
    pub runtime_id: String,
    pub model_id: String,
    pub model_revision: String,
    pub minimum_macos_major: u32,
    pub minimum_memory_bytes: u64,
    pub memory_requirement_scope: String,
    pub detected_memory_bytes: Option<u64>,
    pub memory_sufficient: bool,
    pub total_bytes: u64,
    pub installed: bool,
    pub installing: bool,
    pub downloaded_bytes: u64,
    pub licenses: Vec<String>,
    pub error_code: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct MossCandidateModelPackProof {
    pub pack_id: String,
    pub runtime_id: String,
    pub model_id: String,
    pub model_revision: String,
    pub model_sha256: String,
    pub model_size_bytes: u64,
}

/// Revalidate persisted preparation pins without downloading or trusting a
/// caller-supplied model list. Actual bytes still require the manager/worker's
/// file proof; this binds the durable declaration to the compiled catalogs.
pub(crate) fn validate_preparation_model_pins(
    model: &MossCandidateModelPackProof,
    speakerkit: &SpeakerKitCandidateModelPackProof,
    summary: &crate::processing::LocalSummaryCheckpoint,
) -> Result<(), LocalModelError> {
    let catalog = MossCandidateCatalog::load()?;
    let speakers = SpeakerKitCandidateCatalog::load()?;
    let full = LocalPackCatalog::load()?;
    let summary_file = full.summary_model_file()?;
    if model.pack_id != catalog.pack_id
        || model.runtime_id != catalog.runtime_id
        || model.model_id != catalog.model_id
        || model.model_revision != catalog.model_revision
        || model.model_sha256 != catalog.files[0].sha256
        || model.model_size_bytes != catalog.files[0].size_bytes
        || speakerkit.pack_id != speakers.pack_id
        // Runtime policy is explicitly persisted separately from the unchanged
        // model bytes. Retained MOSS preparations keep their selected v1.
        || !matches!(
            speakerkit.quality_preset.as_str(),
            LOCAL_DIARIZATION_SPEAKERKIT_PRESET | LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET
        )
        || speakerkit.model_revision != speakers.model_revision
        || speakerkit.model_files != speakers.model_files()?
        || summary.model_id != full.summary_model_id
        || summary.model_sha256 != summary_file.sha256
        || summary.model_size_bytes != summary_file.size_bytes
        || summary.prompt_version != echowall_local_summary_protocol::LOCAL_SUMMARY_PROMPT_VERSION
        || summary.transcript_sha256.is_some()
    {
        return Err(LocalModelError::new("invalid_catalog"));
    }
    Ok(())
}

/// Compiled catalog metadata only. This does not assert installation, read or
/// hash model bytes, download anything, or bypass the later worker byte proof.
pub(crate) fn preparation_model_pins() -> Result<
    (
        MossCandidateModelPackProof,
        SpeakerKitCandidateModelPackProof,
        crate::processing::LocalSummaryCheckpoint,
    ),
    LocalModelError,
> {
    let model = MossCandidateCatalog::load()?;
    let speakers = SpeakerKitCandidateCatalog::load()?;
    let full = LocalPackCatalog::load()?;
    let summary = full.summary_model_file()?;
    Ok((
        MossCandidateModelPackProof {
            pack_id: model.pack_id.clone(),
            runtime_id: model.runtime_id.clone(),
            model_id: model.model_id.clone(),
            model_revision: model.model_revision.clone(),
            model_sha256: model.files[0].sha256.clone(),
            model_size_bytes: model.files[0].size_bytes,
        },
        SpeakerKitCandidateModelPackProof {
            pack_id: speakers.pack_id.clone(),
            // New MOSS candidate jobs select the measured runtime policy.
            // This does not change the separate SpeakerKit catalog default,
            // model files, or any retained preparation/plan request.
            quality_preset: LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET.into(),
            model_revision: speakers.model_revision.clone(),
            model_files: speakers.model_files()?,
        },
        crate::processing::LocalSummaryCheckpoint {
            model_id: full.summary_model_id.clone(),
            model_sha256: summary.sha256.clone(),
            model_size_bytes: summary.size_bytes,
            prompt_version: echowall_local_summary_protocol::LOCAL_SUMMARY_PROMPT_VERSION.into(),
            transcript_sha256: None,
        },
    ))
}

#[cfg(test)]
pub(crate) fn preparation_model_pins_fixture() -> (
    MossCandidateModelPackProof,
    SpeakerKitCandidateModelPackProof,
    crate::processing::LocalSummaryCheckpoint,
) {
    preparation_model_pins().unwrap()
}

// Dropping an aborted installation future must not leave a permanently busy UI.
struct InstallStateLease<'a>(&'a Mutex<OperationStatus>);

impl Drop for InstallStateLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut operation) = self.0.lock() {
            if operation.installing {
                operation.installing = false;
                operation.installed = false;
                operation.error_code = Some("model_install_canceled");
            }
        }
    }
}

impl LocalModelPackManager {
    pub async fn moss_status(
        &self,
        enabled: bool,
    ) -> Result<MossCandidateModelPackStatus, LocalModelError> {
        let installing = self
            .moss_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?
            .installing;
        if !installing {
            let _busy = self.acquire_busy()?;
            let root = self.app_data_root.clone();
            let catalog = MossCandidateCatalog::load()?.clone();
            let (installed, staged_bytes) = tokio::task::spawn_blocking(move || {
                let installed = verify_exact_catalog_tree(&root, &catalog.files, INSTALL_PREFIX)?;
                let staged_bytes = if installed {
                    catalog.total_bytes()
                } else {
                    installed_and_partial_catalog_bytes(
                        &root,
                        &catalog.files,
                        catalog.total_bytes(),
                    )?
                };
                Ok::<_, LocalModelError>((installed, staged_bytes))
            })
            .await
            .map_err(|_| LocalModelError::new("model_state_unavailable"))??;
            let mut operation = self
                .moss_operation
                .lock()
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            operation.installed = installed;
            operation.downloaded_bytes = staged_bytes;
            if installed {
                operation.error_code = None;
            }
        }
        self.moss_snapshot(enabled)
    }

    pub async fn moss_proof(&self) -> Result<MossCandidateModelPackProof, LocalModelError> {
        let catalog = MossCandidateCatalog::load()?;
        self.require_moss_memory(catalog)?;
        let _busy = self.acquire_busy()?;
        let root = self.app_data_root.clone();
        let files = catalog.files.clone();
        if !tokio::task::spawn_blocking(move || {
            verify_exact_catalog_tree(&root, &files, INSTALL_PREFIX)
        })
        .await
        .map_err(|_| LocalModelError::new("model_state_unavailable"))??
        {
            return Err(LocalModelError::new("model_pack_not_installed"));
        }
        Ok(MossCandidateModelPackProof {
            pack_id: catalog.pack_id.clone(),
            runtime_id: catalog.runtime_id.clone(),
            model_id: catalog.model_id.clone(),
            model_revision: catalog.model_revision.clone(),
            model_sha256: catalog.files[0].sha256.clone(),
            model_size_bytes: catalog.files[0].size_bytes,
        })
    }

    pub async fn install_moss(
        &self,
        pack_id: &str,
    ) -> Result<MossCandidateModelPackStatus, LocalModelError> {
        self.require_moss_pack(pack_id)?;
        let catalog = MossCandidateCatalog::load()?;
        self.require_moss_memory(catalog)?;
        let _busy = self.acquire_busy()?;
        self.moss_cancel.store(false, Ordering::Release);
        {
            let mut operation = self
                .moss_operation
                .lock()
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            operation.installing = true;
            operation.installed = false;
            operation.downloaded_bytes = 0;
            operation.error_code = None;
        }
        let _state = InstallStateLease(&self.moss_operation);
        let result = self.install_moss_inner(catalog).await;
        let mut operation = self
            .moss_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        operation.installing = false;
        match result {
            Ok(()) => {
                operation.installed = true;
                operation.downloaded_bytes = catalog.total_bytes();
                operation.error_code = None;
            }
            Err(error) => {
                operation.installed = false;
                operation.error_code = Some(error.code);
                return Err(error);
            }
        }
        drop(operation);
        self.moss_snapshot(true)
    }

    pub async fn remove_moss(
        &self,
        pack_id: &str,
    ) -> Result<MossCandidateModelPackStatus, LocalModelError> {
        self.require_moss_pack(pack_id)?;
        let _busy = self.acquire_busy()?;
        let root = self.app_data_root.clone();
        let files = MossCandidateCatalog::load()?.files.clone();
        let remove_owner = Arc::clone(&_busy);
        tokio::task::spawn_blocking(move || {
            let _owner = remove_owner;
            remove_catalog_files(&root, &files)
        })
        .await
        .map_err(|_| LocalModelError::new("model_remove_failed"))??;
        *self
            .moss_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))? =
            OperationStatus::default();
        self.moss_snapshot(true)
    }

    async fn install_moss_inner(
        &self,
        catalog: &MossCandidateCatalog,
    ) -> Result<(), LocalModelError> {
        for file in &catalog.files {
            self.check_moss_cancel()?;
            if let Some(path) =
                resolve_install_file(&self.app_data_root, &file.install_path, false)?
            {
                if file_matches(&path, file) {
                    self.add_moss_progress(file.size_bytes)?;
                    continue;
                }
                remove_regular_file(&path)?;
            }
            let destination = ensure_install_destination(&self.app_data_root, &file.install_path)?;
            let partial = partial_path(&destination)?;
            validate_partial(&partial, file.size_bytes)?;
            let result = self.download_moss_file(catalog, file, &partial).await;
            if result.as_ref().is_err_and(|error| {
                matches!(
                    error.code,
                    "model_download_rejected" | "model_verification_failed"
                )
            }) {
                let _ = remove_regular_file(&partial);
            }
            result?;
            self.check_moss_cancel()?;
            if destination.exists() {
                return Err(LocalModelError::new("model_destination_conflict"));
            }
            tokio::fs::rename(&partial, &destination)
                .await
                .map_err(|_| LocalModelError::new("model_install_failed"))?;
            if !file_matches(&destination, file) {
                return Err(LocalModelError::new("model_verification_failed"));
            }
            sync_parent(&destination).await?;
        }
        let root = self.app_data_root.clone();
        let files = catalog.files.clone();
        if !tokio::task::spawn_blocking(move || {
            verify_exact_catalog_tree(&root, &files, INSTALL_PREFIX)
        })
        .await
        .map_err(|_| LocalModelError::new("model_verification_failed"))??
        {
            return Err(LocalModelError::new("model_verification_failed"));
        }
        self.check_moss_cancel()
    }

    async fn download_moss_file(
        &self,
        catalog: &MossCandidateCatalog,
        file: &CatalogFile,
        partial: &Path,
    ) -> Result<(), LocalModelError> {
        self.check_moss_cancel()?;
        validate_partial(partial, file.size_bytes)?;
        let (mut digest, mut size_bytes) =
            self.await_moss_io(partial_digest(partial, file)).await?;
        self.add_moss_progress(size_bytes)?;
        if size_bytes == file.size_bytes {
            return Ok(());
        }
        let remaining = file.size_bytes - size_bytes;
        let mut request = self.client.get(catalog.source_url(file)?);
        if size_bytes > 0 {
            request = request.header(RANGE, format!("bytes={size_bytes}-"));
        }
        let response = self
            .await_moss_io(async {
                request
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|_| LocalModelError::new("model_download_failed"))
            })
            .await?;
        if !allowed_download_url(response.url())
            || response
                .content_length()
                .is_some_and(|length| length != remaining)
            || size_bytes > 0
                && (response.status() != reqwest::StatusCode::PARTIAL_CONTENT
                    || response
                        .headers()
                        .get(CONTENT_RANGE)
                        .and_then(|value| value.to_str().ok())
                        .is_none_or(|value| {
                            !valid_content_range(value, size_bytes, file.size_bytes)
                        }))
        {
            return Err(LocalModelError::new("model_download_rejected"));
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true);
        if size_bytes == 0 {
            options.create_new(true);
        } else {
            options.append(true);
        }
        let mut output = options
            .open(partial)
            .await
            .map_err(|_| LocalModelError::new("model_install_failed"))?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = self
            .await_moss_io(async {
                stream
                    .next()
                    .await
                    .transpose()
                    .map_err(|_| LocalModelError::new("model_download_failed"))
            })
            .await?
        {
            size_bytes = size_bytes
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| LocalModelError::new("model_download_rejected"))?;
            if size_bytes > file.size_bytes {
                return Err(LocalModelError::new("model_download_rejected"));
            }
            output
                .write_all(&chunk)
                .await
                .map_err(|_| LocalModelError::new("model_install_failed"))?;
            digest.update(&chunk);
            self.add_moss_progress(chunk.len() as u64)?;
        }
        if size_bytes != file.size_bytes || hex::encode(digest.finalize()) != file.sha256 {
            return Err(LocalModelError::new("model_verification_failed"));
        }
        output
            .flush()
            .await
            .map_err(|_| LocalModelError::new("model_install_failed"))?;
        output
            .sync_all()
            .await
            .map_err(|_| LocalModelError::new("model_install_failed"))
    }

    // Cancellation must also interrupt a stalled response, not wait for a chunk.
    async fn await_moss_io<T>(
        &self,
        io: impl std::future::Future<Output = Result<T, LocalModelError>>,
    ) -> Result<T, LocalModelError> {
        tokio::select! {
            biased;
            canceled = async {
                loop {
                    if let Err(error) = self.check_moss_cancel() {
                        break error;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            } => Err(canceled),
            result = tokio::time::timeout(Duration::from_secs(60), io) => {
                result.map_err(|_| LocalModelError::new("model_download_failed"))?
            }
        }
    }

    fn check_moss_cancel(&self) -> Result<(), LocalModelError> {
        if self.moss_cancel.load(Ordering::Acquire) {
            Err(LocalModelError::new("model_install_canceled"))
        } else {
            Ok(())
        }
    }

    fn add_moss_progress(&self, bytes: u64) -> Result<(), LocalModelError> {
        let mut operation = self
            .moss_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        operation.downloaded_bytes = operation
            .downloaded_bytes
            .saturating_add(bytes)
            .min(MODEL_SIZE_BYTES);
        Ok(())
    }

    fn require_moss_pack(&self, pack_id: &str) -> Result<(), LocalModelError> {
        if pack_id == MOSS_CANDIDATE_PACK_ID {
            Ok(())
        } else {
            Err(LocalModelError::new("unknown_model_pack"))
        }
    }

    fn require_moss_memory(&self, catalog: &MossCandidateCatalog) -> Result<(), LocalModelError> {
        if system_memory_bytes().is_none_or(|bytes| bytes < catalog.minimum_memory_bytes) {
            Err(LocalModelError::new("insufficient_model_memory"))
        } else {
            Ok(())
        }
    }

    fn moss_snapshot(
        &self,
        enabled: bool,
    ) -> Result<MossCandidateModelPackStatus, LocalModelError> {
        let catalog = MossCandidateCatalog::load()?;
        let operation = self
            .moss_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        let detected_memory_bytes = system_memory_bytes();
        let mut licenses: Vec<_> = catalog
            .sources
            .iter()
            .map(|source| format!("{} · {}", source.license, source.attribution))
            .collect();
        licenses.push(format!(
            "{} · {}",
            catalog.runtime_license, catalog.runtime_attribution
        ));
        Ok(MossCandidateModelPackStatus {
            supported: crate::features::full_local_supported(),
            enabled,
            pack_id: catalog.pack_id.clone(),
            display_name: catalog.display_name.clone(),
            runtime_id: catalog.runtime_id.clone(),
            model_id: catalog.model_id.clone(),
            model_revision: catalog.model_revision.clone(),
            minimum_macos_major: catalog.minimum_macos_major,
            minimum_memory_bytes: catalog.minimum_memory_bytes,
            memory_requirement_scope: catalog.memory_requirement_scope.clone(),
            detected_memory_bytes,
            memory_sufficient: detected_memory_bytes
                .is_some_and(|bytes| bytes >= catalog.minimum_memory_bytes),
            total_bytes: catalog.total_bytes(),
            installed: operation.installed,
            installing: operation.installing,
            downloaded_bytes: operation.downloaded_bytes,
            licenses,
            error_code: operation.error_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture_catalog(bytes: &[u8]) -> MossCandidateCatalog {
        let mut catalog = MossCandidateCatalog::load().unwrap().clone();
        catalog.files[0].sha256 = hex::encode(Sha256::digest(bytes));
        catalog.files[0].size_bytes = bytes.len() as u64;
        catalog
    }

    #[test]
    fn moss_catalog_is_closed_separate_and_bound_to_worker_identity() {
        let catalog = MossCandidateCatalog::load().unwrap();
        catalog.validate().unwrap();
        assert_eq!(catalog.total_bytes(), MODEL_SIZE_BYTES);
        assert_eq!(catalog.files[0].sha256, MODEL_SHA256);
        assert_eq!(catalog.model_id, MODEL_ID);
        assert_eq!(catalog.model_revision, MODEL_REVISION);
        assert_eq!(catalog.runtime_id, RUNTIME_ID);
        assert_eq!(catalog.memory_requirement_scope, "full-local-pipeline");
        assert_eq!(catalog.sources[0].license, "Apache-2.0");
        assert_eq!(catalog.runtime_license, "MIT");
        for sibling in [
            &LocalPackCatalog::load().unwrap().files,
            &QwenCandidateCatalog::load().unwrap().files,
            &SpeakerKitCandidateCatalog::load().unwrap().files,
        ] {
            assert!(sibling
                .iter()
                .all(|file| !file.install_path.starts_with("models/moss/")));
        }
        let original: serde_json::Value = serde_json::from_str(CATALOG_JSON).unwrap();
        for (pointer, value) in [
            ("/schema_version", serde_json::json!(2)),
            ("/pack_id", serde_json::json!(FULL_LOCAL_PACK_ID)),
            ("/runtime_id", serde_json::json!("other-runtime")),
            ("/runtime_license", serde_json::json!("Apache-2.0")),
            ("/model_id", serde_json::json!("whisper")),
            ("/model_revision", serde_json::json!("main")),
            ("/minimum_macos_major", serde_json::json!(13)),
            ("/minimum_memory_bytes", serde_json::json!(1)),
            (
                "/memory_requirement_scope",
                serde_json::json!("model-alone"),
            ),
            (
                "/sources/0/base_url",
                serde_json::json!("https://attacker.invalid/model"),
            ),
            ("/sources/0/license", serde_json::json!("MIT")),
            ("/files/0/source_id", serde_json::json!("other")),
            ("/files/0/source_path", serde_json::json!("../model.gguf")),
            (
                "/files/0/install_path",
                serde_json::json!("models/whisper/model.gguf"),
            ),
            ("/files/0/sha256", serde_json::json!("0".repeat(64))),
            (
                "/files/0/size_bytes",
                serde_json::json!(MODEL_SIZE_BYTES + 1),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            let changed: MossCandidateCatalog = serde_json::from_value(changed).unwrap();
            assert_eq!(
                changed.validate().unwrap_err().code,
                "invalid_catalog",
                "{pointer}"
            );
        }
        let mut changed = original;
        changed["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<MossCandidateCatalog>(changed).is_err());
        let mut changed = catalog.clone();
        changed.files.push(changed.files[0].clone());
        assert_eq!(changed.validate().unwrap_err().code, "invalid_catalog");
    }

    #[tokio::test]
    async fn compiled_preparation_pins_are_metadata_not_installed_byte_proof() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let (model, speakers, summary) = preparation_model_pins().unwrap();
        validate_preparation_model_pins(&model, &speakers, &summary).unwrap();
        assert_eq!(model.model_id, MODEL_ID);
        assert_eq!(speakers.model_files.len(), 29);
        assert!(summary.transcript_sha256.is_none());
        assert!(!manager.moss_status(true).await.unwrap().installed);
        assert!(!root.path().join("models").exists());
        let features = RuntimeFeatures {
            recording: true,
            audio_import: true,
            direct_processing: true,
            browser_capture: true,
            local_stt: true,
            local_qwen_candidate: true,
            local_speakerkit_candidate: true,
            local_moss_candidate: false,
        };
        assert!(require_moss_candidate(&features).is_err());
    }

    #[tokio::test]
    async fn moss_status_and_removal_preserve_sibling_packs_and_unknown_files() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let status = manager.moss_status(false).await.unwrap();
        assert_eq!(status.pack_id, MOSS_CANDIDATE_PACK_ID);
        assert_eq!(status.total_bytes, MODEL_SIZE_BYTES);
        assert!(!status.enabled);
        assert!(!status.installed);
        let catalog = MossCandidateCatalog::load().unwrap();
        let destination =
            ensure_install_destination(root.path(), &catalog.files[0].install_path).unwrap();
        let partial = partial_path(&destination).unwrap();
        std::fs::write(&partial, b"resumable-fragment").unwrap();
        std::fs::write(&destination, b"invalid-installed-fragment").unwrap();
        let unknown = destination.parent().unwrap().join("user-note.txt");
        std::fs::write(&unknown, b"keep").unwrap();
        let siblings = [
            "models/whisper/keep",
            "models/qwen/keep",
            "models/diarization/speakerkit-v1/keep",
            "models/moss/another-pack/keep",
        ];
        for relative in siblings {
            let path = root.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"keep").unwrap();
        }
        let staged = manager.moss_status(true).await.unwrap();
        assert!(!staged.installed);
        assert_eq!(staged.downloaded_bytes, b"resumable-fragment".len() as u64);
        let removed = manager.remove_moss(MOSS_CANDIDATE_PACK_ID).await.unwrap();
        assert!(!removed.installed);
        assert_eq!(removed.downloaded_bytes, 0);
        assert!(!destination.exists());
        assert!(!partial.exists());
        assert_eq!(std::fs::read(unknown).unwrap(), b"keep");
        for relative in siblings {
            assert_eq!(std::fs::read(root.path().join(relative)).unwrap(), b"keep");
        }
    }

    #[tokio::test]
    async fn moss_complete_partial_is_verified_and_atomically_published_without_network() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let bytes = b"tiny-offline-model-fixture";
        let catalog = fixture_catalog(bytes);
        let destination =
            ensure_install_destination(root.path(), &catalog.files[0].install_path).unwrap();
        let partial = partial_path(&destination).unwrap();
        std::fs::write(&partial, bytes).unwrap();
        manager.install_moss_inner(&catalog).await.unwrap();
        assert!(!partial.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        assert!(
            verify_exact_catalog_tree(&manager.app_data_root, &catalog.files, INSTALL_PREFIX)
                .unwrap()
        );
        assert_eq!(
            manager.moss_snapshot(true).unwrap().downloaded_bytes,
            bytes.len() as u64
        );
        // Extra data cannot become a proof, and removal never recurses into it.
        let unknown = destination.parent().unwrap().join("extra.gguf");
        std::fs::write(&unknown, b"keep").unwrap();
        assert!(
            !verify_exact_catalog_tree(&manager.app_data_root, &catalog.files, INSTALL_PREFIX)
                .unwrap()
        );
        manager.remove_moss(MOSS_CANDIDATE_PACK_ID).await.unwrap();
        assert_eq!(std::fs::read(unknown).unwrap(), b"keep");
    }

    #[tokio::test]
    async fn moss_partial_hash_and_size_fail_closed_without_network() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let catalog = fixture_catalog(b"expected");
        let destination =
            ensure_install_destination(root.path(), &catalog.files[0].install_path).unwrap();
        let partial = partial_path(&destination).unwrap();
        std::fs::write(&partial, b"tampered").unwrap();
        assert_eq!(
            manager.install_moss_inner(&catalog).await.unwrap_err().code,
            "model_verification_failed"
        );
        assert!(!destination.exists());
        assert!(!partial.exists());
        std::fs::write(&partial, b"oversized-fragment").unwrap();
        assert_eq!(
            manager.install_moss_inner(&catalog).await.unwrap_err().code,
            "model_path_rejected"
        );
        assert!(!destination.exists());
        assert!(partial.exists());
        assert!(valid_content_range("bytes 2-7/8", 2, 8));
        assert!(!valid_content_range("bytes 0-7/8", 2, 8));
        assert!(!valid_content_range("bytes 2-8/9", 2, 8));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn moss_rejects_symlinked_pack_model_and_partial_without_touching_targets() {
        use std::os::unix::fs::symlink;
        for variant in ["pack", "model", "partial"] {
            let root = TempDir::new().unwrap();
            let outside = TempDir::new().unwrap();
            let target = outside.path().join("keep");
            std::fs::write(&target, b"keep").unwrap();
            let manager = LocalModelPackManager::open(root.path()).unwrap();
            let catalog = MossCandidateCatalog::load().unwrap();
            let destination =
                ensure_install_destination(root.path(), &catalog.files[0].install_path).unwrap();
            match variant {
                "pack" => {
                    std::fs::remove_dir(destination.parent().unwrap()).unwrap();
                    symlink(outside.path(), destination.parent().unwrap()).unwrap();
                }
                "model" => symlink(&target, &destination).unwrap(),
                _ => symlink(&target, partial_path(&destination).unwrap()).unwrap(),
            }
            assert_eq!(
                manager.moss_status(true).await.unwrap_err().code,
                "model_path_rejected",
                "{variant}"
            );
            assert_eq!(
                manager
                    .remove_moss(MOSS_CANDIDATE_PACK_ID)
                    .await
                    .unwrap_err()
                    .code,
                "model_path_rejected",
                "{variant}"
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        }
    }

    #[tokio::test]
    async fn moss_busy_cancel_and_progress_are_isolated_and_bounded() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        for invalid in [
            FULL_LOCAL_PACK_ID,
            QWEN_CANDIDATE_PACK_ID,
            SPEAKERKIT_CANDIDATE_PACK_ID,
            "../moss",
        ] {
            assert_eq!(
                manager.install_moss(invalid).await.unwrap_err().code,
                "unknown_model_pack"
            );
            assert_eq!(
                manager.remove_moss(invalid).await.unwrap_err().code,
                "unknown_model_pack"
            );
        }
        let lease = manager.acquire_busy().unwrap();
        assert_eq!(
            manager
                .remove_moss(MOSS_CANDIDATE_PACK_ID)
                .await
                .unwrap_err()
                .code,
            "model_operation_in_progress"
        );
        assert_eq!(
            manager.moss_status(true).await.unwrap_err().code,
            "model_operation_in_progress"
        );
        drop(lease);
        manager.cancel_install(MOSS_CANDIDATE_PACK_ID).unwrap();
        assert!(manager.moss_cancel.load(Ordering::Acquire));
        assert!(!manager.cancel.load(Ordering::Acquire));
        assert!(!manager.qwen_cancel.load(Ordering::Acquire));
        assert!(!manager.speakerkit_cancel.load(Ordering::Acquire));
        assert_eq!(
            manager
                .install_moss_inner(MossCandidateCatalog::load().unwrap())
                .await
                .unwrap_err()
                .code,
            "model_install_canceled"
        );
        assert!(!root.path().join("models").exists());
        manager.add_moss_progress(u64::MAX).unwrap();
        manager.add_moss_progress(1).unwrap();
        assert_eq!(
            manager.moss_snapshot(true).unwrap().downloaded_bytes,
            MODEL_SIZE_BYTES
        );
        assert_eq!(
            manager.speakerkit_snapshot(true).unwrap().downloaded_bytes,
            0
        );
    }

    #[tokio::test]
    async fn moss_cancellation_interrupts_stalled_io_and_dropped_install_state_recovers() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let wait = manager.await_moss_io(std::future::pending::<Result<(), LocalModelError>>());
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            manager.cancel_install(MOSS_CANDIDATE_PACK_ID).unwrap();
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(wait, cancel) })
                .await
                .unwrap();
        assert_eq!(result.unwrap_err().code, "model_install_canceled");
        manager.moss_operation.lock().unwrap().installing = true;
        drop(InstallStateLease(&manager.moss_operation));
        let recovered = manager.moss_snapshot(true).unwrap();
        assert!(!recovered.installing);
        assert!(!recovered.installed);
        assert_eq!(recovered.error_code, Some("model_install_canceled"));
        // A normally completed install isn't changed by the cancellation guard.
        manager.moss_operation.lock().unwrap().installed = true;
        drop(InstallStateLease(&manager.moss_operation));
        assert!(manager.moss_snapshot(true).unwrap().installed);
    }
}
