#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::{
    io::Write,
    process::{Command, Stdio},
};

fn rejected(args: &[&str], input: &[u8], expected: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut request = tempfile::NamedTempFile::new().unwrap();
    request.write_all(input).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_echowall-moss-worker"))
        .args(args)
        .env_clear()
        .env("ECHOWALL_APP_DATA_ROOT", root.path())
        .stdin(Stdio::from(request.reopen().unwrap()))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "rejected input must not emit a success frame"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!("echowall_moss_worker_error:{expected}\n")
    );
}

#[test]
fn arguments_and_untrusted_json_never_escape_through_errors() {
    rejected(&["private-path-canary"], b"", "arguments_forbidden");
    rejected(&[], b"{\"private-token-canary\":true}", "invalid_request");
    rejected(&[], b"", "invalid_request");
}

#[test]
fn oversized_frames_fail_before_model_or_audio_access() {
    rejected(
        &[],
        &vec![b'a'; echowall_local_moss_protocol::MAX_REQUEST_BYTES + 1],
        "invalid_request",
    );
}
