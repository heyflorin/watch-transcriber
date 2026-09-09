use std::collections::HashSet;
use std::fmt;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use super::state::JobState;

pub const RECORDING_ENVELOPE_VERSION: u32 = 1;
pub const MAX_ENVELOPE_TRACKS: usize = 32;
pub const MAX_CAPTURE_WARNINGS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedRecordingEnvelope")]
pub struct RecordingEnvelope {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub source: RecordingSource,
    pub captured_at: DateTime<FixedOffset>,
    pub ended_at: DateTime<FixedOffset>,
    pub duration_ms: u64,
    pub tracks: Vec<AudioTrack>,
    pub normalized_audio: Option<String>,
    pub normalized_sha256: Option<String>,
    pub imported_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_review: Option<ImportReview>,
    pub capture_warnings: Vec<CaptureWarning>,
    pub job: JobStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedRecordingEnvelope {
    schema_version: u32,
    recording_id: Uuid,
    source: RecordingSource,
    captured_at: DateTime<FixedOffset>,
    ended_at: DateTime<FixedOffset>,
    duration_ms: u64,
    tracks: Vec<AudioTrack>,
    #[serde(deserialize_with = "required_option")]
    normalized_audio: Option<String>,
    #[serde(deserialize_with = "required_option")]
    normalized_sha256: Option<String>,
    #[serde(deserialize_with = "required_option")]
    imported_name: Option<String>,
    #[serde(default)]
    import_review: Option<ImportReview>,
    capture_warnings: Vec<CaptureWarning>,
    job: JobStatus,
}

impl TryFrom<UncheckedRecordingEnvelope> for RecordingEnvelope {
    type Error = EnvelopeValidationError;

    fn try_from(value: UncheckedRecordingEnvelope) -> Result<Self, Self::Error> {
        let envelope = Self {
            schema_version: value.schema_version,
            recording_id: value.recording_id,
            source: value.source,
            captured_at: value.captured_at,
            ended_at: value.ended_at,
            duration_ms: value.duration_ms,
            tracks: value.tracks,
            normalized_audio: value.normalized_audio,
            normalized_sha256: value.normalized_sha256,
            imported_name: value.imported_name,
            import_review: value.import_review,
            capture_warnings: value.capture_warnings,
            job: value.job,
        };
        envelope.validate()?;
        Ok(envelope)
    }
}

impl RecordingEnvelope {
    /// Revalidate constructed or mutated values before any persistence or I/O.
    pub fn validate(&self) -> Result<(), EnvelopeValidationError> {
        if self.schema_version != RECORDING_ENVELOPE_VERSION {
            return Err(invalid("schema_version must be 1"));
        }
        if self.ended_at < self.captured_at {
            return Err(invalid("ended_at must not precede captured_at"));
        }
        self.source.validate()?;
        if self.tracks.is_empty() || self.tracks.len() > MAX_ENVELOPE_TRACKS {
            return Err(invalid("tracks must contain between 1 and 32 items"));
        }

        let mut paths = HashSet::with_capacity(self.tracks.len());
        for (index, track) in self.tracks.iter().enumerate() {
            track.validate(index)?;
            if !paths.insert(track.relative_path.as_str()) {
                return Err(invalid("tracks contain a duplicate relative path"));
            }
        }

        match (&self.normalized_audio, &self.normalized_sha256) {
            (None, None) => {}
            (Some(path), Some(digest)) => {
                validate_relative_path(path, "normalized_audio")?;
                validate_sha256(digest, "normalized_sha256")?;
            }
            _ => {
                return Err(invalid(
                    "normalized_audio and normalized_sha256 must both be set or null",
                ));
            }
        }

        validate_optional_string(&self.imported_name, "imported_name", 1, 1024)?;
        if self.source.kind == SourceKind::FileImport && self.imported_name.is_none() {
            return Err(invalid("file import requires imported_name"));
        }
        if let Some(review) = &self.import_review {
            if self.source.kind != SourceKind::FileImport {
                return Err(invalid("import_review is only valid for file imports"));
            }
            review.validate()?;
        }
        if self.capture_warnings.len() > MAX_CAPTURE_WARNINGS {
            return Err(invalid("capture_warnings must contain at most 128 items"));
        }
        for (index, warning) in self.capture_warnings.iter().enumerate() {
            warning.validate(index, self.duration_ms)?;
        }
        self.job.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportReview {
    pub timestamp_source: ImportTimestampSource,
    pub timestamp_confidence: ImportTimestampConfidence,
    #[serde(deserialize_with = "required_option")]
    pub display_title: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub speaker_count: Option<u32>,
    #[serde(deserialize_with = "required_option")]
    pub confirmed_at: Option<DateTime<FixedOffset>>,
}

impl ImportReview {
    fn validate(&self) -> Result<(), EnvelopeValidationError> {
        validate_optional_string(&self.display_title, "import_review.display_title", 1, 200)?;
        if matches!(self.speaker_count, Some(0 | 51..)) {
            return Err(invalid("import_review.speaker_count is out of range"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingSource {
    pub kind: SourceKind,
    pub platform: Platform,
    #[serde(deserialize_with = "required_option")]
    pub label: Option<String>,
    pub capture_scope: CaptureScope,
}

impl RecordingSource {
    fn validate(&self) -> Result<(), EnvelopeValidationError> {
        validate_optional_string(&self.label, "source.label", 1, 512)?;
        let compatible = match self.kind {
            SourceKind::VoiceMemosWatch => {
                matches!(self.platform, Platform::Macos | Platform::Ios)
                    && self.capture_scope == CaptureScope::ImportedFile
            }
            SourceKind::VoiceMemosShare => {
                self.platform == Platform::Ios && self.capture_scope == CaptureScope::ImportedFile
            }
            SourceKind::DesktopVoiceMemo => {
                matches!(self.platform, Platform::Macos | Platform::Windows)
                    && self.capture_scope == CaptureScope::Microphone
            }
            SourceKind::DesktopMeeting => {
                matches!(self.platform, Platform::Macos | Platform::Windows)
                    && matches!(
                        self.capture_scope,
                        CaptureScope::Application
                            | CaptureScope::ProcessTree
                            | CaptureScope::BrowserTab
                            | CaptureScope::BrowserApplication
                    )
            }
            SourceKind::DesktopSystem => {
                matches!(self.platform, Platform::Macos | Platform::Windows)
                    && self.capture_scope == CaptureScope::System
            }
            SourceKind::MobileVoiceMemo => {
                matches!(self.platform, Platform::Ios | Platform::Android)
                    && self.capture_scope == CaptureScope::Microphone
            }
            SourceKind::FileImport => self.capture_scope == CaptureScope::ImportedFile,
        };
        if compatible {
            Ok(())
        } else {
            Err(invalid("source kind/platform/scope is incompatible"))
        }
    }
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $value)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }
    };
}

string_enum!(SourceKind {
    VoiceMemosWatch => "voice_memos_watch",
    VoiceMemosShare => "voice_memos_share",
    DesktopVoiceMemo => "desktop_voice_memo",
    DesktopMeeting => "desktop_meeting",
    DesktopSystem => "desktop_system",
    MobileVoiceMemo => "mobile_voice_memo",
    FileImport => "file_import",
});

string_enum!(Platform {
    Macos => "macos",
    Windows => "windows",
    Ios => "ios",
    Android => "android",
});

string_enum!(CaptureScope {
    Microphone => "microphone",
    Application => "application",
    ProcessTree => "process_tree",
    BrowserTab => "browser_tab",
    BrowserApplication => "browser_application",
    System => "system",
    ImportedFile => "imported_file",
});

string_enum!(TrackRole {
    Microphone => "microphone",
    System => "system",
    Mixed => "mixed",
    Imported => "imported",
});

string_enum!(ImportTimestampSource {
    EmbeddedMetadata => "embedded_metadata",
    Filename => "filename",
    FileMtime => "file_mtime",
    User => "user",
});

string_enum!(ImportTimestampConfidence {
    High => "high",
    Medium => "medium",
    Low => "low",
    UserConfirmed => "user_confirmed",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioTrack {
    pub role: TrackRole,
    pub relative_path: String,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_ms: u64,
    pub clock_start_ns: u64,
    pub sha256: String,
}

impl AudioTrack {
    fn validate(&self, index: usize) -> Result<(), EnvelopeValidationError> {
        let prefix = format!("tracks[{index}]");
        validate_relative_path(&self.relative_path, &format!("{prefix}.relative_path"))?;
        validate_string(&self.codec, &format!("{prefix}.codec"), 1, 64)?;
        if !(1..=768_000).contains(&self.sample_rate) {
            return Err(invalid(format!("{prefix}.sample_rate is out of range")));
        }
        if !(1..=64).contains(&self.channels) {
            return Err(invalid(format!("{prefix}.channels is out of range")));
        }
        validate_sha256(&self.sha256, &format!("{prefix}.sha256"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureWarning {
    pub code: String,
    pub message: String,
    pub at_ms: u64,
}

impl CaptureWarning {
    fn validate(&self, index: usize, duration_ms: u64) -> Result<(), EnvelopeValidationError> {
        let prefix = format!("capture_warnings[{index}]");
        if !valid_code(&self.code) {
            return Err(invalid(format!("{prefix}.code is invalid")));
        }
        validate_string(&self.message, &format!("{prefix}.message"), 1, 2048)?;
        if self.at_ms > duration_ms {
            return Err(invalid(format!("{prefix}.at_ms is out of range")));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobStatus {
    pub state: JobState,
    pub attempt: u64,
    #[serde(deserialize_with = "required_option")]
    pub remote_job_id: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub last_error: Option<String>,
}

impl JobStatus {
    fn validate(&self) -> Result<(), EnvelopeValidationError> {
        validate_optional_string(&self.remote_job_id, "job.remote_job_id", 1, 256)?;
        validate_optional_string(&self.last_error, "job.last_error", 1, 4096)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeValidationError {
    message: String,
}

impl EnvelopeValidationError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for EnvelopeValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EnvelopeValidationError {}

fn invalid(message: impl Into<String>) -> EnvelopeValidationError {
    EnvelopeValidationError {
        message: message.into(),
    }
}

pub fn validate_relative_path(value: &str, field: &str) -> Result<(), EnvelopeValidationError> {
    let invalid_path = || invalid(format!("{field} must be a safe relative path"));
    if value.is_empty() || value.chars().count() > 1024 || value.contains('\\') {
        return Err(invalid_path());
    }
    let bytes = value.as_bytes();
    if value.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid_path());
    }
    Ok(())
}

pub fn validate_sha256(value: &str, field: &str) -> Result<(), EnvelopeValidationError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(invalid(format!("{field} must be a lowercase SHA-256")))
    }
}

fn validate_string(
    value: &str,
    field: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), EnvelopeValidationError> {
    let length = value.chars().count();
    if (minimum..=maximum).contains(&length) {
        Ok(())
    } else {
        Err(invalid(format!("{field} must be a valid string")))
    }
}

fn validate_optional_string(
    value: &Option<String>,
    field: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), EnvelopeValidationError> {
    match value {
        Some(value) => validate_string(value, field, minimum, maximum),
        None => Ok(()),
    }
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

pub(crate) fn valid_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{json, Value};

    use super::*;

    const SCHEMA: &str = include_str!("../../../../schemas/recording-envelope-v1.json");
    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn valid_value() -> Value {
        json!({
            "schema_version": 1,
            "recording_id": "018f92d8-6ad4-7dc1-8e28-8b020d2942cb",
            "source": {
                "kind": "desktop_meeting",
                "platform": "windows",
                "label": "Microsoft Teams",
                "capture_scope": "process_tree"
            },
            "captured_at": "2026-09-02T09:00:00-07:00",
            "ended_at": "2026-09-02T10:00:00-07:00",
            "duration_ms": 3_600_000,
            "tracks": [{
                "role": "microphone",
                "relative_path": "tracks/mic-0001.m4a",
                "codec": "aac",
                "sample_rate": 48_000,
                "channels": 1,
                "duration_ms": 3_600_000,
                "clock_start_ns": 42,
                "sha256": SHA_A
            }],
            "normalized_audio": "derived/mixed.m4a",
            "normalized_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "imported_name": null,
            "capture_warnings": [],
            "job": {
                "state": "ready",
                "attempt": 0,
                "remote_job_id": null,
                "last_error": null
            }
        })
    }

    #[test]
    fn accepts_and_round_trips_a_valid_envelope() {
        let value = valid_value();
        let envelope: RecordingEnvelope = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(envelope).unwrap(), value);
    }

    #[test]
    fn rejects_unknown_fields_and_semantic_errors_during_deserialization() {
        let mut unknown = valid_value();
        unknown["secret"] = json!("not allowed");
        assert!(serde_json::from_value::<RecordingEnvelope>(unknown).is_err());

        let mut naive_time = valid_value();
        naive_time["captured_at"] = json!("2026-09-02T09:00:00");
        assert!(serde_json::from_value::<RecordingEnvelope>(naive_time).is_err());

        let mut bad_source = valid_value();
        bad_source["source"]["platform"] = json!("ios");
        assert!(serde_json::from_value::<RecordingEnvelope>(bad_source).is_err());

        let mut mismatched_normalized = valid_value();
        mismatched_normalized["normalized_sha256"] = Value::Null;
        assert!(serde_json::from_value::<RecordingEnvelope>(mismatched_normalized).is_err());

        for required_nullable in ["normalized_audio", "normalized_sha256", "imported_name"] {
            let mut missing = valid_value();
            missing.as_object_mut().unwrap().remove(required_nullable);
            assert!(
                serde_json::from_value::<RecordingEnvelope>(missing).is_err(),
                "accepted missing {required_nullable}"
            );
        }

        let mut missing_label = valid_value();
        missing_label["source"]
            .as_object_mut()
            .unwrap()
            .remove("label");
        assert!(serde_json::from_value::<RecordingEnvelope>(missing_label).is_err());

        let mut missing_remote_id = valid_value();
        missing_remote_id["job"]
            .as_object_mut()
            .unwrap()
            .remove("remote_job_id");
        assert!(serde_json::from_value::<RecordingEnvelope>(missing_remote_id).is_err());
    }

    #[test]
    fn rejects_path_traversal_and_noncanonical_paths() {
        for path in [
            "/tmp/mic.m4a",
            "../mic.m4a",
            "tracks/../../mic.m4a",
            "tracks\\mic.m4a",
            "C:/mic.m4a",
            "tracks//mic.m4a",
            "tracks/./mic.m4a",
        ] {
            let mut value = valid_value();
            value["tracks"][0]["relative_path"] = json!(path);
            assert!(
                serde_json::from_value::<RecordingEnvelope>(value).is_err(),
                "accepted {path}"
            );
        }

        let mut normalized = valid_value();
        normalized["normalized_audio"] = json!("derived/../outside.m4a");
        assert!(serde_json::from_value::<RecordingEnvelope>(normalized).is_err());
    }

    #[test]
    fn rejects_bad_identity_hash_warning_and_timeline_invariants() {
        let mut bad_uuid = valid_value();
        bad_uuid["recording_id"] = json!("not-a-uuid");
        assert!(serde_json::from_value::<RecordingEnvelope>(bad_uuid).is_err());

        let mut reversed = valid_value();
        reversed["ended_at"] = json!("2026-09-02T08:59:59-07:00");
        assert!(serde_json::from_value::<RecordingEnvelope>(reversed).is_err());

        let mut uppercase_hash = valid_value();
        uppercase_hash["tracks"][0]["sha256"] = json!(SHA_A.to_uppercase());
        assert!(serde_json::from_value::<RecordingEnvelope>(uppercase_hash).is_err());

        let mut bad_warning = valid_value();
        bad_warning["capture_warnings"] = json!([{
            "code": "Route-Changed",
            "message": "Output changed",
            "at_ms": 1
        }]);
        assert!(serde_json::from_value::<RecordingEnvelope>(bad_warning).is_err());

        let mut late_warning = valid_value();
        late_warning["capture_warnings"] = json!([{
            "code": "route_changed",
            "message": "Output changed",
            "at_ms": 3_600_001
        }]);
        assert!(serde_json::from_value::<RecordingEnvelope>(late_warning).is_err());

        let mut duplicate_path = valid_value();
        let duplicate = duplicate_path["tracks"][0].clone();
        duplicate_path["tracks"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(serde_json::from_value::<RecordingEnvelope>(duplicate_path).is_err());
    }

    #[test]
    fn file_import_requires_an_original_name() {
        let mut value = valid_value();
        value["source"] = json!({
            "kind": "file_import",
            "platform": "android",
            "label": null,
            "capture_scope": "imported_file"
        });
        value["tracks"][0]["role"] = json!("imported");
        assert!(serde_json::from_value::<RecordingEnvelope>(value.clone()).is_err());

        value["imported_name"] = json!("meeting.m4a");
        assert!(serde_json::from_value::<RecordingEnvelope>(value).is_ok());
    }

    #[test]
    fn collection_limits_match_and_enforce_the_public_schema() {
        let schema: Value = serde_json::from_str(SCHEMA).unwrap();
        let maximum_tracks = schema
            .pointer("/properties/tracks/maxItems")
            .and_then(Value::as_u64)
            .unwrap() as usize;
        let maximum_warnings = schema
            .pointer("/properties/capture_warnings/maxItems")
            .and_then(Value::as_u64)
            .unwrap() as usize;
        assert_eq!(maximum_tracks, 32);
        assert_eq!(maximum_warnings, 128);
        assert_eq!(maximum_tracks, MAX_ENVELOPE_TRACKS);
        assert_eq!(maximum_warnings, MAX_CAPTURE_WARNINGS);

        let mut too_many_tracks = valid_value();
        let track = too_many_tracks["tracks"][0].clone();
        let tracks = too_many_tracks["tracks"].as_array_mut().unwrap();
        for index in 2..=maximum_tracks + 1 {
            let mut item = track.clone();
            item["relative_path"] = json!(format!("tracks/mic-{index:04}.m4a"));
            tracks.push(item);
        }
        assert!(serde_json::from_value::<RecordingEnvelope>(too_many_tracks).is_err());

        let mut too_many_warnings = valid_value();
        too_many_warnings["capture_warnings"] = Value::Array(
            (0..=maximum_warnings)
                .map(|index| {
                    json!({
                        "code": "route_changed",
                        "message": format!("Fabricated warning {index}"),
                        "at_ms": 1
                    })
                })
                .collect(),
        );
        assert!(serde_json::from_value::<RecordingEnvelope>(too_many_warnings).is_err());
    }

    fn schema_enum(schema: &Value, pointer: &str) -> BTreeSet<String> {
        schema
            .pointer(pointer)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect()
    }

    fn values<T: Copy>(all: &[T], as_str: impl Fn(T) -> &'static str) -> BTreeSet<String> {
        all.iter().copied().map(as_str).map(str::to_owned).collect()
    }

    #[test]
    fn rust_enum_values_stay_in_sync_with_the_public_schema() {
        let schema: Value = serde_json::from_str(SCHEMA).unwrap();
        assert_eq!(
            schema_enum(&schema, "/$defs/source/properties/kind/enum"),
            values(SourceKind::ALL, SourceKind::as_str)
        );
        assert_eq!(
            schema_enum(&schema, "/$defs/source/properties/platform/enum"),
            values(Platform::ALL, Platform::as_str)
        );
        assert_eq!(
            schema_enum(&schema, "/$defs/source/properties/capture_scope/enum"),
            values(CaptureScope::ALL, CaptureScope::as_str)
        );
        assert_eq!(
            schema_enum(&schema, "/$defs/track/properties/role/enum"),
            values(TrackRole::ALL, TrackRole::as_str)
        );
        assert_eq!(
            schema_enum(&schema, "/$defs/job/properties/state/enum"),
            values(&JobState::ALL, JobState::as_str)
        );
    }
}
