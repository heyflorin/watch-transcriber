from datetime import datetime
from pathlib import Path

import pytest

from transcribe import format_note


RESULT = {
    "title": "Synthetic meeting",
    "category": "工作商务",
    "summary_en": "A fabricated summary.",
    "summary_zh": "一段合成摘要。",
    "key_points_en": ["One point"],
    "key_points_zh": ["一个要点"],
    "action_items": ["Follow up"],
    "transcript": "[00:00:00 - 00:00:01] SPEAKER_1: synthetic",
}


def test_format_note_legacy_output_is_unchanged():
    audio = Path("20260726 013421-44BB9B24.m4a")

    note = format_note(audio, RESULT)

    assert note == {
        "title": "2026-07-26 01:34 Synthetic meeting",
        "transcript": "[00:00:00 - 00:00:01] SPEAKER_1: synthetic",
        "summary": "A fabricated summary.\n\n一段合成摘要。",
        "todos": ["Follow up"],
        "category": "工作商务",
        "audio_path": str(audio),
        "timestamp": "2026-07-26T01:34:21",
        "markdown": (
            "# 2026-07-26 01:34 Synthetic meeting\n\n"
            "**Recorded:** 2026-07-26T01:34:21\n"
            "**Source:** Apple Watch Voice Memo\n"
            "**File:** `20260726 013421-44BB9B24.m4a`\n\n"
            "## Summary\n\nA fabricated summary.\n\n一段合成摘要。\n\n"
            "## Key Points\n\n- One point\n\n- 一个要点\n\n"
            "## Action Items\n\n- [ ] Follow up\n\n"
            "---\n\n## Transcript\n\n```\n"
            "[00:00:00 - 00:00:01] SPEAKER_1: synthetic\n```\n"
        ),
        "html": (
            "<h1>2026-07-26 01:34 Synthetic meeting</h1>\n"
            "<p><b>Recorded:</b> 2026-07-26T01:34:21<br>"
            "<b>Source:</b> Apple Watch Voice Memo<br>"
            "<b>File:</b> 20260726 013421-44BB9B24.m4a</p>\n"
            "<h2>Summary</h2>\n<p>A fabricated summary.</p>\n"
            "<p>一段合成摘要。</p>\n<h2>Key Points</h2><ul>\n"
            "<li>One point</li>\n<li>一个要点</li>\n</ul>\n"
            "<h2>Action Items</h2><ul>\n<li>Follow up</li>\n</ul>\n"
            "<h2>Transcript</h2>\n"
            '<div style="font-family:ui-monospace,Menlo,monospace;font-size:0.9em">'
            "[00:00:00 - 00:00:01] SPEAKER_1: synthetic</div>"
        ),
    }


@pytest.mark.parametrize(
    ("captured_at", "source_label", "source_kind", "expected_source"),
    [
        (
            "2026-09-02T08:15:30-07:00",
            "Imported interview.wav",
            "file_import",
            "Imported interview.wav",
        ),
        (
            "2026-09-02T09:16:31-07:00",
            "iPhone 17 Pro Max",
            "mobile_voice_memo",
            "iPhone 17 Pro Max",
        ),
        (
            "2026-09-02T10:17:32-07:00",
            "Microsoft Teams",
            "desktop_meeting",
            "Microsoft Teams",
        ),
    ],
)
def test_format_note_uses_true_capture_and_source_metadata(
    captured_at, source_label, source_kind, expected_source
):
    note = format_note(
        Path("materialized.m4a"),
        RESULT,
        captured_at=captured_at,
        source_label=source_label,
        source_kind=source_kind,
        recording_id="018f92d8-6ad4-7dc1-8e28-8b020d2942cb",
        publish_generation=7,
    )

    expected_timestamp = datetime.fromisoformat(captured_at).isoformat()
    assert note["timestamp"] == expected_timestamp
    assert note["title"].startswith(
        datetime.fromisoformat(captured_at).strftime("%Y-%m-%d %H:%M ")
    )
    assert note["source"] == expected_source
    assert note["source_kind"] == source_kind
    assert note["recording_id"] == "018f92d8-6ad4-7dc1-8e28-8b020d2942cb"
    assert note["publish_generation"] == 7
    assert f"**Source:** {expected_source}" in note["markdown"]
    assert f"<b>Source:</b> {expected_source}" in note["html"]


def test_format_note_falls_back_to_source_kind_when_label_is_absent():
    note = format_note(
        Path("materialized.m4a"),
        RESULT,
        captured_at="2026-09-02T10:17:32-07:00",
        source_label=None,
        source_kind="desktop_system",
    )

    assert note["source"] == "desktop_system"
