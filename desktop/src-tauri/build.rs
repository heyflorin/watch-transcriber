fn main() {
    // `screencapturekit` uses a narrow Swift bridge internally. Swift runtime
    // libraries ship in macOS's dyld shared cache, but rustc does not add the
    // system Swift rpath when the final executable contains no Swift target of
    // its own. Without this, even a no-test binary fails before `main`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        build_macos_process_tap();
    }
    const APP_COMMANDS: &[&str] = &[
        "accept_local_transcript_only",
        "adopt_mobile_imports",
        "cancel_processing_recording",
        "capture_capabilities",
        "capture_permissions",
        "capture_preflight",
        "capture_request_permissions",
        "capture_source_icon",
        "capture_sources",
        "capture_status",
        "confirm_import_review",
        "delete_processing_credentials",
        "delete_sync_credentials",
        "discard_processing_recording",
        "export_recording_original",
        "finalize_mobile_capture",
        "finalize_pending_mobile_capture",
        "get_processing_preference",
        "import_audio_files",
        "install_local_model_pack",
        "install_moss_candidate_model_pack",
        "install_qwen_candidate_model_pack",
        "install_speakerkit_candidate_model_pack",
        "list_pending_mobile_recordings",
        "local_model_pack_status",
        "moss_candidate_model_pack_status",
        "moss_pipeline_model_status",
        "install_moss_pipeline_model_pack",
        "remove_moss_pipeline_model_pack",
        "qwen_candidate_model_pack_status",
        "speakerkit_candidate_model_pack_status",
        "list_processing_recordings",
        "mobile_acknowledge_shared_imports",
        "mobile_check_permissions",
        "mobile_drain_shared_imports",
        "mobile_open_audio_picker",
        "mobile_pause",
        "mobile_preflight",
        "mobile_request_permissions",
        "mobile_resume",
        "mobile_start",
        "mobile_status",
        "mobile_stop",
        "open_capture_permission_settings",
        "pause_capture",
        "process_recordings",
        "process_recording_with_local_models",
        "process_recording_with_moss_candidate",
        "process_recording_with_qwen_candidate",
        "processing_credentials_status",
        "reprocess_recording",
        "remove_local_model_pack",
        "remove_moss_candidate_model_pack",
        "remove_qwen_candidate_model_pack",
        "remove_speakerkit_candidate_model_pack",
        "resume_capture",
        "retry_processing_recording",
        "runtime_features",
        "save_processing_credentials",
        "save_sync_credentials",
        "set_processing_preference",
        "cancel_local_model_install",
        "start_capture",
        "stop_capture",
        "take_over_processing_with_local_models",
        "take_over_processing_with_qwen_candidate",
    ];
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(APP_COMMANDS));
    tauri_build::try_build(attributes).expect("failed to build Tauri application manifest")
}

fn build_macos_process_tap() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").expect("target architecture is missing");
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo output directory is missing"),
    );
    let source = "native/macos/ProcessTapBridge.swift";
    println!("cargo:rerun-if-changed={source}");
    let status = std::process::Command::new("xcrun")
        .args([
            "swiftc",
            "-parse-as-library",
            "-O",
            "-target",
            &format!("{arch}-apple-macosx13.0"),
            "-emit-library",
            "-static",
            "-module-name",
            "EchoWallProcessTap",
            source,
            "-o",
        ])
        .arg(output.join("libechowall_process_tap.a"))
        .args([
            "-framework",
            "AVFoundation",
            "-framework",
            "AudioToolbox",
            "-framework",
            "CoreAudio",
        ])
        .status()
        .expect("failed to start Swift compiler for the process-tap bridge");
    assert!(status.success(), "failed to compile the process-tap bridge");
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=static=echowall_process_tap");
    for framework in ["AVFoundation", "AudioToolbox", "CoreAudio"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
