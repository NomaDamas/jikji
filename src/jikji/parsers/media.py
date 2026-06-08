"""Optional local media parsers (OCR/transcription/metadata).

No heavyweight dependency is required.  If local tools are installed, Jikji uses
those tools with bounded output.  Images without OCR text return empty so camera
EXIF does not pollute body search; audio can still expose ffprobe tags.
"""
from __future__ import annotations

import json
import logging
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

log = logging.getLogger(__name__)

_IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".tif", ".tiff", ".webp", ".bmp", ".gif"}
_AUDIO_EXTS = {".mp3", ".wav", ".m4a", ".flac", ".ogg", ".aac", ".opus", ".wma"}
_VIDEO_EXTS = {".mp4", ".mov", ".mkv", ".avi", ".webm", ".m4v", ".wmv", ".flv", ".mpg", ".mpeg"}


def _env_flag(name: str, default: bool = False) -> bool:
    raw = os.environ.get(name)
    if raw is None:
        return default
    return raw.strip().lower() in {"1", "true", "yes", "on"}


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if not raw:
        return default
    try:
        return max(0.1, float(raw))
    except ValueError:
        return default


def _run(cmd: list[str], *, timeout: float) -> str:
    try:
        proc = subprocess.run(  # noqa: S603 - command path is resolved by shutil.which/caller.
            cmd,
            check=False,
            capture_output=True,
            text=True,
            errors="ignore",
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        log.debug("media command failed %s: %s", cmd[:2], exc)
        return ""
    if proc.returncode != 0:
        log.debug("media command non-zero %s: %s", cmd[:2], proc.stderr[:200])
        return ""
    return proc.stdout.strip()


def _image_metadata(path: Path) -> list[str]:
    parts: list[str] = [f"# Image: {path.name}"]
    try:
        from PIL import ExifTags, Image  # type: ignore
    except ImportError:
        return parts
    try:
        with Image.open(path) as image:
            parts.append(f"Format: {image.format or path.suffix.lstrip('.').upper()}")
            parts.append(f"Size: {image.width}x{image.height}")
            parts.append(f"Mode: {image.mode}")
            exif = image.getexif()
            if exif:
                names = getattr(ExifTags, "TAGS", {})
                for key, value in list(exif.items())[:40]:
                    label = names.get(key, str(key))
                    if label in {"MakerNote", "UserComment"}:
                        continue
                    text = str(value).strip()
                    if text:
                        parts.append(f"EXIF {label}: {text[:200]}")
    except Exception as exc:
        log.debug("image metadata failed %s: %s", path, exc)
    return parts


def _ocr_image(path: Path, max_chars: int) -> str:
    tesseract = shutil.which("tesseract")
    if not tesseract:
        return ""
    timeout = _env_float("JIKJI_OCR_TIMEOUT", 15.0)
    lang = os.environ.get("JIKJI_TESSERACT_LANG", "").strip()
    cmd = [tesseract, str(path.resolve()), "stdout", "--psm", os.environ.get("JIKJI_TESSERACT_PSM", "6")]
    if lang:
        cmd.extend(["-l", lang])
    return _run(cmd, timeout=timeout)[:max_chars]


def parse_image(path: Path, max_chars: int) -> str:
    ocr = _ocr_image(path, max_chars)
    if not ocr:
        # Do not treat camera EXIF/dimensions as document body text.  Filename,
        # extension, size, and timestamps are already indexed in file_index.jsonl.
        return ""
    parts = [f"# Image OCR: {path.name}"]
    # Lightweight dimensions are useful context once OCR has proven this image
    # contains text, but EXIF tags stay out of the body index to avoid noisy
    # camera-brand matches across photo libraries.
    for line in _image_metadata(path):
        if line.startswith(("Format:", "Size:", "Mode:")):
            parts.append(line)
    parts.append("# OCR text\n" + ocr)
    return "\n".join(parts)[:max_chars]


def _ffprobe_metadata(path: Path) -> list[str]:
    ffprobe = shutil.which("ffprobe")
    if not ffprobe:
        return []
    raw = _run(
        [
            ffprobe,
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            str(path.resolve()),
        ],
        timeout=_env_float("JIKJI_FFPROBE_TIMEOUT", 8.0),
    )
    if not raw:
        return []
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return []
    parts: list[str] = []
    fmt = data.get("format") if isinstance(data, dict) else {}
    if isinstance(fmt, dict):
        if fmt.get("format_long_name"):
            parts.append(f"Format: {fmt['format_long_name']}")
        if fmt.get("duration"):
            parts.append(f"Duration seconds: {fmt['duration']}")
        tags = fmt.get("tags") or {}
        if isinstance(tags, dict):
            for key in ("title", "artist", "album", "album_artist", "genre", "date", "comment"):
                value = tags.get(key) or tags.get(key.upper())
                if value:
                    parts.append(f"{key.title()}: {str(value)[:300]}")
    streams = data.get("streams") if isinstance(data, dict) else []
    if isinstance(streams, list):
        for stream in streams[:4]:
            if isinstance(stream, dict) and stream.get("codec_type"):
                parts.append(
                    "Stream: "
                    + " ".join(
                        str(x)
                        for x in (stream.get("codec_type"), stream.get("codec_name"), stream.get("language"))
                        if x
                    )
                )
    return parts


def _transcribe_audio(path: Path, max_chars: int) -> str:
    if not _env_flag("JIKJI_ENABLE_TRANSCRIPTION", default=False):
        return ""
    max_mb = _env_float("JIKJI_TRANSCRIBE_MAX_MB", 25.0)
    try:
        if path.stat().st_size > max_mb * 1024 * 1024:
            return ""
    except OSError:
        return ""
    whisper = shutil.which("whisper")
    if not whisper:
        return ""
    model = os.environ.get("JIKJI_WHISPER_MODEL", "tiny")
    timeout = _env_float("JIKJI_TRANSCRIBE_TIMEOUT", 120.0)
    with tempfile.TemporaryDirectory(prefix="jikji-whisper-") as tmp:
        cmd = [
            whisper,
            str(path.resolve()),
            "--model",
            model,
            "--output_format",
            "txt",
            "--output_dir",
            tmp,
            "--fp16",
            "False",
        ]
        language = os.environ.get("JIKJI_WHISPER_LANGUAGE", "").strip()
        if language:
            cmd.extend(["--language", language])
        try:
            proc = subprocess.run(  # noqa: S603 - executable resolved by shutil.which.
                cmd,
                check=False,
                capture_output=True,
                text=True,
                errors="ignore",
                timeout=timeout,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            log.debug("whisper failed %s: %s", path, exc)
            return ""
        if proc.returncode != 0:
            log.debug("whisper non-zero %s: %s", path, proc.stderr[:200])
            return ""
        for txt in Path(tmp).glob("*.txt"):
            try:
                return txt.read_text(encoding="utf-8", errors="ignore")[:max_chars]
            except OSError:
                continue
    return ""


def parse_audio(path: Path, max_chars: int) -> str:
    parts: list[str] = [f"# Audio: {path.name}"]
    parts.extend(_ffprobe_metadata(path))
    transcript = _transcribe_audio(path, max_chars)
    if transcript:
        parts.append("# Transcript\n" + transcript)
    if len(parts) == 1:
        return ""
    return "\n".join(parts)[:max_chars]


def parse_video(path: Path, max_chars: int) -> str:
    parts: list[str] = [f"# Video: {path.name}"]
    parts.extend(_ffprobe_metadata(path))
    if len(parts) == 1:
        return ""
    return "\n".join(parts)[:max_chars]
