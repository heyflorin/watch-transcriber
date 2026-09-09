//! Read-only parity with the retained coalesced-v2 + JavaScript-v3 long corpus.
use std::{fs, path::Path};

use echowall_local_moss_protocol::speakers::{self, TimingSegment, Window};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[path = "support/public_raw_files.rs"]
mod public_raw_files;
use public_raw_files::read_bounded;

const OUTPUT: &str = "outputs/moss-quiet12-coalesced-v2";

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_json(root: &Path, relative: &str) -> Result<(Value, Vec<u8>), &'static str> {
    let bytes = read_bounded(root, relative, 32 * 1024 * 1024)?;
    let value = serde_json::from_slice(&bytes).map_err(|_| "fixture_invalid")?;
    Ok((value, bytes))
}

fn timing_only(value: &Value) -> Result<Vec<TimingSegment>, &'static str> {
    if value["schema_version"] != 1 {
        return Err("fixture_invalid");
    }
    value["segments"]
        .as_array()
        .ok_or("fixture_invalid")?
        .iter()
        .map(|segment| {
            let speaker = segment["speaker"].as_str().ok_or("fixture_invalid")?;
            Ok(TimingSegment {
                start_ms: segment["start_ms"].as_u64().ok_or("fixture_invalid")?,
                end_ms: segment["end_ms"].as_u64().ok_or("fixture_invalid")?,
                speaker: (speaker != "local_unknown").then(|| speaker.to_owned()),
            })
        })
        .collect()
}

#[test]
#[ignore = "read-only timing-only parity on5588 retained public assignments; requires confirmation"]
fn retained_coalesced_public_assignments_match_javascript_v3() -> Result<(), &'static str> {
    if std::env::var("ECHOWALL_MOSS_SPEAKER_MAPPING_REPLAY_CONFIRM").as_deref()
        != Ok("public-speaker-mapping-replay-authorized")
    {
        return Err("confirmation_required");
    }
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("repository_missing")?;
    let root = fs::canonicalize(repository.join("local-eval/matrix"))
        .map_err(|_| "fixture_root_missing")?;
    let (manifest, _) = read_json(
        &root,
        "manifest-moss-quiet12-coalesced-v2-speakerkit-v3-long4.json",
    )?;
    let cases = manifest["cases"].as_array().ok_or("manifest_invalid")?;
    if cases.len() != 4 {
        return Err("manifest_invalid");
    }
    let (mut segments, mut windows_total, mut unknown, mut search_nodes, mut work_units) =
        (0, 0, 0, 0, 0);
    for (index, case) in cases.iter().enumerate() {
        let id = format!("long_form_{:02}", index + 1);
        if case["case_id"] != id || case["local"] != format!("{OUTPUT}/mapped-{id}.json") {
            return Err("case_scope_invalid");
        }
        let (source, source_bytes) = read_json(&root, &format!("{OUTPUT}/{id}.json"))?;
        let (expected, _) = read_json(&root, &format!("{OUTPUT}/mapped-{id}.json"))?;
        let (retained, _) = read_json(&root, &format!("{OUTPUT}/mapped-{id}.mapping.json"))?;
        let (provenance, provenance_bytes) =
            read_json(&root, &format!("{OUTPUT}/{id}.provenance.json"))?;
        let (anchors, anchor_bytes) = read_json(
            &root,
            &format!("outputs/speakerkit-community1-diar-only/{id}.json"),
        )?;
        for (name, relative, bytes) in [
            (
                "coalesced_moss",
                format!("{OUTPUT}/{id}.json"),
                &source_bytes,
            ),
            (
                "coalesced_provenance",
                format!("{OUTPUT}/{id}.provenance.json"),
                &provenance_bytes,
            ),
            (
                "anchors",
                format!("outputs/speakerkit-community1-diar-only/{id}.json"),
                &anchor_bytes,
            ),
        ] {
            if retained["inputs"][name]["relative_path"] != relative
                || retained["inputs"][name]["sha256"] != digest(bytes)
            {
                return Err("fixture_identity_mismatch");
            }
        }
        if retained["policy"] != speakers::POLICY
            || retained["status"] != "mapped"
            || provenance["windows"] != 8
        {
            return Err("fixture_policy_mismatch");
        }
        let windows: Vec<Window> = provenance["receipts"]
            .as_array()
            .ok_or("fixture_invalid")?
            .iter()
            .map(|receipt| {
                Ok(Window {
                    index: usize::try_from(receipt["index"].as_u64().ok_or("fixture_invalid")?)
                        .map_err(|_| "fixture_invalid")?,
                    start_ms: receipt["start_ms"].as_u64().ok_or("fixture_invalid")?,
                    end_ms: receipt["end_ms"].as_u64().ok_or("fixture_invalid")?,
                })
            })
            .collect::<Result<_, &'static str>>()?;
        let result = speakers::reconcile(
            &timing_only(&source)?,
            &timing_only(&anchors)?,
            &windows,
            case["duration_ms"].as_u64().ok_or("fixture_invalid")?,
        )
        .map_err(|error| error.code())?;
        let mut reconstructed = source.clone();
        let values = reconstructed["segments"]
            .as_array_mut()
            .ok_or("fixture_invalid")?;
        if values.len() != result.assignments.len() {
            return Err("assignment_count_mismatch");
        }
        for (value, assignment) in values.iter_mut().zip(&result.assignments) {
            value["speaker"] = json!(assignment.as_deref().unwrap_or("local_unknown"));
        }
        if reconstructed != expected {
            return Err("assignment_or_non_speaker_content_mismatch");
        }
        let expected_windows = retained["assignments"]
            .as_array()
            .ok_or("fixture_invalid")?;
        if result.windows.len() != expected_windows.len() {
            return Err("window_count_mismatch");
        }
        for (window, expected) in result.windows.iter().zip(expected_windows) {
            let actual = serde_json::to_value(window).map_err(|_| "evidence_invalid")?;
            for key in [
                "window_index",
                "local_slot_order",
                "conflict_edges",
                "unknown_allowed",
                "columns",
                "total_support_ms",
                "components",
                "search_nodes",
            ] {
                if actual[key] != expected[key] {
                    return Err("window_evidence_mismatch");
                }
            }
            for support in &window.support {
                let expected = retained["support"]
                    .as_array()
                    .ok_or("fixture_invalid")?
                    .iter()
                    .find(|row| {
                        row["window_index"] == window.window_index
                            && row["local_speaker"] == support.local_speaker
                    })
                    .ok_or("support_missing")?;
                let actual = serde_json::to_value(support).map_err(|_| "evidence_invalid")?;
                for key in [
                    "support_ms",
                    "total_overlap_ms",
                    "local_duration_ms",
                    "support_by_global",
                    "unknown_allowed",
                ] {
                    if actual[key] != expected[key] {
                        return Err("support_evidence_mismatch");
                    }
                }
            }
            search_nodes += window.search_nodes;
        }
        segments += result.assignments.len();
        windows_total += result.windows.len();
        unknown += result.output_unknown_segments;
        work_units += result.work_units;
    }
    if segments != 5588 || windows_total != 32 || unknown != 1 {
        return Err("aggregate_mismatch");
    }
    println!(
        "{}",
        json!({"state":"pass","cases":4,"windows":windows_total,"assignments":segments,"unknown_segments":unknown,
        "search_nodes":search_nodes,"work_units":work_units,"assignment_support_graph_and_search_parity":true,
        "non_speaker_content_unchanged":true,"read_only":true,"inference_run":false})
    );
    Ok(())
}
