//! Mobile sync core: pull the private notes repo as a GitHub tarball into the
//! app-sandbox data dir, stream/pin audio from R2, and expose the /api/sync/*
//! surface the setup page and viewer talk to.
//!
//! State machine (surfaced via GET /api/sync/status):
//!   no-tokens -> syncing -> ok | error | offline | unauthorized
//! Errors keep stale data usable; `last_sync` persists across launches in
//! sync_state.json (non-secret). Tokens live only in secrets.rs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tower::ServiceExt;
use tower_http::services::ServeDir;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{r2, secrets};

const LEGACY_DEFAULT_REPO: &str = "xingfanxia/watch-transcriber-data";
const LEGACY_DEFAULT_BUCKET: &str = "watch-transcriber-audio";
const VIEWER_CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self' ipc: http://ipc.localhost; media-src 'self'; img-src 'self' data:; font-src 'self'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'self'; worker-src 'none'";
const TRUSTED_MARKED_JS: &[u8] = include_bytes!("../../../deliveries/vendor/marked.min.js");

#[derive(Clone)]
pub struct SyncRuntimeState(pub Arc<SyncCtx>);

/// Write-only native setup request. It intentionally has no `Serialize`
/// implementation, so credentials cannot be returned to the webview.
#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveSyncSetup {
    github_pat: String,
    r2_account_id: String,
    r2_access_key_id: String,
    r2_secret_access_key: String,
    repo: String,
    bucket: String,
}

impl std::fmt::Debug for ArchiveSyncSetup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ArchiveSyncSetup(<redacted>)")
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveSyncSetupError {
    field: &'static str,
    error: &'static str,
}

impl ArchiveSyncSetupError {
    fn new(field: &'static str, error: &'static str) -> Self {
        Self { field, error }
    }
}

#[derive(Clone)]
pub struct GhCfg {
    pub pat: String,
    pub repo: String,
}

pub struct SyncCtx {
    pub data: PathBuf,
    pub base: PathBuf,
    pub cache: PathBuf,
    pub gh: RwLock<Option<GhCfg>>,
    pub r2: RwLock<Option<r2::R2Cfg>>,
    pub state: RwLock<String>,
    pub error: RwLock<Option<String>>,
    pub last_sync: RwLock<Option<String>>,
    pub syncing: AtomicBool,
    credential_generation: AtomicU64,
    pub inflight: Arc<Mutex<HashSet<String>>>,
}

impl SyncCtx {
    /// Build the context and hydrate tokens + persisted state.
    pub fn new(data: PathBuf) -> Arc<Self> {
        let base = data.parent().unwrap_or(Path::new(".")).to_path_buf();
        let cache = base.join("audio-cache");
        let ctx = SyncCtx {
            data,
            cache,
            gh: RwLock::new(None),
            r2: RwLock::new(None),
            state: RwLock::new("no-tokens".into()),
            error: RwLock::new(None),
            last_sync: RwLock::new(None),
            syncing: AtomicBool::new(false),
            credential_generation: AtomicU64::new(1),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            base,
        };
        if let Some(t) = load_bound_credentials(&ctx.base) {
            *ctx.gh.write().unwrap() = Some(GhCfg {
                pat: t.github_pat.clone(),
                repo: t.repo.clone(),
            });
            *ctx.r2.write().unwrap() = Some(r2::R2Cfg {
                account_id: t.r2_account_id.clone(),
                access_key_id: t.r2_access_key_id.clone(),
                secret_access_key: t.r2_secret_access_key.clone(),
                bucket: t.bucket.clone(),
            });
            *ctx.state.write().unwrap() = "idle".into();
        }
        if let Ok(raw) = std::fs::read_to_string(ctx.base.join("sync_state.json")) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                *ctx.last_sync.write().unwrap() =
                    v.get("last_sync").and_then(Value::as_str).map(String::from);
            }
        }
        Arc::new(ctx)
    }

    fn set_state(&self, state: &str, error: Option<String>) {
        *self.state.write().unwrap() = state.into();
        *self.error.write().unwrap() = error;
    }

    fn install_credentials(&self, tokens: &secrets::SyncTokens, repo: String, bucket: String) {
        self.credential_generation.fetch_add(1, Ordering::SeqCst);
        *self.gh.write().unwrap() = Some(GhCfg {
            pat: tokens.github_pat.clone(),
            repo,
        });
        *self.r2.write().unwrap() = Some(r2::R2Cfg {
            account_id: tokens.r2_account_id.clone(),
            access_key_id: tokens.r2_access_key_id.clone(),
            secret_access_key: tokens.r2_secret_access_key.clone(),
            bucket,
        });
        self.set_state("idle", None);
    }

    fn revoke_credentials(&self) {
        self.credential_generation.fetch_add(1, Ordering::SeqCst);
        *self.gh.write().unwrap() = None;
        *self.r2.write().unwrap() = None;
        self.set_state("no-tokens", None);
    }

    fn recordings(&self) -> usize {
        std::fs::read_to_string(self.data.join("manifest.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.as_object().map(|o| o.len()))
            .unwrap_or(0)
    }
}

/// Map a reqwest failure to a sync state name.
fn classify(e: &reqwest::Error) -> &'static str {
    if e.is_connect() || e.is_timeout() || e.is_request() {
        "offline"
    } else {
        "error"
    }
}

/// Full pull: tarball download -> overlay extract into data/. Safe to call
/// repeatedly; concurrent calls collapse into one.
pub async fn pull(ctx: Arc<SyncCtx>) {
    if ctx.syncing.swap(true, Ordering::SeqCst) {
        return;
    }
    let generation = ctx.credential_generation.load(Ordering::SeqCst);
    let result = pull_inner(&ctx, generation).await;
    if ctx.credential_generation.load(Ordering::SeqCst) != generation {
        ctx.syncing.store(false, Ordering::SeqCst);
        return;
    }
    match result {
        Ok(()) => {
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            *ctx.last_sync.write().unwrap() = Some(now.clone());
            let _ = std::fs::write(
                ctx.base.join("sync_state.json"),
                json!({ "last_sync": now }).to_string(),
            );
            ctx.set_state("ok", None);
        }
        Err((state, msg)) => ctx.set_state(state, Some(msg)),
    }
    ctx.syncing.store(false, Ordering::SeqCst);
}

async fn pull_inner(ctx: &SyncCtx, generation: u64) -> Result<(), (&'static str, String)> {
    let Some(gh) = ctx.gh.read().unwrap().clone() else {
        return Err(("no-tokens", "尚未配置 token".into()));
    };
    ctx.set_state("syncing", None);
    let url = format!("https://api.github.com/repos/{}/tarball/HEAD", gh.repo);
    let mut resp = r2::http()
        .get(&url)
        .header(header::USER_AGENT, "EchoWall")
        .header(header::AUTHORIZATION, format!("Bearer {}", gh.pat))
        .send()
        .await
        .map_err(|e| (classify(&e), e.to_string()))?;
    match resp.status().as_u16() {
        200 => {}
        301 | 302 | 303 | 307 | 308 => {
            let redirect = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| validated_github_tarball_redirect(&gh.repo, value).ok())
                .ok_or(("error", "GitHub tarball redirect was rejected".to_owned()))?;
            // The private-repository redirect carries its own short-lived
            // query authorization. Never forward the GitHub PAT cross-host.
            resp = r2::http()
                .get(redirect)
                .header(header::USER_AGENT, "EchoWall")
                .send()
                .await
                .map_err(|e| (classify(&e), "GitHub archive download failed".to_owned()))?;
            if resp.status().as_u16() != 200 {
                return Err((
                    "error",
                    format!("GitHub archive download HTTP {}", resp.status()),
                ));
            }
        }
        401 | 403 => return Err(("unauthorized", format!("GitHub {}", resp.status()))),
        s => return Err(("error", format!("GitHub tarball HTTP {s}"))),
    }
    const MAX_TARBALL_BYTES: usize = 256 * 1024 * 1024;
    if resp
        .content_length()
        .is_some_and(|size| size > MAX_TARBALL_BYTES as u64)
    {
        return Err(("error", "GitHub archive is too large".to_owned()));
    }
    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| (classify(&error), "GitHub download failed".to_owned()))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_TARBALL_BYTES {
            return Err(("error", "GitHub archive is too large".to_owned()));
        }
        bytes.extend_from_slice(&chunk);
    }
    if ctx.credential_generation.load(Ordering::SeqCst) != generation {
        return Err(("no-tokens", "凭据已撤销".to_owned()));
    }
    let data = ctx.data.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = crate::processing::archive::try_archive_mutation_lock(&data)
            .map_err(|_| "archive is busy".to_owned())?;
        extract_overlay(&bytes, &data)
    })
    .await
    .map_err(|e| ("error", e.to_string()))?
    .map_err(|e| ("error", e))
}

/// Extract a GitHub tarball over the data dir: strip the `owner-repo-sha/`
/// prefix, refuse path escapes, skip VCS files. Overlay only — local pinned
/// audio and cache are never deleted by a pull.
pub fn extract_overlay(tar_gz: &[u8], dest: &Path) -> Result<(), String> {
    let gz = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(gz);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut entries_seen = 0usize;
    let mut total_bytes = 0u64;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        entries_seen += 1;
        if entries_seen > 10_000 {
            return Err("archive has too many entries".to_owned());
        }
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let mut comps = path.components();
        comps.next(); // strip owner-repo-sha/
        let rel: PathBuf = comps.as_path().to_path_buf();
        if rel.as_os_str().is_empty() {
            continue;
        }
        if rel
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || rel.starts_with(".git")
        {
            continue;
        }
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                safe_overlay_target(dest, &rel, true)?;
            }
            tar::EntryType::Regular => {
                let size = entry.header().size().map_err(|_| "bad archive size")?;
                if size > 32 * 1024 * 1024 {
                    return Err("archive entry is too large".to_owned());
                }
                total_bytes = total_bytes.saturating_add(size);
                if total_bytes > 256 * 1024 * 1024 {
                    return Err("archive is too large".to_owned());
                }
                let target = safe_overlay_target(dest, &rel, false)?;
                entry.unpack(&target).map_err(|e| e.to_string())?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn safe_overlay_target(dest: &Path, relative: &Path, directory: bool) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(dest).map_err(|_| "archive root is unavailable")?;
    let components: Vec<_> = relative.components().collect();
    let mut current = root.clone();
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(component) = component else {
            return Err("archive path is invalid".to_owned());
        };
        current.push(component);
        let final_component = index + 1 == components.len();
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || (!final_component && !metadata.is_dir())
                    || (final_component && directory && !metadata.is_dir())
                    || (final_component && !directory && !metadata.is_file())
                {
                    return Err("archive path is unsafe".to_owned());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !final_component || directory {
                    std::fs::create_dir(&current)
                        .map_err(|_| "archive directory could not be created")?;
                }
            }
            Err(_) => return Err("archive path is unavailable".to_owned()),
        }
        let anchor = if current.exists() {
            current.as_path()
        } else {
            current
                .parent()
                .ok_or_else(|| "archive path is invalid".to_owned())?
        };
        if !std::fs::canonicalize(anchor)
            .map_err(|_| "archive path is unavailable")?
            .starts_with(&root)
        {
            return Err("archive path escaped its root".to_owned());
        }
    }
    Ok(current)
}

// ---------------------------------------------------------------- API routes

pub async fn status(State(ctx): State<Arc<SyncCtx>>) -> Json<Value> {
    Json(json!({
        "mode": if cfg!(mobile) { "mobile" } else { "desktop" },
        "state": ctx.state.read().unwrap().clone(),
        "error": ctx.error.read().unwrap().clone(),
        "last_sync": ctx.last_sync.read().unwrap().clone(),
        "recordings": ctx.recordings(),
    }))
}

/// Validate archive credentials, persist them to the platform secure store,
/// and update the in-app sync runtime. This is a Tauri IPC command rather than
/// an HTTP endpoint so secrets never traverse the loopback archive server.
#[tauri::command]
pub async fn save_sync_credentials(
    state: tauri::State<'_, SyncRuntimeState>,
    archive: tauri::State<'_, crate::DesktopArchiveState>,
    setup: ArchiveSyncSetup,
) -> Result<(), ArchiveSyncSetupError> {
    let ctx = Arc::clone(&state.0);
    let repo = setup.repo.clone();
    let bucket = setup.bucket.clone();
    validate_repo(&repo).map_err(|()| ArchiveSyncSetupError::new("github", "仓库名称格式无效"))?;
    validate_bucket(&bucket)
        .map_err(|()| ArchiveSyncSetupError::new("r2", "Bucket 名称格式无效"))?;
    let tokens = secrets::SyncTokens {
        schema_version: 1,
        github_pat: setup.github_pat.clone(),
        r2_account_id: setup.r2_account_id.clone(),
        r2_access_key_id: setup.r2_access_key_id.clone(),
        r2_secret_access_key: setup.r2_secret_access_key.clone(),
        repo: repo.clone(),
        bucket: bucket.clone(),
    };
    secrets::validate_sync_tokens(&tokens)
        .map_err(|_| ArchiveSyncSetupError::new("store", "凭据格式无效"))?;

    let github = r2::http()
        .get(format!("https://api.github.com/repos/{repo}"))
        .header(header::USER_AGENT, "EchoWall")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", tokens.github_pat),
        )
        .send()
        .await
        .map_err(|_| ArchiveSyncSetupError::new("github", "无法连接 GitHub"))?;
    match github.status().as_u16() {
        200 => {}
        401 => {
            return Err(ArchiveSyncSetupError::new("github", "GitHub token 无效"));
        }
        403 | 404 => {
            return Err(ArchiveSyncSetupError::new("github", "GitHub 仓库访问被拒"));
        }
        500..=599 => {
            return Err(ArchiveSyncSetupError::new("github", "GitHub 暂时不可用"));
        }
        _ => {
            return Err(ArchiveSyncSetupError::new("github", "GitHub 凭据验证失败"));
        }
    }

    let r2_config = r2::R2Cfg {
        account_id: tokens.r2_account_id.clone(),
        access_key_id: tokens.r2_access_key_id.clone(),
        secret_access_key: tokens.r2_secret_access_key.clone(),
        bucket: bucket.clone(),
    };
    let r2_response = r2::request(&r2_config, "HEAD", "", None)
        .await
        .map_err(|_| ArchiveSyncSetupError::new("r2", "无法连接 R2"))?;
    if !r2_response.status().is_success() {
        return Err(ArchiveSyncSetupError::new("r2", "R2 凭据或权限无效"));
    }

    let _archive_transaction = archive
        .0
        .begin_transaction()
        .await
        .map_err(|_| ArchiveSyncSetupError::new("store", "归档正在更新，请重试"))?;
    secrets::save(&tokens)
        .map_err(|_| ArchiveSyncSetupError::new("store", "无法写入系统安全存储"))?;
    ctx.install_credentials(&tokens, repo, bucket);
    let pending_archive = Arc::clone(&archive.0);
    tauri::async_runtime::spawn(async move {
        let _ = pending_archive.resume_pending_native_edits().await;
    });
    Ok(())
}

#[tauri::command]
pub async fn delete_sync_credentials(
    state: tauri::State<'_, SyncRuntimeState>,
    archive: tauri::State<'_, crate::DesktopArchiveState>,
) -> Result<(), String> {
    let _archive_transaction = archive
        .0
        .begin_transaction()
        .await
        .map_err(|_| "archive is busy".to_owned())?;
    state.0.revoke_credentials();
    secrets::delete_archive()
}

fn validate_repo(value: &str) -> Result<(), ()> {
    secrets::validate_archive_repo(value).map_err(|_| ())
}

fn validated_github_tarball_redirect(repo: &str, value: &str) -> Result<reqwest::Url, ()> {
    if value.is_empty() || value.len() > 16 * 1024 {
        return Err(());
    }
    validate_repo(repo)?;
    let mut repository = repo.split('/');
    let owner = repository.next().ok_or(())?;
    let name = repository.next().ok_or(())?;
    if repository.next().is_some() {
        return Err(());
    }
    let url = reqwest::Url::parse(value).map_err(|_| ())?;
    let legacy_prefix = format!("/{owner}/{name}/legacy.tar.gz/");
    let current_prefix = format!("/{owner}/{name}/tar.gz/");
    let path = url.path();
    let allowed_path = path
        .strip_prefix(&legacy_prefix)
        .or_else(|| path.strip_prefix(&current_prefix))
        .is_some_and(|tail| {
            !tail.is_empty()
                && tail.len() <= 512
                && tail.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.')
                })
        });
    if url.scheme() != "https"
        || url.host_str() != Some("codeload.github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !allowed_path
    {
        return Err(());
    }
    Ok(url)
}

fn validate_bucket(value: &str) -> Result<(), ()> {
    secrets::validate_archive_bucket(value).map_err(|_| ())
}

fn legacy_sync_config(base: &Path) -> (String, String) {
    let mut repo = LEGACY_DEFAULT_REPO.to_owned();
    let mut bucket = LEGACY_DEFAULT_BUCKET.to_owned();
    if let Ok(raw) = std::fs::read_to_string(base.join("sync_config.json")) {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            if let Some(candidate) = value.get("repo").and_then(Value::as_str) {
                if validate_repo(candidate).is_ok() {
                    repo = candidate.to_owned();
                }
            }
            if let Some(candidate) = value.get("bucket").and_then(Value::as_str) {
                if validate_bucket(candidate).is_ok() {
                    bucket = candidate.to_owned();
                }
            }
        }
    }
    (repo, bucket)
}

fn bind_legacy_destination(
    mut tokens: secrets::SyncTokens,
    base: &Path,
) -> Result<(secrets::SyncTokens, bool), ()> {
    let migration = tokens.repo.is_empty() && tokens.bucket.is_empty();
    if !migration && (tokens.repo.is_empty() || tokens.bucket.is_empty()) {
        return Err(());
    }
    if migration {
        secrets::validate_sync_secret_material(&tokens).map_err(|_| ())?;
        let (repo, bucket) = legacy_sync_config(base);
        tokens.repo = repo;
        tokens.bucket = bucket;
    }
    secrets::validate_sync_tokens(&tokens).map_err(|_| ())?;
    Ok((tokens, migration))
}

pub(crate) fn load_bound_credentials(base: &Path) -> Option<secrets::SyncTokens> {
    let (tokens, migrated) = bind_legacy_destination(secrets::load()?, base).ok()?;
    if migrated {
        secrets::save(&tokens).ok()?;
    }
    Some(tokens)
}

pub async fn refresh(State(ctx): State<Arc<SyncCtx>>) -> Json<Value> {
    tokio::spawn(pull(ctx.clone()));
    Json(json!({ "ok": true }))
}

/// Pinned = the audio file exists inside data/ (survives pulls; excluded from
/// LRU eviction, which only runs on audio-cache/).
pub async fn pins(State(ctx): State<Arc<SyncCtx>>) -> Json<Value> {
    let mut pins: Vec<String> = Vec::new();
    let mut walk = vec![ctx.data.clone()];
    while let Some(dir) = walk.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk.push(p);
            } else if p.extension().is_some_and(is_supported_audio_extension) {
                if let Ok(rel) = p.strip_prefix(&ctx.data) {
                    pins.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    Json(json!({ "pins": pins }))
}

pub async fn set_pin(
    State(ctx): State<Arc<SyncCtx>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let rel = body
        .get("rel")
        .and_then(Value::as_str)
        .ok_or((StatusCode::BAD_REQUEST, "missing rel".into()))?;
    let on = body.get("on").and_then(Value::as_bool).unwrap_or(true);
    let relative = validate_audio_relative(rel)
        .map_err(|_| (StatusCode::BAD_REQUEST, "bad rel".to_owned()))?;
    let target = managed_path(&ctx.data, &relative, on)
        .map_err(|_| (StatusCode::BAD_REQUEST, "bad rel".to_owned()))?;
    if on {
        // Cache hit = local move instead of a re-download.
        let cached = managed_path(&ctx.cache, &relative, true)
            .map_err(|_| (StatusCode::BAD_REQUEST, "bad rel".to_owned()))?;
        if cached.exists() {
            std::fs::rename(&cached, &target)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        } else {
            let cfg = ctx
                .r2
                .read()
                .unwrap()
                .clone()
                .ok_or((StatusCode::CONFLICT, "R2 未配置".to_string()))?;
            r2::download_to(&cfg, rel, &target)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        }
    } else {
        let _ = std::fs::remove_file(&target);
    }
    Ok(Json(json!({ "ok": true, "pinned": on })))
}

fn validate_audio_relative(value: &str) -> Result<PathBuf, ()> {
    if value.is_empty()
        || value.len() > 1_024
        || value.contains(['\\', '\0', ':'])
        || value.to_ascii_lowercase().contains("%2f")
        || value.to_ascii_lowercase().contains("%5c")
    {
        return Err(());
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || !path.extension().is_some_and(is_supported_audio_extension)
    {
        return Err(());
    }
    Ok(path.to_path_buf())
}

fn is_supported_audio_extension(extension: &std::ffi::OsStr) -> bool {
    extension.to_str().is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "m4a" | "mp3" | "wav"
        )
    })
}

fn managed_path(root: &Path, relative: &Path, create_parents: bool) -> Result<PathBuf, ()> {
    if create_parents {
        std::fs::create_dir_all(root).map_err(|_| ())?;
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|_| ())?;
    let components: Vec<_> = relative.components().collect();
    let mut current = canonical_root.clone();
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(component) = component else {
            return Err(());
        };
        current.push(component);
        let final_component = index + 1 == components.len();
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || (!final_component && !metadata.is_dir())
                    || (final_component && !metadata.is_file())
                    || !std::fs::canonicalize(&current)
                        .map_err(|_| ())?
                        .starts_with(&canonical_root)
                {
                    return Err(());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !final_component && create_parents {
                    std::fs::create_dir(&current).map_err(|_| ())?;
                } else if !final_component {
                    return Err(());
                }
            }
            Err(_) => return Err(()),
        }
    }
    Ok(current)
}

fn remote_audio_key(data: &Path, requested: &Path) -> Result<PathBuf, ()> {
    let manifest_path = managed_path(data, Path::new("manifest.json"), false)?;
    let metadata = std::fs::metadata(&manifest_path).map_err(|_| ())?;
    if metadata.len() > 32 * 1024 * 1024 {
        return Err(());
    }
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(manifest_path).map_err(|_| ())?).map_err(|_| ())?;
    let requested = requested.to_str().ok_or(())?;
    let entries = manifest.as_object().ok_or(())?;
    if entries.len() > 10_000 {
        return Err(());
    }
    let Some(r2_key) = entries.values().find_map(|entry| {
        (entry.get("audio").and_then(Value::as_str) == Some(requested))
            .then(|| entry.get("r2_key").and_then(Value::as_str))
            .flatten()
    }) else {
        return validate_audio_relative(requested);
    };
    validate_audio_relative(r2_key)
}

/// Fallback for every non-API path: local archive first (Range via ServeDir),
/// then audio cache, then a signed R2 streaming proxy that also warms the
/// cache in the background.
#[axum::debug_handler]
pub async fn serve_or_fetch(State(ctx): State<Arc<SyncCtx>>, req: Request) -> Response {
    // Decompose into owned parts so no !Sync body reference crosses an await.
    let (parts, _body) = req.into_parts();
    let (uri, headers) = (parts.uri, parts.headers);
    let path = uri.path().to_string();
    if path == "/index.html" {
        let Ok(_guard) = crate::processing::archive::try_archive_mutation_lock(&ctx.data) else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        return match crate::processing::archive::render_existing_viewer(&ctx.data) {
            Ok(bytes) => trusted_response(bytes, "text/html; charset=utf-8"),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
    }
    if path == "/marked.min.js" {
        return trusted_response(TRUSTED_MARKED_JS.to_vec(), "text/javascript; charset=utf-8");
    }
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let make_req = || {
        let mut r = Request::builder()
            .uri(uri.clone())
            .body(Body::empty())
            .unwrap();
        *r.headers_mut() = headers.clone();
        r
    };

    let local = ServeDir::new(&ctx.data)
        .oneshot(make_req())
        .await
        .expect("ServeDir is infallible");
    if local.status() != StatusCode::NOT_FOUND {
        return local.into_response();
    }
    let key = percent_encoding::percent_decode_str(path.trim_start_matches('/'))
        .decode_utf8_lossy()
        .into_owned();
    let Ok(relative) = validate_audio_relative(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let remote_key = match remote_audio_key(&ctx.data, &relative) {
        Ok(key) => key,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let Ok(cached) = managed_path(&ctx.cache, &relative, true) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if cached.exists() {
        r2::touch(&cached);
        return ServeDir::new(&ctx.cache)
            .oneshot(make_req())
            .await
            .expect("ServeDir is infallible")
            .into_response();
    }

    let Some(cfg) = ctx.r2.read().unwrap().clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let remote_key = remote_key.to_string_lossy().into_owned();
    match r2::request(&cfg, "GET", &remote_key, range.as_deref()).await {
        Ok(upstream) => {
            let status = upstream.status();
            if !(status.is_success() || status.as_u16() == 206) {
                return (StatusCode::BAD_GATEWAY, format!("R2 {status}")).into_response();
            }
            r2::spawn_cache_fill(
                cfg,
                ctx.cache.clone(),
                cached,
                remote_key,
                ctx.inflight.clone(),
            );
            let mut builder = Response::builder().status(status.as_u16());
            for h in [
                header::CONTENT_TYPE,
                header::CONTENT_LENGTH,
                header::CONTENT_RANGE,
                header::ACCEPT_RANGES,
                header::ETAG,
            ] {
                if let Some(v) = upstream.headers().get(&h) {
                    builder = builder.header(h, v);
                }
            }
            use futures_util::TryStreamExt;
            builder
                .body(Body::from_stream(
                    upstream.bytes_stream().map_err(std::io::Error::other),
                ))
                .unwrap_or_else(|e| {
                    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
                })
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("R2 代理失败: {e}")).into_response(),
    }
}

pub fn trusted_html_response(html: &'static str) -> Response {
    trusted_response(html.as_bytes().to_vec(), "text/html; charset=utf-8")
}

fn trusted_response(bytes: Vec<u8>, content_type: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_SECURITY_POLICY, VIEWER_CSP)
        .header("x-content-type-options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_tarball_redirect_is_one_host_and_repo_bound() {
        let accepted = validated_github_tarball_redirect(
            "owner/private-notes",
            "https://codeload.github.com/owner/private-notes/legacy.tar.gz/HEAD?token=signed",
        )
        .unwrap();
        assert_eq!(accepted.host_str(), Some("codeload.github.com"));
        for rejected in [
            "http://codeload.github.com/owner/private-notes/legacy.tar.gz/HEAD",
            "https://codeload.github.com.evil/owner/private-notes/legacy.tar.gz/HEAD",
            "https://codeload.github.com:444/owner/private-notes/legacy.tar.gz/HEAD",
            "https://secret@codeload.github.com/owner/private-notes/legacy.tar.gz/HEAD",
            "https://codeload.github.com/owner/other/legacy.tar.gz/HEAD",
            "https://codeload.github.com/owner/private-notes/legacy.tar.gz/HEAD#fragment",
            "https://codeload.github.com/owner/private-notes/legacy.tar.gz/%2e%2e",
        ] {
            assert!(validated_github_tarball_redirect("owner/private-notes", rejected).is_err());
        }
    }

    #[test]
    fn desktop_missing_local_audio_maps_to_the_manifest_r2_identity() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let friendly = "2026-09-02/120000-Synthetic.m4a";
        let r2_key = "2026-09-02/120000-recording-aaaaaaaaaaaaaaaa-g1-018f92d8-6ad4-7dc1-8e28-8b020d2942cb.m4a";
        std::fs::write(
            data.join("manifest.json"),
            json!({
                "2026-09-02 120000": {
                    "audio": friendly,
                    "r2_key": r2_key
                }
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            remote_audio_key(&data, Path::new(friendly)).unwrap(),
            PathBuf::from(r2_key)
        );
        assert!(remote_audio_key(&data, Path::new("../outside.m4a")).is_err());
    }

    fn setup_value() -> Value {
        json!({
            "githubPat": "github_pat_fabricated",
            "r2AccountId": "0123456789abcdef0123456789abcdef",
            "r2AccessKeyId": "FABRICATEDACCESSKEY123456",
            "r2SecretAccessKey": "fabricated-secret-123456",
            "repo": "owner/private-notes",
            "bucket": "private-audio"
        })
    }

    #[test]
    fn archive_setup_is_write_only_closed_and_never_uses_loopback_for_secrets() {
        let setup: ArchiveSyncSetup = serde_json::from_value(setup_value()).unwrap();
        assert_eq!(format!("{setup:?}"), "ArchiveSyncSetup(<redacted>)");
        assert!(std::mem::needs_drop::<ArchiveSyncSetup>());
        assert!(validate_repo(&setup.repo).is_ok());
        assert!(validate_bucket(&setup.bucket).is_ok());

        let mut unknown = setup_value();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("callbackUrl".to_owned(), json!("https://attacker.invalid"));
        assert!(serde_json::from_value::<ArchiveSyncSetup>(unknown).is_err());
        assert!(validate_repo("owner/repo?ref=secret").is_err());
        assert!(validate_bucket("../audio").is_err());

        let page = include_str!("setup.html");
        assert!(page.contains("save_sync_credentials"));
        assert!(page.contains("initialize_local_archive"));
        assert!(page.contains("先创建本机档案"));
        assert!(page.contains("DESKTOP_SETUP"));
        assert!(page.contains("j.state === \"ok\""));
        assert!(!page.contains("recordings > 0"));
        assert!(!page.contains("value=\"xingfanxia/watch-transcriber-data\""));
        assert!(!page.contains("restore_archive.py"));
        assert!(!page.contains("/api/sync/tokens"));
        assert!(!page.contains("JSON.stringify(setup)"));
    }

    #[test]
    fn ios_app_and_share_extension_use_the_same_private_app_group() {
        const EXPECTED: &str = "group.ai.ax.watch-transcriber";
        let app = include_str!("../gen/apple/desktop_iOS/desktop_iOS.entitlements");
        let share = include_str!("../gen/apple/ShareExtension/EchoWallShare.entitlements");
        for entitlements in [app, share] {
            assert!(entitlements.contains("com.apple.security.application-groups"));
            assert_eq!(entitlements.matches(EXPECTED).count(), 1);
        }
    }

    #[test]
    fn mobile_shared_import_cleanup_requires_durable_rust_adoption_ack() {
        let swift = include_str!("../../plugins/echowall-capture/ios/Sources/CapturePlugin.swift");
        let drain = swift
            .split("static func run()")
            .nth(1)
            .and_then(|tail| tail.split("static func acknowledge").next())
            .expect("shared import drain source must be present");
        assert!(drain.contains("stableIdentifier(source)"));
        assert!(!drain.contains("removeItem(at: source)"));
        assert!(swift.contains("at: source, withIntermediateDirectories: true"));
        let filter = drain
            .find("let audioSources = sources.filter")
            .expect("shared inbox must filter audio before applying its batch bound");
        let bound = drain
            .find("for source in audioSources.prefix(16)")
            .expect("shared inbox must apply the sixteen-item bound to audio only");
        assert!(filter < bound);
        assert!(!drain.contains("sources.prefix(16)"));
        assert!(swift.contains("@objc func acknowledgeSharedImports"));
        assert!(swift.contains("@objc func openAudioPicker"));
        assert!(swift.contains("forOpeningContentTypes: [UTType.audio], asCopy: false"));
        assert!(swift.contains("documentTypes: [\"public.audio\"], in: .open"));
        assert!(swift.contains("SharedImportDrain.stagePicked(url)"));
        assert!(swift.contains("receiptDisplayName("));
        assert!(swift.contains("root.appendingPathComponent(\"\\(identifier).json\")"));
        assert!(swift.contains("@objc func exportAudio"));
        assert!(
            swift.contains("UIDocumentPickerViewController(forExporting: [staged], asCopy: true)")
        );
        assert!(swift.contains("digest.sha256 == pending.expectedSha256"));
        assert!(swift.contains("appendingPathComponent(\"native-capture\""));
        assert!(swift.contains("echoWallAppIdentifier = \"ai.ax.watch-transcriber\""));
        assert!(swift.contains("echoWallAppDataRoot(support)"));
        assert!(swift.contains("appendingPathComponent(\"inbox\", isDirectory: true)"));
        assert!(swift.contains("appendingPathComponent(\"recording.json\")"));
        assert!(swift.contains("recorder?.updateMeters()"));
        assert!(swift.contains("averagePower(forChannel: 0)"));
        assert!(swift.contains("result[\"microphoneLevel\"]"));
        assert!(swift.contains("minimumCaptureFreeBytes"));
        assert!(swift.contains("volumeAvailableCapacityForImportantUsage"));
        assert!(swift.contains("\"storageReady\""));
        let kotlin = include_str!(
            "../gen/android/app/src/main/java/ai/ax/watch_transcriber/capture/ImportInbox.kt"
        );
        assert!(kotlin.contains("fun acknowledge("));
        assert!(kotlin.contains("acknowledged-imports.ndjson"));
        let android_export = include_str!(
            "../gen/android/app/src/main/java/ai/ax/watch_transcriber/capture/ExportAudio.kt"
        );
        assert!(android_export.contains("File(context.filesDir.absoluteFile, \"inbox\")"));
        assert!(android_export.contains("source contains a symbolic link"));
        assert!(!android_export.contains("\"recordings\""));
        assert!(kotlin.contains("if (item.optString(\"importId\") !in acknowledged)"));
        let android_plugin = include_str!(
            "../gen/android/app/src/main/java/ai/ax/watch_transcriber/capture/RecorderPlugin.kt"
        );
        assert!(android_plugin.contains("pendingRecovery"));
        assert!(android_plugin.contains("RecordingService.isActive()"));
        assert!(android_plugin.contains("journal.recoverInterrupted()"));
        assert!(android_plugin.contains("RecordingService.latestLevel()"));
        assert!(android_plugin.contains("CaptureStorage.isReady(availableBytes)"));
        assert!(android_plugin.contains("\"storageReady\""));
        let mac_info = include_str!("../Info.plist");
        assert!(mac_info.contains("NSMicrophoneUsageDescription"));
        assert!(mac_info.contains("NSScreenCaptureUsageDescription"));
        assert!(mac_info.contains("NSAudioCaptureUsageDescription"));
        assert!(mac_info.contains("whole browser"));
        assert!(mac_info.contains("System Capture"));
        let android_gradle = include_str!("../gen/android/app/build.gradle.kts");
        assert!(android_gradle.contains("testInstrumentationRunnerArguments[\"notClass\"]"));
        assert!(android_gradle.contains("RecordingProcessDeathHostTest"));
        let process_death_test = include_str!(
            "../gen/android/app/src/androidTest/java/ai/ax/watch_transcriber/capture/RecordingProcessDeathHostTest.kt"
        );
        assert!(process_death_test.contains("host-kill-ready"));
        assert!(process_death_test.contains("while (true) SystemClock.sleep(1_000)"));
        assert!(process_death_test.contains("SessionState.INTERRUPTED"));
        let recreated_process_phase = process_death_test
            .split("fun phaseTwoRecoversRecordingInRecreatedProcess()")
            .nth(1)
            .and_then(|tail| tail.split("fun nativeImportStagedForHostKill()").next())
            .expect("recreated-process phase must remain present");
        assert!(recreated_process_phase.contains("context.startActivity(launch)"));
        assert!(recreated_process_phase.contains("awaitState(context, SessionState.INTERRUPTED)"));
        assert!(!recreated_process_phase.contains("recoverInterrupted()"));
        assert!(process_death_test.contains("nativeImportStagedForHostKill"));
        assert!(process_death_test.contains("nativeImportReachesRustInboxAfterProcessDeath"));
        assert!(process_death_test.contains("awaitRustAdoption(appRoot, displayName, nativeCopy)"));
        let host_script = include_str!("../../../scripts/demo/test_android_process_death.sh");
        assert!(host_script.contains("ro.kernel.qemu"));
        assert!(host_script.contains("am force-stop"));
        assert!(host_script.contains("phaseTwoRecoversRecordingInRecreatedProcess"));
        assert!(host_script.contains("start_blocked_phase nativeImportStagedForHostKill"));
        assert!(host_script
            .contains("start_blocked_phase nativeImportReachesRustInboxAfterProcessDeath"));
        let android_import_test = include_str!(
            "../gen/android/app/src/androidTest/java/ai/ax/watch_transcriber/capture/ImportInboxInstrumentedTest.kt"
        );
        assert!(android_import_test.contains("ImportInbox.copyPickedUri(context, uri)"));
        assert!(android_import_test.contains("ImportInbox.acknowledge(context, listOf(importId))"));
        assert!(android_import_test.contains("assertArrayEquals(bytes, copied.readBytes())"));

        for capability in [
            include_str!("../capabilities/default.json"),
            include_str!("../capabilities/archive-viewer-remote.json"),
        ] {
            assert!(capability.contains("allow-mobile-acknowledge-shared-imports"));
            assert!(capability.contains("allow-mobile-start"));
            assert!(capability.contains("allow-capture-request-permissions"));
            assert!(capability.contains("allow-open-capture-permission-settings"));
            assert!(!capability.contains("echowall-capture:"));
        }
        let viewer = include_str!("../../../deliveries/viewer_template.html");
        assert!(viewer.contains("TAURI.core.invoke(`mobile_${command}`"));
        assert!(!viewer.contains("plugin:echowall-capture|"));
        assert!(viewer.contains("role=\"meter\" aria-label=\"麦克风实时电平\""));
        assert!(viewer.contains("status && status.microphoneLevel"));
        assert!(viewer.contains("可用存储空间不足 1 GB"));
    }

    #[test]
    fn legacy_destination_is_bound_into_one_versioned_secure_record() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("sync_config.json"),
            json!({"repo": "owner/legacy", "bucket": "legacy-audio"}).to_string(),
        )
        .unwrap();
        let legacy = secrets::SyncTokens {
            schema_version: 1,
            github_pat: "github_pat_fabricated".to_owned(),
            r2_account_id: "0123456789abcdef0123456789abcdef".to_owned(),
            r2_access_key_id: "FABRICATEDACCESSKEY123456".to_owned(),
            r2_secret_access_key: "fabricated-secret-123456".to_owned(),
            repo: String::new(),
            bucket: String::new(),
        };
        let (bound, migrated) = bind_legacy_destination(legacy, directory.path()).unwrap();
        assert!(migrated);
        assert_eq!(bound.repo, "owner/legacy");
        assert_eq!(bound.bucket, "legacy-audio");
        let encoded = serde_json::to_value(&bound).unwrap();
        assert_eq!(encoded["schema_version"], 1);
        assert_eq!(encoded["repo"], "owner/legacy");
        assert_eq!(encoded["bucket"], "legacy-audio");

        let mut half_bound = bound;
        half_bound.bucket.clear();
        assert!(bind_legacy_destination(half_bound, directory.path()).is_err());
    }

    #[test]
    fn managed_audio_paths_reject_absolute_encoded_and_symlink_escape() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("data");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        assert!(validate_audio_relative("/tmp/victim.m4a").is_err());
        assert!(validate_audio_relative("C:/victim.m4a").is_err());
        assert!(validate_audio_relative("day%2Fvictim.m4a").is_err());
        assert!(validate_audio_relative("../victim.m4a").is_err());
        assert!(validate_audio_relative("2026-09-02/audio.wav").is_ok());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
            let relative = validate_audio_relative("linked/victim.m4a").unwrap();
            assert!(managed_path(&root, &relative, true).is_err());
        }
    }

    #[test]
    fn credential_revocation_clears_all_in_memory_network_handles_immediately() {
        let directory = tempfile::tempdir().unwrap();
        let ctx = SyncCtx {
            data: directory.path().join("data"),
            base: directory.path().to_path_buf(),
            cache: directory.path().join("cache"),
            gh: RwLock::new(None),
            r2: RwLock::new(None),
            state: RwLock::new("no-tokens".to_owned()),
            error: RwLock::new(None),
            last_sync: RwLock::new(None),
            syncing: AtomicBool::new(false),
            credential_generation: AtomicU64::new(1),
            inflight: Arc::new(Mutex::new(HashSet::new())),
        };
        let tokens = secrets::SyncTokens {
            schema_version: 1,
            github_pat: "github_pat_fabricated".to_owned(),
            r2_account_id: "0123456789abcdef0123456789abcdef".to_owned(),
            r2_access_key_id: "FABRICATEDACCESSKEY123456".to_owned(),
            r2_secret_access_key: "fabricated-secret-123456".to_owned(),
            repo: "owner/private".to_owned(),
            bucket: "private-audio".to_owned(),
        };
        ctx.install_credentials(&tokens, tokens.repo.clone(), tokens.bucket.clone());
        let installed_generation = ctx.credential_generation.load(Ordering::SeqCst);
        assert!(ctx.gh.read().unwrap().is_some());
        assert!(ctx.r2.read().unwrap().is_some());

        ctx.revoke_credentials();
        assert!(ctx.gh.read().unwrap().is_none());
        assert!(ctx.r2.read().unwrap().is_none());
        assert_eq!(&*ctx.state.read().unwrap(), "no-tokens");
        assert!(ctx.credential_generation.load(Ordering::SeqCst) > installed_generation);
    }

    #[tokio::test]
    async fn trusted_index_ignores_synced_executable_html_and_enforces_csp() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("manifest.json"), b"{}").unwrap();
        std::fs::write(
            data.join("index.html"),
            b"<script>window.compromised=true</script>",
        )
        .unwrap();
        let ctx = Arc::new(SyncCtx {
            data: data.clone(),
            base: directory.path().to_path_buf(),
            cache: directory.path().join("cache"),
            gh: RwLock::new(None),
            r2: RwLock::new(None),
            state: RwLock::new("no-tokens".to_owned()),
            error: RwLock::new(None),
            last_sync: RwLock::new(None),
            syncing: AtomicBool::new(false),
            credential_generation: AtomicU64::new(1),
            inflight: Arc::new(Mutex::new(HashSet::new())),
        });
        let response = serve_or_fetch(
            State(ctx),
            Request::builder()
                .uri("/index.html")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        for directive in [
            "default-src 'none'",
            "connect-src 'self' ipc: http://ipc.localhost",
            "frame-src 'none'",
            "object-src 'none'",
            "base-uri 'none'",
        ] {
            assert!(csp.contains(directive));
        }
        let body = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("safeMarkdownFragment"));
        assert!(!html.contains("window.compromised"));
    }

    /// Build a gzipped tarball in memory with the GitHub `owner-repo-sha/` prefix.
    fn fake_tarball(files: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    format!("owner-repo-abc123/{path}"),
                    content.as_bytes(),
                )
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extract_strips_prefix_and_overlays() {
        let dir = std::env::temp_dir().join(format!("echowall-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Pre-existing pinned audio must survive the pull.
        std::fs::create_dir_all(dir.join("2026-07-20")).unwrap();
        std::fs::write(dir.join("2026-07-20/213456-test.m4a"), b"audio").unwrap();

        let tarball = fake_tarball(&[
            ("manifest.json", "{\"2026-07-20 213456\": {}}"),
            ("2026-07-20/213456-test.md", "# note"),
            (".git/config", "vcs noise"),
        ]);
        extract_overlay(&tarball, &dir).unwrap();

        assert!(dir.join("manifest.json").exists());
        assert!(dir.join("2026-07-20/213456-test.md").exists());
        assert!(
            dir.join("2026-07-20/213456-test.m4a").exists(),
            "pin survived"
        );
        assert!(!dir.join(".git").exists(), "vcs entries skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn extract_overlay_rejects_preexisting_symlink_parent() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&data).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, data.join("escape")).unwrap();
        let tarball = fake_tarball(&[("escape/payload.md", "must stay contained")]);
        assert!(extract_overlay(&tarball, &data).is_err());
        assert!(!outside.join("payload.md").exists());
    }
}
