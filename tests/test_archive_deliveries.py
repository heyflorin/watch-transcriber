"""Tests for the shared archive naming helpers and the audio_archive /
manifest deliveries. All filesystem work happens in tmp_path via
LOCAL_ARCHIVE_DIR; the Voice Memos library is never touched."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import subprocess  # noqa: E402

from deliveries import archive_mutation_lock, recording_stem, slug  # noqa: E402
from deliveries import (  # noqa: E402
    archive_git,
    audio_archive,
    local_archive,
    manifest,
    r2_backup,
    viewer,
)


def _note(tmp_path, title="2026-07-26 01:34 银发大基建中控平台合作", audio=None):
    return {
        "title": title,
        "timestamp": "2026-07-26T01:34:21",
        "audio_path": str(audio or tmp_path / "20260726 013421-44BB9B24.m4a"),
        "category": "工作商务",
    }


@pytest.fixture
def archive(tmp_path, monkeypatch):
    root = tmp_path / "data"
    root.mkdir()
    monkeypatch.setenv("LOCAL_ARCHIVE_DIR", str(root))
    return root


def test_slug_idempotent_and_fallback():
    assert slug("银发大基建 中控平台/合作?") == slug(slug("银发大基建 中控平台/合作?"))
    assert slug("???") == "note"
    assert len(slug("x" * 200)) == 60


@pytest.mark.skipif(sys.platform == "win32", reason="legacy watcher ships on macOS")
def test_archive_mutation_lock_matches_embedded_publisher_protocol(archive):
    import fcntl

    path = archive.parent / ".echowall-runtime" / "archive-publisher.lock"
    with archive_mutation_lock():
        with path.open("a+b") as contender:
            with pytest.raises(BlockingIOError):
                fcntl.flock(contender.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    with path.open("a+b") as contender:
        fcntl.flock(contender.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        fcntl.flock(contender.fileno(), fcntl.LOCK_UN)


def test_recording_stem_strips_leading_timestamp(archive, tmp_path):
    stem = recording_stem(_note(tmp_path))
    assert stem == "013421-银发大基建中控平台合作"
    # A title that is ONLY the timestamp falls back to slugging the full title.
    stem = recording_stem(_note(tmp_path, title="2026-07-26 01:34 Voice Note"))
    assert stem == "013421-Voice-Note"


def test_audio_archive_copies_and_skips(archive, tmp_path, capsys):
    src = tmp_path / "20260726 013421-44BB9B24.m4a"
    src.write_bytes(b"AUDio" * 100)
    note = _note(tmp_path, audio=src)

    assert audio_archive.deliver(note)
    dest = archive / "2026-07-26" / "013421-银发大基建中控平台合作.m4a"
    assert dest.read_bytes() == src.read_bytes()

    # Second run: size matches -> no re-copy.
    assert audio_archive.deliver(note)
    assert "up-to-date" in capsys.readouterr().out

    # Truncated copy (crashed run) -> size mismatch -> re-copied.
    dest.write_bytes(b"AUD")
    assert audio_archive.deliver(note)
    assert dest.read_bytes() == src.read_bytes()

    # A same-sized corrupt file must not be mistaken for a verified backup.
    dest.write_bytes(b"x" * len(src.read_bytes()))
    assert audio_archive.deliver(note)
    assert dest.read_bytes() == src.read_bytes()


def test_audio_archive_replaces_stale_title_copy(archive, tmp_path):
    src = tmp_path / "20260726 013421-44BB9B24.m4a"
    src.write_bytes(b"x" * 64)
    (archive / "2026-07-26").mkdir()
    stale = archive / "2026-07-26" / "013421-旧标题.m4a"
    stale.write_bytes(b"x" * 64)

    assert audio_archive.deliver(_note(tmp_path, audio=src))
    assert not stale.exists()
    assert (archive / "2026-07-26" / "013421-银发大基建中控平台合作.m4a").exists()


def test_audio_archive_missing_source_returns_false(archive, tmp_path):
    assert audio_archive.deliver(_note(tmp_path)) is False


def test_audio_archive_returns_independently_verified_stable_locator(
    archive, tmp_path
):
    import hashlib

    src = tmp_path / "fixture.m4a"
    src.write_bytes(b"generated-audio-fixture")
    note = _note(tmp_path, audio=src)
    assert audio_archive.deliver(note)

    proof = audio_archive.verify_destination(
        note,
        expected_sha256=hashlib.sha256(src.read_bytes()).hexdigest(),
        expected_size_bytes=src.stat().st_size,
    )
    assert proof == {
        "backend": "local_archive",
        "locator": "2026-07-26/013421-银发大基建中控平台合作.m4a",
        "version_id": f"sha256:{hashlib.sha256(src.read_bytes()).hexdigest()}",
        "sha256": hashlib.sha256(src.read_bytes()).hexdigest(),
        "size_bytes": src.stat().st_size,
    }

    archived = archive / proof["locator"]
    archived.write_bytes(b"x" * src.stat().st_size)
    with pytest.raises(OSError, match="integrity"):
        audio_archive.verify_destination(
            note,
            expected_sha256=hashlib.sha256(src.read_bytes()).hexdigest(),
            expected_size_bytes=src.stat().st_size,
        )


def test_clean_category_whitelist():
    assert manifest.clean_category("工作商务") == "工作商务"
    assert manifest.clean_category(" 亲密关系 ") == "亲密关系"
    for bad in ("Work", "", None, "工作", 3):
        assert manifest.clean_category(bad) == "其他"


def test_manifest_deliver_and_views(archive, tmp_path):
    src = tmp_path / "20260726 013421-44BB9B24.m4a"
    src.write_bytes(b"x" * 64)
    note = _note(tmp_path, audio=src)
    day = archive / "2026-07-26"
    day.mkdir()
    (day / "013421-银发大基建中控平台合作.md").write_text("# t", encoding="utf-8")
    audio_archive.deliver(note)

    assert manifest.deliver(note)
    m = manifest.load()
    entry = m["2026-07-26 013421"]
    assert entry["original"] == "20260726 013421-44BB9B24.m4a"
    assert entry["category"] == "工作商务"
    assert entry["note"] == "2026-07-26/013421-银发大基建中控平台合作.md"
    assert entry["audio"] == "2026-07-26/013421-银发大基建中控平台合作.m4a"

    links = list((archive / "by-topic" / "工作商务").iterdir())
    assert sorted(p.suffix for p in links) == [".m4a", ".md"]
    for link in links:
        assert link.is_symlink() and link.resolve().exists()

    # Recategorize -> views move, old category dir disappears.
    note["category"] = "生活日常"
    manifest.deliver(note)
    assert not (archive / "by-topic" / "工作商务").exists()
    assert (archive / "by-topic" / "生活日常").is_dir()


def test_viewer_build_embeds_entries(archive, tmp_path):
    src = tmp_path / "20260726 013421-44BB9B24.m4a"
    src.write_bytes(b"x" * 64)
    note = _note(tmp_path, audio=src)
    day = archive / "2026-07-26"
    day.mkdir()
    (day / "013421-银发大基建中控平台合作.md").write_text(
        "# 2026-07-26 01:34 银发大基建中控平台合作\n\n**File:** `20260726 013421-44BB9B24.m4a`\n\n"
        "## Summary\n\nEnglish summary.\n\n中文摘要。\n\n"
        "## Key Points\n\n- point one\n- 要点一\n\n"
        "## Action Items\n\n- [ ] follow up\n\n"
        "---\n\n## Transcript\n\n```\n[00:00:01 - 00:00:05] SPEAKER_1: 你好\n```\n",
        encoding="utf-8",
    )
    audio_archive.deliver(note)
    manifest.deliver(note)

    dest = viewer.build()
    html = dest.read_text(encoding="utf-8")
    assert dest == archive / "index.html"
    assert "__PAYLOAD__" not in html
    payload = html.split('type="application/json">', 1)[1].split("</script>", 1)[0]
    import json

    data = json.loads(payload.replace("<\\/", "</"))
    (entry,) = data["entries"]
    assert entry["ai_title"] == "银发大基建中控平台合作"
    assert entry["summary_zh"] == "中文摘要。"
    assert entry["key_points"] == ["要点一"]
    assert entry["todos"] == ["follow up"]
    assert entry["duration"] == 5
    assert entry["audio"].endswith(".m4a")


def test_archive_git_noop_without_repo(archive, tmp_path, capsys):
    assert archive_git.deliver(_note(tmp_path)) is True
    assert "not a git repo" in capsys.readouterr().out


def test_archive_git_bootstraps_gitignore_and_commits(archive, tmp_path):
    src = tmp_path / "20260726 013421-44BB9B24.m4a"
    src.write_bytes(b"x" * 64)
    note = _note(tmp_path, audio=src)
    day = archive / "2026-07-26"
    day.mkdir()
    (day / "013421-银发大基建中控平台合作.md").write_text("# t", encoding="utf-8")
    audio_archive.deliver(note)

    def git(*args):
        return subprocess.run(
            ["git", "-C", str(archive), "-c", "core.quotepath=false", *args],
            capture_output=True,
            text=True,
        )

    git("init", "-q")
    git("config", "user.email", "t@t")
    git("config", "user.name", "t")
    assert archive_git.deliver(note) is True
    tracked = git("ls-files").stdout
    assert ".gitignore" in tracked
    assert "013421-银发大基建中控平台合作.md" in tracked
    assert ".m4a" not in tracked  # audio must NEVER enter git history
    # Second run with no changes: no new commit, still True.
    assert archive_git.deliver(note) is True
    assert git("rev-list", "--count", "HEAD").stdout.strip() == "1"


def test_required_delivery_logs_do_not_expose_private_title_path_or_subprocess_error(
    archive, tmp_path, monkeypatch, capsys
):
    sentinel = "PRIVATE-SENTINEL-DO-NOT-LOG"
    src = tmp_path / f"{sentinel}.m4a"
    src.write_bytes(b"generated-audio")
    note = {
        **_note(tmp_path, title=f"2026-07-26 01:34 {sentinel}", audio=src),
        "markdown": f"# {sentinel}\n",
        "recording_id": "018f92d8-6ad4-7dc1-8e28-8b020d2942cb",
        "archive_key": "2026-07-26 013421",
        "publish_generation": 1,
    }
    assert local_archive.deliver(note)
    assert audio_archive.deliver(note)
    assert manifest.deliver(note)
    assert viewer.deliver(note)
    subprocess.run(["git", "-C", str(archive), "init", "-q"], check=True)
    subprocess.run(
        ["git", "-C", str(archive), "config", "user.email", "fixture@example.test"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(archive), "config", "user.name", "Fixture"], check=True
    )
    assert archive_git.deliver(note)

    def failed_run(*_args, **_kwargs):
        return subprocess.CompletedProcess([], 1, stdout="", stderr=sentinel)

    monkeypatch.setattr(r2_backup, "wrangler_bin", lambda: "/fake/wrangler")
    monkeypatch.setattr(r2_backup.subprocess, "run", failed_run)
    assert r2_backup.deliver(note) is False

    output = capsys.readouterr().out
    assert sentinel not in output
    assert str(tmp_path) not in output


def test_manifest_preserves_user_speakers_on_reprocess(archive, tmp_path):
    note = _note(tmp_path)
    day = archive / "2026-07-26"
    day.mkdir()
    (day / "013421-银发大基建中控平台合作.md").write_text("# t", encoding="utf-8")
    manifest.deliver(note)
    m = manifest.load()
    m["2026-07-26 013421"]["speakers"] = {"SPEAKER_1": "AX"}
    manifest.save(m)

    manifest.deliver(note)  # reprocess must not wipe user-authored tags
    assert manifest.load()["2026-07-26 013421"]["speakers"] == {"SPEAKER_1": "AX"}


def test_apply_speakers_rewrites_and_reverts(archive, tmp_path):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts" / "ops"))
    from apply_speakers import apply_keys

    note = _note(tmp_path)
    day = archive / "2026-07-26"
    day.mkdir()
    md = day / "013421-银发大基建中控平台合作.md"
    md.write_text(
        "# t\n\n## Transcript\n\n```\n"
        "[00:00:01 - 00:00:02] SPEAKER_1: 你好\n"
        "[00:00:03 - 00:00:04] SPEAKER_2: 嗯\n```\n",
        encoding="utf-8",
    )
    manifest.deliver(note)
    m = manifest.load()
    key = "2026-07-26 013421"
    m[key]["speakers"] = {"SPEAKER_1": "妍子"}
    manifest.save(m)

    assert apply_keys([key], rebuild_viewer=False) == 1
    text = md.read_text(encoding="utf-8")
    assert "] 妍子: 你好" in text and "SPEAKER_2: 嗯" in text
    assert manifest.load()[key]["speakers_applied"] == {"SPEAKER_1": "妍子"}
    # Idempotent second run, then rename, then clear back to the raw slot.
    assert apply_keys([key], rebuild_viewer=False) == 0
    m = manifest.load()
    m[key]["speakers"] = {"SPEAKER_1": "AX"}
    manifest.save(m)
    apply_keys([key], rebuild_viewer=False)
    assert "] AX: 你好" in md.read_text(encoding="utf-8")
    m = manifest.load()
    m[key]["speakers"] = {}
    manifest.save(m)
    apply_keys([key], rebuild_viewer=False)
    assert "] SPEAKER_1: 你好" in md.read_text(encoding="utf-8")
    assert "speakers_applied" not in manifest.load()[key]


def test_manifest_persists_recording_identity_and_rejects_stale_or_conflicting_publish(
    archive, tmp_path
):
    note = _note(tmp_path)
    note["recording_id"] = "018f92d8-6ad4-7dc1-8e28-8b020d2942cb"
    note["publish_generation"] = 7
    note["source"] = "Microsoft Teams"
    note["source_kind"] = "desktop_meeting"
    day = archive / "2026-07-26"
    day.mkdir()
    (day / "013421-银发大基建中控平台合作.md").write_text("# t", encoding="utf-8")

    assert manifest.deliver(note)
    entry = manifest.load()["2026-07-26 013421"]
    assert entry["recording_id"] == note["recording_id"]
    assert entry["publish_generation"] == 7
    assert entry["source"] == "Microsoft Teams"
    assert entry["source_kind"] == "desktop_meeting"
    app_owned = manifest.load()
    app_owned_entry = app_owned["2026-07-26 013421"]
    app_owned_entry.update(
        {
            "r2_key": (
                "2026-07-26/013421-recording-aaaaaaaaaaaaaaaa-g7-"
                f"{note['recording_id']}.m4a"
            ),
            "r2_generation": 7,
            "audio_sha256": "a" * 64,
            "audio_size_bytes": 1234,
        }
    )
    manifest.save(app_owned)

    legacy_reprocess = _note(tmp_path)
    assert manifest.deliver(legacy_reprocess)
    entry = manifest.load()["2026-07-26 013421"]
    assert entry["recording_id"] == note["recording_id"]
    assert entry["publish_generation"] == 7
    assert entry["r2_generation"] == 7
    assert entry["r2_key"] == app_owned_entry["r2_key"]
    assert entry["audio_sha256"] == "a" * 64
    assert entry["audio_size_bytes"] == 1234

    conflicting = {
        **note,
        "recording_id": "different-recording",
        "publish_generation": 8,
    }
    with pytest.raises(ValueError, match="recording_id"):
        manifest.deliver(conflicting)

    stale = {**note, "publish_generation": 6}
    with pytest.raises(ValueError, match="publish_generation"):
        manifest.deliver(stale)


def test_local_manifest_and_viewer_proofs_read_persisted_outputs(archive, tmp_path):
    src = tmp_path / "fixture.m4a"
    src.write_bytes(b"synthetic-audio")
    note = {
        **_note(tmp_path, audio=src),
        "markdown": "# generated\n",
        "recording_id": "018f92d8-6ad4-7dc1-8e28-8b020d2942cb",
        "publish_generation": 7,
        "source": "Imported fixture.m4a",
        "source_kind": "file_import",
    }

    assert local_archive.deliver(note)
    assert audio_archive.deliver(note)
    assert manifest.deliver(note)
    assert viewer.deliver(note)
    assert local_archive.verify_destination(note)["locator"].endswith(".md")
    assert manifest.verify_destination(note)["recording_key"] == "2026-07-26 013421"
    assert viewer.verify_destination(note)["recording_key"] == "2026-07-26 013421"

    manifest_data = manifest.load()
    manifest_data["2026-07-26 013421"]["recording_id"] = "wrong"
    manifest.save(manifest_data)
    with pytest.raises(OSError, match="identity"):
        manifest.verify_destination(note)


def test_app_archive_keys_preserve_two_recordings_captured_same_second(
    archive, tmp_path
):
    first_audio = tmp_path / "first.m4a"
    second_audio = tmp_path / "second.m4a"
    first_audio.write_bytes(b"first-generated-audio")
    second_audio.write_bytes(b"second-generated-audio")
    common = {
        "timestamp": "2026-07-26T01:34:21-07:00",
        "category": "工作商务",
        "publish_generation": 1,
        "source": "Imported fixture",
        "source_kind": "file_import",
    }
    first = {
        **common,
        "title": "2026-07-26 01:34 First",
        "audio_path": str(first_audio),
        "markdown": "# first\n",
        "recording_id": "018f92d8-6ad4-7dc1-8e28-8b020d2942cb",
        "archive_key": "2026-07-26 013421",
    }
    second = {
        **common,
        "title": "2026-07-26 01:34 Second",
        "audio_path": str(second_audio),
        "markdown": "# second\n",
        "recording_id": "028f92d8-6ad4-7dc1-8e28-8b020d2942cb",
        "archive_key": "2026-07-26 013422",
        "publish_generation": 2,
    }

    for note in (first, second):
        assert local_archive.deliver(note)
        assert audio_archive.deliver(note)
        assert manifest.deliver(note)

    entries = manifest.load()
    assert set(entries) == {"2026-07-26 013421", "2026-07-26 013422"}
    first_entry = entries["2026-07-26 013421"]
    second_entry = entries["2026-07-26 013422"]
    assert (archive / first_entry["note"]).read_text() == "# first\n"
    assert (archive / first_entry["audio"]).read_bytes() == first_audio.read_bytes()
    assert (archive / second_entry["note"]).read_text() == "# second\n"
    assert (archive / second_entry["audio"]).read_bytes() == second_audio.read_bytes()
    assert first_entry["captured_at"] == second_entry["captured_at"]


def test_app_cannot_overwrite_another_recordings_archive_key(archive, tmp_path):
    first_audio = tmp_path / "first.m4a"
    second_audio = tmp_path / "second.m4a"
    first_audio.write_bytes(b"first-generated-audio")
    second_audio.write_bytes(b"second-generated-audio")
    first = {
        **_note(tmp_path, title="2026-07-26 01:34 First", audio=first_audio),
        "markdown": "# first\n",
        "recording_id": "018f92d8-6ad4-7dc1-8e28-8b020d2942cb",
        "archive_key": "2026-07-26 013421",
        "publish_generation": 1,
    }
    assert local_archive.deliver(first)
    assert audio_archive.deliver(first)
    assert manifest.deliver(first)
    first_note = local_archive.destination_path(first)
    first_copy = audio_archive.destination_path(first)

    collision = {
        **_note(tmp_path, title="2026-07-26 01:34 Second", audio=second_audio),
        "markdown": "# second\n",
        "recording_id": "028f92d8-6ad4-7dc1-8e28-8b020d2942cb",
        "archive_key": "2026-07-26 013421",
        "publish_generation": 2,
    }
    with pytest.raises(OSError, match="another recording"):
        local_archive.deliver(collision)
    with pytest.raises(OSError, match="another recording"):
        audio_archive.deliver(collision)
    assert first_note.read_text() == "# first\n"
    assert first_copy.read_bytes() == first_audio.read_bytes()


def test_uncheckpointed_app_artifacts_replay_only_when_bytes_match(
    archive, tmp_path
):
    source = tmp_path / "source.m4a"
    source.write_bytes(b"generated-audio")
    note = {
        **_note(tmp_path, title="2026-07-26 01:34 Replay", audio=source),
        "markdown": "# replay\n",
        "recording_id": "018f92d8-6ad4-7dc1-8e28-8b020d2942cb",
        "archive_key": "2026-07-26 013421",
        "publish_generation": 1,
    }

    # Simulate target success followed by process death before its DB/manifest
    # checkpoint. Exact bytes are safe to recognize on replay.
    assert local_archive.deliver(note)
    assert audio_archive.deliver(note)
    assert not manifest.manifest_path().exists()
    assert local_archive.deliver(note)
    assert audio_archive.deliver(note)

    local_archive.destination_path(note).write_text("# foreign\n")
    with pytest.raises(OSError, match="does not match"):
        local_archive.deliver(note)
    local_archive.destination_path(note).write_text(note["markdown"])
    audio_archive.destination_path(note).write_bytes(b"foreign-content")
    with pytest.raises(OSError, match="does not match"):
        audio_archive.deliver(note)


def test_delete_recording_removes_everything(archive, tmp_path):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts" / "ops"))
    from delete_recording import delete_recording

    src = tmp_path / "20260726 013421-44BB9B24.m4a"
    src.write_bytes(b"x" * 64)
    note = _note(tmp_path, audio=src)
    day = archive / "2026-07-26"
    day.mkdir()
    (day / "013421-银发大基建中控平台合作.md").write_text("# t", encoding="utf-8")
    audio_archive.deliver(note)
    manifest.deliver(note)
    att = day / "013421-attachments"
    att.mkdir()
    (att / "x.md").write_text("hi", encoding="utf-8")
    m = manifest.load()
    m["2026-07-26 013421"]["attachments"] = ["2026-07-26/013421-attachments/x.md"]
    manifest.save(m)

    r = delete_recording("2026-07-26 013421", dry_run=True, keep_r2=True)
    assert r["ok"] and len(r["would_delete"]) >= 3
    r = delete_recording("2026-07-26 013421", keep_r2=True)
    assert r["ok"]
    assert not day.exists()  # last recording of the day -> dir retired
    assert "2026-07-26 013421" not in manifest.load()
    assert delete_recording("2026-07-26 013421", keep_r2=True)["ok"] is False


def test_manifest_missing_audio_is_null(archive, tmp_path):
    note = _note(tmp_path)  # audio_path doesn't exist, no copy made
    day = archive / "2026-07-26"
    day.mkdir()
    (day / "013421-银发大基建中控平台合作.md").write_text("# t", encoding="utf-8")
    manifest.deliver(note)
    assert manifest.load()["2026-07-26 013421"]["audio"] is None
