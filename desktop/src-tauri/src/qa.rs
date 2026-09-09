//! Build-only isolation for exercising the real App without production data.
//! No fake processor/provider is installed. Filesystem/credential names differ;
//! an explicit QA-only trigger can submit one hash-allowlisted public fixture
//! through the real importer/processor. HTTP clients fail before connection in
//! this flavor; that guard is not an OS sandbox or installed-App offline proof.
//! The worker's production deny-network sandbox is unchanged.

#[cfg(all(feature = "isolated-qa", not(target_os = "macos")))]
compile_error!("isolated-qa is a macOS-only verification flavor");

#[cfg(any(feature = "isolated-qa", test))]
pub(crate) const IDENTIFIER: &str = "ai.ax.watch-transcriber.qa.moss";

/// Identity in release builds; a connection-denying layer in opt-in QA builds.
/// All five App HTTP client constructors pass through this boundary. The
/// provider's explicit pre-client DNS lookup has an additional early guard.
pub(crate) fn guard_http(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    #[cfg(feature = "isolated-qa")]
    let builder = builder
        .no_proxy()
        .connector_layer(tower::layer::layer_fn(DenyConnector));
    builder
}

#[cfg(feature = "isolated-qa")]
pub(crate) fn deny_http() -> Result<(), std::io::Error> {
    // Fixed diagnostic only: no URL, token, body, transcript or provider error.
    // The QA launcher drains/caps this private log. No connection is attempted
    // even if stderr is unavailable; log absence alone is not a network trace.
    eprintln!("echowall-isolated-qa-http-denied-v1");
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "HTTP is disabled in the isolated QA build",
    ))
}

#[cfg(feature = "isolated-qa")]
#[derive(Clone)]
struct DenyConnector<S>(S);

#[cfg(feature = "isolated-qa")]
impl<T, S: tower::Service<T>> tower::Service<T> for DenyConnector<S> {
    type Response = S::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // Never poll the inner connector: DNS/network setup belongs to it.
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: T) -> Self::Future {
        std::future::ready(Err(deny_http().unwrap_err().into()))
    }
}

#[cfg(any(feature = "isolated-qa", test))]
fn validate(identifier: &str, root: &std::path::Path) -> Result<(), &'static str> {
    if identifier != IDENTIFIER
        || !root.is_absolute()
        || root.file_name() != Some(std::ffi::OsStr::new(IDENTIFIER))
    {
        return Err("isolated QA requires its dedicated bundle identifier and App-data namespace");
    }
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if std::fs::canonicalize(root).map_err(|_| "QA root cannot be resolved")? != root {
                return Err("isolated QA root must be canonical");
            }
            // Bootstrap adapters canonicalize their own roots. Reject existing
            // child symlinks BEFORE handing them any path, rather than allowing
            // that canonicalization to redirect an archive/inbox/model write.
            let mut pending = vec![(root.to_path_buf(), 0)];
            let mut count = 0;
            while let Some((directory, depth)) = pending.pop() {
                if depth > 64 {
                    return Err("QA directory tree exceeds its validation bound");
                }
                for entry in std::fs::read_dir(directory).map_err(|_| "QA directory unavailable")? {
                    let entry = entry.map_err(|_| "QA directory entry unavailable")?;
                    count += 1;
                    if count > 100_000 {
                        return Err("QA directory tree exceeds its validation bound");
                    }
                    let kind = entry.file_type().map_err(|_| "QA entry type unavailable")?;
                    if kind.is_dir() {
                        pending.push((entry.path(), depth + 1));
                    } else if kind.is_symlink() && internal_archive_topic_link(root, &entry.path())
                    {
                        // Existing archive contract: read-only by-topic leaf
                        // aliases point to canonical files within this archive.
                    } else if !kind.is_file() {
                        return Err("QA child paths must not be symlinks or special files");
                    }
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = root.parent().ok_or("QA root parent missing")?;
            if std::fs::canonicalize(parent).map_err(|_| "QA root parent unavailable")? != parent {
                return Err("QA root parent must be canonical");
            }
            Ok(())
        }
        _ => Err("isolated QA App-data root is unsafe"),
    }
}

#[cfg(any(feature = "isolated-qa", test))]
fn internal_archive_topic_link(root: &std::path::Path, path: &std::path::Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let parts: Vec<_> = relative.iter().collect();
    if parts.len() != 4 || parts[0] != "data" || parts[1] != "by-topic" {
        return false;
    }
    let Ok(target) = std::fs::canonicalize(path) else {
        return false;
    };
    target.starts_with(root.join("data"))
        && std::fs::symlink_metadata(target).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(feature = "isolated-qa")]
fn create_directory(path: &std::path::Path) -> Result<(), &'static str> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("QA directory creation failed"),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "QA directory unavailable")?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || std::fs::canonicalize(path).map_err(|_| "QA directory cannot be resolved")? != path
    {
        return Err("QA directory is not canonical and contained");
    }
    Ok(())
}

#[cfg(feature = "isolated-qa")]
pub(crate) fn archive_directory(
    identifier: &str,
    root: &std::path::Path,
) -> Result<std::path::PathBuf, &'static str> {
    validate(identifier, root)?;
    create_directory(root)?;
    // Create one validated component at a time. Later adapter-owned nested
    // writes start from these roots and the prechecked existing tree. Hostile
    // concurrent same-uid replacement is not the QA isolation threat model.
    for child in ["data", "inbox", "processing", "models", "qa-evidence"] {
        create_directory(&root.join(child))?;
    }
    // Deliberately ignore WATCH_TRANSCRIBER_DATA and repository discovery.
    Ok(root.join("data"))
}

#[cfg(feature = "isolated-qa")]
pub(crate) fn record_launch(
    identifier: &str,
    root: &std::path::Path,
) -> Result<(), std::io::Error> {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    validate(identifier, root).map_err(std::io::Error::other)?;
    let directory = root.join("qa-evidence");
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&directory)?;
    if fs::symlink_metadata(&directory)?.file_type().is_symlink() {
        return Err(std::io::Error::other("unsafe QA receipt path"));
    }
    let run = uuid::Uuid::new_v4();
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join(format!("launch-{run}.json")))?;
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({
            "marker":"echowall-isolated-qa-v1", "identifier":identifier,
            "pid":std::process::id(), "run_id":run,
            "scope":"real-app-isolated-paths-and-keychain-not-release",
            "http_guard":"connector-denied-not-os-network-trace",
            "worker_sandbox":"unchanged-production-deny-network"
        }),
    )?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    fs::File::open(directory)?.sync_all()
}

#[cfg(feature = "isolated-qa")]
pub(crate) fn prepare_public_if_requested(
    root: &std::path::Path,
    inbox: std::sync::Arc<crate::ingest::inbox::Inbox>,
    processor: &dyn crate::processing::commands::AppProcessor,
) -> Result<Option<uuid::Uuid>, &'static str> {
    use crate::{
        ingest::{
            envelope::Platform,
            import::{
                ConfirmImportReviewRequest, DesktopImporter, ImportReviewState,
                DEFAULT_MAX_IMPORT_BYTES,
            },
        },
        processing::{engine::EngineError, TranscriptionBackend},
    };
    use sha2::{Digest, Sha256};
    match std::env::var("ECHOWALL_QA_RUN_PUBLIC").as_deref() {
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Ok("english_01" | "english_01_retry") => {}
        _ => return Err("unknown isolated QA fixture"),
    }
    validate(IDENTIFIER, root)?;
    let audio = root.join("public-english_01.m4a");
    let metadata = std::fs::symlink_metadata(&audio).map_err(|_| "QA fixture missing")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != 2_967_552 {
        return Err("QA fixture rejected");
    }
    let expected = "22c340e2193a29a14ecfcda5d6e7c0010b9d7efbbc3e4d0fc9ac374a2969fc8c";
    if hex::encode(Sha256::digest(
        std::fs::read(&audio).map_err(|_| "QA fixture unavailable")?,
    )) != expected
    {
        return Err("QA fixture hash mismatch");
    }
    let imported = DesktopImporter::new(
        std::sync::Arc::clone(&inbox),
        Platform::Macos,
        DEFAULT_MAX_IMPORT_BYTES,
    )
    .map_err(|_| "QA importer unavailable")?
    .import_paths(vec![audio.to_string_lossy().into_owned()]);
    let result = imported.results.first().ok_or("QA import failed")?;
    if result.sha256.as_deref() != Some(expected) {
        return Err("QA imported identity mismatch");
    }
    let id = uuid::Uuid::parse_str(result.recording_id.as_deref().ok_or("QA import failed")?)
        .map_err(|_| "QA import failed")?;
    match processor.status(id) {
        Ok(ledger) if ledger.transcription_backend == TranscriptionBackend::MossLocal => {
            return Ok(Some(id))
        }
        Ok(_) => return Err("QA fixture already selected a different route"),
        Err(EngineError::Local(error)) if error.code == "recording_not_found" => {}
        Err(_) => return Err("QA ledger cannot be read"),
    }
    ImportReviewState::new(inbox)
        .confirm(ConfirmImportReviewRequest {
            recording_id: id.to_string(),
            captured_at: result
                .proposed_captured_at
                .clone()
                .ok_or("QA review time missing")?,
            display_title: Some("Public AMI english_01 · isolated QA".into()),
            speaker_count: None,
        })
        .map_err(|_| "QA import review failed")?;
    processor
        .select_full_local_moss(id, None)
        .map_err(|_| "QA local selection failed")?;
    // The ordinary startup resume loop executes the persisted MOSS job. This
    // is not a second queue, test server, fake model, or UI acceptance proof.
    Ok(Some(id))
}

/// An explicit QA retry invokes the same use case as the Retry UI command.
/// It does not edit the ledger or auto-retry a failure on an ordinary launch.
#[cfg(feature = "isolated-qa")]
pub(crate) fn retry_public_if_requested(
    id: Option<uuid::Uuid>,
    processor: std::sync::Arc<dyn crate::processing::commands::AppProcessor>,
) -> Result<bool, &'static str> {
    if std::env::var("ECHOWALL_QA_RUN_PUBLIC").as_deref() != Ok("english_01_retry") {
        return Ok(false);
    }
    let id = id.ok_or("QA retry requires the allowlisted public import")?;
    let ledger = processor
        .status(id)
        .map_err(|_| "QA retry ledger unavailable")?;
    if ledger.transcription_backend != crate::processing::TranscriptionBackend::MossLocal
        || ledger.state != crate::processing::ProcessingState::ProviderFailed
    {
        return Err("QA retry requires a failed MOSS job; use normal launch to resume");
    }
    tauri::async_runtime::spawn(async move {
        let result = processor.retry(id).await;
        eprintln!(
            "echowall-isolated-qa-retry-finished success={}",
            result.is_ok()
        );
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qa_namespace_rejects_production_relative_and_symlinked_roots() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(temp.path()).unwrap();
        let root = canonical.join(IDENTIFIER);
        assert!(validate(IDENTIFIER, &root).is_ok());
        assert!(validate("ai.ax.watch-transcriber", &root).is_err());
        assert!(validate(IDENTIFIER, &temp.path().join("ai.ax.watch-transcriber")).is_err());
        assert!(validate(IDENTIFIER, std::path::Path::new(IDENTIFIER)).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path(), &root).unwrap();
            assert!(validate(IDENTIFIER, &root).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn qa_rejects_archive_inbox_and_nested_model_symlinks_before_writing() {
        for relative in [
            "data",
            "inbox",
            "models/summary",
            "processing/jobs",
            "qa-evidence",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let canonical = std::fs::canonicalize(temp.path()).unwrap();
            let root = canonical.join(IDENTIFIER);
            let external = canonical.join("external-sentinel");
            std::fs::create_dir_all(&external).unwrap();
            let sentinel = external.join("do-not-change");
            std::fs::write(&sentinel, b"preserved external bytes").unwrap();
            let redirected = root.join(relative);
            std::fs::create_dir_all(redirected.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&external, &redirected).unwrap();
            assert!(validate(IDENTIFIER, &root).is_err(), "{relative}");
            #[cfg(feature = "isolated-qa")]
            assert!(archive_directory(IDENTIFIER, &root).is_err(), "{relative}");
            assert_eq!(
                std::fs::read(&sentinel).unwrap(),
                b"preserved external bytes"
            );
            assert_eq!(std::fs::read_dir(&external).unwrap().count(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn qa_preserves_only_internal_regular_file_archive_topic_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(temp.path()).unwrap();
        let root = canonical.join(IDENTIFIER);
        let topic = root.join("data/by-topic/topic");
        let target = root.join("data/2026-09-05");
        std::fs::create_dir_all(&topic).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let note = target.join("note.md");
        std::fs::write(&note, b"retained note").unwrap();
        let alias = topic.join("note.md");
        std::os::unix::fs::symlink("../../2026-09-05/note.md", &alias).unwrap();
        assert!(validate(IDENTIFIER, &root).is_ok());
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert!(
            validate(IDENTIFIER, &root).is_err(),
            "directory aliases are forbidden"
        );
        std::fs::remove_file(&alias).unwrap();
        let external = canonical.join("external.md");
        std::fs::write(&external, b"external sentinel").unwrap();
        std::os::unix::fs::symlink(&external, &alias).unwrap();
        assert!(
            validate(IDENTIFIER, &root).is_err(),
            "external leaf targets are forbidden"
        );
        assert_eq!(std::fs::read(external).unwrap(), b"external sentinel");
        assert_eq!(std::fs::read(note).unwrap(), b"retained note");
    }

    #[cfg(feature = "isolated-qa")]
    #[tokio::test]
    async fn qa_connector_never_polls_or_calls_inner_service() {
        use tower::{Service, ServiceExt};
        #[derive(Clone)]
        struct MustNotConnect;
        impl Service<()> for MustNotConnect {
            type Response = ();
            type Error = std::io::Error;
            type Future = std::future::Ready<Result<(), Self::Error>>;
            fn poll_ready(
                &mut self,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                panic!("inner readiness must not run");
            }
            fn call(&mut self, _: ()) -> Self::Future {
                panic!("inner DNS/connection must not run");
            }
        }
        let error = DenyConnector(MustNotConnect).oneshot(()).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[cfg(feature = "isolated-qa")]
    #[tokio::test]
    async fn qa_reqwest_guard_refuses_http_before_loopback_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = guard_http(reqwest::Client::builder()).build().unwrap();
        let error = client
            .get(format!("http://{address}/synthetic"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}
