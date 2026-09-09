//! Release-time/runtime kill switches for independently holding risky paths.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFeatures {
    pub recording: bool,
    pub audio_import: bool,
    pub direct_processing: bool,
    pub browser_capture: bool,
    pub local_stt: bool,
    pub local_qwen_candidate: bool,
    pub local_speakerkit_candidate: bool,
    pub local_moss_candidate: bool,
}

impl RuntimeFeatures {
    pub fn load() -> Self {
        Self {
            recording: enabled(
                "ECHOWALL_RECORDING_ENABLED",
                option_env!("ECHOWALL_RECORDING_ENABLED"),
            ),
            audio_import: enabled(
                "ECHOWALL_IMPORT_ENABLED",
                option_env!("ECHOWALL_IMPORT_ENABLED"),
            ),
            direct_processing: enabled(
                "ECHOWALL_PROCESSING_ENABLED",
                option_env!("ECHOWALL_PROCESSING_ENABLED"),
            ),
            browser_capture: enabled(
                "ECHOWALL_BROWSER_CAPTURE_ENABLED",
                option_env!("ECHOWALL_BROWSER_CAPTURE_ENABLED"),
            ),
            local_stt: full_local_supported()
                && enabled(
                    "ECHOWALL_LOCAL_STT_ENABLED",
                    option_env!("ECHOWALL_LOCAL_STT_ENABLED"),
                ),
            local_qwen_candidate: full_local_supported()
                && opt_in_enabled(
                    "ECHOWALL_QWEN_CANDIDATE_ENABLED",
                    option_env!("ECHOWALL_QWEN_CANDIDATE_ENABLED"),
                ),
            local_speakerkit_candidate: full_local_supported()
                && opt_in_enabled(
                    "ECHOWALL_SPEAKERKIT_CANDIDATE_ENABLED",
                    option_env!("ECHOWALL_SPEAKERKIT_CANDIDATE_ENABLED"),
                ),
            local_moss_candidate: moss_candidate_enabled(
                full_local_supported(),
                std::env::var("ECHOWALL_MOSS_CANDIDATE_ENABLED")
                    .ok()
                    .as_deref(),
                option_env!("ECHOWALL_MOSS_CANDIDATE_ENABLED"),
            ),
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn full_local_supported() -> bool {
    use objc2_foundation::NSProcessInfo;

    NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 14
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub const fn full_local_supported() -> bool {
    false
}

fn enabled(runtime_name: &str, compiled: Option<&str>) -> bool {
    std::env::var(runtime_name)
        .ok()
        .as_deref()
        .or(compiled)
        .is_none_or(parse_enabled)
}

fn opt_in_enabled(runtime_name: &str, compiled: Option<&str>) -> bool {
    std::env::var(runtime_name)
        .ok()
        .as_deref()
        .or(compiled)
        .is_some_and(parse_enabled)
}

fn parse_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

fn moss_candidate_enabled(supported: bool, runtime: Option<&str>, compiled: Option<&str>) -> bool {
    // Retain the existing kill-switch and IPC names for installed clients.
    supported && runtime.or(compiled).is_none_or(parse_enabled)
}

#[tauri::command]
pub fn runtime_features(state: tauri::State<'_, RuntimeFeatures>) -> RuntimeFeatures {
    state.inner().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_switch_values_are_conservative_and_explicit() {
        for disabled in ["0", "false", "OFF", " no "] {
            assert!(!parse_enabled(disabled));
        }
        for enabled in ["1", "true", "on", "unexpected"] {
            assert!(parse_enabled(enabled));
        }
        assert!(!opt_in_enabled("ECHOWALL_TEST_MISSING_OPT_IN", None));
    }

    #[test]
    fn moss_is_available_by_default_platform_gated_and_independently_disableable() {
        assert!(moss_candidate_enabled(true, None, None));
        assert!(moss_candidate_enabled(true, Some("1"), None));
        assert!(moss_candidate_enabled(true, None, Some("true")));
        assert!(!moss_candidate_enabled(true, Some("off"), Some("1")));
        assert!(!moss_candidate_enabled(true, None, Some("false")));
        assert!(moss_candidate_enabled(true, Some("1"), Some("false")));
        assert!(!moss_candidate_enabled(false, None, None));
        assert!(!moss_candidate_enabled(false, Some("1"), Some("1")));
        let features = RuntimeFeatures {
            recording: true,
            audio_import: true,
            direct_processing: true,
            browser_capture: true,
            local_stt: true,
            local_qwen_candidate: false,
            local_speakerkit_candidate: false,
            local_moss_candidate: moss_candidate_enabled(true, Some("1"), None),
        };
        let value = serde_json::to_value(features).unwrap();
        assert_eq!(value["localMossCandidate"], true);
        assert_eq!(value["localQwenCandidate"], false);
        assert_eq!(value["localSpeakerkitCandidate"], false);
        assert_eq!(value["localStt"], true);
    }
}
