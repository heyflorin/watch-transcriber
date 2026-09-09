//! R2 audio access: SigV4-signed GETs against the S3-compatible endpoint,
//! streaming Range passthrough for playback, and a size-capped LRU disk cache
//! filled in the background so replays go local.
//!
//! App-published object keys are immutable recording/generation identities.
//! Legacy friendly archive paths remain readable for existing watcher output.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub const CACHE_CAP_BYTES: u64 = 500 * 1024 * 1024;

/// Shared HTTP client with bundled webpki roots. Rationale: reqwest's default
/// rustls path verifies TLS through rustls-platform-verifier, which on Android
/// needs JNI + a Gradle-side Kotlin component and panics when absent. We talk
/// to exactly two hosts (github.com, r2.cloudflarestorage.com) — Mozilla's
/// bundled roots cover both on every platform with zero platform glue.
pub fn http() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        crate::qa::guard_http(reqwest::Client::builder())
            .use_preconfigured_tls(tls)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client")
    })
}

#[derive(Clone)]
pub struct R2Cfg {
    pub account_id: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct R2PutProof {
    pub key: String,
    pub recording_id: String,
    pub version_id: String,
    pub etag: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum R2DeleteOutcome {
    Deleted,
    AlreadyMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum R2ArchiveErrorKind {
    Configuration,
    Network,
    Conflict,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct R2ArchiveError {
    pub kind: R2ArchiveErrorKind,
    message: &'static str,
}

impl R2ArchiveError {
    fn new(kind: R2ArchiveErrorKind) -> Self {
        Self {
            kind,
            message: match kind {
                R2ArchiveErrorKind::Configuration => "R2 archive is not configured",
                R2ArchiveErrorKind::Network => "R2 archive network operation failed",
                R2ArchiveErrorKind::Conflict => "R2 archive key is already occupied",
                R2ArchiveErrorKind::Verification => "R2 archive verification failed",
            },
        }
    }

    fn configuration() -> Self {
        Self::new(R2ArchiveErrorKind::Configuration)
    }

    fn network() -> Self {
        Self::new(R2ArchiveErrorKind::Network)
    }

    fn conflict() -> Self {
        Self::new(R2ArchiveErrorKind::Conflict)
    }

    fn verification() -> Self {
        Self::new(R2ArchiveErrorKind::Verification)
    }
}

impl std::fmt::Display for R2ArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for R2ArchiveError {}

#[derive(Debug)]
enum HeadState {
    Missing,
    Verified(R2PutProof),
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::digest::KeyInit;
    let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("hmac accepts any key len");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// AWS SigV4 signing key derivation (kSecret -> kDate -> kRegion -> kService -> kSigning).
fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac_sha256(&k, region.as_bytes());
    let k = hmac_sha256(&k, service.as_bytes());
    hmac_sha256(&k, b"aws4_request")
}

/// RFC 3986 encode a single path segment (S3 canonical URI keeps `/` separators).
fn uri_encode_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build a signed GET/HEAD request for `key` ("" = bucket root) on the R2
/// S3 endpoint. Returns (url, headers) ready for reqwest.
pub fn sign_get(
    cfg: &R2Cfg,
    method: &str,
    key: &str,
    range: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> (String, Vec<(String, String)>) {
    let host = format!("{}.r2.cloudflarestorage.com", cfg.account_id);
    let canonical_uri = if key.is_empty() {
        format!("/{}", cfg.bucket)
    } else {
        let encoded: Vec<String> = key.split('/').map(uri_encode_segment).collect();
        format!("/{}/{}", cfg.bucket, encoded.join("/"))
    };
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let payload_hash = "UNSIGNED-PAYLOAD";

    // Canonical headers must be sorted; range is optional.
    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), host.clone()),
        ("x-amz-content-sha256".into(), payload_hash.into()),
        ("x-amz-date".into(), amz_date.clone()),
    ];
    if let Some(r) = range {
        headers.push(("range".into(), r.to_string()));
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers: String = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let scope = format!("{date}/auto/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let key_bytes = signing_key(&cfg.secret_access_key, &date, "auto", "s3");
    let signature = hex::encode(hmac_sha256(&key_bytes, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        cfg.access_key_id
    );

    let mut out: Vec<(String, String)> = headers.into_iter().filter(|(k, _)| k != "host").collect();
    out.push(("authorization".into(), authorization));
    (format!("https://{host}{canonical_uri}"), out)
}

/// Sign one fixed-size PUT. The caller-supplied SHA-256 is included both as
/// the payload hash and immutable object metadata so a subsequent HEAD can
/// verify identity without downloading the canonical audio again.
pub fn sign_put(
    cfg: &R2Cfg,
    key: &str,
    recording_id: &str,
    content_length: u64,
    sha256: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> (String, Vec<(String, String)>) {
    let host = format!("{}.r2.cloudflarestorage.com", cfg.account_id);
    let encoded: Vec<String> = key.split('/').map(uri_encode_segment).collect();
    let canonical_uri = format!("/{}/{}", cfg.bucket, encoded.join("/"));
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let mut headers = vec![
        ("content-length".to_owned(), content_length.to_string()),
        (
            "content-type".to_owned(),
            "application/octet-stream".to_owned(),
        ),
        ("host".to_owned(), host.clone()),
        ("if-none-match".to_owned(), "*".to_owned()),
        ("x-amz-content-sha256".to_owned(), sha256.to_owned()),
        ("x-amz-date".to_owned(), amz_date.clone()),
        (
            "x-amz-meta-echowall-recording-id".to_owned(),
            recording_id.to_owned(),
        ),
        ("x-amz-meta-echowall-sha256".to_owned(), sha256.to_owned()),
    ];
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request =
        format!("PUT\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{sha256}");
    let scope = format!("{date}/auto/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let signing = signing_key(&cfg.secret_access_key, &date, "auto", "s3");
    let signature = hex::encode(hmac_sha256(&signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        cfg.access_key_id
    );
    let mut request_headers: Vec<_> = headers
        .into_iter()
        .filter(|(name, _)| name != "host")
        .collect();
    request_headers.push(("authorization".to_owned(), authorization));
    (format!("https://{host}{canonical_uri}"), request_headers)
}

/// Upload and independently HEAD-verify one canonical archive object.
pub async fn put_file_verified(
    cfg: &R2Cfg,
    key: &str,
    recording_id: &str,
    source: &Path,
    expected_sha256: &str,
    expected_size_bytes: u64,
) -> Result<R2PutProof, R2ArchiveError> {
    if !valid_archive_identity(key, recording_id, expected_sha256, expected_size_bytes) {
        return Err(R2ArchiveError::configuration());
    }
    let metadata = std::fs::symlink_metadata(source).map_err(|_| R2ArchiveError::verification())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != expected_size_bytes
    {
        return Err(R2ArchiveError::verification());
    }
    let digest = crate::ingest::inbox::hash_file_streaming(source)
        .map_err(|_| R2ArchiveError::verification())?;
    if digest.sha256 != expected_sha256 || digest.size_bytes != expected_size_bytes {
        return Err(R2ArchiveError::verification());
    }

    // Reconcile before every write. This is what makes a retry after a lost
    // successful PUT response return the existing proof instead of creating a
    // second object version.
    match inspect_head(cfg, key, recording_id, expected_sha256, expected_size_bytes).await? {
        HeadState::Verified(proof) => return Ok(proof),
        HeadState::Missing => {}
    }

    let (url, headers) = sign_put(
        cfg,
        key,
        recording_id,
        expected_size_bytes,
        expected_sha256,
        chrono::Utc::now(),
    );
    let file = tokio::fs::File::open(source)
        .await
        .map_err(|_| R2ArchiveError::verification())?;
    let mut put_request = http().put(url).body(file);
    for (name, value) in headers {
        put_request = put_request.header(name, value);
    }
    let response = tokio::time::timeout(upload_deadline(expected_size_bytes), put_request.send())
        .await
        .map_err(|_| R2ArchiveError::network())?
        .map_err(|_| R2ArchiveError::network())?;
    if response.status().as_u16() == 412 {
        return match inspect_head(cfg, key, recording_id, expected_sha256, expected_size_bytes)
            .await?
        {
            HeadState::Verified(proof) => Ok(proof),
            HeadState::Missing => Err(R2ArchiveError::conflict()),
        };
    }
    if !response.status().is_success() {
        return Err(classify_r2_status(response.status().as_u16()));
    }
    let version_id = response
        .headers()
        .get("x-amz-version-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let put_etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    let mut proof =
        match inspect_head(cfg, key, recording_id, expected_sha256, expected_size_bytes).await? {
            HeadState::Verified(proof) => proof,
            HeadState::Missing => return Err(R2ArchiveError::verification()),
        };
    if proof.version_id.starts_with("sha256:") && !version_id.is_empty() {
        proof.version_id = version_id;
    }
    if proof.etag.is_empty() && !put_etag.is_empty() {
        proof.etag = put_etag;
    }
    if proof.etag.is_empty() {
        return Err(R2ArchiveError::verification());
    }
    Ok(proof)
}

/// Independently HEAD-verify one immutable archive object. The proof contains
/// no signed URL or credentials and is safe to persist in the processing
/// ledger.
pub async fn head_file_verified(
    cfg: &R2Cfg,
    key: &str,
    recording_id: &str,
    expected_sha256: &str,
    expected_size_bytes: u64,
) -> Result<R2PutProof, R2ArchiveError> {
    if !valid_archive_identity(key, recording_id, expected_sha256, expected_size_bytes) {
        return Err(R2ArchiveError::configuration());
    }
    let state = inspect_head(cfg, key, recording_id, expected_sha256, expected_size_bytes)
        .await
        .map_err(|error| {
            if error.kind == R2ArchiveErrorKind::Conflict {
                R2ArchiveError::verification()
            } else {
                error
            }
        })?;
    match state {
        HeadState::Verified(proof) => Ok(proof),
        HeadState::Missing => Err(R2ArchiveError::verification()),
    }
}

/// Delete an immutable archive object only after its persisted owner metadata
/// proves that it belongs to the requested recording.
pub async fn delete_owned_object(
    cfg: &R2Cfg,
    key: &str,
    recording_id: &str,
    generation: u64,
    expected_sha256: &str,
    expected_size_bytes: u64,
) -> Result<R2DeleteOutcome, R2ArchiveError> {
    if key.is_empty()
        || key.len() > 1_024
        || key.starts_with('/')
        || key.contains(['\\', '\0'])
        || key
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || archive_object_generation(key, recording_id) != Some(generation)
        || expected_sha256.len() != 64
        || !expected_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || expected_size_bytes == 0
    {
        return Err(R2ArchiveError::configuration());
    }
    let head = request(cfg, "HEAD", key, None)
        .await
        .map_err(|_| R2ArchiveError::network())?;
    if head.status().as_u16() == 404 {
        return Ok(R2DeleteOutcome::AlreadyMissing);
    }
    if !head.status().is_success() {
        return Err(classify_r2_status(head.status().as_u16()));
    }
    if head
        .headers()
        .get("x-amz-meta-echowall-recording-id")
        .and_then(|value| value.to_str().ok())
        != Some(recording_id)
        || head
            .headers()
            .get("x-amz-meta-echowall-sha256")
            .and_then(|value| value.to_str().ok())
            != Some(expected_sha256)
        || head
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            != Some(expected_size_bytes)
    {
        return Err(R2ArchiveError::verification());
    }

    let (url, headers) = sign_get(cfg, "DELETE", key, None, chrono::Utc::now());
    let mut delete = http().delete(url);
    for (name, value) in headers {
        delete = delete.header(name, value);
    }
    let response = tokio::time::timeout(std::time::Duration::from_secs(60), delete.send())
        .await
        .map_err(|_| R2ArchiveError::network())?
        .map_err(|_| R2ArchiveError::network())?;
    if response.status().as_u16() == 404 {
        return Ok(R2DeleteOutcome::AlreadyMissing);
    }
    if !response.status().is_success() {
        return Err(classify_r2_status(response.status().as_u16()));
    }
    let verify = request(cfg, "HEAD", key, None)
        .await
        .map_err(|_| R2ArchiveError::network())?;
    if verify.status().as_u16() != 404 {
        return Err(R2ArchiveError::verification());
    }
    Ok(R2DeleteOutcome::Deleted)
}

fn valid_archive_identity(
    key: &str,
    recording_id: &str,
    expected_sha256: &str,
    expected_size_bytes: u64,
) -> bool {
    !key.is_empty()
        && key.len() <= 1024
        && !key.starts_with('/')
        && !key.contains(['\\', '\0'])
        && !key
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && archive_object_generation(key, recording_id).is_some()
        && expected_sha256.len() == 64
        && expected_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && expected_size_bytes > 0
}

fn archive_object_generation(key: &str, recording_id: &str) -> Option<u64> {
    let parsed_id = uuid::Uuid::parse_str(recording_id).ok()?;
    if parsed_id.to_string() != recording_id {
        return None;
    }
    let mut components = Path::new(key).components();
    let day = match components.next()? {
        std::path::Component::Normal(value) => value.to_str()?,
        _ => return None,
    };
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let filename = match components.next()? {
        std::path::Component::Normal(value) => value.to_str()?,
        _ => return None,
    };
    if components.next().is_some()
        || filename.len() < 8
        || filename.as_bytes().get(6) != Some(&b'-')
        || !filename.as_bytes()[..6].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let (stem, extension) = filename.rsplit_once('.')?;
    if !matches!(extension, "wav" | "mp3" | "m4a") {
        return None;
    }
    let identity_suffix = format!("-{recording_id}");
    let before_id = stem.strip_suffix(&identity_suffix)?;
    let (_, generation) = before_id.rsplit_once("-g")?;
    let generation = generation.parse::<u64>().ok()?;
    (generation > 0).then_some(generation)
}

pub(crate) fn is_generation_owned_key(key: &str, recording_id: &str, generation: u64) -> bool {
    archive_object_generation(key, recording_id) == Some(generation)
}

async fn inspect_head(
    cfg: &R2Cfg,
    key: &str,
    recording_id: &str,
    expected_sha256: &str,
    expected_size_bytes: u64,
) -> Result<HeadState, R2ArchiveError> {
    let response = request(cfg, "HEAD", key, None)
        .await
        .map_err(|_| R2ArchiveError::network())?;
    head_state(
        response.status().as_u16(),
        response.headers(),
        key,
        recording_id,
        expected_sha256,
        expected_size_bytes,
    )
}

fn head_state(
    status: u16,
    headers: &reqwest::header::HeaderMap,
    key: &str,
    recording_id: &str,
    expected_sha256: &str,
    expected_size_bytes: u64,
) -> Result<HeadState, R2ArchiveError> {
    if status == 404 {
        return Ok(HeadState::Missing);
    }
    if !(200..300).contains(&status) {
        return Err(classify_r2_status(status));
    }
    let size = headers
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let sha256 = headers
        .get("x-amz-meta-echowall-sha256")
        .and_then(|value| value.to_str().ok());
    let owner = headers
        .get("x-amz-meta-echowall-recording-id")
        .and_then(|value| value.to_str().ok());
    if size != Some(expected_size_bytes)
        || sha256 != Some(expected_sha256)
        || owner != Some(recording_id)
    {
        return Err(R2ArchiveError::conflict());
    }
    let etag = headers
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(R2ArchiveError::verification)?
        .to_owned();
    let version_id = headers
        .get("x-amz-version-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("sha256:{expected_sha256}"));
    Ok(HeadState::Verified(R2PutProof {
        key: key.to_owned(),
        recording_id: recording_id.to_owned(),
        version_id,
        etag,
        sha256: expected_sha256.to_owned(),
        size_bytes: expected_size_bytes,
    }))
}

fn classify_r2_status(status: u16) -> R2ArchiveError {
    match status {
        401 | 403 => R2ArchiveError::configuration(),
        409 | 412 => R2ArchiveError::conflict(),
        500..=599 => R2ArchiveError::network(),
        _ => R2ArchiveError::verification(),
    }
}

fn upload_deadline(size_bytes: u64) -> std::time::Duration {
    const BYTES_PER_SECOND_FLOOR: u64 = 64 * 1024;
    const BASE_SECONDS: u64 = 120;
    const MAX_SECONDS: u64 = 6 * 60 * 60;
    let transfer_seconds = size_bytes.div_ceil(BYTES_PER_SECOND_FLOOR);
    std::time::Duration::from_secs(
        BASE_SECONDS
            .saturating_add(transfer_seconds)
            .min(MAX_SECONDS),
    )
}

/// Signed GET (or HEAD) against R2. `range` passes through for 206 playback.
pub async fn request(
    cfg: &R2Cfg,
    method: &str,
    key: &str,
    range: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    let (url, headers) = sign_get(cfg, method, key, range, chrono::Utc::now());
    let mut req = match method {
        "HEAD" => http().head(&url),
        _ => http().get(&url),
    };
    for (k, v) in headers {
        req = req.header(k, v);
    }
    req.send().await
}

/// Download `key` fully to `dest` (tmp + rename). Used by pin and cache fill.
pub async fn download_to(cfg: &R2Cfg, key: &str, dest: &Path) -> Result<(), String> {
    let resp = request(cfg, "GET", key, None)
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("R2 GET {key}: {}", resp.status()));
    }
    const MAX_AUDIO_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;
    if resp
        .content_length()
        .is_some_and(|size| size > MAX_AUDIO_DOWNLOAD_BYTES)
    {
        return Err("R2 audio is too large".to_owned());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let parent = dest
        .parent()
        .ok_or_else(|| "R2 cache destination is invalid".to_owned())?;
    let tmp = parent.join(format!(".r2-download-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp)
        .map_err(|_| "R2 cache destination is unavailable".to_owned())?;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    use std::io::Write;
    let mut received = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => {
                let _ = std::fs::remove_file(&tmp);
                return Err("R2 cache download failed".to_owned());
            }
        };
        received = received.saturating_add(chunk.len() as u64);
        if received > MAX_AUDIO_DOWNLOAD_BYTES {
            let _ = std::fs::remove_file(&tmp);
            return Err("R2 audio is too large".to_owned());
        }
        if file.write_all(&chunk).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Err("R2 cache write failed".to_owned());
        }
    }
    if file.flush().and_then(|()| file.sync_all()).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("R2 cache write failed".to_owned());
    }
    drop(file);
    if std::fs::rename(&tmp, dest).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("R2 cache install failed".to_owned());
    }
    Ok(())
}

/// Kick a background full download of `key` into the cache (deduped), then
/// enforce the LRU cap. Cheap to call on every proxied request.
pub fn spawn_cache_fill(
    cfg: R2Cfg,
    cache_dir: PathBuf,
    dest: PathBuf,
    key: String,
    inflight: Arc<Mutex<HashSet<String>>>,
) {
    {
        let mut set = inflight.lock().unwrap();
        if !set.insert(key.clone()) {
            return;
        }
    }
    tokio::spawn(async move {
        if !dest.exists() {
            if let Err(e) = download_to(&cfg, &key, &dest).await {
                eprintln!("cache fill {key}: {e}");
            } else {
                evict_lru(&cache_dir, CACHE_CAP_BYTES);
            }
        }
        inflight.lock().unwrap().remove(&key);
    });
}

/// Delete oldest-accessed cache files until total size fits the cap.
pub fn evict_lru(cache_dir: &Path, cap: u64) {
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut walk = vec![cache_dir.to_path_buf()];
    while let Some(dir) = walk.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk.push(p);
            } else if let Ok(md) = e.metadata() {
                let atime = md.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                files.push((p, md.len(), atime));
            }
        }
    }
    let mut total: u64 = files.iter().map(|(_, s, _)| s).sum();
    if total <= cap {
        return;
    }
    files.sort_by_key(|(_, _, t)| *t);
    for (path, size, _) in files {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Bump mtime so LRU eviction sees this file as recently used. (No shelling
/// out — iOS forbids spawning processes.)
pub fn touch(path: &Path) {
    let _ = filetime::set_file_mtime(path, filetime::FileTime::now());
}

#[cfg(test)]
mod tests {
    use super::*;

    // AWS SigV4 documented test vector: secret wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY,
    // date 20150830, us-east-1/iam derives this signing key.
    #[test]
    fn sigv4_signing_key_matches_aws_vector() {
        let k = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        );
        assert_eq!(
            hex::encode(k),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn sign_get_shape() {
        let cfg = R2Cfg {
            account_id: "0123456789abcdef0123456789abcdef".into(),
            access_key_id: "AKID".into(),
            secret_access_key: "SECRET".into(),
            bucket: "watch-transcriber-audio".into(),
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-28T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let (url, headers) = sign_get(
            &cfg,
            "GET",
            "2026-07-20/213456-测试.m4a",
            Some("bytes=0-99"),
            now,
        );
        assert!(url.starts_with(
            "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com/watch-transcriber-audio/2026-07-20/"
        ));
        assert!(url.contains("%E6%B5%8B%E8%AF%95")); // path segment percent-encoded
        let auth = &headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .unwrap()
            .1;
        assert!(auth.contains("Credential=AKID/20260728/auto/s3/aws4_request"));
        assert!(auth.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    }

    #[test]
    fn sign_put_binds_size_hash_and_metadata_without_exposing_secret() {
        let cfg = R2Cfg {
            account_id: "0123456789abcdef0123456789abcdef".into(),
            access_key_id: "AKID".into(),
            secret_access_key: "DO-NOT-LEAK".into(),
            bucket: "watch-transcriber-audio".into(),
        };
        let recording_id = "018f92d8-6ad4-7dc1-8e28-8b020d2942cb";
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-28T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let hash = "a".repeat(64);
        let (url, headers) = sign_put(&cfg, "2026-07-20/audio.wav", recording_id, 42, &hash, now);
        assert!(!url.contains("DO-NOT-LEAK"));
        assert!(headers
            .iter()
            .all(|(_, value)| !value.contains("DO-NOT-LEAK")));
        assert!(headers
            .iter()
            .any(|(name, value)| name == "content-length" && value == "42"));
        assert!(headers
            .iter()
            .any(|(name, value)| name == "if-none-match" && value == "*"));
        assert!(headers
            .iter()
            .any(|(name, value)| { name == "x-amz-meta-echowall-sha256" && value == &hash }));
        assert!(headers.iter().any(|(name, value)| {
            name == "x-amz-meta-echowall-recording-id" && value == recording_id
        }));
        let authorization = headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value)
            .unwrap();
        assert!(authorization.contains(
            "SignedHeaders=content-length;content-type;host;if-none-match;x-amz-content-sha256;x-amz-date;x-amz-meta-echowall-recording-id;x-amz-meta-echowall-sha256"
        ));
    }

    #[test]
    fn immutable_archive_key_binds_recording_and_generation() {
        let recording_id = "018f92d8-6ad4-7dc1-8e28-8b020d2942cb";
        let key = format!("2026-07-20/213456-meeting-g7-{recording_id}.m4a");
        assert!(is_generation_owned_key(&key, recording_id, 7));
        assert!(!is_generation_owned_key(&key, recording_id, 8));
        assert!(!is_generation_owned_key(
            "2026-07-20/213456-meeting.m4a",
            recording_id,
            7
        ));
        assert!(!is_generation_owned_key(
            "2026-07-20/213456-meeting-g7-018f92d8-6ad4-7dc1-8e28-8b020d2942cc.m4a",
            recording_id,
            7
        ));
    }

    #[test]
    fn head_reconciliation_distinguishes_missing_matching_and_occupied_objects() {
        let hash = "a".repeat(64);
        let recording_id = "018f92d8-6ad4-7dc1-8e28-8b020d2942cb";
        let missing = head_state(
            404,
            &reqwest::header::HeaderMap::new(),
            "day/audio.wav",
            recording_id,
            &hash,
            42,
        )
        .unwrap();
        assert!(matches!(missing, HeadState::Missing));

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-length", "42".parse().unwrap());
        headers.insert(
            "x-amz-meta-echowall-recording-id",
            recording_id.parse().unwrap(),
        );
        headers.insert("x-amz-meta-echowall-sha256", hash.parse().unwrap());
        headers.insert("etag", "\"fabricated-etag\"".parse().unwrap());
        headers.insert("x-amz-version-id", "version-1".parse().unwrap());
        let matched = head_state(200, &headers, "day/audio.wav", recording_id, &hash, 42).unwrap();
        let HeadState::Verified(proof) = matched else {
            panic!("matching HEAD must return a durable proof");
        };
        assert_eq!(proof.key, "day/audio.wav");
        assert_eq!(proof.recording_id, recording_id);
        assert_eq!(proof.version_id, "version-1");
        assert_eq!(proof.sha256, hash);
        assert_eq!(proof.size_bytes, 42);

        headers.insert("content-length", "43".parse().unwrap());
        let occupied =
            head_state(200, &headers, "day/audio.wav", recording_id, &hash, 42).unwrap_err();
        assert_eq!(occupied.kind, R2ArchiveErrorKind::Conflict);
        headers.insert(
            "x-amz-meta-echowall-recording-id",
            "118f92d8-6ad4-7dc1-8e28-8b020d2942cb".parse().unwrap(),
        );
        assert_eq!(
            head_state(200, &headers, "day/audio.wav", recording_id, &hash, 43,)
                .unwrap_err()
                .kind,
            R2ArchiveErrorKind::Conflict
        );
        assert_eq!(classify_r2_status(412).kind, R2ArchiveErrorKind::Conflict);
    }

    #[test]
    fn upload_deadline_is_size_aware_and_bounded() {
        assert_eq!(upload_deadline(1), std::time::Duration::from_secs(121));
        assert!(upload_deadline(1024 * 1024 * 1024) > std::time::Duration::from_secs(60 * 60));
        assert_eq!(
            upload_deadline(u64::MAX),
            std::time::Duration::from_secs(6 * 60 * 60)
        );
    }
}
