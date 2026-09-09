//! Platform-secure credential storage.
//!
//! Archive sync tokens remain compatible with the existing viewer sync path.
//! Processing credentials use a separate versioned keyring entry and cross IPC
//! only on the write path; status responses contain booleans only.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[cfg(not(feature = "isolated-qa"))]
const SERVICE: &str = "ai.ax.echowall";
#[cfg(feature = "isolated-qa")]
const SERVICE: &str = "ai.ax.echowall.qa.moss";
const USER: &str = "sync-tokens";
#[cfg(not(feature = "isolated-qa"))]
const PROCESSING_SERVICE: &str = "ai.ax.echowall.processing";
#[cfg(feature = "isolated-qa")]
const PROCESSING_SERVICE: &str = "ai.ax.echowall.processing.qa.moss";
const PROCESSING_USER: &str = "credentials-v1";

fn sync_credentials_version() -> u32 {
    1
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct SyncTokens {
    #[serde(default = "sync_credentials_version")]
    pub schema_version: u32,
    pub github_pat: String,
    pub r2_account_id: String,
    pub r2_access_key_id: String,
    pub r2_secret_access_key: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub bucket: String,
}

impl std::fmt::Debug for SyncTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SyncTokens(<redacted>)")
    }
}

/// Write-only IPC request and in-memory processing credential handle.
///
/// It intentionally does not implement `Serialize`, preventing accidental use
/// as a Tauri response. All fields are cleared when the value is dropped.
#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessingCredentials {
    tos_access_key: String,
    tos_secret_key: String,
    tos_bucket: String,
    tos_region: String,
    tos_endpoint: String,
    volc_api_key: String,
    gemini_api_key: String,
    gemini_model: String,
}

impl std::fmt::Debug for ProcessingCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProcessingCredentials(<redacted>)")
    }
}

impl ProcessingCredentials {
    fn validate(&self) -> Result<(), String> {
        validate_secret("TOS access key", &self.tos_access_key, 3, 256)?;
        validate_secret("TOS secret key", &self.tos_secret_key, 8, 4096)?;
        validate_bucket(&self.tos_bucket)?;
        validate_region(&self.tos_region)?;
        validate_endpoint(&self.tos_endpoint)?;
        validate_secret("VOLC API key", &self.volc_api_key, 8, 4096)?;
        validate_secret("Gemini API key", &self.gemini_api_key, 8, 4096)?;
        validate_model(&self.gemini_model)
    }

    fn wire(&self) -> ProcessingCredentialsRef<'_> {
        ProcessingCredentialsRef {
            tos_access_key: &self.tos_access_key,
            tos_secret_key: &self.tos_secret_key,
            tos_bucket: &self.tos_bucket,
            tos_region: &self.tos_region,
            tos_endpoint: &self.tos_endpoint,
            volc_api_key: &self.volc_api_key,
            gemini_api_key: &self.gemini_api_key,
            gemini_model: &self.gemini_model,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn tos_access_key(&self) -> &str {
        &self.tos_access_key
    }

    #[allow(dead_code)]
    pub(crate) fn tos_secret_key(&self) -> &str {
        &self.tos_secret_key
    }

    #[allow(dead_code)]
    pub(crate) fn tos_bucket(&self) -> &str {
        &self.tos_bucket
    }

    #[allow(dead_code)]
    pub(crate) fn tos_region(&self) -> &str {
        &self.tos_region
    }

    #[allow(dead_code)]
    pub(crate) fn tos_endpoint(&self) -> &str {
        &self.tos_endpoint
    }

    #[allow(dead_code)]
    pub(crate) fn volc_api_key(&self) -> &str {
        &self.volc_api_key
    }

    #[allow(dead_code)]
    pub(crate) fn gemini_api_key(&self) -> &str {
        &self.gemini_api_key
    }

    #[allow(dead_code)]
    pub(crate) fn gemini_model(&self) -> &str {
        &self.gemini_model
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessingCredentialsRef<'a> {
    tos_access_key: &'a str,
    tos_secret_key: &'a str,
    tos_bucket: &'a str,
    tos_region: &'a str,
    tos_endpoint: &'a str,
    volc_api_key: &'a str,
    gemini_api_key: &'a str,
    gemini_model: &'a str,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingCredentialsStatus {
    pub configured: bool,
    pub archive_configured: bool,
    pub has_tos_access_key: bool,
    pub has_tos_secret_key: bool,
    pub has_tos_bucket: bool,
    pub has_tos_region: bool,
    pub has_tos_endpoint: bool,
    pub has_volc_api_key: bool,
    pub has_gemini_api_key: bool,
    pub has_gemini_model: bool,
}

impl ProcessingCredentialsStatus {
    fn configured() -> Self {
        Self {
            configured: true,
            archive_configured: false,
            has_tos_access_key: true,
            has_tos_secret_key: true,
            has_tos_bucket: true,
            has_tos_region: true,
            has_tos_endpoint: true,
            has_volc_api_key: true,
            has_gemini_api_key: true,
            has_gemini_model: true,
        }
    }
}

/// Install the platform credential store once. Errors are returned as strings
/// so callers can surface "secure store unavailable" instead of panicking.
fn ensure_store() -> Result<(), String> {
    if keyring_core::get_default_store().is_some() {
        return Ok(());
    }
    #[cfg(target_os = "ios")]
    {
        use apple_native_keyring_store::protected;
        keyring_core::set_default_store(protected::Store::new().map_err(|e| e.to_string())?);
    }
    #[cfg(target_os = "macos")]
    {
        use apple_native_keyring_store::keychain;
        keyring_core::set_default_store(keychain::Store::new().map_err(|e| e.to_string())?);
    }
    #[cfg(target_os = "android")]
    {
        use android_native_keyring_store::Store;
        let store = std::panic::catch_unwind(Store::new)
            .map_err(|_| "Android Keystore 不可用 (ndk context 未初始化)".to_string())?
            .map_err(|e| e.to_string())?;
        keyring_core::set_default_store(store);
    }
    #[cfg(target_os = "windows")]
    {
        use windows_native_keyring_store::Store;
        keyring_core::set_default_store(Store::new().map_err(|e| e.to_string())?);
    }
    #[cfg(not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "android",
        target_os = "windows"
    )))]
    {
        return Err("no secure store on this platform".into());
    }
    #[allow(unreachable_code)]
    Ok(())
}

fn entry_for(service: &str, user: &str) -> Result<keyring_core::Entry, String> {
    ensure_store()?;
    #[cfg(target_os = "ios")]
    {
        let mods = std::collections::HashMap::from([("access-policy", "after-first-unlock")]);
        return keyring_core::Entry::new_with_modifiers(service, user, &mods)
            .map_err(|e| e.to_string());
    }
    #[allow(unreachable_code)]
    keyring_core::Entry::new(service, user).map_err(|e| e.to_string())
}

fn entry() -> Result<keyring_core::Entry, String> {
    entry_for(SERVICE, USER)
}

pub fn save(tokens: &SyncTokens) -> Result<(), String> {
    let blob = Zeroizing::new(serde_json::to_string(tokens).map_err(|e| e.to_string())?);
    entry()?
        .set_password(blob.as_str())
        .map_err(|e| e.to_string())
}

pub(crate) fn validate_sync_tokens(tokens: &SyncTokens) -> Result<(), String> {
    if tokens.schema_version != sync_credentials_version() {
        return Err("archive credential version is unsupported".to_owned());
    }
    validate_sync_secret_material(tokens)?;
    validate_archive_repo(&tokens.repo)?;
    validate_archive_bucket(&tokens.bucket)
}

pub(crate) fn validate_sync_secret_material(tokens: &SyncTokens) -> Result<(), String> {
    validate_secret("GitHub token", &tokens.github_pat, 8, 4096)?;
    if tokens.r2_account_id.len() != 32
        || !tokens
            .r2_account_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("R2 account ID has an invalid format".to_owned());
    }
    if !(16..=128).contains(&tokens.r2_access_key_id.len())
        || !tokens
            .r2_access_key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("R2 access key has an invalid format".to_owned());
    }
    validate_secret("R2 secret key", &tokens.r2_secret_access_key, 16, 4096)
}

pub(crate) fn validate_archive_repo(value: &str) -> Result<(), String> {
    let mut parts = value.split('/');
    let valid_component = |component: &str| {
        !component.is_empty()
            && component.len() <= 100
            && component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if parts.next().is_none() && valid_component(owner) && valid_component(repo) {
        Ok(())
    } else {
        Err("GitHub archive repo has an invalid format".to_owned())
    }
}

pub(crate) fn validate_archive_bucket(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if (3..=63).contains(&bytes.len())
        && edge(bytes[0])
        && edge(bytes[bytes.len() - 1])
        && bytes.iter().all(|byte| edge(*byte) || *byte == b'-')
    {
        Ok(())
    } else {
        Err("R2 archive bucket has an invalid format".to_owned())
    }
}

pub fn load() -> Option<SyncTokens> {
    let blob = Zeroizing::new(entry().ok()?.get_password().ok()?);
    serde_json::from_str(blob.as_str()).ok()
}

pub fn delete_archive() -> Result<(), String> {
    let archive_entry = entry()?;
    match archive_entry.delete_credential() {
        Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(_) => Err("could not delete archive credentials".to_owned()),
    }
}

trait ProcessingCredentialStore: Send + Sync {
    fn set(&self, blob: &str) -> Result<(), String>;
    fn get(&self) -> Result<Option<Zeroizing<String>>, String>;
    fn delete(&self) -> Result<(), String>;
}

struct PlatformProcessingCredentialStore;

impl ProcessingCredentialStore for PlatformProcessingCredentialStore {
    fn set(&self, blob: &str) -> Result<(), String> {
        entry_for(PROCESSING_SERVICE, PROCESSING_USER)?
            .set_password(blob)
            .map_err(|_| "could not save processing credentials".to_owned())
    }

    fn get(&self) -> Result<Option<Zeroizing<String>>, String> {
        let entry = entry_for(PROCESSING_SERVICE, PROCESSING_USER)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(_) => Err("could not read processing credential status".to_owned()),
        }
    }

    fn delete(&self) -> Result<(), String> {
        let entry = entry_for(PROCESSING_SERVICE, PROCESSING_USER)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(_) => Err("could not delete processing credentials".to_owned()),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct EphemeralProcessingCredentialStore {
    value: Mutex<Option<Zeroizing<String>>>,
}

#[cfg(test)]
impl ProcessingCredentialStore for EphemeralProcessingCredentialStore {
    fn set(&self, blob: &str) -> Result<(), String> {
        *self.value.lock().map_err(|_| "test store unavailable")? =
            Some(Zeroizing::new(blob.to_owned()));
        Ok(())
    }

    fn get(&self) -> Result<Option<Zeroizing<String>>, String> {
        Ok(self
            .value
            .lock()
            .map_err(|_| "test store unavailable")?
            .as_ref()
            .map(|value| Zeroizing::new(value.to_string())))
    }

    fn delete(&self) -> Result<(), String> {
        self.value
            .lock()
            .map_err(|_| "test store unavailable")?
            .take();
        Ok(())
    }
}

#[derive(Clone)]
pub struct ProcessingCredentialsState {
    store: Arc<dyn ProcessingCredentialStore>,
    operation_lock: Arc<Mutex<()>>,
}

impl ProcessingCredentialsState {
    pub fn new() -> Self {
        Self {
            store: Arc::new(PlatformProcessingCredentialStore),
            operation_lock: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(crate) fn ephemeral() -> Self {
        Self {
            store: Arc::new(EphemeralProcessingCredentialStore::default()),
            operation_lock: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    fn with_store(store: Arc<dyn ProcessingCredentialStore>) -> Self {
        Self {
            store,
            operation_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn save(&self, credentials: &ProcessingCredentials) -> Result<(), String> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| "processing credential state is unavailable".to_owned())?;
        credentials.validate()?;
        let blob = Zeroizing::new(
            serde_json::to_string(&credentials.wire())
                .map_err(|_| "could not encode processing credentials".to_owned())?,
        );
        self.store.set(blob.as_str())
    }

    fn status(&self) -> Result<ProcessingCredentialsStatus, String> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| "processing credential state is unavailable".to_owned())?;
        let Some(credentials) = self.load_unlocked()? else {
            return Ok(ProcessingCredentialsStatus::default());
        };
        credentials
            .validate()
            .map_err(|_| "stored processing credentials are invalid".to_owned())?;
        Ok(ProcessingCredentialsStatus::configured())
    }

    #[allow(dead_code)]
    pub(crate) fn load(&self) -> Result<Option<ProcessingCredentials>, String> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| "processing credential state is unavailable".to_owned())?;
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> Result<Option<ProcessingCredentials>, String> {
        self.store
            .get()?
            .map(|blob| {
                serde_json::from_str(blob.as_str())
                    .map_err(|_| "stored processing credentials are invalid".to_owned())
            })
            .transpose()
    }

    pub(crate) fn delete(&self) -> Result<(), String> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| "processing credential state is unavailable".to_owned())?;
        self.store.delete()
    }
}

impl Default for ProcessingCredentialsState {
    fn default() -> Self {
        Self::new()
    }
}

#[tauri::command]
pub fn save_processing_credentials(
    state: tauri::State<'_, ProcessingCredentialsState>,
    credentials: ProcessingCredentials,
) -> Result<(), String> {
    state.save(&credentials)
}

#[tauri::command]
pub fn processing_credentials_status(
    state: tauri::State<'_, ProcessingCredentialsState>,
    sync_state: tauri::State<'_, crate::sync::SyncRuntimeState>,
) -> Result<ProcessingCredentialsStatus, String> {
    let mut status = state.status()?;
    status.archive_configured =
        sync_state.0.gh.read().unwrap().is_some() && sync_state.0.r2.read().unwrap().is_some();
    status.configured = status.configured && status.archive_configured;
    Ok(status)
}

#[tauri::command]
pub fn delete_processing_credentials(
    state: tauri::State<'_, ProcessingCredentialsState>,
) -> Result<(), String> {
    state.delete()
}

fn validate_secret(label: &str, value: &str, minimum: usize, maximum: usize) -> Result<(), String> {
    let length = value.len();
    if value == value.trim()
        && (minimum..=maximum).contains(&length)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
    {
        Ok(())
    } else {
        Err(format!("{label} has an invalid format"))
    }
}

fn validate_bucket(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let valid_edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if (3..=63).contains(&bytes.len())
        && valid_edge(bytes[0])
        && valid_edge(bytes[bytes.len() - 1])
        && bytes.iter().all(|byte| valid_edge(*byte) || *byte == b'-')
    {
        Ok(())
    } else {
        Err("TOS bucket has an invalid format".to_owned())
    }
}

fn validate_region(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if (2..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        Ok(())
    } else {
        Err("TOS region has an invalid format".to_owned())
    }
}

fn validate_endpoint(value: &str) -> Result<(), String> {
    if value.contains("://") {
        let url = reqwest::Url::parse(value)
            .map_err(|_| "TOS endpoint has an invalid format".to_owned())?;
        if url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.path(), "" | "/")
        {
            return Ok(());
        }
        return Err("TOS endpoint must be a credential-free HTTPS origin".to_owned());
    }
    if (4..=253).contains(&value.len())
        && value.contains('.')
        && !value.starts_with(['.', '-'])
        && !value.ends_with(['.', '-'])
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        && value.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.len() <= 63
        })
    {
        Ok(())
    } else {
        Err("TOS endpoint has an invalid format".to_owned())
    }
}

fn validate_model(value: &str) -> Result<(), String> {
    if (1..=128).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
    {
        Ok(())
    } else {
        Err("Gemini model has an invalid format".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryStore {
        value: Mutex<Option<Zeroizing<String>>>,
    }

    impl ProcessingCredentialStore for MemoryStore {
        fn set(&self, blob: &str) -> Result<(), String> {
            *self.value.lock().unwrap() = Some(Zeroizing::new(blob.to_owned()));
            Ok(())
        }

        fn get(&self) -> Result<Option<Zeroizing<String>>, String> {
            Ok(self
                .value
                .lock()
                .unwrap()
                .as_ref()
                .map(|value| Zeroizing::new(value.to_string())))
        }

        fn delete(&self) -> Result<(), String> {
            self.value.lock().unwrap().take();
            Ok(())
        }
    }

    fn credentials() -> ProcessingCredentials {
        ProcessingCredentials {
            tos_access_key: "AKLTfabricated123".to_owned(),
            tos_secret_key: "fabricated-tos-secret-123456".to_owned(),
            tos_bucket: "echowall-private".to_owned(),
            tos_region: "cn-hongkong".to_owned(),
            tos_endpoint: "tos-cn-hongkong.volces.com".to_owned(),
            volc_api_key: "fabricated-volc-key-123456".to_owned(),
            gemini_api_key: "fabricated-gemini-key-123456".to_owned(),
            gemini_model: "gemini-3.5-flash".to_owned(),
        }
    }

    #[test]
    fn processing_credentials_are_debug_redacted_and_zeroizing() {
        let mut credentials = credentials();
        let debug = format!("{credentials:?}");
        assert_eq!(debug, "ProcessingCredentials(<redacted>)");
        assert!(!debug.contains(credentials.gemini_api_key()));
        assert!(std::mem::needs_drop::<ProcessingCredentials>());
        assert!(std::mem::needs_drop::<SyncTokens>());
        credentials.zeroize();
        assert!(credentials.tos_access_key().is_empty());
        assert!(credentials.tos_secret_key().is_empty());
        assert!(credentials.volc_api_key().is_empty());
        assert!(credentials.gemini_api_key().is_empty());
    }

    #[test]
    fn credential_namespaces_match_build_flavor_without_migrating_production_entries() {
        if cfg!(feature = "isolated-qa") {
            assert_eq!(SERVICE, "ai.ax.echowall.qa.moss");
            assert_eq!(PROCESSING_SERVICE, "ai.ax.echowall.processing.qa.moss");
        } else {
            assert_eq!(SERVICE, "ai.ax.echowall");
            assert_eq!(PROCESSING_SERVICE, "ai.ax.echowall.processing");
        }
        assert_eq!(USER, "sync-tokens");
        assert_eq!(PROCESSING_USER, "credentials-v1");
    }

    #[test]
    fn save_status_and_delete_never_return_credential_values() {
        let store = Arc::new(MemoryStore::default());
        let state = ProcessingCredentialsState::with_store(store);
        let credentials = credentials();
        state.save(&credentials).unwrap();
        let status = state.status().unwrap();
        assert_eq!(status, ProcessingCredentialsStatus::configured());
        let loaded = state.load().unwrap().unwrap();
        assert_eq!(loaded.tos_bucket(), credentials.tos_bucket());
        assert_eq!(loaded.tos_region(), credentials.tos_region());
        assert_eq!(loaded.tos_endpoint(), credentials.tos_endpoint());
        assert_eq!(loaded.gemini_model(), credentials.gemini_model());
        let json = serde_json::to_value(status).unwrap();
        assert!(json
            .as_object()
            .unwrap()
            .values()
            .all(serde_json::Value::is_boolean));
        let encoded = serde_json::to_string(&json).unwrap();
        for secret in [
            credentials.tos_access_key(),
            credentials.tos_secret_key(),
            credentials.volc_api_key(),
            credentials.gemini_api_key(),
        ] {
            assert!(!encoded.contains(secret));
        }
        state.delete().unwrap();
        assert_eq!(
            state.status().unwrap(),
            ProcessingCredentialsStatus::default()
        );
    }

    #[test]
    fn validation_rejects_whitespace_unsafe_endpoint_and_invalid_names() {
        let store = Arc::new(MemoryStore::default());
        let state = ProcessingCredentialsState::with_store(store);

        let mut invalid = credentials();
        invalid.gemini_api_key.push(' ');
        assert!(state.save(&invalid).is_err());

        let mut invalid = credentials();
        invalid.tos_endpoint = "https://secret@example.com/path?token=x".to_owned();
        assert!(state.save(&invalid).is_err());

        let mut invalid = credentials();
        invalid.tos_bucket = "Bad_Bucket".to_owned();
        assert!(state.save(&invalid).is_err());

        let mut invalid = credentials();
        invalid.gemini_model = "gemini model".to_owned();
        assert!(state.save(&invalid).is_err());
    }

    #[test]
    fn unknown_ipc_fields_are_rejected() {
        let mut value = serde_json::to_value(credentials().wire()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_owned(), serde_json::json!("secret"));
        assert!(serde_json::from_value::<ProcessingCredentials>(value).is_err());
    }

    #[test]
    fn corrupt_store_error_does_not_echo_stored_secret_material() {
        let store = Arc::new(MemoryStore::default());
        store.set(r#"{"geminiApiKey":"must-not-escape"}"#).unwrap();
        let state = ProcessingCredentialsState::with_store(store);
        let error = state.status().unwrap_err();
        assert_eq!(error, "stored processing credentials are invalid");
        assert!(!error.contains("must-not-escape"));
        state.delete().unwrap();
        state.delete().unwrap();
    }

    #[test]
    fn archive_credentials_reject_r2_host_breakout_values() {
        let mut tokens = SyncTokens {
            schema_version: 1,
            github_pat: "github_pat_fabricated".to_owned(),
            r2_account_id: "0123456789abcdef0123456789abcdef".to_owned(),
            r2_access_key_id: "FABRICATEDACCESSKEY123456".to_owned(),
            r2_secret_access_key: "fabricated-r2-secret-123456".to_owned(),
            repo: "owner/private-notes".to_owned(),
            bucket: "private-audio".to_owned(),
        };
        assert!(validate_sync_tokens(&tokens).is_ok());
        tokens.r2_account_id = "attacker.example/path".to_owned();
        assert!(validate_sync_tokens(&tokens).is_err());
        tokens.r2_account_id = "0123456789abcdef0123456789abcdef".to_owned(); // gitleaks:allow -- fabricated test value
        tokens.r2_access_key_id = "bad/key".to_owned();
        assert!(validate_sync_tokens(&tokens).is_err());
    }

    #[test]
    fn legacy_archive_record_decodes_only_as_explicitly_unbound() {
        let tokens: SyncTokens = serde_json::from_value(serde_json::json!({
            "github_pat": "github_pat_fabricated",
            "r2_account_id": "0123456789abcdef0123456789abcdef",
            "r2_access_key_id": "FABRICATEDACCESSKEY123456",
            "r2_secret_access_key": "fabricated-r2-secret-123456"
        }))
        .unwrap();
        assert_eq!(tokens.schema_version, 1);
        assert!(tokens.repo.is_empty());
        assert!(tokens.bucket.is_empty());
        assert!(validate_sync_secret_material(&tokens).is_ok());
        assert!(validate_sync_tokens(&tokens).is_err());
    }
}
