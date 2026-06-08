"""Extension → parser dispatcher. Never raises; returns '' on failure."""
from __future__ import annotations

import concurrent.futures as _futures
import logging
import os
import sys
import threading
from collections.abc import Callable
from pathlib import Path

log = logging.getLogger(__name__)

_MIB = 1024 * 1024


def _env_int(name: str, default: int, *, min_value: int = 1) -> int:
    raw = os.environ.get(name)
    if not raw:
        return default
    try:
        value = int(raw)
    except (TypeError, ValueError):
        log.warning("invalid %s=%r; using %d", name, raw, default)
        return default
    return max(min_value, value)


def _env_float(name: str, default: float, *, min_value: float = 0.0) -> float:
    raw = os.environ.get(name)
    if not raw:
        return default
    try:
        value = float(raw)
    except (TypeError, ValueError):
        log.warning("invalid %s=%r; using %.1f", name, raw, default)
        return default
    return max(min_value, value)


# Single shared worker pool so we don't spawn one thread per file just
# to enforce a timeout.  Cross-platform: works identically on Linux,
# macOS, and Windows (no SIGALRM dependency).
#
# Windows one-file/installer builds can be memory-constrained because
# pypdf/openpyxl/python-docx and the Korean tokenizer model live in the
# same process.  Keep the parser pool intentionally small, and expose an
# env override for field diagnostics without changing user config files.
_DEFAULT_PARSE_WORKERS = 1 if sys.platform.startswith("win") else 2
_PARSE_WORKERS = _env_int(
    "JIKJI_PARSE_WORKERS", _DEFAULT_PARSE_WORKERS, min_value=1
)
_PARSE_POOL = _futures.ThreadPoolExecutor(
    max_workers=_PARSE_WORKERS, thread_name_prefix="jikji-parse"
)
_PARSE_QUEUE_SLOTS = _env_int(
    "JIKJI_PARSE_QUEUE_SLOTS",
    max(_PARSE_WORKERS, _PARSE_WORKERS * 2),
    min_value=_PARSE_WORKERS,
)
_PARSE_SLOTS = threading.BoundedSemaphore(_PARSE_QUEUE_SLOTS)

# Large document parsers can allocate many times the on-disk size while
# inflating XML streams or building PDF objects.  For huge files, Jikji
# still classifies from filename/metadata, but skips body extraction.
_DEFAULT_MAX_PARSE_MIB = 64.0
_MAX_PARSE_BYTES = int(
    _env_float("JIKJI_MAX_PARSE_MB", _DEFAULT_MAX_PARSE_MIB) * _MIB
)

SUPPORTED_EXTENSIONS: set[str] = {
    ".pdf",
    ".epub",
    ".eml",
    ".ics",
    ".sqlite",
    ".sqlite3",
    ".db",
    ".docx",
    ".pptx",
    ".ppsx",
    ".xlsx",
    ".doc",
    ".ppt",
    ".pps",
    ".xls",
    ".hwp",
    ".hwpx",
    ".odt",
    ".rtf",
    ".txt",
    ".md",
    ".markdown",
    ".csv",
    ".tsv",
    ".log",
    ".html",
    ".htm",
    ".json",
    ".jsonl",
    ".xml",
    ".yaml",
    ".yml",
    ".ini",
    ".cfg",
    ".toml",
    # Images/audio — OCR/transcription is optional and local-only.
    ".png",
    ".jpg",
    ".jpeg",
    ".tif",
    ".tiff",
    ".webp",
    ".bmp",
    ".gif",
    ".mp3",
    ".wav",
    ".m4a",
    ".flac",
    ".ogg",
    ".aac",
    ".opus",
    ".wma",
    ".mp4",
    ".mov",
    ".mkv",
    ".avi",
    ".webm",
    ".m4v",
    ".wmv",
    ".flv",
    ".mpg",
    ".mpeg",
    # Archives — listed for member-name extraction (no decompression).
    ".zip",
    ".jar",
    ".war",
    ".tar",
    ".tgz",
    ".tbz",
    ".txz",
    ".7z",
    ".rar",
}


def _too_large_for_body_parse(path: Path) -> bool:
    if _MAX_PARSE_BYTES <= 0:
        return False
    try:
        size = path.stat().st_size
    except OSError:
        return False
    return size > _MAX_PARSE_BYTES


def _safe(parser: Callable[[Path, int], str], path: Path, max_chars: int, timeout: float) -> str:
    """Run *parser* with a hard wall-clock timeout, returning ``""`` on
    failure.  Cross-platform: uses a thread-pool ``Future`` so it works
    on Linux, macOS, and Windows alike, including from non-main threads.

    The parser thread may keep running after the timeout (Python has no
    safe way to kill it), but the caller is unblocked immediately.
    """
    wait_s = max(0.1, min(1.0, float(timeout)))
    if not _PARSE_SLOTS.acquire(timeout=wait_s):
        log.warning(
            "parser queue saturated; skipping body parse for %s "
            "(workers=%d slots=%d)",
            path,
            _PARSE_WORKERS,
            _PARSE_QUEUE_SLOTS,
        )
        return ""

    release_by_callback = False
    try:
        future = _PARSE_POOL.submit(parser, path, max_chars)
        release_by_callback = True
        future.add_done_callback(lambda _f: _PARSE_SLOTS.release())
        try:
            text = future.result(timeout=max(0.1, float(timeout)))
        except _futures.TimeoutError:
            future.cancel()
            log.warning("parser timeout (%.1fs): %s", timeout, path)
            return ""
    except Exception as exc:  # pragma: no cover — parser-specific
        log.warning("parser failed for %s: %s", path, exc)
        return ""
    finally:
        if not release_by_callback:
            try:
                _PARSE_SLOTS.release()
            except ValueError:
                pass
    return (text or "").strip()[:max_chars]


def extract_excerpt(path: Path, max_chars: int = 1800, timeout: float = 5.0) -> str:
    """Return up to ``max_chars`` characters of plain text from the document.

    Returns '' if the file is unsupported, unreadable, or parsing fails.
    """
    from . import hwp as hwp_parser
    from . import media, office, structured
    from . import pdf as pdf_parser
    from . import text as text_parser

    path = Path(path)
    ext = path.suffix.lower()
    if ext in SUPPORTED_EXTENSIONS and _too_large_for_body_parse(path):
        log.warning(
            "skip body parse for large file over %.1f MiB: %s",
            _MAX_PARSE_BYTES / _MIB,
            path,
        )
        return ""
    if ext == ".pdf":
        return _safe(pdf_parser.parse, path, max_chars, timeout)
    if ext == ".epub":
        return _safe(structured.parse_epub, path, max_chars, timeout)
    if ext == ".eml":
        return _safe(structured.parse_eml, path, max_chars, timeout)
    if ext == ".ics":
        return _safe(structured.parse_ics, path, max_chars, timeout)
    if ext in {".sqlite", ".sqlite3", ".db"}:
        return _safe(structured.parse_sqlite, path, max_chars, timeout)
    if ext in {".png", ".jpg", ".jpeg", ".tif", ".tiff", ".webp", ".bmp", ".gif"}:
        return _safe(media.parse_image, path, max_chars, timeout)
    if ext in {".mp3", ".wav", ".m4a", ".flac", ".ogg", ".aac", ".opus", ".wma"}:
        return _safe(media.parse_audio, path, max_chars, timeout)
    if ext in {".mp4", ".mov", ".mkv", ".avi", ".webm", ".m4v", ".wmv", ".flv", ".mpg", ".mpeg"}:
        return _safe(media.parse_video, path, max_chars, timeout)
    if ext == ".docx":
        return _safe(office.parse_docx, path, max_chars, timeout)
    if ext in {".pptx", ".ppsx"}:
        # .ppsx is an autoplay variant of .pptx — same XML container.
        return _safe(office.parse_pptx, path, max_chars, timeout)
    if ext == ".xlsx":
        return _safe(office.parse_xlsx, path, max_chars, timeout)
    if ext == ".odt":
        return _safe(office.parse_odt, path, max_chars, timeout)
    if ext in {".doc", ".ppt", ".pps", ".xls"}:
        # Legacy binary Office formats — best-effort text scrape via
        # OLE compound storage; better than nothing for indexing.
        return _safe(office.parse_legacy_office, path, max_chars, timeout)
    if ext == ".hwpx":
        return _safe(hwp_parser.parse_hwpx, path, max_chars, timeout)
    if ext == ".hwp":
        return _safe(hwp_parser.parse_hwp, path, max_chars, timeout)
    if ext == ".rtf":
        return _safe(text_parser.parse_rtf, path, max_chars, timeout)
    if ext in {".txt", ".md", ".markdown", ".csv", ".tsv", ".log",
               ".json", ".jsonl", ".xml", ".yaml", ".yml",
               ".ini", ".cfg", ".toml"}:
        return _safe(text_parser.parse_plain, path, max_chars, timeout)
    if ext in {".html", ".htm"}:
        return _safe(text_parser.parse_html, path, max_chars, timeout)
    # Archive containers: list member names so the classifier can use
    # them as a synthetic "body".  See :mod:`jikji.parsers.archive`.
    from . import archive as archive_parser
    if archive_parser.is_archive(path):
        return _safe(archive_parser.parse, path, max_chars, timeout)
    return ""
