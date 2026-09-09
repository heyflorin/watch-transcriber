//! Separate process exits before Rust Drop at actual ledger/file cut points.
//! Fixtures contain only fabricated audio bytes; no model or native capture.
use super::*;
use crate::ingest::inbox::Inbox;
use crate::processing::ProcessingStore;
use std::{fs, process::Command};

const ROOT: &str = "ECHOWALL_RESPONSE_CRASH_FIXTURE_ROOT";
const RECORDING: &str = "ECHOWALL_RESPONSE_CRASH_FIXTURE_ID";
const CUT: &str = "ECHOWALL_RESPONSE_CRASH_FIXTURE_CUT";

#[tokio::test]
async fn process_loss_before_and_after_publication_replays_only_missing_effects() {
    for (cut, anchors, published) in [
        ("window-intent", false, false),
        ("window-published", false, true),
        ("anchor-intent", true, false),
        ("anchor-published", true, true),
    ] {
        let fixture = Fixture::new();
        let root = fixture.engine.store.root().parent().unwrap();
        let test = format!(
            "{}::interrupted_response_process",
            module_path!().split_once("::").unwrap().1
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--ignored", "--nocapture"])
            .env_clear()
            .env(ROOT, root)
            .env(RECORDING, fixture.id.to_string())
            .env(CUT, cut)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(73),
            "child cut failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let before = fixture.engine.store.load(fixture.id).unwrap();
        let checkpoint = before.local_moss.as_ref().unwrap();
        let pending = checkpoint.pending_response().unwrap().reference().clone();
        assert_eq!(checkpoint.completed_windows().len(), usize::from(anchors));
        assert_eq!(
            fixture
                .artifacts
                .read_if_present(&pending)
                .unwrap()
                .is_some(),
            published
        );
        // The child exited without dropping its lease; the OS, not an in-
        // process RAII simulation, must make the recording available again.
        drop(fixture.artifacts.try_owner().unwrap());
        let complete = fixture.engine.run_until_wait(fixture.id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        let calls = fixture.effects.calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|&&call| call == "window").count(),
            usize::from(!anchors && !published)
        );
        assert_eq!(
            calls.iter().filter(|&&call| call == "anchors").count(),
            usize::from(!anchors || !published)
        );
        assert_eq!(calls.iter().filter(|&&call| call == "summary").count(), 1);
        assert_eq!(calls.iter().filter(|&&call| call == "publish").count(), 1);
        assert_eq!(
            complete.transcript_json.as_ref().unwrap()[0]["content"],
            if !anchors && published {
                "first 世界 saved before crash"
            } else {
                "first 世界"
            }
        );
        assert!(complete
            .local_moss
            .as_ref()
            .unwrap()
            .pending_response()
            .is_none());
    }
}

#[tokio::test]
#[ignore = "exact synthetic recording fixture and intentional process exit; invoked by recovery test"]
async fn interrupted_response_process() {
    let root = std::path::PathBuf::from(std::env::var_os(ROOT).expect("fixture root"));
    let id = Uuid::parse_str(&std::env::var(RECORDING).expect("fixture id")).unwrap();
    let (anchors, publish) = match std::env::var(CUT).as_deref() {
        Ok("window-intent") => (false, false),
        Ok("window-published") => (false, true),
        Ok("anchor-intent") => (true, false),
        Ok("anchor-published") => (true, true),
        _ => panic!("unknown synthetic cut"),
    };
    assert!(root.is_absolute());
    assert_eq!(
        fs::read(
            root.join("inbox")
                .join(id.to_string())
                .join("tracks/input.wav")
        )
        .unwrap(),
        b"fabricated normalized audio"
    );
    let archive = root.parent().unwrap().join("archive");
    let inbox = Arc::new(Inbox::open(&root, &archive).unwrap());
    let store = Arc::new(ProcessingStore::open(&root, inbox.root(), &archive).unwrap());
    let generation = store
        .load(id)
        .unwrap()
        .local_moss
        .as_ref()
        .unwrap()
        .generation();
    let effects = Arc::new(Effects::default());
    let engine = Arc::new(ProcessingEngine::new(inbox, store, Arc::clone(&effects)));
    let fixture = Fixture {
        _temp: None,
        engine,
        effects,
        id,
        artifacts: MossArtifacts::open(&root, id, generation).unwrap(),
    };
    interrupted_intent(&fixture, anchors, publish, true).await;
    unreachable!("fixture must exit at its selected cut");
}
