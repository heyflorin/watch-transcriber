//! Direct TOS object operations through a minimal TOS4-signed HTTP adapter.
//!
//! The signing shape is ported from Volcano Engine's Apache-2.0 official Rust
//! SDK. EchoWall owns only the four operations it uses (PUT, HEAD, DELETE, and
//! presigned GET) so the App does not inherit that SDK's obsolete HTTP/DNS/TLS
//! dependency graph. The processing ledger owns retry/reconciliation.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use hmac::{Hmac, Mac};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HOST,
};
use sha2::{Digest as ShaDigest, Sha256};
use tokio_util::io::ReaderStream;

use crate::secrets::ProcessingCredentials;

use super::providers::SecretText;

const SHA_METADATA_KEY: &str = "echowall-sha256";
const RECORDING_METADATA_KEY: &str = "echowall-recording-id";
const MIN_PRESIGN_SECONDS: i64 = 60;
const MAX_PRESIGN_SECONDS: i64 = 7_200;
const EMPTY_HASH_PAYLOAD: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
const TOS_ALGORITHM: &str = "TOS4-HMAC-SHA256";

pub struct TosConfiguration {
    access_key: SecretText,
    secret_key: SecretText,
    bucket: String,
    region: String,
    endpoint: String,
}

impl TosConfiguration {
    #[allow(dead_code)]
    pub(crate) fn from_credentials(
        credentials: &ProcessingCredentials,
    ) -> Result<Self, TosErrorSafe> {
        let configuration = Self {
            access_key: SecretText::new(credentials.tos_access_key().to_owned())
                .map_err(|_| TosErrorSafe::configuration())?,
            secret_key: SecretText::new(credentials.tos_secret_key().to_owned())
                .map_err(|_| TosErrorSafe::configuration())?,
            bucket: credentials.tos_bucket().to_owned(),
            region: credentials.tos_region().to_owned(),
            endpoint: normalize_endpoint(credentials.tos_endpoint())?,
        };
        configuration.validate()?;
        Ok(configuration)
    }

    #[cfg(test)]
    fn fabricated() -> Self {
        Self {
            access_key: SecretText::new("AKLTfabricated123".to_owned()).unwrap(),
            secret_key: SecretText::new("fabricated-secret-123456789".to_owned()).unwrap(),
            bucket: "echowall-private".to_owned(),
            region: "cn-hongkong".to_owned(),
            endpoint: "https://tos-cn-hongkong.volces.com".to_owned(),
        }
    }

    fn validate(&self) -> Result<(), TosErrorSafe> {
        let endpoint =
            reqwest::Url::parse(&self.endpoint).map_err(|_| TosErrorSafe::configuration())?;
        let endpoint_host = endpoint
            .host_str()
            .ok_or_else(TosErrorSafe::configuration)?;
        if endpoint.scheme() != "https"
            || !is_official_tos_endpoint_host(endpoint_host)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(endpoint.path(), "" | "/")
            || !valid_bucket(&self.bucket)
            || !valid_region(&self.region)
        {
            return Err(TosErrorSafe::configuration());
        }
        Ok(())
    }

    fn client(&self) -> Result<reqwest::Client, TosErrorSafe> {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        crate::qa::guard_http(reqwest::Client::builder())
            .use_preconfigured_tls(tls)
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(300))
            .timeout(Duration::from_secs(300))
            .no_proxy()
            .build()
            .map_err(|_| TosErrorSafe::configuration())
    }
}

impl std::fmt::Debug for TosConfiguration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TosConfiguration")
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("bucket", &"<configured>")
            .field("region", &"<configured>")
            .field("endpoint", &"<configured>")
            .finish()
    }
}

#[allow(dead_code)]
fn normalize_endpoint(value: &str) -> Result<String, TosErrorSafe> {
    if value.contains("://") {
        Ok(value.trim_end_matches('/').to_owned())
    } else {
        Ok(format!("https://{}", value.trim_end_matches('/')))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TosObjectReceipt {
    pub bucket: String,
    pub key: String,
    pub version_id: String,
    pub etag: String,
    pub sha256: String,
    pub size_bytes: u64,
}

impl std::fmt::Debug for TosObjectReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TosObjectReceipt")
            .field("bucket", &"<redacted>")
            .field("key", &"<redacted>")
            .field("version_id", &"<redacted>")
            .field("etag", &"<redacted>")
            .field("sha256", &"<redacted>")
            .field("size_bytes", &self.size_bytes)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TosErrorKind {
    Configuration,
    Network,
    NotFound,
    Conflict,
    Verification,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TosErrorSafe {
    pub kind: TosErrorKind,
    message: &'static str,
}

impl TosErrorSafe {
    fn new(kind: TosErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    fn configuration() -> Self {
        Self::new(TosErrorKind::Configuration, "TOS configuration is invalid")
    }

    pub(crate) fn network() -> Self {
        Self::new(TosErrorKind::Network, "TOS request failed")
    }

    fn conflict() -> Self {
        Self::new(
            TosErrorKind::Conflict,
            "TOS object conflicts with the recording",
        )
    }

    fn verification() -> Self {
        Self::new(
            TosErrorKind::Verification,
            "TOS object could not be verified",
        )
    }
}

impl std::fmt::Display for TosErrorSafe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TosErrorSafe {}

pub struct DirectTos {
    configuration: TosConfiguration,
}

impl DirectTos {
    pub fn new(configuration: TosConfiguration) -> Result<Self, TosErrorSafe> {
        configuration.validate()?;
        Ok(Self { configuration })
    }

    pub async fn probe_expected(
        &self,
        recording_id: &str,
        key: &str,
        expected_sha256: &str,
        expected_size_bytes: u64,
        expected_crc64: u64,
    ) -> Result<Option<TosObjectReceipt>, TosErrorSafe> {
        validate_recording_identity(recording_id, key, expected_sha256, expected_size_bytes)?;
        let request = signed_request(
            &self.configuration,
            "HEAD",
            key,
            &BTreeMap::new(),
            BTreeMap::new(),
            chrono::Utc::now(),
        )?;
        let response = self
            .configuration
            .client()?
            .head(request.url)
            .headers(request.headers)
            .send()
            .await
            .map_err(|_| TosErrorSafe::network())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(classify_http_status(response.status()));
        };
        verify_head_headers(
            ExpectedObjectIdentity {
                bucket: &self.configuration.bucket,
                key,
                recording_id,
                sha256: expected_sha256,
                size_bytes: expected_size_bytes,
                crc64: expected_crc64,
                version_id: None,
                etag: None,
            },
            response.headers(),
        )
        .map(Some)
    }

    pub async fn probe_source_expected(
        &self,
        recording_id: &str,
        key: &str,
        source: &Path,
        expected_sha256: &str,
        expected_size_bytes: u64,
    ) -> Result<Option<TosObjectReceipt>, TosErrorSafe> {
        validate_recording_identity(recording_id, key, expected_sha256, expected_size_bytes)?;
        let metadata =
            std::fs::symlink_metadata(source).map_err(|_| TosErrorSafe::verification())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != expected_size_bytes
        {
            return Err(TosErrorSafe::verification());
        }
        let fingerprint = hash_and_crc(source)?;
        if fingerprint.sha256 != expected_sha256 || fingerprint.size_bytes != expected_size_bytes {
            return Err(TosErrorSafe::verification());
        }
        self.probe_expected(
            recording_id,
            key,
            expected_sha256,
            expected_size_bytes,
            fingerprint.crc64,
        )
        .await
    }

    pub async fn upload_verified(
        &self,
        recording_id: &str,
        key: &str,
        source: &Path,
        expected_sha256: &str,
        expected_size_bytes: u64,
    ) -> Result<TosObjectReceipt, TosErrorSafe> {
        validate_recording_identity(recording_id, key, expected_sha256, expected_size_bytes)?;
        let metadata =
            std::fs::symlink_metadata(source).map_err(|_| TosErrorSafe::verification())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != expected_size_bytes
        {
            return Err(TosErrorSafe::verification());
        }
        let initial = hash_and_crc(source)?;
        if initial.size_bytes != expected_size_bytes || initial.sha256 != expected_sha256 {
            return Err(TosErrorSafe::verification());
        }
        if let Some(existing) = self
            .probe_expected(
                recording_id,
                key,
                expected_sha256,
                expected_size_bytes,
                initial.crc64,
            )
            .await?
        {
            return Ok(existing);
        }

        let headers = put_headers(key, recording_id, expected_sha256, expected_size_bytes)?;
        let request = signed_request(
            &self.configuration,
            "PUT",
            key,
            &BTreeMap::new(),
            headers,
            chrono::Utc::now(),
        )?;
        let file = tokio::fs::File::open(source)
            .await
            .map_err(|_| TosErrorSafe::verification())?;
        let response = self
            .configuration
            .client()?
            .put(request.url)
            .headers(request.headers)
            .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
            .send()
            .await
            .map_err(|_| TosErrorSafe::network())?;
        if matches!(response.status().as_u16(), 409 | 412) {
            return self
                .probe_expected(
                    recording_id,
                    key,
                    expected_sha256,
                    expected_size_bytes,
                    initial.crc64,
                )
                .await?
                .ok_or_else(TosErrorSafe::conflict);
        }
        if !response.status().is_success() {
            return Err(classify_http_status(response.status()));
        }
        let version_id = required_header(response.headers(), "x-tos-version-id")?;
        let etag = required_header(response.headers(), ETAG.as_str())?;
        let crc64 = required_header(response.headers(), "x-tos-hash-crc64ecma")?
            .parse::<u64>()
            .map_err(|_| TosErrorSafe::verification())?;
        if crc64 != initial.crc64 {
            return Err(TosErrorSafe::verification());
        }
        let receipt = TosObjectReceipt {
            bucket: self.configuration.bucket.clone(),
            key: key.to_owned(),
            version_id,
            etag,
            sha256: expected_sha256.to_owned(),
            size_bytes: expected_size_bytes,
        };
        let final_fingerprint = hash_and_crc(source)?;
        if final_fingerprint != initial {
            let _ = self.delete_exact(&receipt).await;
            return Err(TosErrorSafe::verification());
        }
        self.probe_exact(recording_id, &receipt, initial.crc64)
            .await
    }

    pub async fn presign_exact_get(
        &self,
        receipt: &TosObjectReceipt,
        expires_seconds: i64,
    ) -> Result<String, TosErrorSafe> {
        self.validate_receipt(receipt)?;
        if !(MIN_PRESIGN_SECONDS..=MAX_PRESIGN_SECONDS).contains(&expires_seconds) {
            return Err(TosErrorSafe::configuration());
        }
        let url = presign_get(
            &self.configuration,
            receipt,
            expires_seconds,
            chrono::Utc::now(),
        )?;
        if !valid_exact_presigned_url(&url, receipt) {
            return Err(TosErrorSafe::verification());
        }
        Ok(url)
    }

    pub async fn delete_exact(&self, receipt: &TosObjectReceipt) -> Result<(), TosErrorSafe> {
        self.validate_receipt(receipt)?;
        let query = BTreeMap::from([("versionId".to_owned(), receipt.version_id.clone())]);
        let headers = BTreeMap::from([("x-tos-if-match".to_owned(), receipt.etag.clone())]);
        let request = signed_request(
            &self.configuration,
            "DELETE",
            &receipt.key,
            &query,
            headers,
            chrono::Utc::now(),
        )?;
        let response = self
            .configuration
            .client()?
            .delete(request.url)
            .headers(request.headers)
            .send()
            .await
            .map_err(|_| TosErrorSafe::network())?;
        match response.status().as_u16() {
            200..=299 | 404 => Ok(()),
            409 | 412 => Err(TosErrorSafe::conflict()),
            _ => Err(classify_http_status(response.status())),
        }
    }

    fn validate_receipt(&self, receipt: &TosObjectReceipt) -> Result<(), TosErrorSafe> {
        validate_object_identity(&receipt.key, &receipt.sha256, receipt.size_bytes)?;
        if receipt.bucket != self.configuration.bucket
            || !valid_opaque_header(&receipt.version_id)
            || !valid_opaque_header(&receipt.etag)
        {
            return Err(TosErrorSafe::verification());
        }
        Ok(())
    }

    async fn probe_exact(
        &self,
        recording_id: &str,
        receipt: &TosObjectReceipt,
        expected_crc64: u64,
    ) -> Result<TosObjectReceipt, TosErrorSafe> {
        self.validate_receipt(receipt)?;
        let query = BTreeMap::from([("versionId".to_owned(), receipt.version_id.clone())]);
        let mut headers = BTreeMap::new();
        headers.insert("if-match".to_owned(), receipt.etag.clone());
        let request = signed_request(
            &self.configuration,
            "HEAD",
            &receipt.key,
            &query,
            headers,
            chrono::Utc::now(),
        )?;
        let response = self
            .configuration
            .client()?
            .head(request.url)
            .headers(request.headers)
            .send()
            .await
            .map_err(|_| TosErrorSafe::network())?;
        if !response.status().is_success() {
            return Err(classify_http_status(response.status()));
        }
        verify_head_headers(
            ExpectedObjectIdentity {
                bucket: &receipt.bucket,
                key: &receipt.key,
                recording_id,
                sha256: &receipt.sha256,
                size_bytes: receipt.size_bytes,
                crc64: expected_crc64,
                version_id: Some(&receipt.version_id),
                etag: Some(&receipt.etag),
            },
            response.headers(),
        )
    }
}

impl std::fmt::Debug for DirectTos {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DirectTos")
            .field("configuration", &self.configuration)
            .finish()
    }
}

fn validate_object_identity(
    key: &str,
    sha256: &str,
    size_bytes: u64,
) -> Result<uuid::Uuid, TosErrorSafe> {
    let safe_hash = sha256.len() == 64
        && sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let mut parts = key.split('/');
    let namespace = (parts.next(), parts.next(), parts.next());
    let recording_id = parts
        .next()
        .and_then(|value| uuid::Uuid::parse_str(value).ok());
    let filename = parts.next();
    let filename_hash = filename
        .and_then(|value| value.rsplit_once('.'))
        .filter(|(_, extension)| matches!(*extension, "wav" | "mp3" | "m4a"))
        .map(|(hash, _)| hash);
    let safe_key = key.len() <= 256
        && namespace == (Some("echowall"), Some("processing"), Some("v1"))
        && filename_hash == Some(sha256)
        && parts.next().is_none()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.'));
    if !safe_key || !safe_hash || size_bytes == 0 || size_bytes > i64::MAX as u64 {
        return Err(TosErrorSafe::verification());
    }
    recording_id.ok_or_else(TosErrorSafe::verification)
}

fn validate_recording_identity(
    recording_id: &str,
    key: &str,
    sha256: &str,
    size_bytes: u64,
) -> Result<(), TosErrorSafe> {
    let expected = uuid::Uuid::parse_str(recording_id).map_err(|_| TosErrorSafe::verification())?;
    if validate_object_identity(key, sha256, size_bytes)? != expected {
        return Err(TosErrorSafe::verification());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalFingerprint {
    sha256: String,
    crc64: u64,
    size_bytes: u64,
}

fn hash_and_crc(path: &Path) -> Result<LocalFingerprint, TosErrorSafe> {
    let mut file = std::fs::File::open(path).map_err(|_| TosErrorSafe::verification())?;
    let mut sha256 = Sha256::new();
    let mut crc64 = crc64fast::Digest::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| TosErrorSafe::verification())?;
        if read == 0 {
            break;
        }
        sha256.update(&buffer[..read]);
        crc64.write(&buffer[..read]);
        size_bytes = size_bytes
            .checked_add(read as u64)
            .ok_or_else(TosErrorSafe::verification)?;
    }
    Ok(LocalFingerprint {
        sha256: hex::encode(sha256.finalize()),
        crc64: crc64.sum64(),
        size_bytes,
    })
}

struct ExpectedObjectIdentity<'a> {
    bucket: &'a str,
    key: &'a str,
    recording_id: &'a str,
    sha256: &'a str,
    size_bytes: u64,
    crc64: u64,
    version_id: Option<&'a str>,
    etag: Option<&'a str>,
}

fn verify_head_headers(
    expected: ExpectedObjectIdentity<'_>,
    headers: &HeaderMap,
) -> Result<TosObjectReceipt, TosErrorSafe> {
    let actual_size = required_header(headers, CONTENT_LENGTH.as_str())?
        .parse::<u64>()
        .map_err(|_| TosErrorSafe::verification())?;
    let actual_crc64 = required_header(headers, "x-tos-hash-crc64ecma")?
        .parse::<u64>()
        .map_err(|_| TosErrorSafe::verification())?;
    let actual_sha256 = required_header(headers, "x-tos-meta-echowall-sha256")?;
    let actual_recording_id = required_header(headers, "x-tos-meta-echowall-recording-id")?;
    if actual_size != expected.size_bytes
        || actual_crc64 != expected.crc64
        || actual_sha256 != expected.sha256
        || actual_recording_id != expected.recording_id
    {
        return Err(TosErrorSafe::conflict());
    }
    let version_id = required_header(headers, "x-tos-version-id")?;
    let etag = required_header(headers, ETAG.as_str())?;
    if !valid_opaque_header(&version_id)
        || !valid_opaque_header(&etag)
        || expected
            .version_id
            .is_some_and(|expected| version_id != expected)
        || expected.etag.is_some_and(|expected| etag != expected)
    {
        return Err(TosErrorSafe::verification());
    }
    Ok(TosObjectReceipt {
        bucket: expected.bucket.to_owned(),
        key: expected.key.to_owned(),
        version_id,
        etag,
        sha256: expected.sha256.to_owned(),
        size_bytes: expected.size_bytes,
    })
}

fn required_header(headers: &HeaderMap, name: &str) -> Result<String, TosErrorSafe> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_opaque_header(value))
        .map(str::to_owned)
        .ok_or_else(TosErrorSafe::verification)
}

fn put_headers(
    key: &str,
    recording_id: &str,
    sha256: &str,
    size_bytes: u64,
) -> Result<BTreeMap<String, String>, TosErrorSafe> {
    let content_type = object_content_type(key).ok_or_else(TosErrorSafe::verification)?;
    Ok(BTreeMap::from([
        (CONTENT_LENGTH.as_str().to_owned(), size_bytes.to_string()),
        (CONTENT_TYPE.as_str().to_owned(), content_type.to_owned()),
        ("x-tos-forbid-overwrite".to_owned(), "true".to_owned()),
        (format!("x-tos-meta-{SHA_METADATA_KEY}"), sha256.to_owned()),
        (
            format!("x-tos-meta-{RECORDING_METADATA_KEY}"),
            recording_id.to_owned(),
        ),
    ]))
}

fn object_content_type(key: &str) -> Option<&'static str> {
    match Path::new(key).extension()?.to_str()? {
        "wav" => Some("audio/wav"),
        "mp3" => Some("audio/mpeg"),
        "m4a" => Some("audio/mp4"),
        _ => None,
    }
}

struct SignedRequest {
    url: String,
    headers: HeaderMap,
}

fn signed_request(
    configuration: &TosConfiguration,
    method: &str,
    key: &str,
    query: &BTreeMap<String, String>,
    mut headers: BTreeMap<String, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<SignedRequest, TosErrorSafe> {
    if !matches!(method, "PUT" | "HEAD" | "DELETE") {
        return Err(TosErrorSafe::configuration());
    }
    let host = object_host(configuration)?;
    let long_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = now.format("%Y%m%d").to_string();
    headers.insert(HOST.as_str().to_owned(), host.clone());
    headers.insert("x-tos-date".to_owned(), long_date.clone());
    let (canonical_headers, signed_headers) = canonical_headers(&headers)?;
    let canonical_query = canonical_query(query);
    let canonical_uri = format!("/{}", encode_path(key));
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{EMPTY_HASH_PAYLOAD}"
    );
    let scope = format!("{short_date}/{}/tos/request", configuration.region);
    let string_to_sign = format!(
        "{TOS_ALGORITHM}\n{long_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let signature = tos_signature(
        configuration.secret_key.expose(),
        &short_date,
        &configuration.region,
        &string_to_sign,
    );
    headers.insert(
        "authorization".to_owned(),
        format!(
            "{TOS_ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            configuration.access_key.expose()
        ),
    );
    Ok(SignedRequest {
        url: object_url(&host, key, &canonical_query),
        headers: to_header_map(headers)?,
    })
}

fn presign_get(
    configuration: &TosConfiguration,
    receipt: &TosObjectReceipt,
    expires_seconds: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String, TosErrorSafe> {
    let host = object_host(configuration)?;
    let long_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = now.format("%Y%m%d").to_string();
    let scope = format!("{short_date}/{}/tos/request", configuration.region);
    let query = BTreeMap::from([
        ("X-Tos-Algorithm".to_owned(), TOS_ALGORITHM.to_owned()),
        (
            "X-Tos-Credential".to_owned(),
            format!("{}/{scope}", configuration.access_key.expose()),
        ),
        ("X-Tos-Date".to_owned(), long_date.clone()),
        ("X-Tos-Expires".to_owned(), expires_seconds.to_string()),
        ("X-Tos-SignedHeaders".to_owned(), "host".to_owned()),
        ("versionId".to_owned(), receipt.version_id.clone()),
    ]);
    let canonical_query = canonical_query(&query);
    let canonical_uri = format!("/{}", encode_path(&receipt.key));
    let canonical_headers = format!("host:{host}\n");
    let canonical_request = format!(
        "GET\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\nhost\n{UNSIGNED_PAYLOAD}"
    );
    let string_to_sign = format!(
        "{TOS_ALGORITHM}\n{long_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let signature = tos_signature(
        configuration.secret_key.expose(),
        &short_date,
        &configuration.region,
        &string_to_sign,
    );
    Ok(format!(
        "{}&X-Tos-Signature={signature}",
        object_url(&host, &receipt.key, &canonical_query)
    ))
}

fn object_host(configuration: &TosConfiguration) -> Result<String, TosErrorSafe> {
    let endpoint =
        reqwest::Url::parse(&configuration.endpoint).map_err(|_| TosErrorSafe::configuration())?;
    let host = endpoint
        .host_str()
        .ok_or_else(TosErrorSafe::configuration)?;
    Ok(format!("{}.{}", configuration.bucket, host))
}

fn object_url(host: &str, key: &str, canonical_query: &str) -> String {
    let base = format!("https://{host}/{}", encode_path(key));
    if canonical_query.is_empty() {
        base
    } else {
        format!("{base}?{canonical_query}")
    }
}

fn canonical_headers(headers: &BTreeMap<String, String>) -> Result<(String, String), TosErrorSafe> {
    let selected = headers
        .iter()
        .filter(|(name, _)| {
            name.as_str() == HOST.as_str()
                || name.as_str() == CONTENT_TYPE.as_str()
                || name.starts_with("x-tos-")
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(TosErrorSafe::configuration());
    }
    let mut canonical = String::new();
    let mut names = Vec::with_capacity(selected.len());
    for (name, value) in selected {
        let normalized_name = name.to_ascii_lowercase();
        let normalized_value = value.trim();
        if normalized_value.is_empty() || normalized_value.contains(['\r', '\n']) {
            return Err(TosErrorSafe::configuration());
        }
        canonical.push_str(&normalized_name);
        canonical.push(':');
        canonical.push_str(normalized_value);
        canonical.push('\n');
        names.push(normalized_name);
    }
    Ok((canonical, names.join(";")))
}

fn canonical_query(query: &BTreeMap<String, String>) -> String {
    query
        .iter()
        .map(|(name, value)| format!("{}={}", uri_encode(name), uri_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(uri_encode)
        .collect::<Vec<_>>()
        .join("/")
}

fn uri_encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                output.push(char::from(byte));
            }
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

fn sha256_hex(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Vec<u8> {
    use hmac::digest::KeyInit;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(value);
    mac.finalize().into_bytes().to_vec()
}

fn tos_signature(secret: &str, date: &str, region: &str, string_to_sign: &str) -> String {
    let date_key = hmac_sha256(secret.as_bytes(), date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, b"tos");
    let request_key = hmac_sha256(&service_key, b"request");
    hex::encode(hmac_sha256(&request_key, string_to_sign.as_bytes()))
}

fn to_header_map(headers: BTreeMap<String, String>) -> Result<HeaderMap, TosErrorSafe> {
    let mut output = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let name =
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| TosErrorSafe::configuration())?;
        let value = HeaderValue::from_str(&value).map_err(|_| TosErrorSafe::configuration())?;
        output.insert(name, value);
    }
    Ok(output)
}

fn valid_exact_presigned_url(value: &str, receipt: &TosObjectReceipt) -> bool {
    if value.len() > 16 * 1024 {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    let query = url.query_pairs().collect::<Vec<_>>();
    let version_ids = query
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("versionId"))
        .map(|(_, value)| value.as_ref())
        .collect::<Vec<_>>();
    let has_query = |expected: &str| {
        query
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case(expected) && !value.is_empty())
    };
    let host = url.host_str().unwrap_or_default();
    let virtual_host_path = host
        .strip_prefix(&format!("{}.", receipt.bucket))
        .is_some_and(is_official_tos_host)
        && url.path() == format!("/{}", receipt.key);
    let path_style = is_official_tos_endpoint_host(host)
        && url.path() == format!("/{}/{}", receipt.bucket, receipt.key);
    url.scheme() == "https"
        && (virtual_host_path || path_style)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && version_ids.len() == 1
        && version_ids[0] == receipt.version_id
        && has_query("X-Tos-Algorithm")
        && has_query("X-Tos-Credential")
        && has_query("X-Tos-Date")
        && has_query("X-Tos-Expires")
        && has_query("X-Tos-SignedHeaders")
        && has_query("X-Tos-Signature")
}

fn valid_opaque_header(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn is_official_tos_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host.ends_with(".volces.com") && host.split('.').any(|label| label.starts_with("tos"))
}

fn is_official_tos_endpoint_host(host: &str) -> bool {
    is_official_tos_host(host)
        && host
            .split('.')
            .next()
            .is_some_and(|label| label.starts_with("tos"))
}

fn valid_bucket(value: &str) -> bool {
    let bytes = value.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn valid_region(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=64).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn classify_http_status(status: reqwest::StatusCode) -> TosErrorSafe {
    #[cfg(test)]
    if std::env::var("ECHOWALL_TOS_DIAGNOSTIC").as_deref() == Ok("status-code-only") {
        eprintln!(
            "echowall_tos_diagnostic_status={} code=redacted",
            status.as_u16()
        );
    }
    match status.as_u16() {
        404 => TosErrorSafe::new(TosErrorKind::NotFound, "TOS object was not found"),
        409 | 412 => TosErrorSafe::conflict(),
        408 | 429 | 500..=599 => TosErrorSafe::network(),
        _ => TosErrorSafe::verification(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> TosObjectReceipt {
        let recording_id = uuid::Uuid::parse_str("5a1d15ec-4cc2-451f-aac8-0c0a7c281d12").unwrap();
        let sha256 = "a".repeat(64);
        TosObjectReceipt {
            bucket: "echowall-private".to_owned(),
            key: format!("echowall/processing/v1/{recording_id}/{sha256}.wav"),
            version_id: "version-1".to_owned(),
            etag: "etag-1".to_owned(),
            sha256,
            size_bytes: 1,
        }
    }

    #[test]
    fn configuration_and_debug_never_expose_credentials_or_bucket() {
        let configuration = TosConfiguration::fabricated();
        let debug = format!("{configuration:?}");
        for sentinel in [
            "AKLTfabricated123",
            "fabricated-secret",
            "echowall-private",
            "cn-hongkong",
            "tos-cn-hongkong",
        ] {
            assert!(!debug.contains(sentinel));
        }
        assert!(configuration.validate().is_ok());
    }

    #[test]
    fn object_identity_is_internal_and_bounded() {
        let recording_id = "5a1d15ec-4cc2-451f-aac8-0c0a7c281d12";
        let hash = "a".repeat(64);
        assert!(validate_object_identity(
            &format!("echowall/processing/v1/{recording_id}/{hash}.wav"),
            &hash,
            1
        )
        .is_ok());
        for extension in ["mp3", "m4a"] {
            assert!(validate_object_identity(
                &format!("echowall/processing/v1/{recording_id}/{hash}.{extension}"),
                &hash,
                1,
            )
            .is_ok());
        }
        for key in [
            "recordings/5a1d15ec-4cc2-451f-aac8-0c0a7c281d12/a.wav",
            "echowall/processing/v1/../escape.wav",
            "echowall/processing/v1/a\\b.wav",
        ] {
            assert!(validate_object_identity(key, &hash, 1).is_err());
        }
        let key = format!("echowall/processing/v1/{recording_id}/{hash}.wav");
        assert!(validate_object_identity(&key, "A", 1).is_err());
        assert!(validate_object_identity(&key, &hash, 0).is_err());
        assert!(validate_recording_identity(
            "00000000-0000-4000-8000-000000000000",
            &key,
            &hash,
            1
        )
        .is_err());
    }

    #[test]
    fn endpoint_normalization_never_permits_non_https_origins() {
        assert_eq!(
            normalize_endpoint("tos-cn-hongkong.volces.com").unwrap(),
            "https://tos-cn-hongkong.volces.com"
        );
        let mut configuration = TosConfiguration::fabricated();
        configuration.endpoint = "http://tos.example.test".to_owned();
        assert!(configuration.validate().is_err());
        configuration.endpoint = "https://secret@tos.example.test/path".to_owned();
        assert!(configuration.validate().is_err());
        configuration.endpoint = "https://tos.attacker.example".to_owned();
        assert!(configuration.validate().is_err());
    }

    #[test]
    fn local_fingerprint_binds_sha_crc_and_size() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), b"fabricated audio bytes").unwrap();
        let fingerprint = hash_and_crc(temp.path()).unwrap();
        assert_eq!(fingerprint.size_bytes, 22);
        assert_eq!(fingerprint.sha256.len(), 64);
        assert_ne!(fingerprint.crc64, 0);
    }

    #[test]
    fn native_contract_is_zero_retry_no_redirect_and_exact_version() {
        let receipt = receipt();
        let configuration = TosConfiguration::fabricated();
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-03T23:22:52Z")
            .unwrap()
            .to_utc();
        let query = BTreeMap::from([("versionId".to_owned(), receipt.version_id.clone())]);
        let head = signed_request(
            &configuration,
            "HEAD",
            &receipt.key,
            &query,
            BTreeMap::from([("if-match".to_owned(), receipt.etag.clone())]),
            now,
        )
        .unwrap();
        assert!(head.url.contains("versionId=version-1"));
        assert_eq!(head.headers.get("if-match").unwrap(), "etag-1");
        let delete = signed_request(
            &configuration,
            "DELETE",
            &receipt.key,
            &query,
            BTreeMap::from([("x-tos-if-match".to_owned(), receipt.etag.clone())]),
            now,
        )
        .unwrap();
        assert!(delete.url.contains("versionId=version-1"));
        assert_eq!(delete.headers.get("x-tos-if-match").unwrap(), "etag-1");
        let recording_id = "5a1d15ec-4cc2-451f-aac8-0c0a7c281d12";
        let put = put_headers(
            &receipt.key,
            recording_id,
            &receipt.sha256,
            receipt.size_bytes,
        )
        .unwrap();
        assert_eq!(put.get("content-length").map(String::as_str), Some("1"));
        assert_eq!(
            put.get("content-type").map(String::as_str),
            Some("audio/wav")
        );
        assert!(!put.contains_key("x-tos-content-sha256"));
        assert_eq!(
            put.get("x-tos-forbid-overwrite").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            put.get("x-tos-meta-echowall-recording-id")
                .map(String::as_str),
            Some(recording_id)
        );
        assert_eq!(
            put.get("x-tos-meta-echowall-sha256").map(String::as_str),
            Some(receipt.sha256.as_str())
        );
        for (extension, expected) in [("mp3", "audio/mpeg"), ("m4a", "audio/mp4")] {
            let key = format!(
                "echowall/processing/v1/{recording_id}/{}.{extension}",
                receipt.sha256
            );
            let input =
                put_headers(&key, recording_id, &receipt.sha256, receipt.size_bytes).unwrap();
            assert_eq!(
                input.get("content-type").map(String::as_str),
                Some(expected)
            );
        }
    }

    #[test]
    fn presign_matches_the_official_sdk_vector() {
        let configuration = TosConfiguration::fabricated();
        let recording_id = "018f92d8-6ad4-7dc1-8e28-8b020d2942cb";
        let sha256 = "a".repeat(64);
        let receipt = TosObjectReceipt {
            bucket: "echowall-private".to_owned(),
            key: format!("echowall/processing/v1/{recording_id}/{sha256}.wav"),
            version_id: "fabricated-version".to_owned(),
            etag: "fabricated-etag".to_owned(),
            sha256,
            size_bytes: 1,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-03T23:22:52Z")
            .unwrap()
            .to_utc();
        let url = presign_get(&configuration, &receipt, 900, now).unwrap();
        assert!(url.contains(
            "X-Tos-Signature=eda5551a48244455561b8849324d9478d9b0f3246648302038438e5a9dd4dd43"
        ));
        assert!(valid_exact_presigned_url(&url, &receipt));
    }

    #[test]
    fn presigned_url_and_receipt_debug_require_and_redact_exact_identity() {
        let receipt = receipt();
        let url = format!(
            "https://{}.tos-cn-hongkong.volces.com/{}?versionId={}&X-Tos-Algorithm=TOS4-HMAC-SHA256&X-Tos-Credential=ak&X-Tos-Date=date&X-Tos-Expires=60&X-Tos-SignedHeaders=host&X-Tos-Signature=secret",
            receipt.bucket, receipt.key, receipt.version_id
        );
        assert!(valid_exact_presigned_url(&url, &receipt));
        assert!(!valid_exact_presigned_url(
            "https://echowall-private.tos-cn-hongkong.volces.com/key?X-Tos-Signature=secret",
            &receipt
        ));
        let debug = format!("{receipt:?}");
        for secret in [
            receipt.bucket.as_str(),
            receipt.key.as_str(),
            receipt.version_id.as_str(),
            receipt.etag.as_str(),
        ] {
            assert!(!debug.contains(secret));
        }
    }
}
