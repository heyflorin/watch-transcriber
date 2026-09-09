"""R2 backup delivery — off-site copy of each recording's archive audio.

Uploads the AI-titled .m4a that audio_archive wrote to a PRIVATE Cloudflare
R2 bucket, keyed by its archive-relative path (YYYY-MM-DD/HHMMSS-<slug>.m4a).
Auth rides on the local `wrangler` OAuth login (no secrets in .env); an
upload failure (offline, wrangler missing) warns and returns False without
blocking the other deliveries — the backfill script sweeps up gaps.

Run AFTER audio_archive in DELIVERY_TARGETS. Bulk/catch-up:
    venv/bin/python3 scripts/backfill/backfill_r2_audio.py
"""

import os
import shutil
import subprocess
import hashlib
import threading

from . import archive_root, parse_note_dt, recording_stem

DEFAULT_BUCKET = "watch-transcriber-audio"


def bucket() -> str:
    return os.environ.get("R2_BUCKET", DEFAULT_BUCKET)


def wrangler_bin() -> str | None:
    return os.environ.get("R2_WRANGLER") or shutil.which("wrangler")


def wrangler_env() -> dict:
    """Subprocess env for wrangler: headless token auth when configured
    (CLOUDFLARE_API_TOKEN_FILE keeps the secret out of .env), else the
    inherited env falls back to the local OAuth login."""
    env = dict(os.environ)
    token_file = env.get("CLOUDFLARE_API_TOKEN_FILE")
    if "CLOUDFLARE_API_TOKEN" not in env and token_file and os.path.exists(token_file):
        with open(token_file, encoding="utf-8") as f:
            env["CLOUDFLARE_API_TOKEN"] = f.read().strip()
    return env


def upload(rel_path: str) -> bool:
    """Upload one archive-relative audio file. Shared with the backfill."""
    wrangler = wrangler_bin()
    if not wrangler:
        print("[delivery:r2_backup] wrangler not found — skipping upload")
        return False
    src = archive_root() / rel_path
    r = subprocess.run(
        [wrangler, "r2", "object", "put", f"{bucket()}/{rel_path}",
         "--file", str(src), "--remote"],
        capture_output=True, text=True, timeout=900, env=wrangler_env(),
    )
    if r.returncode != 0:
        print("[delivery:r2_backup] upload failed")
        return False
    return True


def deliver(note: dict) -> bool:
    dt = parse_note_dt(note)
    date_dir = archive_root() / dt.strftime("%Y-%m-%d")
    audio = next(
        (p for p in sorted(date_dir.glob(f"{recording_stem(note)}.*"))
         if p.suffix not in (".md", ".tmp")),
        None,
    )
    if not audio:
        print("[delivery:r2_backup] no archive audio to upload")
        return True
    rel = str(audio.relative_to(archive_root()))
    if not upload(rel):
        return False
    print("[delivery:r2_backup] recording audio uploaded")
    return True


def audio_relative_path(note: dict) -> str:
    dt = parse_note_dt(note)
    date_dir = archive_root() / dt.strftime("%Y-%m-%d")
    audio = next(
        (
            path
            for path in sorted(date_dir.glob(f"{recording_stem(note)}.*"))
            if path.suffix not in (".md", ".tmp")
        ),
        None,
    )
    if audio is None:
        raise OSError("archive audio is unavailable for R2 verification")
    return audio.relative_to(archive_root()).as_posix()


def verify_destination(
    note: dict,
    *,
    expected_sha256: str,
    expected_size_bytes: int,
    timeout_seconds: float = 900,
) -> dict[str, object]:
    """Bounded-download and hash the actual private R2 object."""

    wrangler = wrangler_bin()
    if not wrangler:
        raise OSError("R2 verification tool is unavailable")
    rel = audio_relative_path(note)
    try:
        process = subprocess.Popen(
            [wrangler, "r2", "object", "get", f"{bucket()}/{rel}", "--pipe", "--remote"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=wrangler_env(),
        )
    except Exception:
        raise OSError("R2 verification could not start") from None

    observed: dict[str, object] = {}

    def read_bounded() -> None:
        digest = hashlib.sha256()
        size = 0
        assert process.stdout is not None
        try:
            while True:
                chunk = process.stdout.read(1024 * 1024)
                if not chunk:
                    break
                size += len(chunk)
                if size > expected_size_bytes:
                    observed["error"] = "oversized"
                    process.kill()
                    return
                digest.update(chunk)
            observed["sha256"] = digest.hexdigest()
            observed["size_bytes"] = size
        except Exception:
            observed["error"] = "read"
            process.kill()

    reader = threading.Thread(target=read_bounded, daemon=True)
    reader.start()
    try:
        return_code = process.wait(timeout=timeout_seconds)
    except Exception:
        process.kill()
        process.wait()
        reader.join(timeout=5)
        raise OSError("R2 verification timed out") from None
    reader.join(timeout=5)
    if (
        reader.is_alive()
        or return_code != 0
        or observed.get("error") is not None
        or observed.get("size_bytes") != expected_size_bytes
        or observed.get("sha256") != expected_sha256
    ):
        raise OSError("R2 object failed integrity verification")
    return {
        "backend": "r2",
        "locator": f"{bucket()}/{rel}",
        "version_id": f"sha256:{expected_sha256}",
        "sha256": expected_sha256,
        "size_bytes": expected_size_bytes,
    }
