"""Pluggable delivery layer for watch-transcriber.

Each delivery module must implement:
    deliver(note: dict) -> bool

Where note has:
    - title: str (timestamp + AI-generated topic, e.g. "2026-03-29 21:25 项目排期讨论"
      so name-sorted lists order chronologically; falls back to
      "2026-03-29 21:25 Voice Note" when the summarize stage yields no title)
    - transcript: str (raw transcript with timestamps/speakers)
    - summary: str (AI-generated summary, may be empty)
    - todos: list[str] (extracted action items, may be empty)
    - category: str (AI-assigned topic, one of manifest.CATEGORIES)
    - audio_path: str (path to original .m4a file)
    - timestamp: str (ISO format)
    - markdown: str (formatted markdown combining all fields)
"""

import importlib
import json
import os
import re
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path


def safe_filename(title: str) -> str:
    """Whitelist-sanitize a note title for cross-platform filenames.

    Windows / Obsidian sync reject ? * " | < > : (and fullwidth variants are
    confusing); AI-generated titles can contain any of these. Keeps word chars,
    CJK, spaces, parens, dots, hyphens; everything else becomes a hyphen.
    """
    s = re.sub(r"[^\w一-鿿《》「」【】· ().-]", "-", title)
    s = re.sub(r"-{2,}", "-", s)
    s = re.sub(r"\s+", " ", s).strip(" -.")
    return s[:80] or "note"


def slug(s: str) -> str:
    """Archive filename slug. Shared by every per-recording artifact — the .md
    note, the .m4a copy, and the manifest all derive the same stem, so this
    must stay the single implementation or the pairing silently breaks."""
    s = re.sub(r"[^\w一-鿿 -]", "", s)
    s = re.sub(r"\s+", "-", s.strip())
    return s[:60] or "note"


def archive_root() -> Path:
    return Path(os.environ.get("LOCAL_ARCHIVE_DIR", "./data")).expanduser().resolve()


@contextmanager
def archive_mutation_lock():
    """Serialize legacy Python archive writes with the embedded Rust publisher."""

    runtime = archive_root().parent / ".echowall-runtime"
    runtime.mkdir(parents=True, exist_ok=True)
    path = runtime / "archive-publisher.lock"
    with path.open("a+b") as handle:
        if os.name == "nt":
            import msvcrt

            if path.stat().st_size == 0:
                handle.write(b"\0")
                handle.flush()
            handle.seek(0)
            msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK, 1)
            try:
                yield
            finally:
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def parse_note_dt(note: dict) -> datetime:
    ts = note.get("timestamp")
    if ts:
        try:
            return datetime.fromisoformat(ts)
        except ValueError:
            pass
    return datetime.now()


def archive_key(note: dict) -> str:
    """Return the persisted seconds-precision archive slot for a note."""

    value = note.get("archive_key")
    if value is None:
        return parse_note_dt(note).strftime("%Y-%m-%d %H%M%S")
    if not isinstance(value, str):
        raise ValueError("archive_key must be text")
    try:
        parsed = datetime.strptime(value, "%Y-%m-%d %H%M%S")
    except ValueError:
        raise ValueError("archive_key must use YYYY-MM-DD HHMMSS") from None
    if parsed.strftime("%Y-%m-%d %H%M%S") != value:
        raise ValueError("archive_key is not canonical")
    return value


def archive_note_dt(note: dict) -> datetime:
    return datetime.strptime(archive_key(note), "%Y-%m-%d %H%M%S")


def assert_archive_destination_owner(
    note: dict, destination: Path, field: str
) -> str | None:
    """Prevent one recording from overwriting another archive artifact."""

    recording_id = note.get("recording_id")
    if recording_id is None:
        return None
    manifest_path = archive_root() / "manifest.json"
    try:
        entries = (
            json.loads(manifest_path.read_text(encoding="utf-8"))
            if manifest_path.exists()
            else {}
        )
    except (OSError, UnicodeError, json.JSONDecodeError):
        raise OSError("archive ownership metadata is unavailable") from None
    if not isinstance(entries, dict):
        raise OSError("archive ownership metadata is unavailable")
    keyed = entries.get(archive_key(note))
    if isinstance(keyed, dict) and keyed.get("recording_id") not in (
        None,
        recording_id,
    ):
        raise OSError("archive key belongs to another recording")
    if not destination.exists():
        return None
    relative = destination.resolve().relative_to(archive_root()).as_posix()
    owner = next(
        (
            entry.get("recording_id")
            for entry in entries.values()
            if isinstance(entry, dict) and entry.get(field) == relative
        ),
        None,
    )
    if owner not in (None, recording_id):
        raise OSError("archive destination belongs to another recording")
    return owner


def prior_owned_artifact(note: dict, field: str) -> Path | None:
    recording_id = note.get("recording_id")
    if recording_id is None:
        return None
    manifest_path = archive_root() / "manifest.json"
    if not manifest_path.exists():
        return None
    try:
        entries = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raise OSError("archive ownership metadata is unavailable") from None
    if not isinstance(entries, dict):
        raise OSError("archive ownership metadata is unavailable")
    for entry in entries.values():
        if not isinstance(entry, dict) or entry.get("recording_id") != recording_id:
            continue
        relative = entry.get(field)
        if not isinstance(relative, str) or not relative:
            return None
        candidate = (archive_root() / relative).resolve()
        try:
            candidate.relative_to(archive_root())
        except ValueError:
            raise OSError("archive ownership path is invalid") from None
        return candidate
    return None


def recording_stem(note: dict) -> str:
    """Deterministic "HHMMSS-<title-slug>" stem pairing all archive artifacts
    of one recording. The archive filename already carries HHMMSS and the date
    dir carries the day, so the title's leading timestamp is stripped before
    slugging to avoid duplicating the date."""
    dt = archive_note_dt(note)
    display = re.sub(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}\s*", "", note["title"])
    return f"{dt.strftime('%H%M%S')}-{slug(display or note['title'])}"


BUILTIN_DELIVERIES = ["file", "local_archive", "audio_archive", "manifest", "viewer", "archive_git", "r2_backup", "apple_notes", "feishu", "feishu_notify", "obsidian_git", "agent"]


def get_active_deliveries() -> list[str]:
    targets = os.environ.get("DELIVERY_TARGETS", "file")
    return [t.strip() for t in targets.split(",") if t.strip()]


def deliver_all(note: dict) -> dict[str, bool]:
    results = {}
    with archive_mutation_lock():
        for target in get_active_deliveries():
            try:
                mod = importlib.import_module(f"deliveries.{target}")
                results[target] = mod.deliver(note)
            except Exception:
                print(f"[delivery:{target}] failed")
                results[target] = False
    return results


def deliver_target(note: dict, target: str) -> bool:
    """Run one allowlisted delivery for durable per-target checkpointing."""

    if target not in BUILTIN_DELIVERIES:
        raise ValueError("delivery target is not allowlisted")
    with archive_mutation_lock():
        mod = importlib.import_module(f"deliveries.{target}")
        return mod.deliver(note) is True
