//! Explicit, hash-pinned model-pack manager for full local mode.
//!
//! The catalog is compiled into the App, but model bytes are never bundled or
//! downloaded implicitly. Installation is a user action; every redirect,
//! byte count, path, and SHA-256 is checked before an atomic rename into the
//! App-owned model directory.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use echowall_local_qwen_protocol::{
    LocalQwenModelFileIdentity, LOCAL_QWEN_ALIGNER_FILES, LOCAL_QWEN_ALIGNER_MODEL_ID,
    LOCAL_QWEN_ALIGNER_REVISION, LOCAL_QWEN_ASR_FILES, LOCAL_QWEN_ASR_MODEL_ID,
    LOCAL_QWEN_ASR_REVISION, LOCAL_QWEN_RUNTIME_ID,
};
use echowall_local_whisper_protocol::{
    LOCAL_DIARIZATION_SPEAKERKIT_FILES, LOCAL_DIARIZATION_SPEAKERKIT_PRESET,
};
use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::features::RuntimeFeatures;
use crate::ingest::envelope::validate_sha256;
use crate::ingest::inbox::hash_file_streaming;

use super::local_whisper::LocalModelFileIdentity;

pub mod moss;
pub mod moss_pipeline;
pub use moss::{MossCandidateModelPackProof, MossCandidateModelPackStatus, MOSS_CANDIDATE_PACK_ID};

pub const FULL_LOCAL_PACK_ID: &str = "full-local-v1";
pub const QWEN_CANDIDATE_PACK_ID: &str = "qwen-asr-candidate-v1";
pub const SPEAKERKIT_CANDIDATE_PACK_ID: &str = "speakerkit-v1";
const CATALOG_JSON: &str = include_str!("../../../model-catalog/full-local-v1.json");
const QWEN_CATALOG_JSON: &str = include_str!("../../../model-catalog/qwen-asr-candidate-v1.json");
const SPEAKERKIT_CATALOG_JSON: &str =
    include_str!("../../../model-catalog/speakerkit-candidate-v1.json");
const DIARIZATION_INSTALL_PREFIX: &str = "models/diarization/fluid-v1/";
const SPEAKERKIT_INSTALL_PREFIX: &str = "models/diarization/speakerkit-v1/";
const SUMMARY_INSTALL_PREFIX: &str = "models/summary/";
const MAX_CATALOG_FILES: usize = 64;
const MAX_CATALOG_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalPackCatalog {
    schema_version: u32,
    pack_id: String,
    display_name: String,
    whisper_model_id: String,
    diarization_pack_id: String,
    diarization_default: bool,
    summary_model_id: String,
    minimum_macos_major: u32,
    minimum_memory_bytes: u64,
    sources: Vec<CatalogSource>,
    files: Vec<CatalogFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogSource {
    id: String,
    base_url: String,
    license: String,
    attribution: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    source_id: String,
    source_path: String,
    install_path: String,
    sha256: String,
    size_bytes: u64,
}

impl LocalPackCatalog {
    fn load() -> Result<&'static Self, LocalModelError> {
        static CATALOG: OnceLock<Result<LocalPackCatalog, LocalModelError>> = OnceLock::new();
        CATALOG
            .get_or_init(|| {
                let catalog: LocalPackCatalog = serde_json::from_str(CATALOG_JSON)
                    .map_err(|_| LocalModelError::new("invalid_catalog"))?;
                catalog.validate()?;
                Ok(catalog)
            })
            .as_ref()
            .map_err(|error| *error)
    }

    fn validate(&self) -> Result<(), LocalModelError> {
        if self.schema_version != 1
            || self.pack_id != FULL_LOCAL_PACK_ID
            || self.display_name.is_empty()
            || self.display_name.len() > 160
            || self.whisper_model_id != "large-v3-turbo-q5_0"
            || self.diarization_pack_id != "fluid-v1"
            || !self.diarization_default
            || self.summary_model_id != "qwen3.8-27b-ud-q4-k-xl"
            || self.minimum_macos_major != 14
            || self.minimum_memory_bytes != 32 * 1024 * 1024 * 1024
            || self.sources.len() != 3
            || self.files.len() != 23
            || self.files.len() > MAX_CATALOG_FILES
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        let mut source_ids = HashSet::new();
        let mut sources = HashMap::new();
        for source in &self.sources {
            if !valid_identifier(&source.id)
                || !source_ids.insert(&source.id)
                || source.license.is_empty()
                || source.license.len() > 64
                || source.attribution.is_empty()
                || source.attribution.len() > 512
            {
                return Err(LocalModelError::new("invalid_catalog"));
            }
            let url = reqwest::Url::parse(&source.base_url)
                .map_err(|_| LocalModelError::new("invalid_catalog"))?;
            if url.scheme() != "https"
                || url.host_str() != Some("huggingface.co")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.port().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || !pinned_huggingface_base(&url)
            {
                return Err(LocalModelError::new("invalid_catalog"));
            }
            sources.insert(source.id.as_str(), source);
        }
        let mut install_paths = HashSet::new();
        let mut total = 0_u64;
        for file in &self.files {
            if !sources.contains_key(file.source_id.as_str())
                || !valid_source_path(&file.source_path)
                || !valid_relative_path(&file.install_path)
                || !install_paths.insert(&file.install_path)
                || validate_sha256(&file.sha256, "model.sha256").is_err()
                || file.size_bytes == 0
            {
                return Err(LocalModelError::new("invalid_catalog"));
            }
            total = total
                .checked_add(file.size_bytes)
                .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
        }
        if total > MAX_CATALOG_BYTES
            || self
                .files
                .iter()
                .filter(|file| file.install_path.starts_with("models/whisper/"))
                .count()
                != 1
            || self.summary_model_file().is_err()
            || self.diarization_model_files()?.len() != 21
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(())
    }

    fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size_bytes).sum()
    }

    fn source(&self, id: &str) -> Result<&CatalogSource, LocalModelError> {
        self.sources
            .iter()
            .find(|source| source.id == id)
            .ok_or_else(|| LocalModelError::new("invalid_catalog"))
    }

    fn source_url(&self, file: &CatalogFile) -> Result<reqwest::Url, LocalModelError> {
        let source = self.source(&file.source_id)?;
        let url = reqwest::Url::parse(&format!(
            "{}/{}",
            source.base_url.trim_end_matches('/'),
            file.source_path
        ))
        .map_err(|_| LocalModelError::new("invalid_catalog"))?;
        if !allowed_download_url(&url) || url.query().is_some() || url.fragment().is_some() {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(url)
    }

    pub fn diarization_model_files(&self) -> Result<Vec<LocalModelFileIdentity>, LocalModelError> {
        let mut files = Vec::new();
        for file in &self.files {
            let Some(relative_path) = file.install_path.strip_prefix(DIARIZATION_INSTALL_PREFIX)
            else {
                continue;
            };
            files.push(LocalModelFileIdentity {
                relative_path: relative_path.to_owned(),
                sha256: file.sha256.clone(),
                size_bytes: file.size_bytes,
            });
        }
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        if files.len() != 21 {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(files)
    }

    fn summary_model_file(&self) -> Result<&CatalogFile, LocalModelError> {
        let mut files = self
            .files
            .iter()
            .filter(|file| file.install_path.starts_with(SUMMARY_INSTALL_PREFIX));
        let file = files
            .next()
            .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
        if files.next().is_some()
            || file.install_path != format!("models/summary/{}/model.gguf", self.summary_model_id)
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(file)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct QwenCandidateCatalog {
    schema_version: u32,
    pack_id: String,
    display_name: String,
    runtime_id: String,
    asr_model_id: String,
    asr_model_revision: String,
    aligner_model_id: String,
    aligner_model_revision: String,
    minimum_macos_major: u32,
    minimum_memory_bytes: u64,
    sources: Vec<CatalogSource>,
    files: Vec<CatalogFile>,
}

impl QwenCandidateCatalog {
    fn load() -> Result<&'static Self, LocalModelError> {
        static CATALOG: OnceLock<Result<QwenCandidateCatalog, LocalModelError>> = OnceLock::new();
        CATALOG
            .get_or_init(|| {
                let catalog: QwenCandidateCatalog = serde_json::from_str(QWEN_CATALOG_JSON)
                    .map_err(|_| LocalModelError::new("invalid_catalog"))?;
                catalog.validate()?;
                Ok(catalog)
            })
            .as_ref()
            .map_err(|error| *error)
    }

    fn validate(&self) -> Result<(), LocalModelError> {
        if self.schema_version != 1
            || self.pack_id != QWEN_CANDIDATE_PACK_ID
            || self.display_name.is_empty()
            || self.display_name.len() > 160
            || self.runtime_id != LOCAL_QWEN_RUNTIME_ID
            || self.asr_model_id != LOCAL_QWEN_ASR_MODEL_ID
            || self.asr_model_revision != LOCAL_QWEN_ASR_REVISION
            || self.aligner_model_id != LOCAL_QWEN_ALIGNER_MODEL_ID
            || self.aligner_model_revision != LOCAL_QWEN_ALIGNER_REVISION
            || self.minimum_macos_major != 14
            || self.minimum_memory_bytes != 32 * 1024 * 1024 * 1024
            || self.sources.len() != 2
            || self.files.len() != 5
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        let mut source_ids = HashSet::new();
        for source in &self.sources {
            let url = reqwest::Url::parse(&source.base_url)
                .map_err(|_| LocalModelError::new("invalid_catalog"))?;
            if !valid_identifier(&source.id)
                || !source_ids.insert(source.id.as_str())
                || source.license != "Apache-2.0"
                || source.attribution.is_empty()
                || source.attribution.len() > 512
                || !pinned_huggingface_base(&url)
            {
                return Err(LocalModelError::new("invalid_catalog"));
            }
        }
        let exact_paths: Vec<String> =
            LOCAL_QWEN_ASR_FILES
                .iter()
                .map(|path| format!("models/qwen/asr/{LOCAL_QWEN_ASR_MODEL_ID}/{path}"))
                .chain(LOCAL_QWEN_ALIGNER_FILES.iter().map(|path| {
                    format!("models/qwen/aligner/{LOCAL_QWEN_ALIGNER_MODEL_ID}/{path}")
                }))
                .collect();
        let mut total = 0_u64;
        for (file, exact_path) in self.files.iter().zip(&exact_paths) {
            if !source_ids.contains(file.source_id.as_str())
                || !valid_source_path(&file.source_path)
                || file.install_path != *exact_path
                || validate_sha256(&file.sha256, "model.sha256").is_err()
                || file.size_bytes == 0
            {
                return Err(LocalModelError::new("invalid_catalog"));
            }
            total = total
                .checked_add(file.size_bytes)
                .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
        }
        if total != 6_539_619_722 {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(())
    }

    fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size_bytes).sum()
    }

    fn source_url(&self, file: &CatalogFile) -> Result<reqwest::Url, LocalModelError> {
        let source = self
            .sources
            .iter()
            .find(|source| source.id == file.source_id)
            .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
        let url = reqwest::Url::parse(&format!(
            "{}/{}",
            source.base_url.trim_end_matches('/'),
            file.source_path
        ))
        .map_err(|_| LocalModelError::new("invalid_catalog"))?;
        if !allowed_download_url(&url) || url.query().is_some() || url.fragment().is_some() {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(url)
    }

    fn model_files(
        &self,
        prefix: &str,
        exact_paths: &[&str],
    ) -> Result<Vec<LocalQwenModelFileIdentity>, LocalModelError> {
        let mut files = Vec::new();
        for file in &self.files {
            let Some(relative_path) = file.install_path.strip_prefix(prefix) else {
                continue;
            };
            files.push(LocalQwenModelFileIdentity {
                relative_path: relative_path.to_owned(),
                sha256: file.sha256.clone(),
                size_bytes: file.size_bytes,
            });
        }
        if files.len() != exact_paths.len()
            || files
                .iter()
                .zip(exact_paths)
                .any(|(file, path)| file.relative_path != *path)
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(files)
    }

    fn asr_files(&self) -> Result<Vec<LocalQwenModelFileIdentity>, LocalModelError> {
        self.model_files(
            &format!("models/qwen/asr/{LOCAL_QWEN_ASR_MODEL_ID}/"),
            &LOCAL_QWEN_ASR_FILES,
        )
    }

    fn aligner_files(&self) -> Result<Vec<LocalQwenModelFileIdentity>, LocalModelError> {
        self.model_files(
            &format!("models/qwen/aligner/{LOCAL_QWEN_ALIGNER_MODEL_ID}/"),
            &LOCAL_QWEN_ALIGNER_FILES,
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpeakerKitCandidateCatalog {
    schema_version: u32,
    pack_id: String,
    display_name: String,
    quality_preset: String,
    model_revision: String,
    minimum_macos_major: u32,
    minimum_memory_bytes: u64,
    sources: Vec<CatalogSource>,
    files: Vec<CatalogFile>,
}

impl SpeakerKitCandidateCatalog {
    fn load() -> Result<&'static Self, LocalModelError> {
        static CATALOG: OnceLock<Result<SpeakerKitCandidateCatalog, LocalModelError>> =
            OnceLock::new();
        CATALOG
            .get_or_init(|| {
                let catalog: SpeakerKitCandidateCatalog =
                    serde_json::from_str(SPEAKERKIT_CATALOG_JSON)
                        .map_err(|_| LocalModelError::new("invalid_catalog"))?;
                catalog.validate()?;
                Ok(catalog)
            })
            .as_ref()
            .map_err(|error| *error)
    }

    fn validate(&self) -> Result<(), LocalModelError> {
        if self.schema_version != 1
            || self.pack_id != SPEAKERKIT_CANDIDATE_PACK_ID
            || self.display_name.is_empty()
            || self.display_name.len() > 160
            || self.quality_preset != LOCAL_DIARIZATION_SPEAKERKIT_PRESET
            || !is_pinned_revision(&self.model_revision)
            || self.minimum_macos_major != 14
            || self.minimum_memory_bytes != 32 * 1024 * 1024 * 1024
            || self.sources.len() != 1
            || self.files.len() != 29
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        let source = &self.sources[0];
        let source_url = reqwest::Url::parse(&source.base_url)
            .map_err(|_| LocalModelError::new("invalid_catalog"))?;
        if source.id != "speakerkit-coreml"
            || source.license.is_empty()
            || source.license.len() > 64
            || source.attribution.is_empty()
            || source.attribution.len() > 512
            || !pinned_huggingface_base(&source_url)
            || !source.base_url.ends_with(&self.model_revision)
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        let mut paths = HashSet::new();
        let mut total = 0_u64;
        for (file, expected_path) in self.files.iter().zip(LOCAL_DIARIZATION_SPEAKERKIT_FILES) {
            let Some(relative_path) = file.install_path.strip_prefix(SPEAKERKIT_INSTALL_PREFIX)
            else {
                return Err(LocalModelError::new("invalid_catalog"));
            };
            if file.source_id != source.id
                || file.source_path != relative_path
                || relative_path != expected_path
                || !valid_source_path(&file.source_path)
                || !paths.insert(file.install_path.as_str())
                || validate_sha256(&file.sha256, "model.sha256").is_err()
                || file.size_bytes == 0
            {
                return Err(LocalModelError::new("invalid_catalog"));
            }
            total = total
                .checked_add(file.size_bytes)
                .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
        }
        if total != 17_187_070 {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(())
    }

    fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size_bytes).sum()
    }

    fn source_url(&self, file: &CatalogFile) -> Result<reqwest::Url, LocalModelError> {
        let source = self
            .sources
            .iter()
            .find(|source| source.id == file.source_id)
            .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
        let url = reqwest::Url::parse(&format!(
            "{}/{}",
            source.base_url.trim_end_matches('/'),
            file.source_path
        ))
        .map_err(|_| LocalModelError::new("invalid_catalog"))?;
        if !allowed_download_url(&url) || url.query().is_some() || url.fragment().is_some() {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(url)
    }

    fn model_files(&self) -> Result<Vec<LocalModelFileIdentity>, LocalModelError> {
        let mut files = Vec::with_capacity(self.files.len());
        for file in &self.files {
            let relative_path = file
                .install_path
                .strip_prefix(SPEAKERKIT_INSTALL_PREFIX)
                .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
            files.push(LocalModelFileIdentity {
                relative_path: relative_path.to_owned(),
                sha256: file.sha256.clone(),
                size_bytes: file.size_bytes,
            });
        }
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        if files.len() != LOCAL_DIARIZATION_SPEAKERKIT_FILES.len()
            || files
                .iter()
                .zip(LOCAL_DIARIZATION_SPEAKERKIT_FILES)
                .any(|(file, expected_path)| file.relative_path != expected_path)
        {
            return Err(LocalModelError::new("invalid_catalog"));
        }
        Ok(files)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelPackStatus {
    pub supported: bool,
    pub enabled: bool,
    pub pack_id: String,
    pub display_name: String,
    pub whisper_model_id: String,
    pub diarization_default: bool,
    pub summary_model_id: String,
    pub minimum_macos_major: u32,
    pub minimum_memory_bytes: u64,
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
pub struct LocalModelPackProof {
    pub whisper_model_id: String,
    pub whisper_sha256: String,
    pub whisper_size_bytes: u64,
    pub diarization_pack_id: String,
    pub diarization_files: Vec<LocalModelFileIdentity>,
    pub summary_model_id: String,
    pub summary_sha256: String,
    pub summary_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QwenCandidateModelPackStatus {
    pub supported: bool,
    pub enabled: bool,
    pub pack_id: String,
    pub display_name: String,
    pub runtime_id: String,
    pub asr_model_id: String,
    pub aligner_model_id: String,
    pub minimum_macos_major: u32,
    pub minimum_memory_bytes: u64,
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
pub struct QwenCandidateModelPackProof {
    pub runtime_id: String,
    pub asr_model_id: String,
    pub asr_model_revision: String,
    pub asr_model_files: Vec<LocalQwenModelFileIdentity>,
    pub aligner_model_id: String,
    pub aligner_model_revision: String,
    pub aligner_model_files: Vec<LocalQwenModelFileIdentity>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerKitCandidateModelPackStatus {
    pub supported: bool,
    pub enabled: bool,
    pub pack_id: String,
    pub display_name: String,
    pub quality_preset: String,
    pub model_revision: String,
    pub minimum_macos_major: u32,
    pub minimum_memory_bytes: u64,
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
pub struct SpeakerKitCandidateModelPackProof {
    pub pack_id: String,
    pub quality_preset: String,
    pub model_revision: String,
    pub model_files: Vec<LocalModelFileIdentity>,
}

#[derive(Default)]
struct OperationStatus {
    installing: bool,
    downloaded_bytes: u64,
    installed: bool,
    error_code: Option<&'static str>,
}

pub struct LocalModelPackManager {
    app_data_root: PathBuf,
    client: reqwest::Client,
    operation: Mutex<OperationStatus>,
    qwen_operation: Mutex<OperationStatus>,
    speakerkit_operation: Mutex<OperationStatus>,
    moss_operation: Mutex<OperationStatus>,
    moss_pipeline: Arc<Mutex<moss_pipeline::PipelineState>>,
    busy: Arc<AtomicBool>,
    cancel: AtomicBool,
    qwen_cancel: AtomicBool,
    speakerkit_cancel: AtomicBool,
    moss_cancel: AtomicBool,
}

struct BusyLease {
    flag: Arc<AtomicBool>,
}

impl Drop for BusyLease {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

impl LocalModelPackManager {
    pub fn open(app_data_root: impl AsRef<Path>) -> Result<Self, LocalModelError> {
        LocalPackCatalog::load()?;
        QwenCandidateCatalog::load()?;
        SpeakerKitCandidateCatalog::load()?;
        moss::MossCandidateCatalog::load()?;
        let metadata = std::fs::symlink_metadata(app_data_root.as_ref())
            .map_err(|_| LocalModelError::new("invalid_model_root"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(LocalModelError::new("invalid_model_root"));
        }
        let app_data_root = std::fs::canonicalize(app_data_root.as_ref())
            .map_err(|_| LocalModelError::new("invalid_model_root"))?;
        let client = crate::qa::guard_http(reqwest::Client::builder())
            .https_only(true)
            .connect_timeout(Duration::from_secs(20))
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() > 8 || !allowed_download_url(attempt.url()) {
                    attempt.error("model download redirect rejected")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|_| LocalModelError::new("download_unavailable"))?;
        Ok(Self {
            app_data_root,
            client,
            operation: Mutex::new(OperationStatus::default()),
            qwen_operation: Mutex::new(OperationStatus::default()),
            speakerkit_operation: Mutex::new(OperationStatus::default()),
            moss_operation: Mutex::new(OperationStatus::default()),
            moss_pipeline: Arc::new(Mutex::new(moss_pipeline::PipelineState::default())),
            busy: Arc::new(AtomicBool::new(false)),
            cancel: AtomicBool::new(false),
            qwen_cancel: AtomicBool::new(false),
            speakerkit_cancel: AtomicBool::new(false),
            moss_cancel: AtomicBool::new(false),
        })
    }

    pub async fn speakerkit_status(
        &self,
        enabled: bool,
    ) -> Result<SpeakerKitCandidateModelPackStatus, LocalModelError> {
        let catalog = SpeakerKitCandidateCatalog::load()?;
        let installing = self
            .speakerkit_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?
            .installing;
        if !installing {
            let root = self.app_data_root.clone();
            let catalog = catalog.clone();
            let total_bytes = catalog.total_bytes();
            let (installed, staged_bytes) = tokio::task::spawn_blocking(move || {
                let installed =
                    verify_exact_catalog_tree(&root, &catalog.files, SPEAKERKIT_INSTALL_PREFIX)?;
                let staged_bytes = if installed {
                    total_bytes
                } else {
                    installed_and_partial_catalog_bytes(&root, &catalog.files, total_bytes)?
                };
                Ok::<_, LocalModelError>((installed, staged_bytes))
            })
            .await
            .map_err(|_| LocalModelError::new("model_state_unavailable"))??;
            let mut operation = self
                .speakerkit_operation
                .lock()
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            operation.installed = installed;
            operation.downloaded_bytes = staged_bytes;
            if installed {
                operation.error_code = None;
            }
        }
        self.speakerkit_snapshot(enabled)
    }

    pub async fn speakerkit_proof(
        &self,
    ) -> Result<SpeakerKitCandidateModelPackProof, LocalModelError> {
        let catalog = SpeakerKitCandidateCatalog::load()?;
        if system_memory_bytes().is_none_or(|bytes| bytes < catalog.minimum_memory_bytes) {
            return Err(LocalModelError::new("insufficient_model_memory"));
        }
        let root = self.app_data_root.clone();
        let files = catalog.files.clone();
        if !tokio::task::spawn_blocking(move || {
            verify_exact_catalog_tree(&root, &files, SPEAKERKIT_INSTALL_PREFIX)
        })
        .await
        .map_err(|_| LocalModelError::new("model_state_unavailable"))??
        {
            return Err(LocalModelError::new("model_pack_not_installed"));
        }
        Ok(SpeakerKitCandidateModelPackProof {
            pack_id: catalog.pack_id.clone(),
            quality_preset: catalog.quality_preset.clone(),
            model_revision: catalog.model_revision.clone(),
            model_files: catalog.model_files()?,
        })
    }

    pub async fn qwen_status(
        &self,
        enabled: bool,
    ) -> Result<QwenCandidateModelPackStatus, LocalModelError> {
        let catalog = QwenCandidateCatalog::load()?;
        let installing = self
            .qwen_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?
            .installing;
        if !installing {
            let root = self.app_data_root.clone();
            let catalog = catalog.clone();
            let total_bytes = catalog.total_bytes();
            let (installed, staged_bytes) = tokio::task::spawn_blocking(move || {
                let installed = verify_catalog_files(&root, &catalog.files)?;
                let staged_bytes = if installed {
                    total_bytes
                } else {
                    installed_and_partial_catalog_bytes(&root, &catalog.files, total_bytes)?
                };
                Ok::<_, LocalModelError>((installed, staged_bytes))
            })
            .await
            .map_err(|_| LocalModelError::new("model_state_unavailable"))??;
            let mut operation = self
                .qwen_operation
                .lock()
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            operation.installed = installed;
            operation.downloaded_bytes = staged_bytes;
            if installed {
                operation.error_code = None;
            }
        }
        self.qwen_snapshot(enabled)
    }

    pub async fn qwen_proof(&self) -> Result<QwenCandidateModelPackProof, LocalModelError> {
        let catalog = QwenCandidateCatalog::load()?;
        if system_memory_bytes().is_none_or(|bytes| bytes < catalog.minimum_memory_bytes) {
            return Err(LocalModelError::new("insufficient_model_memory"));
        }
        let root = self.app_data_root.clone();
        let files = catalog.files.clone();
        if !tokio::task::spawn_blocking(move || verify_catalog_files(&root, &files))
            .await
            .map_err(|_| LocalModelError::new("model_state_unavailable"))??
        {
            return Err(LocalModelError::new("model_pack_not_installed"));
        }
        Ok(QwenCandidateModelPackProof {
            runtime_id: catalog.runtime_id.clone(),
            asr_model_id: catalog.asr_model_id.clone(),
            asr_model_revision: catalog.asr_model_revision.clone(),
            asr_model_files: catalog.asr_files()?,
            aligner_model_id: catalog.aligner_model_id.clone(),
            aligner_model_revision: catalog.aligner_model_revision.clone(),
            aligner_model_files: catalog.aligner_files()?,
        })
    }

    pub async fn status(&self, enabled: bool) -> Result<LocalModelPackStatus, LocalModelError> {
        let catalog = LocalPackCatalog::load()?;
        let installing = self
            .operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?
            .installing;
        if !installing {
            let root = self.app_data_root.clone();
            let catalog = catalog.clone();
            let total_bytes = catalog.total_bytes();
            let (installed, staged_bytes) = tokio::task::spawn_blocking(move || {
                let installed = verify_pack(&root, &catalog)?;
                let staged_bytes = if installed {
                    total_bytes
                } else {
                    installed_and_partial_bytes(&root, &catalog)?
                };
                Ok::<_, LocalModelError>((installed, staged_bytes))
            })
            .await
            .map_err(|_| LocalModelError::new("model_state_unavailable"))??;
            let mut operation = self
                .operation
                .lock()
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            operation.installed = installed;
            operation.downloaded_bytes = staged_bytes;
            if installed {
                operation.error_code = None;
            }
        }
        self.snapshot(enabled)
    }

    pub async fn install(&self, pack_id: &str) -> Result<LocalModelPackStatus, LocalModelError> {
        self.require_pack(pack_id)?;
        if system_memory_bytes().is_none_or(|bytes| {
            bytes
                < LocalPackCatalog::load()
                    .map(|catalog| catalog.minimum_memory_bytes)
                    .unwrap_or(u64::MAX)
        }) {
            return Err(LocalModelError::new("insufficient_model_memory"));
        }
        let _busy = self.acquire_busy()?;
        self.cancel.store(false, Ordering::Release);
        {
            let mut operation = self
                .operation
                .lock()
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            operation.installing = true;
            operation.downloaded_bytes = 0;
            operation.error_code = None;
        }
        let result = self.install_inner().await;
        let mut operation = self
            .operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        operation.installing = false;
        match result {
            Ok(()) => {
                operation.installed = true;
                operation.downloaded_bytes = LocalPackCatalog::load()?.total_bytes();
                operation.error_code = None;
            }
            Err(error) => {
                operation.installed = false;
                operation.error_code = Some(error.code);
                return Err(error);
            }
        }
        drop(operation);
        self.snapshot(true)
    }

    pub async fn install_qwen(
        &self,
        pack_id: &str,
    ) -> Result<QwenCandidateModelPackStatus, LocalModelError> {
        self.require_qwen_pack(pack_id)?;
        let catalog = QwenCandidateCatalog::load()?;
        if system_memory_bytes().is_none_or(|bytes| bytes < catalog.minimum_memory_bytes) {
            return Err(LocalModelError::new("insufficient_model_memory"));
        }
        let _busy = self.acquire_busy()?;
        self.qwen_cancel.store(false, Ordering::Release);
        {
            let mut operation = self
                .qwen_operation
                .lock()
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            operation.installing = true;
            operation.downloaded_bytes = 0;
            operation.error_code = None;
        }
        let result = self.install_qwen_inner().await;
        let mut operation = self
            .qwen_operation
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
        self.qwen_snapshot(true)
    }

    pub async fn install_speakerkit(
        &self,
        pack_id: &str,
    ) -> Result<SpeakerKitCandidateModelPackStatus, LocalModelError> {
        self.require_speakerkit_pack(pack_id)?;
        let catalog = SpeakerKitCandidateCatalog::load()?;
        if system_memory_bytes().is_none_or(|bytes| bytes < catalog.minimum_memory_bytes) {
            return Err(LocalModelError::new("insufficient_model_memory"));
        }
        let _busy = self.acquire_busy()?;
        self.speakerkit_cancel.store(false, Ordering::Release);
        {
            let mut operation = self
                .speakerkit_operation
                .lock()
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            operation.installing = true;
            operation.downloaded_bytes = 0;
            operation.error_code = None;
        }
        let result = self.install_speakerkit_inner().await;
        let mut operation = self
            .speakerkit_operation
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
        self.speakerkit_snapshot(true)
    }

    pub fn cancel_install(&self, pack_id: &str) -> Result<(), LocalModelError> {
        match pack_id {
            FULL_LOCAL_PACK_ID => self.cancel.store(true, Ordering::Release),
            QWEN_CANDIDATE_PACK_ID => self.qwen_cancel.store(true, Ordering::Release),
            SPEAKERKIT_CANDIDATE_PACK_ID => self.speakerkit_cancel.store(true, Ordering::Release),
            MOSS_CANDIDATE_PACK_ID => self.moss_cancel.store(true, Ordering::Release),
            moss_pipeline::MOSS_PIPELINE_PACK_ID => self.cancel_moss_pipeline_install()?,
            _ => return Err(LocalModelError::new("unknown_model_pack")),
        }
        Ok(())
    }

    pub async fn remove(&self, pack_id: &str) -> Result<LocalModelPackStatus, LocalModelError> {
        self.require_pack(pack_id)?;
        let _busy = self.acquire_busy()?;
        let root = self.app_data_root.clone();
        let catalog = LocalPackCatalog::load()?.clone();
        let remove_owner = Arc::clone(&_busy);
        let result = tokio::task::spawn_blocking(move || {
            let _owner = remove_owner;
            remove_pack(&root, &catalog)
        })
        .await;
        let result = result.map_err(|_| LocalModelError::new("model_remove_failed"))?;
        result?;
        let mut operation = self
            .operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        *operation = OperationStatus::default();
        drop(operation);
        self.snapshot(true)
    }

    pub async fn remove_qwen(
        &self,
        pack_id: &str,
    ) -> Result<QwenCandidateModelPackStatus, LocalModelError> {
        self.require_qwen_pack(pack_id)?;
        let _busy = self.acquire_busy()?;
        let root = self.app_data_root.clone();
        let files = QwenCandidateCatalog::load()?.files.clone();
        let remove_owner = Arc::clone(&_busy);
        tokio::task::spawn_blocking(move || {
            let _owner = remove_owner;
            remove_catalog_files(&root, &files)
        })
        .await
        .map_err(|_| LocalModelError::new("model_remove_failed"))??;
        let mut operation = self
            .qwen_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        *operation = OperationStatus::default();
        drop(operation);
        self.qwen_snapshot(true)
    }

    pub async fn remove_speakerkit(
        &self,
        pack_id: &str,
    ) -> Result<SpeakerKitCandidateModelPackStatus, LocalModelError> {
        self.require_speakerkit_pack(pack_id)?;
        let _busy = self.acquire_busy()?;
        let root = self.app_data_root.clone();
        let files = SpeakerKitCandidateCatalog::load()?.files.clone();
        let remove_owner = Arc::clone(&_busy);
        tokio::task::spawn_blocking(move || {
            let _owner = remove_owner;
            remove_catalog_files(&root, &files)
        })
        .await
        .map_err(|_| LocalModelError::new("model_remove_failed"))??;
        let mut operation = self
            .speakerkit_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        *operation = OperationStatus::default();
        drop(operation);
        self.speakerkit_snapshot(true)
    }

    pub async fn proof(&self) -> Result<LocalModelPackProof, LocalModelError> {
        let catalog = LocalPackCatalog::load()?;
        if system_memory_bytes().is_none_or(|bytes| bytes < catalog.minimum_memory_bytes) {
            return Err(LocalModelError::new("insufficient_model_memory"));
        }
        let root = self.app_data_root.clone();
        let owned = catalog.clone();
        if !tokio::task::spawn_blocking(move || verify_pack(&root, &owned))
            .await
            .map_err(|_| LocalModelError::new("model_state_unavailable"))??
        {
            return Err(LocalModelError::new("model_pack_not_installed"));
        }
        let whisper = catalog
            .files
            .iter()
            .find(|file| file.install_path.starts_with("models/whisper/"))
            .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
        let summary = catalog.summary_model_file()?;
        Ok(LocalModelPackProof {
            whisper_model_id: catalog.whisper_model_id.clone(),
            whisper_sha256: whisper.sha256.clone(),
            whisper_size_bytes: whisper.size_bytes,
            diarization_pack_id: catalog.diarization_pack_id.clone(),
            diarization_files: catalog.diarization_model_files()?,
            summary_model_id: catalog.summary_model_id.clone(),
            summary_sha256: summary.sha256.clone(),
            summary_size_bytes: summary.size_bytes,
        })
    }

    async fn install_inner(&self) -> Result<(), LocalModelError> {
        let catalog = LocalPackCatalog::load()?;
        for file in &catalog.files {
            if self.cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
            let installed = resolve_install_file(&self.app_data_root, &file.install_path, false)?;
            if let Some(path) = installed {
                if file_matches(&path, file) {
                    self.add_progress(file.size_bytes)?;
                    continue;
                }
                remove_regular_file(&path)?;
            }
            let destination = ensure_install_destination(&self.app_data_root, &file.install_path)?;
            let partial = partial_path(&destination)?;
            validate_partial(&partial, file.size_bytes)?;
            let result = self.download_file(catalog, file, &partial).await;
            if result.as_ref().is_err_and(|error| {
                matches!(
                    error.code,
                    "model_download_rejected" | "model_verification_failed"
                )
            }) {
                let _ = remove_regular_file(&partial);
            }
            result?;
            if self.cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
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
        let catalog = catalog.clone();
        if !tokio::task::spawn_blocking(move || verify_pack(&root, &catalog))
            .await
            .map_err(|_| LocalModelError::new("model_verification_failed"))??
        {
            return Err(LocalModelError::new("model_verification_failed"));
        }
        Ok(())
    }

    async fn install_qwen_inner(&self) -> Result<(), LocalModelError> {
        let catalog = QwenCandidateCatalog::load()?;
        for file in &catalog.files {
            if self.qwen_cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
            let installed = resolve_install_file(&self.app_data_root, &file.install_path, false)?;
            if let Some(path) = installed {
                if file_matches(&path, file) {
                    self.add_qwen_progress(file.size_bytes)?;
                    continue;
                }
                remove_regular_file(&path)?;
            }
            let destination = ensure_install_destination(&self.app_data_root, &file.install_path)?;
            let partial = partial_path(&destination)?;
            validate_partial(&partial, file.size_bytes)?;
            let result = self.download_qwen_file(catalog, file, &partial).await;
            if result.as_ref().is_err_and(|error| {
                matches!(
                    error.code,
                    "model_download_rejected" | "model_verification_failed"
                )
            }) {
                let _ = remove_regular_file(&partial);
            }
            result?;
            if self.qwen_cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
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
        if !tokio::task::spawn_blocking(move || verify_catalog_files(&root, &files))
            .await
            .map_err(|_| LocalModelError::new("model_verification_failed"))??
        {
            return Err(LocalModelError::new("model_verification_failed"));
        }
        Ok(())
    }

    async fn install_speakerkit_inner(&self) -> Result<(), LocalModelError> {
        let catalog = SpeakerKitCandidateCatalog::load()?;
        for file in &catalog.files {
            if self.speakerkit_cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
            let installed = resolve_install_file(&self.app_data_root, &file.install_path, false)?;
            if let Some(path) = installed {
                if file_matches(&path, file) {
                    self.add_speakerkit_progress(file.size_bytes)?;
                    continue;
                }
                remove_regular_file(&path)?;
            }
            let destination = ensure_install_destination(&self.app_data_root, &file.install_path)?;
            let partial = partial_path(&destination)?;
            validate_partial(&partial, file.size_bytes)?;
            let result = self.download_speakerkit_file(catalog, file, &partial).await;
            if result.as_ref().is_err_and(|error| {
                matches!(
                    error.code,
                    "model_download_rejected" | "model_verification_failed"
                )
            }) {
                let _ = remove_regular_file(&partial);
            }
            result?;
            if self.speakerkit_cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
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
            verify_exact_catalog_tree(&root, &files, SPEAKERKIT_INSTALL_PREFIX)
        })
        .await
        .map_err(|_| LocalModelError::new("model_verification_failed"))??
        {
            return Err(LocalModelError::new("model_verification_failed"));
        }
        Ok(())
    }

    async fn download_file(
        &self,
        catalog: &LocalPackCatalog,
        file: &CatalogFile,
        partial: &Path,
    ) -> Result<(), LocalModelError> {
        let (mut digest, mut size_bytes) = partial_digest(partial, file).await?;
        if size_bytes == file.size_bytes {
            return Ok(());
        }
        self.add_progress(size_bytes)?;
        let url = catalog.source_url(file)?;
        let remaining = file.size_bytes.saturating_sub(size_bytes);
        let mut request = self.client.get(url);
        if size_bytes > 0 {
            request = request.header(RANGE, format!("bytes={size_bytes}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|_| LocalModelError::new("model_download_failed"))?
            .error_for_status()
            .map_err(|_| LocalModelError::new("model_download_failed"))?;
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
        while let Some(chunk) = stream.next().await {
            if self.cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
            let chunk = chunk.map_err(|_| LocalModelError::new("model_download_failed"))?;
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
            self.add_progress(chunk.len() as u64)?;
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

    async fn download_qwen_file(
        &self,
        catalog: &QwenCandidateCatalog,
        file: &CatalogFile,
        partial: &Path,
    ) -> Result<(), LocalModelError> {
        let (mut digest, mut size_bytes) = partial_digest(partial, file).await?;
        if size_bytes == file.size_bytes {
            return Ok(());
        }
        self.add_qwen_progress(size_bytes)?;
        let url = catalog.source_url(file)?;
        let remaining = file.size_bytes.saturating_sub(size_bytes);
        let mut request = self.client.get(url);
        if size_bytes > 0 {
            request = request.header(RANGE, format!("bytes={size_bytes}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|_| LocalModelError::new("model_download_failed"))?
            .error_for_status()
            .map_err(|_| LocalModelError::new("model_download_failed"))?;
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
        while let Some(chunk) = stream.next().await {
            if self.qwen_cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
            let chunk = chunk.map_err(|_| LocalModelError::new("model_download_failed"))?;
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
            self.add_qwen_progress(chunk.len() as u64)?;
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

    async fn download_speakerkit_file(
        &self,
        catalog: &SpeakerKitCandidateCatalog,
        file: &CatalogFile,
        partial: &Path,
    ) -> Result<(), LocalModelError> {
        let (mut digest, mut size_bytes) = partial_digest(partial, file).await?;
        if size_bytes == file.size_bytes {
            return Ok(());
        }
        self.add_speakerkit_progress(size_bytes)?;
        let url = catalog.source_url(file)?;
        let remaining = file.size_bytes.saturating_sub(size_bytes);
        let mut request = self.client.get(url);
        if size_bytes > 0 {
            request = request.header(RANGE, format!("bytes={size_bytes}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|_| LocalModelError::new("model_download_failed"))?
            .error_for_status()
            .map_err(|_| LocalModelError::new("model_download_failed"))?;
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
        while let Some(chunk) = stream.next().await {
            if self.speakerkit_cancel.load(Ordering::Acquire) {
                return Err(LocalModelError::new("model_install_canceled"));
            }
            let chunk = chunk.map_err(|_| LocalModelError::new("model_download_failed"))?;
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
            self.add_speakerkit_progress(chunk.len() as u64)?;
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

    fn add_progress(&self, bytes: u64) -> Result<(), LocalModelError> {
        let mut operation = self
            .operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        operation.downloaded_bytes = operation
            .downloaded_bytes
            .saturating_add(bytes)
            .min(LocalPackCatalog::load()?.total_bytes());
        Ok(())
    }

    fn add_qwen_progress(&self, bytes: u64) -> Result<(), LocalModelError> {
        let mut operation = self
            .qwen_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        operation.downloaded_bytes = operation
            .downloaded_bytes
            .saturating_add(bytes)
            .min(QwenCandidateCatalog::load()?.total_bytes());
        Ok(())
    }

    fn add_speakerkit_progress(&self, bytes: u64) -> Result<(), LocalModelError> {
        let mut operation = self
            .speakerkit_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        operation.downloaded_bytes = operation
            .downloaded_bytes
            .saturating_add(bytes)
            .min(SpeakerKitCandidateCatalog::load()?.total_bytes());
        Ok(())
    }

    fn require_pack(&self, pack_id: &str) -> Result<(), LocalModelError> {
        if pack_id == FULL_LOCAL_PACK_ID {
            Ok(())
        } else {
            Err(LocalModelError::new("unknown_model_pack"))
        }
    }

    fn require_qwen_pack(&self, pack_id: &str) -> Result<(), LocalModelError> {
        if pack_id == QWEN_CANDIDATE_PACK_ID {
            Ok(())
        } else {
            Err(LocalModelError::new("unknown_model_pack"))
        }
    }

    fn require_speakerkit_pack(&self, pack_id: &str) -> Result<(), LocalModelError> {
        if pack_id == SPEAKERKIT_CANDIDATE_PACK_ID {
            Ok(())
        } else {
            Err(LocalModelError::new("unknown_model_pack"))
        }
    }

    fn acquire_busy(&self) -> Result<Arc<BusyLease>, LocalModelError> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| LocalModelError::new("model_operation_in_progress"))?;
        Ok(Arc::new(BusyLease {
            flag: Arc::clone(&self.busy),
        }))
    }

    fn snapshot(&self, enabled: bool) -> Result<LocalModelPackStatus, LocalModelError> {
        let catalog = LocalPackCatalog::load()?;
        let operation = self
            .operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        Ok(LocalModelPackStatus {
            supported: crate::features::full_local_supported(),
            enabled,
            pack_id: catalog.pack_id.clone(),
            display_name: catalog.display_name.clone(),
            whisper_model_id: catalog.whisper_model_id.clone(),
            diarization_default: catalog.diarization_default,
            summary_model_id: catalog.summary_model_id.clone(),
            minimum_macos_major: catalog.minimum_macos_major,
            minimum_memory_bytes: catalog.minimum_memory_bytes,
            detected_memory_bytes: system_memory_bytes(),
            memory_sufficient: system_memory_bytes()
                .is_some_and(|bytes| bytes >= catalog.minimum_memory_bytes),
            total_bytes: catalog.total_bytes(),
            installed: operation.installed,
            installing: operation.installing,
            downloaded_bytes: operation.downloaded_bytes,
            licenses: catalog
                .sources
                .iter()
                .map(|source| format!("{} · {}", source.license, source.attribution))
                .collect(),
            error_code: operation.error_code,
        })
    }

    fn qwen_snapshot(
        &self,
        enabled: bool,
    ) -> Result<QwenCandidateModelPackStatus, LocalModelError> {
        let catalog = QwenCandidateCatalog::load()?;
        let operation = self
            .qwen_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        Ok(QwenCandidateModelPackStatus {
            supported: crate::features::full_local_supported(),
            enabled,
            pack_id: catalog.pack_id.clone(),
            display_name: catalog.display_name.clone(),
            runtime_id: catalog.runtime_id.clone(),
            asr_model_id: catalog.asr_model_id.clone(),
            aligner_model_id: catalog.aligner_model_id.clone(),
            minimum_macos_major: catalog.minimum_macos_major,
            minimum_memory_bytes: catalog.minimum_memory_bytes,
            detected_memory_bytes: system_memory_bytes(),
            memory_sufficient: system_memory_bytes()
                .is_some_and(|bytes| bytes >= catalog.minimum_memory_bytes),
            total_bytes: catalog.total_bytes(),
            installed: operation.installed,
            installing: operation.installing,
            downloaded_bytes: operation.downloaded_bytes,
            licenses: catalog
                .sources
                .iter()
                .map(|source| format!("{} · {}", source.license, source.attribution))
                .collect(),
            error_code: operation.error_code,
        })
    }

    fn speakerkit_snapshot(
        &self,
        enabled: bool,
    ) -> Result<SpeakerKitCandidateModelPackStatus, LocalModelError> {
        let catalog = SpeakerKitCandidateCatalog::load()?;
        let operation = self
            .speakerkit_operation
            .lock()
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        Ok(SpeakerKitCandidateModelPackStatus {
            supported: crate::features::full_local_supported(),
            enabled,
            pack_id: catalog.pack_id.clone(),
            display_name: catalog.display_name.clone(),
            quality_preset: catalog.quality_preset.clone(),
            model_revision: catalog.model_revision.clone(),
            minimum_macos_major: catalog.minimum_macos_major,
            minimum_memory_bytes: catalog.minimum_memory_bytes,
            detected_memory_bytes: system_memory_bytes(),
            memory_sufficient: system_memory_bytes()
                .is_some_and(|bytes| bytes >= catalog.minimum_memory_bytes),
            total_bytes: catalog.total_bytes(),
            installed: operation.installed,
            installing: operation.installing,
            downloaded_bytes: operation.downloaded_bytes,
            licenses: catalog
                .sources
                .iter()
                .map(|source| format!("{} · {}", source.license, source.attribution))
                .collect(),
            error_code: operation.error_code,
        })
    }
}

pub struct LocalModelState {
    manager: Arc<LocalModelPackManager>,
}

impl LocalModelState {
    pub fn new(manager: Arc<LocalModelPackManager>) -> Self {
        Self { manager }
    }

    pub fn manager(&self) -> Arc<LocalModelPackManager> {
        Arc::clone(&self.manager)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalModelPackRequest {
    pub pack_id: String,
}

#[tauri::command]
pub async fn local_model_pack_status(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
) -> Result<LocalModelPackStatus, String> {
    state
        .manager()
        .status(features.local_stt)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn qwen_candidate_model_pack_status(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
) -> Result<QwenCandidateModelPackStatus, String> {
    state
        .manager()
        .qwen_status(features.local_qwen_candidate)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn speakerkit_candidate_model_pack_status(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
) -> Result<SpeakerKitCandidateModelPackStatus, String> {
    state
        .manager()
        .speakerkit_status(features.local_speakerkit_candidate)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn install_local_model_pack(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
    request: LocalModelPackRequest,
) -> Result<LocalModelPackStatus, String> {
    if !features.local_stt || !crate::features::full_local_supported() {
        return Err("full local mode is unavailable on this platform".to_owned());
    }
    state
        .manager()
        .install(&request.pack_id)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn install_qwen_candidate_model_pack(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
    request: LocalModelPackRequest,
) -> Result<QwenCandidateModelPackStatus, String> {
    if !features.local_qwen_candidate || !crate::features::full_local_supported() {
        return Err("Qwen candidate mode is unavailable".to_owned());
    }
    state
        .manager()
        .install_qwen(&request.pack_id)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn install_speakerkit_candidate_model_pack(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
    request: LocalModelPackRequest,
) -> Result<SpeakerKitCandidateModelPackStatus, String> {
    if !features.local_speakerkit_candidate || !crate::features::full_local_supported() {
        return Err("SpeakerKit candidate mode is unavailable".to_owned());
    }
    state
        .manager()
        .install_speakerkit(&request.pack_id)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub fn cancel_local_model_install(
    state: tauri::State<'_, LocalModelState>,
    request: LocalModelPackRequest,
) -> Result<(), String> {
    state
        .manager()
        .cancel_install(&request.pack_id)
        .map_err(safe_error)
}

#[tauri::command]
pub async fn remove_local_model_pack(
    state: tauri::State<'_, LocalModelState>,
    request: LocalModelPackRequest,
) -> Result<LocalModelPackStatus, String> {
    state
        .manager()
        .remove(&request.pack_id)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn remove_qwen_candidate_model_pack(
    state: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, RuntimeFeatures>,
    request: LocalModelPackRequest,
) -> Result<QwenCandidateModelPackStatus, String> {
    if !features.local_qwen_candidate || !crate::features::full_local_supported() {
        return Err("Qwen candidate mode is unavailable".to_owned());
    }
    state
        .manager()
        .remove_qwen(&request.pack_id)
        .await
        .map_err(safe_error)
}

#[tauri::command]
pub async fn remove_speakerkit_candidate_model_pack(
    state: tauri::State<'_, LocalModelState>,
    request: LocalModelPackRequest,
) -> Result<SpeakerKitCandidateModelPackStatus, String> {
    state
        .manager()
        .remove_speakerkit(&request.pack_id)
        .await
        .map_err(safe_error)
}

fn safe_error(error: LocalModelError) -> String {
    error.to_string()
}

fn allowed_download_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some_and(|host| {
            host == "huggingface.co" || host == "hf.co" || host.ends_with(".hf.co")
        })
}

fn verify_pack(root: &Path, catalog: &LocalPackCatalog) -> Result<bool, LocalModelError> {
    verify_catalog_files(root, &catalog.files)
}

fn verify_catalog_files(root: &Path, files: &[CatalogFile]) -> Result<bool, LocalModelError> {
    for file in files {
        let Some(path) = resolve_install_file(root, &file.install_path, false)? else {
            return Ok(false);
        };
        if !file_matches(&path, file) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn verify_exact_catalog_tree(
    root: &Path,
    files: &[CatalogFile],
    install_prefix: &str,
) -> Result<bool, LocalModelError> {
    if !verify_catalog_files(root, files)? {
        return Ok(false);
    }
    verify_exact_catalog_paths(root, files, install_prefix)
}

/// Metadata-only counterpart for callers retaining a hash-bound file cache.
fn verify_exact_catalog_paths(
    root: &Path,
    files: &[CatalogFile],
    install_prefix: &str,
) -> Result<bool, LocalModelError> {
    let relative_root = install_prefix
        .strip_suffix('/')
        .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
    let Some(pack_root) = resolve_install_directory(root, relative_root, false)? else {
        return Ok(false);
    };
    let expected: HashSet<_> = files.iter().map(|file| file.install_path.clone()).collect();
    let mut discovered = HashSet::new();
    let mut pending = vec![pack_root];
    let mut scanned_entries = 0_usize;
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
        for entry in entries {
            let entry = entry.map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            scanned_entries = scanned_entries
                .checked_add(1)
                .ok_or_else(|| LocalModelError::new("model_path_rejected"))?;
            if scanned_entries > MAX_CATALOG_FILES * 8 {
                return Err(LocalModelError::new("model_path_rejected"));
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| LocalModelError::new("model_state_unavailable"))?;
            if metadata.file_type().is_symlink() {
                return Err(LocalModelError::new("model_path_rejected"));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| LocalModelError::new("model_path_rejected"))?
                    .to_str()
                    .ok_or_else(|| LocalModelError::new("model_path_rejected"))?;
                if !discovered.insert(relative.to_owned()) {
                    return Err(LocalModelError::new("model_path_rejected"));
                }
            } else {
                return Err(LocalModelError::new("model_path_rejected"));
            }
        }
    }
    Ok(discovered == expected)
}

fn installed_and_partial_bytes(
    root: &Path,
    catalog: &LocalPackCatalog,
) -> Result<u64, LocalModelError> {
    installed_and_partial_catalog_bytes(root, &catalog.files, catalog.total_bytes())
}

fn installed_and_partial_catalog_bytes(
    root: &Path,
    files: &[CatalogFile],
    total_bytes: u64,
) -> Result<u64, LocalModelError> {
    let mut total = 0_u64;
    for file in files {
        if let Some(path) = resolve_install_file(root, &file.install_path, false)? {
            if file_matches(&path, file) {
                total = total.saturating_add(file.size_bytes);
                continue;
            }
        }
        let partial = partial_path(&root.join(&file.install_path))?;
        match std::fs::symlink_metadata(&partial) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= file.size_bytes =>
            {
                total = total.saturating_add(metadata.len());
            }
            Ok(_) => return Err(LocalModelError::new("model_path_rejected")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(LocalModelError::new("model_state_unavailable")),
        }
    }
    Ok(total.min(total_bytes))
}

fn file_matches(path: &Path, file: &CatalogFile) -> bool {
    hash_file_streaming(path)
        .is_ok_and(|digest| digest.size_bytes == file.size_bytes && digest.sha256 == file.sha256)
}

fn ensure_install_destination(root: &Path, relative: &str) -> Result<PathBuf, LocalModelError> {
    let path = Path::new(relative);
    if !valid_relative_path(relative) {
        return Err(LocalModelError::new("invalid_catalog"));
    }
    let mut destination = root.to_path_buf();
    let components: Vec<_> = path.components().collect();
    for component in &components[..components.len().saturating_sub(1)] {
        let Component::Normal(name) = component else {
            return Err(LocalModelError::new("invalid_catalog"));
        };
        destination.push(name);
        match std::fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(LocalModelError::new("model_path_rejected"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&destination)
                    .map_err(|_| LocalModelError::new("model_install_failed"))?;
            }
            Err(_) => return Err(LocalModelError::new("model_install_failed")),
        }
    }
    let file_name = components
        .last()
        .and_then(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .ok_or_else(|| LocalModelError::new("invalid_catalog"))?;
    destination.push(file_name);
    if let Ok(metadata) = std::fs::symlink_metadata(&destination) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(LocalModelError::new("model_path_rejected"));
        }
        return Err(LocalModelError::new("model_destination_conflict"));
    }
    Ok(destination)
}

fn resolve_install_file(
    root: &Path,
    relative: &str,
    require_present: bool,
) -> Result<Option<PathBuf>, LocalModelError> {
    if !valid_relative_path(relative) {
        return Err(LocalModelError::new("invalid_catalog"));
    }
    let mut candidate = root.to_path_buf();
    let components: Vec<_> = Path::new(relative).components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(LocalModelError::new("invalid_catalog"));
        };
        candidate.push(name);
        let metadata = match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !require_present => {
                return Ok(None);
            }
            Err(_) => return Err(LocalModelError::new("model_path_rejected")),
        };
        if metadata.file_type().is_symlink()
            || index + 1 < components.len() && !metadata.is_dir()
            || index + 1 == components.len() && !metadata.is_file()
        {
            return Err(LocalModelError::new("model_path_rejected"));
        }
    }
    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|_| LocalModelError::new("model_path_rejected"))?;
    if !canonical.starts_with(root) {
        return Err(LocalModelError::new("model_path_rejected"));
    }
    Ok(Some(canonical))
}

fn resolve_install_directory(
    root: &Path,
    relative: &str,
    require_present: bool,
) -> Result<Option<PathBuf>, LocalModelError> {
    if !valid_relative_path(relative) {
        return Err(LocalModelError::new("invalid_catalog"));
    }
    let mut candidate = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(name) = component else {
            return Err(LocalModelError::new("invalid_catalog"));
        };
        candidate.push(name);
        let metadata = match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !require_present => {
                return Ok(None);
            }
            Err(_) => return Err(LocalModelError::new("model_path_rejected")),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(LocalModelError::new("model_path_rejected"));
        }
    }
    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|_| LocalModelError::new("model_path_rejected"))?;
    if !canonical.starts_with(root) {
        return Err(LocalModelError::new("model_path_rejected"));
    }
    Ok(Some(canonical))
}

fn partial_path(destination: &Path) -> Result<PathBuf, LocalModelError> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| LocalModelError::new("model_path_rejected"))?;
    Ok(destination.with_file_name(format!("{name}.echowall-partial")))
}

fn validate_partial(path: &Path, maximum_bytes: u64) -> Result<(), LocalModelError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > maximum_bytes =>
        {
            Err(LocalModelError::new("model_path_rejected"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(LocalModelError::new("model_install_failed")),
    }
}

async fn partial_digest(
    path: &Path,
    expected: &CatalogFile,
) -> Result<(Sha256, u64), LocalModelError> {
    let mut digest = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut input = match tokio::fs::File::open(path).await {
        Ok(input) => input,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((digest, 0));
        }
        Err(_) => return Err(LocalModelError::new("model_install_failed")),
    };
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .await
            .map_err(|_| LocalModelError::new("model_install_failed"))?;
        if read == 0 {
            break;
        }
        size_bytes = size_bytes
            .checked_add(read as u64)
            .ok_or_else(|| LocalModelError::new("model_download_rejected"))?;
        if size_bytes > expected.size_bytes {
            return Err(LocalModelError::new("model_download_rejected"));
        }
        digest.update(&buffer[..read]);
    }
    if size_bytes == expected.size_bytes {
        if hex::encode(digest.clone().finalize()) != expected.sha256 {
            return Err(LocalModelError::new("model_verification_failed"));
        }
        input
            .sync_all()
            .await
            .map_err(|_| LocalModelError::new("model_install_failed"))?;
    }
    Ok((digest, size_bytes))
}

fn valid_content_range(value: &str, start: u64, total: u64) -> bool {
    let Some(value) = value.strip_prefix("bytes ") else {
        return false;
    };
    let Some((range, supplied_total)) = value.split_once('/') else {
        return false;
    };
    let Some((supplied_start, supplied_end)) = range.split_once('-') else {
        return false;
    };
    supplied_start.parse::<u64>().ok() == Some(start)
        && supplied_end.parse::<u64>().ok() == total.checked_sub(1)
        && supplied_total.parse::<u64>().ok() == Some(total)
}

fn remove_regular_file(path: &Path) -> Result<(), LocalModelError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| LocalModelError::new("model_remove_failed"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LocalModelError::new("model_path_rejected"));
    }
    std::fs::remove_file(path).map_err(|_| LocalModelError::new("model_remove_failed"))
}

async fn sync_parent(path: &Path) -> Result<(), LocalModelError> {
    let parent = path
        .parent()
        .ok_or_else(|| LocalModelError::new("model_install_failed"))?
        .to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| LocalModelError::new("model_install_failed"))
    })
    .await
    .map_err(|_| LocalModelError::new("model_install_failed"))?
}

fn remove_pack(root: &Path, catalog: &LocalPackCatalog) -> Result<(), LocalModelError> {
    remove_catalog_files(root, &catalog.files)
}

fn remove_catalog_files(root: &Path, files: &[CatalogFile]) -> Result<(), LocalModelError> {
    let mut directories = HashSet::new();
    for file in files {
        if let Some(path) = resolve_install_file(root, &file.install_path, false)? {
            remove_regular_file(&path)?;
        }
        let destination = root.join(&file.install_path);
        if let Ok(partial) = partial_path(&destination) {
            match std::fs::symlink_metadata(&partial) {
                Ok(_) => remove_regular_file(&partial)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(LocalModelError::new("model_remove_failed")),
            }
        }
        let mut parent = destination.parent();
        while let Some(directory) = parent {
            if directory == root {
                break;
            }
            directories.insert(directory.to_path_buf());
            parent = directory.parent();
        }
    }
    let mut directories: Vec<_> = directories.into_iter().collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        match std::fs::remove_dir(&directory) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(_) => return Err(LocalModelError::new("model_remove_failed")),
        }
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=128).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2_048
        && !value.contains('\\')
        && !Path::new(value).is_absolute()
        && Path::new(value).components().all(|component| {
            matches!(component, Component::Normal(_)) && component.as_os_str().to_str().is_some()
        })
}

fn valid_source_path(value: &str) -> bool {
    valid_relative_path(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
}

#[cfg(target_os = "macos")]
fn system_memory_bytes() -> Option<u64> {
    use std::ffi::{c_char, c_void};

    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            old_value: *mut c_void,
            old_length: *mut usize,
            new_value: *mut c_void,
            new_length: usize,
        ) -> i32;
    }
    let name = c"hw.memsize";
    let mut value = 0_u64;
    let mut length = std::mem::size_of::<u64>();
    let result = unsafe {
        sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && length == std::mem::size_of::<u64>() && value > 0).then_some(value)
}

#[cfg(not(target_os = "macos"))]
const fn system_memory_bytes() -> Option<u64> {
    None
}

fn pinned_huggingface_base(url: &reqwest::Url) -> bool {
    if url.scheme() != "https"
        || url.host_str() != Some("huggingface.co")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(segments) = url.path_segments() else {
        return false;
    };
    let segments: Vec<_> = segments.collect();
    segments.len() == 4
        && !segments[0].is_empty()
        && !segments[1].is_empty()
        && segments[2] == "resolve"
        && segments[3].len() == 40
        && segments[3]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_pinned_revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalModelError {
    pub code: &'static str,
}

impl LocalModelError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }
}

impl std::fmt::Display for LocalModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self.code {
            "model_install_canceled" => "local model installation was canceled",
            "model_operation_in_progress" => "a local model operation is already running",
            "model_pack_not_installed" => "the full local model pack is not installed",
            "insufficient_model_memory" => {
                "the full local quality model requires at least 32 GiB of unified memory"
            }
            _ => "local model pack is unavailable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for LocalModelError {}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn embedded_catalog_is_closed_unique_and_default_on() {
        let catalog = LocalPackCatalog::load().unwrap();
        assert_eq!(catalog.files.len(), 23);
        assert_eq!(catalog.diarization_model_files().unwrap().len(), 21);
        assert_eq!(
            catalog.summary_model_file().unwrap().size_bytes,
            17_559_178_144
        );
        assert_eq!(catalog.total_bytes(), 18_154_818_756);
        assert!(catalog.diarization_default);
        assert_eq!(catalog.minimum_macos_major, 14);
        assert_eq!(catalog.minimum_memory_bytes, 32 * 1024 * 1024 * 1024);
        #[cfg(target_os = "macos")]
        assert!(system_memory_bytes().is_some_and(|bytes| bytes >= catalog.minimum_memory_bytes));
    }

    #[test]
    fn qwen_candidate_catalog_is_separate_closed_and_exact() {
        let catalog = QwenCandidateCatalog::load().unwrap();
        assert_eq!(catalog.pack_id, QWEN_CANDIDATE_PACK_ID);
        assert_eq!(catalog.runtime_id, LOCAL_QWEN_RUNTIME_ID);
        assert_eq!(catalog.asr_files().unwrap().len(), 3);
        assert_eq!(catalog.aligner_files().unwrap().len(), 2);
        assert_eq!(catalog.total_bytes(), 6_539_619_722);
        assert_eq!(catalog.sources.len(), 2);
        assert!(catalog
            .files
            .iter()
            .all(|file| file.install_path.starts_with("models/qwen/")));
        assert!(LocalPackCatalog::load()
            .unwrap()
            .files
            .iter()
            .all(|file| !file.install_path.starts_with("models/qwen/")));
    }

    #[test]
    fn speakerkit_candidate_catalog_is_separate_closed_and_exact() {
        let catalog = SpeakerKitCandidateCatalog::load().unwrap();
        assert_eq!(catalog.pack_id, SPEAKERKIT_CANDIDATE_PACK_ID);
        assert_eq!(catalog.quality_preset, LOCAL_DIARIZATION_SPEAKERKIT_PRESET);
        assert_eq!(
            catalog.model_revision,
            "86ec9c929b52208b6656eb6a6361ed0d822a1f78"
        );
        assert_eq!(catalog.model_files().unwrap().len(), 29);
        assert_eq!(catalog.total_bytes(), 17_187_070);
        assert_eq!(catalog.sources.len(), 1);
        assert!(catalog
            .files
            .iter()
            .all(|file| file.install_path.starts_with(SPEAKERKIT_INSTALL_PREFIX)));
        assert!(LocalPackCatalog::load()
            .unwrap()
            .files
            .iter()
            .all(|file| !file.install_path.starts_with(SPEAKERKIT_INSTALL_PREFIX)));
    }

    #[test]
    fn install_boundary_rejects_symlinks_and_unknown_pack_ids() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("models")).unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), root.path().join("models/whisper")).unwrap();
        assert_eq!(
            ensure_install_destination(root.path(), "models/whisper/large-v3-turbo-q5_0/model.bin")
                .unwrap_err()
                .code,
            "model_path_rejected"
        );

        let manager = LocalModelPackManager::open(root.path()).unwrap();
        assert_eq!(
            manager.require_pack("../../other").unwrap_err().code,
            "unknown_model_pack"
        );
        assert_eq!(
            manager
                .require_qwen_pack(FULL_LOCAL_PACK_ID)
                .unwrap_err()
                .code,
            "unknown_model_pack"
        );
        assert_eq!(
            manager
                .require_speakerkit_pack(FULL_LOCAL_PACK_ID)
                .unwrap_err()
                .code,
            "unknown_model_pack"
        );
        assert!(manager.cancel_install(QWEN_CANDIDATE_PACK_ID).is_ok());
        assert!(manager.cancel_install(SPEAKERKIT_CANDIDATE_PACK_ID).is_ok());
    }

    #[test]
    fn download_redirect_allowlist_is_closed() {
        for url in [
            "https://huggingface.co/owner/repo/resolve/revision/model.bin",
            "https://hf.co/model.bin",
            "https://us.aws.cdn.hf.co/xet-bridge-us/object",
        ] {
            assert!(allowed_download_url(&reqwest::Url::parse(url).unwrap()));
        }
        assert!(pinned_huggingface_base(
            &reqwest::Url::parse(
                "https://huggingface.co/owner/repo/resolve/0123456789abcdef0123456789abcdef01234567"
            )
            .unwrap()
        ));
        for url in [
            "https://huggingface.co/owner/repo/resolve/main",
            "https://huggingface.co/owner/repo/blob/0123456789abcdef0123456789abcdef01234567",
            "https://user@huggingface.co/owner/repo/resolve/0123456789abcdef0123456789abcdef01234567",
        ] {
            assert!(!pinned_huggingface_base(
                &reqwest::Url::parse(url).unwrap()
            ));
        }
        assert!(valid_source_path("Embedding.mlmodelc/weights/weight.bin"));
        assert!(!valid_source_path("model.bin?download=true"));
        for url in [
            "http://huggingface.co/model.bin",
            "https://huggingface.co.attacker.invalid/model.bin",
            "https://hf.co.attacker.invalid/model.bin",
            "https://example.com/model.bin",
        ] {
            assert!(!allowed_download_url(&reqwest::Url::parse(url).unwrap()));
        }
    }

    #[tokio::test]
    async fn partial_downloads_are_bounded_hashed_and_range_validated() {
        assert!(valid_content_range("bytes 123-999/1000", 123, 1000));
        for invalid in [
            "bytes 122-999/1000",
            "bytes 123-998/1000",
            "bytes 123-999/999",
            "items 123-999/1000",
        ] {
            assert!(!valid_content_range(invalid, 123, 1000));
        }

        let root = TempDir::new().unwrap();
        let data = b"resumable-public-model-fragment";
        let file = CatalogFile {
            source_id: "fixture".to_owned(),
            source_path: "model.gguf".to_owned(),
            install_path: "models/summary/fixture/model.gguf".to_owned(),
            sha256: hex::encode(Sha256::digest(data)),
            size_bytes: data.len() as u64,
        };
        let destination = ensure_install_destination(root.path(), &file.install_path).unwrap();
        let partial = partial_path(&destination).unwrap();
        std::fs::write(&partial, data).unwrap();
        validate_partial(&partial, file.size_bytes).unwrap();
        let (_, bytes) = partial_digest(&partial, &file).await.unwrap();
        assert_eq!(bytes, file.size_bytes);

        std::fs::write(&partial, vec![b'x'; data.len()]).unwrap();
        assert_eq!(
            partial_digest(&partial, &file).await.unwrap_err().code,
            "model_verification_failed"
        );
        assert_eq!(
            validate_partial(&partial, 1).unwrap_err().code,
            "model_path_rejected"
        );
    }

    #[test]
    fn exact_candidate_tree_rejects_extra_files_and_symlinks() {
        let root = TempDir::new().unwrap();
        let root_path = std::fs::canonicalize(root.path()).unwrap();
        let bytes = b"verified-model";
        let file = CatalogFile {
            source_id: "fixture".to_owned(),
            source_path: "model.bin".to_owned(),
            install_path: format!("{SPEAKERKIT_INSTALL_PREFIX}model.bin"),
            sha256: hex::encode(Sha256::digest(bytes)),
            size_bytes: bytes.len() as u64,
        };
        let model = root_path.join(&file.install_path);
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        std::fs::write(&model, bytes).unwrap();
        assert!(verify_exact_catalog_tree(
            &root_path,
            std::slice::from_ref(&file),
            SPEAKERKIT_INSTALL_PREFIX
        )
        .unwrap());

        let extra = model.parent().unwrap().join("download.cache");
        std::fs::write(&extra, b"unexpected").unwrap();
        assert!(!verify_exact_catalog_tree(
            &root_path,
            std::slice::from_ref(&file),
            SPEAKERKIT_INSTALL_PREFIX
        )
        .unwrap());
        std::fs::remove_file(&extra).unwrap();

        let outside = TempDir::new().unwrap();
        let link = model.parent().unwrap().join("linked-cache");
        symlink(outside.path(), &link).unwrap();
        assert_eq!(
            verify_exact_catalog_tree(
                &root_path,
                std::slice::from_ref(&file),
                SPEAKERKIT_INSTALL_PREFIX
            )
            .unwrap_err()
            .code,
            "model_path_rejected"
        );
    }

    #[tokio::test]
    async fn busy_lease_blocks_overlapping_model_operations_and_releases_on_drop() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let lease = manager.acquire_busy().unwrap();
        assert_eq!(
            manager.remove(FULL_LOCAL_PACK_ID).await.unwrap_err().code,
            "model_operation_in_progress"
        );
        drop(lease);
        assert!(manager.acquire_busy().is_ok());
    }

    #[tokio::test]
    async fn qwen_status_and_removal_are_isolated_from_the_full_local_pack() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let status = manager.qwen_status(true).await.unwrap();
        assert_eq!(status.pack_id, QWEN_CANDIDATE_PACK_ID);
        assert_eq!(status.total_bytes, 6_539_619_722);
        assert!(!status.installed);

        let full_marker = root.path().join("models/whisper/keep/model.bin");
        std::fs::create_dir_all(full_marker.parent().unwrap()).unwrap();
        std::fs::write(&full_marker, b"user-owned-full-pack-marker").unwrap();
        for file in &QwenCandidateCatalog::load().unwrap().files {
            let path = root.path().join(&file.install_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"candidate-fragment").unwrap();
        }
        let removed = manager.remove_qwen(QWEN_CANDIDATE_PACK_ID).await.unwrap();
        assert!(!removed.installed);
        assert!(full_marker.is_file());
        assert!(!root.path().join("models/qwen").exists());
    }

    #[tokio::test]
    async fn speakerkit_status_and_removal_preserve_other_model_packs_and_unknown_files() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let status = manager.speakerkit_status(true).await.unwrap();
        assert_eq!(status.pack_id, SPEAKERKIT_CANDIDATE_PACK_ID);
        assert_eq!(status.total_bytes, 17_187_070);
        assert!(!status.installed);

        let fluid_marker = root
            .path()
            .join("models/diarization/fluid-v1/keep/model.bin");
        std::fs::create_dir_all(fluid_marker.parent().unwrap()).unwrap();
        std::fs::write(&fluid_marker, b"user-owned-fluid-pack-marker").unwrap();
        for file in &SpeakerKitCandidateCatalog::load().unwrap().files {
            let path = root.path().join(&file.install_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"candidate-fragment").unwrap();
        }
        let unknown = root
            .path()
            .join("models/diarization/speakerkit-v1/user-note.txt");
        std::fs::write(&unknown, b"do not remove unrelated data").unwrap();

        let removed = manager
            .remove_speakerkit(SPEAKERKIT_CANDIDATE_PACK_ID)
            .await
            .unwrap();
        assert!(!removed.installed);
        assert!(fluid_marker.is_file());
        assert!(unknown.is_file());
        assert_eq!(
            std::fs::read(&unknown).unwrap(),
            b"do not remove unrelated data"
        );
    }

    #[tokio::test]
    #[ignore = "downloads and removes the pinned 16.91 GiB public model pack"]
    async fn live_catalog_install_verifies_every_file_and_removes_exact_pack() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let installed = manager.install(FULL_LOCAL_PACK_ID).await.unwrap();
        assert!(installed.installed);
        assert_eq!(installed.downloaded_bytes, installed.total_bytes);
        let proof = manager.proof().await.unwrap();
        assert_eq!(proof.whisper_model_id, "large-v3-turbo-q5_0");
        assert_eq!(proof.diarization_files.len(), 21);
        assert_eq!(proof.summary_model_id, "qwen3.8-27b-ud-q4-k-xl");
        let removed = manager.remove(FULL_LOCAL_PACK_ID).await.unwrap();
        assert!(!removed.installed);
        assert_eq!(removed.downloaded_bytes, 0);
        assert!(!root.path().join("models/whisper").exists());
        assert!(!root.path().join("models/diarization").exists());
        assert!(!root.path().join("models/summary").exists());
    }

    #[tokio::test]
    #[ignore = "downloads and removes the pinned 6.09 GiB Qwen candidate model pack"]
    async fn live_qwen_catalog_install_verifies_every_file_and_removes_exact_pack() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let installed = manager.install_qwen(QWEN_CANDIDATE_PACK_ID).await.unwrap();
        assert!(installed.installed);
        assert_eq!(installed.downloaded_bytes, installed.total_bytes);
        let proof = manager.qwen_proof().await.unwrap();
        assert_eq!(proof.runtime_id, LOCAL_QWEN_RUNTIME_ID);
        assert_eq!(proof.asr_model_files.len(), 3);
        assert_eq!(proof.aligner_model_files.len(), 2);
        let removed = manager.remove_qwen(QWEN_CANDIDATE_PACK_ID).await.unwrap();
        assert!(!removed.installed);
        assert_eq!(removed.downloaded_bytes, 0);
        assert!(!root.path().join("models/qwen").exists());
    }

    #[tokio::test]
    #[ignore = "downloads, verifies, and removes the pinned 16.39 MiB SpeakerKit candidate pack"]
    async fn live_speakerkit_catalog_install_verifies_every_file_and_removes_exact_pack() {
        let root = TempDir::new().unwrap();
        let manager = LocalModelPackManager::open(root.path()).unwrap();
        let installed = manager
            .install_speakerkit(SPEAKERKIT_CANDIDATE_PACK_ID)
            .await
            .unwrap();
        assert!(installed.installed);
        assert_eq!(installed.downloaded_bytes, installed.total_bytes);
        let proof = manager.speakerkit_proof().await.unwrap();
        assert_eq!(proof.pack_id, SPEAKERKIT_CANDIDATE_PACK_ID);
        assert_eq!(proof.quality_preset, LOCAL_DIARIZATION_SPEAKERKIT_PRESET);
        assert_eq!(proof.model_files.len(), 29);

        let unrelated = root
            .path()
            .join("models/diarization/speakerkit-v1/user-note.txt");
        std::fs::write(&unrelated, b"unrelated").unwrap();
        let removed = manager
            .remove_speakerkit(SPEAKERKIT_CANDIDATE_PACK_ID)
            .await
            .unwrap();
        assert!(!removed.installed);
        assert_eq!(removed.downloaded_bytes, 0);
        assert!(unrelated.is_file());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    #[ignore = "downloads, verifies, and retains both exact model packs for the authorized quality matrix"]
    async fn live_quality_matrix_model_packs_install_and_retain_at_authorized_root() {
        assert_eq!(
            std::env::var("ECHOWALL_LIVE_QUALITY_MODEL_CONFIRM").as_deref(),
            Ok("public-model-quality-matrix-authorized"),
            "quality model install requires the exact confirmation guard"
        );
        let supplied = std::path::PathBuf::from(
            std::env::var("ECHOWALL_LIVE_QUALITY_MODEL_ROOT")
                .expect("quality model root must be supplied"),
        );
        std::fs::create_dir_all(&supplied).unwrap();
        let root = std::fs::canonicalize(&supplied).unwrap();
        let repository = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .unwrap(),
        )
        .unwrap();
        let allowed_root = repository.join("local-eval");
        assert!(
            root.starts_with(&allowed_root) && root != allowed_root,
            "retained quality models must remain below the ignored local-eval root"
        );

        let manager = LocalModelPackManager::open(&root).unwrap();
        let full = manager.install(FULL_LOCAL_PACK_ID).await.unwrap();
        assert!(full.installed);
        assert_eq!(full.downloaded_bytes, full.total_bytes);
        let full_proof = manager.proof().await.unwrap();
        assert_eq!(full_proof.whisper_model_id, "large-v3-turbo-q5_0");
        assert_eq!(full_proof.diarization_files.len(), 21);
        assert_eq!(full_proof.summary_model_id, "qwen3.8-27b-ud-q4-k-xl");

        let qwen = manager.install_qwen(QWEN_CANDIDATE_PACK_ID).await.unwrap();
        assert!(qwen.installed);
        assert_eq!(qwen.downloaded_bytes, qwen.total_bytes);
        let qwen_proof = manager.qwen_proof().await.unwrap();
        assert_eq!(qwen_proof.runtime_id, LOCAL_QWEN_RUNTIME_ID);
        assert_eq!(qwen_proof.asr_model_files.len(), 3);
        assert_eq!(qwen_proof.aligner_model_files.len(), 2);

        println!(
            "{}",
            serde_json::json!({
                "state": "verified_and_retained",
                "fullPackBytes": full.total_bytes,
                "fullPackFiles": 23,
                "qwenPackBytes": qwen.total_bytes,
                "qwenPackFiles": 5,
            })
        );
    }
}
