//! Complete MOSS pipeline pack, composed from existing pinned component
//! catalogs. Shared SpeakerKit/summary bytes are reused, never duplicated.
use super::*;
use uuid::Uuid;

mod download;
mod files;
#[cfg(test)]
mod tests;

pub const MOSS_PIPELINE_PACK_ID: &str = "moss-full-local-v1";
const MINIMUM_MEMORY_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MossPipelineComponentStatus {
    pub id: &'static str,
    pub display_name: String,
    pub installed: bool,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MossPipelineModelStatus {
    pub supported: bool,
    pub enabled: bool,
    pub pack_id: &'static str,
    pub installed: bool,
    pub installing: bool,
    pub memory_sufficient: bool,
    pub minimum_memory_bytes: u64,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub components: Vec<MossPipelineComponentStatus>,
    pub licenses: Vec<String>,
    pub error_code: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct MossPipelineModelProof {
    pub moss: MossCandidateModelPackProof,
    pub speakerkit: SpeakerKitCandidateModelPackProof,
    pub summary: crate::processing::LocalSummaryCheckpoint,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveMossPipelineRequest {
    pub pack_id: String,
    /// Shared files are retained unless the UI explicitly confirms their
    /// removal also affects Qwen/Whisper or other local workflows using them.
    #[serde(default)]
    pub remove_shared: bool,
}

#[derive(Clone)]
struct PackFile {
    file: CatalogFile,
    url: reqwest::Url,
}
#[derive(Clone)]
struct ComponentPack {
    id: &'static str,
    display_name: String,
    prefix: String,
    files: Vec<PackFile>,
}
impl ComponentPack {
    fn total_bytes(&self) -> u64 {
        self.files.iter().map(|entry| entry.file.size_bytes).sum()
    }
    fn plain_files(&self) -> Vec<CatalogFile> {
        self.files.iter().map(|entry| entry.file.clone()).collect()
    }
}

#[derive(Clone)]
struct PipelineCatalog {
    components: Vec<ComponentPack>,
    licenses: Vec<String>,
}
impl PipelineCatalog {
    fn load() -> Result<Self, LocalModelError> {
        let moss = moss::MossCandidateCatalog::load()?;
        let speakers = SpeakerKitCandidateCatalog::load()?;
        let full = LocalPackCatalog::load()?;
        let summary = full.summary_model_file()?;
        let summary_source = full.source(&summary.source_id)?;
        let mut licenses = moss.pipeline_licenses();
        licenses.extend(
            speakers
                .sources
                .iter()
                .map(|source| format!("{} · {}", source.license, source.attribution)),
        );
        licenses.push(format!(
            "{} · {}",
            summary_source.license, summary_source.attribution
        ));
        let catalog = Self {
            components: vec![
                ComponentPack {
                    id: "moss",
                    display_name: "MOSS Transcribe Diarize Q8_0".into(),
                    prefix: format!("models/moss/{}/", echowall_local_moss_protocol::MODEL_ID),
                    files: moss
                        .pipeline_files()?
                        .into_iter()
                        .map(|(file, url)| PackFile { file, url })
                        .collect(),
                },
                ComponentPack {
                    id: "speakerkit",
                    display_name: "SpeakerKit Pyannote v3".into(),
                    prefix: SPEAKERKIT_INSTALL_PREFIX.into(),
                    files: speakers
                        .files
                        .iter()
                        .map(|file| {
                            Ok(PackFile {
                                file: file.clone(),
                                url: speakers.source_url(file)?,
                            })
                        })
                        .collect::<Result<_, LocalModelError>>()?,
                },
                ComponentPack {
                    id: "summary",
                    display_name: "Qwen3.8 27B local summary".into(),
                    prefix: format!("models/summary/{}/", full.summary_model_id),
                    files: vec![PackFile {
                        file: summary.clone(),
                        url: full.source_url(summary)?,
                    }],
                },
            ],
            licenses,
        };
        let mut paths = HashSet::new();
        for component in &catalog.components {
            for entry in &component.files {
                if !entry.file.install_path.starts_with(&component.prefix)
                    || !paths.insert(&entry.file.install_path)
                {
                    return Err(fail("invalid_catalog"));
                }
            }
        }
        if paths.len() != 31 || catalog.total_bytes() > MAX_CATALOG_BYTES {
            return Err(fail("invalid_catalog"));
        }
        Ok(catalog)
    }
    fn total_bytes(&self) -> u64 {
        self.components.iter().map(ComponentPack::total_bytes).sum()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OperationKind {
    Inspect,
    Install,
    Remove(bool),
    Proof,
}
struct ActiveOperation {
    token: Uuid,
    kind: OperationKind,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub(super) struct PipelineState {
    active: Option<ActiveOperation>,
    cache: HashMap<String, files::CachedFile>,
    component_installed: HashMap<&'static str, bool>,
    installed: bool,
    downloaded_bytes: u64,
    error_code: Option<&'static str>,
    #[cfg(test)]
    hashed_files: usize,
}

struct OperationLease {
    token: Uuid,
    state: Arc<Mutex<PipelineState>>,
    // The last task/thread doing I/O retains mutation ownership even if the
    // IPC future is dropped. No detached writer can race the next operation.
    _busy: Arc<BusyLease>,
}
impl Drop for OperationLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.token == self.token)
            {
                state.active = None;
                state.installed = false;
                state.error_code = Some("model_install_canceled");
            }
        }
    }
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
struct Work {
    root: PathBuf,
    client: reqwest::Client,
    catalog: Arc<PipelineCatalog>,
    state: Arc<Mutex<PipelineState>>,
    cancel: Arc<AtomicBool>,
    lease: Arc<OperationLease>,
    #[cfg(test)]
    forbid_network: bool,
}
impl Work {
    fn check_cancel(&self) -> Result<(), LocalModelError> {
        if self.cancel.load(Ordering::Acquire) {
            Err(fail("model_install_canceled"))
        } else {
            Ok(())
        }
    }
    fn with_state<T>(
        &self,
        call: impl FnOnce(&mut PipelineState) -> T,
    ) -> Result<T, LocalModelError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| fail("model_state_unavailable"))?;
        if state
            .active
            .as_ref()
            .is_none_or(|active| active.token != self.lease.token)
        {
            return Err(fail("model_operation_in_progress"));
        }
        Ok(call(&mut state))
    }
    fn add_progress(&self, amount: u64) -> Result<(), LocalModelError> {
        self.with_state(|state| {
            state.downloaded_bytes = state
                .downloaded_bytes
                .saturating_add(amount)
                .min(self.catalog.total_bytes())
        })
    }
    async fn scan(&self, force: bool) -> Result<(), LocalModelError> {
        let work = self.clone();
        tokio::task::spawn_blocking(move || files::scan(&work, force))
            .await
            .map_err(|_| fail("model_state_unavailable"))?
    }
    async fn execute(&self, kind: OperationKind) -> Result<(), LocalModelError> {
        match kind {
            OperationKind::Inspect => self.scan(false).await,
            OperationKind::Proof => {
                self.scan(true).await?;
                if !self.with_state(|state| state.installed)? {
                    return Err(fail("model_pack_not_installed"));
                }
                Ok(())
            }
            OperationKind::Install => {
                self.with_state(|state| {
                    state.downloaded_bytes = 0;
                    state.installed = false;
                })?;
                for component in &self.catalog.components {
                    for entry in &component.files {
                        self.check_cancel()?;
                        self.install_file(entry).await?;
                    }
                    let work = self.clone();
                    let component = component.clone();
                    tokio::task::spawn_blocking(move || {
                        files::verify_component(&work, &component, false)
                    })
                    .await
                    .map_err(|_| fail("model_state_unavailable"))??;
                }
                self.scan(false).await?;
                if !self.with_state(|state| state.installed)? {
                    return Err(fail("model_verification_failed"));
                }
                Ok(())
            }
            OperationKind::Remove(shared) => {
                let work = self.clone();
                tokio::task::spawn_blocking(move || {
                    let files: Vec<_> = work
                        .catalog
                        .components
                        .iter()
                        .filter(|component| shared || component.id == "moss")
                        .flat_map(ComponentPack::plain_files)
                        .collect();
                    for file in &files {
                        work.check_cancel()?;
                        remove_catalog_files(&work.root, std::slice::from_ref(file))?;
                    }
                    work.with_state(|state| {
                        for file in &files {
                            state.cache.remove(&file.install_path);
                        }
                    })?;
                    Ok::<_, LocalModelError>(())
                })
                .await
                .map_err(|_| fail("model_remove_failed"))??;
                self.scan(false).await
            }
        }
    }
}

impl LocalModelPackManager {
    pub async fn moss_pipeline_status(
        &self,
        enabled: bool,
    ) -> Result<MossPipelineModelStatus, LocalModelError> {
        let catalog = PipelineCatalog::load()?;
        if self
            .moss_pipeline
            .lock()
            .map_err(|_| fail("model_state_unavailable"))?
            .active
            .is_some()
        {
            return pipeline_snapshot(&self.moss_pipeline, &catalog, enabled, false);
        }
        match self
            .run_pipeline(OperationKind::Inspect, catalog.clone(), false)
            .await
        {
            Ok(()) => pipeline_snapshot(&self.moss_pipeline, &catalog, enabled, false),
            Err(error) if error.code == "model_operation_in_progress" => {
                pipeline_snapshot(&self.moss_pipeline, &catalog, enabled, true)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn install_moss_pipeline(
        &self,
        pack_id: &str,
    ) -> Result<MossPipelineModelStatus, LocalModelError> {
        require_pack(pack_id)?;
        require_memory()?;
        let catalog = PipelineCatalog::load()?;
        self.run_pipeline(OperationKind::Install, catalog.clone(), false)
            .await?;
        pipeline_snapshot(&self.moss_pipeline, &catalog, true, false)
    }

    pub async fn remove_moss_pipeline(
        &self,
        pack_id: &str,
        remove_shared: bool,
    ) -> Result<MossPipelineModelStatus, LocalModelError> {
        require_pack(pack_id)?;
        let catalog = PipelineCatalog::load()?;
        self.run_pipeline(OperationKind::Remove(remove_shared), catalog.clone(), false)
            .await?;
        pipeline_snapshot(&self.moss_pipeline, &catalog, true, false)
    }

    pub async fn moss_pipeline_proof(&self) -> Result<MossPipelineModelProof, LocalModelError> {
        require_memory()?;
        self.run_pipeline(OperationKind::Proof, PipelineCatalog::load()?, false)
            .await?;
        let (moss, speakerkit, summary) = moss::preparation_model_pins()?;
        Ok(MossPipelineModelProof {
            moss,
            speakerkit,
            summary,
        })
    }

    pub(super) fn cancel_moss_pipeline_install(&self) -> Result<(), LocalModelError> {
        let state = self
            .moss_pipeline
            .lock()
            .map_err(|_| fail("model_state_unavailable"))?;
        if let Some(active) = &state.active {
            if active.kind == OperationKind::Install {
                active.cancel.store(true, Ordering::Release);
            }
        }
        Ok(())
    }

    fn start_pipeline(
        &self,
        kind: OperationKind,
        catalog: PipelineCatalog,
        forbid_network: bool,
    ) -> Result<Work, LocalModelError> {
        let busy = self.acquire_busy()?;
        let token = Uuid::new_v4();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut state = self
                .moss_pipeline
                .lock()
                .map_err(|_| fail("model_state_unavailable"))?;
            state.active = Some(ActiveOperation {
                token,
                kind,
                cancel: Arc::clone(&cancel),
            });
            state.installed = false;
            state.error_code = None;
        }
        #[cfg(not(test))]
        let _ = forbid_network;
        Ok(Work {
            root: self.app_data_root.clone(),
            client: self.client.clone(),
            catalog: Arc::new(catalog),
            state: Arc::clone(&self.moss_pipeline),
            cancel,
            lease: Arc::new(OperationLease {
                token,
                state: Arc::clone(&self.moss_pipeline),
                _busy: busy,
            }),
            #[cfg(test)]
            forbid_network,
        })
    }

    async fn run_pipeline(
        &self,
        kind: OperationKind,
        catalog: PipelineCatalog,
        forbid_network: bool,
    ) -> Result<(), LocalModelError> {
        let work = self.start_pipeline(kind, catalog, forbid_network)?;
        let _cancel_on_drop = CancelOnDrop(Arc::clone(&work.cancel));
        // The owned task is one operation, not a service. Dropping the IPC
        // request signals only its token; the I/O owner drains before release.
        tokio::spawn(async move {
            let result = work.execute(kind).await;
            work.with_state(|state| {
                if let Err(error) = &result {
                    state.installed = false;
                    state.error_code = Some(error.code);
                }
                state.active = None;
            })?;
            result
        })
        .await
        .map_err(|_| fail("model_state_unavailable"))?
    }
}

fn pipeline_snapshot(
    state: &Mutex<PipelineState>,
    catalog: &PipelineCatalog,
    enabled: bool,
    busy: bool,
) -> Result<MossPipelineModelStatus, LocalModelError> {
    let state = state.lock().map_err(|_| fail("model_state_unavailable"))?;
    Ok(MossPipelineModelStatus {
        supported: crate::features::full_local_supported(),
        enabled,
        pack_id: MOSS_PIPELINE_PACK_ID,
        installed: state.installed && !busy && state.active.is_none(),
        installing: state
            .active
            .as_ref()
            .is_some_and(|active| active.kind == OperationKind::Install),
        memory_sufficient: system_memory_bytes().is_some_and(|bytes| bytes >= MINIMUM_MEMORY_BYTES),
        minimum_memory_bytes: MINIMUM_MEMORY_BYTES,
        total_bytes: catalog.total_bytes(),
        downloaded_bytes: state.downloaded_bytes.min(catalog.total_bytes()),
        components: catalog
            .components
            .iter()
            .map(|component| MossPipelineComponentStatus {
                id: component.id,
                display_name: component.display_name.clone(),
                installed: state
                    .component_installed
                    .get(component.id)
                    .copied()
                    .unwrap_or(false),
                total_bytes: component.total_bytes(),
            })
            .collect(),
        licenses: catalog.licenses.clone(),
        error_code: if busy {
            Some("model_operation_in_progress")
        } else {
            state.error_code
        },
    })
}
fn require_pack(pack_id: &str) -> Result<(), LocalModelError> {
    if pack_id != MOSS_PIPELINE_PACK_ID {
        Err(fail("unknown_model_pack"))
    } else {
        Ok(())
    }
}
fn require_memory() -> Result<(), LocalModelError> {
    if system_memory_bytes().is_none_or(|bytes| bytes < MINIMUM_MEMORY_BYTES) {
        Err(fail("insufficient_model_memory"))
    } else {
        Ok(())
    }
}
fn require_feature(features: &RuntimeFeatures) -> Result<(), String> {
    if !features.local_moss_candidate || !crate::features::full_local_supported() {
        Err("MOSS candidate mode is unavailable".into())
    } else {
        Ok(())
    }
}
fn fail(code: &'static str) -> LocalModelError {
    LocalModelError::new(code)
}

#[tauri::command]
pub async fn moss_pipeline_model_status(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
) -> Result<MossPipelineModelStatus, String> {
    require_feature(&features)?;
    state
        .manager()
        .moss_pipeline_status(features.local_moss_candidate)
        .await
        .map_err(safe_error)
}
#[tauri::command]
pub async fn install_moss_pipeline_model_pack(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
    request: LocalModelPackRequest,
) -> Result<MossPipelineModelStatus, String> {
    require_feature(&features)?;
    state
        .manager()
        .install_moss_pipeline(&request.pack_id)
        .await
        .map_err(safe_error)
}
#[tauri::command]
pub async fn remove_moss_pipeline_model_pack(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
    request: RemoveMossPipelineRequest,
) -> Result<MossPipelineModelStatus, String> {
    require_feature(&features)?;
    state
        .manager()
        .remove_moss_pipeline(&request.pack_id, request.remove_shared)
        .await
        .map_err(safe_error)
}
