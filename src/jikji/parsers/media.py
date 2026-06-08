"""Optional local media parsers (OCR/transcription/metadata).

No heavyweight dependency is required.  If local tools are installed, Jikji uses
those tools with bounded output.  Images always expose lightweight visual
metadata (format/dimensions and selected datetime EXIF when available), while
OCR text is appended only when a local ``tesseract`` binary is installed.
"""
from __future__ import annotations

import json
import logging
import os
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path

log = logging.getLogger(__name__)

_IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".tif", ".tiff", ".webp", ".bmp", ".gif"}
_AUDIO_EXTS = {".mp3", ".wav", ".m4a", ".flac", ".ogg", ".aac", ".opus", ".wma"}
_EXIF_DATETIME_TAGS = ("DateTimeOriginal", "DateTimeDigitized", "DateTime")


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


def image_ocr_available() -> bool:
    """Return whether local image/PDF OCR can run via Tesseract."""
    return shutil.which("tesseract") is not None


def _format_from_suffix(path: Path) -> str:
    ext = path.suffix.lower().lstrip(".")
    if ext in {"jpg", "jpeg"}:
        return "JPEG"
    if ext:
        return ext.upper()
    return "image"


def _jpeg_dimensions(data: bytes) -> tuple[int, int] | None:
    if not data.startswith(b"\xff\xd8"):
        return None
    sof_markers = {
        0xC0,
        0xC1,
        0xC2,
        0xC3,
        0xC5,
        0xC6,
        0xC7,
        0xC9,
        0xCA,
        0xCB,
        0xCD,
        0xCE,
        0xCF,
    }
    idx = 2
    while idx + 9 < len(data):
        if data[idx] != 0xFF:
            idx += 1
            continue
        while idx < len(data) and data[idx] == 0xFF:
            idx += 1
        if idx >= len(data):
            break
        marker = data[idx]
        idx += 1
        if marker in {0x01, *range(0xD0, 0xD9)}:
            continue
        if idx + 2 > len(data):
            break
        block_len = int.from_bytes(data[idx:idx + 2], "big")
        if block_len < 2 or idx + block_len > len(data):
            break
        if marker in sof_markers and block_len >= 7:
            height = int.from_bytes(data[idx + 3:idx + 5], "big")
            width = int.from_bytes(data[idx + 5:idx + 7], "big")
            if width > 0 and height > 0:
                return width, height
        idx += block_len
    return None


def _webp_dimensions(data: bytes) -> tuple[int, int] | None:
    if len(data) < 30 or not (data.startswith(b"RIFF") and data[8:12] == b"WEBP"):
        return None
    chunk = data[12:16]
    if chunk == b"VP8X" and len(data) >= 30:
        width = int.from_bytes(data[24:27], "little") + 1
        height = int.from_bytes(data[27:30], "little") + 1
        return width, height
    if chunk == b"VP8L" and len(data) >= 25 and data[20] == 0x2F:
        b0, b1, b2, b3 = data[21:25]
        width = 1 + (((b1 & 0x3F) << 8) | b0)
        height = 1 + (((b3 & 0x0F) << 10) | (b2 << 2) | ((b1 & 0xC0) >> 6))
        return width, height
    if chunk == b"VP8 " and len(data) >= 30 and data[23:26] == b"\x9d\x01\x2a":
        width = int.from_bytes(data[26:28], "little") & 0x3FFF
        height = int.from_bytes(data[28:30], "little") & 0x3FFF
        if width > 0 and height > 0:
            return width, height
    return None


def _header_image_metadata(path: Path) -> dict[str, object]:
    """Return format/dimensions from common image headers without dependencies."""
    meta: dict[str, object] = {"format": _format_from_suffix(path)}
    try:
        with path.open("rb") as fh:
            data = fh.read(65536)
    except OSError as exc:
        log.debug("image header read failed %s: %s", path, exc)
        return meta
    if len(data) >= 24 and data.startswith(b"\x89PNG\r\n\x1a\n") and data[12:16] == b"IHDR":
        meta["format"] = "PNG"
        meta["width"] = int.from_bytes(data[16:20], "big")
        meta["height"] = int.from_bytes(data[20:24], "big")
    elif len(data) >= 10 and data[:6] in {b"GIF87a", b"GIF89a"}:
        meta["format"] = "GIF"
        meta["width"] = int.from_bytes(data[6:8], "little")
        meta["height"] = int.from_bytes(data[8:10], "little")
    elif len(data) >= 26 and data.startswith(b"BM"):
        meta["format"] = "BMP"
        try:
            meta["width"] = struct.unpack_from("<i", data, 18)[0]
            meta["height"] = abs(struct.unpack_from("<i", data, 22)[0])
        except struct.error:
            pass
    elif dims := _jpeg_dimensions(data):
        meta["format"] = "JPEG"
        meta["width"], meta["height"] = dims
    elif dims := _webp_dimensions(data):
        meta["format"] = "WEBP"
        meta["width"], meta["height"] = dims
    return meta


def _image_metadata(path: Path) -> list[str]:
    fallback = _header_image_metadata(path)
    parts: list[str] = [f"# Image: {path.name}"]
    try:
        from PIL import ExifTags, Image  # type: ignore
    except ImportError:
        image_format = str(fallback.get("format") or _format_from_suffix(path))
        parts.append(f"Format: {image_format}")
        width = fallback.get("width")
        height = fallback.get("height")
        if isinstance(width, int) and isinstance(height, int) and width > 0 and height > 0:
            parts.append(f"Dimensions: {width}x{height} pixels")
        return parts
    try:
        with Image.open(path) as image:
            parts.append(f"Format: {image.format or fallback.get('format') or _format_from_suffix(path)}")
            parts.append(f"Dimensions: {image.width}x{image.height} pixels")
            parts.append(f"Color mode: {image.mode}")
            frames = int(getattr(image, "n_frames", 1) or 1)
            if frames > 1:
                parts.append(f"Frames: {frames}")
            exif = image.getexif()
            if exif:
                tags = getattr(ExifTags, "TAGS", {})
                wanted = {label: key for key, label in tags.items() if label in _EXIF_DATETIME_TAGS}
                for label in _EXIF_DATETIME_TAGS:
                    value = exif.get(wanted.get(label))
                    if value:
                        parts.append(f"EXIF {label}: {str(value).strip()[:80]}")
                        break
    except Exception as exc:
        log.debug("image metadata failed %s: %s", path, exc)
        image_format = str(fallback.get("format") or _format_from_suffix(path))
        parts.append(f"Format: {image_format}")
        width = fallback.get("width")
        height = fallback.get("height")
        if isinstance(width, int) and isinstance(height, int) and width > 0 and height > 0:
            parts.append(f"Dimensions: {width}x{height} pixels")
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
    parts = _image_metadata(path)
    ocr = _ocr_image(path, max_chars)
    if ocr:
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
