//! Desktop tray/menu-bar controls for an active recording.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::menu::{Menu, MenuItem, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{App, AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use uuid::Uuid;

use crate::capture::commands::CaptureCommandState;
use crate::capture::session::CapturePhase;
use crate::processing::commands::EmbeddedProcessingState;

const TRAY_ID: &str = "echowall-recording";
const STATUS_ID: &str = "tray-status";
const PAUSE_ID: &str = "tray-pause";
const STOP_ID: &str = "tray-stop";
const SHOW_ID: &str = "tray-show";
const QUIT_ID: &str = "tray-quit";

static QUIT_PROMPT_OPEN: AtomicBool = AtomicBool::new(false);

pub fn install(app: &App) -> tauri::Result<()> {
    let status = MenuItemBuilder::with_id(STATUS_ID, "未在录音")
        .enabled(false)
        .build(app)?;
    let pause = MenuItemBuilder::with_id(PAUSE_ID, "暂停")
        .enabled(false)
        .build(app)?;
    let stop = MenuItemBuilder::with_id(STOP_ID, "停止并保存")
        .enabled(false)
        .build(app)?;
    let show = MenuItemBuilder::with_id(SHOW_ID, "打开 EchoWall").build(app)?;
    let quit = MenuItemBuilder::with_id(QUIT_ID, "退出 EchoWall").build(app)?;
    let menu = Menu::with_items(app, &[&status, &pause, &stop, &show, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("EchoWall");
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone()).icon_as_template(true);
    }
    let tray = builder
        .on_menu_event(|app, event| match event.id().as_ref() {
            SHOW_ID => show_main_window(app),
            PAUSE_ID => {
                let _ = app.state::<CaptureCommandState>().toggle_pause_for_tray();
            }
            STOP_ID => {
                let _ = stop_and_enqueue(app);
            }
            QUIT_ID => request_quit(app),
            _ => {}
        })
        .build(app)?;

    let app_handle = app.handle().clone();
    tauri::async_runtime::spawn(update_loop(app_handle, tray, status, pause, stop));
    Ok(())
}

pub fn request_quit(app: &AppHandle) {
    if !app.state::<CaptureCommandState>().is_active() {
        app.exit(0);
        return;
    }
    if QUIT_PROMPT_OPEN.swap(true, Ordering::SeqCst) {
        show_main_window(app);
        return;
    }
    let handle = app.clone();
    app.dialog()
        .message("正在录音。退出前会先关闭当前分段并保存到本机；处理可以在下次启动继续。")
        .title("停止录音并退出 EchoWall？")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "停止并退出".to_owned(),
            "继续录音".to_owned(),
        ))
        .show(move |confirmed| {
            if confirmed {
                if stop_and_enqueue(&handle) {
                    handle.exit(0);
                    return;
                }
                show_main_window(&handle);
                handle
                    .dialog()
                    .message("录音尚未安全收尾，EchoWall 没有退出。请打开窗口后重试停止。")
                    .title("无法退出")
                    .show(|_| {});
            }
            QUIT_PROMPT_OPEN.store(false, Ordering::SeqCst);
        });
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn stop_and_enqueue(app: &AppHandle) -> bool {
    let stopped = match app.state::<CaptureCommandState>().stop_for_tray() {
        Ok(stopped) => stopped,
        Err(_) => return false,
    };
    let Ok(recording_id) = Uuid::parse_str(&stopped.recording_id) else {
        return false;
    };
    let processor = app.state::<EmbeddedProcessingState>().processor();
    if processor.enqueue(recording_id).is_err() {
        return false;
    }
    show_main_window(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval("location.reload()");
    }
    if app
        .state::<crate::features::RuntimeFeatures>()
        .direct_processing
    {
        tauri::async_runtime::spawn(async move {
            let _ = processor.process(recording_id).await;
        });
    }
    true
}

async fn update_loop(
    app: AppHandle,
    tray: tauri::tray::TrayIcon,
    status: MenuItem<tauri::Wry>,
    pause: MenuItem<tauri::Wry>,
    stop: MenuItem<tauri::Wry>,
) {
    loop {
        match app.state::<CaptureCommandState>().tray_snapshot() {
            Ok(Some(snapshot)) => {
                let elapsed = format_duration(snapshot.duration_ms);
                let state = match snapshot.phase {
                    CapturePhase::Paused => "已暂停",
                    CapturePhase::SourceLost => "来源中断",
                    CapturePhase::Interrupted => "已中断",
                    CapturePhase::Finalizing => "正在保存",
                    _ => "正在录音",
                };
                let _ = status.set_text(format!("{state} · {elapsed} · {}", snapshot.source_label));
                let _ = pause.set_text(if snapshot.phase == CapturePhase::Paused {
                    "继续"
                } else {
                    "暂停"
                });
                let can_toggle = matches!(
                    snapshot.phase,
                    CapturePhase::Recording | CapturePhase::Paused
                );
                let _ = pause.set_enabled(can_toggle);
                let _ = stop.set_enabled(true);
                let _ = tray.set_title(Some(format!("● {elapsed}")));
                let _ = tray.set_tooltip(Some(format!(
                    "EchoWall · {state} · {}",
                    snapshot.source_label
                )));
            }
            _ => {
                let _ = status.set_text("未在录音");
                let _ = pause.set_text("暂停");
                let _ = pause.set_enabled(false);
                let _ = stop.set_enabled(false);
                let _ = tray.set_title(None::<&str>);
                let _ = tray.set_tooltip(Some("EchoWall"));
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn format_duration(duration_ms: u64) -> String {
    let seconds = duration_ms / 1_000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_duration_is_hours_minutes_seconds() {
        assert_eq!(format_duration(3_661_999), "01:01:01");
    }
}
