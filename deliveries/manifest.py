"""Manifest delivery — the 1:1 note↔audio map plus topic views.

Maintains data/manifest.json, keyed by "YYYY-MM-DD HHMMSS" (recording start,
the same deterministic key the archive filenames carry):

    {
      "2026-07-26 013421": {
        "original": "20260726 013421-44BB9B24.m4a",   # Voice Memos source file
        "title": "2026-07-26 01:34 银发大基建中控平台合作",
        "category": "工作商务",                        # one of CATEGORIES
        "note": "2026-07-26/013421-银发大基建中控平台合作.md",
        "audio": "2026-07-26/013421-银发大基建中控平台合作.m4a"  # null if no copy
      }, ...
    }

After every update it regenerates data/by-topic/<category>/ — relative
symlinks to the real note+audio pairs, so Finder gets a topic-organized view
without duplicating files. by-topic/ is generated output: never hand-edit.

Run this delivery AFTER local_archive and audio_archive in DELIVERY_TARGETS —
it locates their outputs on disk by the shared HHMMSS-stem.
"""

import json
import os
from pathlib import Path

from . import archive_key, archive_note_dt, archive_root, recording_stem, safe_filename

# Fixed taxonomy for the summarize stage's category field. Order matters only
# for prompt display. Keep in sync with nothing — this list IS the source;
# the summarize prompt and all validation derive from it.
CATEGORIES = ["亲密关系", "自我成长", "学习认知", "工作商务", "生活日常", "其他"]
FALLBACK_CATEGORY = "其他"


def clean_category(raw) -> str:
    """Tolerant whitelist validation for a model-emitted category."""
    c = str(raw or "").strip()
    return c if c in CATEGORIES else FALLBACK_CATEGORY


def manifest_path() -> Path:
    return archive_root() / "manifest.json"


def key_for_note(note: dict) -> str:
    return archive_key(note)


def load() -> dict:
    p = manifest_path()
    if p.exists():
        return json.loads(p.read_text(encoding="utf-8"))
    return {}


def save(manifest: dict) -> None:
    p = manifest_path()
    tmp = p.with_suffix(".json.tmp")
    tmp.write_text(
        json.dumps(dict(sorted(manifest.items())), ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    os.replace(tmp, p)


def deliver(note: dict) -> bool:
    dt = archive_note_dt(note)
    key = key_for_note(note)
    date_dir = archive_root() / dt.strftime("%Y-%m-%d")
    stem = recording_stem(note)

    note_file = date_dir / f"{stem}.md"
    audio_file = next(
        (
            p
            for p in sorted(date_dir.glob(f"{stem}.*"))
            if p.suffix not in (".md", ".tmp")
        ),
        None,
    )

    manifest = load()
    prior = manifest.get(key) or {}
    recording_id = note.get("recording_id")
    if recording_id is not None:
        if not isinstance(recording_id, str) or not recording_id.strip():
            raise ValueError("recording_id must be non-empty text")
        recording_id = recording_id.strip()
        if prior.get("recording_id") not in (None, recording_id):
            raise ValueError("recording_id conflicts with the existing archive entry")

    publish_generation = note.get("publish_generation")
    if publish_generation is not None:
        if (
            isinstance(publish_generation, bool)
            or not isinstance(publish_generation, int)
            or publish_generation < 1
        ):
            raise ValueError("publish_generation must be a positive integer")
        prior_generation = prior.get("publish_generation")
        if isinstance(prior_generation, int) and publish_generation < prior_generation:
            raise ValueError(
                "publish_generation is older than the existing archive entry"
            )

    entry = {
        "original": Path(note["audio_path"]).name,
        "title": note["title"],
        "category": clean_category(note.get("category")),
        "note": _rel(note_file) if note_file.exists() else None,
        "audio": _rel(audio_file) if audio_file else None,
        "captured_at": note.get("timestamp"),
    }
    stable_recording_id = recording_id or prior.get("recording_id")
    if stable_recording_id:
        entry["recording_id"] = stable_recording_id
    stable_generation = publish_generation or prior.get("publish_generation")
    if stable_generation:
        entry["publish_generation"] = stable_generation
    # App-owned immutable R2 identity stays attached when the legacy watcher
    # re-renders the same recording. The Python path must never silently turn a
    # verifiable App object into an unowned legacy object.
    if stable_recording_id and prior.get("recording_id") == stable_recording_id:
        for field in (
            "r2_key",
            "r2_generation",
            "audio_sha256",
            "audio_size_bytes",
        ):
            if field in prior:
                entry[field] = prior[field]
    for field in ("source", "source_kind"):
        value = note.get(field) or prior.get(field)
        if value:
            entry[field] = value
    # User-authored fields (desktop app writes them) survive a reprocess.
    for field in ("speakers", "speakers_applied", "attachments"):
        if prior.get(field):
            entry[field] = prior[field]
    manifest[key] = entry
    save(manifest)
    rebuild_views(manifest)
    print("[delivery:manifest] recording entry updated")
    return True


def verify_destination(note: dict) -> dict[str, object]:
    """Parse the persisted manifest and verify this publication identity."""

    path = manifest_path()
    try:
        entry = load()[key_for_note(note)]
    except (OSError, UnicodeError, json.JSONDecodeError, KeyError, TypeError):
        raise OSError("archive manifest could not be verified") from None
    expected = {
        "title": note["title"],
        "category": clean_category(note.get("category")),
        "recording_id": note.get("recording_id"),
        "publish_generation": note.get("publish_generation"),
        "source": note.get("source"),
        "source_kind": note.get("source_kind"),
    }
    if any(value is not None and entry.get(field) != value for field, value in expected.items()):
        raise OSError("archive manifest failed identity verification")
    if not entry.get("note") or not entry.get("audio"):
        raise OSError("archive manifest is missing canonical artifacts")
    return {
        "backend": "local_archive",
        "locator": path.resolve().relative_to(archive_root()).as_posix(),
        "recording_key": key_for_note(note),
    }


def _rel(p: Path) -> str:
    return str(p.relative_to(archive_root()))


def rebuild_views(manifest: dict) -> None:
    """Regenerate data/by-topic/ from scratch — it only ever contains symlinks,
    so a full wipe is safe and keeps renames/recategorizations clean.

    Deliberately full-rebuild, not incremental: the whole tree is ~2 symlinks
    per recording (a few hundred total, milliseconds to recreate), and a wipe
    is the only approach that needs no bookkeeping when a reprocess changes an
    entry's title or category. Revisit only if the archive grows ~10x."""
    views = archive_root() / "by-topic"
    if views.exists():
        for entry in views.rglob("*"):
            if entry.is_symlink() or entry.is_file():
                entry.unlink()
        for d in sorted((d for d in views.rglob("*") if d.is_dir()), reverse=True):
            d.rmdir()
    for key, entry in manifest.items():
        cat_dir = views / (entry.get("category") or FALLBACK_CATEGORY)
        cat_dir.mkdir(parents=True, exist_ok=True)
        for field in ("note", "audio"):
            rel = entry.get(field)
            if not rel:
                continue
            target = archive_root() / rel
            if not target.exists():
                continue
            link = cat_dir / f"{safe_filename(entry['title'])}{target.suffix}"
            if link.exists():
                # Same category + same displayed minute + same AI title:
                # disambiguate with the seconds-precision key.
                link = (
                    cat_dir
                    / f"{safe_filename(entry['title'])}-{key.split()[1]}{target.suffix}"
                )
            if link.exists():
                print("[manifest] duplicate generated view name skipped")
                continue
            link.symlink_to(os.path.relpath(target, cat_dir))
