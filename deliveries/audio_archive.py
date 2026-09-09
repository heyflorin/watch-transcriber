"""Audio archive delivery — AI-titled copy of the original recording.

Copies the source .m4a next to the note that local_archive writes, sharing
the recording_stem naming convention:

    data/YYYY-MM-DD/
      HHMMSS-<title-slug>.md    # local_archive
      HHMMSS-<title-slug>.m4a   # this delivery

The Voice Memos original is never touched (renaming it in place would break
CloudRecordings.db references and iCloud sync). Reprocess-safe: stale copies
from a run that produced a different AI title are removed (the HHMMSS prefix
is the deterministic key — the whole archive assumes at most one recording
per wall-clock second, which a single Watch can't violate).
"""

import hashlib
import os
import shutil
from pathlib import Path

from . import (
    archive_note_dt,
    archive_root,
    assert_archive_destination_owner,
    prior_owned_artifact,
    recording_stem,
)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def destination_path(note: dict) -> Path:
    """Return the canonical local archive path for this recording."""

    src = Path(note["audio_path"])
    dt = archive_note_dt(note)
    return archive_root() / dt.strftime("%Y-%m-%d") / f"{recording_stem(note)}{src.suffix}"


def verify_destination(
    note: dict, *, expected_sha256: str, expected_size_bytes: int | None = None
) -> dict[str, object]:
    """Independently verify and describe the durable local audio copy.

    The returned locator is stable and archive-relative; it is never a signed
    URL.  Callers may persist this proof before deleting a temporary provider
    object.
    """

    root = archive_root()
    dest = destination_path(note)
    try:
        resolved = dest.resolve(strict=True)
        resolved.relative_to(root)
        actual_size = resolved.stat().st_size
        actual_sha256 = _sha256(resolved)
    except (OSError, ValueError):
        raise OSError("archived audio could not be verified") from None
    if (
        (expected_size_bytes is not None and actual_size != expected_size_bytes)
        or actual_sha256 != expected_sha256
    ):
        raise OSError("archived audio failed integrity verification")
    return {
        "backend": "local_archive",
        "locator": resolved.relative_to(root).as_posix(),
        "version_id": f"sha256:{actual_sha256}",
        "sha256": actual_sha256,
        "size_bytes": actual_size,
    }


def deliver(note: dict) -> bool:
    src = Path(note["audio_path"])
    if not src.exists():
        print("[delivery:audio_archive] source missing")
        return False

    dt = archive_note_dt(note)
    dest = destination_path(note)
    date_dir = dest.parent
    date_dir.mkdir(parents=True, exist_ok=True)
    owner = assert_archive_destination_owner(note, dest, "audio")
    prior = prior_owned_artifact(note, "audio")

    # Clear tmp orphans from any crashed run for this recording, whatever
    # title it had then (they'd otherwise never match dest or the stale glob).
    exact_tmp = dest.with_suffix(dest.suffix + ".tmp")
    exact_tmp.unlink(missing_ok=True)
    if note.get("recording_id") is None:
        for orphan in date_dir.glob(f"{dt.strftime('%H%M%S')}-*{src.suffix}.tmp"):
            orphan.unlink()

    source_sha256 = _sha256(src)
    if (
        dest.exists()
        and note.get("recording_id") is not None
        and owner is None
        and (
            dest.stat().st_size != src.stat().st_size
            or _sha256(dest) != source_sha256
        )
    ):
        raise OSError("unowned archive audio does not match this recording")
    destination_matches = (
        dest.exists()
        and dest.stat().st_size == src.stat().st_size
        and _sha256(dest) == source_sha256
    )
    if not destination_matches:
        # Copy via tmp + rename so concurrent readers of data/ (cloud backup,
        # Obsidian sync) never see a half-written recording.
        tmp = dest.with_suffix(dest.suffix + ".tmp")
        shutil.copy2(src, tmp)
        if tmp.stat().st_size != src.stat().st_size or _sha256(tmp) != source_sha256:
            tmp.unlink(missing_ok=True)
            raise OSError("archived audio failed integrity verification")
        os.replace(tmp, dest)
        print("[delivery:audio_archive] copied recording audio")
    else:
        print("[delivery:audio_archive] recording audio is up-to-date")

    # Drop stale copies from a reprocess that produced a different title —
    # only after the fresh copy has landed, so a crash never leaves the
    # recording with no archived audio at all.
    if prior is not None and prior != dest:
        prior.unlink(missing_ok=True)
    elif note.get("recording_id") is None:
        for old in date_dir.glob(f"{dt.strftime('%H%M%S')}-*{src.suffix}"):
            if old != dest:
                old.unlink()
    return True
