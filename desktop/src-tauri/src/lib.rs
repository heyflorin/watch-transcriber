//! EchoWall's Rust/Tauri application boundary.
//!
//! Serves the pipeline-generated archive (data/index.html + audio) over a
//! loopback axum server — ServeDir gives HTTP Range support, so audio seek
//! works — and opens a webview onto it. The viewer stays a single compiled
//! template rendered from validated archive data by Rust; synced executable
//! HTML and JavaScript are never served.
//! The loopback server binds 127.0.0.1 on a random port. A process-scoped,
//! unguessable URL prefix protects all archive/media/API routes, and mutating
//! HTTP requests additionally require the exact loopback Origin.

pub mod capture;
mod features;
pub mod ingest;
pub mod processing;
mod qa;
mod r2;
mod secrets;
mod sync;
#[cfg(not(mobile))]
mod tray;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use serde_json::Value;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Clone)]
struct LoopbackAuth {
    origin: Arc<str>,
}

#[derive(Clone)]
struct DesktopApiState {
    data: PathBuf,
    archive: Arc<processing::archive::DeferredArchive>,
}

#[derive(Clone)]
struct DesktopEditState {
    data: PathBuf,
    archive: Arc<processing::archive::DeferredArchive>,
}

impl axum::extract::FromRef<DesktopApiState> for DesktopEditState {
    fn from_ref(state: &DesktopApiState) -> Self {
        Self {
            data: state.data.clone(),
            archive: Arc::clone(&state.archive),
        }
    }
}

#[derive(Clone)]
struct DesktopArchiveState(Arc<processing::archive::DeferredArchive>);

#[cfg(not(mobile))]
#[tauri::command]
async fn initialize_local_archive(
    state: tauri::State<'_, DesktopArchiveState>,
) -> Result<(), String> {
    state
        .0
        .initialize_local()
        .await
        .map_err(|_| "local archive could not be initialized".to_owned())
}

fn archive_edit_http_error(error: processing::engine::EffectError) -> (StatusCode, String) {
    if error.kind == processing::engine::EffectErrorKind::PublicationConflict {
        (
            StatusCode::CONFLICT,
            "archive changed; refresh and retry".to_owned(),
        )
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "archive publish failed".to_owned(),
        )
    }
}

async fn require_loopback_session(
    State(auth): State<LoopbackAuth>,
    request: Request,
    next: Next,
) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        let same_origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|origin| origin.as_bytes() == auth.origin.as_bytes());
        if !same_origin {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    next.run(request).await
}

fn allowed_main_navigation(url: &tauri::Url, port: u16, session_token: &str) -> bool {
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port() != Some(port)
        || url.fragment().is_some()
    {
        return false;
    }
    let prefix = format!("/__echowall/{session_token}/");
    match url.path().strip_prefix(&prefix) {
        Some("index.html") => matches!(url.query(), None | Some("m=1")),
        Some("setup") => matches!(url.query(), None | Some("desktop=1")),
        _ => false,
    }
}

/// Merge speaker-tag updates into data/manifest.json and persist them to the
/// private notes repo. The archive's user-authored state lives ONLY in the
/// manifest (Python's manifest delivery preserves the `speakers` field on
/// reprocess). The shared cross-process archive lock serializes this
/// read-modify-write with the Rust publisher and legacy Python watcher.
async fn save_speakers(
    State(state): State<DesktopEditState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let err500 = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let bad = |message: &str| (StatusCode::BAD_REQUEST, message.to_owned());
    let dir = state.data.clone();
    let transaction = processing::archive::begin_native_archive_transaction(&dir)
        .await
        .map_err(|_| (StatusCode::CONFLICT, "archive is busy".to_owned()))?;
    let updates = body
        .get("updates")
        .and_then(Value::as_array)
        .ok_or((StatusCode::BAD_REQUEST, "missing updates[]".into()))?;
    if updates.len() > 100 {
        return Err(bad("too many updates"));
    }
    let mut deltas = Vec::with_capacity(updates.len());
    for u in updates {
        let (Some(key), Some(speakers)) = (
            u.get("key").and_then(Value::as_str),
            u.get("speakers").and_then(Value::as_object),
        ) else {
            return Err(bad("invalid speaker update"));
        };
        if speakers.len() > 64 {
            return Err(bad("too many speakers"));
        }
        let entry_value = processing::archive::archive_entry_snapshot(&dir, key)
            .map_err(|_| bad("unknown recording"))?;
        let Some(entry) = entry_value.as_object() else {
            return Err(bad("unknown recording"));
        };
        let recording_id = entry
            .get("recording_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let base_entry_sha256 = processing::archive::archive_entry_sha256(&entry_value)
            .map_err(|_| err500("invalid archive entry".to_owned()))?;
        let current_speakers = match entry.get("speakers") {
            Some(value) => value
                .as_object()
                .cloned()
                .ok_or(bad("invalid speaker state"))?,
            None => Default::default(),
        };
        let mut speaker_slots = Vec::with_capacity(speakers.len());
        for (slot, name) in speakers {
            let name = name.as_str().unwrap_or("").trim();
            if slot.is_empty()
                || slot.len() > 32
                || !slot
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || name.len() > 60
                || name.chars().any(char::is_control)
            {
                return Err(bad("invalid speaker"));
            }
            let expected = current_speakers
                .get(slot)
                .and_then(Value::as_str)
                .map(str::to_owned);
            let desired = (!name.is_empty()).then(|| name.to_owned());
            speaker_slots.push(processing::archive::SpeakerSlotDelta {
                slot: slot.clone(),
                expected,
                desired,
            });
        }
        deltas.push(processing::archive::ArchiveEntryDelta {
            key: key.to_owned(),
            recording_id,
            base_entry_sha256: Some(base_entry_sha256),
            speaker_slots,
            add_attachments: Vec::new(),
        });
    }

    state
        .archive
        .execute_native_edit_in_transaction(
            &transaction,
            processing::archive::NativeArchiveEdit::UserFields { deltas },
        )
        .await
        .map_err(archive_edit_http_error)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Attach a markdown note to a recording: writes
/// data/<date>/<HHMMSS>-attachments/<name>.md, records it in the manifest
/// entry's `attachments` list, rebuilds the viewer, pushes the notes repo.
async fn save_attachment(
    State(state): State<DesktopEditState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let bad = |m: &str| (StatusCode::BAD_REQUEST, m.to_string());
    let dir = state.data.clone();
    let transaction = processing::archive::begin_native_archive_transaction(&dir)
        .await
        .map_err(|_| (StatusCode::CONFLICT, "archive is busy".to_owned()))?;
    let key = body
        .get("key")
        .and_then(Value::as_str)
        .ok_or(bad("missing key"))?;
    let content = body
        .get("content")
        .and_then(Value::as_str)
        .ok_or(bad("missing content"))?;
    let raw_name = body.get("name").and_then(Value::as_str).unwrap_or("附注");
    if content.len() > 4 * 1024 * 1024 || raw_name.len() > 256 {
        return Err(bad("attachment is too large"));
    }

    let (rel, delta) = processing::archive::prepare_local_attachment(&dir, key, raw_name, content)
        .map_err(|_| bad("attachment cannot be written"))?;

    state
        .archive
        .execute_native_edit_in_transaction(
            &transaction,
            processing::archive::NativeArchiveEdit::UserFields {
                deltas: vec![delta],
            },
        )
        .await
        .map_err(archive_edit_http_error)?;
    Ok(Json(serde_json::json!({ "ok": true, "rel": rel })))
}

/// Persist a custom speaker color to data/speakers.json (source for the
/// viewer's spkColor override), then rebuild the viewer + push the repo.
async fn save_speaker_color(
    State(state): State<DesktopEditState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let err500 = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let bad = |m: &str| (StatusCode::BAD_REQUEST, m.to_string());
    let dir = state.data.clone();
    let transaction = processing::archive::begin_native_archive_transaction(&dir)
        .await
        .map_err(|_| (StatusCode::CONFLICT, "archive is busy".to_owned()))?;
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .ok_or(bad("missing name"))?;
    let color = body
        .get("color")
        .and_then(Value::as_str)
        .ok_or(bad("missing color"))?;
    if name.is_empty() || name.len() > 60 || name.contains('\n') {
        return Err(bad("bad name"));
    }
    let hex_ok = color.len() == 7
        && color.starts_with('#')
        && color[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !hex_ok {
        return Err(bad("color must be #rrggbb"));
    }

    let expected = processing::archive::speaker_color_snapshot(&dir, name)
        .map_err(|_| err500("invalid speaker color state".to_owned()))?;
    state
        .archive
        .execute_native_edit_in_transaction(
            &transaction,
            processing::archive::NativeArchiveEdit::SpeakerColors {
                name: name.to_owned(),
                expected,
                desired: color.to_owned(),
            },
        )
        .await
        .map_err(archive_edit_http_error)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Delete one recording through the native archive boundary. Local artifacts,
/// manifest/viewer/topic/rollup state are removed under the shared archive
/// lock; an owner-verified R2 delete follows before the local Git push.
async fn delete_recording(
    State(state): State<DesktopApiState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let key = body
        .get("key")
        .and_then(Value::as_str)
        .ok_or((StatusCode::BAD_REQUEST, "missing key".to_string()))?
        .to_string();
    let retain_unverified_audio = match body.get("retainUnverifiedAudio") {
        Some(value) => value.as_bool().ok_or((
            StatusCode::BAD_REQUEST,
            "retainUnverifiedAudio must be boolean".to_owned(),
        ))?,
        None => false,
    };
    let data = state.data.clone();
    let transaction = processing::archive::begin_native_archive_transaction(&data)
        .await
        .map_err(|_| (StatusCode::CONFLICT, "archive is busy".to_owned()))?;
    let deleted = processing::archive::plan_local_recording_delete(&data, &key).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "recording cannot be deleted".to_owned(),
        )
    })?;
    let r2_delete = match (
        deleted.r2_key.clone().or_else(|| {
            deleted.audio_relative.clone().filter(|key| {
                deleted
                    .recording_id
                    .as_deref()
                    .zip(deleted.r2_generation)
                    .is_some_and(|(recording_id, generation)| {
                        r2::is_generation_owned_key(key, recording_id, generation)
                    })
            })
        }),
        deleted.recording_id.clone(),
        deleted.r2_generation,
        deleted.audio_sha256.clone(),
        deleted.audio_size_bytes,
    ) {
        (Some(key), Some(recording_id), Some(generation), Some(sha256), Some(size_bytes))
            if r2::is_generation_owned_key(&key, &recording_id, generation) =>
        {
            Some(processing::archive::R2DeleteIntent {
                key,
                recording_id,
                generation,
                sha256,
                size_bytes,
            })
        }
        _ => None,
    };
    let legacy_audio_retained = deleted.audio_relative.is_some() && r2_delete.is_none();
    if legacy_audio_retained && !retain_unverified_audio {
        return Err((
            StatusCode::CONFLICT,
            "legacy audio has no ownership proof; confirm note deletion with the old remote audio retained"
                .to_owned(),
        ));
    }
    let r2_status = if r2_delete.is_some() {
        "deleted-or-already-missing"
    } else {
        if legacy_audio_retained {
            "retained-unverified-legacy"
        } else {
            "not-applicable"
        }
    };
    state
        .archive
        .execute_native_edit_in_transaction(
            &transaction,
            processing::archive::NativeArchiveEdit::Delete {
                key: key.clone(),
                recording_id: deleted.recording_id.clone(),
                base_entry_sha256: Some(deleted.base_entry_sha256.clone()),
                deleted_paths: deleted.deleted.clone(),
                r2_delete,
            },
        )
        .await
        .map_err(archive_edit_http_error)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "deleted": deleted.deleted,
        "r2": r2_status,
    })))
}

#[cfg(not(feature = "isolated-qa"))]
fn discovered_data_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WATCH_TRANSCRIBER_DATA") {
        return Some(PathBuf::from(p));
    }
    // Walk up from the executable looking for the repo's data/ — covers the
    // debug/release bundle launched from inside any clone, wherever it lives.
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.as_path();
        while let Some(parent) = cur.parent() {
            let candidate = parent.join("data");
            if candidate.join("manifest.json").exists() || parent.join("transcribe.py").exists() {
                return Some(candidate);
            }
            cur = parent;
        }
    }
    None
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_echowall_capture::init());
    #[cfg(not(mobile))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        capture::commands::capture_capabilities,
        capture::commands::capture_permissions,
        capture::commands::capture_preflight,
        capture::commands::capture_request_permissions,
        capture::commands::capture_source_icon,
        capture::commands::capture_sources,
        capture::commands::capture_status,
        capture::commands::open_capture_permission_settings,
        capture::commands::pause_capture,
        capture::commands::resume_capture,
        capture::commands::start_capture,
        capture::commands::stop_capture,
        features::runtime_features,
        ingest::import::import_audio_files,
        ingest::import::confirm_import_review,
        ingest::import::export_recording_original,
        initialize_local_archive,
        processing::local_models::cancel_local_model_install,
        processing::local_models::install_local_model_pack,
        processing::local_models::moss::install_moss_candidate_model_pack,
        processing::local_models::moss::moss_candidate_model_pack_status,
        processing::local_models::moss::remove_moss_candidate_model_pack,
        processing::local_models::moss_pipeline::moss_pipeline_model_status,
        processing::local_models::moss_pipeline::install_moss_pipeline_model_pack,
        processing::local_models::moss_pipeline::remove_moss_pipeline_model_pack,
        processing::local_models::install_qwen_candidate_model_pack,
        processing::local_models::install_speakerkit_candidate_model_pack,
        processing::local_models::local_model_pack_status,
        processing::local_models::qwen_candidate_model_pack_status,
        processing::local_models::remove_local_model_pack,
        processing::local_models::remove_qwen_candidate_model_pack,
        processing::local_models::remove_speakerkit_candidate_model_pack,
        processing::local_models::speakerkit_candidate_model_pack_status,
        processing::commands::accept_local_transcript_only,
        processing::commands::back_up_local_recording_to_cloud,
        processing::commands::cancel_processing_recording,
        processing::commands::discard_processing_recording,
        processing::commands::list_processing_recordings,
        processing::commands::process_recordings,
        processing::commands::process_recording_with_local_models,
        processing::commands::process_recording_with_moss_candidate,
        processing::commands::process_recording_with_qwen_candidate,
        processing::commands::reprocess_recording,
        processing::commands::retry_processing_recording,
        processing::commands::take_over_processing_with_local_models,
        processing::commands::take_over_processing_with_qwen_candidate,
        processing::preference::get_processing_preference,
        processing::preference::set_processing_preference,
        secrets::delete_processing_credentials,
        secrets::processing_credentials_status,
        secrets::save_processing_credentials,
        sync::delete_sync_credentials,
        sync::save_sync_credentials
    ]);
    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        ingest::mobile::adopt_mobile_imports,
        ingest::mobile::finalize_mobile_capture,
        ingest::mobile::finalize_pending_mobile_capture,
        ingest::mobile::list_pending_mobile_recordings,
        ingest::mobile::mobile_acknowledge_shared_imports,
        ingest::mobile::mobile_check_permissions,
        ingest::mobile::mobile_drain_shared_imports,
        ingest::mobile::mobile_open_audio_picker,
        ingest::mobile::mobile_pause,
        ingest::mobile::mobile_preflight,
        ingest::mobile::mobile_request_permissions,
        ingest::mobile::mobile_resume,
        ingest::mobile::mobile_start,
        ingest::mobile::mobile_status,
        ingest::mobile::mobile_stop,
        features::runtime_features,
        ingest::import::confirm_import_review,
        ingest::import::export_recording_original,
        processing::commands::cancel_processing_recording,
        processing::commands::discard_processing_recording,
        processing::commands::list_processing_recordings,
        processing::commands::process_recordings,
        processing::commands::reprocess_recording,
        processing::commands::retry_processing_recording,
        processing::preference::get_processing_preference,
        processing::preference::set_processing_preference,
        secrets::delete_processing_credentials,
        secrets::processing_credentials_status,
        secrets::save_processing_credentials,
        sync::delete_sync_credentials,
        sync::save_sync_credentials
    ]);
    builder
        .manage(features::RuntimeFeatures::load())
        .setup(|app| {
            // Mobile has no repo to walk up to and often no $HOME — resolve the
            // platform app-data dir and route it through the existing env check.
            #[cfg(mobile)]
            {
                if std::env::var("WATCH_TRANSCRIBER_DATA").is_err() {
                    if let Ok(d) = app.path().app_data_dir() {
                        std::env::set_var("WATCH_TRANSCRIBER_DATA", d.join("data"));
                    }
                }
            }
            let app_data_directory = app.path().app_data_dir()?;
            #[cfg(feature = "isolated-qa")]
            let dir = qa::archive_directory(&app.config().identifier, &app_data_directory)?;
            #[cfg(not(feature = "isolated-qa"))]
            let dir = discovered_data_dir().unwrap_or_else(|| app_data_directory.join("data"));
            std::fs::create_dir_all(&dir)?;
            #[cfg(feature = "isolated-qa")]
            qa::record_launch(&app.config().identifier, &app_data_directory)?;
            {
                let inbox = Arc::new(ingest::inbox::Inbox::open(&app_data_directory, &dir)?);
                app.manage(processing::preference::ProcessingPreferenceState::open(
                    &app_data_directory,
                )?);
                let credential_state = secrets::ProcessingCredentialsState::new();
                let credential_handle = Arc::new(credential_state.clone());
                app.manage(credential_state);
                app.manage(ingest::import::ImportReviewState::new(Arc::clone(&inbox)));
                app.manage(ingest::import::RecordingFileState::new(Arc::clone(&inbox)));
                #[cfg(not(mobile))]
                app.manage(processing::local_models::LocalModelState::new(Arc::new(
                    processing::local_models::LocalModelPackManager::open(&app_data_directory)?,
                )));
                let processing_store = Arc::new(processing::ProcessingStore::open(
                    &app_data_directory,
                    inbox.root(),
                    &dir,
                )?);
                let archive = Arc::new(processing::archive::DeferredArchive::new(
                    dir.clone(),
                    inbox.root().to_path_buf(),
                    app_data_directory.clone(),
                )?);
                let desktop_archive = Arc::clone(&archive);
                let pending_edit_archive = Arc::clone(&archive);
                tauri::async_runtime::spawn(async move {
                    let _ = pending_edit_archive.resume_pending_native_edits().await;
                });
                let effects = processing::direct::DirectEffects::production(
                    credential_handle,
                    archive,
                    processing::local_worker::LocalWhisperWorker::bundled(
                        &app_data_directory,
                        app.state::<features::RuntimeFeatures>().local_stt,
                    )?,
                )?;
                // Optional candidate initialization must never break startup
                // on mobile, Windows or Intel Macs, whose worker is unsupported.
                #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
                let effects =
                    effects.with_moss_worker(processing::moss_worker::MossWorker::bundled(
                        &app_data_directory,
                        app.state::<features::RuntimeFeatures>()
                            .local_moss_candidate
                            && app.state::<features::RuntimeFeatures>().local_stt,
                    )?);
                let effects = Arc::new(effects);
                let engine = Arc::new(processing::engine::ProcessingEngine::new(
                    Arc::clone(&inbox),
                    processing_store,
                    effects,
                ));
                let processor: Arc<dyn processing::commands::AppProcessor> = engine;
                #[cfg(feature = "isolated-qa")]
                let qa_public_recording = qa::prepare_public_if_requested(
                    &app_data_directory,
                    Arc::clone(&inbox),
                    &*processor,
                )?;
                let processing_state =
                    processing::commands::EmbeddedProcessingState::new(processor);
                #[cfg(feature = "isolated-qa")]
                let qa_retry_started = qa::retry_public_if_requested(
                    qa_public_recording,
                    processing_state.processor(),
                )?;
                #[cfg(not(feature = "isolated-qa"))]
                let qa_retry_started = false;
                if !qa_retry_started {
                    processing::commands::resume_pending(
                        &processing_state,
                        app.state::<features::RuntimeFeatures>().direct_processing,
                    );
                }
                app.manage(processing_state);
                app.manage(DesktopArchiveState(desktop_archive));
                #[cfg(not(mobile))]
                let importer = ingest::import::DesktopImporter::new(
                    Arc::clone(&inbox),
                    ingest::import::current_desktop_platform()?,
                    ingest::import::DEFAULT_MAX_IMPORT_BYTES,
                )?;
                #[cfg(not(mobile))]
                app.manage(ingest::import::DesktopImportState::new(importer));
                #[cfg(not(mobile))]
                app.manage(capture::commands::initialize_capture_state(Arc::clone(
                    &inbox,
                ))?);
                #[cfg(mobile)]
                {
                    let mut container_roots = vec![app_data_directory];
                    #[cfg(target_os = "android")]
                    {
                        let files_directory = container_roots[0].join("files");
                        if files_directory.is_dir() {
                            container_roots.push(files_directory);
                        }
                    }
                    for candidate in [
                        app.path().document_dir(),
                        app.path().cache_dir(),
                        app.path().temp_dir(),
                    ] {
                        if let Ok(path) = candidate {
                            if path.is_dir() && !container_roots.contains(&path) {
                                container_roots.push(path);
                            }
                        }
                    }
                    let finalizer = Arc::new(ingest::mobile::SymphoniaPcmFinalizer::new(
                        u64::from(u32::MAX) - 44,
                    )?);
                    let mobile = ingest::mobile::MobileIngest::new(
                        Arc::clone(&inbox),
                        ingest::mobile::current_mobile_platform()?,
                        container_roots,
                        ingest::mobile::DEFAULT_MAX_MOBILE_IMPORT_BYTES,
                        finalizer,
                    )?;
                    app.manage(ingest::mobile::MobileIngestState::new(mobile));
                }
            }
            let ready = dir.join("manifest.json").exists();
            let ctx = sync::SyncCtx::new(dir.clone());
            app.manage(sync::SyncRuntimeState(Arc::clone(&ctx)));
            let has_tokens = ctx.gh.read().unwrap().is_some();

            // Every standalone install can bootstrap its private archive
            // directly through the Rust GitHub/R2 sync path. Source checkouts
            // with an existing data/ directory keep the desktop archive view.
            let page = if cfg!(mobile) {
                // Tokens saved but archive not landed yet -> setup resumes its
                // sync-progress polling and enters the viewer when done.
                if ready {
                    "index.html?m=1"
                } else {
                    "setup"
                }
            } else if ready {
                "index.html"
            } else {
                "setup?desktop=1"
            };

            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let session_token = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let auth = LoopbackAuth {
                origin: Arc::from(format!("http://127.0.0.1:{port}")),
            };
            let srv_ctx = ctx.clone();
            let server_auth = auth.clone();
            let server_session_token = session_token.clone();
            let server_archive = app.state::<DesktopArchiveState>().0.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                rt.block_on(async move {
                    listener
                        .set_nonblocking(true)
                        .expect("nonblocking listener");
                    let listener =
                        tokio::net::TcpListener::from_std(listener).expect("tokio listener");
                    // Sync-on-launch: mobile with saved tokens refreshes the
                    // archive in the background while the viewer opens.
                    if has_tokens && (cfg!(mobile) || !ready) {
                        tokio::spawn(sync::pull(srv_ctx.clone()));
                    }
                    // /setup is the native-secure-store first-run page for
                    // desktop and mobile standalone installs.
                    let desktop_api: axum::Router = axum::Router::new()
                        .route("/api/speakers", post(save_speakers))
                        .route("/api/attachments", post(save_attachment))
                        .route("/api/delete", post(delete_recording))
                        .route("/api/speaker-colors", post(save_speaker_color))
                        .with_state(DesktopApiState {
                            data: dir,
                            archive: server_archive,
                        });
                    let sync_api: axum::Router = axum::Router::new()
                        .route(
                            "/setup",
                            get(|| async {
                                sync::trusted_html_response(include_str!("setup.html"))
                            }),
                        )
                        .route("/api/sync/status", get(sync::status))
                        .route("/api/sync/refresh", post(sync::refresh))
                        .route("/api/sync/pins", get(sync::pins))
                        .route("/api/sync/pin", post(sync::set_pin))
                        .fallback(get(sync::serve_or_fetch))
                        .with_state(srv_ctx);
                    let protected =
                        desktop_api
                            .merge(sync_api)
                            .layer(axum::middleware::from_fn_with_state(
                                server_auth.clone(),
                                require_loopback_session,
                            ));
                    let router = axum::Router::new()
                        .nest(&format!("/__echowall/{server_session_token}"), protected);
                    axum::serve(listener, router).await.expect("archive server");
                });
            });
            let url = format!("http://127.0.0.1:{port}/__echowall/{session_token}/{page}");
            let navigation_token = session_token.clone();
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .on_navigation(move |url| allowed_main_navigation(url, port, &navigation_token))
                .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
                .title(if cfg!(feature = "isolated-qa") {
                    "回音壁 EchoWall · 隔离测试"
                } else {
                    "回音壁 EchoWall"
                });
            // Desktop-only sizing: on iOS/Android these leak into the webview's
            // logical viewport (CSS sees ~1360px → phone renders the desktop
            // layout); mobile must take the native screen size.
            #[cfg(not(mobile))]
            let win = win.inner_size(1360.0, 900.0).min_inner_size(800.0, 600.0);
            #[cfg(not(mobile))]
            {
                let window = win.build()?;
                let close_window = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = close_window.hide();
                    }
                });
                tray::install(app)?;
            }
            #[cfg(mobile)]
            win.build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(mobile)]
            let _ = (app, event);
            #[cfg(not(mobile))]
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                if app
                    .state::<capture::commands::CaptureCommandState>()
                    .is_active()
                {
                    api.prevent_exit();
                    tray::request_quit(app);
                }
            }
        });
}

#[cfg(test)]
mod loopback_auth_tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn router(auth: LoopbackAuth) -> axum::Router {
        let protected = axum::Router::new()
            .route(
                "/private",
                get(|| async { "private" }).post(|| async { "updated" }),
            )
            .layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                require_loopback_session,
            ));
        axum::Router::new().nest("/__echowall/fabricated-session-token", protected)
    }

    fn auth() -> LoopbackAuth {
        LoopbackAuth {
            origin: Arc::from("http://127.0.0.1:43123"),
        }
    }

    #[tokio::test]
    async fn loopback_archive_requires_capability_path_and_same_origin_for_writes() {
        let app = router(auth());
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/private")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::NOT_FOUND);

        let wrong_session = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/__echowall/wrong/private")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_session.status(), StatusCode::NOT_FOUND);

        let readable = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/__echowall/fabricated-session-token/private")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(readable.status(), StatusCode::OK);

        let cross_origin_write = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/__echowall/fabricated-session-token/private")
                    .header(header::ORIGIN, "https://attacker.invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cross_origin_write.status(), StatusCode::FORBIDDEN);

        let same_origin_write = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/__echowall/fabricated-session-token/private")
                    .header(header::ORIGIN, "http://127.0.0.1:43123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(same_origin_write.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn attachment_write_rejects_path_traversal_before_touching_disk() {
        let temporary = tempfile::tempdir().unwrap();
        let archive = temporary.path().join("archive");
        std::fs::create_dir(&archive).unwrap();
        std::fs::write(
            archive.join("manifest.json"),
            r#"{"2026-09-02 120000":{"note":"2026-09-02/note.md"}}"#,
        )
        .unwrap();
        let inbox = temporary.path().join("inbox");
        std::fs::create_dir(&inbox).unwrap();
        let archive_adapter = Arc::new(
            processing::archive::DeferredArchive::new(
                archive.clone(),
                inbox,
                temporary.path().to_path_buf(),
            )
            .unwrap(),
        );
        let result = save_attachment(
            State(DesktopEditState {
                data: archive,
                archive: archive_adapter,
            }),
            Json(serde_json::json!({
                "key": "../../outside 120000",
                "name": "escape",
                "content": "fabricated"
            })),
        )
        .await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(!temporary.path().join("outside").exists());
    }

    #[test]
    fn main_webview_navigation_is_pinned_to_the_runtime_capability_pages() {
        let token = "fabricated-session";
        for allowed in [
            "http://127.0.0.1:43123/__echowall/fabricated-session/index.html",
            "http://127.0.0.1:43123/__echowall/fabricated-session/index.html?m=1",
            "http://127.0.0.1:43123/__echowall/fabricated-session/setup",
            "http://127.0.0.1:43123/__echowall/fabricated-session/setup?desktop=1",
        ] {
            assert!(allowed_main_navigation(
                &allowed.parse().unwrap(),
                43123,
                token
            ));
        }
        for denied in [
            "https://127.0.0.1:43123/__echowall/fabricated-session/index.html",
            "http://localhost:43123/__echowall/fabricated-session/index.html",
            "http://127.0.0.1:43124/__echowall/fabricated-session/index.html",
            "http://127.0.0.1:43123/__echowall/wrong/index.html",
            "http://127.0.0.1:43123/__echowall/fabricated-session/evil.html",
            "http://127.0.0.1:43123/__echowall/fabricated-session/index.html?admin=1",
            "http://127.0.0.1:43123/__echowall/fabricated-session/setup?desktop=2",
            "http://127.0.0.1:43123/__echowall/fabricated-session/bootstrap",
        ] {
            assert!(!allowed_main_navigation(
                &denied.parse().unwrap(),
                43123,
                token
            ));
        }
    }

    #[test]
    fn remote_capability_is_loopback_only_and_excludes_external_opening() {
        let capability: Value =
            serde_json::from_str(include_str!("../capabilities/archive-viewer-remote.json"))
                .unwrap();
        assert_eq!(capability["local"], false);
        assert_eq!(
            capability["remote"]["urls"],
            serde_json::json!(["http://127.0.0.1:*"])
        );
        assert_eq!(capability["windows"], serde_json::json!(["main"]));
        let permissions = capability["permissions"].as_array().unwrap();
        for required in [
            "allow-accept-local-transcript-only",
            "allow-back-up-local-recording-to-cloud",
            "allow-initialize-local-archive",
            "allow-save-sync-credentials",
            "allow-save-processing-credentials",
            "allow-start-capture",
            "allow-import-audio-files",
            "allow-install-speakerkit-candidate-model-pack",
            "allow-install-moss-candidate-model-pack",
            "allow-moss-candidate-model-pack-status",
            "allow-remove-moss-candidate-model-pack",
            "allow-process-recording-with-moss-candidate",
            "allow-speakerkit-candidate-model-pack-status",
            "allow-remove-speakerkit-candidate-model-pack",
            "allow-take-over-processing-with-local-models",
            "allow-take-over-processing-with-qwen-candidate",
        ] {
            assert!(permissions.iter().any(|permission| permission == required));
        }
        assert!(!permissions
            .iter()
            .any(|permission| permission == "opener:default"));
    }
}
