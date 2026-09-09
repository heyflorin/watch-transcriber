use tauri::{plugin::TauriPlugin, Manager, Runtime};

use serde::{de::DeserializeOwned, Serialize};
#[cfg(mobile)]
use tauri::plugin::PluginHandle;

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_capture);

pub struct Capture<R: Runtime> {
    #[cfg(mobile)]
    handle: PluginHandle<R>,
    #[cfg(not(mobile))]
    marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: Runtime> Capture<R> {
    #[cfg(mobile)]
    pub async fn invoke<T: DeserializeOwned>(
        &self,
        command: &str,
        payload: impl Serialize,
    ) -> Result<T, String> {
        self.handle
            .run_mobile_plugin_async(command, payload)
            .await
            .map_err(|_| "native capture operation failed".to_owned())
    }
}

pub trait CaptureExt<R: Runtime> {
    fn echowall_capture(&self) -> &Capture<R>;
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartArgs {
    recording_id: String,
    mode: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordingArgs {
    recording_id: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcknowledgeSharedImportsArgs {
    import_ids: Vec<String>,
}

async fn invoke_native<R: Runtime, T: DeserializeOwned>(
    app: tauri::AppHandle<R>,
    command: &str,
    payload: impl Serialize,
) -> Result<T, String> {
    #[cfg(mobile)]
    {
        app.echowall_capture().invoke(command, payload).await
    }
    #[cfg(not(mobile))]
    {
        let _ = (app, command, payload);
        Err("native iOS capture is unavailable".to_owned())
    }
}

#[tauri::command]
async fn check_permissions<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<serde_json::Value, String> {
    invoke_native(app, "checkPermissions", ()).await
}

#[tauri::command]
async fn request_permissions<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<serde_json::Value, String> {
    invoke_native(app, "requestPermissions", ()).await
}

#[tauri::command]
async fn preflight<R: Runtime>(app: tauri::AppHandle<R>) -> Result<serde_json::Value, String> {
    invoke_native(app, "preflight", ()).await
}

#[tauri::command]
async fn start<R: Runtime>(
    app: tauri::AppHandle<R>,
    recording_id: String,
    mode: String,
) -> Result<serde_json::Value, String> {
    invoke_native(app, "start", StartArgs { recording_id, mode }).await
}

macro_rules! recording_command {
    ($name:ident, $native:literal) => {
        #[tauri::command]
        async fn $name<R: Runtime>(
            app: tauri::AppHandle<R>,
            recording_id: String,
        ) -> Result<serde_json::Value, String> {
            invoke_native(app, $native, RecordingArgs { recording_id }).await
        }
    };
}

recording_command!(pause, "pause");
recording_command!(resume, "resume");
recording_command!(stop, "stop");

#[tauri::command]
async fn status<R: Runtime>(
    app: tauri::AppHandle<R>,
    recording_id: Option<String>,
) -> Result<serde_json::Value, String> {
    invoke_native(
        app,
        "status",
        serde_json::json!({ "recordingId": recording_id }),
    )
    .await
}

#[tauri::command]
async fn open_audio_picker<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<serde_json::Value, String> {
    invoke_native(app, "openAudioPicker", ()).await
}

#[tauri::command]
async fn drain_shared_imports<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<serde_json::Value, String> {
    invoke_native(app, "drainSharedImports", ()).await
}

#[tauri::command]
async fn acknowledge_shared_imports<R: Runtime>(
    app: tauri::AppHandle<R>,
    import_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    invoke_native(
        app,
        "acknowledgeSharedImports",
        AcknowledgeSharedImportsArgs { import_ids },
    )
    .await
}

impl<R: Runtime, T: Manager<R>> CaptureExt<R> for T {
    fn echowall_capture(&self) -> &Capture<R> {
        self.state::<Capture<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    tauri::plugin::Builder::new("echowall-capture")
        .invoke_handler(tauri::generate_handler![
            check_permissions,
            request_permissions,
            preflight,
            start,
            pause,
            resume,
            stop,
            status,
            open_audio_picker,
            drain_shared_imports,
            acknowledge_shared_imports,
        ])
        .setup(|app, _api| {
            #[cfg(target_os = "ios")]
            let capture: Capture<R> = Capture {
                handle: _api.register_ios_plugin(init_plugin_capture)?,
            };
            #[cfg(target_os = "android")]
            let capture: Capture<R> = Capture {
                handle: _api
                    .register_android_plugin("ai.ax.watch_transcriber.capture", "RecorderPlugin")?,
            };
            #[cfg(not(mobile))]
            let capture: Capture<R> = Capture {
                marker: std::marker::PhantomData,
            };
            app.manage(capture);
            Ok(())
        })
        .build()
}
