"""Non-destructive Jikji agent/human metadata index builder.

The builder keeps the user's folders and filenames untouched.  It writes a
`.jikji/` sidecar workspace containing JSONL/Markdown indexes and, for
parser-required document formats, reusable plain-text caches so CLI agents can
use `rg`/`jq` without reparsing Office/PDF/HWP files on every search.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import tempfile
from collections import Counter, defaultdict
from collections.abc import Callable, Iterable
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

from .config import Config
from .metadata import collect
from .models import FileEntry
from .parsers import extract_excerpt
from .parsers.registry import SUPPORTED_EXTENSIONS
from .scanner import ScanTooLargeError
from .search_index import (
    INSTANT_SEARCH_INDEX,
    INSTANT_SEARCH_SCHEMA_VERSION,
    build_instant_search_index,
)

ProgressCB = Callable[[str, float], None]

AGENT_DIR_NAME = ".jikji"
DOCUMENT_CACHE_EXTENSIONS = {
    ".pdf",
    ".epub",
    ".eml",
    ".ics",
    ".sqlite",
    ".sqlite3",
    ".db",
    ".doc",
    ".docx",
    ".ppt",
    ".pptx",
    ".pps",
    ".ppsx",
    ".xls",
    ".xlsx",
    ".hwp",
    ".hwpx",
    ".odt",
    ".rtf",
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
TEXT_LIKE_EXTENSIONS = SUPPORTED_EXTENSIONS - DOCUMENT_CACHE_EXTENSIONS
_DEFAULT_TEXT_MAX_CHARS = 2_000_000
_DEFAULT_CHUNK_CHARS = 1_000_000
CACHE_KEY_POLICY = "sha256, recomputed only when size or mtime_ns changed or cache is missing"
LOCK_STALE_AFTER_SECONDS = 60 * 60
OWNED_GENERATED_PATHS = [
    "000_JIKJI_AGENT_MAP.md",
    ".jikji/manifest.json",
    ".jikji/file_index.jsonl",
    ".jikji/folder_index.jsonl",
    ".jikji/document_index.jsonl",
    ".jikji/file_cards.jsonl",
    ".jikji/chunk_map.jsonl",
    f".jikji/{INSTANT_SEARCH_INDEX}",
    ".jikji/duplicate_map.jsonl",
    ".jikji/folder_profile.jsonl",
    ".jikji/corpus_profile.json",
    ".jikji/intent_taxonomy.json",
    ".jikji/autorag_manifest.json",
    ".jikji/parse_errors.jsonl",
    ".jikji/agent_map.md",
    ".jikji/agent_routes.md",
    ".jikji/agent_skill_context.md",
    ".jikji/human_guide.md",
    ".jikji/.lock",
    ".jikji/doc_text/",
    ".jikji/doc_meta/",
    ".jikji/eval/",
]
RETIRED_GENERATED_PATHS = [
    ".jikji/search_terms.json",
    ".jikji/search_terms.jsonl",
    ".jikji/folder_cards/",
    ".jikji/file_cards/",
]

INTENT_TAXONOMY: dict[str, tuple[str, ...]] = {
    "계약검토": ("계약", "견적", "발주", "납품", "정산", "대금", "입찰", "협약"),
    "제안준비": ("제안요청서", "RFP", "제안서", "요구사항", "입찰", "제안", "공고"),
    "평가자료": ("평가", "평가항목", "점수", "배점", "채점", "심사", "평가지표", "판정"),
    "교육자료": ("교육", "강의", "창의력", "아이디어", "수업", "발표", "학습", "교재"),
    "점검자료": ("점검", "수행일지", "최종점검", "수시점검", "보고", "회의", "검토의견"),
    "설계자료": ("설계", "클래스설계", "요구사항정의", "화면설계", "아키텍처", "인터페이스"),
    "학습데이터": ("학습데이터", "데이터셋", "말뭉치", "라벨링", "품질검증", "검수", "데이터구축"),
    "감리검수": ("감리", "검수", "검사", "승인", "완료보고", "산출물", "종료"),
}

_FORMAT_HINTS_BY_EXT: dict[str, tuple[str, ...]] = {
    ".hwp": ("한글 문서", "hwp", "문서"),
    ".hwpx": ("한글 문서", "hwpx", "문서"),
    ".pdf": ("PDF", "pdf", "문서"),
    ".ppt": ("파워포인트", "ppt", "발표자료"),
    ".pptx": ("파워포인트", "pptx", "발표자료"),
    ".xls": ("엑셀", "xls", "스프레드시트"),
    ".xlsx": ("엑셀", "xlsx", "스프레드시트"),
    ".doc": ("워드", "doc", "문서"),
    ".docx": ("워드", "docx", "문서"),
    ".txt": ("텍스트", "txt"),
    ".md": ("마크다운", "markdown"),
    ".csv": ("CSV", "표"),
    ".mp4": ("비디오", "video", "recording", "mp4"),
    ".mov": ("비디오", "video", "recording", "mov"),
    ".mkv": ("비디오", "video", "recording", "mkv"),
    ".avi": ("비디오", "video", "recording", "avi"),
    ".webm": ("비디오", "video", "recording", "webm"),
    ".m4v": ("비디오", "video", "recording", "m4v"),
    ".wmv": ("비디오", "video", "recording", "wmv"),
    ".flv": ("비디오", "video", "recording", "flv"),
    ".mpg": ("비디오", "video", "recording", "mpg"),
    ".mpeg": ("비디오", "video", "recording", "mpeg"),
}

_MAP_NOISE_TERMS = {
    "source", "parsed", "jikji", "sha256", "metadata", "cache", "chunk", "parser", "json", "jsonl",
    "path", "name", "file", "data", "true", "false", "null", "none", "sheet", "slide", "page",
    "문서", "파일", "자료", "내용", "관련", "항목", "정보", "관리", "사업", "계획", "보고", "기준",
    "작성", "검토", "확인", "대한", "통해", "위한", "경우", "현재", "사용", "제공", "포함", "결과",
    "일반", "가능", "사항", "부분", "전체", "방법", "대상", "업무", "서비스", "시스템", "데이터",
}
_MAP_NOISE_RE = re.compile(
    r"(?:백업|다운로드|download|kakao|카카오|source|sha256|json|cache|metadata|copyright|"
    r"confidential|reserved|parsed|sheet|android|ios|노트북|usb|desktop|바탕화면|문서백업|"
    r"과거백업|chunk|parser)",
    re.IGNORECASE,
)
_COPY_SUFFIX_RE = re.compile(r"(?:\s*\(\d+\)|\s*-\s*copy|\s+copy|_copy|\s*사본)$", re.IGNORECASE)


def _now_iso() -> str:
    return datetime.now(tz=UTC).astimezone().isoformat(timespec="seconds")


def _rel(root: Path, path: Path) -> str:
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return path.as_posix()


def _json_dump(obj) -> str:
    return json.dumps(obj, ensure_ascii=False, sort_keys=True)


def _atomic_write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", encoding="utf-8", dir=str(path.parent), delete=False
    ) as fh:
        fh.write(text)
        tmp = Path(fh.name)
    tmp.replace(path)


def _write_json(path: Path, obj) -> None:
    _atomic_write_text(path, json.dumps(obj, ensure_ascii=False, indent=2, sort_keys=True) + "\n")


def _write_jsonl(path: Path, rows: Iterable[dict]) -> None:
    _atomic_write_text(path, "".join(_json_dump(row) + "\n" for row in rows))


def _remove_path_quietly(path: Path) -> None:
    try:
        if path.is_dir():
            shutil.rmtree(path)
        elif path.exists():
            path.unlink()
    except OSError:
        return


@contextmanager
def _index_lock(index_dir: Path):
    """Best-effort same-root prepare lock.

    Jikji writes files atomically, but a concurrent prepare for the same root can
    still make summaries inconsistent. The lock itself is a generated artifact
    and is removed on normal exit.
    """
    lock_path = index_dir / ".lock"
    try:
        fd = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    except FileExistsError as exc:
        if _remove_stale_lock(lock_path):
            try:
                fd = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
            except FileExistsError as retry_exc:
                raise RuntimeError(
                    f"Jikji index is already being prepared: {lock_path}. "
                    "If no Jikji process is running, remove this stale lock and retry."
                ) from retry_exc
        else:
            raise RuntimeError(
                f"Jikji index is already being prepared: {lock_path}. "
                "If no Jikji process is running, remove this stale lock and retry."
            ) from exc
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(json.dumps({"pid": os.getpid(), "started_at": _now_iso()}, ensure_ascii=False))
        yield
    finally:
        try:
            lock_path.unlink()
        except FileNotFoundError:
            pass


def _remove_stale_lock(lock_path: Path) -> bool:
    try:
        raw = lock_path.read_text(encoding="utf-8", errors="ignore")
        data = json.loads(raw) if raw.strip() else {}
    except (OSError, json.JSONDecodeError):
        return False
    pid = data.get("pid")
    if not isinstance(pid, int) or pid <= 0:
        return _unlink_if_lock_age_stale(lock_path, data)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        try:
            lock_path.unlink()
            return True
        except OSError:
            return False
    except OSError:
        return False
    # PID reuse can make an unrelated live process look like the original
    # Jikji writer. A coarse age fallback prevents permanent deadlocks while
    # still giving normal prepare runs ample time to finish.
    return _unlink_if_lock_age_stale(lock_path, data)


def _unlink_if_lock_age_stale(lock_path: Path, data: dict) -> bool:
    started = str(data.get("started_at") or "")
    try:
        started_at = datetime.fromisoformat(started)
        if started_at.tzinfo is None:
            started_at = started_at.replace(tzinfo=UTC)
    except ValueError:
        try:
            age = datetime.now(tz=UTC).timestamp() - lock_path.stat().st_mtime
        except OSError:
            return False
        if age < LOCK_STALE_AFTER_SECONDS:
            return False
        try:
            lock_path.unlink()
            return True
        except OSError:
            return False
    age = (datetime.now(tz=UTC) - started_at.astimezone(UTC)).total_seconds()
    if age < LOCK_STALE_AFTER_SECONDS:
        return False
    try:
        lock_path.unlink()
        return True
    except OSError:
        return False


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def _hash_allowed(path: Path, max_hash_bytes: int) -> bool:
    if max_hash_bytes <= 0:
        return True
    try:
        return path.stat().st_size <= max_hash_bytes
    except OSError:
        return False


def _fingerprint(path: Path) -> dict:
    st = path.stat()
    return {
        "size": int(st.st_size),
        "mtime_ns": int(st.st_mtime_ns),
        "mtime": datetime.fromtimestamp(st.st_mtime, tz=UTC).astimezone().isoformat(timespec="seconds"),
    }


def _load_jsonl_by_path(path: Path) -> dict[str, dict]:
    if not path.exists():
        return {}
    out: dict[str, dict] = {}
    try:
        for line in path.read_text(encoding="utf-8").split("\n"):
            if not line.strip():
                continue
            row = json.loads(line)
            p = str(row.get("path") or "")
            if p:
                out[p] = row
    except Exception:
        return {}
    return out


def _ignore_name(name: str, patterns: Iterable[str]) -> bool:
    import fnmatch

    if name == AGENT_DIR_NAME:
        return True
    if name == "000_JIKJI_AGENT_MAP.md" or name.startswith("Jikji_Report_"):
        return True
    return any(fnmatch.fnmatch(name, pat) for pat in patterns)


def _is_archive_path(path: Path) -> bool:
    from .parsers import archive as archive_parser

    return archive_parser.is_archive(path)


def _body_keyword_text(text: str) -> str:
    """Drop generated parser/cache headings before choosing sparse keywords."""
    lines = []
    for line in (text or "").splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith("#"):
            continue
        lines.append(stripped)
    return "\n".join(lines)


def _parser_required(path: Path, ext: str) -> bool:
    """Return True when Jikji should create a reusable text cache.

    Some archive formats have compound suffixes (``.tar.gz``) that
    ``Path.suffix`` sees as ``.gz``.  Delegate that detection to the archive
    parser so member-name listings are cached just like PDF/Office text.
    """
    if ext in DOCUMENT_CACHE_EXTENSIONS:
        return True
    return _is_archive_path(path)


def _read_cached_doc_text(path: Path) -> str | None:
    """Return cached parser text from a file or chunk directory if it exists."""
    try:
        if path.is_file():
            return path.read_text(encoding="utf-8", errors="ignore")
        if path.is_dir():
            parts: list[str] = []
            total = 0
            for chunk in sorted(path.glob("chunk_*.txt")):
                text = chunk.read_text(encoding="utf-8", errors="ignore")
                parts.append(text)
                total += len(text)
                if total >= 16_000:
                    break
            return "\n".join(parts)
    except OSError:
        return None
    return None


def _scan_files_and_dirs(root: Path, config: Config) -> tuple[list[Path], list[Path]]:
    root = Path(root).expanduser().resolve()
    ignore = list(getattr(config, "ignore_patterns", []) or [])
    if not getattr(config, "include_hidden", False):
        ignore.append(".*")
    if not getattr(config, "include_sensitive", False):
        ignore.extend(getattr(config, "safety_ignore_patterns", []) or [])
    dirs: list[Path] = []
    files: list[Path] = []
    limit = int(getattr(config, "max_files", 5000) or 5000)

    def walk(cur: Path) -> None:
        try:
            with os.scandir(cur) as it:
                for entry in it:
                    name = entry.name
                    if _ignore_name(name, ignore):
                        continue
                    try:
                        if entry.is_symlink():
                            continue
                        p = Path(entry.path)
                        if entry.is_dir(follow_symlinks=False):
                            dirs.append(p)
                            walk(p)
                        elif entry.is_file(follow_symlinks=False):
                            files.append(p)
                            if len(files) > limit:
                                raise ScanTooLargeError(len(files), limit)
                    except PermissionError:
                        continue
        except PermissionError:
            return

    walk(root)
    return sorted(files, key=lambda p: str(p)), sorted(dirs, key=lambda p: str(p))


_TOKEN_TEXT_RE = re.compile(r"[0-9A-Za-z가-힣ぁ-ゟ゠-ヿ一-鿿][0-9A-Za-z가-힣ぁ-ゟ゠-ヿ一-鿿._-]*")
_CJK_RE = re.compile(r"[가-힣ぁ-ゟ゠-ヿ一-鿿]")


def _cjk_ngrams(text: str, *, limit: int = 24) -> list[str]:
    compact = re.sub(r"[^0-9a-z가-힣ぁ-ゟ゠-ヿ一-鿿]+", "", (text or "").casefold())
    if len(compact) < 2 or not _CJK_RE.search(compact):
        return []
    out: list[str] = []
    seen: set[str] = set()
    for n in (4, 3, 2):
        if len(compact) < n:
            continue
        for idx in range(0, len(compact) - n + 1):
            gram = compact[idx:idx + n]
            if gram in seen:
                continue
            seen.add(gram)
            out.append(gram)
            if len(out) >= limit:
                return out
    return out


def _tokens_from_text(text: str, *, limit: int = 16) -> list[str]:
    tokens = []
    seen = set()
    for raw in _TOKEN_TEXT_RE.findall(text or ""):
        tok = raw.strip("._-")
        candidates = [tok]
        if _CJK_RE.search(tok) and len(tok) >= 3:
            # Japanese and Chinese text often arrives as long script runs
            # without whitespace. Add bounded character n-grams so a query for
            # a remembered phrase can still route to the right file without an
            # embedding model or language-specific tokenizer.
            candidates.extend(_cjk_ngrams(tok, limit=max(limit * 4, 64)))
        for candidate in candidates:
            if len(candidate) < 2:
                continue
            norm = candidate.casefold()
            if norm in seen or norm in {"jikji", "file", "data", "文書", "문서", "파일", "資料", "자료"}:
                continue
            seen.add(norm)
            tokens.append(candidate)
            if len(tokens) >= limit:
                return tokens
    return tokens


def _semantic_hints(rel_path: str, name: str, ext: str, keywords: list[str], summary: str = "") -> list[str]:
    """Return deterministic search hints for natural-language file descriptions.

    These hints are deliberately local and transparent: they are derived from
    structural strings (path/name/extension/keywords) only, not from a remote
    LLM and not from baked-in benchmark-specific aliases.
    """
    hints: list[str] = []

    def add(*terms: str) -> None:
        for term in terms:
            if term and term not in hints:
                hints.append(term)

    path_parts = [p for p in Path(rel_path).parts if p not in {".", ""}]
    split_structural = re.sub(r"[/_.()\\[\\]{}&+-]+", " ", " ".join([rel_path, name]))
    add(*_tokens_from_text(" ".join(path_parts), limit=24))
    add(*_tokens_from_text(split_structural, limit=32))
    add(*keywords[:16])
    if ext:
        add(ext.lstrip("."))
    return hints[:48]


def _read_map_text(root: Path, row: dict, *, max_chars: int = 96_000) -> str:
    """Read local text available to Jikji and trim it for map feature extraction."""
    cache = str(row.get("text_cache_path") or "")
    parts: list[str] = []
    if cache:
        path = root / cache
        try:
            if path.is_file():
                parts.append(path.read_text(encoding="utf-8", errors="ignore")[:max_chars])
            elif path.is_dir():
                total = 0
                for chunk in sorted(path.glob("chunk_*.txt")):
                    text = chunk.read_text(encoding="utf-8", errors="ignore")
                    parts.append(text)
                    total += len(text)
                    if total >= max_chars:
                        break
        except OSError:
            pass
    elif str(row.get("ext") or "").lower() in TEXT_LIKE_EXTENSIONS:
        try:
            parts.append((root / str(row.get("path") or "")).read_text(encoding="utf-8", errors="ignore")[:max_chars])
        except OSError:
            pass
    text = _body_keyword_text("\n".join(parts))
    if not text.strip():
        text = str(row.get("summary") or "")
    return text[:max_chars]


def _clean_map_terms(text: str, *, rel_path: str = "", limit: int = 80) -> list[str]:
    """Extract deterministic, low-noise map terms without embedding/LLM calls."""
    out: list[str] = []
    seen: set[str] = set()
    rel_norm = rel_path.casefold()
    for raw in _tokens_from_text(text, limit=limit * 8):
        token = raw.strip("._-:;,()[]{}")
        norm = token.casefold()
        if not token or norm in seen or norm in _MAP_NOISE_TERMS:
            continue
        if _MAP_NOISE_RE.search(token):
            continue
        if norm in rel_norm and re.search(r"[가-힣]", token):
            # Avoid path/backup labels dominating semantic map cards. Keep
            # English path acronyms because they are often genuine product IDs.
            continue
        if len(token) < 2 or len(token) > 28:
            continue
        if re.fullmatch(r"[0-9_.:/()\\-]+", token):
            continue
        if re.fullmatch(r"[a-f0-9]{10,}", norm):
            continue
        digit_ratio = sum(ch.isdigit() for ch in token) / max(1, len(token))
        if digit_ratio > 0.35:
            continue
        has_cjk = bool(_CJK_RE.search(token))
        has_alpha = bool(re.search(r"[A-Za-z]", token))
        if not has_cjk and (not has_alpha or len(token) < 4):
            continue
        seen.add(norm)
        out.append(token)
        if len(out) >= limit:
            break
    return out


def _phrase_signatures(terms: list[str], *, limit: int = 24) -> list[str]:
    phrases: list[str] = []
    seen: set[str] = set()
    for n in (2, 3):
        for idx in range(0, max(0, len(terms) - n + 1)):
            phrase = " ".join(terms[idx:idx + n])
            norm = phrase.casefold()
            if norm in seen:
                continue
            seen.add(norm)
            phrases.append(phrase)
            if len(phrases) >= limit:
                return phrases
    return phrases


def _intent_tags(text: str) -> list[str]:
    compact = re.sub(r"\s+", "", text or "").casefold()
    tags: list[str] = []
    for tag, needles in INTENT_TAXONOMY.items():
        if any(re.sub(r"\s+", "", needle).casefold() in compact for needle in needles):
            tags.append(tag)
    return tags


def _evidence_previews(text: str, terms: list[str], *, limit: int = 5) -> list[str]:
    previews: list[str] = []
    seen: set[str] = set()
    useful_terms = [t for t in terms[:20] if len(t) >= 2]
    for raw in re.split(r"[\n\r。.!?]\s*", text or ""):
        line = re.sub(r"\s+", " ", raw).strip()
        if len(line) < 20:
            continue
        if _MAP_NOISE_RE.search(line):
            continue
        hit_count = sum(1 for term in useful_terms if term in line)
        if hit_count == 0:
            continue
        preview = line[:260]
        if preview in seen:
            continue
        seen.add(preview)
        previews.append(preview)
        if len(previews) >= limit:
            break
    if not previews:
        fallback = re.sub(r"\s+", " ", (text or "").strip())[:260]
        if fallback:
            previews.append(fallback)
    return previews


def _normalised_duplicate_stem(path: str) -> str:
    stem = Path(Path(path).name).stem.casefold().strip()
    while True:
        cleaned = _COPY_SUFFIX_RE.sub("", stem).strip()
        if cleaned == stem:
            return cleaned
        stem = cleaned


def _compact_filename_lookup_text(text: str) -> str:
    return re.sub(r"[^0-9a-z가-힣ぁ-ゟ゠-ヿ一-鿿]+", "", (text or "").casefold())


def _filename_lookup_keys(path_or_name: str) -> list[str]:
    raw = (path_or_name or "").strip()
    name = Path(raw).name or raw
    stem = Path(name).stem or name
    duplicate_stem = _normalised_duplicate_stem(name)
    keys = {
        _compact_filename_lookup_text(raw),
        _compact_filename_lookup_text(name),
        _compact_filename_lookup_text(stem),
        _compact_filename_lookup_text(duplicate_stem),
    }
    return sorted(key for key in keys if key)


def _backup_like_score(path: str) -> int:
    return len(re.findall(r"백업|backup|copy|사본|다운로드|download|\(\d+\)", path, flags=re.IGNORECASE))


def _representative_path(paths: list[str]) -> str:
    return sorted(paths, key=lambda p: (_backup_like_score(p), len(Path(p).parts), len(p), p))[0]


def _build_duplicate_groups(file_rows: list[dict]) -> tuple[list[dict], dict[str, str]]:
    groups: list[dict] = []
    path_to_group: dict[str, str] = {}
    by_hash: dict[str, list[dict]] = defaultdict(list)
    by_near: dict[tuple[str, str, int], list[dict]] = defaultdict(list)
    for row in file_rows:
        path = str(row.get("path") or "")
        sha = str(row.get("sha256") or "")
        if sha:
            by_hash[sha].append(row)
        size_bucket = int(row.get("size") or 0) // 4096
        by_near[(_normalised_duplicate_stem(path), str(row.get("ext") or ""), size_bucket)].append(row)

    for sha, rows in sorted(by_hash.items()):
        if len(rows) < 2:
            continue
        members = sorted(str(row.get("path") or "") for row in rows if row.get("path"))
        group_id = f"sha256_{sha}"
        groups.append({
            "group_id": group_id,
            "type": "exact_hash",
            "representative": _representative_path(members),
            "members": members,
            "member_count": len(members),
        })
        for member in members:
            path_to_group[member] = group_id

    for key, rows in sorted(by_near.items()):
        members = sorted(str(row.get("path") or "") for row in rows if row.get("path"))
        if len(members) < 2 or all(member in path_to_group for member in members):
            continue
        digest = hashlib.sha1("|".join(str(x) for x in key).encode("utf-8", "ignore")).hexdigest()[:16]
        group_id = f"near_{digest}"
        groups.append({
            "group_id": group_id,
            "type": "near_name_size",
            "representative": _representative_path(members),
            "members": members,
            "member_count": len(members),
            "key": {"stem": key[0], "ext": key[1], "size_bucket_4k": key[2]},
        })
        for member in members:
            path_to_group.setdefault(member, group_id)

    for row in file_rows:
        path = str(row.get("path") or "")
        sha = str(row.get("sha256") or "")
        if path and path not in path_to_group and sha:
            path_to_group[path] = f"sha256_{sha}"
    return groups, path_to_group


def _build_folder_profiles(folder_rows: list[dict], file_rows: list[dict]) -> list[dict]:
    child_docs: dict[str, list[dict]] = defaultdict(list)
    for row in file_rows:
        parent = str(Path(str(row.get("path") or "")).parent)
        if parent == "":
            parent = "."
        child_docs[parent].append(row)

    profiles = []
    for row in folder_rows:
        path = str(row.get("path") or ".")
        docs = child_docs.get(path, [])
        text = " ".join([
            path,
            " ".join(str(x) for x in row.get("keywords") or []),
            " ".join(str(doc.get("name") or "") for doc in docs[:80]),
        ])
        roles = _intent_tags(text)
        backup_score = _backup_like_score(path)
        profiles.append({
            "path": path,
            "folder_id": row.get("folder_id"),
            "roles": roles,
            "backup_like_score": backup_score,
            "file_count_direct": row.get("file_count_direct", 0),
            "top_extensions_direct": row.get("top_extensions_direct", {}),
            "autorag_priority": "low" if backup_score >= 2 else ("high" if roles else "normal"),
            "summary": row.get("summary", ""),
        })
    return profiles


def _build_map_artifacts(root: Path, file_rows: list[dict], folder_rows: list[dict], doc_rows: list[dict]) -> dict:
    duplicate_groups, path_to_group = _build_duplicate_groups(file_rows)
    doc_paths = {str(row.get("path") or "") for row in doc_rows}
    map_inputs: list[tuple[dict, str, list[str]]] = []
    df: Counter[str] = Counter()
    for row in file_rows:
        rel_path = str(row.get("path") or "")
        text = _read_map_text(root, row)
        combined = "\n".join([
            str(row.get("name") or ""),
            str(row.get("summary") or ""),
            " ".join(str(x) for x in row.get("keywords") or []),
            text,
        ])
        terms = _clean_map_terms(combined, rel_path=rel_path, limit=96)
        df.update({term.casefold() for term in terms})
        map_inputs.append((row, text, terms))

    total = max(1, len(map_inputs))
    file_cards: list[dict] = []
    chunk_rows: list[dict] = []
    for row, text, terms in map_inputs:
        rel_path = str(row.get("path") or "")
        ext = str(row.get("ext") or "").lower()

        def term_key(term: str) -> tuple[float, int, str]:
            rarity = total / max(1, df.get(term.casefold(), 1))
            return (-rarity, -len(term), term)

        content_terms = terms[:48]
        rare_terms = sorted(terms, key=term_key)[:32]
        content_phrases = _phrase_signatures(content_terms[:32], limit=16)
        rare_phrases = _phrase_signatures(rare_terms, limit=16)
        phrase_signatures = (content_phrases + [p for p in rare_phrases if p not in content_phrases])[:28]
        tag_text = " ".join([
            rel_path,
            str(row.get("name") or ""),
            str(row.get("summary") or ""),
            " ".join(rare_terms),
            text[:4000],
        ])
        intent_tags = _intent_tags(tag_text)
        format_hints = list(_FORMAT_HINTS_BY_EXT.get(ext, (ext.lstrip("."),))) if ext else []
        evidence = _evidence_previews(text, content_terms + rare_terms, limit=5)
        filename_keys = _filename_lookup_keys(rel_path)
        for key in _filename_lookup_keys(str(row.get("name") or "")):
            if key not in filename_keys:
                filename_keys.append(key)
        file_cards.append({
            "schema_version": 1,
            "path": rel_path,
            "name": row.get("name", ""),
            "ext": ext,
            "mime": row.get("mime", ""),
            "size": row.get("size", 0),
            "mtime": row.get("mtime", ""),
            "sha256": row.get("sha256", ""),
            "parse_status": row.get("parse_status", ""),
            "text_cache_path": row.get("text_cache_path", ""),
            "doc_meta_path": row.get("doc_meta_path", ""),
            "duplicate_group_id": path_to_group.get(rel_path, ""),
            "is_document": rel_path in doc_paths,
            "folder_roles": _intent_tags(str(Path(rel_path).parent)),
            "intent_tags": intent_tags,
            "content_terms": content_terms,
            "rare_terms": rare_terms,
            "phrase_signatures": phrase_signatures,
            "format_hints": format_hints,
            "path_terms": row.get("path_terms", []),
            "name_terms": row.get("name_terms", []),
            "folder_terms": row.get("folder_terms", []),
            "filename_lookup_keys": filename_keys,
            "evidence_previews": evidence,
            "summary": row.get("summary", ""),
            "map_quality": {
                "has_body_text": bool(text.strip()),
                "content_term_count": len(content_terms),
                "rare_term_count": len(rare_terms),
                "intent_tag_count": len(intent_tags),
                "preview_count": len(evidence),
            },
        })

        if text.strip():
            chunk_size = 6000
            max_chunks = 24
            for n, start in enumerate(range(0, min(len(text), chunk_size * max_chunks), chunk_size), 1):
                chunk = text[start:start + chunk_size]
                chunk_terms = _clean_map_terms(chunk, rel_path=rel_path, limit=32)
                if not chunk_terms and len(chunk.strip()) < 80:
                    continue
                chunk_content = chunk_terms[:24]
                chunk_rare = sorted(chunk_terms, key=term_key)[:16]
                chunk_rows.append({
                    "schema_version": 1,
                    "path": rel_path,
                    "chunk_id": f"{row.get('sha256') or hashlib.sha1(rel_path.encode()).hexdigest()[:16]}:{n:04d}",
                    "text_cache_path": row.get("text_cache_path", ""),
                    "char_start": start,
                    "char_end": start + len(chunk),
                    "token_estimate": max(1, len(chunk) // 4),
                    "heading_hint": "",
                    "page_hint": None,
                    "sheet_hint": None,
                    "slide_hint": None,
                    "content_terms": chunk_content,
                    "rare_terms": chunk_rare,
                    "phrase_signatures": _phrase_signatures(chunk_content, limit=8),
                    "intent_tags": _intent_tags(chunk),
                    "preview": _evidence_previews(chunk, chunk_rare, limit=1)[0] if chunk.strip() else "",
                })

    folder_profiles = _build_folder_profiles(folder_rows, file_rows)
    ext_counts = Counter(str(row.get("ext") or "[noext]").lower() for row in file_rows)
    parse_counts = Counter(str(row.get("parse_status") or "unknown") for row in file_rows)
    corpus_profile = {
        "schema_version": 1,
        "root": str(root),
        "files": len(file_rows),
        "folders": len(folder_rows),
        "documents": len(doc_rows),
        "file_cards": len(file_cards),
        "chunks": len(chunk_rows),
        "duplicate_groups": len(duplicate_groups),
        "top_extensions": dict(ext_counts.most_common(30)),
        "parse_status_counts": dict(parse_counts.most_common()),
        "autorag_readiness": {
            "has_file_cards": bool(file_cards),
            "has_chunk_map": bool(chunk_rows),
            "has_duplicate_map": bool(duplicate_groups),
            "document_card_coverage": round(sum(1 for c in file_cards if c.get("is_document")) / max(1, len(doc_rows)), 4),
        },
    }
    return {
        "file_cards": sorted(file_cards, key=lambda row: str(row.get("path") or "")),
        "chunk_map": sorted(chunk_rows, key=lambda row: (str(row.get("path") or ""), str(row.get("chunk_id") or ""))),
        "duplicate_map": sorted(duplicate_groups, key=lambda row: str(row.get("group_id") or "")),
        "folder_profile": sorted(folder_profiles, key=lambda row: str(row.get("path") or "")),
        "corpus_profile": corpus_profile,
    }


@dataclass
class AgentIndexResult:
    files: int = 0
    folders: int = 0
    docs_parsed: int = 0
    docs_reused: int = 0
    docs_failed: int = 0
    deleted: int = 0
    index_dir: Path | None = None
    agent_map: Path | None = None


def build_agent_index(
    target_root: Path,
    config: Config,
    *,
    progress: ProgressCB | None = None,
    cancel_check=None,
) -> AgentIndexResult:
    """Create/update `.jikji` metadata artifacts with a same-root lock."""
    root = Path(target_root).expanduser().resolve()
    if not root.is_dir():
        raise NotADirectoryError(root)
    index_dir = root / AGENT_DIR_NAME
    index_dir.mkdir(parents=True, exist_ok=True)
    with _index_lock(index_dir):
        return _build_agent_index_unlocked(root, config, progress=progress, cancel_check=cancel_check)


def _build_agent_index_unlocked(
    target_root: Path,
    config: Config,
    *,
    progress: ProgressCB | None = None,
    cancel_check=None,
) -> AgentIndexResult:
    """Create/update `.jikji` metadata artifacts without moving files."""
    root = Path(target_root).expanduser().resolve()
    if not root.is_dir():
        raise NotADirectoryError(root)

    def check() -> None:
        if cancel_check is not None and cancel_check():
            raise RuntimeError("canceled by user")

    index_dir = root / AGENT_DIR_NAME
    doc_text_dir = index_dir / "doc_text"
    doc_meta_dir = index_dir / "doc_meta"
    folder_cards_dir = index_dir / "folder_cards"
    file_cards_dir = index_dir / "file_cards"
    for d in (doc_text_dir, doc_meta_dir):
        d.mkdir(parents=True, exist_ok=True)

    if progress:
        progress("jikji: 파일/폴더 변경분 스캔", 0.02)
    files, dirs = _scan_files_and_dirs(root, config)
    check()

    previous = _load_jsonl_by_path(index_dir / "file_index.jsonl")
    prev_paths = set(previous)
    current_paths = {_rel(root, p) for p in files}
    deleted_rows = [previous[p] | {"status": "deleted", "deleted_at": _now_iso()} for p in sorted(prev_paths - current_paths)]

    folder_children: dict[str, list[str]] = defaultdict(list)
    for d in dirs:
        parent = _rel(root, d.parent) if d.parent != root else "."
        folder_children[parent].append(d.name)

    folder_file_counts: Counter[str] = Counter()
    folder_size_counts: Counter[str] = Counter()
    folder_ext_counts: dict[str, Counter[str]] = defaultdict(Counter)
    for f in files:
        parent_rel = _rel(root, f.parent) if f.parent != root else "."
        try:
            size = f.stat().st_size
        except OSError:
            size = 0
        folder_file_counts[parent_rel] += 1
        folder_size_counts[parent_rel] += size
        folder_ext_counts[parent_rel][f.suffix.lower() or "[noext]"] += 1

    file_rows: list[dict] = []
    doc_rows: list[dict] = []
    parse_errors: list[dict] = []
    result = AgentIndexResult(files=len(files), folders=len(dirs), deleted=len(deleted_rows), index_dir=index_dir)
    text_max = int(getattr(config, "agent_doc_text_max_chars", _DEFAULT_TEXT_MAX_CHARS) or _DEFAULT_TEXT_MAX_CHARS)
    chunk_chars = int(getattr(config, "agent_doc_text_chunk_chars", _DEFAULT_CHUNK_CHARS) or _DEFAULT_CHUNK_CHARS)
    parse_timeout = float(getattr(config, "parse_timeout_s", 5.0) or 5.0)
    max_hash_bytes = int(getattr(config, "max_hash_bytes", 512 * 1024 * 1024) or 0)

    for idx, path in enumerate(files, 1):
        check()
        rel_path = _rel(root, path)
        if progress and (idx == 1 or idx % 50 == 0 or idx == len(files)):
            progress(f"jikji: 파일 메타 갱신 {idx}/{len(files)}", 0.05 + 0.55 * (idx / max(1, len(files))))
        try:
            entry: FileEntry = collect(path)
            fp = _fingerprint(path)
        except OSError as exc:
            parse_errors.append({
                "path": rel_path,
                "code": "access_denied",
                "error": str(exc),
                "stage": "metadata",
            })
            continue

        prev = previous.get(rel_path) or {}
        unchanged = (
            prev.get("size") == fp["size"]
            and prev.get("mtime_ns") == fp["mtime_ns"]
            and prev.get("status", "present") == "present"
        )
        ext = entry.ext.lower()
        parser_required = _parser_required(path, ext)
        text_cache_path = prev.get("text_cache_path", "") if unchanged else ""
        doc_meta_path = prev.get("doc_meta_path", "") if unchanged else ""
        content_hash = prev.get("sha256", "") if unchanged else ""
        parse_status = prev.get("parse_status", "not_required") if unchanged else "not_required"
        summary = prev.get("summary", "") if unchanged else ""
        keywords = list(prev.get("keywords", []) or []) if unchanged else []

        if parser_required:
            parsed_text_sample = ""
            if unchanged and text_cache_path and (root / text_cache_path).exists():
                result.docs_reused += 1
            elif not _hash_allowed(path, max_hash_bytes):
                parse_status = "hash_oversize"
                result.docs_failed += 1
                parse_errors.append({
                    "path": rel_path,
                    "code": "hash_oversize",
                    "error": f"file exceeds max_hash_bytes={max_hash_bytes}",
                    "stage": "hash",
                })
                content_hash = ""
                text_cache_path = ""
                doc_meta_path = ""
            else:
                try:
                    content_hash = _sha256(path)
                    text_cache_path = f"{AGENT_DIR_NAME}/doc_text/sha256_{content_hash}.txt"
                    doc_meta_path = f"{AGENT_DIR_NAME}/doc_meta/sha256_{content_hash}.json"
                    text_path = root / text_cache_path
                    cached_text = _read_cached_doc_text(text_path)
                    if cached_text is not None:
                        parsed_text_sample = cached_text
                        parse_status = "success" if cached_text.strip() else "empty"
                        result.docs_reused += 1
                    else:
                        parsed_text = extract_excerpt(path, max_chars=text_max, timeout=parse_timeout)
                        parsed_text_sample = parsed_text
                        if parsed_text.strip():
                            header = (
                                f"# Source: {rel_path}\n"
                                f"# File ID: sha256:{content_hash}\n"
                                f"# Parsed by: Jikji\n\n"
                            )
                            if len(parsed_text) > chunk_chars:
                                chunk_dir = root / f"{AGENT_DIR_NAME}/doc_text/sha256_{content_hash}"
                                if chunk_dir.exists() and chunk_dir.is_file():
                                    chunk_dir.unlink()
                                chunk_dir.mkdir(parents=True, exist_ok=True)
                                for old in chunk_dir.glob("chunk_*.txt"):
                                    old.unlink()
                                for n, start in enumerate(range(0, len(parsed_text), chunk_chars), 1):
                                    chunk = parsed_text[start:start + chunk_chars]
                                    _atomic_write_text(chunk_dir / f"chunk_{n:04d}.txt", header + chunk)
                                text_cache_path = f"{AGENT_DIR_NAME}/doc_text/sha256_{content_hash}"
                            else:
                                _atomic_write_text(text_path, header + parsed_text)
                            parse_status = "archive_listing" if _is_archive_path(path) else "success"
                            result.docs_parsed += 1
                        else:
                            parse_status = "empty"
                            result.docs_failed += 1
                    if parsed_text_sample:
                        keyword_text = _body_keyword_text(parsed_text_sample)[:4000]
                        keywords = _tokens_from_text(f"{entry.name}\n{keyword_text}")
                        summary = parsed_text_sample.strip().replace("\n", " ")[:240]
                except Exception as exc:  # parser/hash failure should not abort indexing
                    if parse_status != "hash_oversize":
                        parse_status = "failed"
                        result.docs_failed += 1
                        parse_errors.append({
                            "path": rel_path,
                            "code": "parser_crashed",
                            "error": str(exc),
                            "stage": "parse",
                        })
                    if not content_hash:
                        content_hash = ""
        elif ext in TEXT_LIKE_EXTENSIONS:
            keywords = _tokens_from_text(entry.name)
            parse_status = "native_text"
        else:
            keywords = _tokens_from_text(entry.name)
            parse_status = "not_required"

        if not content_hash and not unchanged:
            # Hash every new/changed file so moves can be correlated later.
            try:
                if _hash_allowed(path, max_hash_bytes):
                    content_hash = _sha256(path)
                else:
                    parse_errors.append({
                        "path": rel_path,
                        "code": "hash_oversize",
                        "error": f"file exceeds max_hash_bytes={max_hash_bytes}",
                        "stage": "hash",
                    })
            except OSError:
                content_hash = ""

        row = {
            "status": "present",
            "path": rel_path,
            "name": entry.name,
            "ext": ext,
            "mime": entry.mime,
            "size": fp["size"],
            "mtime": fp["mtime"],
            "mtime_ns": fp["mtime_ns"],
            "created": entry.created.isoformat(timespec="seconds"),
            "modified": entry.modified.isoformat(timespec="seconds"),
            "sha256": content_hash,
            "parser_required": parser_required,
            "parse_status": parse_status,
            "text_cache_path": text_cache_path,
            "doc_meta_path": doc_meta_path,
            "keywords": keywords,
            "path_terms": _tokens_from_text(rel_path, limit=24),
            "name_terms": _tokens_from_text(entry.name, limit=16),
            "folder_terms": _tokens_from_text(str(Path(rel_path).parent), limit=16),
            "semantic_hints": _semantic_hints(rel_path, entry.name, ext, keywords, summary),
            "summary": summary,
            "indexed_at": _now_iso(),
        }
        file_rows.append(row)
        if parser_required:
            doc_row = row | {"file_id": f"sha256:{content_hash}" if content_hash else ""}
            doc_rows.append(doc_row)
            if doc_meta_path:
                _write_json(root / doc_meta_path, _doc_meta_envelope(doc_row))

    # Keep deleted rows visible for agents/history, but current indexes list present first.
    file_rows_sorted = sorted(file_rows, key=lambda r: r["path"])
    folder_rows = _build_folder_rows(root, dirs, folder_file_counts, folder_size_counts, folder_ext_counts, folder_children)
    doc_rows_sorted = sorted(doc_rows, key=lambda r: r["path"])
    current_doc_hashes = {row["sha256"] for row in doc_rows_sorted if row.get("sha256")}
    _prune_stale_doc_artifacts(doc_text_dir, doc_meta_dir, current_doc_hashes)
    _remove_path_quietly(folder_cards_dir)
    _remove_path_quietly(file_cards_dir)
    map_artifacts = _build_map_artifacts(root, file_rows_sorted, folder_rows, doc_rows_sorted)

    if progress:
        progress("jikji: 인덱스/탐색 지도 작성", 0.82)
    _write_jsonl(index_dir / "file_index.jsonl", file_rows_sorted + deleted_rows)
    _write_jsonl(index_dir / "folder_index.jsonl", folder_rows)
    _write_jsonl(index_dir / "document_index.jsonl", doc_rows_sorted)
    _write_jsonl(index_dir / "parse_errors.jsonl", parse_errors)
    _write_jsonl(index_dir / "file_cards.jsonl", map_artifacts["file_cards"])
    _write_jsonl(index_dir / "chunk_map.jsonl", map_artifacts["chunk_map"])
    search_index_path = build_instant_search_index(
        index_dir,
        map_artifacts["file_cards"],
        map_artifacts["chunk_map"],
    )
    _write_jsonl(index_dir / "duplicate_map.jsonl", map_artifacts["duplicate_map"])
    _write_jsonl(index_dir / "folder_profile.jsonl", map_artifacts["folder_profile"])
    _write_json(index_dir / "corpus_profile.json", map_artifacts["corpus_profile"])
    _write_json(index_dir / "intent_taxonomy.json", {k: list(v) for k, v in INTENT_TAXONOMY.items()})

    search_terms = _build_search_terms(folder_rows, file_rows_sorted, doc_rows_sorted)
    _remove_path_quietly(index_dir / "search_terms.json")
    _remove_path_quietly(index_dir / "search_terms.jsonl")
    manifest = {
        "schema_version": 1,
        "search_index_schema_version": INSTANT_SEARCH_SCHEMA_VERSION,
        "generated_at": _now_iso(),
        "root": str(root),
        "files": len(file_rows_sorted),
        "folders": len(folder_rows),
        "documents": len(doc_rows_sorted),
        "docs_parsed": result.docs_parsed,
        "docs_reused": result.docs_reused,
        "docs_failed": result.docs_failed,
        "parse_errors": len(parse_errors),
        "file_cards": len(map_artifacts["file_cards"]),
        "chunks": len(map_artifacts["chunk_map"]),
        "duplicate_groups": len(map_artifacts["duplicate_map"]),
        "search_index": f"{AGENT_DIR_NAME}/{INSTANT_SEARCH_INDEX}",
        "search_index_bytes": search_index_path.stat().st_size if search_index_path.exists() else 0,
        "deleted_since_last_index": len(deleted_rows),
        "mode": "agent_search",
        "non_destructive": True,
        "cache_key_policy": CACHE_KEY_POLICY,
        "owned_paths": OWNED_GENERATED_PATHS,
        "retired_cleanup_paths": RETIRED_GENERATED_PATHS,
        "parser_required_extensions": sorted(DOCUMENT_CACHE_EXTENSIONS),
        "native_text_extensions": sorted(TEXT_LIKE_EXTENSIONS),
    }
    _write_json(index_dir / "manifest.json", manifest)
    _write_json(index_dir / "autorag_manifest.json", {
        "schema_version": 1,
        "generated_at": manifest["generated_at"],
        "root": str(root),
        "artifacts": {
            "file_cards": f"{AGENT_DIR_NAME}/file_cards.jsonl",
            "chunk_map": f"{AGENT_DIR_NAME}/chunk_map.jsonl",
            "instant_search_index": f"{AGENT_DIR_NAME}/{INSTANT_SEARCH_INDEX}",
            "duplicate_map": f"{AGENT_DIR_NAME}/duplicate_map.jsonl",
            "folder_profile": f"{AGENT_DIR_NAME}/folder_profile.jsonl",
            "corpus_profile": f"{AGENT_DIR_NAME}/corpus_profile.json",
            "document_index": f"{AGENT_DIR_NAME}/document_index.jsonl",
            "doc_text_dir": f"{AGENT_DIR_NAME}/doc_text",
        },
        "capabilities": {
            "has_doc_text": bool(doc_rows_sorted),
            "has_file_cards": bool(map_artifacts["file_cards"]),
            "has_chunk_map": bool(map_artifacts["chunk_map"]),
            "has_duplicate_map": bool(map_artifacts["duplicate_map"]),
            "has_intent_tags": True,
            "embedding_required": False,
            "rag_required": False,
        },
    })
    _atomic_write_text(index_dir / "agent_routes.md", _agent_routes_markdown(manifest))
    _atomic_write_text(index_dir / "agent_skill_context.md", _agent_skill_context_markdown(manifest))
    _atomic_write_text(index_dir / "human_guide.md", _human_guide_markdown(manifest))
    agent_map = index_dir / "agent_map.md"
    _atomic_write_text(agent_map, _agent_map_markdown(root, manifest, folder_rows, doc_rows_sorted, search_terms))
    result.agent_map = agent_map

    # Backwards/convenience visible root map. Keep short and overwrite safely.
    _atomic_write_text(root / "000_JIKJI_AGENT_MAP.md", _visible_agent_map(agent_map))
    if progress:
        progress(
            f"jikji: 완료 — 파일 {result.files}개 / 폴더 {result.folders}개 / 문서 캐시 신규 {result.docs_parsed}개·재사용 {result.docs_reused}개",
            0.98,
        )
    return result


def _prune_stale_doc_artifacts(
    doc_text_dir: Path,
    doc_meta_dir: Path,
    live_hashes: set[str],
) -> None:
    """Remove generated document caches no longer referenced by current docs."""
    live_names = {f"sha256_{h}" for h in live_hashes}
    for child in doc_text_dir.glob("sha256_*"):
        cache_key = child.name if child.is_dir() else child.stem
        if cache_key not in live_names:
            _remove_path_quietly(child)
    for child in doc_meta_dir.glob("sha256_*.json"):
        stem = child.stem
        if stem not in live_names:
            _remove_path_quietly(child)


def _doc_meta_envelope(row: dict) -> dict:
    file_id = row.get("file_id") or (f"sha256:{row.get('sha256')}" if row.get("sha256") else "")
    return {
        "schema_version": 1,
        "file_id": file_id,
        "path": row.get("path", ""),
        "title": "",
        "author": "",
        "subject": "",
        "created": row.get("created", ""),
        "modified": row.get("modified", ""),
        "page_count": None,
        "source": "jikji",
        "exif": {},
        "office": {},
        "parser": {
            "parse_status": row.get("parse_status", ""),
            "text_cache_path": row.get("text_cache_path", ""),
            "summary": row.get("summary", ""),
        },
    }


def _folder_id(path_rel: str) -> str:
    return "folder_" + hashlib.sha1(path_rel.encode("utf-8", "ignore")).hexdigest()[:12]


def _build_folder_rows(root, dirs, file_counts, size_counts, ext_counts, children) -> list[dict]:
    rows = []
    all_dirs = [root] + list(dirs)
    for d in all_dirs:
        rel_path = "." if d == root else _rel(root, d)
        child_names = children.get(rel_path, [])[:80]
        exts = dict(ext_counts.get(rel_path, Counter()).most_common(12))
        text = " ".join([d.name, rel_path, " ".join(child_names), " ".join(exts)])
        rows.append({
            "folder_id": _folder_id(rel_path),
            "path": rel_path,
            "name": d.name if d != root else root.name,
            "depth": 0 if rel_path == "." else len(Path(rel_path).parts),
            "file_count_direct": int(file_counts.get(rel_path, 0)),
            "subfolder_count_direct": len(children.get(rel_path, [])),
            "total_size_direct": int(size_counts.get(rel_path, 0)),
            "top_extensions_direct": exts,
            "child_folders": child_names,
            "keywords": _tokens_from_text(text),
            "summary": f"{rel_path} — 파일 {file_counts.get(rel_path, 0)}개, 하위 폴더 {len(children.get(rel_path, []))}개",
        })
    return sorted(rows, key=lambda r: (r["depth"], r["path"]))


def _build_search_terms(folder_rows, file_rows, doc_rows) -> dict:
    terms: dict[str, dict] = {}
    for kind, rows in (("folder", folder_rows), ("file", file_rows), ("document", doc_rows)):
        for row in rows:
            candidates = set(row.get("keywords") or [])
            candidates.update(_tokens_from_text(row.get("path", ""), limit=12))
            for term in candidates:
                bucket = terms.setdefault(term, {"folders": [], "files": [], "documents": []})
                key = kind + "s"
                if row.get("path") not in bucket[key]:
                    bucket[key].append(row.get("path"))
                    bucket[key] = bucket[key][:40]
    return {k: terms[k] for k in sorted(terms)}

def _agent_map_markdown(root, manifest, folders, docs, terms) -> str:
    top_folders = [r for r in folders if r.get("depth") == 1][:40]
    top_docs = docs[:40]
    top_terms = list(terms.keys())[:80]
    lines = [
        "# Jikji Agent Map",
        "",
        "이 폴더는 Jikji가 원본 구조를 변경하지 않고 에이전트/사람 탐색용 메타데이터를 생성한 상태입니다.",
        "",
        "## 빠른 사용법",
        "- 전체 폴더 메타: `.jikji/folder_index.jsonl`",
        "- 전체 파일 메타: `.jikji/file_index.jsonl`",
        "- 파싱 문서 본문 캐시: `.jikji/doc_text/`",
        "- 문서 인덱스: `.jikji/document_index.jsonl`",
        "- 파일별 탐색 카드: `.jikji/file_cards.jsonl`",
        "- 문서 chunk 탐색 지도: `.jikji/chunk_map.jsonl`",
        "- 중복/사본 그룹: `.jikji/duplicate_map.jsonl`",
        "- AutoRAG 연동 계약: `.jikji/autorag_manifest.json`",
        "- CLI 검색 예: `rg \"검색어\" .jikji/doc_text .jikji/*.jsonl`",
        "",
        "## 요약",
        f"- 루트: `{root}`",
        f"- 파일: {manifest['files']}개",
        f"- 폴더: {manifest['folders']}개",
        f"- 파서 필요 문서: {manifest['documents']}개",
        f"- 문서 캐시 신규/재사용/실패: {manifest['docs_parsed']} / {manifest['docs_reused']} / {manifest['docs_failed']}",
        f"- 파서/메타 경고: {manifest.get('parse_errors', 0)}개",
        "",
        "## 에이전트 검색 규칙",
        "- PDF/Office/HWP/RTF 등 파서 필요 문서 본문: `.jikji/doc_text/`에서 `rg`",
        "- 자연어 단서 후보 추리: `.jikji/file_cards.jsonl`과 `.jikji/chunk_map.jsonl` 우선 확인",
        "- 중복/백업 사본 판단: `.jikji/duplicate_map.jsonl` 확인",
        "- txt/md/csv/json/log 등 텍스트형 파일: 원본 폴더에서 `.jikji`를 제외하고 `rg`",
        "- 최종 파일 접근은 JSONL의 `path` 필드가 가리키는 원본 경로 사용",
        "",
        "## 최상위 폴더",
    ]
    lines.extend(f"- `{r['path']}` — {r['summary']}" for r in top_folders)
    lines.extend(["", "## 문서 텍스트 캐시 후보"])
    lines.extend(f"- `{r['path']}` → `{r.get('text_cache_path') or '캐시 없음'}`" for r in top_docs)
    lines.extend(["", "## 주요 검색 토큰"])
    lines.append(", ".join(top_terms) if top_terms else "—")
    lines.append("")
    return "\n".join(lines)


def _visible_agent_map(agent_map: Path) -> str:
    return (
        "# Jikji Agent Map\n\n"
        "상세 탐색 지도와 파일/문서 인덱스는 아래 경로에 있습니다.\n\n"
        f"- `{agent_map.as_posix()}`\n"
        "- `.jikji/file_index.jsonl`\n"
        "- `.jikji/folder_index.jsonl`\n"
        "- `.jikji/document_index.jsonl`\n"
        "- `.jikji/file_cards.jsonl`\n"
        "- `.jikji/chunk_map.jsonl`\n"
        "- `.jikji/duplicate_map.jsonl`\n"
        "- `.jikji/autorag_manifest.json`\n"
        "- `.jikji/doc_text/`\n"
    )


def _agent_routes_markdown(manifest) -> str:
    return (
        "# Jikji Agent Routes\n\n"
        "1. 먼저 `.jikji/agent_map.md`를 읽는다.\n"
        "2. 자연어/내용 단서 후보는 `.jikji/file_cards.jsonl`와 `.jikji/chunk_map.jsonl`에서 먼저 찾는다.\n"
        "3. 폴더 후보는 `.jikji/folder_index.jsonl` 또는 `.jikji/folder_profile.jsonl`에서 찾는다.\n"
        "4. 파일 후보는 `.jikji/file_index.jsonl`에서 찾는다.\n"
        "5. 중복/백업 사본은 `.jikji/duplicate_map.jsonl`에서 확인한다.\n"
        "6. PDF/Office/HWP 문서 본문은 `.jikji/doc_text/`에서 `rg`로 검색한다.\n"
        "7. txt/md/csv/json/log 같은 텍스트형 파일은 원본 폴더에서 `.jikji`를 제외하고 검색한다.\n"
        "8. 최종 접근은 `path` 필드의 원본 파일 경로를 사용한다.\n"
        "9. 검색 작업 중 원본 파일을 이동/이름변경/삭제하지 않는다.\n\n"
        f"생성 시각: {manifest['generated_at']}\n"
        f"캐시 정책: {manifest.get('cache_key_policy', CACHE_KEY_POLICY)}\n"
    )


def _agent_skill_context_markdown(manifest) -> str:
    return (
        "# Jikji Skill Context\n\n"
        "Jikji는 검색기가 아니라 로컬 에이전트가 CLI에서 파일 시스템을 잘 찾도록 준비하는 도구입니다.\n"
        "이 인덱스는 비파괴적으로 생성되었으며 원본 폴더/파일명은 변경하지 않았습니다.\n\n"
        "검색 규칙: 자연어 후보는 `.jikji/file_cards.jsonl`/`.jikji/chunk_map.jsonl`, 파서 필요 문서는 `.jikji/doc_text/`, 텍스트형 파일은 원본 경로를 검색하세요.\n\n"
        "## Read first\n"
        "- `.jikji/agent_map.md`\n"
        "- `.jikji/agent_routes.md`\n"
        "- `.jikji/file_index.jsonl`\n"
        "- `.jikji/document_index.jsonl`\n"
        "- `.jikji/file_cards.jsonl`\n"
        "- `.jikji/chunk_map.jsonl`\n"
        "- `.jikji/duplicate_map.jsonl`\n"
        "- `.jikji/autorag_manifest.json`\n"
    )


def _human_guide_markdown(manifest) -> str:
    return (
        "# Jikji Human Guide\n\n"
        "기존 폴더와 파일은 이동/변경하지 않았습니다. `.jikji/` 아래에 탐색용 지도와 인덱스만 생성했습니다.\n\n"
        "주의: `.jikji/doc_text/`에는 원본 문서에서 추출된 민감한 텍스트가 포함될 수 있습니다. Git 커밋 전에 검토하세요.\n\n"
        f"- 파일: {manifest['files']}개\n"
        f"- 폴더: {manifest['folders']}개\n"
        f"- 문서 텍스트 캐시: {manifest['documents']}개 대상\n"
    )
