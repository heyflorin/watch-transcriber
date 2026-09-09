use serde::{Deserialize, Serialize};

use crate::ingest::envelope::{CaptureScope, Platform, RecordingSource, SourceKind, TrackRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    VoiceMemo,
    Meeting,
    SystemCapture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioInputSelection {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioOutputSelection {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MeetingSource {
    NativeApplication { id: String, label: String },
    WholeBrowser { id: String, label: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureTarget {
    VoiceMemo {
        microphone: AudioInputSelection,
    },
    Meeting {
        microphone: AudioInputSelection,
        source: MeetingSource,
    },
    SystemCapture {
        microphone: AudioInputSelection,
        output: AudioOutputSelection,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentAcknowledgements {
    pub microphone: bool,
    pub selected_source: bool,
    pub whole_browser_warning: bool,
    pub all_system_audio: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePlan {
    pub mode: CaptureMode,
    pub platform: Platform,
    pub target: CaptureTarget,
    pub consent: ConsentAcknowledgements,
}

impl CapturePlan {
    pub fn validate(&self) -> Result<(), CaptureModelError> {
        if !matches!(self.platform, Platform::Macos | Platform::Windows) {
            return Err(CaptureModelError::new(
                "unsupported_platform",
                "desktop capture requires macOS or Windows",
            ));
        }
        let microphone = match &self.target {
            CaptureTarget::VoiceMemo { microphone }
            | CaptureTarget::Meeting { microphone, .. }
            | CaptureTarget::SystemCapture { microphone, .. } => microphone,
        };
        validate_identity(&microphone.id, "microphone id")?;
        validate_label(&microphone.label, "microphone label")?;
        if !self.consent.microphone {
            return Err(CaptureModelError::new(
                "microphone_consent_required",
                "microphone capture requires explicit acknowledgement",
            ));
        }

        match (&self.mode, &self.target) {
            (CaptureMode::VoiceMemo, CaptureTarget::VoiceMemo { .. }) => Ok(()),
            (CaptureMode::Meeting, CaptureTarget::Meeting { source, .. }) => {
                if !self.consent.selected_source {
                    return Err(CaptureModelError::new(
                        "source_consent_required",
                        "meeting capture requires explicit selected-source acknowledgement",
                    ));
                }
                match source {
                    MeetingSource::NativeApplication { id, label } => {
                        validate_identity(id, "application id")?;
                        validate_label(label, "application label")?;
                    }
                    MeetingSource::WholeBrowser { id, label } => {
                        validate_identity(id, "browser id")?;
                        validate_label(label, "browser label")?;
                        if !self.consent.whole_browser_warning {
                            return Err(CaptureModelError::new(
                                "browser_scope_consent_required",
                                "whole-browser capture requires warning acknowledgement",
                            ));
                        }
                    }
                }
                Ok(())
            }
            (CaptureMode::SystemCapture, CaptureTarget::SystemCapture { output, .. }) => {
                validate_identity(&output.id, "system output id")?;
                validate_label(&output.label, "system output label")?;
                if !self.consent.all_system_audio {
                    return Err(CaptureModelError::new(
                        "system_scope_consent_required",
                        "all-system capture requires explicit acknowledgement",
                    ));
                }
                Ok(())
            }
            _ => Err(CaptureModelError::new(
                "mode_target_mismatch",
                "capture mode and selected target do not match",
            )),
        }
    }

    pub fn required_roles(&self) -> &'static [TrackRole] {
        match self.mode {
            CaptureMode::VoiceMemo => &[TrackRole::Microphone],
            CaptureMode::Meeting | CaptureMode::SystemCapture => {
                &[TrackRole::Microphone, TrackRole::System]
            }
        }
    }

    pub fn recording_source(&self) -> RecordingSource {
        match &self.target {
            CaptureTarget::VoiceMemo { microphone } => RecordingSource {
                kind: SourceKind::DesktopVoiceMemo,
                platform: self.platform,
                label: Some(microphone.label.clone()),
                capture_scope: CaptureScope::Microphone,
            },
            CaptureTarget::Meeting { source, .. } => match source {
                MeetingSource::NativeApplication { label, .. } => RecordingSource {
                    kind: SourceKind::DesktopMeeting,
                    platform: self.platform,
                    label: Some(label.clone()),
                    capture_scope: if self.platform == Platform::Macos {
                        CaptureScope::Application
                    } else {
                        CaptureScope::ProcessTree
                    },
                },
                MeetingSource::WholeBrowser { label, .. } => RecordingSource {
                    kind: SourceKind::DesktopMeeting,
                    platform: self.platform,
                    label: Some(label.clone()),
                    capture_scope: CaptureScope::BrowserApplication,
                },
            },
            CaptureTarget::SystemCapture { output, .. } => RecordingSource {
                kind: SourceKind::DesktopSystem,
                platform: self.platform,
                label: Some(output.label.clone()),
                capture_scope: CaptureScope::System,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    NotRequired,
    NotDetermined,
    Granted,
    Denied,
    Restricted,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    Available,
    Missing,
    Silent,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePreflight {
    pub backend_available: bool,
    pub microphone_permission: PermissionState,
    pub system_audio_permission: PermissionState,
    pub selected_source: SourceAvailability,
    pub warnings: Vec<String>,
}

impl CapturePreflight {
    pub fn ready_for(&self, plan: &CapturePlan) -> bool {
        self.backend_available
            && self.microphone_permission == PermissionState::Granted
            && match plan.mode {
                CaptureMode::VoiceMemo => true,
                CaptureMode::Meeting | CaptureMode::SystemCapture => {
                    self.system_audio_permission == PermissionState::Granted
                        && matches!(
                            self.selected_source,
                            SourceAvailability::Available | SourceAvailability::Silent
                        )
                }
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureModelError {
    pub code: &'static str,
    pub message: &'static str,
}

impl CaptureModelError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl std::fmt::Display for CaptureModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for CaptureModelError {}

fn validate_identity(value: &str, field: &str) -> Result<(), CaptureModelError> {
    if value.is_empty() || value.chars().count() > 512 || value.chars().any(char::is_control) {
        return Err(CaptureModelError::new(
            "invalid_source_identity",
            if field == "microphone id" {
                "microphone id is invalid"
            } else {
                "selected source id is invalid"
            },
        ));
    }
    Ok(())
}

fn validate_label(value: &str, field: &str) -> Result<(), CaptureModelError> {
    if value.is_empty() || value.chars().count() > 512 || value.chars().any(char::is_control) {
        return Err(CaptureModelError::new(
            "invalid_source_label",
            if field == "microphone label" {
                "microphone label is invalid"
            } else {
                "selected source label is invalid"
            },
        ));
    }
    Ok(())
}
