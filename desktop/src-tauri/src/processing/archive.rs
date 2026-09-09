//! Native archive publication for the embedded processing engine.
//!
//! The adapter writes the local archive atomically, uploads canonical audio to
//! R2, and publishes note metadata through GitHub's Git Data API. It never
//! shells out or starts a local server.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use async_trait::async_trait;
use chrono::{DateTime, Duration, FixedOffset, Timelike};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::ingest::envelope::RecordingEnvelope;
use crate::r2::{self, R2Cfg, R2PutProof};
use crate::secrets::SyncTokens;

use super::direct::ArchiveEffects;
use super::engine::{EffectError, EffectErrorKind};
use super::{
    CanonicalBackupCheckpoint, NormalizedArtifactCheckpoint, PublicationBackend, PublicationProof,
    PublicationTargetPlan, RetryMode,
};

const TARGET_ID: &str = "archive";
const LOCAL_TARGET_ID: &str = "local_archive";
const MAX_LOCAL_JSON_BYTES: u64 = 32 * 1024 * 1024;
const MAX_NOTE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_GITHUB_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_GITHUB_RETRIES: usize = 4;
const NATIVE_EDIT_OUTBOX_SCHEMA: u32 = 1;
const MAX_NATIVE_EDIT_OUTBOX_BYTES: u64 = 1024 * 1024;
const MAX_PENDING_NATIVE_EDITS: usize = 1_000;
const DELETED_RECORDINGS_PATH: &str = "deleted-recordings.json";
const R2_ATTEMPT_SCHEMA: u32 = 1;
const MAX_R2_ATTEMPT_BYTES: u64 = 16 * 1024;
const VIEWER_TEMPLATE: &str = include_str!("../../../../deliveries/viewer_template.html");
const VIEWER_MARKED_JS: &[u8] = include_bytes!("../../../../deliveries/vendor/marked.min.js");
const ARCHIVE_GITIGNORE: &[u8] = b"*.m4a\n*.mp3\n*.wav\n*.qta\n*.tmp\n.DS_Store\nby-topic/\ndaily.md\ndaily.html\nindex.html\nmarked.min.js\n";
const CATEGORIES: [&str; 6] = [
    "亲密关系",
    "自我成长",
    "学习认知",
    "工作商务",
    "生活日常",
    "其他",
];

type HttpFuture = Pin<Box<dyn Future<Output = Result<ArchiveHttpResponse, ArchiveError>> + Send>>;
type R2Future = Pin<Box<dyn Future<Output = Result<R2PutProof, ArchiveError>> + Send>>;
type R2DeleteFuture =
    Pin<Box<dyn Future<Output = Result<r2::R2DeleteOutcome, ArchiveError>> + Send>>;

#[cfg(test)]
fn live_archive_phase(phase: &'static str) {
    if std::env::var("ECHOWALL_ARCHIVE_DIAGNOSTIC").as_deref() == Ok("phase-only") {
        eprintln!("echowall_archive_diagnostic_phase={phase}");
    }
}

type ArchiveOperationLock = tokio::sync::Mutex<()>;

static ARCHIVE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<ArchiveOperationLock>>>> =
    OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveMethod {
    Get,
    Post,
    Patch,
}

pub struct ArchiveHttpRequest {
    pub method: ArchiveMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub response_limit: usize,
}

impl std::fmt::Debug for ArchiveHttpRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArchiveHttpRequest")
            .field("method", &self.method)
            .field("url", &"<redacted>")
            .field(
                "header_names",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .field("response_limit", &self.response_limit)
            .finish()
    }
}

#[derive(Clone)]
pub struct ArchiveHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for ArchiveHttpResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArchiveHttpResponse")
            .field("status", &self.status)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

pub trait ArchiveHttpTransport: Send + Sync + 'static {
    fn execute(&self, request: ArchiveHttpRequest) -> HttpFuture;
}

pub trait ArchiveR2: Send + Sync + 'static {
    fn destination_identity(&self) -> String;

    fn put_verified(
        &self,
        key: String,
        recording_id: String,
        source: PathBuf,
        sha256: String,
        size_bytes: u64,
    ) -> R2Future;

    fn head_verified(
        &self,
        key: String,
        recording_id: String,
        sha256: String,
        size_bytes: u64,
    ) -> R2Future;

    fn delete_owned(
        &self,
        key: String,
        recording_id: String,
        generation: u64,
        sha256: String,
        size_bytes: u64,
    ) -> R2DeleteFuture;
}

#[derive(Clone)]
pub struct ReqwestArchiveTransport {
    client: reqwest::Client,
}

impl ReqwestArchiveTransport {
    pub fn new() -> Result<Self, ArchiveError> {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client = crate::qa::guard_http(reqwest::Client::builder())
            .use_preconfigured_tls(tls)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|_| ArchiveError::configuration())?;
        Ok(Self { client })
    }
}

impl ArchiveHttpTransport for ReqwestArchiveTransport {
    fn execute(&self, request: ArchiveHttpRequest) -> HttpFuture {
        let client = self.client.clone();
        Box::pin(async move {
            let mut builder = match request.method {
                ArchiveMethod::Get => client.get(&request.url),
                ArchiveMethod::Post => client.post(&request.url),
                ArchiveMethod::Patch => client.patch(&request.url),
            };
            for (name, value) in request.headers {
                builder = builder.header(name, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let response = builder.send().await.map_err(|_| ArchiveError::network())?;
            let status = response.status().as_u16();
            if response
                .content_length()
                .is_some_and(|length| length > request.response_limit as u64)
            {
                return Err(ArchiveError::verification());
            }
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            use futures_util::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| ArchiveError::network())?;
                if body.len().saturating_add(chunk.len()) > request.response_limit {
                    return Err(ArchiveError::verification());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(ArchiveHttpResponse { status, body })
        })
    }
}

#[derive(Clone)]
pub struct DirectArchiveR2 {
    config: R2Cfg,
}

impl DirectArchiveR2 {
    fn new(config: R2Cfg) -> Self {
        Self { config }
    }
}

impl std::fmt::Debug for DirectArchiveR2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DirectArchiveR2(<redacted>)")
    }
}

impl ArchiveR2 for DirectArchiveR2 {
    fn destination_identity(&self) -> String {
        format!("{}/{}", self.config.account_id, self.config.bucket)
    }

    fn put_verified(
        &self,
        key: String,
        recording_id: String,
        source: PathBuf,
        sha256: String,
        size_bytes: u64,
    ) -> R2Future {
        let config = self.config.clone();
        Box::pin(async move {
            r2::put_file_verified(&config, &key, &recording_id, &source, &sha256, size_bytes)
                .await
                .map_err(map_r2_error)
        })
    }

    fn head_verified(
        &self,
        key: String,
        recording_id: String,
        sha256: String,
        size_bytes: u64,
    ) -> R2Future {
        let config = self.config.clone();
        Box::pin(async move {
            r2::head_file_verified(&config, &key, &recording_id, &sha256, size_bytes)
                .await
                .map_err(map_r2_error)
        })
    }

    fn delete_owned(
        &self,
        key: String,
        recording_id: String,
        generation: u64,
        sha256: String,
        size_bytes: u64,
    ) -> R2DeleteFuture {
        let config = self.config.clone();
        Box::pin(async move {
            r2::delete_owned_object(
                &config,
                &key,
                &recording_id,
                generation,
                &sha256,
                size_bytes,
            )
            .await
            .map_err(map_r2_error)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveErrorKind {
    Configuration,
    Network,
    Conflict,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveError {
    pub kind: ArchiveErrorKind,
    message: &'static str,
}

impl ArchiveError {
    fn configuration() -> Self {
        Self::new(ArchiveErrorKind::Configuration)
    }

    fn network() -> Self {
        Self::new(ArchiveErrorKind::Network)
    }

    fn conflict() -> Self {
        Self::new(ArchiveErrorKind::Conflict)
    }

    fn verification() -> Self {
        Self::new(ArchiveErrorKind::Verification)
    }

    fn new(kind: ArchiveErrorKind) -> Self {
        Self {
            kind,
            message: match kind {
                ArchiveErrorKind::Configuration => "archive is not configured",
                ArchiveErrorKind::Network => "archive network operation failed",
                ArchiveErrorKind::Conflict => "archive publication requires reconciliation",
                ArchiveErrorKind::Verification => "archive verification failed",
            },
        }
    }
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ArchiveError {}

#[derive(Clone)]
struct GitHubConfig {
    repo: String,
    token: String,
}

impl std::fmt::Debug for GitHubConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GitHubConfig(<redacted>)")
    }
}

pub type ProductionArchive = ArchiveAdapter<ReqwestArchiveTransport, DirectArchiveR2>;

#[derive(Debug, Clone)]
pub struct DeferredArchive {
    archive_root: PathBuf,
    inbox_root: PathBuf,
    sync_base: PathBuf,
    native_edit_outbox: PathBuf,
}

impl DeferredArchive {
    pub fn new(
        archive_root: PathBuf,
        inbox_root: PathBuf,
        sync_base: PathBuf,
    ) -> Result<Self, ArchiveError> {
        fs::create_dir_all(&archive_root).map_err(|_| ArchiveError::verification())?;
        let archive_root =
            fs::canonicalize(archive_root).map_err(|_| ArchiveError::verification())?;
        let inbox_root = fs::canonicalize(inbox_root).map_err(|_| ArchiveError::verification())?;
        if archive_root.starts_with(&inbox_root) || inbox_root.starts_with(&archive_root) {
            return Err(ArchiveError::configuration());
        }
        let native_edit_outbox = open_native_edit_outbox(&archive_root)?;
        Ok(Self {
            archive_root,
            inbox_root,
            sync_base,
            native_edit_outbox,
        })
    }

    fn load(&self) -> Result<ProductionArchive, EffectError> {
        ProductionArchive::production(
            self.archive_root.clone(),
            self.inbox_root.clone(),
            &self.sync_base,
        )
        .map_err(map_archive_error)
    }

    fn load_local(
        &self,
    ) -> Result<ArchiveAdapter<OfflineArchiveHttp, OfflineArchiveR2>, EffectError> {
        ArchiveAdapter::with_components(
            self.archive_root.clone(),
            self.inbox_root.clone(),
            "local/archive".to_owned(),
            "local-only-no-credential".to_owned(),
            Arc::new(OfflineArchiveHttp),
            Arc::new(OfflineArchiveR2),
        )
        .map_err(map_archive_error)
    }

    pub(crate) async fn begin_transaction(&self) -> Result<NativeArchiveTransaction, EffectError> {
        begin_native_archive_transaction(&self.archive_root)
            .await
            .map_err(map_archive_error)
    }

    #[cfg(any(not(mobile), test))]
    pub(crate) async fn initialize_local(&self) -> Result<(), EffectError> {
        let _transaction = begin_native_archive_transaction(&self.archive_root)
            .await
            .map_err(map_archive_error)?;
        let manifest = load_manifest(&self.archive_root).map_err(map_archive_error)?;
        let manifest_path = self.archive_root.join("manifest.json");
        if !manifest_path.exists() {
            atomic_write(
                &manifest_path,
                &serialize_manifest(&manifest).map_err(map_archive_error)?,
            )
            .map_err(map_archive_error)?;
        }
        rebuild_viewer(&self.archive_root, &manifest).map_err(map_archive_error)?;
        Ok(())
    }

    fn load_with_binding(
        &self,
    ) -> Result<(ProductionArchive, ArchiveDestinationBinding), EffectError> {
        let tokens = crate::sync::load_bound_credentials(&self.sync_base)
            .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
        let binding = ArchiveDestinationBinding::from_tokens(&tokens);
        let archive = ProductionArchive::from_tokens(
            self.archive_root.clone(),
            self.inbox_root.clone(),
            &tokens,
        )
        .map_err(map_archive_error)?;
        Ok((archive, binding))
    }

    pub(crate) fn stage_native_edit_in_transaction(
        &self,
        transaction: &NativeArchiveTransaction,
        edit: NativeArchiveEdit,
    ) -> Result<NativeEditTicket, EffectError> {
        let destination = self.current_destination_binding()?;
        self.stage_native_edit_with_binding(transaction, edit, destination)
    }

    fn stage_native_edit_with_binding(
        &self,
        transaction: &NativeArchiveTransaction,
        edit: NativeArchiveEdit,
        destination: ArchiveDestinationBinding,
    ) -> Result<NativeEditTicket, EffectError> {
        if transaction.archive_root != self.archive_root {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        let edit_id = Uuid::new_v4();
        let sequence =
            next_native_edit_sequence(&self.native_edit_outbox).map_err(map_archive_error)?;
        let record = NativeEditOutboxRecord {
            schema_version: NATIVE_EDIT_OUTBOX_SCHEMA,
            edit_id,
            sequence,
            destination,
            edit,
        };
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|_| EffectError::new(EffectErrorKind::Verification))?;
        if bytes.len() as u64 > MAX_NATIVE_EDIT_OUTBOX_BYTES {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        let path = self
            .native_edit_outbox
            .join(format!("{sequence:020}-{edit_id}.json"));
        atomic_write(&path, &bytes).map_err(map_archive_error)?;
        Ok(NativeEditTicket { path, record })
    }

    pub(crate) async fn publish_staged_native_edit_in_transaction(
        &self,
        transaction: &NativeArchiveTransaction,
        ticket: &NativeEditTicket,
    ) -> Result<PublicationProof, EffectError> {
        if transaction.archive_root != self.archive_root {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        if ticket.path.parent() != Some(self.native_edit_outbox.as_path()) {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        let (archive, current_destination) = self.load_with_binding()?;
        verify_destination_binding(&ticket.record.destination, &current_destination)?;
        let proof = archive
            .publish_native_edit_inner(ticket.record.edit.clone())
            .await
            .map_err(map_archive_error)?;
        if matches!(ticket.record.edit, NativeArchiveEdit::Delete { .. }) {
            apply_native_edit_locally(&self.archive_root, &ticket.record.edit)
                .map_err(map_archive_error)?;
        }
        fs::remove_file(&ticket.path)
            .map_err(|_| EffectError::new(EffectErrorKind::Verification))?;
        sync_directory(&self.native_edit_outbox).map_err(map_archive_error)?;
        Ok(proof)
    }

    fn current_destination_binding(&self) -> Result<ArchiveDestinationBinding, EffectError> {
        let tokens = crate::sync::load_bound_credentials(&self.sync_base)
            .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
        Ok(ArchiveDestinationBinding::from_tokens(&tokens))
    }

    pub(crate) async fn execute_native_edit_in_transaction(
        &self,
        transaction: &NativeArchiveTransaction,
        edit: NativeArchiveEdit,
    ) -> Result<PublicationProof, EffectError> {
        let ticket = self.stage_native_edit_in_transaction(transaction, edit)?;
        if !matches!(ticket.record.edit, NativeArchiveEdit::Delete { .. }) {
            apply_native_edit_locally(&self.archive_root, &ticket.record.edit)
                .map_err(map_archive_error)?;
        }
        self.drain_native_edit_outbox_in_transaction(transaction, Some(ticket.record.edit_id))
            .await?
            .ok_or_else(|| EffectError::new(EffectErrorKind::PublicationConflict))
    }

    pub(crate) async fn resume_pending_native_edits(&self) -> Result<(), EffectError> {
        let transaction = begin_native_archive_transaction(&self.archive_root)
            .await
            .map_err(map_archive_error)?;
        self.drain_native_edit_outbox_in_transaction(&transaction, None)
            .await?;
        Ok(())
    }

    async fn drain_native_edit_outbox_in_transaction(
        &self,
        transaction: &NativeArchiveTransaction,
        target: Option<Uuid>,
    ) -> Result<Option<PublicationProof>, EffectError> {
        let mut target_proof = None;
        let mut target_conflicted = false;
        for ticket in
            load_native_edit_tickets(&self.native_edit_outbox).map_err(map_archive_error)?
        {
            match self
                .publish_staged_native_edit_in_transaction(transaction, &ticket)
                .await
            {
                Ok(proof) => {
                    if target == Some(ticket.record.edit_id) {
                        target_proof = Some(proof);
                    }
                }
                Err(error) if error.kind == EffectErrorKind::PublicationConflict => {
                    quarantine_native_edit_ticket(&self.native_edit_outbox, &ticket)
                        .map_err(map_archive_error)?;
                    if target == Some(ticket.record.edit_id) {
                        target_conflicted = true;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        if target_conflicted {
            return Err(EffectError::new(EffectErrorKind::PublicationConflict));
        }
        Ok(target_proof)
    }
}

#[derive(Clone)]
struct OfflineArchiveHttp;

impl ArchiveHttpTransport for OfflineArchiveHttp {
    fn execute(&self, _: ArchiveHttpRequest) -> HttpFuture {
        Box::pin(async { Err(ArchiveError::configuration()) })
    }
}

#[derive(Clone)]
struct OfflineArchiveR2;

impl ArchiveR2 for OfflineArchiveR2 {
    fn destination_identity(&self) -> String {
        "local-only".to_owned()
    }

    fn put_verified(&self, _: String, _: String, _: PathBuf, _: String, _: u64) -> R2Future {
        Box::pin(async { Err(ArchiveError::configuration()) })
    }

    fn head_verified(&self, _: String, _: String, _: String, _: u64) -> R2Future {
        Box::pin(async { Err(ArchiveError::configuration()) })
    }

    fn delete_owned(&self, _: String, _: String, _: u64, _: String, _: u64) -> R2DeleteFuture {
        Box::pin(async { Err(ArchiveError::configuration()) })
    }
}

#[derive(Debug)]
pub(crate) struct NativeEditTicket {
    path: PathBuf,
    record: NativeEditOutboxRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeEditOutboxRecord {
    schema_version: u32,
    edit_id: Uuid,
    sequence: u64,
    destination: ArchiveDestinationBinding,
    edit: NativeArchiveEdit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveDestinationBinding {
    schema_version: u32,
    github_repo: String,
    r2_account_id: String,
    r2_bucket: String,
}

impl ArchiveDestinationBinding {
    fn from_tokens(tokens: &SyncTokens) -> Self {
        Self {
            schema_version: tokens.schema_version,
            github_repo: tokens.repo.clone(),
            r2_account_id: tokens.r2_account_id.clone(),
            r2_bucket: tokens.bucket.clone(),
        }
    }
}

fn verify_destination_binding(
    expected: &ArchiveDestinationBinding,
    current: &ArchiveDestinationBinding,
) -> Result<(), EffectError> {
    if expected == current {
        Ok(())
    } else {
        Err(EffectError::new(EffectErrorKind::PublicationConflict))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct R2AttemptRecord {
    schema_version: u32,
    recording_id: String,
    generation: u64,
    key: String,
    sha256: String,
    size_bytes: u64,
    github_repo: String,
    r2_destination: String,
}

pub struct ArchiveAdapter<H: ArchiveHttpTransport, R: ArchiveR2> {
    archive_root: PathBuf,
    inbox_root: PathBuf,
    github: GitHubConfig,
    http: Arc<H>,
    r2: Arc<R>,
    operation_lock: Arc<ArchiveOperationLock>,
    process_lock: Arc<File>,
    r2_attempt_root: PathBuf,
    r2_destination: String,
}

impl<H: ArchiveHttpTransport, R: ArchiveR2> std::fmt::Debug for ArchiveAdapter<H, R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ArchiveAdapter(<redacted>)")
    }
}

impl ProductionArchive {
    /// Loads the versioned, destination-bound archive credential record used
    /// by the viewer sync path.
    pub fn production(
        archive_root: PathBuf,
        inbox_root: PathBuf,
        sync_base: &Path,
    ) -> Result<Self, ArchiveError> {
        let tokens = crate::sync::load_bound_credentials(sync_base)
            .ok_or_else(ArchiveError::configuration)?;
        Self::from_tokens(archive_root, inbox_root, &tokens)
    }

    fn from_tokens(
        archive_root: PathBuf,
        inbox_root: PathBuf,
        tokens: &SyncTokens,
    ) -> Result<Self, ArchiveError> {
        let repo = tokens.repo.clone();
        let bucket = tokens.bucket.clone();
        let http = Arc::new(ReqwestArchiveTransport::new()?);
        let r2 = Arc::new(DirectArchiveR2::new(r2_config(tokens, bucket)));
        let process_lock_path = archive_process_lock_path(&archive_root)?;
        Self::with_components_and_lock_path(
            archive_root,
            inbox_root,
            repo,
            tokens.github_pat.clone(),
            http,
            r2,
            process_lock_path,
        )
    }
}

impl<H: ArchiveHttpTransport, R: ArchiveR2> ArchiveAdapter<H, R> {
    pub fn with_components(
        archive_root: PathBuf,
        inbox_root: PathBuf,
        repo: String,
        github_token: String,
        http: Arc<H>,
        r2: Arc<R>,
    ) -> Result<Self, ArchiveError> {
        let process_lock_path = archive_process_lock_path(&archive_root)?;
        Self::with_components_and_lock_path(
            archive_root,
            inbox_root,
            repo,
            github_token,
            http,
            r2,
            process_lock_path,
        )
    }

    fn with_components_and_lock_path(
        archive_root: PathBuf,
        inbox_root: PathBuf,
        repo: String,
        github_token: String,
        http: Arc<H>,
        r2: Arc<R>,
        process_lock_path: PathBuf,
    ) -> Result<Self, ArchiveError> {
        validate_repo(&repo)?;
        if github_token.trim().is_empty() || github_token.len() > 4096 {
            return Err(ArchiveError::configuration());
        }
        fs::create_dir_all(&archive_root).map_err(|_| ArchiveError::verification())?;
        let archive_root =
            fs::canonicalize(archive_root).map_err(|_| ArchiveError::verification())?;
        let inbox_root = fs::canonicalize(inbox_root).map_err(|_| ArchiveError::verification())?;
        if archive_root.starts_with(&inbox_root) || inbox_root.starts_with(&archive_root) {
            return Err(ArchiveError::configuration());
        }
        let operation_lock = archive_lock(&archive_root)?;
        let process_lock = Arc::new(open_archive_process_lock(&process_lock_path)?);
        let r2_attempt_root = open_runtime_subdirectory(&archive_root, "r2-attempts")?;
        let r2_destination = r2.destination_identity();
        Ok(Self {
            archive_root,
            inbox_root,
            github: GitHubConfig {
                repo,
                token: github_token,
            },
            http,
            r2,
            operation_lock,
            process_lock,
            r2_attempt_root,
            r2_destination,
        })
    }

    fn prepare_local(
        &self,
        generation: u64,
        envelope: &RecordingEnvelope,
        transcript: &Value,
        summary: &Value,
        artifact: &NormalizedArtifactCheckpoint,
        preparation: ArchivePreparation<'_>,
    ) -> Result<PreparedArchive, ArchiveError> {
        let empty_remote = RemoteArchive {
            manifest: BTreeMap::new(),
            notes: Vec::new(),
            deleted_recording_ids: Default::default(),
        };
        let (remote, include_remote_fields) = match preparation {
            ArchivePreparation::Remote(remote) => (remote, true),
            ArchivePreparation::LocalOnly => (&empty_remote, false),
        };
        envelope
            .validate()
            .map_err(|_| ArchiveError::verification())?;
        #[cfg(test)]
        live_archive_phase("envelope_validated");
        if generation == 0
            || envelope.normalized_sha256.as_deref() != Some(&artifact.sha256)
            || envelope.normalized_audio.as_deref() != Some(&artifact.relative_path)
        {
            return Err(ArchiveError::verification());
        }
        if remote
            .deleted_recording_ids
            .contains(&envelope.recording_id.to_string())
            || load_local_deleted_recordings(&self.archive_root)?
                .contains(&envelope.recording_id.to_string())
            || pending_deleted_recording_ids(&self.archive_root)?
                .contains(&envelope.recording_id.to_string())
        {
            return Err(ArchiveError::conflict());
        }
        let source = resolve_source(&self.inbox_root, envelope.recording_id, artifact)?;
        #[cfg(test)]
        live_archive_phase("source_verified");
        let title = archive_title(envelope, summary)?;
        #[cfg(test)]
        live_archive_phase("title_validated");
        let display_title_value = display_title(envelope, &title);
        let category = clean_category(summary.get("category").and_then(Value::as_str));
        let local_manifest = load_manifest(&self.archive_root)?;
        #[cfg(test)]
        live_archive_phase("local_manifest_loaded");
        let local_prior = prior_entry(&local_manifest, envelope.recording_id).cloned();
        let remote_prior = prior_entry(&remote.manifest, envelope.recording_id).cloned();
        let mut manifest = merge_manifests(&remote.manifest, &local_manifest)?;
        #[cfg(test)]
        live_archive_phase("manifests_merged");
        for (relative, contents) in &remote.notes {
            let path = safe_archive_path(&self.archive_root, relative)?;
            atomic_write(&path, contents)?;
        }
        #[cfg(test)]
        live_archive_phase("remote_notes_staged");
        let key = reserve_archive_key(&manifest, envelope.recording_id, envelope.captured_at);
        let slot = parse_archive_key(&key)?;
        #[cfg(test)]
        live_archive_phase("archive_slot_reserved");
        let stem = recording_stem(slot, &title);
        let day = slot.format("%Y-%m-%d").to_string();
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| matches!(*value, "wav" | "m4a" | "mp3"))
            .unwrap_or("wav");
        let generated_note = format!("{day}/{stem}.md");
        // Keep the local archive path compatible with the legacy
        // HHMMSS-title layout. R2 gets a separate immutable identity so every
        // device converges even when friendly archive slots collide.
        let generated_audio = format!("{day}/{stem}.{extension}");
        let audio_day = envelope.captured_at.format("%Y-%m-%d");
        let audio_time = envelope.captured_at.format("%H%M%S");
        let audio_hash = artifact
            .sha256
            .get(..16)
            .ok_or_else(ArchiveError::verification)?;
        let generated_r2_key = format!(
            "{audio_day}/{audio_time}-recording-{audio_hash}-g{generation}-{}.{extension}",
            envelope.recording_id
        );
        let reusable_remote_r2 =
            reusable_remote_r2(remote_prior.as_ref(), envelope.recording_id, artifact)?;
        let pending_attempt = self.load_r2_attempt(envelope.recording_id, generation)?;
        let reusable_attempt = local_prior.as_ref().filter(|entry| {
            remote_prior.is_none()
                && entry.get("publish_generation").and_then(Value::as_u64) == Some(generation)
                && entry.get("title").and_then(Value::as_str) == Some(display_title_value.as_str())
        });
        let note_relative = reusable_attempt
            .and_then(|entry| entry.get("note"))
            .and_then(Value::as_str)
            .filter(|path| {
                path.ends_with(".md")
                    && validate_owned_archive_path(
                        &self.archive_root,
                        slot,
                        path,
                        OwnedArchivePathKind::Note,
                    )
                    .is_ok()
                    && !manifest_path_owned_by_other(
                        &remote.manifest,
                        "note",
                        path,
                        envelope.recording_id,
                    )
            })
            .unwrap_or(&generated_note)
            .to_owned();
        let audio_relative = reusable_attempt
            .and_then(|entry| entry.get("audio"))
            .and_then(Value::as_str)
            .filter(|path| {
                path.ends_with(&format!(".{extension}"))
                    && validate_owned_archive_path(
                        &self.archive_root,
                        slot,
                        path,
                        OwnedArchivePathKind::Audio,
                    )
                    .is_ok()
                    && is_friendly_archive_audio(slot, path)
                    && !manifest_path_owned_by_other(
                        &remote.manifest,
                        "audio",
                        path,
                        envelope.recording_id,
                    )
            })
            .unwrap_or(&generated_audio)
            .to_owned();
        let (r2_key, r2_generation, reuse_remote_r2, replacement_r2_key) =
            if let Some(attempt) = pending_attempt {
                if attempt.sha256 != artifact.sha256 || attempt.size_bytes != artifact.size_bytes {
                    return Err(ArchiveError::conflict());
                }
                (attempt.key, generation, false, None)
            } else if let Some(reusable) = reusable_remote_r2 {
                (
                    reusable.key,
                    reusable.generation,
                    true,
                    Some(generated_r2_key.clone()),
                )
            } else {
                let key = reusable_attempt
                    .and_then(|entry| entry.get("r2_key"))
                    .and_then(Value::as_str)
                    .filter(|path| {
                        r2::is_generation_owned_key(
                            path,
                            &envelope.recording_id.to_string(),
                            generation,
                        )
                    })
                    .unwrap_or(&generated_r2_key)
                    .to_owned();
                (key, generation, false, None)
            };
        safe_archive_path(&self.archive_root, &note_relative)?;
        safe_archive_path(&self.archive_root, &audio_relative)?;
        ensure_archive_directory(&self.archive_root, &day)?;
        #[cfg(test)]
        live_archive_phase("archive_paths_ready");
        let prior = prior_entry(&manifest, envelope.recording_id).cloned();
        validate_destination_owner(&manifest, &key, envelope.recording_id)?;
        let note = build_note(envelope, transcript, summary, &title)?;
        #[cfg(test)]
        live_archive_phase("note_built");
        atomic_write(&self.archive_root.join(&note_relative), note.as_bytes())?;
        atomic_copy_verified(
            &source,
            &self.archive_root.join(&audio_relative),
            &artifact.sha256,
            artifact.size_bytes,
        )?;
        #[cfg(test)]
        live_archive_phase("audio_copied");

        let mut entry = Map::new();
        entry.insert(
            "original".to_owned(),
            Value::String(
                envelope
                    .imported_name
                    .clone()
                    .unwrap_or_else(|| format!("{}.{}", envelope.recording_id, extension)),
            ),
        );
        entry.insert("title".to_owned(), Value::String(display_title_value));
        entry.insert("category".to_owned(), Value::String(category));
        entry.insert("note".to_owned(), Value::String(note_relative.clone()));
        entry.insert("audio".to_owned(), Value::String(audio_relative.clone()));
        if include_remote_fields {
            entry.insert("r2_key".to_owned(), Value::String(r2_key.clone()));
            entry.insert("r2_generation".to_owned(), Value::from(r2_generation));
        }
        entry.insert(
            "captured_at".to_owned(),
            Value::String(envelope.captured_at.to_rfc3339()),
        );
        entry.insert(
            "recording_id".to_owned(),
            Value::String(envelope.recording_id.to_string()),
        );
        entry.insert("publish_generation".to_owned(), Value::from(generation));
        entry.insert(
            "audio_sha256".to_owned(),
            Value::String(artifact.sha256.clone()),
        );
        entry.insert(
            "audio_size_bytes".to_owned(),
            Value::from(artifact.size_bytes),
        );
        entry.insert(
            "source".to_owned(),
            Value::String(
                envelope
                    .source
                    .label
                    .clone()
                    .unwrap_or_else(|| source_kind_text(envelope)),
            ),
        );
        entry.insert(
            "source_kind".to_owned(),
            serde_json::to_value(envelope.source.kind).map_err(|_| ArchiveError::verification())?,
        );
        if let Some(Value::Object(prior)) = &prior {
            for field in ["speakers", "speakers_applied", "attachments"] {
                if let Some(value) = prior.get(field) {
                    entry.insert(field.to_owned(), value.clone());
                }
            }
        }
        manifest.insert(key.clone(), Value::Object(entry));
        let manifest_bytes = serialize_manifest(&manifest)?;
        atomic_write(&self.archive_root.join("manifest.json"), &manifest_bytes)?;
        #[cfg(test)]
        live_archive_phase("manifest_written");
        refresh_all_daily_rollups(&self.archive_root, &manifest)?;
        rebuild_topic_views(&self.archive_root, &manifest)?;
        rebuild_viewer(&self.archive_root, &manifest)?;
        #[cfg(test)]
        live_archive_phase("derivatives_rebuilt");

        if let Some(Value::Object(prior)) = &remote_prior {
            remove_stale_owned_file(
                &self.archive_root,
                prior.get("note"),
                &note_relative,
                &manifest,
                "note",
            )?;
            remove_stale_owned_file(
                &self.archive_root,
                prior.get("audio"),
                &audio_relative,
                &manifest,
                "audio",
            )?;
        }
        let prior_note_relative = remote_prior
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|entry| entry.get("note"))
            .and_then(Value::as_str)
            .filter(|path| *path != note_relative)
            .filter(|path| !manifest_references_path(&manifest, "note", path))
            .map(str::to_owned);
        Ok(PreparedArchive {
            note_relative,
            prior_note_relative,
            audio_relative,
            r2_key,
            r2_generation,
            reuse_remote_r2,
            replacement_r2_key,
            note,
            manifest: manifest_bytes,
        })
    }

    fn checkpoint_r2_collision(
        &self,
        generation: u64,
        recording_id: Uuid,
        occupied_r2_key: &str,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<(), ArchiveError> {
        let mut manifest = load_manifest(&self.archive_root)?;
        let entry = manifest
            .values_mut()
            .find(|entry| {
                entry.get("recording_id").and_then(Value::as_str)
                    == Some(recording_id.to_string().as_str())
                    && entry.get("publish_generation").and_then(Value::as_u64) == Some(generation)
            })
            .and_then(Value::as_object_mut)
            .ok_or_else(ArchiveError::verification)?;
        if entry.get("r2_key").and_then(Value::as_str) != Some(occupied_r2_key) {
            return Err(ArchiveError::conflict());
        }
        let occupied_path = Path::new(occupied_r2_key);
        let parent = occupied_path
            .parent()
            .ok_or_else(ArchiveError::verification)?;
        let stem = occupied_path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(ArchiveError::verification)?;
        let extension = occupied_path
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(ArchiveError::verification)?;
        let identity_suffix = format!("-g{generation}-{recording_id}");
        let title_stem = stem
            .strip_suffix(&identity_suffix)
            .ok_or_else(ArchiveError::verification)?;
        let collision = artifact
            .sha256
            .get(..12)
            .ok_or_else(ArchiveError::verification)?;
        let fallback = parent
            .join(format!(
                "{title_stem}-c{collision}{identity_suffix}.{extension}"
            ))
            .to_string_lossy()
            .into_owned();
        if fallback == occupied_r2_key || fallback.len() > 1_024 {
            return Err(ArchiveError::conflict());
        }
        entry.insert("r2_key".to_owned(), Value::String(fallback));
        atomic_write(
            &self.archive_root.join("manifest.json"),
            &serialize_manifest(&manifest)?,
        )?;
        Ok(())
    }

    fn r2_attempt_path(&self, recording_id: Uuid, generation: u64) -> PathBuf {
        self.r2_attempt_root
            .join(format!("{recording_id}-g{generation}.json"))
    }

    fn persist_r2_attempt(
        &self,
        recording_id: Uuid,
        generation: u64,
        key: &str,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<(), ArchiveError> {
        if !r2::is_generation_owned_key(key, &recording_id.to_string(), generation) {
            return Err(ArchiveError::verification());
        }
        if self
            .load_r2_attempt(recording_id, generation)?
            .is_some_and(|existing| existing.key != key)
        {
            return Err(ArchiveError::conflict());
        }
        let record = R2AttemptRecord {
            schema_version: R2_ATTEMPT_SCHEMA,
            recording_id: recording_id.to_string(),
            generation,
            key: key.to_owned(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
            github_repo: self.github.repo.clone(),
            r2_destination: self.r2_destination.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&record).map_err(|_| ArchiveError::verification())?;
        if bytes.len() as u64 > MAX_R2_ATTEMPT_BYTES {
            return Err(ArchiveError::verification());
        }
        atomic_write(&self.r2_attempt_path(recording_id, generation), &bytes)
    }

    fn load_r2_attempt(
        &self,
        recording_id: Uuid,
        generation: u64,
    ) -> Result<Option<R2AttemptRecord>, ArchiveError> {
        let path = self.r2_attempt_path(recording_id, generation);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ArchiveError::verification()),
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_R2_ATTEMPT_BYTES
        {
            return Err(ArchiveError::verification());
        }
        let record: R2AttemptRecord =
            serde_json::from_reader(File::open(path).map_err(|_| ArchiveError::verification())?)
                .map_err(|_| ArchiveError::verification())?;
        if record.schema_version != R2_ATTEMPT_SCHEMA
            || record.recording_id != recording_id.to_string()
            || record.generation != generation
            || record.github_repo != self.github.repo
            || record.r2_destination != self.r2_destination
            || !r2::is_generation_owned_key(&record.key, &record.recording_id, generation)
            || record.sha256.len() != 64
            || record.size_bytes == 0
        {
            return Err(ArchiveError::conflict());
        }
        Ok(Some(record))
    }

    fn clear_r2_attempt(&self, recording_id: Uuid, generation: u64) -> Result<(), ArchiveError> {
        let path = self.r2_attempt_path(recording_id, generation);
        match fs::remove_file(path) {
            Ok(()) => sync_directory(&self.r2_attempt_root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(ArchiveError::verification()),
        }
    }

    async fn begin_r2_attempt(
        &self,
        recording_id: Uuid,
        generation: u64,
        key: &str,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<(), ArchiveError> {
        if let Some(existing) = self.load_r2_attempt(recording_id, generation)? {
            if existing.sha256 != artifact.sha256 || existing.size_bytes != artifact.size_bytes {
                return Err(ArchiveError::conflict());
            }
            if existing.key != key {
                match self
                    .r2
                    .delete_owned(
                        existing.key,
                        existing.recording_id,
                        existing.generation,
                        existing.sha256,
                        existing.size_bytes,
                    )
                    .await?
                {
                    r2::R2DeleteOutcome::Deleted | r2::R2DeleteOutcome::AlreadyMissing => {
                        self.clear_r2_attempt(recording_id, generation)?;
                    }
                }
            }
        }
        self.persist_r2_attempt(recording_id, generation, key, artifact)
    }

    async fn finish_reconciled_r2_attempt(
        &self,
        recording_id: Uuid,
        generation: u64,
        remote: &RemoteArchive,
    ) -> Result<(), ArchiveError> {
        let Some(attempt) = self.load_r2_attempt(recording_id, generation)? else {
            return Ok(());
        };
        let recording_id_text = recording_id.to_string();
        let canonical = remote
            .manifest
            .values()
            .find(|entry| {
                entry.get("recording_id").and_then(Value::as_str)
                    == Some(recording_id_text.as_str())
                    && entry.get("publish_generation").and_then(Value::as_u64) == Some(generation)
            })
            .and_then(|entry| entry.get("r2_key").or_else(|| entry.get("audio")))
            .and_then(Value::as_str)
            .ok_or_else(ArchiveError::verification)?;
        if attempt.key != canonical {
            if manifest_references_path(&remote.manifest, "audio", &attempt.key) {
                return Err(ArchiveError::conflict());
            }
            match self
                .r2
                .delete_owned(
                    attempt.key,
                    attempt.recording_id,
                    attempt.generation,
                    attempt.sha256,
                    attempt.size_bytes,
                )
                .await?
            {
                r2::R2DeleteOutcome::Deleted | r2::R2DeleteOutcome::AlreadyMissing => {}
            }
        }
        self.clear_r2_attempt(recording_id, generation)
    }

    #[cfg(test)]
    async fn publish_native_edit(
        &self,
        edit: NativeArchiveEdit,
    ) -> Result<PublicationProof, ArchiveError> {
        let _operation_guard = self.operation_lock.lock().await;
        let _process_guard = ArchiveFileLockGuard::acquire(&self.process_lock)?;
        self.publish_native_edit_inner(edit).await
    }

    async fn publish_native_edit_inner(
        &self,
        edit: NativeArchiveEdit,
    ) -> Result<PublicationProof, ArchiveError> {
        for _ in 0..MAX_GITHUB_RETRIES {
            let force_recording_ids = self.native_edit_recording_ids(&edit)?;
            let force_legacy_keys = self.native_edit_legacy_keys(&edit);
            let head = self
                .fetch_github_head(&force_recording_ids, &force_legacy_keys)
                .await?;
            let prepared = self.prepare_native_edit(&edit, &head.remote)?;
            if self.native_edit_matches_head(&prepared, &head).await? {
                self.finish_native_edit_r2(&edit).await?;
                return Ok(native_edit_proof(&prepared, &head.head_sha));
            }
            for (path, bytes) in &prepared.upserts {
                if path == "manifest.json" || path == "index.html" {
                    continue;
                }
                safe_archive_path(&self.archive_root, path)?;
                if bytes.len() > MAX_NOTE_BYTES {
                    return Err(ArchiveError::verification());
                }
            }
            let mut tree = Vec::new();
            for (path, bytes) in &prepared.upserts {
                let blob = self.create_blob(bytes).await?;
                tree.push(json!({"path": path, "mode": "100644", "type": "blob", "sha": blob}));
            }
            for path in &prepared.deletes {
                // GitHub rejects sha:null for a path that is already absent
                // from the base tree. Canonical audio lives only in R2, so its
                // friendly local path is commonly absent from Git by design.
                if head.tree_blobs.contains_key(path) {
                    tree.push(
                        json!({"path": path, "mode": "100644", "type": "blob", "sha": Value::Null}),
                    );
                }
            }
            let tree_response = self
                .github_json(
                    ArchiveMethod::Post,
                    "git/trees",
                    Some(json!({"base_tree": head.tree_sha, "tree": tree})),
                )
                .await?;
            let tree_sha = json_pointer_text(&tree_response, "/sha")?;
            let commit_response = self
                .github_json(
                    ArchiveMethod::Post,
                    "git/commits",
                    Some(json!({
                        "message": prepared.marker,
                        "tree": tree_sha,
                        "parents": [head.head_sha]
                    })),
                )
                .await?;
            let commit_sha = json_pointer_text(&commit_response, "/sha")?;
            let update = self
                .github_request(
                    ArchiveMethod::Patch,
                    "git/refs/heads/main",
                    Some(json!({"sha": commit_sha, "force": false})),
                )
                .await?;
            if update.status == 200 {
                self.finish_native_edit_r2(&edit).await?;
                return Ok(native_edit_proof(&prepared, &commit_sha));
            }
            if !matches!(update.status, 409 | 422) {
                return Err(classify_github_status(update.status));
            }
        }
        Err(ArchiveError::conflict())
    }

    async fn finish_native_edit_r2(&self, edit: &NativeArchiveEdit) -> Result<(), ArchiveError> {
        let NativeArchiveEdit::Delete {
            r2_delete: Some(intent),
            ..
        } = edit
        else {
            return Ok(());
        };
        match self
            .r2
            .delete_owned(
                intent.key.clone(),
                intent.recording_id.clone(),
                intent.generation,
                intent.sha256.clone(),
                intent.size_bytes,
            )
            .await?
        {
            r2::R2DeleteOutcome::Deleted | r2::R2DeleteOutcome::AlreadyMissing => Ok(()),
        }
    }

    fn native_edit_recording_ids(
        &self,
        edit: &NativeArchiveEdit,
    ) -> Result<std::collections::HashSet<String>, ArchiveError> {
        let mut ids = std::collections::HashSet::new();
        match edit {
            NativeArchiveEdit::UserFields { deltas } => {
                if deltas.is_empty() || deltas.len() > 100 {
                    return Err(ArchiveError::verification());
                }
                for delta in deltas {
                    if let Some(recording_id) = &delta.recording_id {
                        Uuid::parse_str(recording_id).map_err(|_| ArchiveError::verification())?;
                        ids.insert(recording_id.clone());
                    }
                }
            }
            NativeArchiveEdit::Delete { recording_id, .. } => {
                if let Some(recording_id) = recording_id {
                    Uuid::parse_str(recording_id).map_err(|_| ArchiveError::verification())?;
                    ids.insert(recording_id.clone());
                }
            }
            NativeArchiveEdit::SpeakerColors { .. } => {}
        }
        Ok(ids)
    }

    fn native_edit_legacy_keys(
        &self,
        edit: &NativeArchiveEdit,
    ) -> std::collections::HashSet<String> {
        match edit {
            NativeArchiveEdit::UserFields { deltas } => deltas
                .iter()
                .filter(|delta| delta.recording_id.is_none())
                .map(|delta| delta.key.clone())
                .collect(),
            NativeArchiveEdit::Delete {
                key,
                recording_id: None,
                ..
            } => std::iter::once(key.clone()).collect(),
            _ => std::collections::HashSet::new(),
        }
    }

    fn prepare_native_edit(
        &self,
        edit: &NativeArchiveEdit,
        remote: &RemoteArchive,
    ) -> Result<PreparedNativeEdit, ArchiveError> {
        let is_delete = matches!(edit, NativeArchiveEdit::Delete { .. });
        let deleted_paths: std::collections::HashSet<&str> = match edit {
            NativeArchiveEdit::Delete { deleted_paths, .. } => {
                deleted_paths.iter().map(String::as_str).collect()
            }
            _ => std::collections::HashSet::new(),
        };
        for (relative, contents) in &remote.notes {
            if !deleted_paths.contains(relative.as_str()) {
                atomic_write(&safe_archive_path(&self.archive_root, relative)?, contents)?;
            }
        }
        let mut manifest = remote.manifest.clone();
        let mut deleted_recording_ids = remote.deleted_recording_ids.clone();
        let mut upsert_paths = std::collections::BTreeSet::new();
        let mut inline_upserts = std::collections::BTreeMap::new();
        let mut deletes = std::collections::BTreeSet::new();
        match edit {
            NativeArchiveEdit::UserFields { deltas } => {
                let mut remote_keys = Vec::new();
                for delta in deltas {
                    if delta.speaker_slots.len() > 64 || delta.add_attachments.len() > 100 {
                        return Err(ArchiveError::verification());
                    }
                    let remote_key = if let Some(recording_id) = &delta.recording_id {
                        manifest
                            .iter()
                            .find_map(|(remote_key, entry)| {
                                (entry.get("recording_id").and_then(Value::as_str)
                                    == Some(recording_id.as_str()))
                                .then(|| remote_key.clone())
                            })
                            .ok_or_else(ArchiveError::conflict)?
                    } else {
                        let remote_entry = manifest
                            .get(&delta.key)
                            .filter(|entry| entry.get("recording_id").is_none())
                            .ok_or_else(ArchiveError::conflict)?;
                        if delta.base_entry_sha256.as_deref()
                            != Some(archive_entry_sha256(remote_entry)?.as_str())
                            && !entry_satisfies_delta(remote_entry, delta)
                        {
                            return Err(ArchiveError::conflict());
                        }
                        delta.key.clone()
                    };
                    let remote_entry = manifest
                        .get_mut(&remote_key)
                        .and_then(Value::as_object_mut)
                        .ok_or_else(ArchiveError::verification)?;
                    let mut speakers = speaker_map(remote_entry.get("speakers"))?;
                    for change in &delta.speaker_slots {
                        if change.slot.is_empty()
                            || change.slot.len() > 32
                            || !change
                                .slot
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                            || change
                                .desired
                                .as_deref()
                                .is_some_and(|name| name.is_empty() || name.len() > 60)
                        {
                            return Err(ArchiveError::verification());
                        }
                        let current = speakers.get(&change.slot).cloned();
                        if current == change.desired {
                            continue;
                        }
                        if current != change.expected {
                            return Err(ArchiveError::conflict());
                        }
                        if let Some(desired) = &change.desired {
                            speakers.insert(change.slot.clone(), desired.clone());
                        } else {
                            speakers.remove(&change.slot);
                        }
                    }
                    if speakers.is_empty() {
                        remote_entry.remove("speakers");
                    } else {
                        remote_entry.insert(
                            "speakers".to_owned(),
                            Value::Object(
                                speakers
                                    .into_iter()
                                    .map(|(slot, name)| (slot, Value::String(name)))
                                    .collect(),
                            ),
                        );
                    }
                    let attachments = remote_entry
                        .entry("attachments")
                        .or_insert_with(|| Value::Array(Vec::new()))
                        .as_array_mut()
                        .ok_or_else(ArchiveError::verification)?;
                    for path in &delta.add_attachments {
                        let absolute = safe_archive_path(&self.archive_root, path)?;
                        let bytes =
                            fs::read(&absolute).map_err(|_| ArchiveError::verification())?;
                        require_attachment_content_identity(path, &bytes)?;
                        if !attachments.iter().any(|value| value.as_str() == Some(path)) {
                            attachments.push(Value::String(path.clone()));
                        }
                        upsert_paths.insert(path.clone());
                    }
                    if attachments.is_empty() {
                        remote_entry.remove("attachments");
                    }
                    // `speakers_applied` describes the remote note's current
                    // labels. Keep it until `apply_speaker_labels` rewrites the
                    // freshly fetched remote note to the desired slot deltas.
                    if let Some(note) = remote_entry.get("note").and_then(Value::as_str) {
                        upsert_paths.insert(note.to_owned());
                    }
                    remote_keys.push(remote_key);
                }
                atomic_write(
                    &self.archive_root.join("manifest.json"),
                    &serialize_manifest(&manifest)?,
                )?;
                apply_speaker_labels(&self.archive_root, &remote_keys)?;
                manifest = load_manifest(&self.archive_root)?;
                refresh_all_daily_rollups(&self.archive_root, &manifest)?;
            }
            NativeArchiveEdit::SpeakerColors {
                name,
                expected,
                desired,
            } => {
                if name.is_empty()
                    || name.len() > 60
                    || name.chars().any(char::is_control)
                    || desired.len() != 7
                    || !desired.starts_with('#')
                    || !desired[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(ArchiveError::verification());
                }
                let path = self.archive_root.join("speakers.json");
                let mut colors: Map<String, Value> = if path.exists() {
                    serde_json::from_slice(
                        &fs::read(&path).map_err(|_| ArchiveError::verification())?,
                    )
                    .map_err(|_| ArchiveError::verification())?
                } else {
                    Map::new()
                };
                let current = colors.get(name).and_then(Value::as_str).map(str::to_owned);
                if current != *expected && current.as_deref() != Some(desired) {
                    return Err(ArchiveError::conflict());
                }
                colors.insert(name.clone(), Value::String(desired.clone()));
                atomic_write(
                    &path,
                    &serde_json::to_vec_pretty(&colors)
                        .map_err(|_| ArchiveError::verification())?,
                )?;
                atomic_write(
                    &self.archive_root.join("manifest.json"),
                    &serialize_manifest(&manifest)?,
                )?;
                upsert_paths.insert("speakers.json".to_owned());
            }
            NativeArchiveEdit::Delete {
                key,
                recording_id,
                base_entry_sha256,
                deleted_paths,
                r2_delete: _,
            } => {
                if deleted_paths.len() > 2_048 {
                    return Err(ArchiveError::verification());
                }
                let target = if let Some(recording_id) = recording_id {
                    // Identity-bearing deletes are CAS fenced to the exact
                    // version the user saw. The tombstone then prevents a
                    // stale publisher from resurrecting that recording id.
                    let target = manifest.iter().find_map(|(remote_key, entry)| {
                        (entry.get("recording_id").and_then(Value::as_str)
                            == Some(recording_id.as_str()))
                        .then(|| remote_key.clone())
                    });
                    if let Some(target) = &target {
                        let entry = manifest
                            .get(target)
                            .ok_or_else(ArchiveError::verification)?;
                        if base_entry_sha256.as_deref()
                            != Some(archive_entry_sha256(entry)?.as_str())
                        {
                            return Err(ArchiveError::conflict());
                        }
                    }
                    deleted_recording_ids.insert(recording_id.clone());
                    target
                } else if let Some(entry) = manifest.get(key) {
                    if entry.get("recording_id").is_some()
                        || base_entry_sha256.as_deref()
                            != Some(archive_entry_sha256(entry)?.as_str())
                    {
                        return Err(ArchiveError::conflict());
                    }
                    Some(key.clone())
                } else {
                    None
                };
                if let Some(target) = target {
                    manifest.remove(&target);
                }
                if recording_id.is_some() {
                    let tombstones = serialize_deleted_recordings(&deleted_recording_ids)?;
                    inline_upserts.insert(DELETED_RECORDINGS_PATH.to_owned(), tombstones);
                }
                for path in deleted_paths {
                    safe_archive_path(&self.archive_root, path)?;
                    if !manifest_references_any_path(&manifest, path) {
                        deletes.insert(path.clone());
                    }
                }
            }
        }
        let manifest_bytes = serialize_manifest(&manifest)?;
        if is_delete {
            render_viewer(&self.archive_root, &manifest)?;
        } else {
            atomic_write(&self.archive_root.join("manifest.json"), &manifest_bytes)?;
            rebuild_viewer(&self.archive_root, &manifest)?;
        }
        let mut upserts = vec![
            ("manifest.json".to_owned(), manifest_bytes.clone()),
            (".gitignore".to_owned(), ARCHIVE_GITIGNORE.to_vec()),
        ];
        upserts.extend(inline_upserts);
        for path in upsert_paths {
            let absolute = safe_archive_path(&self.archive_root, &path)?;
            let metadata =
                fs::symlink_metadata(&absolute).map_err(|_| ArchiveError::verification())?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_NOTE_BYTES as u64
            {
                return Err(ArchiveError::verification());
            }
            upserts.push((
                path,
                fs::read(absolute).map_err(|_| ArchiveError::verification())?,
            ));
        }
        let deletes: Vec<_> = deletes.into_iter().collect();
        let mut identity = Sha256::new();
        for (path, bytes) in &upserts {
            identity.update(path.as_bytes());
            identity.update((bytes.len() as u64).to_be_bytes());
            identity.update(bytes);
        }
        for path in &deletes {
            identity.update(b"delete\0");
            identity.update(path.as_bytes());
        }
        let marker = format!("echowall-edit:{}", hex::encode(identity.finalize()));
        Ok(PreparedNativeEdit {
            upserts,
            deletes,
            manifest: manifest_bytes,
            marker,
        })
    }

    async fn native_edit_matches_head(
        &self,
        prepared: &PreparedNativeEdit,
        head: &GitHubHead,
    ) -> Result<bool, ArchiveError> {
        if prepared
            .deletes
            .iter()
            .any(|path| head.tree_blobs.contains_key(path))
        {
            return Ok(false);
        }
        for (path, expected) in &prepared.upserts {
            let Some(blob_sha) = head.tree_blobs.get(path) else {
                return Ok(false);
            };
            let blob = self
                .github_json(ArchiveMethod::Get, &format!("git/blobs/{blob_sha}"), None)
                .await?;
            if blob.get("encoding").and_then(Value::as_str) != Some("base64") {
                return Err(ArchiveError::verification());
            }
            let bytes = decode_base64(
                blob.get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(ArchiveError::verification)?,
            )?;
            if &bytes != expected {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn fetch_github_head(
        &self,
        force_recording_ids: &std::collections::HashSet<String>,
        force_legacy_keys: &std::collections::HashSet<String>,
    ) -> Result<GitHubHead, ArchiveError> {
        let head = self
            .github_json(ArchiveMethod::Get, "git/ref/heads/main", None)
            .await?;
        let head_sha = json_pointer_text(&head, "/object/sha")?;
        let commit = self
            .github_json(ArchiveMethod::Get, &format!("git/commits/{head_sha}"), None)
            .await?;
        let tree_sha = json_pointer_text(&commit, "/tree/sha")?;
        let message = commit
            .get("message")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 2048)
            .unwrap_or_default()
            .to_owned();
        let tree = self
            .github_json(
                ArchiveMethod::Get,
                &format!("git/trees/{tree_sha}?recursive=1"),
                None,
            )
            .await?;
        let tree_entries = tree
            .get("tree")
            .and_then(Value::as_array)
            .ok_or_else(ArchiveError::verification)?;
        let manifest_sha = tree_entries
            .iter()
            .find(|entry| {
                entry.get("path").and_then(Value::as_str) == Some("manifest.json")
                    && entry.get("type").and_then(Value::as_str) == Some("blob")
                    && entry.get("mode").and_then(Value::as_str) == Some("100644")
            })
            .and_then(|entry| entry.get("sha"))
            .and_then(Value::as_str);
        let manifest: BTreeMap<String, Value> = if let Some(manifest_sha) = manifest_sha {
            let blob = self
                .github_json(
                    ArchiveMethod::Get,
                    &format!("git/blobs/{manifest_sha}"),
                    None,
                )
                .await?;
            if blob.get("encoding").and_then(Value::as_str) != Some("base64") {
                return Err(ArchiveError::verification());
            }
            let bytes = decode_base64(
                blob.get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(ArchiveError::verification)?,
            )?;
            if bytes.len() as u64 > MAX_LOCAL_JSON_BYTES {
                return Err(ArchiveError::verification());
            }
            serde_json::from_slice(&bytes).map_err(|_| ArchiveError::verification())?
        } else {
            BTreeMap::new()
        };
        let mut tree_blobs = HashMap::new();
        for entry in tree_entries {
            if entry.get("type").and_then(Value::as_str) != Some("blob")
                || entry.get("mode").and_then(Value::as_str) != Some("100644")
            {
                continue;
            }
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(ArchiveError::verification)?;
            let sha = entry
                .get("sha")
                .and_then(Value::as_str)
                .ok_or_else(ArchiveError::verification)?;
            if path.len() > 1_024
                || sha.is_empty()
                || tree_blobs.insert(path.to_owned(), sha.to_owned()).is_some()
            {
                return Err(ArchiveError::verification());
            }
        }
        let deleted_recording_ids = if let Some(blob_sha) = tree_blobs.get(DELETED_RECORDINGS_PATH)
        {
            let blob = self
                .github_json(ArchiveMethod::Get, &format!("git/blobs/{blob_sha}"), None)
                .await?;
            if blob.get("encoding").and_then(Value::as_str) != Some("base64") {
                return Err(ArchiveError::verification());
            }
            let bytes = decode_base64(
                blob.get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(ArchiveError::verification)?,
            )?;
            parse_deleted_recordings(&bytes)?
        } else {
            std::collections::BTreeSet::new()
        };
        let local_manifest = load_manifest(&self.archive_root)?;
        let mut notes = Vec::new();
        for (archive_key, entry) in &manifest {
            let slot = parse_archive_key(archive_key)?;
            if slot.format("%Y-%m-%d %H%M%S").to_string() != *archive_key {
                return Err(ArchiveError::verification());
            }
            if let Some(recording_id) = entry.get("recording_id").and_then(Value::as_str) {
                Uuid::parse_str(recording_id).map_err(|_| ArchiveError::verification())?;
            }
            validate_manifest_entry_paths(&self.archive_root, slot, entry)?;
            let Some(relative) = entry.get("note").and_then(Value::as_str) else {
                continue;
            };
            validate_owned_archive_path(
                &self.archive_root,
                slot,
                relative,
                OwnedArchivePathKind::Note,
            )?;
            if notes.len() >= 1_024 {
                return Err(ArchiveError::verification());
            }
            let local = safe_archive_path(&self.archive_root, relative)?;
            let remote_owner = entry.get("recording_id").and_then(Value::as_str);
            let local_owner = local_manifest.values().find_map(|local_entry| {
                (local_entry.get("note").and_then(Value::as_str) == Some(relative))
                    .then(|| local_entry.get("recording_id").and_then(Value::as_str))
                    .flatten()
            });
            let force = remote_owner.is_some_and(|owner| force_recording_ids.contains(owner))
                || (remote_owner.is_none() && force_legacy_keys.contains(archive_key.as_str()));
            let blob_sha = tree_blobs
                .get(relative)
                .ok_or_else(ArchiveError::verification)?;
            if local_owner == remote_owner
                && !force
                && local_matches_git_blob(&local, MAX_NOTE_BYTES as u64, blob_sha)?
            {
                continue;
            }
            let blob = self
                .github_json(ArchiveMethod::Get, &format!("git/blobs/{blob_sha}"), None)
                .await?;
            let bytes = decode_base64(
                blob.get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(ArchiveError::verification)?,
            )?;
            if bytes.len() > MAX_NOTE_BYTES || std::str::from_utf8(&bytes).is_err() {
                return Err(ArchiveError::verification());
            }
            notes.push((relative.to_owned(), bytes));
        }
        let mut fetched_paths: std::collections::HashSet<String> =
            notes.iter().map(|(path, _)| path.clone()).collect();
        for (archive_key, entry) in &manifest {
            let slot = parse_archive_key(archive_key)?;
            let Some(attachments) = entry.get("attachments").and_then(Value::as_array) else {
                continue;
            };
            if attachments.len() > 1_000 {
                return Err(ArchiveError::verification());
            }
            for relative in attachments.iter().map(Value::as_str) {
                let relative = relative.ok_or_else(ArchiveError::verification)?;
                validate_owned_archive_path(
                    &self.archive_root,
                    slot,
                    relative,
                    OwnedArchivePathKind::Attachment,
                )?;
                if fetched_paths.contains(relative) {
                    continue;
                }
                if notes.len() >= 2_048 {
                    return Err(ArchiveError::verification());
                }
                let local = safe_archive_path(&self.archive_root, relative)?;
                let remote_owner = entry.get("recording_id").and_then(Value::as_str);
                let force = remote_owner.is_some_and(|owner| force_recording_ids.contains(owner))
                    || (remote_owner.is_none() && force_legacy_keys.contains(archive_key.as_str()));
                let blob_sha = tree_blobs
                    .get(relative)
                    .ok_or_else(ArchiveError::verification)?;
                if !force && local_matches_git_blob(&local, MAX_ATTACHMENT_BYTES, blob_sha)? {
                    continue;
                }
                let blob = self
                    .github_json(ArchiveMethod::Get, &format!("git/blobs/{blob_sha}"), None)
                    .await?;
                let bytes = decode_base64(
                    blob.get("content")
                        .and_then(Value::as_str)
                        .ok_or_else(ArchiveError::verification)?,
                )?;
                if bytes.len() as u64 > MAX_ATTACHMENT_BYTES || std::str::from_utf8(&bytes).is_err()
                {
                    return Err(ArchiveError::verification());
                }
                validate_attachment_content_identity(relative, &bytes)?;
                fetched_paths.insert(relative.to_owned());
                notes.push((relative.to_owned(), bytes));
            }
        }
        if let Some(blob_sha) = tree_blobs.get("speakers.json") {
            let blob = self
                .github_json(ArchiveMethod::Get, &format!("git/blobs/{blob_sha}"), None)
                .await?;
            let bytes = decode_base64(
                blob.get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(ArchiveError::verification)?,
            )?;
            if bytes.len() as u64 > MAX_LOCAL_JSON_BYTES
                || serde_json::from_slice::<Map<String, Value>>(&bytes).is_err()
            {
                return Err(ArchiveError::verification());
            }
            notes.retain(|(path, _)| path != "speakers.json");
            notes.push(("speakers.json".to_owned(), bytes));
        }
        Ok(GitHubHead {
            head_sha,
            tree_sha,
            message,
            tree_blobs,
            remote: RemoteArchive {
                manifest,
                notes,
                deleted_recording_ids,
            },
        })
    }

    async fn reconcile_remote_publication(
        &self,
        generation: u64,
        envelope: &RecordingEnvelope,
        summary: &Value,
        artifact: &NormalizedArtifactCheckpoint,
        head: &GitHubHead,
    ) -> Result<Option<PublicationProof>, ArchiveError> {
        let recording_id = envelope.recording_id.to_string();
        if head.remote.deleted_recording_ids.contains(&recording_id) {
            return Err(ArchiveError::conflict());
        }
        let Some(entry) = head.remote.manifest.values().find(|entry| {
            entry.get("recording_id").and_then(Value::as_str) == Some(recording_id.as_str())
        }) else {
            return Ok(None);
        };
        let remote_generation = entry
            .get("publish_generation")
            .and_then(Value::as_u64)
            .ok_or_else(ArchiveError::verification)?;
        if remote_generation > generation {
            return Err(ArchiveError::conflict());
        }
        if remote_generation < generation {
            return Ok(None);
        }

        let expected_title = display_title(envelope, &archive_title(envelope, summary)?);
        let expected_captured_at = envelope.captured_at.to_rfc3339();
        let expected_source_kind =
            serde_json::to_value(envelope.source.kind).map_err(|_| ArchiveError::verification())?;
        if entry.get("title").and_then(Value::as_str) != Some(expected_title.as_str())
            || entry.get("captured_at").and_then(Value::as_str)
                != Some(expected_captured_at.as_str())
            || entry.get("source_kind") != Some(&expected_source_kind)
        {
            return Err(ArchiveError::verification());
        }
        let note_relative = entry
            .get("note")
            .and_then(Value::as_str)
            .ok_or_else(ArchiveError::verification)?;
        let note = head
            .remote
            .notes
            .iter()
            .find(|(relative, _)| relative == note_relative)
            .map(|(_, bytes)| bytes)
            .ok_or_else(ArchiveError::verification)?;
        let expected_heading = format!("# {expected_title}\n");
        if !std::str::from_utf8(note).is_ok_and(|note| note.starts_with(&expected_heading)) {
            return Err(ArchiveError::verification());
        }
        let audio = entry
            .get("r2_key")
            .or_else(|| entry.get("audio"))
            .and_then(Value::as_str)
            .ok_or_else(ArchiveError::verification)?
            .to_owned();
        let r2_generation = entry
            .get("r2_generation")
            .or_else(|| entry.get("publish_generation"))
            .and_then(Value::as_u64)
            .ok_or_else(ArchiveError::verification)?;
        if !r2::is_generation_owned_key(&audio, &recording_id, r2_generation) {
            return Err(ArchiveError::verification());
        }
        safe_archive_path(&self.archive_root, &audio)?;
        let r2_proof = self
            .r2
            .head_verified(
                audio.clone(),
                recording_id.clone(),
                artifact.sha256.clone(),
                artifact.size_bytes,
            )
            .await?;
        if r2_proof.key != audio
            || r2_proof.recording_id != recording_id
            || r2_proof.sha256 != artifact.sha256
            || r2_proof.size_bytes != artifact.size_bytes
            || r2_proof.etag.is_empty()
            || r2_proof.version_id.is_empty()
        {
            return Err(ArchiveError::verification());
        }
        let manifest = serialize_manifest(&head.remote.manifest)?;
        Ok(Some(PublicationProof {
            locator: format!("github:commit:{}", head.head_sha),
            version: head.head_sha.clone(),
            sha256: sha256_hex(&manifest),
            size_bytes: manifest.len() as u64,
        }))
    }

    async fn publish_github_at_head(
        &self,
        generation: u64,
        recording_id: Uuid,
        prepared: &PreparedArchive,
        head: &GitHubHead,
    ) -> Result<Option<PublicationProof>, ArchiveError> {
        let manifest_sha256 = sha256_hex(&prepared.manifest);
        let marker = format!("echowall:{recording_id}:{generation}:{manifest_sha256}");
        if head.message == marker {
            return Ok(Some(PublicationProof {
                locator: format!("github:commit:{}", head.head_sha),
                version: head.head_sha.clone(),
                sha256: manifest_sha256,
                size_bytes: prepared.manifest.len() as u64,
            }));
        }
        let note_blob = self.create_blob(prepared.note.as_bytes()).await?;
        let manifest_blob = self.create_blob(&prepared.manifest).await?;
        let gitignore_blob = self.create_blob(ARCHIVE_GITIGNORE).await?;
        let mut tree = vec![
            json!({"path": prepared.note_relative, "mode": "100644", "type": "blob", "sha": note_blob}),
            json!({"path": "manifest.json", "mode": "100644", "type": "blob", "sha": manifest_blob}),
            json!({"path": ".gitignore", "mode": "100644", "type": "blob", "sha": gitignore_blob}),
        ];
        if let Some(prior) = &prepared.prior_note_relative {
            tree.push(json!({"path": prior, "mode": "100644", "type": "blob", "sha": Value::Null}));
        }
        let tree_response = self
            .github_json(
                ArchiveMethod::Post,
                "git/trees",
                Some(json!({"base_tree": head.tree_sha, "tree": tree})),
            )
            .await?;
        let tree_sha = json_pointer_text(&tree_response, "/sha")?;
        let commit_response = self
            .github_json(
                ArchiveMethod::Post,
                "git/commits",
                Some(json!({"message": marker, "tree": tree_sha, "parents": [head.head_sha]})),
            )
            .await?;
        let new_sha = json_pointer_text(&commit_response, "/sha")?;
        let update = self
            .github_request(
                ArchiveMethod::Patch,
                "git/refs/heads/main",
                Some(json!({"sha": new_sha, "force": false})),
            )
            .await?;
        if update.status == 200 {
            Ok(Some(PublicationProof {
                locator: format!("github:commit:{new_sha}"),
                version: new_sha,
                sha256: manifest_sha256,
                size_bytes: prepared.manifest.len() as u64,
            }))
        } else if matches!(update.status, 409 | 422) {
            Ok(None)
        } else {
            Err(classify_github_status(update.status))
        }
    }

    async fn cleanup_tombstoned_audio(
        &self,
        generation: u64,
        recording_id: Uuid,
        attempted_audio: Option<&str>,
    ) -> Result<(), ArchiveError> {
        let persisted = self.load_r2_attempt(recording_id, generation)?;
        let recording_id_text = recording_id.to_string();
        let fallback = load_manifest(&self.archive_root)?
            .values()
            .find(|entry| {
                entry.get("recording_id").and_then(Value::as_str)
                    == Some(recording_id_text.as_str())
                    && entry.get("publish_generation").and_then(Value::as_u64) == Some(generation)
            })
            .and_then(|entry| {
                Some((
                    entry
                        .get("r2_key")
                        .or_else(|| entry.get("audio"))?
                        .as_str()?
                        .to_owned(),
                    entry.get("audio_sha256")?.as_str()?.to_owned(),
                    entry.get("audio_size_bytes")?.as_u64()?,
                ))
            });
        let persisted_key = persisted.as_ref().map(|record| record.key.as_str());
        if attempted_audio.is_some() && persisted_key.is_some() && attempted_audio != persisted_key
        {
            return Err(ArchiveError::verification());
        }
        let selected = persisted
            .map(|record| (record.key, record.sha256, record.size_bytes))
            .or(fallback);
        let Some((key, sha256, size_bytes)) = selected else {
            return Ok(());
        };
        if attempted_audio.is_some_and(|attempted| attempted != key) {
            return Err(ArchiveError::verification());
        }
        if !r2::is_generation_owned_key(&key, &recording_id_text, generation) {
            return Err(ArchiveError::verification());
        }
        match self
            .r2
            .delete_owned(key, recording_id_text, generation, sha256, size_bytes)
            .await?
        {
            r2::R2DeleteOutcome::Deleted | r2::R2DeleteOutcome::AlreadyMissing => {
                self.clear_r2_attempt(recording_id, generation)
            }
        }
    }

    async fn create_blob(&self, contents: &[u8]) -> Result<String, ArchiveError> {
        let response = self
            .github_json(
                ArchiveMethod::Post,
                "git/blobs",
                Some(json!({"content": base64(contents), "encoding": "base64"})),
            )
            .await?;
        json_pointer_text(&response, "/sha")
    }

    async fn github_json(
        &self,
        method: ArchiveMethod,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ArchiveError> {
        let response = self.github_request(method, path, body).await?;
        if !(200..300).contains(&response.status) {
            return Err(classify_github_status(response.status));
        }
        serde_json::from_slice(&response.body).map_err(|_| ArchiveError::verification())
    }

    async fn github_request(
        &self,
        method: ArchiveMethod,
        path: &str,
        body: Option<Value>,
    ) -> Result<ArchiveHttpResponse, ArchiveError> {
        let allowed_tree_query = path.starts_with("git/trees/") && path.ends_with("?recursive=1");
        if path.contains('#')
            || (path.contains('?') && !allowed_tree_query)
            || path.split('/').any(|part| part == "..")
        {
            return Err(ArchiveError::configuration());
        }
        let body = body
            .map(|value| serde_json::to_vec(&value).map_err(|_| ArchiveError::verification()))
            .transpose()?;
        self.http
            .execute(ArchiveHttpRequest {
                method,
                url: format!("https://api.github.com/repos/{}/{}", self.github.repo, path),
                headers: vec![
                    (
                        "accept".to_owned(),
                        "application/vnd.github+json".to_owned(),
                    ),
                    (
                        "authorization".to_owned(),
                        format!("Bearer {}", self.github.token),
                    ),
                    ("user-agent".to_owned(), "EchoWall".to_owned()),
                    ("content-type".to_owned(), "application/json".to_owned()),
                    ("x-github-api-version".to_owned(), "2022-11-28".to_owned()),
                ],
                body,
                response_limit: MAX_GITHUB_RESPONSE_BYTES,
            })
            .await
    }
}

impl<H: ArchiveHttpTransport, R: ArchiveR2> ArchiveAdapter<H, R> {
    async fn publish_local_only(
        &self,
        generation: u64,
        envelope: &RecordingEnvelope,
        transcript: &Value,
        summary: &Value,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<PublicationProof, EffectError> {
        let _operation_guard = self.operation_lock.lock().await;
        let _process_guard =
            ArchiveFileLockGuard::acquire(&self.process_lock).map_err(map_archive_error)?;
        let prepared = self
            .prepare_local(
                generation,
                envelope,
                transcript,
                summary,
                artifact,
                ArchivePreparation::LocalOnly,
            )
            .map_err(map_archive_error)?;
        Ok(PublicationProof {
            locator: format!("local:{}", prepared.note_relative),
            version: format!("local-g{generation}"),
            sha256: sha256_hex(&prepared.manifest),
            size_bytes: prepared.manifest.len() as u64,
        })
    }

    async fn verify_local_backup(
        &self,
        generation: u64,
        envelope: &RecordingEnvelope,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<CanonicalBackupCheckpoint, EffectError> {
        let _operation_guard = self.operation_lock.lock().await;
        let _process_guard =
            ArchiveFileLockGuard::acquire(&self.process_lock).map_err(map_archive_error)?;
        let manifest = load_manifest(&self.archive_root).map_err(map_archive_error)?;
        let (_, entry) = manifest
            .iter()
            .find(|(_, entry)| {
                entry.get("recording_id").and_then(Value::as_str)
                    == Some(envelope.recording_id.to_string().as_str())
                    && entry.get("publish_generation").and_then(Value::as_u64) == Some(generation)
            })
            .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
        if entry.get("r2_key").is_some() || entry.get("r2_generation").is_some() {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        let relative = entry
            .get("audio")
            .and_then(Value::as_str)
            .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
        let audio = safe_archive_path(&self.archive_root, relative).map_err(map_archive_error)?;
        let (sha256, size_bytes) = hash_file(&audio).map_err(map_archive_error)?;
        if sha256 != artifact.sha256 || size_bytes != artifact.size_bytes {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        Ok(CanonicalBackupCheckpoint {
            locator: format!("local:{relative}"),
            version_id: format!("local-g{generation}"),
            sha256,
            size_bytes,
            proof_json: json!({"local_hash_verified": true}),
        })
    }
}

#[async_trait]
impl<H: ArchiveHttpTransport, R: ArchiveR2> ArchiveEffects for ArchiveAdapter<H, R> {
    fn plan(
        &self,
        _: &RecordingEnvelope,
        backend: PublicationBackend,
    ) -> Result<Vec<PublicationTargetPlan>, EffectError> {
        Ok(vec![PublicationTargetPlan {
            id: match backend {
                PublicationBackend::RemoteArchive => TARGET_ID,
                PublicationBackend::LocalArchive => LOCAL_TARGET_ID,
            }
            .to_owned(),
            retry_mode: RetryMode::ReconcileBeforeRetry,
            required: true,
        }])
    }

    async fn publish(
        &self,
        target_id: &str,
        generation: u64,
        envelope: &RecordingEnvelope,
        transcript: &Value,
        summary: &Value,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<PublicationProof, EffectError> {
        if target_id == LOCAL_TARGET_ID {
            return self
                .publish_local_only(generation, envelope, transcript, summary, artifact)
                .await;
        }
        if target_id != TARGET_ID {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        let _operation_guard = self.operation_lock.lock().await;
        let _process_guard =
            ArchiveFileLockGuard::acquire(&self.process_lock).map_err(map_archive_error)?;
        let mut attempted_audio = None;
        for _ in 0..MAX_GITHUB_RETRIES {
            let force_recording_ids =
                std::collections::HashSet::from([envelope.recording_id.to_string()]);
            let head = self
                .fetch_github_head(&force_recording_ids, &std::collections::HashSet::new())
                .await
                .map_err(map_archive_error)?;
            if head
                .remote
                .deleted_recording_ids
                .contains(&envelope.recording_id.to_string())
            {
                self.cleanup_tombstoned_audio(
                    generation,
                    envelope.recording_id,
                    attempted_audio.as_deref(),
                )
                .await
                .map_err(map_archive_error)?;
                return Err(EffectError::new(EffectErrorKind::PublicationConflict));
            }
            if let Some(proof) = self
                .reconcile_remote_publication(generation, envelope, summary, artifact, &head)
                .await
                .map_err(map_archive_error)?
            {
                self.finish_reconciled_r2_attempt(envelope.recording_id, generation, &head.remote)
                    .await
                    .map_err(map_archive_error)?;
                return Ok(proof);
            }
            let prepared = self
                .prepare_local(
                    generation,
                    envelope,
                    transcript,
                    summary,
                    artifact,
                    ArchivePreparation::Remote(&head.remote),
                )
                .map_err(map_archive_error)?;
            let r2_proof = if prepared.reuse_remote_r2 {
                match self
                    .r2
                    .head_verified(
                        prepared.r2_key.clone(),
                        envelope.recording_id.to_string(),
                        artifact.sha256.clone(),
                        artifact.size_bytes,
                    )
                    .await
                {
                    Ok(proof) => proof,
                    Err(error) if error.kind == ArchiveErrorKind::Verification => {
                        let replacement = prepared
                            .replacement_r2_key
                            .as_deref()
                            .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
                        self.persist_r2_attempt(
                            envelope.recording_id,
                            generation,
                            replacement,
                            artifact,
                        )
                        .map_err(map_archive_error)?;
                        continue;
                    }
                    Err(error) => return Err(map_archive_error(error)),
                }
            } else {
                self.begin_r2_attempt(
                    envelope.recording_id,
                    generation,
                    &prepared.r2_key,
                    artifact,
                )
                .await
                .map_err(map_archive_error)?;
                match self
                    .r2
                    .put_verified(
                        prepared.r2_key.clone(),
                        envelope.recording_id.to_string(),
                        self.archive_root.join(&prepared.audio_relative),
                        artifact.sha256.clone(),
                        artifact.size_bytes,
                    )
                    .await
                {
                    Ok(proof) => proof,
                    Err(error) if error.kind == ArchiveErrorKind::Conflict => {
                        self.clear_r2_attempt(envelope.recording_id, generation)
                            .map_err(map_archive_error)?;
                        self.checkpoint_r2_collision(
                            generation,
                            envelope.recording_id,
                            &prepared.r2_key,
                            artifact,
                        )
                        .map_err(map_archive_error)?;
                        continue;
                    }
                    Err(error) => return Err(map_archive_error(error)),
                }
            };
            if r2_proof.key != prepared.r2_key
                || r2_proof.recording_id != envelope.recording_id.to_string()
                || r2_proof.sha256 != artifact.sha256
                || r2_proof.size_bytes != artifact.size_bytes
                || r2_proof.etag.is_empty()
                || r2_proof.version_id.is_empty()
                || !r2::is_generation_owned_key(
                    &r2_proof.key,
                    &r2_proof.recording_id,
                    prepared.r2_generation,
                )
            {
                return Err(EffectError::new(EffectErrorKind::Verification));
            }
            if !prepared.reuse_remote_r2 {
                attempted_audio = Some(prepared.r2_key.clone());
            }
            // Re-read the Git authority after the cross-service R2 effect.
            // A delete tombstone that won meanwhile must clean this exact
            // generation before the stale Git CAS can be attempted.
            let guarded_head = self
                .fetch_github_head(&force_recording_ids, &std::collections::HashSet::new())
                .await
                .map_err(map_archive_error)?;
            if guarded_head
                .remote
                .deleted_recording_ids
                .contains(&envelope.recording_id.to_string())
            {
                self.cleanup_tombstoned_audio(
                    generation,
                    envelope.recording_id,
                    attempted_audio.as_deref(),
                )
                .await
                .map_err(map_archive_error)?;
                return Err(EffectError::new(EffectErrorKind::PublicationConflict));
            }
            if guarded_head.head_sha != head.head_sha {
                continue;
            }
            if let Some(proof) = self
                .publish_github_at_head(generation, envelope.recording_id, &prepared, &head)
                .await
                .map_err(map_archive_error)?
            {
                if !prepared.reuse_remote_r2 {
                    self.clear_r2_attempt(envelope.recording_id, generation)
                        .map_err(map_archive_error)?;
                }
                return Ok(proof);
            }
        }
        Err(EffectError::new(EffectErrorKind::PublicationConflict))
    }

    async fn verify_backup(
        &self,
        backend: PublicationBackend,
        generation: u64,
        envelope: &RecordingEnvelope,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<CanonicalBackupCheckpoint, EffectError> {
        if backend == PublicationBackend::LocalArchive {
            return self
                .verify_local_backup(generation, envelope, artifact)
                .await;
        }
        let _operation_guard = self.operation_lock.lock().await;
        let _process_guard =
            ArchiveFileLockGuard::acquire(&self.process_lock).map_err(map_archive_error)?;
        let manifest = load_manifest(&self.archive_root).map_err(map_archive_error)?;
        let (_, entry) = manifest
            .iter()
            .find(|(_, entry)| {
                entry.get("recording_id").and_then(Value::as_str)
                    == Some(envelope.recording_id.to_string().as_str())
                    && entry.get("publish_generation").and_then(Value::as_u64) == Some(generation)
            })
            .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
        let audio = entry
            .get("r2_key")
            .or_else(|| entry.get("audio"))
            .and_then(Value::as_str)
            .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?
            .to_owned();
        let r2_generation = entry
            .get("r2_generation")
            .or_else(|| entry.get("publish_generation"))
            .and_then(Value::as_u64)
            .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
        if !r2::is_generation_owned_key(&audio, &envelope.recording_id.to_string(), r2_generation) {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        let proof = self
            .r2
            .head_verified(
                audio.clone(),
                envelope.recording_id.to_string(),
                artifact.sha256.clone(),
                artifact.size_bytes,
            )
            .await
            .map_err(map_archive_error)?;
        if proof.key != audio
            || proof.recording_id != envelope.recording_id.to_string()
            || proof.sha256 != artifact.sha256
            || proof.size_bytes != artifact.size_bytes
        {
            return Err(EffectError::new(EffectErrorKind::Verification));
        }
        Ok(CanonicalBackupCheckpoint {
            locator: format!("r2:{}", proof.key),
            version_id: proof.version_id,
            sha256: proof.sha256,
            size_bytes: proof.size_bytes,
            proof_json: json!({"etag": proof.etag, "head_verified": true}),
        })
    }
}

#[async_trait]
impl ArchiveEffects for DeferredArchive {
    fn plan(
        &self,
        _: &RecordingEnvelope,
        backend: PublicationBackend,
    ) -> Result<Vec<PublicationTargetPlan>, EffectError> {
        Ok(vec![PublicationTargetPlan {
            id: match backend {
                PublicationBackend::RemoteArchive => TARGET_ID,
                PublicationBackend::LocalArchive => LOCAL_TARGET_ID,
            }
            .to_owned(),
            retry_mode: RetryMode::ReconcileBeforeRetry,
            required: true,
        }])
    }

    async fn publish(
        &self,
        target_id: &str,
        generation: u64,
        envelope: &RecordingEnvelope,
        transcript: &Value,
        summary: &Value,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<PublicationProof, EffectError> {
        if target_id == LOCAL_TARGET_ID {
            self.load_local()?
                .publish(
                    target_id, generation, envelope, transcript, summary, artifact,
                )
                .await
        } else {
            self.load()?
                .publish(
                    target_id, generation, envelope, transcript, summary, artifact,
                )
                .await
        }
    }

    async fn verify_backup(
        &self,
        backend: PublicationBackend,
        generation: u64,
        envelope: &RecordingEnvelope,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<CanonicalBackupCheckpoint, EffectError> {
        match backend {
            PublicationBackend::RemoteArchive => {
                self.load()?
                    .verify_backup(backend, generation, envelope, artifact)
                    .await
            }
            PublicationBackend::LocalArchive => {
                self.load_local()?
                    .verify_backup(backend, generation, envelope, artifact)
                    .await
            }
        }
    }
}

struct PreparedArchive {
    note_relative: String,
    prior_note_relative: Option<String>,
    audio_relative: String,
    r2_key: String,
    r2_generation: u64,
    reuse_remote_r2: bool,
    replacement_r2_key: Option<String>,
    note: String,
    manifest: Vec<u8>,
}

struct ReusableRemoteR2 {
    key: String,
    generation: u64,
}

fn reusable_remote_r2(
    entry: Option<&Value>,
    recording_id: Uuid,
    artifact: &NormalizedArtifactCheckpoint,
) -> Result<Option<ReusableRemoteR2>, ArchiveError> {
    let Some(entry) = entry.and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(key) = entry.get("r2_key").and_then(Value::as_str) else {
        return Ok(None);
    };
    let generation = entry
        .get("r2_generation")
        .or_else(|| entry.get("publish_generation"))
        .and_then(Value::as_u64)
        .filter(|generation| *generation > 0)
        .ok_or_else(ArchiveError::verification)?;
    if !r2::is_generation_owned_key(key, &recording_id.to_string(), generation)
        || entry.get("audio_sha256").and_then(Value::as_str) != Some(artifact.sha256.as_str())
        || entry.get("audio_size_bytes").and_then(Value::as_u64) != Some(artifact.size_bytes)
    {
        return Err(ArchiveError::verification());
    }
    Ok(Some(ReusableRemoteR2 {
        key: key.to_owned(),
        generation,
    }))
}

struct PreparedNativeEdit {
    upserts: Vec<(String, Vec<u8>)>,
    deletes: Vec<String>,
    manifest: Vec<u8>,
    marker: String,
}

fn native_edit_proof(prepared: &PreparedNativeEdit, commit_sha: &str) -> PublicationProof {
    PublicationProof {
        locator: format!("github:commit:{commit_sha}"),
        version: commit_sha.to_owned(),
        sha256: sha256_hex(&prepared.manifest),
        size_bytes: prepared.manifest.len() as u64,
    }
}

struct GitHubHead {
    head_sha: String,
    tree_sha: String,
    message: String,
    tree_blobs: HashMap<String, String>,
    remote: RemoteArchive,
}

struct RemoteArchive {
    manifest: BTreeMap<String, Value>,
    notes: Vec<(String, Vec<u8>)>,
    deleted_recording_ids: std::collections::BTreeSet<String>,
}

enum ArchivePreparation<'a> {
    Remote(&'a RemoteArchive),
    LocalOnly,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeletedRecordingsFile {
    schema_version: u32,
    recording_ids: Vec<String>,
}

fn r2_config(tokens: &SyncTokens, bucket: String) -> R2Cfg {
    R2Cfg {
        account_id: tokens.r2_account_id.clone(),
        access_key_id: tokens.r2_access_key_id.clone(),
        secret_access_key: tokens.r2_secret_access_key.clone(),
        bucket,
    }
}

fn validate_repo(repo: &str) -> Result<(), ArchiveError> {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err(ArchiveError::configuration());
    }
    validate_component(owner)?;
    validate_component(name)
}

fn validate_component(value: &str) -> Result<(), ArchiveError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ArchiveError::configuration());
    }
    Ok(())
}

fn resolve_source(
    inbox_root: &Path,
    recording_id: Uuid,
    artifact: &NormalizedArtifactCheckpoint,
) -> Result<PathBuf, ArchiveError> {
    let relative = Path::new(&artifact.relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ArchiveError::verification());
    }
    let package = inbox_root.join(recording_id.to_string());
    let source = package.join(relative);
    let package = fs::canonicalize(package).map_err(|_| ArchiveError::verification())?;
    let source = fs::canonicalize(source).map_err(|_| ArchiveError::verification())?;
    let metadata = fs::symlink_metadata(&source).map_err(|_| ArchiveError::verification())?;
    if !source.starts_with(package) || metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ArchiveError::verification());
    }
    let (sha256, size_bytes) = hash_file(&source)?;
    if sha256 != artifact.sha256 || size_bytes != artifact.size_bytes {
        return Err(ArchiveError::verification());
    }
    Ok(source)
}

fn archive_title(envelope: &RecordingEnvelope, summary: &Value) -> Result<String, ArchiveError> {
    let title = envelope
        .import_review
        .as_ref()
        .filter(|review| review.confirmed_at.is_some())
        .and_then(|review| review.display_title.as_deref())
        .or_else(|| summary.get("title").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .ok_or_else(ArchiveError::verification)?;
    Ok(title.to_owned())
}

fn source_kind_text(envelope: &RecordingEnvelope) -> String {
    serde_json::to_value(envelope.source.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn merge_manifests(
    remote: &BTreeMap<String, Value>,
    local: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, ArchiveError> {
    let mut merged = remote.clone();
    for (local_key, local_entry) in local {
        let local_recording = local_entry.get("recording_id").and_then(Value::as_str);
        let matching_key = local_recording.and_then(|recording_id| {
            merged.iter().find_map(|(key, entry)| {
                (entry.get("recording_id").and_then(Value::as_str) == Some(recording_id))
                    .then(|| key.clone())
            })
        });
        if let Some(matching_key) = matching_key {
            if let (Some(remote), Some(local)) = (
                merged.get_mut(&matching_key).and_then(Value::as_object_mut),
                local_entry.as_object(),
            ) {
                for field in ["speakers", "speakers_applied", "attachments"] {
                    if !remote.contains_key(field) {
                        if let Some(value) = local.get(field) {
                            remote.insert(field.to_owned(), value.clone());
                        }
                    }
                }
            }
            continue;
        }
        let mut key = local_key.clone();
        while merged.contains_key(&key) {
            let slot = parse_archive_key(&key)? + Duration::seconds(1);
            key = slot.format("%Y-%m-%d %H%M%S").to_string();
        }
        merged.insert(key, local_entry.clone());
    }
    Ok(merged)
}

fn clean_category(category: Option<&str>) -> String {
    category
        .filter(|value| CATEGORIES.contains(value))
        .unwrap_or("其他")
        .to_owned()
}

fn reserve_archive_key(
    manifest: &BTreeMap<String, Value>,
    recording_id: Uuid,
    captured_at: DateTime<FixedOffset>,
) -> String {
    if let Some((key, _)) = manifest.iter().find(|(_, entry)| {
        entry.get("recording_id").and_then(Value::as_str) == Some(recording_id.to_string().as_str())
    }) {
        return key.clone();
    }
    let mut slot = captured_at.with_nanosecond(0).unwrap_or(captured_at);
    loop {
        let key = slot.format("%Y-%m-%d %H%M%S").to_string();
        if !manifest.contains_key(&key) {
            return key;
        }
        slot += Duration::seconds(1);
    }
}

fn parse_archive_key(value: &str) -> Result<DateTime<FixedOffset>, ArchiveError> {
    DateTime::parse_from_str(&format!("{value} +0000"), "%Y-%m-%d %H%M%S %z")
        .map_err(|_| ArchiveError::verification())
}

fn recording_stem(slot: DateTime<FixedOffset>, title: &str) -> String {
    format!("{}-{}", slot.format("%H%M%S"), slug(title))
}

fn slug(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_alphanumeric() || matches!(character, '一'..='鿿') {
            if separator && !output.is_empty() {
                output.push('-');
            }
            separator = false;
            output.push(character);
        } else if character.is_whitespace() || character == '-' {
            separator = true;
        }
        if output.chars().count() >= 60 {
            break;
        }
    }
    output.trim_matches('-').to_owned().or_default_note()
}

trait DefaultNote {
    fn or_default_note(self) -> String;
}

impl DefaultNote for String {
    fn or_default_note(self) -> String {
        if self.is_empty() {
            "note".to_owned()
        } else {
            self
        }
    }
}

fn display_title(envelope: &RecordingEnvelope, title: &str) -> String {
    format!("{} {title}", envelope.captured_at.format("%Y-%m-%d %H:%M"))
}

fn build_note(
    envelope: &RecordingEnvelope,
    transcript: &Value,
    summary: &Value,
    title: &str,
) -> Result<String, ArchiveError> {
    let summary_en = if summary.get("summary_en").is_some() {
        bounded_optional_text(summary.get("summary_en"))?
    } else {
        bounded_optional_text(summary.get("summary"))?
    };
    let summary_zh = bounded_optional_text(summary.get("summary_zh"))?;
    let mut key_points = string_array(summary.get("key_points_en"))?;
    let key_points_zh = string_array(summary.get("key_points_zh"))?;
    if key_points.is_empty() {
        key_points = string_array(summary.get("key_points"))?;
    }
    let mut todos = action_item_strings(summary.get("action_items"))?;
    if todos.is_empty() {
        todos = string_array(summary.get("todos"))?
            .into_iter()
            .map(str::to_owned)
            .collect();
    }
    let transcript = transcript_text(transcript)?;
    let source = envelope
        .source
        .label
        .clone()
        .unwrap_or_else(|| source_kind_text(envelope));
    let original = envelope.imported_name.clone().unwrap_or_else(|| {
        Path::new(
            envelope
                .normalized_audio
                .as_deref()
                .unwrap_or("recording.wav"),
        )
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("recording.wav")
        .to_owned()
    });
    let mut lines = vec![
        format!("# {}", display_title(envelope, title)),
        String::new(),
        format!("**Recorded:** {}", envelope.captured_at.to_rfc3339()),
        format!("**Source:** {source}"),
        format!("**File:** `{original}`"),
        String::new(),
    ];
    if !summary_en.is_empty() || !summary_zh.is_empty() {
        lines.push("## Summary".to_owned());
        if !summary_en.is_empty() {
            lines.push(String::new());
            lines.push(summary_en);
        }
        if !summary_zh.is_empty() {
            lines.push(String::new());
            lines.push(summary_zh);
        }
        lines.push(String::new());
    }
    if !key_points.is_empty() || !key_points_zh.is_empty() {
        lines.push("## Key Points".to_owned());
        if !key_points.is_empty() {
            lines.push(String::new());
            lines.extend(key_points.into_iter().map(|point| format!("- {point}")));
        }
        if !key_points_zh.is_empty() {
            lines.push(String::new());
            lines.extend(key_points_zh.into_iter().map(|point| format!("- {point}")));
        }
        lines.push(String::new());
    }
    if !todos.is_empty() {
        lines.push("## Action Items".to_owned());
        lines.push(String::new());
        lines.extend(todos.into_iter().map(|todo| format!("- [ ] {todo}")));
        lines.push(String::new());
    }
    lines.extend([
        "---".to_owned(),
        String::new(),
        "## Transcript".to_owned(),
        String::new(),
        "```".to_owned(),
        transcript.trim().to_owned(),
        "```".to_owned(),
        String::new(),
    ]);
    let note = lines.join("\n");
    if note.len() > MAX_NOTE_BYTES {
        return Err(ArchiveError::verification());
    }
    Ok(note)
}

fn bounded_optional_text(value: Option<&Value>) -> Result<String, ArchiveError> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    value
        .as_str()
        .filter(|value| value.len() <= MAX_NOTE_BYTES)
        .map(str::to_owned)
        .ok_or_else(ArchiveError::verification)
}

fn string_array(value: Option<&Value>) -> Result<Vec<&str>, ArchiveError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(ArchiveError::verification)?;
    if array.len() > 10_000 {
        return Err(ArchiveError::verification());
    }
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| value.len() <= 8_192)
                .ok_or_else(ArchiveError::verification)
        })
        .collect()
}

fn action_item_strings(value: Option<&Value>) -> Result<Vec<String>, ArchiveError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(ArchiveError::verification)?;
    if array.len() > 10_000 {
        return Err(ArchiveError::verification());
    }
    array
        .iter()
        .map(|value| {
            if let Some(value) = value.as_str().filter(|value| value.len() <= 8_192) {
                return Ok(value.to_owned());
            }
            let object = value.as_object().ok_or_else(ArchiveError::verification)?;
            if object.is_empty()
                || object.len() > 3
                || object
                    .keys()
                    .any(|key| !matches!(key.as_str(), "task" | "owner" | "due_date"))
            {
                return Err(ArchiveError::verification());
            }
            let task = object
                .get("task")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.len() <= 8_192)
                .ok_or_else(ArchiveError::verification)?;
            let optional = |key: &str| -> Result<Option<&str>, ArchiveError> {
                object
                    .get(key)
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::trim)
                            .filter(|value| !value.is_empty() && value.len() <= 2_000)
                            .ok_or_else(ArchiveError::verification)
                    })
                    .transpose()
            };
            let owner = optional("owner")?;
            let due_date = optional("due_date")?;
            let mut normalized = task.to_owned();
            let qualifiers = [
                owner.map(|value| format!("owner: {value}")),
                due_date.map(|value| format!("due: {value}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if !qualifiers.is_empty() {
                normalized.push_str(" (");
                normalized.push_str(&qualifiers.join("; "));
                normalized.push(')');
            }
            if normalized.len() > 8_192 {
                return Err(ArchiveError::verification());
            }
            Ok(normalized)
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn moss_transcript_text_for_test(value: &Value) -> Result<String, ArchiveError> {
    transcript_text(value)
}

fn transcript_text(value: &Value) -> Result<String, ArchiveError> {
    if let Some(text) = value.as_str() {
        return Ok(text.to_owned());
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Ok(text.to_owned());
    }
    let sentences = value
        .as_array()
        .or_else(|| value.get("sentences").and_then(Value::as_array))
        .ok_or_else(ArchiveError::verification)?;
    if sentences.len() > 100_000 {
        return Err(ArchiveError::verification());
    }
    let mut output = String::new();
    for sentence in sentences {
        let start = sentence
            .get("start_time")
            .and_then(Value::as_u64)
            .ok_or_else(ArchiveError::verification)?;
        let end = sentence
            .get("end_time")
            .and_then(Value::as_u64)
            .ok_or_else(ArchiveError::verification)?;
        let speaker = sentence
            .pointer("/speaker/id")
            .and_then(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_u64().map(|value| value.to_string()))
            })
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 32
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
            .ok_or_else(ArchiveError::verification)?;
        let content = sentence
            .get("content")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 1024 * 1024)
            .ok_or_else(ArchiveError::verification)?;
        if end < start {
            return Err(ArchiveError::verification());
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!(
            "[{} - {}] SPEAKER_{}: {}",
            format_hms(start),
            format_hms(end),
            speaker,
            content.replace(['\r', '\n'], " ")
        ));
        if output.len() > MAX_NOTE_BYTES {
            return Err(ArchiveError::verification());
        }
    }
    Ok(output)
}

fn format_hms(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn load_manifest(root: &Path) -> Result<BTreeMap<String, Value>, ArchiveError> {
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    let path = root.join("manifest.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| ArchiveError::verification())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_LOCAL_JSON_BYTES
    {
        return Err(ArchiveError::verification());
    }
    let value: BTreeMap<String, Value> =
        serde_json::from_reader(File::open(path).map_err(|_| ArchiveError::verification())?)
            .map_err(|_| ArchiveError::verification())?;
    for (key, entry) in &value {
        let slot = parse_archive_key(key)?;
        if slot.format("%Y-%m-%d %H%M%S").to_string() != *key {
            return Err(ArchiveError::verification());
        }
        if !entry.is_object() {
            return Err(ArchiveError::verification());
        }
        validate_manifest_entry_paths(&root, slot, entry)?;
    }
    Ok(value)
}

fn serialize_manifest(manifest: &BTreeMap<String, Value>) -> Result<Vec<u8>, ArchiveError> {
    let mut bytes =
        serde_json::to_vec_pretty(manifest).map_err(|_| ArchiveError::verification())?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_LOCAL_JSON_BYTES {
        return Err(ArchiveError::verification());
    }
    Ok(bytes)
}

fn serialize_deleted_recordings(
    recording_ids: &std::collections::BTreeSet<String>,
) -> Result<Vec<u8>, ArchiveError> {
    if recording_ids.len() > 100_000
        || recording_ids
            .iter()
            .any(|recording_id| Uuid::parse_str(recording_id).is_err())
    {
        return Err(ArchiveError::verification());
    }
    let mut bytes = serde_json::to_vec_pretty(&DeletedRecordingsFile {
        schema_version: 1,
        recording_ids: recording_ids.iter().cloned().collect(),
    })
    .map_err(|_| ArchiveError::verification())?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_NATIVE_EDIT_OUTBOX_BYTES {
        return Err(ArchiveError::verification());
    }
    Ok(bytes)
}

fn parse_deleted_recordings(
    bytes: &[u8],
) -> Result<std::collections::BTreeSet<String>, ArchiveError> {
    if bytes.len() as u64 > MAX_NATIVE_EDIT_OUTBOX_BYTES {
        return Err(ArchiveError::verification());
    }
    let file: DeletedRecordingsFile =
        serde_json::from_slice(bytes).map_err(|_| ArchiveError::verification())?;
    if file.schema_version != 1 || file.recording_ids.len() > 100_000 {
        return Err(ArchiveError::verification());
    }
    let mut recording_ids = std::collections::BTreeSet::new();
    for recording_id in file.recording_ids {
        let id = Uuid::parse_str(&recording_id).map_err(|_| ArchiveError::verification())?;
        if id.to_string() != recording_id || !recording_ids.insert(recording_id) {
            return Err(ArchiveError::verification());
        }
    }
    Ok(recording_ids)
}

fn load_local_deleted_recordings(
    root: &Path,
) -> Result<std::collections::BTreeSet<String>, ArchiveError> {
    let path = root.join(DELETED_RECORDINGS_PATH);
    if !path.exists() {
        return Ok(std::collections::BTreeSet::new());
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| ArchiveError::verification())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_NATIVE_EDIT_OUTBOX_BYTES
    {
        return Err(ArchiveError::verification());
    }
    parse_deleted_recordings(&fs::read(path).map_err(|_| ArchiveError::verification())?)
}

fn prior_entry(manifest: &BTreeMap<String, Value>, recording_id: Uuid) -> Option<&Value> {
    manifest.values().find(|entry| {
        entry.get("recording_id").and_then(Value::as_str) == Some(recording_id.to_string().as_str())
    })
}

fn validate_destination_owner(
    manifest: &BTreeMap<String, Value>,
    key: &str,
    recording_id: Uuid,
) -> Result<(), ArchiveError> {
    if manifest.get(key).is_some_and(|entry| {
        entry.get("recording_id").and_then(Value::as_str) != Some(recording_id.to_string().as_str())
    }) {
        return Err(ArchiveError::conflict());
    }
    Ok(())
}

fn remove_stale_owned_file(
    root: &Path,
    value: Option<&Value>,
    current: &str,
    manifest: &BTreeMap<String, Value>,
    field: &str,
) -> Result<(), ArchiveError> {
    let Some(relative) = value
        .and_then(Value::as_str)
        .filter(|value| *value != current)
    else {
        return Ok(());
    };
    if manifest_references_path(manifest, field, relative) {
        return Ok(());
    }
    let path = safe_archive_path(root, relative)?;
    if path.exists() {
        fs::remove_file(path).map_err(|_| ArchiveError::verification())?;
    }
    Ok(())
}

fn manifest_references_path(
    manifest: &BTreeMap<String, Value>,
    field: &str,
    relative: &str,
) -> bool {
    manifest
        .values()
        .any(|entry| entry.get(field).and_then(Value::as_str) == Some(relative))
}

fn manifest_path_owned_by_other(
    manifest: &BTreeMap<String, Value>,
    field: &str,
    relative: &str,
    recording_id: Uuid,
) -> bool {
    let recording_id = recording_id.to_string();
    manifest.values().any(|entry| {
        entry.get(field).and_then(Value::as_str) == Some(relative)
            && entry.get("recording_id").and_then(Value::as_str) != Some(recording_id.as_str())
    })
}

fn safe_archive_path(root: &Path, relative: &str) -> Result<PathBuf, ArchiveError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ArchiveError::verification());
    }
    Ok(root.join(relative))
}

fn ensure_archive_directory(root: &Path, relative: &str) -> Result<PathBuf, ArchiveError> {
    let path = safe_archive_path(root, relative)?;
    if path.exists() {
        let metadata = fs::symlink_metadata(&path).map_err(|_| ArchiveError::verification())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ArchiveError::verification());
        }
    } else {
        fs::create_dir(&path).map_err(|_| ArchiveError::verification())?;
    }
    let resolved = fs::canonicalize(&path).map_err(|_| ArchiveError::verification())?;
    if !resolved.starts_with(root) {
        return Err(ArchiveError::verification());
    }
    Ok(resolved)
}

fn strip_display_prefix(title: &str) -> &str {
    let bytes = title.as_bytes();
    if bytes.len() > 16
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b' ')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b' ')
        && bytes[..16]
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13) || byte.is_ascii_digit())
    {
        &title[17..]
    } else {
        title
    }
}

fn rebuild_viewer(
    root: &Path,
    manifest: &BTreeMap<String, Value>,
) -> Result<Vec<u8>, ArchiveError> {
    let html = render_viewer(root, manifest)?;
    atomic_write(&root.join("marked.min.js"), VIEWER_MARKED_JS)?;
    atomic_write(&root.join("index.html"), &html)?;
    Ok(html)
}

fn render_viewer(root: &Path, manifest: &BTreeMap<String, Value>) -> Result<Vec<u8>, ArchiveError> {
    let mut entries = Vec::new();
    for (key, entry) in manifest.iter().rev() {
        let Some(note_relative) = entry.get("note").and_then(Value::as_str) else {
            continue;
        };
        let note_path = safe_archive_path(root, note_relative)?;
        if !note_path.exists() {
            continue;
        }
        let note = read_bounded_text(&note_path, MAX_NOTE_BYTES as u64)?;
        let sections = note_sections(&note);
        let captured = entry
            .get("captured_at")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok());
        let day = captured
            .map(|value| value.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| key[..10].to_owned());
        let hhmmss = captured
            .map(|value| value.format("%H%M%S").to_string())
            .unwrap_or_else(|| key[11..].to_owned());
        let title = entry.get("title").and_then(Value::as_str).unwrap_or("note");
        let summary_paragraphs: Vec<_> = sections
            .get("Summary")
            .map(String::as_str)
            .unwrap_or_default()
            .split("\n\n")
            .filter(|value| !value.trim().is_empty())
            .collect();
        let summary_zh = summary_paragraphs
            .iter()
            .find(|value| contains_cjk(value))
            .copied()
            .unwrap_or_default();
        let summary_en = summary_paragraphs
            .iter()
            .find(|value| !contains_cjk(value))
            .copied()
            .unwrap_or_default();
        let key_points: Vec<_> = sections
            .get("Key Points")
            .into_iter()
            .flat_map(|value| value.lines())
            .filter_map(|line| line.strip_prefix("- "))
            .collect();
        let todos: Vec<_> = sections
            .get("Action Items")
            .into_iter()
            .flat_map(|value| value.lines())
            .filter_map(|line| line.strip_prefix("- [ ] "))
            .collect();
        let transcript = sections
            .get("Transcript")
            .map(|value| {
                value
                    .trim()
                    .trim_start_matches("```text")
                    .trim_end_matches("```")
                    .trim()
            })
            .unwrap_or_default();
        let duration = transcript_duration_seconds(transcript);
        let attachments = load_attachments(root, entry.get("attachments"))?;
        entries.push(json!({
            "key": key,
            "date": day,
            "weekday": weekday(&day),
            "time": format!("{}:{}", &hhmmss[..2], &hhmmss[2..4]),
            "hour_frac": hhmmss[..2].parse::<f64>().unwrap_or(0.0)
                + hhmmss[2..4].parse::<f64>().unwrap_or(0.0) / 60.0,
            "title": title,
            "ai_title": strip_display_prefix(title),
            "category": entry.get("category").cloned().unwrap_or_else(|| Value::String("其他".to_owned())),
            "original": entry.get("original"),
            "audio": entry.get("audio"),
            "r2_audio": entry.get("r2_key"),
            "recording_id": entry.get("recording_id"),
            "publish_generation": entry.get("publish_generation"),
            "source": entry.get("source"),
            "source_kind": entry.get("source_kind"),
            "speakers": entry.get("speakers").cloned().unwrap_or_else(|| json!({})),
            "attachments": attachments,
            "summary_zh": summary_zh,
            "summary_en": summary_en,
            "key_points": key_points,
            "todos": todos,
            "transcript": transcript,
            "duration": duration,
        }));
    }
    let speaker_colors = load_optional_json(root, "speakers.json")?.unwrap_or_else(|| json!({}));
    let years: std::collections::BTreeSet<_> = entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("date")
                .and_then(Value::as_str)
                .map(|value| &value[..4])
        })
        .collect();
    let span = years
        .first()
        .zip(years.last())
        .map(|(first, last)| format!("{first}–{last}"))
        .unwrap_or_default();
    let payload = json!({
        "span": span,
        "categories": CATEGORIES,
        "speaker_colors": speaker_colors,
        "entries": entries,
    });
    let payload = serde_json::to_string(&payload)
        .map_err(|_| ArchiveError::verification())?
        .replace("</", "<\\/");
    let html = VIEWER_TEMPLATE.replace("__PAYLOAD__", &payload);
    Ok(html.into_bytes())
}

fn transcript_duration_seconds(transcript: &str) -> Option<u64> {
    let bytes = transcript.as_bytes();
    let mut maximum = None;
    for offset in 0..bytes.len().saturating_sub(7) {
        let candidate = &bytes[offset..offset + 8];
        if candidate[2] != b':'
            || candidate[5] != b':'
            || !candidate
                .iter()
                .enumerate()
                .all(|(index, byte)| matches!(index, 2 | 5) || byte.is_ascii_digit())
        {
            continue;
        }
        let hours = u64::from(candidate[0] - b'0') * 10 + u64::from(candidate[1] - b'0');
        let minutes = u64::from(candidate[3] - b'0') * 10 + u64::from(candidate[4] - b'0');
        let seconds = u64::from(candidate[6] - b'0') * 10 + u64::from(candidate[7] - b'0');
        if minutes < 60 && seconds < 60 {
            maximum = Some(
                maximum
                    .unwrap_or(0)
                    .max(hours * 3_600 + minutes * 60 + seconds),
            );
        }
    }
    maximum
}

pub(crate) fn render_existing_viewer(root: &Path) -> Result<Vec<u8>, ArchiveError> {
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    let manifest = load_manifest(&root)?;
    rebuild_viewer(&root, &manifest)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalArchiveDelete {
    pub deleted: Vec<String>,
    pub audio_relative: Option<String>,
    pub r2_key: Option<String>,
    pub recording_id: Option<String>,
    pub publish_generation: Option<u64>,
    pub r2_generation: Option<u64>,
    pub base_entry_sha256: String,
    pub audio_sha256: Option<String>,
    pub audio_size_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub(crate) enum NativeArchiveEdit {
    UserFields {
        deltas: Vec<ArchiveEntryDelta>,
    },
    SpeakerColors {
        name: String,
        expected: Option<String>,
        desired: String,
    },
    Delete {
        key: String,
        recording_id: Option<String>,
        base_entry_sha256: Option<String>,
        deleted_paths: Vec<String>,
        r2_delete: Option<R2DeleteIntent>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchiveEntryDelta {
    pub key: String,
    pub recording_id: Option<String>,
    pub base_entry_sha256: Option<String>,
    pub speaker_slots: Vec<SpeakerSlotDelta>,
    pub add_attachments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpeakerSlotDelta {
    pub slot: String,
    pub expected: Option<String>,
    pub desired: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct R2DeleteIntent {
    pub key: String,
    pub recording_id: String,
    pub generation: u64,
    pub sha256: String,
    pub size_bytes: u64,
}

pub(crate) fn archive_entry_sha256(entry: &Value) -> Result<String, ArchiveError> {
    serde_json::to_vec(entry)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|_| ArchiveError::verification())
}

pub(crate) fn archive_entry_snapshot(root: &Path, key: &str) -> Result<Value, ArchiveError> {
    load_manifest(root)?
        .get(key)
        .cloned()
        .ok_or_else(ArchiveError::verification)
}

pub(crate) fn speaker_color_snapshot(
    root: &Path,
    name: &str,
) -> Result<Option<String>, ArchiveError> {
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    let path = root.join("speakers.json");
    if !path.exists() {
        return Ok(None);
    }
    let value: Map<String, Value> =
        serde_json::from_str(&read_bounded_text(&path, MAX_LOCAL_JSON_BYTES)?)
            .map_err(|_| ArchiveError::verification())?;
    value
        .get(name)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(ArchiveError::verification)
        })
        .transpose()
}

pub(crate) fn prepare_local_attachment(
    root: &Path,
    key: &str,
    raw_name: &str,
    content: &str,
) -> Result<(String, ArchiveEntryDelta), ArchiveError> {
    if content.len() > MAX_ATTACHMENT_BYTES as usize || raw_name.len() > 256 {
        return Err(ArchiveError::verification());
    }
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    let slot = parse_archive_key(key)?;
    if slot.format("%Y-%m-%d %H%M%S").to_string() != key {
        return Err(ArchiveError::verification());
    }
    let entry = archive_entry_snapshot(&root, key)?;
    let recording_id = entry
        .get("recording_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let base_entry_sha256 = archive_entry_sha256(&entry)?;
    let mut name: String = raw_name
        .chars()
        .take(80)
        .filter(|character| {
            character.is_alphanumeric()
                || " ()._-".contains(*character)
                || ('一'..='鿿').contains(character)
        })
        .collect();
    name = name.trim().trim_matches('.').to_owned();
    if name.is_empty() {
        name = "附注".to_owned();
    }
    let name = name.strip_suffix(".md").unwrap_or(&name);
    let day = slot.format("%Y-%m-%d").to_string();
    let time = slot.format("%H%M%S").to_string();
    ensure_archive_directory(&root, &day)?;
    let attachment_directory = format!("{day}/{time}-attachments");
    ensure_archive_directory(&root, &attachment_directory)?;
    let content_sha256 = sha256_hex(content.as_bytes());
    let relative = format!("{attachment_directory}/{name}-{content_sha256}.md");
    let path =
        validate_owned_archive_path(&root, slot, &relative, OwnedArchivePathKind::Attachment)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_ATTACHMENT_BYTES
                || fs::read(&path).map_err(|_| ArchiveError::verification())? != content.as_bytes()
            {
                return Err(ArchiveError::verification());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            atomic_write(&path, content.as_bytes())?;
        }
        Err(_) => return Err(ArchiveError::verification()),
    }
    Ok((
        relative.clone(),
        ArchiveEntryDelta {
            key: key.to_owned(),
            recording_id,
            base_entry_sha256: Some(base_entry_sha256),
            speaker_slots: Vec::new(),
            add_attachments: vec![relative],
        },
    ))
}

fn entry_satisfies_delta(entry: &Value, delta: &ArchiveEntryDelta) -> bool {
    delta.speaker_slots.iter().all(|change| {
        entry
            .get("speakers")
            .and_then(Value::as_object)
            .and_then(|speakers| speakers.get(&change.slot))
            .and_then(Value::as_str)
            .map(str::to_owned)
            == change.desired
    }) && delta.add_attachments.iter().all(|path| {
        entry
            .get("attachments")
            .and_then(Value::as_array)
            .is_some_and(|attachments| {
                attachments
                    .iter()
                    .any(|attachment| attachment.as_str() == Some(path))
            })
    })
}

fn apply_native_edit_locally(root: &Path, edit: &NativeArchiveEdit) -> Result<(), ArchiveError> {
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    match edit {
        NativeArchiveEdit::UserFields { deltas } => {
            let mut manifest = load_manifest(&root)?;
            let mut changed_keys = Vec::new();
            for delta in deltas {
                let target = if let Some(recording_id) = &delta.recording_id {
                    manifest.iter().find_map(|(key, entry)| {
                        (entry.get("recording_id").and_then(Value::as_str)
                            == Some(recording_id.as_str()))
                        .then(|| key.clone())
                    })
                } else if let Some(entry) = manifest.get(&delta.key) {
                    if entry.get("recording_id").is_some()
                        || (delta.base_entry_sha256.as_deref()
                            != Some(archive_entry_sha256(entry)?.as_str())
                            && !entry_satisfies_delta(entry, delta))
                    {
                        return Err(ArchiveError::conflict());
                    }
                    Some(delta.key.clone())
                } else {
                    None
                };
                let Some(target) = target else { continue };
                let slot = parse_archive_key(&target)?;
                let entry = manifest
                    .get_mut(&target)
                    .and_then(Value::as_object_mut)
                    .ok_or_else(ArchiveError::verification)?;
                let mut speakers = speaker_map(entry.get("speakers"))?;
                for change in &delta.speaker_slots {
                    let current = speakers.get(&change.slot).cloned();
                    if current == change.desired {
                        continue;
                    }
                    if current != change.expected {
                        return Err(ArchiveError::conflict());
                    }
                    if let Some(desired) = &change.desired {
                        speakers.insert(change.slot.clone(), desired.clone());
                    } else {
                        speakers.remove(&change.slot);
                    }
                }
                if speakers.is_empty() {
                    entry.remove("speakers");
                } else {
                    entry.insert(
                        "speakers".to_owned(),
                        Value::Object(
                            speakers
                                .into_iter()
                                .map(|(slot, name)| (slot, Value::String(name)))
                                .collect(),
                        ),
                    );
                }
                for path in &delta.add_attachments {
                    let absolute = validate_owned_archive_path(
                        &root,
                        slot,
                        path,
                        OwnedArchivePathKind::Attachment,
                    )?;
                    let metadata = fs::symlink_metadata(&absolute)
                        .map_err(|_| ArchiveError::verification())?;
                    if metadata.file_type().is_symlink()
                        || !metadata.is_file()
                        || metadata.len() > MAX_ATTACHMENT_BYTES
                    {
                        return Err(ArchiveError::verification());
                    }
                    let bytes = fs::read(&absolute).map_err(|_| ArchiveError::verification())?;
                    require_attachment_content_identity(path, &bytes)?;
                    let attachments = entry
                        .entry("attachments")
                        .or_insert_with(|| Value::Array(Vec::new()))
                        .as_array_mut()
                        .ok_or_else(ArchiveError::verification)?;
                    if !attachments
                        .iter()
                        .any(|attachment| attachment.as_str() == Some(path))
                    {
                        attachments.push(Value::String(path.clone()));
                    }
                }
                changed_keys.push(target);
            }
            atomic_write(&root.join("manifest.json"), &serialize_manifest(&manifest)?)?;
            apply_speaker_labels(&root, &changed_keys)?;
            let manifest = load_manifest(&root)?;
            refresh_all_daily_rollups(&root, &manifest)?;
            rebuild_topic_views(&root, &manifest)?;
            rebuild_viewer(&root, &manifest)?;
        }
        NativeArchiveEdit::SpeakerColors {
            name,
            expected,
            desired,
        } => {
            let path = root.join("speakers.json");
            let mut colors: Map<String, Value> = if path.exists() {
                serde_json::from_slice(&fs::read(&path).map_err(|_| ArchiveError::verification())?)
                    .map_err(|_| ArchiveError::verification())?
            } else {
                Map::new()
            };
            let current = colors.get(name).and_then(Value::as_str).map(str::to_owned);
            if current != *expected && current.as_deref() != Some(desired) {
                return Err(ArchiveError::conflict());
            }
            colors.insert(name.clone(), Value::String(desired.clone()));
            atomic_write(
                &path,
                &serde_json::to_vec_pretty(&colors).map_err(|_| ArchiveError::verification())?,
            )?;
            rebuild_viewer(&root, &load_manifest(&root)?)?;
        }
        NativeArchiveEdit::Delete {
            key,
            recording_id,
            base_entry_sha256,
            deleted_paths,
            ..
        } => {
            let slot = parse_archive_key(key)?;
            let mut manifest = load_manifest(&root)?;
            let target = if let Some(recording_id) = recording_id {
                let target = manifest.iter().find_map(|(target, entry)| {
                    (entry.get("recording_id").and_then(Value::as_str)
                        == Some(recording_id.as_str()))
                    .then(|| target.clone())
                });
                if let Some(target) = &target {
                    let entry = manifest
                        .get(target)
                        .ok_or_else(ArchiveError::verification)?;
                    if base_entry_sha256.as_deref() != Some(archive_entry_sha256(entry)?.as_str()) {
                        return Err(ArchiveError::conflict());
                    }
                }
                target
            } else if let Some(entry) = manifest.get(key) {
                if entry.get("recording_id").is_some()
                    || base_entry_sha256.as_deref() != Some(archive_entry_sha256(entry)?.as_str())
                {
                    return Err(ArchiveError::conflict());
                }
                Some(key.clone())
            } else {
                None
            };
            if let Some(target) = target {
                manifest.remove(&target);
            }
            if let Some(recording_id) = recording_id {
                let mut deleted_recording_ids = load_local_deleted_recordings(&root)?;
                deleted_recording_ids.insert(recording_id.clone());
                atomic_write(
                    &root.join(DELETED_RECORDINGS_PATH),
                    &serialize_deleted_recordings(&deleted_recording_ids)?,
                )?;
            }
            let mut removable = Vec::new();
            for relative in deleted_paths {
                if validate_owned_archive_path(&root, slot, relative, OwnedArchivePathKind::Note)
                    .is_err()
                    && validate_owned_archive_path(
                        &root,
                        slot,
                        relative,
                        OwnedArchivePathKind::Audio,
                    )
                    .is_err()
                    && validate_owned_archive_path(
                        &root,
                        slot,
                        relative,
                        OwnedArchivePathKind::Attachment,
                    )
                    .is_err()
                {
                    return Err(ArchiveError::verification());
                }
                if !manifest_references_any_path(&manifest, relative) {
                    removable.push(relative.clone());
                }
            }
            atomic_write(&root.join("manifest.json"), &serialize_manifest(&manifest)?)?;
            rebuild_viewer(&root, &manifest)?;
            cleanup_topic_links(&root, &removable)?;
            let attachment_directories: std::collections::BTreeSet<_> = removable
                .iter()
                .filter_map(|relative| {
                    let parent = Path::new(relative).parent()?;
                    (parent.components().count() == 2)
                        .then(|| parent.to_string_lossy().into_owned())
                })
                .collect();
            for relative in &removable {
                let path = safe_archive_path(&root, relative)?;
                match fs::symlink_metadata(&path) {
                    Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
                        fs::remove_file(path).map_err(|_| ArchiveError::verification())?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err(ArchiveError::verification()),
                }
            }
            for relative in attachment_directories {
                let path = safe_archive_path(&root, &relative)?;
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ArchiveError::verification());
                }
                if fs::read_dir(&path)
                    .map_err(|_| ArchiveError::verification())?
                    .next()
                    .is_none()
                {
                    fs::remove_dir(path).map_err(|_| ArchiveError::verification())?;
                }
            }
            refresh_daily_rollup(&root, &slot.format("%Y-%m-%d").to_string(), &manifest)?;
            rebuild_topic_views(&root, &manifest)?;
        }
    }
    Ok(())
}

pub(crate) fn plan_local_recording_delete(
    root: &Path,
    key: &str,
) -> Result<LocalArchiveDelete, ArchiveError> {
    let slot = parse_archive_key(key)?;
    if slot.format("%Y-%m-%d %H%M%S").to_string() != key {
        return Err(ArchiveError::verification());
    }
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    let mut manifest = load_manifest(&root)?;
    let removed = manifest
        .remove(key)
        .and_then(|entry| entry.as_object().cloned())
        .ok_or_else(ArchiveError::verification)?;
    let audio_relative = removed
        .get("audio")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let r2_key = removed
        .get("r2_key")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let recording_id = removed
        .get("recording_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let publish_generation = removed.get("publish_generation").and_then(Value::as_u64);
    let r2_generation = removed
        .get("r2_generation")
        .and_then(Value::as_u64)
        .or(publish_generation);
    let audio_sha256 = removed
        .get("audio_sha256")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let audio_size_bytes = removed.get("audio_size_bytes").and_then(Value::as_u64);
    let base_entry_sha256 = archive_entry_sha256(&Value::Object(removed.clone()))?;
    if recording_id
        .as_deref()
        .is_some_and(|value| Uuid::parse_str(value).is_err())
    {
        return Err(ArchiveError::verification());
    }

    let mut victims = std::collections::BTreeSet::new();
    for (field, kind) in [
        (removed.get("note"), OwnedArchivePathKind::Note),
        (removed.get("audio"), OwnedArchivePathKind::Audio),
    ] {
        let Some(relative) = field else { continue };
        if matches!(kind, OwnedArchivePathKind::Audio) && relative.is_null() {
            continue;
        }
        let relative = relative.as_str().ok_or_else(ArchiveError::verification)?;
        validate_owned_archive_path(&root, slot, relative, kind)?;
        if !manifest_references_any_path(&manifest, relative) {
            victims.insert(relative.to_owned());
        }
    }
    if let Some(attachments) = removed.get("attachments") {
        let attachments = attachments
            .as_array()
            .ok_or_else(ArchiveError::verification)?;
        if attachments.len() > 1_000 {
            return Err(ArchiveError::verification());
        }
        for relative in attachments {
            let relative = relative.as_str().ok_or_else(ArchiveError::verification)?;
            validate_owned_archive_path(&root, slot, relative, OwnedArchivePathKind::Attachment)?;
            if !manifest_references_any_path(&manifest, relative) {
                victims.insert(relative.to_owned());
            }
        }
    }
    for relative in &victims {
        let path = safe_archive_path(&root, relative)?;
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ArchiveError::verification());
            }
        }
    }

    let victim_list: Vec<_> = victims.iter().cloned().collect();
    Ok(LocalArchiveDelete {
        // GitHub deletion intent includes valid owned paths even when their
        // local copies were already missing after a crash or prior attempt.
        deleted: victim_list,
        audio_relative,
        r2_key,
        recording_id,
        publish_generation,
        r2_generation,
        base_entry_sha256,
        audio_sha256,
        audio_size_bytes,
    })
}

#[cfg(test)]
pub(crate) fn delete_local_recording(
    root: &Path,
    key: &str,
) -> Result<LocalArchiveDelete, ArchiveError> {
    let planned = plan_local_recording_delete(root, key)?;
    apply_native_edit_locally(
        root,
        &NativeArchiveEdit::Delete {
            key: key.to_owned(),
            recording_id: planned.recording_id.clone(),
            base_entry_sha256: Some(planned.base_entry_sha256.clone()),
            deleted_paths: planned.deleted.clone(),
            r2_delete: None,
        },
    )?;
    Ok(planned)
}

#[derive(Debug, Clone, Copy)]
enum OwnedArchivePathKind {
    Note,
    Audio,
    Attachment,
}

fn validate_manifest_entry_paths(
    root: &Path,
    slot: DateTime<FixedOffset>,
    entry: &Value,
) -> Result<(), ArchiveError> {
    let recording_id = if let Some(recording_id) = entry.get("recording_id") {
        let recording_id = recording_id
            .as_str()
            .ok_or_else(ArchiveError::verification)?;
        Uuid::parse_str(recording_id).map_err(|_| ArchiveError::verification())?;
        Some(recording_id)
    } else {
        None
    };
    let generation = entry
        .get("publish_generation")
        .map(|value| value.as_u64().ok_or_else(ArchiveError::verification))
        .transpose()?;
    if generation == Some(0) {
        return Err(ArchiveError::verification());
    }
    let r2_generation = entry
        .get("r2_generation")
        .map(|value| value.as_u64().ok_or_else(ArchiveError::verification))
        .transpose()?
        .or(generation);
    if r2_generation == Some(0)
        || (entry.get("r2_generation").is_some() && entry.get("r2_key").is_none())
    {
        return Err(ArchiveError::verification());
    }
    for (field, kind) in [
        ("note", OwnedArchivePathKind::Note),
        ("audio", OwnedArchivePathKind::Audio),
    ] {
        if let Some(value) = entry.get(field) {
            if field == "audio" && value.is_null() {
                continue;
            }
            let relative = value.as_str().ok_or_else(ArchiveError::verification)?;
            validate_owned_archive_path(root, slot, relative, kind)?;
            if field == "audio" {
                let friendly = is_friendly_archive_audio(slot, relative);
                let interim_immutable =
                    recording_id
                        .zip(generation)
                        .is_some_and(|(recording_id, generation)| {
                            r2::is_generation_owned_key(relative, recording_id, generation)
                        });
                if !friendly && !interim_immutable {
                    return Err(ArchiveError::verification());
                }
            }
        }
    }
    if let Some(value) = entry.get("r2_key") {
        let key = value.as_str().ok_or_else(ArchiveError::verification)?;
        let (Some(recording_id), Some(generation)) = (recording_id, r2_generation) else {
            return Err(ArchiveError::verification());
        };
        if !r2::is_generation_owned_key(key, recording_id, generation) {
            return Err(ArchiveError::verification());
        }
    }
    match (entry.get("audio_sha256"), entry.get("audio_size_bytes")) {
        (None, None) => {}
        (Some(sha256), Some(size))
            if sha256.as_str().is_some_and(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }) && size.as_u64().is_some_and(|value| value > 0) => {}
        _ => return Err(ArchiveError::verification()),
    }
    if let Some(value) = entry.get("attachments") {
        let attachments = value.as_array().ok_or_else(ArchiveError::verification)?;
        if attachments.len() > 1_000 {
            return Err(ArchiveError::verification());
        }
        for attachment in attachments {
            let relative = attachment.as_str().ok_or_else(ArchiveError::verification)?;
            validate_owned_archive_path(root, slot, relative, OwnedArchivePathKind::Attachment)?;
        }
    }
    Ok(())
}

fn validate_owned_archive_path(
    root: &Path,
    slot: DateTime<FixedOffset>,
    relative: &str,
    kind: OwnedArchivePathKind,
) -> Result<PathBuf, ArchiveError> {
    let path = safe_archive_path(root, relative)?;
    let components: Vec<_> = Path::new(relative).components().collect();
    let day = slot.format("%Y-%m-%d").to_string();
    let time = slot.format("%H%M%S").to_string();
    let normal = |index: usize| match components.get(index) {
        Some(Component::Normal(value)) => value.to_str(),
        _ => None,
    };
    let valid = match kind {
        OwnedArchivePathKind::Note => {
            components.len() == 2
                && normal(0) == Some(day.as_str())
                && normal(1).is_some_and(|name| {
                    name.starts_with(&format!("{time}-")) && name.ends_with(".md")
                })
        }
        OwnedArchivePathKind::Audio => {
            components.len() == 2
                && normal(0).is_some_and(|value| {
                    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
                })
                && normal(1).is_some_and(|name| {
                    name.len() >= 8
                        && name.as_bytes().get(6) == Some(&b'-')
                        && name.as_bytes()[..6].iter().all(u8::is_ascii_digit)
                        && ["wav", "mp3", "m4a"]
                            .iter()
                            .any(|extension| name.ends_with(&format!(".{extension}")))
                })
        }
        OwnedArchivePathKind::Attachment => {
            components.len() == 3
                && normal(0) == Some(day.as_str())
                && normal(1) == Some(format!("{time}-attachments").as_str())
                && normal(2).is_some_and(|name| !name.starts_with('.') && name.ends_with(".md"))
        }
    };
    if !valid {
        return Err(ArchiveError::verification());
    }

    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return Err(ArchiveError::verification());
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(ArchiveError::verification());
                }
                let resolved =
                    fs::canonicalize(&current).map_err(|_| ArchiveError::verification())?;
                if !resolved.starts_with(root) {
                    return Err(ArchiveError::verification());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(ArchiveError::verification()),
        }
    }
    Ok(path)
}

fn is_friendly_archive_audio(slot: DateTime<FixedOffset>, relative: &str) -> bool {
    Path::new(relative).parent().and_then(Path::to_str)
        == Some(slot.format("%Y-%m-%d").to_string().as_str())
        && Path::new(relative)
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with(&format!("{}-", slot.format("%H%M%S"))))
}

fn attachment_content_hash(relative: &str) -> Option<&str> {
    let filename = Path::new(relative).file_stem()?.to_str()?;
    let (_, digest) = filename.rsplit_once('-')?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

fn validate_attachment_content_identity(relative: &str, bytes: &[u8]) -> Result<(), ArchiveError> {
    let Some(expected) = attachment_content_hash(relative) else {
        return Ok(());
    };
    if sha256_hex(bytes) != expected {
        return Err(ArchiveError::verification());
    }
    Ok(())
}

fn require_attachment_content_identity(relative: &str, bytes: &[u8]) -> Result<(), ArchiveError> {
    if attachment_content_hash(relative).is_none() {
        return Err(ArchiveError::verification());
    }
    validate_attachment_content_identity(relative, bytes)
}

fn manifest_references_any_path(manifest: &BTreeMap<String, Value>, relative: &str) -> bool {
    manifest.values().any(|entry| {
        ["note", "audio"]
            .iter()
            .any(|field| entry.get(field).and_then(Value::as_str) == Some(relative))
            || entry
                .get("attachments")
                .and_then(Value::as_array)
                .is_some_and(|attachments| {
                    attachments
                        .iter()
                        .any(|value| value.as_str() == Some(relative))
                })
    })
}

fn cleanup_topic_links(root: &Path, deleted: &[String]) -> Result<(), ArchiveError> {
    let views = root.join("by-topic");
    if !views.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(&views).map_err(|_| ArchiveError::verification())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArchiveError::verification());
    }
    let deleted: std::collections::HashSet<_> = deleted
        .iter()
        .filter_map(|relative| safe_archive_path(root, relative).ok())
        .collect();
    let mut directories = Vec::new();
    let mut pending = vec![views];
    while let Some(directory) = pending.pop() {
        directories.push(directory.clone());
        for entry in fs::read_dir(&directory).map_err(|_| ArchiveError::verification())? {
            let entry = entry.map_err(|_| ArchiveError::verification())?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|_| ArchiveError::verification())?;
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).map_err(|_| ArchiveError::verification())?;
                let target = if target.is_absolute() {
                    target
                } else {
                    path.parent()
                        .ok_or_else(ArchiveError::verification)?
                        .join(target)
                };
                if target
                    .canonicalize()
                    .ok()
                    .is_some_and(|target| deleted.contains(&target))
                {
                    fs::remove_file(path).map_err(|_| ArchiveError::verification())?;
                }
            } else if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        if directory
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_none())
        {
            fs::remove_dir(directory).map_err(|_| ArchiveError::verification())?;
        }
    }
    Ok(())
}

fn refresh_daily_rollup(
    root: &Path,
    day: &str,
    manifest: &BTreeMap<String, Value>,
) -> Result<(), ArchiveError> {
    let day_path = safe_archive_path(root, day)?;
    if !day_path.exists() {
        return Ok(());
    }
    let has_recordings = manifest.keys().any(|key| key.starts_with(day));
    for generated in ["daily.md", "daily.html"] {
        let path = day_path.join(generated);
        if path.exists() {
            fs::remove_file(path).map_err(|_| ArchiveError::verification())?;
        }
    }
    if !has_recordings {
        if day_path
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_none())
        {
            fs::remove_dir(day_path).map_err(|_| ArchiveError::verification())?;
        }
        return Ok(());
    }
    let mut notes = Vec::new();
    for entry in manifest.values() {
        let Some(relative) = entry
            .get("note")
            .and_then(Value::as_str)
            .filter(|relative| relative.starts_with(&format!("{day}/")))
        else {
            continue;
        };
        notes.push(read_bounded_text(
            &safe_archive_path(root, relative)?,
            MAX_NOTE_BYTES as u64,
        )?);
    }
    let mut parts = vec![
        format!("# {day}"),
        String::new(),
        format!("_{} recording(s)_", notes.len()),
        String::new(),
    ];
    for (index, note) in notes.iter().enumerate() {
        if index > 0 {
            parts.push("---".to_owned());
            parts.push(String::new());
        }
        parts.push(note.trim_end().to_owned());
        parts.push(String::new());
    }
    let daily = parts.join("\n");
    atomic_write(&day_path.join("daily.md"), daily.as_bytes())?;
    atomic_write(
        &day_path.join("daily.html"),
        daily_markdown_to_html(&daily, day).as_bytes(),
    )
}

fn refresh_all_daily_rollups(
    root: &Path,
    manifest: &BTreeMap<String, Value>,
) -> Result<(), ArchiveError> {
    let days: std::collections::BTreeSet<_> = manifest
        .keys()
        .filter_map(|key| key.get(..10).map(str::to_owned))
        .collect();
    for day in days {
        refresh_daily_rollup(root, &day, manifest)?;
    }
    Ok(())
}

fn daily_markdown_to_html(markdown: &str, title: &str) -> String {
    let body = daily_markdown_body(markdown);
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n<style>\n  body {{ font-family: -apple-system, BlinkMacSystemFont, \"Segoe UI\", sans-serif; max-width: 740px; margin: 2rem auto; padding: 0 1rem; line-height: 1.6; color: #1a1a1a; }}\n  h1, h2, h3 {{ line-height: 1.25; }}\n  h1 {{ font-size: 1.8rem; border-bottom: 1px solid #d0d7de; padding-bottom: 0.3rem; }}\n  h2 {{ font-size: 1.3rem; margin-top: 1.8rem; }}\n  hr {{ border: 0; border-top: 1px solid #d0d7de; margin: 2rem 0; }}\n  code {{ background: #f6f8fa; padding: 0.1em 0.3em; border-radius: 3px; font-size: 0.9em; }}\n  ul.todo {{ list-style: none; padding-left: 1rem; }}\n  ul.todo li {{ margin: 0.25rem 0; }}\n  .transcript {{ white-space: pre-wrap; font-family: ui-monospace, \"SF Mono\", Menlo, monospace; font-size: 0.85rem; background: #f6f8fa; padding: 1rem; border-radius: 6px; }}\n</style>\n</head>\n<body>\n{body}\n</body>\n</html>\n",
        html_escape(title)
    )
}

fn daily_markdown_body(markdown: &str) -> String {
    let mut output = Vec::new();
    let mut list: Option<&str> = None;
    let mut transcript = false;
    let mut transcript_lines = Vec::new();

    let close_list = |output: &mut Vec<String>, list: &mut Option<&str>| {
        if list.take().is_some() {
            output.push("</ul>".to_owned());
        }
    };
    let flush_transcript =
        |output: &mut Vec<String>, transcript: &mut bool, lines: &mut Vec<String>| {
            if *transcript {
                output.push(format!(
                    "<div class=\"transcript\">{}</div>",
                    html_escape(lines.join("\n").trim())
                ));
                *transcript = false;
                lines.clear();
            }
        };

    for line in markdown.split('\n') {
        let value = line.trim_end();
        if transcript {
            if value.starts_with('#') || value.starts_with("---") {
                flush_transcript(&mut output, &mut transcript, &mut transcript_lines);
            } else if value.trim() == "```" {
                continue;
            } else {
                transcript_lines.push(value.to_owned());
                continue;
            }
        }
        if value.trim().is_empty() {
            close_list(&mut output, &mut list);
            continue;
        }
        if value.starts_with("## Transcript") {
            close_list(&mut output, &mut list);
            output.push("<h2>Transcript</h2>".to_owned());
            transcript = true;
        } else if let Some(value) = value.strip_prefix("### ") {
            close_list(&mut output, &mut list);
            output.push(format!("<h3>{}</h3>", daily_inline(value)));
        } else if let Some(value) = value.strip_prefix("## ") {
            close_list(&mut output, &mut list);
            output.push(format!("<h2>{}</h2>", daily_inline(value)));
        } else if let Some(value) = value.strip_prefix("# ") {
            close_list(&mut output, &mut list);
            output.push(format!("<h1>{}</h1>", daily_inline(value)));
        } else if value == "---" {
            close_list(&mut output, &mut list);
            output.push("<hr>".to_owned());
        } else if let Some(value) = value.trim_start().strip_prefix("- [ ] ") {
            if list != Some("todo") {
                close_list(&mut output, &mut list);
                output.push("<ul class=\"todo\">".to_owned());
                list = Some("todo");
            }
            output.push(format!(
                "<li><input type=\"checkbox\" disabled> {}</li>",
                daily_inline(value)
            ));
        } else if let Some(value) = value.trim_start().strip_prefix("- ") {
            if list != Some("list") {
                close_list(&mut output, &mut list);
                output.push("<ul>".to_owned());
                list = Some("list");
            }
            output.push(format!("<li>{}</li>", daily_inline(value)));
        } else {
            close_list(&mut output, &mut list);
            output.push(format!("<p>{}</p>", daily_inline(value)));
        }
    }
    flush_transcript(&mut output, &mut transcript, &mut transcript_lines);
    close_list(&mut output, &mut list);
    output.join("\n")
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn daily_inline(value: &str) -> String {
    let mut value = html_escape(value);
    for (delimiter, open, close) in [
        ("**", "<strong>", "</strong>"),
        ("`", "<code>", "</code>"),
        ("_", "<em>", "</em>"),
    ] {
        let mut output = String::new();
        let mut remaining = value.as_str();
        while let Some(start) = remaining.find(delimiter) {
            let after = &remaining[start + delimiter.len()..];
            let Some(end) = after.find(delimiter) else {
                break;
            };
            output.push_str(&remaining[..start]);
            output.push_str(open);
            output.push_str(&after[..end]);
            output.push_str(close);
            remaining = &after[end + delimiter.len()..];
        }
        output.push_str(remaining);
        value = output;
    }
    value
}

fn rebuild_topic_views(
    root: &Path,
    manifest: &BTreeMap<String, Value>,
) -> Result<(), ArchiveError> {
    let views = root.join("by-topic");
    if views.exists() {
        clear_generated_directory(&views)?;
    }
    if manifest.is_empty() {
        return Ok(());
    }
    fs::create_dir(&views).map_err(|_| ArchiveError::verification())?;
    for (key, entry) in manifest {
        let category = clean_category(entry.get("category").and_then(Value::as_str));
        let category_directory = views.join(category);
        if !category_directory.exists() {
            fs::create_dir(&category_directory).map_err(|_| ArchiveError::verification())?;
        }
        let title = entry.get("title").and_then(Value::as_str).unwrap_or("note");
        for field in ["note", "audio"] {
            let Some(relative) = entry.get(field).and_then(Value::as_str) else {
                continue;
            };
            let target = safe_archive_path(root, relative)?;
            if !target.is_file() {
                continue;
            }
            let extension = target
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            let mut link =
                category_directory.join(format!("{}.{}", safe_topic_filename(title), extension));
            if fs::symlink_metadata(&link).is_ok() {
                link = category_directory.join(format!(
                    "{}-{}.{}",
                    safe_topic_filename(title),
                    key.split_whitespace().nth(1).unwrap_or("000000"),
                    extension
                ));
            }
            if fs::symlink_metadata(&link).is_ok() {
                continue;
            }
            let target = Path::new("../..").join(relative);
            create_archive_symlink(&target, &link)?;
        }
    }
    sync_directory(root)
}

fn clear_generated_directory(path: &Path) -> Result<(), ArchiveError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ArchiveError::verification())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArchiveError::verification());
    }
    for entry in fs::read_dir(path).map_err(|_| ArchiveError::verification())? {
        let entry = entry.map_err(|_| ArchiveError::verification())?;
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child).map_err(|_| ArchiveError::verification())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            clear_generated_directory(&child)?;
        } else {
            fs::remove_file(child).map_err(|_| ArchiveError::verification())?;
        }
    }
    fs::remove_dir(path).map_err(|_| ArchiveError::verification())
}

fn safe_topic_filename(value: &str) -> String {
    let mut output = String::new();
    let mut dash = false;
    let mut space = false;
    for character in value.chars() {
        let allowed = character.is_alphanumeric()
            || matches!(
                character,
                '一'..='鿿' | '《' | '》' | '「' | '」' | '【' | '】' | '·' | '(' | ')' | '.' | '_'
            );
        if allowed {
            if dash && !output.ends_with('-') {
                output.push('-');
            }
            if space && !output.is_empty() && !output.ends_with(' ') {
                output.push(' ');
            }
            dash = false;
            space = false;
            output.push(character);
        } else if character.is_whitespace() {
            space = true;
        } else {
            dash = true;
        }
        if output.chars().count() >= 80 {
            break;
        }
    }
    let output = output.trim_matches([' ', '-', '.']).to_owned();
    if output.is_empty() {
        "note".to_owned()
    } else {
        output
    }
}

#[cfg(unix)]
fn create_archive_symlink(target: &Path, link: &Path) -> Result<(), ArchiveError> {
    std::os::unix::fs::symlink(target, link).map_err(|_| ArchiveError::verification())
}

#[cfg(windows)]
fn create_archive_symlink(target: &Path, link: &Path) -> Result<(), ArchiveError> {
    std::os::windows::fs::symlink_file(target, link).map_err(|_| ArchiveError::verification())
}

pub(crate) fn apply_speaker_labels(root: &Path, keys: &[String]) -> Result<(), ArchiveError> {
    if keys.len() > 100 {
        return Err(ArchiveError::verification());
    }
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    let mut manifest = load_manifest(&root)?;
    let mut changed = false;
    for key in keys {
        let Some(entry) = manifest.get_mut(key).and_then(Value::as_object_mut) else {
            return Err(ArchiveError::verification());
        };
        let Some(note_relative) = entry.get("note").and_then(Value::as_str) else {
            continue;
        };
        let note_path = safe_archive_path(&root, note_relative)?;
        let mut note = read_bounded_text(&note_path, MAX_NOTE_BYTES as u64)?;
        let desired = speaker_map(entry.get("speakers"))?;
        let mut applied = speaker_map(entry.get("speakers_applied"))?;
        let labels = timestamped_labels(&note);
        if !applied.is_empty()
            && !applied
                .values()
                .any(|name| labels.iter().any(|label| *label == name))
        {
            applied.clear();
        }
        if desired == applied {
            continue;
        }

        let mut replacements = Vec::new();
        let slots: std::collections::BTreeSet<_> = desired.keys().chain(applied.keys()).collect();
        for slot in slots {
            let old = applied.get(slot).map(String::as_str).unwrap_or(slot);
            let new = desired.get(slot).map(String::as_str).unwrap_or(slot);
            if old == new
                || desired
                    .iter()
                    .any(|(other_slot, name)| other_slot != slot && name == old)
            {
                continue;
            }
            replacements.push((old.to_owned(), new.to_owned()));
        }
        let had_trailing_newline = note.ends_with('\n');
        note = note
            .lines()
            .map(|line| rewrite_timestamped_label(line, &replacements))
            .collect::<Vec<_>>()
            .join("\n");
        if had_trailing_newline {
            note.push('\n');
        }
        atomic_write(&note_path, note.as_bytes())?;

        let labels = timestamped_labels(&note);
        let new_applied: Map<String, Value> = desired
            .iter()
            .filter(|(_, name)| labels.iter().any(|label| *label == name.as_str()))
            .map(|(slot, name)| (slot.clone(), Value::String(name.clone())))
            .collect();
        if new_applied.is_empty() {
            entry.remove("speakers_applied");
        } else {
            entry.insert("speakers_applied".to_owned(), Value::Object(new_applied));
        }
        changed = true;
    }
    if changed {
        atomic_write(&root.join("manifest.json"), &serialize_manifest(&manifest)?)?;
    }
    rebuild_viewer(&root, &manifest)?;
    Ok(())
}

fn speaker_map(value: Option<&Value>) -> Result<BTreeMap<String, String>, ArchiveError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(ArchiveError::verification)?;
    if object.len() > 64 {
        return Err(ArchiveError::verification());
    }
    object
        .iter()
        .map(|(slot, name)| {
            let name = name.as_str().ok_or_else(ArchiveError::verification)?;
            if slot.is_empty()
                || slot.len() > 32
                || !slot
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || name.is_empty()
                || name.len() > 60
                || name.chars().any(char::is_control)
            {
                return Err(ArchiveError::verification());
            }
            Ok((slot.clone(), name.to_owned()))
        })
        .collect()
}

fn timestamped_labels(note: &str) -> std::collections::HashSet<&str> {
    note.lines()
        .filter_map(timestamped_label_parts)
        .map(|(_, label, _)| label)
        .collect()
}

fn timestamped_label_parts(line: &str) -> Option<(&str, &str, &str)> {
    if !line.starts_with('[') {
        return None;
    }
    let close = line.find(']')?;
    let prefix = &line[..=close];
    let after = line[close + 1..].trim_start();
    let colon = after.find([':', '：'])?;
    let label = after[..colon].trim();
    if label.is_empty() {
        return None;
    }
    let separator_bytes = after[colon..].chars().next()?.len_utf8();
    Some((prefix, label, &after[colon + separator_bytes..]))
}

fn rewrite_timestamped_label(line: &str, replacements: &[(String, String)]) -> String {
    let Some((prefix, label, suffix)) = timestamped_label_parts(line) else {
        return line.to_owned();
    };
    let Some((_, replacement)) = replacements.iter().find(|(old, _)| old == label) else {
        return line.to_owned();
    };
    format!("{prefix} {replacement}:{suffix}")
}

fn note_sections(note: &str) -> HashMap<String, String> {
    let mut sections = HashMap::new();
    let mut current = "_head".to_owned();
    let mut lines = Vec::new();
    for line in note.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            sections.insert(current, lines.join("\n").trim().to_owned());
            current = header.trim().to_owned();
            lines.clear();
        } else {
            lines.push(line);
        }
    }
    sections.insert(current, lines.join("\n").trim().to_owned());
    sections
}

fn load_attachments(root: &Path, value: Option<&Value>) -> Result<Vec<Value>, ArchiveError> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if values.len() > 1_000 {
        return Err(ArchiveError::verification());
    }
    values
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|relative| {
            let path = safe_archive_path(root, relative).ok()?;
            if !path.is_file() {
                return None;
            }
            let content = read_bounded_text(&path, MAX_ATTACHMENT_BYTES).ok()?;
            Some(json!({
                "name": path.file_stem().and_then(|value| value.to_str()).unwrap_or("attachment"),
                "rel": relative,
                "content": content,
            }))
        })
        .collect::<Vec<_>>()
        .pipe(Ok)
}

trait Pipe: Sized {
    fn pipe<T>(self, operation: impl FnOnce(Self) -> T) -> T {
        operation(self)
    }
}

impl<T> Pipe for T {}

fn load_optional_json(root: &Path, relative: &str) -> Result<Option<Value>, ArchiveError> {
    let path = safe_archive_path(root, relative)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = read_bounded_text(&path, MAX_LOCAL_JSON_BYTES)?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|_| ArchiveError::verification())
}

fn read_bounded_text(path: &Path, limit: u64) -> Result<String, ArchiveError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ArchiveError::verification())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit {
        return Err(ArchiveError::verification());
    }
    fs::read_to_string(path).map_err(|_| ArchiveError::verification())
}

fn weekday(day: &str) -> &'static str {
    use chrono::Datelike;
    let weekdays = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .ok()
        .map(|date| weekdays[date.weekday().num_days_from_monday() as usize])
        .unwrap_or("")
}

fn contains_cjk(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character, '一'..='鿿'))
}

fn atomic_copy_verified(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
    expected_size: u64,
) -> Result<(), ArchiveError> {
    if destination.exists() {
        let (sha256, size) = hash_file(destination)?;
        if sha256 == expected_sha256 && size == expected_size {
            return Ok(());
        }
    }
    let parent = destination
        .parent()
        .ok_or_else(ArchiveError::verification)?;
    fs::create_dir_all(parent).map_err(|_| ArchiveError::verification())?;
    let temporary = parent.join(format!(".archive-audio-{}.tmp", Uuid::new_v4()));
    fs::copy(source, &temporary).map_err(|_| ArchiveError::verification())?;
    let (sha256, size) = hash_file(&temporary)?;
    if sha256 != expected_sha256 || size != expected_size {
        let _ = fs::remove_file(&temporary);
        return Err(ArchiveError::verification());
    }
    replace_file(&temporary, destination)?;
    sync_directory(parent)
}

fn atomic_write(destination: &Path, bytes: &[u8]) -> Result<(), ArchiveError> {
    let parent = destination
        .parent()
        .ok_or_else(ArchiveError::verification)?;
    fs::create_dir_all(parent).map_err(|_| ArchiveError::verification())?;
    let temporary = parent.join(format!(".archive-write-{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| ArchiveError::verification())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| ArchiveError::verification())?;
    drop(file);
    replace_file(&temporary, destination)?;
    sync_directory(parent)
}

fn hash_file(path: &Path) -> Result<(String, u64), ArchiveError> {
    let mut file = File::open(path).map_err(|_| ArchiveError::verification())?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ArchiveError::verification())?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(read as u64);
        digest.update(&buffer[..read]);
    }
    Ok((hex::encode(digest.finalize()), size))
}

fn git_blob_sha(bytes: &[u8]) -> String {
    let mut digest = Sha1::default();
    sha1::Digest::update(&mut digest, format!("blob {}\0", bytes.len()).as_bytes());
    sha1::Digest::update(&mut digest, bytes);
    hex::encode(sha1::Digest::finalize(digest))
}

fn local_matches_git_blob(
    path: &Path,
    limit: u64,
    expected_sha: &str,
) -> Result<bool, ArchiveError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(ArchiveError::verification()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit {
        return Err(ArchiveError::verification());
    }
    let bytes = fs::read(path).map_err(|_| ArchiveError::verification())?;
    Ok(git_blob_sha(&bytes) == expected_sha)
}

fn sha256_hex(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn base64(value: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(value.len().div_ceil(3) * 4);
    for chunk in value.chunks(3) {
        let packed = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(TABLE[((packed >> 18) & 63) as usize] as char);
        output.push(TABLE[((packed >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((packed >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(packed & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn decode_base64(value: &str) -> Result<Vec<u8>, ArchiveError> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let compact: Vec<u8> = value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if compact.is_empty() {
        return Ok(Vec::new());
    }
    if !compact.len().is_multiple_of(4) {
        return Err(ArchiveError::verification());
    }
    let mut output = Vec::with_capacity(compact.len() / 4 * 3);
    for (index, chunk) in compact.chunks_exact(4).enumerate() {
        let final_chunk = index + 1 == compact.len() / 4;
        let padding = usize::from(chunk[3] == b'=') + usize::from(chunk[2] == b'=');
        if padding > 2 || (!final_chunk && padding != 0) || (chunk[2] == b'=' && chunk[3] != b'=') {
            return Err(ArchiveError::verification());
        }
        let a = digit(chunk[0]).ok_or_else(ArchiveError::verification)?;
        let b = digit(chunk[1]).ok_or_else(ArchiveError::verification)?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            digit(chunk[2]).ok_or_else(ArchiveError::verification)?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            digit(chunk[3]).ok_or_else(ArchiveError::verification)?
        };
        let packed =
            (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6) | u32::from(d);
        output.push((packed >> 16) as u8);
        if padding < 2 {
            output.push((packed >> 8) as u8);
        }
        if padding == 0 {
            output.push(packed as u8);
        }
    }
    Ok(output)
}

fn json_pointer_text(value: &Value, pointer: &str) -> Result<String, ArchiveError> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map(str::to_owned)
        .ok_or_else(ArchiveError::verification)
}

fn classify_github_status(status: u16) -> ArchiveError {
    match status {
        409 | 422 => ArchiveError::conflict(),
        401 | 403 | 404 => ArchiveError::configuration(),
        500..=599 => ArchiveError::network(),
        _ => ArchiveError::verification(),
    }
}

fn map_archive_error(error: ArchiveError) -> EffectError {
    let kind = match error.kind {
        ArchiveErrorKind::Network => EffectErrorKind::Temporary,
        ArchiveErrorKind::Conflict => EffectErrorKind::PublicationConflict,
        ArchiveErrorKind::Configuration | ArchiveErrorKind::Verification => {
            EffectErrorKind::Verification
        }
    };
    EffectError::new(kind)
}

fn map_r2_error(error: r2::R2ArchiveError) -> ArchiveError {
    match error.kind {
        r2::R2ArchiveErrorKind::Configuration => ArchiveError::configuration(),
        r2::R2ArchiveErrorKind::Network => ArchiveError::network(),
        r2::R2ArchiveErrorKind::Conflict => ArchiveError::conflict(),
        r2::R2ArchiveErrorKind::Verification => ArchiveError::verification(),
    }
}

fn archive_lock(root: &Path) -> Result<Arc<ArchiveOperationLock>, ArchiveError> {
    let registry = ARCHIVE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry.lock().map_err(|_| ArchiveError::verification())?;
    if let Some(lock) = registry.get(root).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(ArchiveOperationLock::new(()));
    registry.insert(root.to_path_buf(), Arc::downgrade(&lock));
    Ok(lock)
}

fn open_archive_process_lock(path: &Path) -> Result<File, ArchiveError> {
    let parent = path.parent().ok_or_else(ArchiveError::configuration)?;
    fs::create_dir_all(parent).map_err(|_| ArchiveError::verification())?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(ArchiveError::verification());
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| ArchiveError::verification())
}

fn archive_process_lock_path(root: &Path) -> Result<PathBuf, ArchiveError> {
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    let parent = root.parent().ok_or_else(ArchiveError::configuration)?;
    Ok(parent.join(".echowall-runtime/archive-publisher.lock"))
}

fn open_native_edit_outbox(root: &Path) -> Result<PathBuf, ArchiveError> {
    open_runtime_subdirectory(root, "native-edits")
}

fn open_runtime_subdirectory(root: &Path, name: &str) -> Result<PathBuf, ArchiveError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(ArchiveError::configuration());
    }
    let root = fs::canonicalize(root).map_err(|_| ArchiveError::verification())?;
    let parent = root.parent().ok_or_else(ArchiveError::configuration)?;
    let runtime = parent.join(".echowall-runtime");
    let target = runtime.join(name);
    for directory in [&runtime, &target] {
        match fs::symlink_metadata(directory) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ArchiveError::verification());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(directory).map_err(|_| ArchiveError::verification())?;
                sync_directory(directory.parent().ok_or_else(ArchiveError::verification)?)?;
            }
            Err(_) => return Err(ArchiveError::verification()),
        }
        let resolved = fs::canonicalize(directory).map_err(|_| ArchiveError::verification())?;
        if !resolved.starts_with(parent) {
            return Err(ArchiveError::verification());
        }
    }
    fs::canonicalize(target).map_err(|_| ArchiveError::verification())
}

fn load_native_edit_tickets(outbox: &Path) -> Result<Vec<NativeEditTicket>, ArchiveError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(outbox).map_err(|_| ArchiveError::verification())? {
        let entry = entry.map_err(|_| ArchiveError::verification())?;
        let path = entry.path();
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(ArchiveError::verification)?
            .to_owned();
        if name.starts_with(".archive-write-") && name.ends_with(".tmp") {
            continue;
        }
        if let Some(stem) = name.strip_suffix(".conflict.json") {
            parse_native_edit_filename(stem)?;
            continue;
        }
        let Some(stem) = name.strip_suffix(".json") else {
            return Err(ArchiveError::verification());
        };
        let (sequence, edit_id) = parse_native_edit_filename(stem)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| ArchiveError::verification())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_NATIVE_EDIT_OUTBOX_BYTES
        {
            return Err(ArchiveError::verification());
        }
        let record: NativeEditOutboxRecord =
            serde_json::from_reader(File::open(&path).map_err(|_| ArchiveError::verification())?)
                .map_err(|_| ArchiveError::verification())?;
        if record.schema_version != NATIVE_EDIT_OUTBOX_SCHEMA
            || record.edit_id != edit_id
            || record.sequence != sequence
        {
            return Err(ArchiveError::verification());
        }
        paths.push(NativeEditTicket { path, record });
        if paths.len() > MAX_PENDING_NATIVE_EDITS {
            return Err(ArchiveError::verification());
        }
    }
    paths.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(paths)
}

fn pending_deleted_recording_ids(
    archive_root: &Path,
) -> Result<std::collections::BTreeSet<String>, ArchiveError> {
    let outbox = open_native_edit_outbox(archive_root)?;
    let mut recording_ids = std::collections::BTreeSet::new();
    for ticket in load_native_edit_tickets(&outbox)? {
        if let NativeArchiveEdit::Delete {
            recording_id: Some(recording_id),
            ..
        } = ticket.record.edit
        {
            recording_ids.insert(recording_id);
        }
    }
    Ok(recording_ids)
}

fn parse_native_edit_filename(stem: &str) -> Result<(u64, Uuid), ArchiveError> {
    let (sequence, edit_id) = stem
        .split_once('-')
        .ok_or_else(ArchiveError::verification)?;
    if sequence.len() != 20 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ArchiveError::verification());
    }
    let sequence = sequence
        .parse::<u64>()
        .map_err(|_| ArchiveError::verification())?;
    let edit_id = Uuid::parse_str(edit_id).map_err(|_| ArchiveError::verification())?;
    Ok((sequence, edit_id))
}

fn next_native_edit_sequence(outbox: &Path) -> Result<u64, ArchiveError> {
    let mut maximum = 0u64;
    let mut count = 0usize;
    for entry in fs::read_dir(outbox).map_err(|_| ArchiveError::verification())? {
        let entry = entry.map_err(|_| ArchiveError::verification())?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(ArchiveError::verification)?
            .to_owned();
        if name.starts_with(".archive-write-") && name.ends_with(".tmp") {
            continue;
        }
        let stem = name
            .strip_suffix(".conflict.json")
            .or_else(|| name.strip_suffix(".json"))
            .ok_or_else(ArchiveError::verification)?;
        let (sequence, _) = parse_native_edit_filename(stem)?;
        maximum = maximum.max(sequence);
        count += 1;
        if count >= MAX_PENDING_NATIVE_EDITS {
            return Err(ArchiveError::verification());
        }
    }
    maximum
        .checked_add(1)
        .ok_or_else(ArchiveError::verification)
}

fn quarantine_native_edit_ticket(
    outbox: &Path,
    ticket: &NativeEditTicket,
) -> Result<(), ArchiveError> {
    if ticket.path.parent() != Some(outbox) {
        return Err(ArchiveError::verification());
    }
    let name = ticket
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_suffix(".json"))
        .ok_or_else(ArchiveError::verification)?;
    let conflict = outbox.join(format!("{name}.conflict.json"));
    if conflict.exists() {
        return Err(ArchiveError::verification());
    }
    fs::rename(&ticket.path, conflict).map_err(|_| ArchiveError::verification())?;
    sync_directory(outbox)
}

pub(crate) struct ArchiveMutationGuard {
    file: File,
}

pub(crate) struct NativeArchiveTransaction {
    archive_root: PathBuf,
    _operation: tokio::sync::OwnedMutexGuard<()>,
    _file: ArchiveMutationGuard,
}

pub(crate) async fn begin_native_archive_transaction(
    archive_root: &Path,
) -> Result<NativeArchiveTransaction, ArchiveError> {
    let archive_root = fs::canonicalize(archive_root).map_err(|_| ArchiveError::verification())?;
    let operation = archive_lock(&archive_root)?.lock_owned().await;
    let file = try_archive_mutation_lock(&archive_root)?;
    Ok(NativeArchiveTransaction {
        archive_root,
        _operation: operation,
        _file: file,
    })
}

pub(crate) fn try_archive_mutation_lock(
    archive_root: &Path,
) -> Result<ArchiveMutationGuard, ArchiveError> {
    let path = archive_process_lock_path(archive_root)?;
    let file = open_archive_process_lock(&path)?;
    file.try_lock_exclusive().map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            ArchiveError::conflict()
        } else {
            ArchiveError::verification()
        }
    })?;
    Ok(ArchiveMutationGuard { file })
}

impl Drop for ArchiveMutationGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

struct ArchiveFileLockGuard<'a> {
    file: &'a File,
}

impl<'a> ArchiveFileLockGuard<'a> {
    fn acquire(file: &'a File) -> Result<Self, ArchiveError> {
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                ArchiveError::network()
            } else {
                ArchiveError::verification()
            }
        })?;
        Ok(Self { file })
    }
}

impl Drop for ArchiveFileLockGuard<'_> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(self.file);
    }
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), ArchiveError> {
    fs::rename(source, destination).map_err(|_| ArchiveError::verification())
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> Result<(), ArchiveError> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(ArchiveError::verification())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn sync_directory(path: &Path) -> Result<(), ArchiveError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ArchiveError::verification())
}

#[cfg(target_os = "windows")]
fn sync_directory(_: &Path) -> Result<(), ArchiveError> {
    Ok(())
}

#[cfg(test)]
mod tests;
