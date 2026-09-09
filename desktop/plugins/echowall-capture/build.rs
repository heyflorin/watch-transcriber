const COMMANDS: &[&str] = &[
    "check_permissions",
    "request_permissions",
    "preflight",
    "start",
    "pause",
    "resume",
    "stop",
    "status",
    "open_audio_picker",
    "drain_shared_imports",
    "acknowledge_shared_imports",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).ios_path("ios").build();
    println!("cargo:rustc-check-cfg=cfg(mobile)");
    if matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("ios" | "android")
    ) {
        println!("cargo:rustc-cfg=mobile");
    }
}
