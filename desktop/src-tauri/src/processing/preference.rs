//! App-owned choice for new recordings, independent of the viewer's random
//! loopback origin. This setting never changes an existing processing ledger.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const FILE_NAME: &str = "processing-preference.json";
const MAX_BYTES: u64 = 1024;
const READ_ERROR: &str = "processing preference could not be read";
const WRITE_ERROR: &str = "processing preference could not be saved";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewRecordingRoute {
    Remote,
    WhisperLocal,
    MossLocal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingPreference {
    /// Only an absent preference file means that no choice has been saved.
    pub new_recording_route: Option<NewRecordingRoute>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetProcessingPreferenceRequest {
    pub new_recording_route: NewRecordingRoute,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredPreference {
    schema_version: u32,
    new_recording_route: NewRecordingRoute,
}

pub struct ProcessingPreferenceState {
    root: PathBuf,
    writer: Mutex<()>,
}

impl ProcessingPreferenceState {
    pub fn open(app_data_directory: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(app_data_directory)?;
        if !fs::symlink_metadata(app_data_directory)?
            .file_type()
            .is_dir()
        {
            return Err(std::io::Error::other("invalid App preference directory"));
        }
        Ok(Self {
            root: fs::canonicalize(app_data_directory)?,
            writer: Mutex::new(()),
        })
    }

    fn check_root(&self) -> std::io::Result<()> {
        if !fs::symlink_metadata(&self.root)?.file_type().is_dir()
            || fs::canonicalize(&self.root)? != self.root
        {
            return Err(std::io::Error::other("invalid App preference directory"));
        }
        Ok(())
    }

    fn get(&self) -> Result<ProcessingPreference, String> {
        let _guard = self.writer.lock().map_err(|_| READ_ERROR.to_owned())?;
        self.check_root().map_err(|_| READ_ERROR.to_owned())?;
        let path = self.root.join(FILE_NAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ProcessingPreference {
                    new_recording_route: None,
                });
            }
            Err(_) => return Err(READ_ERROR.to_owned()),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_BYTES {
            return Err(READ_ERROR.to_owned());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let file = options.open(path).map_err(|_| READ_ERROR.to_owned())?;
        if !file
            .metadata()
            .map_err(|_| READ_ERROR.to_owned())?
            .is_file()
        {
            return Err(READ_ERROR.to_owned());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| READ_ERROR.to_owned())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(READ_ERROR.to_owned());
        }
        let stored: StoredPreference =
            serde_json::from_slice(&bytes).map_err(|_| READ_ERROR.to_owned())?;
        if stored.schema_version != 1 {
            return Err(READ_ERROR.to_owned());
        }
        Ok(ProcessingPreference {
            new_recording_route: Some(stored.new_recording_route),
        })
    }

    fn set(&self, route: NewRecordingRoute) -> Result<ProcessingPreference, String> {
        let _guard = self.writer.lock().map_err(|_| WRITE_ERROR.to_owned())?;
        self.check_root().map_err(|_| WRITE_ERROR.to_owned())?;
        let destination = self.root.join(FILE_NAME);
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(WRITE_ERROR.to_owned()),
        }
        let bytes = serde_json::to_vec(&StoredPreference {
            schema_version: 1,
            new_recording_route: route,
        })
        .map_err(|_| WRITE_ERROR.to_owned())?;
        let temporary = PendingPreference(
            self.root
                .join(format!(".processing-preference-{}.tmp", Uuid::new_v4())),
        );
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary.0)
            .map_err(|_| WRITE_ERROR.to_owned())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| WRITE_ERROR.to_owned())?;
        drop(file);
        // Reuse the ledger's atomic replace, including Windows write-through.
        super::replace_file(&temporary.0, &destination).map_err(|_| WRITE_ERROR.to_owned())?;
        super::sync_directory(&self.root).map_err(|_| WRITE_ERROR.to_owned())?;
        Ok(ProcessingPreference {
            new_recording_route: Some(route),
        })
    }
}

struct PendingPreference(PathBuf);

impl Drop for PendingPreference {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[tauri::command]
pub fn get_processing_preference(
    state: tauri::State<'_, ProcessingPreferenceState>,
) -> Result<ProcessingPreference, String> {
    state.get()
}

#[tauri::command]
pub fn set_processing_preference(
    request: SetProcessingPreferenceRequest,
    state: tauri::State<'_, ProcessingPreferenceState>,
) -> Result<ProcessingPreference, String> {
    state.set(request.new_recording_route)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_survive_reopen_and_app_roots_remain_isolated() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("production");
        let qa = temp.path().join("qa");
        let state = ProcessingPreferenceState::open(&app).unwrap();
        assert_eq!(
            serde_json::to_value(state.get().unwrap()).unwrap(),
            serde_json::json!({"newRecordingRoute": null})
        );
        for route in [
            NewRecordingRoute::WhisperLocal,
            NewRecordingRoute::MossLocal,
            NewRecordingRoute::Remote,
        ] {
            let saved = state.set(route).unwrap();
            assert_eq!(saved.new_recording_route, Some(route));
            assert_eq!(
                ProcessingPreferenceState::open(&app)
                    .unwrap()
                    .get()
                    .unwrap(),
                saved
            );
            assert!(ProcessingPreferenceState::open(&qa)
                .unwrap()
                .get()
                .unwrap()
                .new_recording_route
                .is_none());
        }
        assert_eq!(fs::read_dir(app).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_oversized_and_non_file_preferences_never_become_remote_or_absent() {
        let temp = tempfile::tempdir().unwrap();
        let state = ProcessingPreferenceState::open(temp.path()).unwrap();
        let path = state.root.join(FILE_NAME);
        for value in [
            b"{".to_vec(),
            br#"{"schemaVersion":1,"newRecordingRoute":null}"#.to_vec(),
            br#"{"schemaVersion":2,"newRecordingRoute":"remote"}"#.to_vec(),
            br#"{"schemaVersion":1,"newRecordingRoute":"unknown"}"#.to_vec(),
            br#"{"schemaVersion":1,"newRecordingRoute":"remote","extra":true}"#.to_vec(),
            vec![b' '; MAX_BYTES as usize + 1],
        ] {
            fs::write(&path, &value).unwrap();
            assert_eq!(state.get().unwrap_err(), READ_ERROR);
            assert_eq!(fs::read(&path).unwrap(), value);
        }
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(state.get().unwrap_err(), READ_ERROR);
        assert_eq!(
            state.set(NewRecordingRoute::Remote).unwrap_err(),
            WRITE_ERROR
        );
        assert!(path.is_dir());
    }

    #[test]
    fn request_requires_an_explicit_supported_choice_without_extra_fields() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"newRecordingRoute": null}),
            serde_json::json!({"newRecordingRoute": "qwen_local"}),
            serde_json::json!({"newRecordingRoute": "remote", "extra": true}),
        ] {
            assert!(serde_json::from_value::<SetProcessingPreferenceRequest>(value).is_err());
        }
        for choice in ["remote", "whisper_local", "moss_local"] {
            assert!(serde_json::from_value::<SetProcessingPreferenceRequest>(
                serde_json::json!({"newRecordingRoute": choice})
            )
            .is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_preference_is_neither_followed_nor_overwritten() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let state = ProcessingPreferenceState::open(&temp.path().join("app")).unwrap();
        let outside = temp.path().join("outside.json");
        let bytes = br#"{"schemaVersion":1,"newRecordingRoute":"remote"}"#;
        fs::write(&outside, bytes).unwrap();
        symlink(&outside, state.root.join(FILE_NAME)).unwrap();
        assert_eq!(state.get().unwrap_err(), READ_ERROR);
        assert_eq!(
            state.set(NewRecordingRoute::MossLocal).unwrap_err(),
            WRITE_ERROR
        );
        assert_eq!(fs::read(outside).unwrap(), bytes);
    }
}
