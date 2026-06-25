from __future__ import annotations

# SIZE_OK: legacy CLI command registry; this slice only wires existing focused modules into CLI flags/output.
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

from .agent_brief import (
    brief_markdown,
    build_agent_brief_payload,
    build_compact_agent_brief_payload,
)
from .agent_index import (
    AGENT_DIR_NAME,
    OWNED_GENERATED_PATHS,
    RETIRED_GENERATED_PATHS,
    VISIBLE_MAP_NAME,
    VISIBLE_MAP_NAMES,
    build_agent_index,
    strip_agent_rules_files,
    tree_signature,
)
from .agent_skill_install import (
    CUSTOM_AGENT_NAMES,
    expand_agent_selection,
    install_agent_skill,
    repo_skill_path,
)
from .beir import materialize_beir_dataset, run_beir_suite
from .benchmark_two_call import write_two_call_value_report
from .benchmark_value import Pricing
from .config import Config
from .discover import discover
from .edith import edith_answer_summary, materialize_edith_dataset, run_edith_suite
from .eval import (
    analyze_eval_failures,
    generate_eval_set,
    generate_realistic_eval_set,
    run_eval,
    search,
)
from .graph_query import explain_source, graph_status, query_graph_routes
from .gui import serve_gui
from .hardbench import build_hard_benchmark, run_hard_benchmark_suite
from .hermes_bench import install_hermes_skill, run_hermes_benchmark
from .hermes_compare import compare_benchmark_reports
from .hippocamp import fetch_subset, import_eval_set, run_benchmark, run_suite
from .holdout_eval import generate_holdout_eval_set
from .improvement_loop import run_improvement_loop
from .parsers.media import image_ocr_available, transcription_available
from .publicdata_bench import build_publicdata_benchmark, run_publicdata_suite
from .scanner import ScanTooLargeError
from .search_index import instant_index_path
from .version import __version__
from .workspacebench import (
    DEFAULT_MAX_FILE_BYTES,
    DEFAULT_MAX_TOTAL_BYTES,
    build_workspacebench_benchmark,
    run_workspacebench_suite,
)

_WIKI_SOURCE_PAGE_RE = re.compile(r"^[0-9A-Za-z가-힣_-]+-[0-9a-f]{12}\.md$")
_DOC_CACHE_FILE_RE = re.compile(r"^sha256_[0-9a-f]{64}\.txt$")
_DOC_CACHE_DIR_RE = re.compile(r"^sha256_[0-9a-f]{64}$")
_DOC_META_FILE_RE = re.compile(r"^sha256_[0-9a-f]{64}\.json$")
_POST_INSTALL_DOCUMENT_EXTS = {
    ".pdf",
    ".hwp",
    ".hwpx",
    ".doc",
    ".docx",
    ".xls",
    ".xlsx",
    ".ppt",
    ".pptx",
    ".rtf",
    ".odt",
    ".ods",
    ".odp",
}
_POST_INSTALL_SKIP_DIRS = {
    ".cache",
    ".cargo",
    ".git",
    ".gradle",
    ".local",
    ".npm",
    ".rustup",
    ".venv",
    "__pycache__",
    "AppData",
    "Library",
    "node_modules",
    "site-packages",
    "venv",
}
_POST_INSTALL_COMMON_RELS = (
    "Documents",
    "Downloads",
    "Desktop",
    "문서",
    "다운로드",
    "바탕화면",
    "데스크탑",
    "OneDrive/Documents",
    "OneDrive/문서",
    "Google Drive",
    "Dropbox",
    "iCloud Drive",
    "Downloads/Telegram Desktop",
    "Downloads/KakaoTalk Downloads",
    "Downloads/카카오톡 받은 파일",
    "Documents/KakaoTalk Downloads",
    "Documents/카카오톡 받은 파일",
)


def _config_from_args(args) -> Config:
    cfg = Config()
    cfg.max_files = args.max_files
    cfg.include_hidden = args.include_hidden
    cfg.include_sensitive = args.include_sensitive
    cfg.parse_timeout_s = args.parse_timeout
    cfg.agent_doc_text_max_chars = args.doc_text_max_chars
    cfg.agent_doc_text_chunk_chars = args.doc_text_chunk_chars
    cfg.max_hash_bytes = args.max_hash_bytes
    cfg.enable_media_index = bool(getattr(args, "enable_media_index", False))
    cfg.media_index_max_mb = float(getattr(args, "media_index_max_mb", 25.0) or 25.0)
    if args.exclude:
        cfg.ignore_patterns.extend(args.exclude)
    cfg.write_agent_rules = not getattr(args, "no_agent_rules", False)
    return cfg


def cmd_prepare(args) -> int:
    root = Path(args.path).expanduser().resolve()
    cfg = _config_from_args(args)
    if not cfg.enable_media_index:
        media_enabled, media_max_mb = _existing_media_prepare_policy(root)
        if media_enabled:
            cfg.enable_media_index = True
            if media_max_mb is not None:
                cfg.media_index_max_mb = media_max_mb
    result = build_agent_index(root, cfg)
    if args.json:
        print(json.dumps({
            "root": str(root),
            "index_dir": str(result.index_dir),
            "agent_map": str(result.agent_map),
            "files": result.files,
            "folders": result.folders,
            "docs_parsed": result.docs_parsed,
            "docs_reused": result.docs_reused,
            "docs_failed": result.docs_failed,
            "deleted": result.deleted,
        }, ensure_ascii=False, indent=2))
    else:
        print(f"Jikji prepared: {root}")
        print(f"- files={result.files} folders={result.folders} deleted={result.deleted}")
        print(f"- docs parsed/reused/failed={result.docs_parsed}/{result.docs_reused}/{result.docs_failed}")
        print(f"- map={result.agent_map}")
    return 0


def _clean_manifest_paths() -> list[str]:
    return [*OWNED_GENERATED_PATHS, *RETIRED_GENERATED_PATHS]


def _clean_targets(root: Path) -> list[Path]:
    clean_root = root.resolve()
    targets: list[Path] = []
    cleanup_dirs: list[Path] = []
    for raw_path in _clean_manifest_paths():
        rel_path = _safe_clean_rel_path(raw_path)
        if not rel_path:
            continue
        path = root / rel_path
        if not _is_path_under_root(path, clean_root):
            continue
        if raw_path.endswith("/"):
            targets.extend(_generated_directory_targets(path, rel_path, clean_root))
            cleanup_dirs.append(path)
        elif path.exists():
            targets.append(path)
    targets.extend(_clean_recorded_generated_paths(root, clean_root))
    cleanup_dirs.append(root / AGENT_DIR_NAME)
    return _dedupe_clean_targets([*targets, *_empty_directories(cleanup_dirs, clean_root)])


def _safe_clean_rel_path(raw_path: str) -> str:
    rel_path = raw_path.rstrip("/")
    path = Path(rel_path)
    if path.is_absolute() or ".." in path.parts:
        return ""
    return rel_path


def _generated_directory_targets(path: Path, rel_path: str, clean_root: Path) -> list[Path]:
    if not path.exists() or not path.is_dir():
        return []
    match rel_path:
        case ".jikji/wiki":
            return [candidate for candidate in [path / "index.md"] if candidate.exists() and _is_path_under_root(candidate, clean_root)]
        case ".jikji/doc_text" | ".jikji/doc_meta":
            return []
        case ".jikji/folder_cards" | ".jikji/file_cards":
            return []
        case ".jikji/eval":
            return []
        case _:
            return []


def _clean_recorded_generated_paths(root: Path, clean_root: Path) -> list[Path]:
    targets: list[Path] = []
    for rel_path in _read_generated_rel_paths(root / AGENT_DIR_NAME / "graph_routes.jsonl", clean_root):
        path = root / rel_path
        if _is_path_under_root(path, clean_root) and path.is_file() and _is_generated_recorded_file(path, "wiki_path"):
            targets.append(path)
    for rel_path in _read_generated_rel_paths(root / AGENT_DIR_NAME / "document_index.jsonl", clean_root):
        path = root / rel_path
        if not _is_path_under_root(path, clean_root):
            continue
        if path.is_file() and _is_generated_recorded_file(path, "text_cache_path"):
            targets.append(path)
        elif path.is_dir() and Path(rel_path).parent.as_posix() == f"{AGENT_DIR_NAME}/doc_text":
            targets.extend(
                child
                for child in path.glob("chunk_*.txt")
                if _is_path_under_root(child, clean_root)
                and child.is_file()
                and _is_generated_recorded_file(child, "text_cache_path")
            )
        if path.is_file() and _is_generated_recorded_file(path, "doc_meta_path"):
            targets.append(path)
    return targets


def _read_generated_rel_paths(path: Path, clean_root: Path) -> list[str]:
    rel_paths: list[str] = []
    if not _is_path_under_root(path, clean_root):
        return rel_paths
    try:
        lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    except OSError:
        return rel_paths
    for line in lines:
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(row, dict):
            continue
        for key in ("wiki_path", "text_cache_path", "doc_meta_path", "meta_cache_path"):
            value = row.get(key)
            if not isinstance(value, str) or not value.startswith(f"{AGENT_DIR_NAME}/"):
                continue
            rel_path = _safe_clean_rel_path(value)
            if rel_path and _matches_recorded_generated_pattern(key, rel_path):
                rel_paths.append(rel_path)
    return rel_paths


def _matches_recorded_generated_pattern(key: str, rel_path: str) -> bool:
    path = Path(rel_path)
    match key:
        case "wiki_path":
            return (
                path.parent.as_posix() == f"{AGENT_DIR_NAME}/wiki/sources"
                and _WIKI_SOURCE_PAGE_RE.fullmatch(path.name) is not None
            )
        case "text_cache_path":
            return (
                path.parent.as_posix() == f"{AGENT_DIR_NAME}/doc_text"
                and (_DOC_CACHE_FILE_RE.fullmatch(path.name) is not None or _DOC_CACHE_DIR_RE.fullmatch(path.name) is not None)
            )
        case "doc_meta_path" | "meta_cache_path":
            return (
                path.parent.as_posix() == f"{AGENT_DIR_NAME}/doc_meta"
                and _DOC_META_FILE_RE.fullmatch(path.name) is not None
            )
        case _:
            return False


def _is_generated_recorded_file(path: Path, key: str) -> bool:
    match key:
        case "wiki_path":
            try:
                head = path.read_text(encoding="utf-8", errors="ignore")[:512]
            except OSError:
                return False
            try:
                wiki_rel = path.relative_to(path.parents[2]).as_posix()
            except ValueError:
                return False
            return (
                "schema: jikji.llm_wiki.source.v1" in head
                and f"wiki_path: {json.dumps(wiki_rel, ensure_ascii=False)}" in head
            )
        case "text_cache_path":
            try:
                head = path.read_text(encoding="utf-8", errors="ignore")[:256]
            except OSError:
                return False
            digest = _recorded_doc_digest(path)
            return (
                digest
                and "# Source:" in head
                and f"# File ID: sha256:{digest}" in head
                and "# Parsed by: Jikji" in head
            )
        case "doc_meta_path" | "meta_cache_path":
            try:
                payload = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                return False
            digest = _recorded_doc_digest(path)
            return (
                isinstance(payload, dict)
                and payload.get("schema_version") == 1
                and payload.get("source") == "jikji"
                and bool(digest)
                and payload.get("file_id") == f"sha256:{digest}"
            )
        case _:
            return False


def _recorded_doc_digest(path: Path) -> str:
    if path.name.startswith("chunk_") and path.parent.name.startswith("sha256_"):
        stem = path.parent.name
    elif path.suffix == ".txt":
        stem = path.stem
    elif path.suffix == ".json":
        stem = path.stem
    else:
        stem = path.name
    if not stem.startswith("sha256_"):
        return ""
    digest = stem.removeprefix("sha256_")
    return digest if re.fullmatch(r"[0-9a-f]{64}", digest) else ""


def _empty_directories(paths: list[Path], clean_root: Path) -> list[Path]:
    empty: list[Path] = []
    for path in sorted(paths, key=lambda item: len(item.parts), reverse=True):
        if _is_path_under_root(path, clean_root) and path.is_dir() and not any(path.iterdir()):
            empty.append(path)
    return empty


def _is_path_under_root(path: Path, root: Path) -> bool:
    try:
        resolved = path.resolve()
    except OSError:
        return False
    return resolved == root or root in resolved.parents


def _dedupe_clean_targets(paths: list[Path]) -> list[Path]:
    targets: list[Path] = []
    seen: set[Path] = set()
    for path in paths:
        try:
            resolved = path.resolve()
        except OSError:
            resolved = path
        if resolved in seen:
            continue
        seen.add(resolved)
        targets.append(path)
    return targets


def _read_manifest_for_clean(root: Path) -> dict:
    path = root / AGENT_DIR_NAME / "manifest.json"
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return raw if isinstance(raw, dict) else {}


def _clean_allowed(root: Path, *, force: bool) -> tuple[bool, str]:
    if force:
        return True, "force"
    manifest = _read_manifest_for_clean(root)
    if not manifest:
        return False, "missing_manifest"
    manifest_root = str(manifest.get("root") or "")
    try:
        same_root = Path(manifest_root).expanduser().resolve() == root
    except (OSError, RuntimeError, ValueError):
        same_root = False
    if manifest.get("non_destructive") is True and same_root:
        return True, "manifest_verified"
    return False, "manifest_mismatch"


def cmd_clean(args) -> int:
    """Remove only Jikji-owned generated artifacts for one prepared root."""
    root = Path(args.path).expanduser().resolve()
    allowed, reason = _clean_allowed(root, force=args.force)
    targets = _clean_targets(root)
    existing = [p for p in targets if p.exists()]
    has_agent_dir = (root / AGENT_DIR_NAME).exists()
    rules_preview = strip_agent_rules_files(root, dry_run=True)
    rules_block_paths = rules_preview["edited"] + rules_preview["removed"]
    payload = {
        "root": str(root),
        "ok": allowed or (not existing and not has_agent_dir and not rules_block_paths),
        "reason": reason,
        "dry_run": args.dry_run,
        "removed": [],
        "would_remove": [str(p) for p in existing] + rules_preview["removed"],
        "agent_rules_edited": rules_preview["edited"],
        "preserved_original_files": True,
    }
    if (existing or has_agent_dir or rules_block_paths) and not allowed:
        payload["error"] = (
            f"Refusing to remove {root / AGENT_DIR_NAME} without a verified Jikji manifest. "
            "Use --force only if this directory is known to be Jikji-generated."
        )
        if args.json:
            print(json.dumps(payload, ensure_ascii=False, indent=2))
        else:
            print(payload["error"], file=sys.stderr)
            for p in existing:
                print(f"WOULD_REMOVE {p}")
        return 1
    if not args.dry_run:
        removed: list[str] = []
        clean_root = root.resolve()
        for p in existing:
            if not _is_path_under_root(p, clean_root):
                payload.setdefault("errors", []).append({"path": str(p), "error": "outside_root"})
                continue
            try:
                if p.is_dir():
                    shutil.rmtree(p)
                else:
                    p.unlink()
                removed.append(str(p))
            except OSError as exc:
                payload.setdefault("errors", []).append({"path": str(p), "error": str(exc)})
        rules_applied = strip_agent_rules_files(root, dry_run=False)
        removed.extend(rules_applied["removed"])
        payload["agent_rules_edited"] = rules_applied["edited"]
        payload["removed"] = removed
        payload["ok"] = not payload.get("errors")
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        label = "WOULD_REMOVE" if args.dry_run else "REMOVED"
        for p in existing:
            print(f"{label} {p}")
        if not existing:
            print(f"No Jikji artifacts found under {root}")
    return 0 if payload["ok"] else 1


def cmd_map(args) -> int:
    root = Path(args.path).expanduser().resolve()
    candidates = [root / name for name in VISIBLE_MAP_NAMES]
    candidates.append(root / ".jikji" / "agent_map.md")
    for candidate in candidates:
        if candidate.exists():
            print(candidate.read_text(encoding="utf-8", errors="ignore")[: args.max_chars])
            return 0
    print(f"No Jikji map found under {root}. Run: jikji prepare {root}")
    return 1


def cmd_doctor(args) -> int:
    root = Path(args.path).expanduser().resolve()
    errors: list[str] = []
    warnings: list[str] = []

    def check_file(rel: str) -> None:
        p = root / rel
        if not p.exists():
            errors.append(f"missing required artifact: {rel}")
        elif not p.is_file():
            errors.append(f"required artifact is not a file: {rel}")

    required = [
        ".jikji/manifest.json",
        ".jikji/file_index.jsonl",
        ".jikji/folder_index.jsonl",
        ".jikji/document_index.jsonl",
        ".jikji/file_cards.jsonl",
        ".jikji/chunk_map.jsonl",
        ".jikji/search_index.sqlite",
        ".jikji/duplicate_map.jsonl",
        ".jikji/folder_profile.jsonl",
        ".jikji/corpus_profile.json",
        ".jikji/intent_taxonomy.json",
        ".jikji/autorag_manifest.json",
        ".jikji/knowledge_graph.json",
        ".jikji/graph_routes.jsonl",
        ".jikji/llm_wiki_schema.md",
        ".jikji/wiki/index.md",
        ".jikji/parse_errors.jsonl",
        ".jikji/agent_map.md",
    ]
    for rel in required:
        check_file(rel)
    if not any((root / name).is_file() for name in VISIBLE_MAP_NAMES):
        errors.append(f"missing required artifact: {VISIBLE_MAP_NAME}")

    manifest = {}
    manifest_path = root / ".jikji" / "manifest.json"
    if manifest_path.exists():
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            if manifest.get("schema_version") != 1:
                errors.append(f"unsupported schema_version: {manifest.get('schema_version')!r}")
            if manifest.get("non_destructive") is not True:
                errors.append("manifest non_destructive must be true")
        except json.JSONDecodeError as exc:
            errors.append(f"manifest is not valid JSON: {exc}")

    def read_jsonl(rel: str) -> list[dict]:
        path = root / rel
        rows: list[dict] = []
        if not path.exists():
            return rows
        for lineno, line in enumerate(path.read_text(encoding="utf-8", errors="ignore").split("\n"), 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as exc:
                errors.append(f"{rel}:{lineno} invalid JSON: {exc}")
                continue
            if not isinstance(row, dict):
                errors.append(f"{rel}:{lineno} row is not an object")
                continue
            rows.append(row)
        return rows

    for rel in (
        ".jikji/file_index.jsonl",
        ".jikji/folder_index.jsonl",
        ".jikji/file_cards.jsonl",
        ".jikji/chunk_map.jsonl",
        ".jikji/duplicate_map.jsonl",
        ".jikji/folder_profile.jsonl",
        ".jikji/parse_errors.jsonl",
    ):
        read_jsonl(rel)

    doc_rows = read_jsonl(".jikji/document_index.jsonl")
    image_doc_count = sum(
        1
        for row in doc_rows
        if str(row.get("ext") or "").lower()
        in {".png", ".jpg", ".jpeg", ".tif", ".tiff", ".webp", ".bmp", ".gif"}
    )
    live_hashes: set[str] = set()
    for row in doc_rows:
        sha = row.get("sha256") or ""
        if sha:
            live_hashes.add(f"sha256_{sha}")
        cache = row.get("text_cache_path") or ""
        status = row.get("parse_status")
        if status in {"success", "archive_listing"}:
            if not cache:
                errors.append(f"document success without text_cache_path: {row.get('path')}")
            elif not (root / cache).exists():
                errors.append(f"missing text cache for {row.get('path')}: {cache}")
        meta = row.get("doc_meta_path") or ""
        if meta and not (root / meta).exists():
            warnings.append(f"missing doc_meta for {row.get('path')}: {meta}")

    for base in (root / ".jikji" / "doc_text", root / ".jikji" / "doc_meta"):
        if not base.exists():
            continue
        for child in base.glob("sha256_*"):
            key = child.name if child.is_dir() else child.stem
            if key not in live_hashes:
                warnings.append(f"dangling generated artifact: {child.relative_to(root).as_posix()}")

    media_manifest = manifest.get("media_index") if isinstance(manifest.get("media_index"), dict) else {}
    tesseract_path = shutil.which("tesseract") or ""
    ocr_available = image_ocr_available()
    image_support = {
        "metadata_indexing": True,
        "ocr_active": ocr_available,
        "ocr_available": ocr_available,
        "ocr_activated": bool(media_manifest.get("enabled")) and ocr_available,
        "tesseract_path": tesseract_path,
        "indexed_image_documents": image_doc_count,
    }
    media_support = {
        "enabled": bool(media_manifest.get("enabled")),
        "status": media_manifest.get("status", "unknown"),
        "max_mb": media_manifest.get("max_mb"),
        "media_files": media_manifest.get("media_files", 0),
        "image_ocr_available": ocr_available,
        "audio_video_transcription_available": transcription_available(),
        "opt_in_flag": "--enable-media-index",
    }

    report = {
        "root": str(root),
        "ok": not errors,
        "warnings": warnings,
        "errors": errors,
        "manifest": {
            "schema_version": manifest.get("schema_version"),
            "search_index_schema_version": manifest.get("search_index_schema_version"),
            "files": manifest.get("files"),
            "folders": manifest.get("folders"),
            "documents": manifest.get("documents"),
            "parse_errors": manifest.get("parse_errors"),
            "media_index": media_manifest,
        },
        "image_support": image_support,
        "media_support": media_support,
    }
    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    else:
        for rel in required:
            print(("OK   " if (root / rel).is_file() else "MISS ") + str(root / rel))
        for warning in warnings:
            print(f"WARN {warning}")
        for error in errors:
            print(f"ERR  {error}")
        print("INFO image metadata indexing: active")
        print(
            "INFO image OCR backend availability: "
            + ("available" if image_support["ocr_available"] else "unavailable")
        )
    if errors:
        return 1
    if warnings:
        return 2
    return 0


def cmd_eval_generate(args) -> int:
    root = Path(args.path).expanduser().resolve()
    result = generate_eval_set(root, max_cases=args.cases)
    payload = {
        "root": str(root),
        "eval_set": str(result.eval_set_path),
        "cases": result.cases,
        "scenarios": result.scenarios or {},
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Jikji eval set generated: {result.eval_set_path}")
        print(f"- cases={result.cases} scenarios={result.scenarios or {}}")
    return 0


def cmd_eval_generate_realistic(args) -> int:
    root = Path(args.path).expanduser().resolve()
    out = Path(args.out).expanduser().resolve() if args.out else None
    result = generate_realistic_eval_set(root, max_cases=args.cases, out=out)
    payload = {
        "root": str(root),
        "eval_set": str(result.eval_set_path),
        "cases": result.cases,
        "scenarios": result.scenarios or {},
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Jikji realistic eval set generated: {result.eval_set_path}")
        print(f"- cases={result.cases} scenarios={result.scenarios or {}}")
    return 0


def cmd_eval_generate_holdout(args) -> int:
    root = Path(args.path).expanduser().resolve()
    out = Path(args.out).expanduser().resolve() if args.out else None
    result = generate_holdout_eval_set(root, max_cases=args.cases, out=out, seed=args.seed)
    payload = {
        "root": str(root),
        "eval_set": str(result.eval_set_path),
        "profile": str(result.profile_path),
        "sha256": result.checksum,
        "cases": result.cases,
        "scenarios": result.scenarios,
        "locked_holdout": True,
        "anti_overfit": "Do not inspect individual cases or tune retrieval against this set; use only for frozen final/regression evaluation.",
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Jikji locked holdout eval set generated: {result.eval_set_path}")
        print(f"- profile={result.profile_path}")
        print(f"- sha256={result.checksum}")
        print(f"- cases={result.cases} scenarios={result.scenarios}")
        print("- protocol: do not inspect cases or tune retrieval against this set")
    return 0


def cmd_eval(args) -> int:
    root = Path(args.path).expanduser().resolve()
    eval_set = Path(args.eval_set).expanduser().resolve() if args.eval_set else None
    result = run_eval(root, eval_set=eval_set, top_k=args.top_k)
    payload = {
        "root": str(root),
        "eval_set": str(result.eval_set_path),
        "report": str(result.report_path),
        "metrics": result.metrics or {},
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        metrics = result.metrics or {}
        print(f"Jikji eval complete: {result.report_path}")
        print(
            f"- cases={metrics.get('cases')} hit@1={metrics.get('hit_at_1')} "
            f"hit@5={metrics.get('hit_at_5')} hit@10={metrics.get('hit_at_10')} "
            f"dup@10={metrics.get('duplicate_or_exact_hit_at_10')} mrr={metrics.get('mrr')}"
        )
    return 0


def cmd_bench_analyze(args) -> int:
    root = Path(args.path).expanduser().resolve()
    eval_set = Path(args.eval_set).expanduser().resolve() if args.eval_set else None
    result = analyze_eval_failures(root, eval_set=eval_set, top_k=args.top_k)
    payload = {
        "root": str(root),
        "analysis": str(result.analysis_path),
        "cases": result.cases,
        "summary": result.summary,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Jikji benchmark analysis complete: {result.analysis_path}")
        print(json.dumps(result.summary, ensure_ascii=False, indent=2))
    return 0


def _search_config_from_args(args) -> Config:
    cfg = Config()
    cfg.max_files = args.max_files
    cfg.include_hidden = args.include_hidden
    cfg.include_sensitive = args.include_sensitive
    cfg.parse_timeout_s = args.parse_timeout
    cfg.max_hash_bytes = args.max_hash_bytes
    if args.exclude:
        cfg.ignore_patterns.extend(args.exclude)
    return cfg


def _read_manifest(root: Path) -> dict:
    path = root / AGENT_DIR_NAME / "manifest.json"
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return raw if isinstance(raw, dict) else {}


def _manifest_generated_at(manifest: dict) -> datetime | None:
    try:
        value = str(manifest.get("generated_at") or "")
        if not value:
            return None
        if value.endswith("Z"):
            value = value[:-1] + "+00:00"
        generated = datetime.fromisoformat(value)
        if generated.tzinfo is None:
            generated = generated.replace(tzinfo=UTC)
        return generated
    except (ValueError, TypeError):
        return None


def _stored_tree_signature(manifest: dict) -> dict:
    value = manifest.get("source_tree_signature")
    return value if isinstance(value, dict) else {}


def _tree_signature_changed(root: Path, args, manifest: dict) -> bool:
    stored = _stored_tree_signature(manifest)
    if not stored.get("digest"):
        return False
    try:
        current = tree_signature(root, _search_config_from_args(args))
    except (OSError, RuntimeError):
        return False
    return str(current.get("digest") or "") != str(stored.get("digest") or "")


def _search_index_status(root: Path, args, *, stale_after_seconds: int, detect_changes: bool = True) -> tuple[str, bool]:
    index = instant_index_path(root)
    if not index.exists():
        return "missing", True
    manifest = _read_manifest(root)
    if detect_changes and _tree_signature_changed(root, args, manifest):
        return "changed_using_previous_index", False
    generated_at = _manifest_generated_at(manifest)
    if generated_at is None:
        try:
            age = time.time() - index.stat().st_mtime
        except OSError:
            return "missing", True
    else:
        age = (datetime.now(tz=UTC) - generated_at).total_seconds()
    if stale_after_seconds >= 0 and age >= stale_after_seconds:
        return "stale_using_previous_index", False
    return "ready", False


def _existing_media_prepare_policy(root: Path) -> tuple[bool, float | None]:
    media = _read_manifest(root).get("media_index")
    if not isinstance(media, dict) or media.get("enabled") is not True:
        return False, None
    raw_max_mb = media.get("max_mb")
    if isinstance(raw_max_mb, int | float) and raw_max_mb > 0:
        return True, float(raw_max_mb)
    return True, None


def _search_prepare_config(args, root: Path) -> Config:
    cfg = _search_config_from_args(args)
    media_enabled, media_max_mb = _existing_media_prepare_policy(root)
    if media_enabled:
        cfg.enable_media_index = True
        if media_max_mb is not None:
            cfg.media_index_max_mb = media_max_mb
    return cfg


def _prepare_args_for_search(args, root: Path) -> list[str]:
    cmd = [
        sys.executable,
        "-m",
        "jikji.__main__",
        "prepare",
        str(root),
        "--max-files",
        str(args.max_files),
        "--parse-timeout",
        str(args.parse_timeout),
        "--max-hash-bytes",
        str(args.max_hash_bytes),
        "--json",
    ]
    if args.include_hidden:
        cmd.append("--include-hidden")
    if args.include_sensitive:
        cmd.append("--include-sensitive")
    for pattern in args.exclude or []:
        cmd.extend(["--exclude", pattern])
    media_enabled, media_max_mb = _existing_media_prepare_policy(root)
    if media_enabled:
        cmd.append("--enable-media-index")
        if media_max_mb is not None:
            cmd.extend(["--media-index-max-mb", str(media_max_mb)])
    return cmd


def _maybe_start_background_refresh(args, root: Path) -> bool:
    index_dir = root / AGENT_DIR_NAME
    if not args.background_refresh or (index_dir / ".lock").exists():
        return False
    try:
        index_dir.mkdir(exist_ok=True)
        log = (index_dir / "background_refresh.log").open("ab")
        subprocess.Popen(  # noqa: S603 - command is current Python module with explicit root argument
            _prepare_args_for_search(args, root),
            cwd=str(Path.cwd()),
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            close_fds=True,
        )
        log.close()
        return True
    except OSError:
        return False


def cmd_search(args) -> int:
    root = Path(args.path).expanduser().resolve()
    index_status, should_prepare = _search_index_status(root, args, stale_after_seconds=args.stale_after_seconds)
    foreground_prepared = False
    if args.fresh or (should_prepare and args.auto_prepare):
        build_agent_index(root, _search_prepare_config(args, root))
        index_status = "prepared_now" if should_prepare else "refreshed_now"
        foreground_prepared = True
    elif should_prepare and not args.auto_prepare:
        print(f"No Jikji search index found under {root}. Run: jikji prepare {root}", file=sys.stderr)
        return 1

    ranked = search(root, args.query, top_k=args.top_k)
    background_refresh_started = False
    if index_status in {"stale_using_previous_index", "changed_using_previous_index"} and not args.fresh:
        background_refresh_started = _maybe_start_background_refresh(args, root)
    payload = {
        "root": str(root),
        "query": args.query,
        "top_k": args.top_k,
        "index_status": index_status,
        "foreground_prepared": foreground_prepared,
        "background_refresh_started": background_refresh_started,
        "candidates": ranked,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        if index_status in {"stale_using_previous_index", "changed_using_previous_index"}:
            label = "changed" if index_status == "changed_using_previous_index" else "stale"
            suffix = "background refresh started" if background_refresh_started else "using previous index"
            print(f"Jikji index is {label}; {suffix}.")
        elif foreground_prepared:
            print(f"Jikji index {index_status.replace('_', ' ')}.")
        for idx, item in enumerate(ranked, 1):
            reasons = ",".join(str(x) for x in (item.get("reasons") or []))
            print(f"{idx:02d} {item.get('score'):>8} {item.get('path')}  [{reasons}]")
    return 0


def cmd_brief(args) -> int:
    root = Path(args.path).expanduser().resolve()
    index_status, should_prepare = _search_index_status(root, args, stale_after_seconds=args.stale_after_seconds)
    foreground_prepared = False
    if args.fresh or (should_prepare and args.auto_prepare):
        build_agent_index(root, _search_prepare_config(args, root))
        index_status = "prepared_now" if should_prepare else "refreshed_now"
        foreground_prepared = True
    elif should_prepare and not args.auto_prepare:
        print(f"No Jikji search index found under {root}. Run: jikji prepare {root}", file=sys.stderr)
        return 1

    candidates = search(root, args.query, top_k=args.top_k)
    background_refresh_started = False
    if index_status in {"stale_using_previous_index", "changed_using_previous_index"} and not args.fresh:
        background_refresh_started = _maybe_start_background_refresh(args, root)
    if getattr(args, "compact", False):
        payload = build_compact_agent_brief_payload(
            root,
            args.query,
            top_k=args.top_k,
            index_status=index_status,
            foreground_prepared=foreground_prepared,
            background_refresh_started=background_refresh_started,
            candidates=candidates,
        )
    else:
        payload = build_agent_brief_payload(
            root,
            args.query,
            top_k=args.top_k,
            index_status=index_status,
            foreground_prepared=foreground_prepared,
            background_refresh_started=background_refresh_started,
            candidates=candidates,
        )
    if args.json:
        if getattr(args, "compact", False):
            print(json.dumps(payload, ensure_ascii=False, separators=(",", ":")))
        else:
            print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(brief_markdown(payload))
    return 0


def cmd_find(args) -> int:
    root = Path(args.path).expanduser().resolve()
    index_status, should_prepare = _search_index_status(root, args, stale_after_seconds=args.stale_after_seconds)
    if should_prepare:
        print(f"No Jikji search index found under {root}. Run: jikji prepare {root}", file=sys.stderr)
        return 1
    payload = discover(
        root,
        args.query,
        top_k=args.top_k,
        retry_exhausted=getattr(args, "after_jikji_retry", False),
        retry_proof=getattr(args, "retry_proof", ""),
    )
    paths = [str(path) for path in payload.get("answer_paths") or payload.get("paths") or [] if str(path)]
    if args.first:
        paths = paths[:1]
        payload["answer_paths"] = paths
        payload["paths"] = paths
        payload["candidates"] = list(payload.get("candidates") or [])[:1]
        payload["evidence_pack"] = list(payload.get("evidence_pack") or [])[:1]
    payload["mode"] = "find"
    payload["command"] = "jikji find"
    payload["index_status"] = index_status
    payload["paths"] = paths
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, separators=(",", ":")))
    else:
        for path in paths:
            print(path)
    return 0


def cmd_discover(args) -> int:
    root = Path(args.path).expanduser().resolve()
    index_status, should_prepare = _search_index_status(root, args, stale_after_seconds=args.stale_after_seconds)
    if should_prepare:
        print(f"No Jikji search index found under {root}. Run: jikji prepare {root}", file=sys.stderr)
        return 1
    payload = discover(
        root,
        args.query,
        top_k=args.top_k,
        retry_exhausted=args.after_jikji_retry,
        retry_proof=args.retry_proof,
    )
    payload["index_status"] = index_status
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, separators=(",", ":")))
    else:
        print(f"query_type={payload['query_type']} confidence={payload['confidence']} action={payload['recommended_action']}")
        for idx, item in enumerate(payload.get("candidates") or [], 1):
            print(f"{idx:02d} {item.get('s'):>8} {item.get('p')}  [{','.join(str(x) for x in item.get('why') or [])}]")
    return 0


def cmd_graph(args) -> int:
    root = Path(args.path).expanduser().resolve()
    if args.graph_cmd == "query":
        payload = {
            "root": str(root),
            "query": args.query,
            "candidates": query_graph_routes(root, args.query, top_k=args.top_k),
        }
    elif args.graph_cmd == "explain":
        payload = explain_source(root, args.source_path)
    else:
        payload = graph_status(root)
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    elif args.graph_cmd == "query":
        for idx, item in enumerate(payload.get("candidates") or [], 1):
            print(f"{idx:02d} {item.get('score'):>6} {item.get('path')}  terms={','.join(item.get('matched_terms') or [])}")
    else:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0


def cmd_gui(args) -> int:
    root = Path(args.path).expanduser().resolve()
    if args.background:
        requested_port = int(args.port)
        child_port = 0 if requested_port == 8765 else requested_port
        cmd = [
            sys.executable,
            "-m",
            "jikji.__main__",
            "gui",
            str(root),
            "--host",
            args.host,
            "--port",
            str(child_port),
            "--no-open",
        ]
        if args.prepare:
            cmd.append("--prepare")
        log_path = root / ".jikji" / "gui_server.log"
        log_path.parent.mkdir(parents=True, exist_ok=True)
        log = log_path.open("wb")
        proc = subprocess.Popen(cmd, cwd=str(Path.cwd()), stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT, start_new_session=True, close_fds=True)  # noqa: S603
        log.close()
        url = f"http://{args.host}:{requested_port}/" if child_port != 0 else ""
        deadline = time.time() + 3.0
        while not url and time.time() < deadline:
            try:
                text = log_path.read_text(encoding="utf-8", errors="ignore")
            except OSError:
                text = ""
            for line in text.splitlines():
                if line.startswith("Jikji GUI: "):
                    url = line.split("Jikji GUI: ", 1)[1].strip()
                    break
            if not url:
                time.sleep(0.05)
        if not url:
            url = f"http://{args.host}:0/"
        payload = {"url": url, "root": str(root), "pid": proc.pid, "log": str(log_path)}
        if args.json:
            print(json.dumps(payload, ensure_ascii=False, indent=2))
        else:
            print(f"Jikji GUI: {payload['url']}")
            print(f"Root: {root}")
            print(f"PID: {proc.pid}")
        return 0
    url = serve_gui(
        root,
        host=args.host,
        port=args.port,
        auto_prepare=args.prepare,
        open_browser=not args.no_open,
        quiet=args.json,
    )
    if args.json:
        print(json.dumps({"url": url, "root": str(root)}, ensure_ascii=False, indent=2))
    return 0


def cmd_hippocamp_fetch(args) -> int:
    result = fetch_subset(
        Path(args.dest),
        profile=args.profile,
        split=args.split,
        max_files=args.max_files,
        max_file_bytes=args.max_file_bytes,
        max_total_bytes=args.max_total_bytes,
    )
    payload = {
        "root": str(result.root),
        "annotation": str(result.annotation_path),
        "files_downloaded": result.files_downloaded,
        "bytes_downloaded": result.bytes_downloaded,
        "skipped": result.skipped,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"HippoCamp subset fetched: {result.root}")
        print(
            f"- annotation={result.annotation_path} files={result.files_downloaded} "
            f"bytes={result.bytes_downloaded} skipped={result.skipped}"
        )
    return 0


def cmd_hippocamp_import(args) -> int:
    annotation = Path(args.annotation).expanduser().resolve() if args.annotation else None
    result = import_eval_set(
        Path(args.path),
        annotation=annotation,
        max_cases=args.cases,
        out=Path(args.out).expanduser().resolve() if args.out else None,
    )
    payload = {
        "eval_set": str(result.eval_set_path),
        "cases": result.cases,
        "skipped_cases": result.skipped_cases,
        "scenarios": result.scenarios,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"HippoCamp eval imported: {result.eval_set_path}")
        print(f"- cases={result.cases} scenarios={result.scenarios}")
    return 0


def cmd_bench_run(args) -> int:
    eval_set = Path(args.eval_set).expanduser().resolve() if args.eval_set else None
    modes = tuple(m.strip() for m in args.modes.split(",") if m.strip())
    result = run_benchmark(
        Path(args.path),
        eval_set=eval_set,
        modes=modes,
        top_k=args.top_k,
        prepare=args.prepare,
        allow_leak=args.allow_leak,
    )
    payload = {
        "report": str(result.report_path),
        "metrics": result.metrics,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Benchmark complete: {result.report_path}")
        for mode, metrics in result.metrics.items():
            print(
                f"- {mode}: cases={metrics.get('cases')} hit@1={metrics.get('hit_at_1')} "
                f"hit@5={metrics.get('hit_at_5')} hit@10={metrics.get('hit_at_10')} "
                f"dup@10={metrics.get('duplicate_or_exact_hit_at_10')} mrr={metrics.get('mrr')} seconds={metrics.get('seconds')}"
            )
    return 0


def cmd_bench_iterate(args) -> int:
    eval_set = Path(args.eval_set).expanduser().resolve()
    modes = tuple(m.strip() for m in args.modes.split(",") if m.strip())
    result = run_improvement_loop(
        Path(args.path),
        eval_set=eval_set,
        iterations=args.iterations,
        modes=modes,
        top_k=args.top_k,
        out=Path(args.out).expanduser().resolve() if args.out else None,
        baseline_report=Path(args.baseline_report).expanduser().resolve() if args.baseline_report else None,
    )
    payload = {
        "report": str(result.report_path),
        "iterations": result.iterations,
        "best_metrics": result.best_metrics,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Benchmark repeat loop complete: {result.report_path}")
        print(f"- iterations={result.iterations}")
        print(f"- best={result.best_metrics}")
    return 0


def cmd_hermes_compare(args) -> int:
    payload = compare_benchmark_reports(
        Path(args.raw_report),
        Path(args.jikji_report),
        raw_mode=args.raw_mode,
        jikji_mode=args.jikji_mode,
        max_token_ratio=args.max_token_ratio,
        max_call_ratio=args.max_call_ratio,
        max_seconds_ratio=args.max_seconds_ratio,
        max_avg_llm_calls=args.max_avg_llm_calls,
        max_p95_llm_calls=args.max_p95_llm_calls,
    )
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        status = "PASS" if payload["ok"] else "FAIL"
        print(f"Hermes benchmark compare: {status}")
        for key, ok in payload["checks"].items():
            print(f"- {key}: {'ok' if ok else 'FAIL'}")
        print(f"ratios={payload['ratios']}")
    return 0 if payload["ok"] else 1


def cmd_benchmark_value_report(args) -> int:
    payload = write_two_call_value_report(
        Path(args.raw_discover_dir).expanduser().resolve(),
        Path(args.out).expanduser().resolve(),
        answer_pack_dir=Path(args.answer_pack_dir).expanduser().resolve(),
        answer_pack_report=Path(args.answer_pack_report).expanduser().resolve() if args.answer_pack_report else None,
        pricing=Pricing(
            input_per_1m_usd=args.input_per_1m_usd,
            output_per_1m_usd=args.output_per_1m_usd,
            usd_to_krw=args.usd_to_krw,
        ),
        judge_top_k=args.judge_top_k,
        llm_latency_seconds=args.llm_latency_seconds,
    )
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        raw = payload["modes"]["raw"]
        headline = str(payload.get("headline_strategy") or "jikji-one-call-raw-floor")
        recommended = payload["modes"][headline]
        savings = payload["savings"][f"{headline}_vs_raw"]
        print(f"Benchmark value report written: {Path(args.out).expanduser().resolve()}")
        print(
            f"- raw: hit@1={raw['hit_at_1']} hit@10={raw['hit_at_10']} "
            f"calls={raw['llm_calls']} tokens={raw['total_tokens']} seconds={raw['seconds']}"
        )
        print(
            f"- Jikji find: hit@1={recommended['hit_at_1']} "
            f"hit@10={recommended['hit_at_10']} calls={recommended['llm_calls']} "
            f"tokens={recommended['total_tokens']} seconds={recommended['seconds']}"
        )
        print(
            f"- saved: calls={savings['llm_calls_saved']} tokens={savings['total_tokens_saved']} "
            f"seconds={savings['seconds_saved']} krw={savings['krw_saved']}"
        )
    return 0


def cmd_hermes_bench(args) -> int:
    modes = tuple(m.strip() for m in args.modes.split(",") if m.strip())
    result = run_hermes_benchmark(
        Path(args.path),
        eval_set=Path(args.eval_set),
        modes=modes,
        cases_limit=args.cases if args.cases > 0 else None,
        out=Path(args.out).expanduser().resolve() if args.out else None,
        hermes_bin=args.hermes_bin,
        model=args.model,
        provider=args.provider,
        timeout_s=args.timeout,
        max_turns=args.max_turns,
        fast_max_turns=args.fast_max_turns,
        skills=args.skills,
        candidate_top_k=args.candidate_top_k,
        retries=args.retries,
        allow_leak=args.allow_leak,
        yolo=args.yolo,
    )
    payload = {"report": str(result.report_path), "metrics": result.metrics}
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Hermes benchmark complete: {result.report_path}")
        for mode, metrics in result.metrics.items():
            print(
                f"- {mode}: cases={metrics.get('cases')} hit@3={metrics.get('hit_at_3')} "
                f"hit@5={metrics.get('hit_at_5')} hit@10={metrics.get('hit_at_10')} "
                f"dup@10={metrics.get('duplicate_or_exact_hit_at_10')} avg_seconds={metrics.get('avg_seconds')}"
            )
            print(
                f"    llm_calls={metrics.get('llm_calls')} prompt_tokens={metrics.get('prompt_tokens')} "
                f"completion_tokens={metrics.get('completion_tokens')} total_tokens={metrics.get('total_tokens')}"
            )
    return 0


def cmd_hermes_skill_install(args) -> int:
    result = install_hermes_skill(dest=Path(args.dest).expanduser().resolve() if args.dest else None, force=args.force)
    payload = {"path": str(result.path), "installed": result.installed, "message": result.message}
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Hermes Jikji skill: {result.message} ({result.path})")
    return 0


def cmd_agent_skill_install(args) -> int:
    agents: list[str] = []
    default_agents = ["custom"] if args.dest and not args.agent else ["all"]
    for item in args.agent or default_agents:
        agents.extend(expand_agent_selection(item))
    seen: set[tuple[str, str]] = set()
    results = []
    for agent in agents:
        dest = Path(args.dest).expanduser().resolve() if args.dest else None
        if dest is not None and len(agents) > 1:
            raise ValueError("--dest can only be used with one --agent value")
        result = install_agent_skill(agent, dest=dest, force=args.force)
        key = (result.agent, str(result.path))
        if key in seen:
            continue
        seen.add(key)
        results.append(result)
    prepare_result = _prepare_after_skill_install(args)
    payload = {
        "installed_any": any(item.installed for item in results),
        "results": [
            {
                "agent": item.agent,
                "path": str(item.path),
                "installed": item.installed,
                "message": item.message,
            }
            for item in results
        ],
        "post_install_prepare": prepare_result,
        "after_install_protocol": (
            "When this SKILL.md is in an agent's skill directory, local file/document "
            "discovery requests under an explicit root should trigger Jikji first: "
            "jikji find ROOT \"query\" --json, then follow handoff_action. "
            "Jikji find builds the multi-query, multi-route candidate slate before "
            "any raw filesystem fallback is considered."
        ),
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        for item in results:
            verb = "INSTALLED" if item.installed else "SKIPPED"
            print(f"{verb} {item.agent}: {item.message} ({item.path})")
        _print_post_install_prepare(prepare_result)
        print("After install, file/document discovery should use Jikji first via the skill trigger.")
    return 0


def cmd_skill_export(args) -> int:
    source = repo_skill_path()
    if args.dest:
        result = install_agent_skill("custom", dest=Path(args.dest).expanduser().resolve(), force=args.force)
        prepare_result = _prepare_after_skill_install(args)
        payload = {
            "path": str(result.path),
            "installed": result.installed,
            "message": result.message,
            "post_install_prepare": prepare_result,
            "usage": "Point any coding/local agent's skill loader at this SKILL.md, or paste it into that agent's persistent instructions.",
        }
        if args.json:
            print(json.dumps(payload, ensure_ascii=False, indent=2))
        else:
            print(f"Jikji universal skill: {result.message} ({result.path})")
            _print_post_install_prepare(prepare_result)
        return 0
    text = source.read_text(encoding="utf-8")
    if args.json:
        print(json.dumps({
            "source": str(source),
            "skill_markdown": text,
            "usage": "Install this SKILL.md into any coding/local agent that supports Markdown skills or persistent prompt snippets.",
        }, ensure_ascii=False, indent=2))
    else:
        print(text)
    return 0


def cmd_post_install_prepare(args) -> int:
    payload = {
        "mode": "foreground",
        "roots": _prepare_roots_foreground(
            [Path(item).expanduser().resolve() for item in args.roots],
            max_files=args.max_files,
            parse_timeout=args.parse_timeout,
        ),
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        _print_post_install_prepare(payload)
    return 0


def _prepare_after_skill_install(args) -> dict[str, object]:
    if getattr(args, "no_prepare", False):
        return {"mode": "disabled", "roots": []}
    raw_roots = list(getattr(args, "prepare_root", None) or [])
    if not raw_roots:
        roots, selection = _select_default_post_install_roots()
    else:
        roots = _select_post_install_roots(raw_roots)
        selection = {"source": "explicit_prepare_root", "document_heavy_roots": [], "common_roots": [str(root) for root in roots]}
    if not roots:
        return {"mode": "none", "roots": [], "selection": selection}
    if getattr(args, "foreground_prepare", False):
        return {
            "mode": "foreground",
            "roots": _prepare_roots_foreground(
                roots,
                max_files=getattr(args, "max_files", 0),
                parse_timeout=getattr(args, "parse_timeout", 5.0),
            ),
            "selection": selection,
        }
    return _start_background_post_install_prepare(
        roots,
        max_files=getattr(args, "max_files", 0),
        parse_timeout=getattr(args, "parse_timeout", 5.0),
        selection=selection,
    )


def _select_post_install_roots(raw_roots: list[str]) -> list[Path]:
    candidates = [Path(item).expanduser() for item in raw_roots]
    roots: list[Path] = []
    seen: set[Path] = set()
    for candidate in candidates:
        try:
            root = candidate.expanduser().resolve()
        except (OSError, RuntimeError):
            continue
        if root in seen or not root.exists() or not root.is_dir():
            continue
        seen.add(root)
        roots.append(root)
    return roots


def _post_install_home() -> Path:
    override = os.environ.get("JIKJI_POST_INSTALL_HOME")
    if override:
        return Path(override).expanduser().resolve()
    return Path.home().resolve()


def _select_default_post_install_roots() -> tuple[list[Path], dict[str, object]]:
    policy = _post_install_load_policy()
    max_roots = int(policy["max_default_roots"])
    home = _post_install_home()
    common_roots = _common_post_install_roots(home)
    document_roots, scanned = _document_heavy_post_install_roots(home, common_roots, max_roots=max(0, max_roots - len(common_roots)))
    roots = _dedupe_roots([*common_roots, *document_roots])[:max_roots]
    selection = {
        "source": "auto_common_and_document_roots",
        "home": str(home),
        "common_roots": [str(root) for root in common_roots],
        "document_heavy_roots": [str(root) for root in document_roots],
        "document_extensions": sorted(_POST_INSTALL_DOCUMENT_EXTS),
        "scanned": scanned,
    }
    return roots, selection


def _common_post_install_roots(home: Path) -> list[Path]:
    candidates = [home / rel for rel in _POST_INSTALL_COMMON_RELS]
    return _dedupe_roots([path for path in candidates if path.is_dir()])


def _dedupe_roots(candidates: list[Path]) -> list[Path]:
    roots: list[Path] = []
    seen: set[Path] = set()
    for candidate in candidates:
        try:
            root = candidate.expanduser().resolve()
        except (OSError, RuntimeError):
            continue
        if root in seen or not root.is_dir():
            continue
        if any(root == existing or _is_relative_to(root, existing) for existing in roots):
            continue
        seen.add(root)
        roots.append(root)
    return roots


def _is_relative_to(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
    except ValueError:
        return False
    return True


def _document_heavy_post_install_roots(
    home: Path,
    covered_roots: list[Path],
    *,
    max_roots: int,
) -> tuple[list[Path], dict[str, int]]:
    if max_roots <= 0 or not home.is_dir():
        return [], {"dirs_seen": 0, "files_seen": 0, "document_files": 0}
    scores: dict[Path, int] = {}
    dirs_seen = 0
    files_seen = 0
    document_files = 0
    max_dirs_seen = 4_000
    max_files_seen = 60_000
    for dirpath, dirnames, filenames in os.walk(home):
        current = Path(dirpath)
        dirs_seen += 1
        dirnames[:] = [
            name for name in dirnames
            if name not in _POST_INSTALL_SKIP_DIRS and not name.startswith(".") and name != AGENT_DIR_NAME
        ]
        if _is_under_any(current, covered_roots):
            dirnames[:] = []
            continue
        if dirs_seen > max_dirs_seen or files_seen > max_files_seen:
            dirnames[:] = []
            continue
        count = 0
        for filename in filenames:
            files_seen += 1
            if Path(filename).suffix.lower() in _POST_INSTALL_DOCUMENT_EXTS:
                count += 1
                document_files += 1
        if count >= 3:
            scores[current] = count
    ranked = sorted(scores.items(), key=lambda item: (-item[1], len(item[0].parts), str(item[0])))[:max_roots]
    roots = _dedupe_roots([path for path, _count in ranked])
    return roots, {"dirs_seen": dirs_seen, "files_seen": files_seen, "document_files": document_files}


def _is_under_any(path: Path, roots: list[Path]) -> bool:
    for root in roots:
        if path == root or _is_relative_to(path, root):
            return True
    return False


def _prepare_roots_foreground(
    roots: list[Path],
    *,
    max_files: int,
    parse_timeout: float,
) -> list[dict[str, object]]:
    prepared: list[dict[str, object]] = []
    for root in roots:
        if not root.exists() or not root.is_dir():
            prepared.append({"root": str(root), "ok": False, "error": "missing_or_not_directory"})
            continue
        cfg = Config()
        cfg.max_files = max_files
        cfg.parse_timeout_s = parse_timeout
        try:
            result = build_agent_index(root, cfg)
            prepared.append({
                "root": str(root),
                "ok": True,
                "index_dir": str(result.index_dir),
                "agent_map": str(result.agent_map),
                "files": result.files,
                "folders": result.folders,
                "docs_parsed": result.docs_parsed,
                "docs_reused": result.docs_reused,
                "docs_failed": result.docs_failed,
                "deleted": result.deleted,
            })
        except Exception as exc:  # pragma: no cover - defensive install UX
            prepared.append({"root": str(root), "ok": False, "error": str(exc)})
    return prepared


def _start_background_post_install_prepare(
    roots: list[Path],
    *,
    max_files: int,
    parse_timeout: float,
    selection: dict[str, object],
) -> dict[str, object]:
    log_dir = _post_install_home() / ".local" / "share" / "jikji" / "post_install"
    try:
        log_dir.mkdir(parents=True, exist_ok=True)
        stamp = datetime.now(tz=UTC).strftime("%Y%m%dT%H%M%SZ")
        log_path = log_dir / f"prepare_{stamp}.json"
        log = log_path.open("ab")
        cmd = [
            sys.executable,
            "-m",
            "jikji.__main__",
            "post-install-prepare",
            *[str(root) for root in roots],
            "--max-files",
            str(max_files),
            "--parse-timeout",
            str(parse_timeout),
            "--json",
        ]
        proc = subprocess.Popen(  # noqa: S603 - current Python module with explicit roots
            cmd,
            cwd=str(Path.home()),
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            close_fds=True,
        )
        log.close()
        return {
            "mode": "background",
            "started": True,
            "pid": proc.pid,
            "log": str(log_path),
            "roots": [{"root": str(root), "status": "queued"} for root in roots],
            "policy": _post_install_load_policy(),
            "selection": selection,
        }
    except OSError as exc:
        return {
            "mode": "background",
            "started": False,
            "error": str(exc),
            "roots": [{"root": str(root), "status": "not_started"} for root in roots],
            "policy": _post_install_load_policy(),
            "selection": selection,
        }


def _post_install_load_policy() -> dict[str, object]:
    cpu_count = os.cpu_count() or 1
    memory_gib = _memory_gib()
    if cpu_count <= 2 or (memory_gib is not None and memory_gib <= 4):
        max_roots = 2
    elif cpu_count <= 4 or (memory_gib is not None and memory_gib <= 8):
        max_roots = 3
    else:
        max_roots = 5
    return {
        "cpu_count": cpu_count,
        "memory_gib": memory_gib,
        "max_default_roots": max_roots,
        "concurrency": 1,
        "note": "post-install prepare runs sequentially in one background process",
    }


def _memory_gib() -> float | None:
    try:
        pages = os.sysconf("SC_PHYS_PAGES")
        page_size = os.sysconf("SC_PAGE_SIZE")
        if isinstance(pages, int) and isinstance(page_size, int):
            return round((pages * page_size) / (1024 ** 3), 2)
    except (AttributeError, OSError, ValueError):
        return None
    return None


def _print_post_install_prepare(payload: dict[str, object]) -> None:
    mode = payload.get("mode")
    if mode == "disabled":
        print("POST-INSTALL PREPARE disabled.")
        if payload.get("reason"):
            print(f"POST-INSTALL PREPARE reason: {payload.get('reason')}")
        return
    if mode == "background":
        if payload.get("started"):
            print(f"POST-INSTALL PREPARE background pid={payload.get('pid')} log={payload.get('log')}")
        else:
            print(f"POST-INSTALL PREPARE not started: {payload.get('error')}")
        for item in payload.get("roots", []):
            if isinstance(item, dict):
                print(f"QUEUED {item.get('root')}: {item.get('status')}")
        return
    for item in payload.get("roots", []):
        if not isinstance(item, dict):
            continue
        if item.get("ok") is False:
            print(f"PREPARE-SKIPPED {item['root']}: {item.get('error')}")
        else:
            print(
                f"PREPARED {item['root']}: files={item.get('files')} "
                f"folders={item.get('folders')} docs_failed={item.get('docs_failed')}"
            )


def cmd_hippocamp_suite(args) -> int:
    profiles = tuple(p.strip() for p in args.profiles.split(",") if p.strip())
    result = run_suite(
        Path(args.dest),
        profiles=profiles,
        split=args.split,
        max_files=args.max_files,
        max_file_bytes=args.max_file_bytes,
        max_total_bytes=args.max_total_bytes,
        cases=args.cases,
        top_k=args.top_k,
        fetch=not args.no_fetch,
    )
    payload = {"report": str(result.report_path), "aggregate": result.aggregate, "profiles": result.profiles}
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"HippoCamp suite complete: {result.report_path}")
        for mode, metrics in result.aggregate.items():
            print(
                f"- {mode}: cases={metrics.get('cases')} hit@1={metrics.get('hit_at_1')} "
                f"hit@5={metrics.get('hit_at_5')} hit@10={metrics.get('hit_at_10')} "
                f"dup@10={metrics.get('duplicate_or_exact_hit_at_10')} mrr={metrics.get('mrr')}"
            )
    return 0


def cmd_beir_import(args) -> int:
    result = materialize_beir_dataset(
        args.dataset,
        Path(args.dest),
        split=args.split,
        max_cases=args.cases,
    )
    payload = {
        "dataset": result.dataset,
        "source_dir": str(result.source_dir),
        "corpus_root": str(result.corpus_root),
        "eval_set": str(result.eval_set_path),
        "documents": result.documents,
        "cases": result.cases,
        "qrels": result.qrels,
        "public_benchmark": True,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"BEIR dataset materialized: {result.dataset}")
        print(f"- corpus={result.corpus_root}")
        print(f"- eval_set={result.eval_set_path}")
        print(f"- documents={result.documents} cases={result.cases} qrels={result.qrels}")
    return 0


def cmd_beir_suite(args) -> int:
    datasets = tuple(d.strip() for d in args.datasets.split(",") if d.strip())
    result = run_beir_suite(
        Path(args.dest),
        datasets=datasets,
        split=args.split,
        max_cases=args.cases,
        top_k=args.top_k,
        prepare=not args.no_prepare,
    )
    payload = {
        "report": str(result.report_path),
        "aggregate": result.aggregate,
        "datasets": result.datasets,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"BEIR suite complete: {result.report_path}")
        for mode, metrics in result.aggregate.items():
            if isinstance(metrics, dict) and "cases" in metrics:
                print(
                    f"- {mode}: cases={metrics.get('cases')} hit@1={metrics.get('hit_at_1')} "
                    f"hit@5={metrics.get('hit_at_5')} hit@10={metrics.get('hit_at_10')} "
                    f"mrr={metrics.get('mrr')} seconds={metrics.get('seconds')}"
                )
    return 0


def cmd_edith_summary(args) -> int:
    result = edith_answer_summary(Path(args.dest))
    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print("EDiTh benchmark metadata")
        print(f"- source={result['source']}")
        print(f"- master_rows={result['master_rows']} answer_questions={result['answer_questions']}")
        print(
            f"- file_retrieval_questions={result['file_retrieval_questions']} "
            f"referenced_docs={result['referenced_docs']}"
        )
        print(f"- formats={result['formats']}")
        print(f"- languages={result['languages']}")
    return 0


def cmd_edith_import(args) -> int:
    result = materialize_edith_dataset(
        Path(args.dest),
        max_cases=args.cases,
        max_docs=args.max_docs,
        download_docs=not args.no_docs,
        max_download_bytes=args.max_download_bytes,
    )
    payload = {
        "metadata_dir": str(result.metadata_dir),
        "corpus_root": str(result.corpus_root),
        "eval_set": str(result.eval_set_path),
        "selected_questions": result.selected_questions,
        "selected_docs": result.selected_docs,
        "extracted_docs": result.extracted_docs,
        "skipped_questions": result.skipped_questions,
        "archive_bytes_read": result.archive_bytes_read,
        "archive_byte_limit": result.archive_byte_limit,
        "archive_truncated": result.archive_truncated,
        "public_benchmark": True,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print("EDiTh benchmark materialized")
        print(f"- corpus={result.corpus_root}")
        print(f"- eval_set={result.eval_set_path}")
        print(
            f"- questions={result.selected_questions} selected_docs={result.selected_docs} "
            f"extracted_docs={result.extracted_docs}"
        )
    return 0


def cmd_edith_suite(args) -> int:
    result = run_edith_suite(
        Path(args.dest),
        max_cases=args.cases,
        max_docs=args.max_docs,
        top_k=args.top_k,
        download_docs=not args.no_docs,
        prepare=not args.no_prepare,
        max_download_bytes=args.max_download_bytes,
    )
    payload = {
        "report": str(result.report_path),
        "materialized": {
            "metadata_dir": str(result.materialized.metadata_dir),
            "corpus_root": str(result.materialized.corpus_root),
            "eval_set": str(result.materialized.eval_set_path),
            "selected_questions": result.materialized.selected_questions,
            "selected_docs": result.materialized.selected_docs,
            "extracted_docs": result.materialized.extracted_docs,
            "skipped_questions": result.materialized.skipped_questions,
            "archive_bytes_read": result.materialized.archive_bytes_read,
            "archive_byte_limit": result.materialized.archive_byte_limit,
            "archive_truncated": result.materialized.archive_truncated,
        },
        "prepare_seconds": result.prepare_seconds,
        "metrics": result.metrics,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"EDiTh suite complete: {result.report_path}")
        if "metadata_only" in result.metrics or "no_document_cases" in result.metrics:
            mode = "metadata_only" if "metadata_only" in result.metrics else "no_document_cases"
            metrics = result.metrics[mode]
            print(f"- {mode}: cases={metrics.get('cases')} selected_docs={metrics.get('selected_docs')}")
            print(f"- note: {metrics.get('note')}")
        else:
            for mode, metrics in result.metrics.items():
                print(
                    f"- {mode}: cases={metrics.get('cases')} hit@1={metrics.get('hit_at_1')} "
                    f"hit@5={metrics.get('hit_at_5')} hit@10={metrics.get('hit_at_10')} "
                    f"mrr={metrics.get('mrr')} seconds={metrics.get('seconds')}"
                )
    return 0


def cmd_publicdata_build(args) -> int:
    result = build_publicdata_benchmark(
        Path(args.dest),
        target_docs=args.target_docs,
        max_id=args.max_id,
        max_cases=args.cases,
        seed=args.seed,
    )
    payload = {
        "dest": str(result.dest),
        "train_root": str(result.train_root),
        "valid_root": str(result.valid_root),
        "test_root": str(result.test_root),
        "train_eval_set": str(result.train_eval_set_path),
        "valid_eval_set": str(result.valid_eval_set_path),
        "eval_set": str(result.eval_set_path),
        "manifest": str(result.manifest_path),
        "docs_downloaded": result.docs_downloaded,
        "train_docs": result.train_docs,
        "valid_docs": result.valid_docs,
        "test_docs": result.test_docs,
        "eval_cases": result.eval_cases,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print("Public-data agent benchmark built")
        print(f"- docs={result.docs_downloaded} train/valid/test={result.train_docs}/{result.valid_docs}/{result.test_docs}")
        print(f"- test_eval={result.eval_set_path}")
    return 0


def cmd_publicdata_suite(args) -> int:
    result = run_publicdata_suite(
        Path(args.dest),
        target_docs=args.target_docs,
        max_id=args.max_id,
        max_cases=args.cases,
        seed=args.seed,
        top_k=args.top_k,
    )
    payload = {
        "report": str(result.report_path),
        "build": {
            "dest": str(result.build.dest),
            "train_root": str(result.build.train_root),
            "valid_root": str(result.build.valid_root),
            "test_root": str(result.build.test_root),
            "train_eval_set": str(result.build.train_eval_set_path),
            "valid_eval_set": str(result.build.valid_eval_set_path),
            "eval_set": str(result.build.eval_set_path),
            "manifest": str(result.build.manifest_path),
            "docs_downloaded": result.build.docs_downloaded,
            "train_docs": result.build.train_docs,
            "valid_docs": result.build.valid_docs,
            "test_docs": result.build.test_docs,
            "eval_cases": result.build.eval_cases,
        },
        "prepare_seconds": result.prepare_seconds,
        "deterministic_report": str(result.deterministic_report),
        "deterministic_metrics": result.deterministic_metrics,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Public-data suite complete: {result.report_path}")
        for mode, metrics in result.deterministic_metrics.items():
            print(
                f"- {mode}: cases={metrics.get('cases')} hit@5={metrics.get('hit_at_5')} "
                f"hit@10={metrics.get('hit_at_10')} mrr={metrics.get('mrr')} seconds={metrics.get('seconds')}"
            )
    return 0


def cmd_workspacebench_build(args) -> int:
    result = build_workspacebench_benchmark(
        Path(args.dest),
        max_tasks=args.max_tasks,
        start=args.start,
        max_file_bytes=args.max_file_bytes,
        max_total_bytes=args.max_total_bytes,
    )
    payload = {
        "dest": str(result.dest),
        "corpus_root": str(result.corpus_root),
        "eval_set": str(result.eval_set_path),
        "manifest": str(result.manifest_path),
        "tasks": result.tasks,
        "files_downloaded": result.files_downloaded,
        "bytes_downloaded": result.bytes_downloaded,
        "eval_cases": result.eval_cases,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print("Workspace-Bench-Lite file-discovery benchmark built")
        print(f"- corpus={result.corpus_root}")
        print(f"- tasks={result.tasks} files={result.files_downloaded} eval_cases={result.eval_cases}")
        print(f"- eval_set={result.eval_set_path}")
    return 0


def cmd_workspacebench_suite(args) -> int:
    result = run_workspacebench_suite(
        Path(args.dest),
        max_tasks=args.max_tasks,
        start=args.start,
        top_k=args.top_k,
        max_file_bytes=args.max_file_bytes,
        max_total_bytes=args.max_total_bytes,
    )
    payload = {
        "report": str(result.report_path),
        "build": {
            "dest": str(result.build.dest),
            "corpus_root": str(result.build.corpus_root),
            "eval_set": str(result.build.eval_set_path),
            "manifest": str(result.build.manifest_path),
            "tasks": result.build.tasks,
            "files_downloaded": result.build.files_downloaded,
            "bytes_downloaded": result.build.bytes_downloaded,
            "eval_cases": result.build.eval_cases,
        },
        "prepare_seconds": result.prepare_seconds,
        "deterministic_report": str(result.deterministic_report),
        "deterministic_metrics": result.deterministic_metrics,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Workspace-Bench-Lite suite complete: {result.report_path}")
        for mode, metrics in result.deterministic_metrics.items():
            print(
                f"- {mode}: cases={metrics.get('cases')} hit@5={metrics.get('hit_at_5')} "
                f"hit@10={metrics.get('hit_at_10')} mrr={metrics.get('mrr')} seconds={metrics.get('seconds')}"
            )
    return 0


def cmd_hardbench_build(args) -> int:
    result = build_hard_benchmark(
        Path(args.dest),
        target_docs=args.target_docs,
        max_data_idx=args.max_data_idx,
        max_cases_per_split=args.cases,
        seed=args.seed,
        max_file_bytes=args.max_file_bytes,
        difficulty=args.difficulty,
        source_dir=Path(args.source_dir).expanduser().resolve() if args.source_dir else None,
        max_total_bytes=args.max_total_bytes,
    )
    payload = {
        "dest": str(result.dest),
        "train_root": str(result.train_root),
        "valid_root": str(result.valid_root),
        "test_root": str(result.test_root),
        "train_eval_set": str(result.train_eval_set_path),
        "valid_eval_set": str(result.valid_eval_set_path),
        "eval_set": str(result.eval_set_path),
        "manifest": str(result.manifest_path),
        "docs_downloaded": result.docs_downloaded,
        "train_docs": result.train_docs,
        "valid_docs": result.valid_docs,
        "test_docs": result.test_docs,
        "eval_cases": result.eval_cases,
        "difficulty": args.difficulty,
        "source_dir": args.source_dir,
        "max_total_bytes": args.max_total_bytes,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print("Hard mixed-document benchmark built")
        print(f"- docs={result.docs_downloaded} train/valid/test={result.train_docs}/{result.valid_docs}/{result.test_docs}")
        print(f"- test_eval={result.eval_set_path}")
    return 0


def cmd_hardbench_suite(args) -> int:
    result = run_hard_benchmark_suite(
        Path(args.dest),
        target_docs=args.target_docs,
        max_data_idx=args.max_data_idx,
        max_cases_per_split=args.cases,
        seed=args.seed,
        top_k=args.top_k,
        max_file_bytes=args.max_file_bytes,
        difficulty=args.difficulty,
        source_dir=Path(args.source_dir).expanduser().resolve() if args.source_dir else None,
        max_total_bytes=args.max_total_bytes,
    )
    payload = {
        "report": str(result.report_path),
        "build": {
            "dest": str(result.build.dest),
            "train_root": str(result.build.train_root),
            "valid_root": str(result.build.valid_root),
            "test_root": str(result.build.test_root),
            "train_eval_set": str(result.build.train_eval_set_path),
            "valid_eval_set": str(result.build.valid_eval_set_path),
            "eval_set": str(result.build.eval_set_path),
            "manifest": str(result.build.manifest_path),
            "docs_downloaded": result.build.docs_downloaded,
            "train_docs": result.build.train_docs,
            "valid_docs": result.build.valid_docs,
            "test_docs": result.build.test_docs,
            "eval_cases": result.build.eval_cases,
            "difficulty": args.difficulty,
            "source_dir": args.source_dir,
            "max_total_bytes": args.max_total_bytes,
        },
        "prepare_seconds": result.prepare_seconds,
        "reports": {split: str(path) for split, path in result.reports.items()},
        "metrics": result.metrics,
    }
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(f"Hard mixed-document suite complete: {result.report_path}")
        for split, split_metrics in result.metrics.items():
            for mode, metrics in split_metrics.items():
                print(
                    f"- {split}/{mode}: cases={metrics.get('cases')} hit@5={metrics.get('hit_at_5')} "
                    f"hit@10={metrics.get('hit_at_10')} seconds={metrics.get('seconds')}"
                )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="jikji", description="Prepare local files as agent-readable knowledge maps.")
    parser.add_argument("--version", action="version", version=f"jikji {__version__}")
    public_command_metavar = (
        "{prepare,refresh,clean,map,doctor,find,graph,gui,"
        "agent-skill-install,skill-export,bench-run,hermes-bench}"
    )
    sub = parser.add_subparsers(dest="cmd", metavar=public_command_metavar)

    def add_common(p):
        p.add_argument("path", nargs="?", default=".")
        p.add_argument("--max-files", type=int, default=0, help="optional file-count safety cap; 0 disables it")
        p.add_argument("--include-hidden", action="store_true")
        p.add_argument("--include-sensitive", action="store_true", help="include safety-denied names such as .env or private keys")
        p.add_argument("--exclude", action="append", default=[], help="additional fnmatch pattern to exclude; repeatable")
        p.add_argument("--max-hash-bytes", type=int, default=512 * 1024 * 1024)
        p.add_argument("--parse-timeout", type=float, default=5.0)
        p.add_argument("--doc-text-max-chars", type=int, default=2_000_000)
        p.add_argument("--doc-text-chunk-chars", type=int, default=1_000_000)
        p.add_argument("--enable-media-index", action="store_true", help="opt in to bounded local OCR/ASR for image/audio/video; may use CPU/RAM")
        p.add_argument("--media-index-max-mb", type=float, default=25.0, help="skip media OCR/ASR for files larger than this size")
        p.add_argument("--no-agent-rules", action="store_true", help="do not write the Jikji routing block into project-local AGENTS.md/CLAUDE.md/.cursorrules")
        p.add_argument("--json", action="store_true")

    p_prepare = sub.add_parser("prepare", help="create/update .jikji without moving files")
    add_common(p_prepare)
    p_prepare.set_defaults(func=cmd_prepare)

    p_refresh = sub.add_parser("refresh", help="alias for prepare")
    add_common(p_refresh)
    p_refresh.set_defaults(func=cmd_prepare)

    p_clean = sub.add_parser("clean", help="remove Jikji-generated artifacts from one prepared root")
    p_clean.add_argument("path", nargs="?", default=".")
    p_clean.add_argument("--dry-run", action="store_true")
    p_clean.add_argument("--force", action="store_true", help="remove .jikji even when manifest verification fails")
    p_clean.add_argument("--json", action="store_true")
    p_clean.set_defaults(func=cmd_clean)

    p_map = sub.add_parser("map", help="print the generated Jikji map")
    p_map.add_argument("path", nargs="?", default=".")
    p_map.add_argument("--max-chars", type=int, default=12_000)
    p_map.set_defaults(func=cmd_map)

    p_doctor = sub.add_parser("doctor", help="check whether a folder has Jikji artifacts")
    p_doctor.add_argument("path", nargs="?", default=".")
    p_doctor.add_argument("--json", action="store_true")
    p_doctor.set_defaults(func=cmd_doctor)

    p_eval_gen = sub.add_parser(
        "eval-generate",
        help="generate local search evaluation cases from the current Jikji index",
    )
    p_eval_gen.add_argument("path", nargs="?", default=".")
    p_eval_gen.add_argument("--cases", type=int, default=80)
    p_eval_gen.add_argument("--json", action="store_true")
    p_eval_gen.set_defaults(func=cmd_eval_generate)

    p_eval_real = sub.add_parser(
        "eval-generate-realistic",
        help="generate curated realistic local file-search cases from Jikji map artifacts",
    )
    p_eval_real.add_argument("path", nargs="?", default=".")
    p_eval_real.add_argument("--cases", type=int, default=240)
    p_eval_real.add_argument("--out", default="")
    p_eval_real.add_argument("--json", action="store_true")
    p_eval_real.set_defaults(func=cmd_eval_generate_realistic)

    p_eval_holdout = sub.add_parser(
        "eval-generate-holdout",
        help="generate a locked scorer-blind holdout eval set; do not tune against it",
    )
    p_eval_holdout.add_argument("path", nargs="?", default=".")
    p_eval_holdout.add_argument("--cases", type=int, default=180)
    p_eval_holdout.add_argument("--out", default="")
    p_eval_holdout.add_argument("--seed", default="jikji-holdout-v1")
    p_eval_holdout.add_argument("--json", action="store_true")
    p_eval_holdout.set_defaults(func=cmd_eval_generate_holdout)

    p_eval = sub.add_parser("eval", help="evaluate local search quality against a generated eval set")
    p_eval.add_argument("path", nargs="?", default=".")
    p_eval.add_argument("--eval-set", default="")
    p_eval.add_argument("--top-k", type=int, default=10)
    p_eval.add_argument("--json", action="store_true")
    p_eval.set_defaults(func=cmd_eval)

    p_analyze = sub.add_parser("bench-analyze", help="analyze map-only benchmark failures and answerability")
    p_analyze.add_argument("path", nargs="?", default=".")
    p_analyze.add_argument("--eval-set", default="")
    p_analyze.add_argument("--top-k", type=int, default=50)
    p_analyze.add_argument("--json", action="store_true")
    p_analyze.set_defaults(func=cmd_bench_analyze)

    p_find = sub.add_parser("find", help="find local files with Jikji's candidate slate")
    p_find.add_argument("path")
    p_find.add_argument("query")
    p_find.add_argument("--top-k", type=int, default=20)
    p_find.add_argument("--first", action="store_true", help="print/return only the top path")
    p_find.add_argument("--fresh", action="store_true", help=argparse.SUPPRESS)
    p_find.add_argument("--auto-prepare", dest="auto_prepare", action="store_true", help=argparse.SUPPRESS)
    p_find.add_argument("--no-auto-prepare", dest="auto_prepare", action="store_false", help=argparse.SUPPRESS)
    p_find.add_argument("--stale-after-seconds", type=int, default=24 * 60 * 60)
    p_find.add_argument("--max-files", type=int, default=0, help=argparse.SUPPRESS)
    p_find.add_argument("--include-hidden", action="store_true")
    p_find.add_argument("--include-sensitive", action="store_true")
    p_find.add_argument("--exclude", action="append", default=[])
    p_find.add_argument("--max-hash-bytes", type=int, default=512 * 1024 * 1024)
    p_find.add_argument("--parse-timeout", type=float, default=5.0)
    p_find.add_argument("--after-jikji-retry", action="store_true", help=argparse.SUPPRESS)
    p_find.add_argument("--retry-proof", default="", help=argparse.SUPPRESS)
    p_find.add_argument("--json", action="store_true")
    p_find.set_defaults(auto_prepare=False, background_refresh=False)
    p_find.set_defaults(func=cmd_find)

    p_discover = sub.add_parser("discover", help=argparse.SUPPRESS)
    p_discover.add_argument("path")
    p_discover.add_argument("query")
    p_discover.add_argument("--top-k", type=int, default=20)
    p_discover.add_argument("--fresh", action="store_true", help=argparse.SUPPRESS)
    p_discover.add_argument("--auto-prepare", dest="auto_prepare", action="store_true", help=argparse.SUPPRESS)
    p_discover.add_argument("--no-auto-prepare", dest="auto_prepare", action="store_false", help=argparse.SUPPRESS)
    p_discover.add_argument("--stale-after-seconds", type=int, default=24 * 60 * 60)
    p_discover.add_argument("--max-files", type=int, default=0, help="optional auto-prepare file-count safety cap; 0 disables it")
    p_discover.add_argument("--include-hidden", action="store_true")
    p_discover.add_argument("--include-sensitive", action="store_true")
    p_discover.add_argument("--exclude", action="append", default=[])
    p_discover.add_argument("--max-hash-bytes", type=int, default=512 * 1024 * 1024)
    p_discover.add_argument("--parse-timeout", type=float, default=5.0)
    p_discover.add_argument(
        "--after-jikji-retry",
        action="store_true",
        help=(
            "mark this call as the one allowed Jikji retry so low-confidence "
            "output can permit raw fallback"
        ),
    )
    p_discover.add_argument("--retry-proof", default="", help="retry proof emitted by the previous low-confidence discover payload")
    p_discover.add_argument("--json", action="store_true")
    p_discover.set_defaults(auto_prepare=False, background_refresh=False)
    p_discover.set_defaults(func=cmd_discover)

    p_search = sub.add_parser("search", help=argparse.SUPPRESS)
    p_search.add_argument("path")
    p_search.add_argument("query")
    p_search.add_argument("--top-k", type=int, default=20)
    p_search.add_argument("--fresh", action="store_true", help="run a foreground refresh before searching")
    p_search.add_argument("--auto-prepare", dest="auto_prepare", action="store_true", help="prepare when the instant index is missing")
    p_search.add_argument("--no-auto-prepare", dest="auto_prepare", action="store_false", help="do not auto-prepare when the instant index is missing")
    p_search.add_argument("--no-background-refresh", dest="background_refresh", action="store_false", help="do not launch background refresh for stale indexes")
    p_search.add_argument("--stale-after-seconds", type=int, default=24 * 60 * 60, help="mark an index stale after this age; negative disables staleness")
    p_search.add_argument("--max-files", type=int, default=0, help="optional auto-prepare file-count safety cap; 0 disables it")
    p_search.add_argument("--include-hidden", action="store_true")
    p_search.add_argument("--include-sensitive", action="store_true")
    p_search.add_argument("--exclude", action="append", default=[])
    p_search.add_argument("--max-hash-bytes", type=int, default=512 * 1024 * 1024)
    p_search.add_argument("--parse-timeout", type=float, default=5.0)
    p_search.add_argument("--json", action="store_true")
    p_search.set_defaults(auto_prepare=False, background_refresh=True)
    p_search.set_defaults(func=cmd_search)

    p_brief = sub.add_parser(
        "brief",
        help=argparse.SUPPRESS,
    )
    p_brief.add_argument("path")
    p_brief.add_argument("query")
    p_brief.add_argument("--top-k", type=int, default=10)
    p_brief.add_argument("--fresh", action="store_true", help="run a foreground refresh before briefing")
    p_brief.add_argument("--auto-prepare", dest="auto_prepare", action="store_true", help="prepare when the instant index is missing")
    p_brief.add_argument("--no-auto-prepare", dest="auto_prepare", action="store_false", help="do not auto-prepare when the instant index is missing")
    p_brief.add_argument("--no-background-refresh", dest="background_refresh", action="store_false", help="do not launch background refresh for stale indexes")
    p_brief.add_argument("--stale-after-seconds", type=int, default=24 * 60 * 60, help="mark an index stale after this age; negative disables staleness")
    p_brief.add_argument("--max-files", type=int, default=0, help="optional auto-prepare file-count safety cap; 0 disables it")
    p_brief.add_argument("--include-hidden", action="store_true")
    p_brief.add_argument("--include-sensitive", action="store_true")
    p_brief.add_argument("--exclude", action="append", default=[])
    p_brief.add_argument("--max-hash-bytes", type=int, default=512 * 1024 * 1024)
    p_brief.add_argument("--parse-timeout", type=float, default=5.0)
    p_brief.add_argument("--compact", action="store_true", help="emit token-minimal graph-route JSON for agents")
    p_brief.add_argument("--json", action="store_true")
    p_brief.set_defaults(auto_prepare=False, background_refresh=True)
    p_brief.set_defaults(func=cmd_brief)

    p_hf = sub.add_parser("hippocamp-fetch", help="download a bounded HippoCamp subset from Hugging Face")
    p_hf.add_argument("dest", help="destination directory for the downloaded subset")
    p_hf.add_argument("--profile", default="Adam")
    p_hf.add_argument("--split", default="Subset")
    p_hf.add_argument("--max-files", type=int, default=120)
    p_hf.add_argument("--max-file-bytes", type=int, default=10 * 1024 * 1024)
    p_hf.add_argument("--max-total-bytes", type=int, default=250 * 1024 * 1024)
    p_hf.add_argument("--json", action="store_true")
    p_hf.set_defaults(func=cmd_hippocamp_fetch)

    p_hi = sub.add_parser("hippocamp-import", help="convert HippoCamp QA annotations into a Jikji eval set")
    p_hi.add_argument("path", help="HippoCamp inner root, e.g. downloaded Adam_Subset")
    p_hi.add_argument("--annotation", default="")
    p_hi.add_argument("--cases", type=int, default=200)
    p_hi.add_argument("--out", default="", help="external eval-set output path; defaults next to the root")
    p_hi.add_argument("--json", action="store_true")
    p_hi.set_defaults(func=cmd_hippocamp_import)

    p_bench = sub.add_parser("bench-run", help="compare raw filesystem search with Jikji-assisted search")
    p_bench.add_argument("path")
    p_bench.add_argument("--eval-set", default="")
    p_bench.add_argument("--modes", default="raw,jikji")
    p_bench.add_argument("--top-k", type=int, default=10)
    p_bench.add_argument("--prepare", action="store_true", help="run jikji prepare before benchmarking")
    p_bench.add_argument("--allow-leak", action="store_true", help="allow answer files inside root for diagnostics only")
    p_bench.add_argument("--json", action="store_true")
    p_bench.set_defaults(func=cmd_bench_run)

    p_iter = sub.add_parser("bench-iterate", help="repeat a deterministic benchmark after code/index improvements")
    p_iter.add_argument("path")
    p_iter.add_argument("--eval-set", required=True)
    p_iter.add_argument("--iterations", type=int, default=20)
    p_iter.add_argument("--modes", default="raw,jikji")
    p_iter.add_argument("--top-k", type=int, default=10)
    p_iter.add_argument("--baseline-report", default="")
    p_iter.add_argument("--out", default="")
    p_iter.add_argument("--json", action="store_true")
    p_iter.set_defaults(func=cmd_bench_iterate)

    p_hb = sub.add_parser("hermes-bench", help="run Hermes raw-vs-Jikji benchmark against an external eval set")
    p_hb.add_argument("path")
    p_hb.add_argument("--eval-set", required=True)
    p_hb.add_argument("--modes", default="raw,jikji")
    p_hb.add_argument("--cases", type=int, default=0, help="limit cases; 0 means all")
    p_hb.add_argument("--out", default="")
    p_hb.add_argument("--hermes-bin", default="hermes")
    p_hb.add_argument("--model", default="", help="Hermes model override; blank uses the current Hermes account default GPT/model")
    p_hb.add_argument("--provider", default="", help="Hermes inference provider override; blank uses the current Hermes account/provider")
    p_hb.add_argument("--timeout", type=int, default=240)
    p_hb.add_argument("--max-turns", type=int, default=20)
    p_hb.add_argument(
        "--fast-max-turns",
        type=int,
        default=1,
        help="Hermes max turns for fast Jikji find-assisted modes; raw modes still use --max-turns",
    )
    p_hb.add_argument("--skills", default="")
    p_hb.add_argument("--candidate-top-k", type=int, default=20, help="inject top Jikji find candidates into Jikji prompts")
    p_hb.add_argument("--retries", type=int, default=1, help="retry a case when Hermes returns no parseable paths")
    p_hb.add_argument("--yolo", action="store_true", help="pass Hermes --yolo --accept-hooks; benchmark will still detect mutations")
    p_hb.add_argument("--allow-leak", action="store_true", help="allow eval/annotation files inside root for diagnostics only")
    p_hb.add_argument("--json", action="store_true")
    p_hb.set_defaults(func=cmd_hermes_bench)

    p_hc = sub.add_parser("hermes-compare", help="gate raw-vs-Jikji Hermes benchmark reports")
    p_hc.add_argument("raw_report")
    p_hc.add_argument("jikji_report")
    p_hc.add_argument("--raw-mode", default="raw")
    p_hc.add_argument("--jikji-mode", default="jikji")
    p_hc.add_argument("--max-token-ratio", type=float, default=0.75)
    p_hc.add_argument("--max-call-ratio", type=float, default=0.75)
    p_hc.add_argument("--max-seconds-ratio", type=float, default=1.0)
    p_hc.add_argument("--max-avg-llm-calls", type=float)
    p_hc.add_argument("--max-p95-llm-calls", type=int)
    p_hc.add_argument("--json", action="store_true")
    p_hc.set_defaults(func=cmd_hermes_compare)

    p_bv = sub.add_parser("benchmark-value-report", help="build a raw-vs-Jikji accuracy/cost value report from saved Hermes artifacts")
    p_bv.add_argument(
        "--raw-report-dir",
        dest="raw_discover_dir",
        metavar="DIR",
        default=".benchmarks/hippocamp-full/_hermes_full_gpt54mini_chunks5",
        help="directory containing saved raw-vs-Jikji full-set Hermes reports",
    )
    p_bv.add_argument("--raw-discover-dir", dest="raw_discover_dir", help=argparse.SUPPRESS)
    p_bv.add_argument(
        "--find-candidate-report",
        dest="answer_pack_report",
        metavar="REPORT",
        default=".benchmarks/hippocamp-full/_hermes_answer_pack_full_20260623_anchorfix/full_answer_pack_aggregate_report.json",
        help="optional aggregate report for Jikji find candidate diagnostics",
    )
    p_bv.add_argument("--answer-pack-report", dest="answer_pack_report", help=argparse.SUPPRESS)
    p_bv.add_argument(
        "--find-candidate-dir",
        dest="answer_pack_dir",
        metavar="DIR",
        default=".benchmarks/hippocamp-full/_hermes_answer_pack_full_20260623_anchorfix",
        help="directory containing per-profile Jikji find candidate reports",
    )
    p_bv.add_argument("--answer-pack-dir", dest="answer_pack_dir", help=argparse.SUPPRESS)
    p_bv.add_argument("--judge-top-k", type=int, default=20)
    p_bv.add_argument("--llm-latency-seconds", type=float, default=1.5)
    p_bv.add_argument("--out", default="docs/jikji-value-report.json")
    p_bv.add_argument("--input-per-1m-usd", type=float, default=0.30)
    p_bv.add_argument("--output-per-1m-usd", type=float, default=2.50)
    p_bv.add_argument("--usd-to-krw", type=float, default=1380.0)
    p_bv.add_argument("--json", action="store_true")
    p_bv.set_defaults(func=cmd_benchmark_value_report)

    p_hs = sub.add_parser("hermes-skill-install", help="install the Jikji skill into ~/.hermes/skills")
    p_hs.add_argument("--dest", default="")
    p_hs.add_argument("--force", action="store_true")
    p_hs.add_argument("--json", action="store_true")
    p_hs.set_defaults(func=cmd_hermes_skill_install)

    p_graph = sub.add_parser("graph", help="inspect/query Jikji LLM Wiki knowledge graph artifacts")
    graph_sub = p_graph.add_subparsers(dest="graph_cmd")
    g_status = graph_sub.add_parser("status", help="show graph/wiki artifact status")
    g_status.add_argument("path", nargs="?", default=".")
    g_status.add_argument("--json", action="store_true")
    g_status.set_defaults(func=cmd_graph, graph_cmd="status")
    g_query = graph_sub.add_parser("query", help="query low-token graph routes")
    g_query.add_argument("path")
    g_query.add_argument("query")
    g_query.add_argument("--top-k", type=int, default=10)
    g_query.add_argument("--json", action="store_true")
    g_query.set_defaults(func=cmd_graph, graph_cmd="query")
    g_explain = graph_sub.add_parser("explain", help="explain graph route and neighbors for one source path")
    g_explain.add_argument("path")
    g_explain.add_argument("source_path")
    g_explain.add_argument("--json", action="store_true")
    g_explain.set_defaults(func=cmd_graph, graph_cmd="explain")

    p_gui = sub.add_parser("gui", help="serve a local web UI for searching, opening, and downloading files")
    p_gui.add_argument("path", nargs="?", default=".")
    p_gui.add_argument("--host", default="127.0.0.1", help="bind host; default is loopback only")
    p_gui.add_argument("--port", type=int, default=8765, help="bind port; use 0 for a random free port")
    p_gui.add_argument("--no-open", action="store_true", help="do not open the browser automatically")
    p_gui.add_argument("--prepare", dest="prepare", action="store_true", help="prepare the root before serving the GUI")
    p_gui.add_argument("--no-prepare", dest="prepare", action="store_false", help=argparse.SUPPRESS)
    p_gui.add_argument("--background", action="store_true", help="start GUI in the background and print a clickable local URL")
    p_gui.add_argument("--json", action="store_true")
    p_gui.set_defaults(prepare=False)
    p_gui.set_defaults(func=cmd_gui)

    p_prep_bg = sub.add_parser("post-install-prepare", help=argparse.SUPPRESS)
    p_prep_bg.add_argument("roots", nargs="+")
    p_prep_bg.add_argument("--max-files", type=int, default=0, help="optional file-count safety cap; 0 disables it")
    p_prep_bg.add_argument("--parse-timeout", type=float, default=5.0)
    p_prep_bg.add_argument("--json", action="store_true")
    p_prep_bg.set_defaults(func=cmd_post_install_prepare)

    p_asi = sub.add_parser(
        "agent-skill-install",
        help="install the Jikji auto-use skill for local agents",
    )
    p_asi.add_argument(
        "--agent",
        action="append",
        default=[],
        help=(
            "agent target: hermes, codex, omx, claude, opencode, openclo, "
            f"nanoclo, generic, {','.join(CUSTOM_AGENT_NAMES)}, or all"
        ),
    )
    p_asi.add_argument(
        "--dest",
        default="",
        help="explicit SKILL.md path for any/custom agent; only valid with one --agent",
    )
    p_asi.add_argument(
        "--prepare-root",
        action="append",
        default=[],
        help="queue this explicit root for post-install prepare; repeatable; defaults to common/document-heavy roots",
    )
    p_asi.add_argument("--no-prepare", action="store_true", help="install the skill without preparing any root")
    p_asi.add_argument("--foreground-prepare", action="store_true", help="wait for post-install prepare instead of running it in the background")
    p_asi.add_argument("--max-files", type=int, default=0, help="optional post-install prepare file-count safety cap; 0 disables it")
    p_asi.add_argument("--parse-timeout", type=float, default=5.0, help="parser timeout for post-install prepare")
    p_asi.add_argument("--force", action="store_true")
    p_asi.add_argument("--json", action="store_true")
    p_asi.set_defaults(func=cmd_agent_skill_install)

    for name in ("codex", "omx", "claude", "opencode", "openclo", "nanoclo"):
        p_agent_alias = sub.add_parser(
            f"{name}-skill-install",
            help=f"install the Jikji auto-use skill for {name}",
        )
        p_agent_alias.add_argument("--dest", default="")
        p_agent_alias.add_argument("--prepare-root", action="append", default=[])
        p_agent_alias.add_argument("--no-prepare", action="store_true")
        p_agent_alias.add_argument("--foreground-prepare", action="store_true")
        p_agent_alias.add_argument("--max-files", type=int, default=0, help="optional file-count safety cap; 0 disables it")
        p_agent_alias.add_argument("--parse-timeout", type=float, default=5.0)
        p_agent_alias.add_argument("--force", action="store_true")
        p_agent_alias.add_argument("--json", action="store_true")
        p_agent_alias.set_defaults(func=cmd_agent_skill_install, agent=[name])

    p_export = sub.add_parser(
        "skill-export",
        help="print or write the universal Jikji SKILL.md for any local agent",
    )
    p_export.add_argument("--dest", default="", help="write SKILL.md to an arbitrary agent skill path")
    p_export.add_argument(
        "--prepare-root",
        action="append",
        default=[],
        help="queue this explicit root for post-install prepare after writing --dest; repeatable; defaults to common/document-heavy roots",
    )
    p_export.add_argument("--no-prepare", action="store_true", help="write --dest without preparing any root")
    p_export.add_argument("--foreground-prepare", action="store_true")
    p_export.add_argument("--max-files", type=int, default=0, help="optional file-count safety cap; 0 disables it")
    p_export.add_argument("--parse-timeout", type=float, default=5.0)
    p_export.add_argument("--force", action="store_true")
    p_export.add_argument("--json", action="store_true")
    p_export.set_defaults(func=cmd_skill_export)

    p_suite = sub.add_parser("hippocamp-suite", help="run bounded multi-profile HippoCamp benchmark suite")
    p_suite.add_argument("dest")
    p_suite.add_argument("--profiles", default="Adam,Bei,Victoria")
    p_suite.add_argument("--split", default="Subset")
    p_suite.add_argument("--max-files", type=int, default=120)
    p_suite.add_argument("--max-file-bytes", type=int, default=10 * 1024 * 1024)
    p_suite.add_argument("--max-total-bytes", type=int, default=250 * 1024 * 1024)
    p_suite.add_argument("--cases", type=int, default=200)
    p_suite.add_argument("--top-k", type=int, default=10)
    p_suite.add_argument("--no-fetch", action="store_true")
    p_suite.add_argument("--json", action="store_true")
    p_suite.set_defaults(func=cmd_hippocamp_suite)

    p_beir_import = sub.add_parser("beir-import", help="download/materialize one public BEIR dataset as local files")
    p_beir_import.add_argument("dest")
    p_beir_import.add_argument("--dataset", default="scifact")
    p_beir_import.add_argument("--split", default="test")
    p_beir_import.add_argument("--cases", type=int, default=200)
    p_beir_import.add_argument("--json", action="store_true")
    p_beir_import.set_defaults(func=cmd_beir_import)

    p_beir_suite = sub.add_parser("beir-suite", help="run public BEIR local-file retrieval suite")
    p_beir_suite.add_argument("dest")
    p_beir_suite.add_argument("--datasets", default="scifact,nfcorpus,arguana")
    p_beir_suite.add_argument("--split", default="test")
    p_beir_suite.add_argument("--cases", type=int, default=200)
    p_beir_suite.add_argument("--top-k", type=int, default=10)
    p_beir_suite.add_argument("--no-prepare", action="store_true")
    p_beir_suite.add_argument("--json", action="store_true")
    p_beir_suite.set_defaults(func=cmd_beir_suite)

    p_edith_summary = sub.add_parser("edith-summary", help="inspect public EDiTh benchmark metadata")
    p_edith_summary.add_argument("dest")
    p_edith_summary.add_argument("--json", action="store_true")
    p_edith_summary.set_defaults(func=cmd_edith_summary)

    p_edith_import = sub.add_parser(
        "edith-import",
        help="materialize a bounded EDiTh enterprise-PDF file-retrieval benchmark",
    )
    p_edith_import.add_argument("dest")
    p_edith_import.add_argument("--cases", type=int, default=8)
    p_edith_import.add_argument("--max-docs", type=int, default=60)
    p_edith_import.add_argument(
        "--max-download-bytes",
        type=int,
        default=2_000_000_000,
        help="compressed archive transfer budget for streaming PDFs (default: 2GB)",
    )
    p_edith_import.add_argument("--no-docs", action="store_true", help="download metadata/eval only; do not stream-extract PDFs")
    p_edith_import.add_argument("--json", action="store_true")
    p_edith_import.set_defaults(func=cmd_edith_import)

    p_edith_suite = sub.add_parser("edith-suite", help="run bounded public EDiTh raw-vs-Jikji suite")
    p_edith_suite.add_argument("dest")
    p_edith_suite.add_argument("--cases", type=int, default=8)
    p_edith_suite.add_argument("--max-docs", type=int, default=60)
    p_edith_suite.add_argument("--top-k", type=int, default=10)
    p_edith_suite.add_argument(
        "--max-download-bytes",
        type=int,
        default=2_000_000_000,
        help="compressed archive transfer budget for streaming PDFs (default: 2GB)",
    )
    p_edith_suite.add_argument("--no-docs", action="store_true", help="metadata/eval only; skips prepare/Jikji comparison")
    p_edith_suite.add_argument("--no-prepare", action="store_true")
    p_edith_suite.add_argument("--json", action="store_true")
    p_edith_suite.set_defaults(func=cmd_edith_suite)

    p_publicdata_build = sub.add_parser(
        "publicdata-build",
        help="build a messy Korean public-data local-agent benchmark corpus",
    )
    p_publicdata_build.add_argument("dest")
    p_publicdata_build.add_argument("--target-docs", type=int, default=90)
    p_publicdata_build.add_argument("--max-id", type=int, default=700)
    p_publicdata_build.add_argument("--cases", type=int, default=40)
    p_publicdata_build.add_argument("--seed", type=int, default=20260529)
    p_publicdata_build.add_argument("--json", action="store_true")
    p_publicdata_build.set_defaults(func=cmd_publicdata_build)

    p_publicdata_suite = sub.add_parser(
        "publicdata-suite",
        help="build and run deterministic public-data benchmark diagnostics",
    )
    p_publicdata_suite.add_argument("dest")
    p_publicdata_suite.add_argument("--target-docs", type=int, default=90)
    p_publicdata_suite.add_argument("--max-id", type=int, default=700)
    p_publicdata_suite.add_argument("--cases", type=int, default=40)
    p_publicdata_suite.add_argument("--seed", type=int, default=20260529)
    p_publicdata_suite.add_argument("--top-k", type=int, default=10)
    p_publicdata_suite.add_argument("--json", action="store_true")
    p_publicdata_suite.set_defaults(func=cmd_publicdata_suite)

    p_workspacebench_build = sub.add_parser(
        "workspacebench-build",
        help="build a bounded Workspace-Bench-Lite file-discovery corpus",
    )
    p_workspacebench_build.add_argument("dest")
    p_workspacebench_build.add_argument("--max-tasks", type=int, default=12)
    p_workspacebench_build.add_argument("--start", type=int, default=0)
    p_workspacebench_build.add_argument("--max-file-bytes", type=int, default=DEFAULT_MAX_FILE_BYTES)
    p_workspacebench_build.add_argument("--max-total-bytes", type=int, default=DEFAULT_MAX_TOTAL_BYTES)
    p_workspacebench_build.add_argument("--json", action="store_true")
    p_workspacebench_build.set_defaults(func=cmd_workspacebench_build)

    p_workspacebench_suite = sub.add_parser(
        "workspacebench-suite",
        help="build and run Workspace-Bench-Lite file-discovery diagnostics",
    )
    p_workspacebench_suite.add_argument("dest")
    p_workspacebench_suite.add_argument("--max-tasks", type=int, default=12)
    p_workspacebench_suite.add_argument("--start", type=int, default=0)
    p_workspacebench_suite.add_argument("--top-k", type=int, default=10)
    p_workspacebench_suite.add_argument("--max-file-bytes", type=int, default=DEFAULT_MAX_FILE_BYTES)
    p_workspacebench_suite.add_argument("--max-total-bytes", type=int, default=DEFAULT_MAX_TOTAL_BYTES)
    p_workspacebench_suite.add_argument("--json", action="store_true")
    p_workspacebench_suite.set_defaults(func=cmd_workspacebench_suite)

    p_hardbench_build = sub.add_parser(
        "hardbench-build",
        help="build a large hard mixed PDF/HWP public-document benchmark corpus",
    )
    p_hardbench_build.add_argument("dest")
    p_hardbench_build.add_argument("--target-docs", type=int, default=180)
    p_hardbench_build.add_argument("--max-data-idx", type=int, default=180)
    p_hardbench_build.add_argument("--cases", type=int, default=240)
    p_hardbench_build.add_argument("--seed", type=int, default=20260603)
    p_hardbench_build.add_argument("--max-file-bytes", type=int, default=80 * 1024 * 1024)
    p_hardbench_build.add_argument("--difficulty", choices=("hard", "extreme"), default="hard")
    p_hardbench_build.add_argument("--source-dir", default="", help="use pre-downloaded local public documents instead of crawling KOGL")
    p_hardbench_build.add_argument("--max-total-bytes", type=int, default=0, help="cap selected source bytes when --source-dir is used; 0 means no cap")
    p_hardbench_build.add_argument("--json", action="store_true")
    p_hardbench_build.set_defaults(func=cmd_hardbench_build)

    p_hardbench_suite = sub.add_parser(
        "hardbench-suite",
        help="build and run hard mixed-document benchmark diagnostics",
    )
    p_hardbench_suite.add_argument("dest")
    p_hardbench_suite.add_argument("--target-docs", type=int, default=180)
    p_hardbench_suite.add_argument("--max-data-idx", type=int, default=180)
    p_hardbench_suite.add_argument("--cases", type=int, default=240)
    p_hardbench_suite.add_argument("--seed", type=int, default=20260603)
    p_hardbench_suite.add_argument("--top-k", type=int, default=10)
    p_hardbench_suite.add_argument("--max-file-bytes", type=int, default=80 * 1024 * 1024)
    p_hardbench_suite.add_argument("--difficulty", choices=("hard", "extreme"), default="hard")
    p_hardbench_suite.add_argument("--source-dir", default="", help="use pre-downloaded local public documents instead of crawling KOGL")
    p_hardbench_suite.add_argument("--max-total-bytes", type=int, default=0, help="cap selected source bytes when --source-dir is used; 0 means no cap")
    p_hardbench_suite.add_argument("--json", action="store_true")
    p_hardbench_suite.set_defaults(func=cmd_hardbench_suite)

    hidden_subcommands = {"discover", "search", "brief", "post-install-prepare"}
    sub._choices_actions = [  # noqa: SLF001 - argparse has no public API for hiding compatibility aliases.
        action for action in sub._choices_actions if getattr(action, "dest", "") not in hidden_subcommands
    ]

    args = parser.parse_args(argv)
    if args.cmd is None:
        # Default to safe prepare for agent-skill ergonomics.
        args = parser.parse_args(["prepare", *(argv or [])])
    try:
        return args.func(args)
    except ScanTooLargeError as exc:
        print(
            f"Jikji scan stopped after {exc.count} files because --max-files is {exc.limit}. "
            "Use --max-files 0 to disable this optional cap.",
            file=sys.stderr,
        )
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
