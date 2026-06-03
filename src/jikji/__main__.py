from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

from .agent_brief import brief_markdown, build_agent_brief_payload
from .agent_index import AGENT_DIR_NAME, build_agent_index
from .agent_skill_install import (
    CUSTOM_AGENT_NAMES,
    expand_agent_selection,
    install_agent_skill,
    repo_skill_path,
)
from .beir import materialize_beir_dataset, run_beir_suite
from .config import Config
from .edith import edith_answer_summary, materialize_edith_dataset, run_edith_suite
from .eval import (
    analyze_eval_failures,
    generate_eval_set,
    generate_realistic_eval_set,
    run_eval,
    search,
)
from .hardbench import build_hard_benchmark, run_hard_benchmark_suite
from .hermes_bench import install_hermes_skill, run_hermes_benchmark
from .hippocamp import fetch_subset, import_eval_set, run_benchmark, run_suite
from .holdout_eval import generate_holdout_eval_set
from .improvement_loop import run_improvement_loop
from .publicdata_bench import build_publicdata_benchmark, run_publicdata_suite
from .search_index import instant_index_path
from .version import __version__
from .workspacebench import (
    DEFAULT_MAX_FILE_BYTES,
    DEFAULT_MAX_TOTAL_BYTES,
    build_workspacebench_benchmark,
    run_workspacebench_suite,
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
    if args.exclude:
        cfg.ignore_patterns.extend(args.exclude)
    return cfg


def cmd_prepare(args) -> int:
    root = Path(args.path).expanduser().resolve()
    cfg = _config_from_args(args)
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


def _clean_targets(root: Path) -> list[Path]:
    return [root / AGENT_DIR_NAME, root / "000_JIKJI_AGENT_MAP.md"]


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
    payload = {
        "root": str(root),
        "ok": allowed or not existing,
        "reason": reason,
        "dry_run": args.dry_run,
        "removed": [],
        "would_remove": [str(p) for p in existing],
        "preserved_original_files": True,
    }
    if existing and not allowed:
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
        for p in existing:
            try:
                if p.is_dir():
                    shutil.rmtree(p)
                else:
                    p.unlink()
                removed.append(str(p))
            except OSError as exc:
                payload.setdefault("errors", []).append({"path": str(p), "error": str(exc)})
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
    for candidate in (root / "000_JIKJI_AGENT_MAP.md", root / ".jikji" / "agent_map.md"):
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
        ".jikji/parse_errors.jsonl",
        ".jikji/agent_map.md",
        "000_JIKJI_AGENT_MAP.md",
    ]
    for rel in required:
        check_file(rel)

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
        },
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


def _manifest_generated_at(root: Path) -> datetime | None:
    path = root / AGENT_DIR_NAME / "manifest.json"
    if not path.exists():
        return None
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
        value = str(raw.get("generated_at") or "")
        if not value:
            return None
        if value.endswith("Z"):
            value = value[:-1] + "+00:00"
        generated = datetime.fromisoformat(value)
        if generated.tzinfo is None:
            generated = generated.replace(tzinfo=UTC)
        return generated
    except (OSError, ValueError, json.JSONDecodeError):
        return None


def _search_index_status(root: Path, *, stale_after_seconds: int) -> tuple[str, bool]:
    index = instant_index_path(root)
    if not index.exists():
        return "missing", True
    generated_at = _manifest_generated_at(root)
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
    index_status, should_prepare = _search_index_status(root, stale_after_seconds=args.stale_after_seconds)
    foreground_prepared = False
    if args.fresh or (should_prepare and args.auto_prepare):
        build_agent_index(root, _search_config_from_args(args))
        index_status = "prepared_now" if should_prepare else "refreshed_now"
        foreground_prepared = True
    elif should_prepare and not args.auto_prepare:
        print(f"No Jikji search index found under {root}. Run: jikji prepare {root}", file=sys.stderr)
        return 1

    ranked = search(root, args.query, top_k=args.top_k)
    background_refresh_started = False
    if index_status == "stale_using_previous_index" and not args.fresh:
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
        if index_status == "stale_using_previous_index":
            suffix = "background refresh started" if background_refresh_started else "using previous index"
            print(f"Jikji index is stale; {suffix}.")
        elif foreground_prepared:
            print(f"Jikji index {index_status.replace('_', ' ')}.")
        for idx, item in enumerate(ranked, 1):
            reasons = ",".join(str(x) for x in (item.get("reasons") or []))
            print(f"{idx:02d} {item.get('score'):>8} {item.get('path')}  [{reasons}]")
    return 0


def cmd_brief(args) -> int:
    root = Path(args.path).expanduser().resolve()
    index_status, should_prepare = _search_index_status(root, stale_after_seconds=args.stale_after_seconds)
    foreground_prepared = False
    if args.fresh or (should_prepare and args.auto_prepare):
        build_agent_index(root, _search_config_from_args(args))
        index_status = "prepared_now" if should_prepare else "refreshed_now"
        foreground_prepared = True
    elif should_prepare and not args.auto_prepare:
        print(f"No Jikji search index found under {root}. Run: jikji prepare {root}", file=sys.stderr)
        return 1

    candidates = search(root, args.query, top_k=args.top_k)
    background_refresh_started = False
    if index_status == "stale_using_previous_index" and not args.fresh:
        background_refresh_started = _maybe_start_background_refresh(args, root)
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
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(brief_markdown(payload))
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


def cmd_hermes_bench(args) -> int:
    modes = tuple(m.strip() for m in args.modes.split(",") if m.strip())
    result = run_hermes_benchmark(
        Path(args.path),
        eval_set=Path(args.eval_set),
        modes=modes,
        cases_limit=args.cases if args.cases > 0 else None,
        out=Path(args.out).expanduser().resolve() if args.out else None,
        hermes_bin=args.hermes_bin,
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
            "jikji brief ROOT \"query\" --top-k 10 --json."
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
    explicit_roots = bool(raw_roots)
    roots = _select_post_install_roots(raw_roots, explicit_roots=explicit_roots)
    if not roots:
        return {"mode": "none", "roots": []}
    if getattr(args, "foreground_prepare", False):
        return {
            "mode": "foreground",
            "roots": _prepare_roots_foreground(
                roots,
                max_files=getattr(args, "max_files", 100_000),
                parse_timeout=getattr(args, "parse_timeout", 5.0),
            ),
        }
    return _start_background_post_install_prepare(
        roots,
        max_files=getattr(args, "max_files", 100_000),
        parse_timeout=getattr(args, "parse_timeout", 5.0),
    )


def _select_post_install_roots(raw_roots: list[str], *, explicit_roots: bool) -> list[Path]:
    candidates = [Path(item).expanduser() for item in raw_roots]
    if not candidates:
        candidates = _default_post_install_prepare_roots()
    limit = len(candidates) if explicit_roots else min(len(candidates), _auto_post_install_root_limit())
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
        if len(roots) >= limit:
            break
    return roots


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
) -> dict[str, object]:
    log_dir = Path.home() / ".local" / "share" / "jikji" / "post_install"
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
        }
    except OSError as exc:
        return {
            "mode": "background",
            "started": False,
            "error": str(exc),
            "roots": [{"root": str(root), "status": "not_started"} for root in roots],
            "policy": _post_install_load_policy(),
        }


def _auto_post_install_root_limit() -> int:
    policy = _post_install_load_policy()
    return int(policy["max_default_roots"])


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


def _default_post_install_prepare_roots() -> list[Path]:
    """Return common user-content roots for immediate post-install usefulness."""
    home = Path.home()
    candidates: list[Path] = []

    def add(path: Path | str | None) -> None:
        if not path:
            return
        candidates.append(Path(path).expanduser())

    for env_name in ("USERPROFILE", "OneDrive", "OneDriveCommercial", "OneDriveConsumer"):
        add(os.environ.get(env_name))

    for rel in (
        "Documents",
        "Downloads",
        "Desktop",
        "문서",
        "다운로드",
        "바탕화면",
        "Google Drive",
        "GoogleDrive",
        "My Drive",
        "Dropbox",
        "OneDrive",
        "iCloud Drive",
        "Library/Mobile Documents/com~apple~CloudDocs",
    ):
        add(home / rel)

    candidates.extend(_xdg_user_dirs(home).values())

    roots: list[Path] = []
    seen: set[Path] = set()
    for candidate in candidates:
        try:
            resolved = candidate.expanduser().resolve()
        except (OSError, RuntimeError):
            continue
        if resolved in seen or not resolved.exists() or not resolved.is_dir():
            continue
        seen.add(resolved)
        roots.append(resolved)
    return roots


def _xdg_user_dirs(home: Path) -> dict[str, Path]:
    path = home / ".config" / "user-dirs.dirs"
    wanted = {"XDG_DESKTOP_DIR", "XDG_DOWNLOAD_DIR", "XDG_DOCUMENTS_DIR"}
    dirs: dict[str, Path] = {}
    try:
        lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    except OSError:
        return dirs
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        if key not in wanted:
            continue
        value = value.strip().strip('"').replace("$HOME", str(home))
        if value:
            dirs[key] = Path(value).expanduser()
    return dirs


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
    sub = parser.add_subparsers(dest="cmd")

    def add_common(p):
        p.add_argument("path", nargs="?", default=".")
        p.add_argument("--max-files", type=int, default=5000)
        p.add_argument("--include-hidden", action="store_true")
        p.add_argument("--include-sensitive", action="store_true", help="include safety-denied names such as .env or private keys")
        p.add_argument("--exclude", action="append", default=[], help="additional fnmatch pattern to exclude; repeatable")
        p.add_argument("--max-hash-bytes", type=int, default=512 * 1024 * 1024)
        p.add_argument("--parse-timeout", type=float, default=5.0)
        p.add_argument("--doc-text-max-chars", type=int, default=2_000_000)
        p.add_argument("--doc-text-chunk-chars", type=int, default=1_000_000)
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

    p_search = sub.add_parser("search", help="rank likely files from Jikji indexes for a natural-language query")
    p_search.add_argument("path")
    p_search.add_argument("query")
    p_search.add_argument("--top-k", type=int, default=20)
    p_search.add_argument("--fresh", action="store_true", help="run a foreground refresh before searching")
    p_search.add_argument("--no-auto-prepare", dest="auto_prepare", action="store_false", help="do not auto-prepare when the instant index is missing")
    p_search.add_argument("--no-background-refresh", dest="background_refresh", action="store_false", help="do not launch background refresh for stale indexes")
    p_search.add_argument("--stale-after-seconds", type=int, default=24 * 60 * 60, help="mark an index stale after this age; negative disables staleness")
    p_search.add_argument("--max-files", type=int, default=100_000, help="auto-prepare file safety limit")
    p_search.add_argument("--include-hidden", action="store_true")
    p_search.add_argument("--include-sensitive", action="store_true")
    p_search.add_argument("--exclude", action="append", default=[])
    p_search.add_argument("--max-hash-bytes", type=int, default=512 * 1024 * 1024)
    p_search.add_argument("--parse-timeout", type=float, default=5.0)
    p_search.add_argument("--json", action="store_true")
    p_search.set_defaults(auto_prepare=True, background_refresh=True)
    p_search.set_defaults(func=cmd_search)

    p_brief = sub.add_parser(
        "brief",
        help="emit a compact query-specific route brief for local agents",
    )
    p_brief.add_argument("path")
    p_brief.add_argument("query")
    p_brief.add_argument("--top-k", type=int, default=10)
    p_brief.add_argument("--fresh", action="store_true", help="run a foreground refresh before briefing")
    p_brief.add_argument("--no-auto-prepare", dest="auto_prepare", action="store_false", help="do not auto-prepare when the instant index is missing")
    p_brief.add_argument("--no-background-refresh", dest="background_refresh", action="store_false", help="do not launch background refresh for stale indexes")
    p_brief.add_argument("--stale-after-seconds", type=int, default=24 * 60 * 60, help="mark an index stale after this age; negative disables staleness")
    p_brief.add_argument("--max-files", type=int, default=100_000, help="auto-prepare file safety limit")
    p_brief.add_argument("--include-hidden", action="store_true")
    p_brief.add_argument("--include-sensitive", action="store_true")
    p_brief.add_argument("--exclude", action="append", default=[])
    p_brief.add_argument("--max-hash-bytes", type=int, default=512 * 1024 * 1024)
    p_brief.add_argument("--parse-timeout", type=float, default=5.0)
    p_brief.add_argument("--json", action="store_true")
    p_brief.set_defaults(auto_prepare=True, background_refresh=True)
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
    p_hb.add_argument("--timeout", type=int, default=240)
    p_hb.add_argument("--max-turns", type=int, default=20)
    p_hb.add_argument(
        "--fast-max-turns",
        type=int,
        default=1,
        help="Hermes max turns for jikji-fast/map-first modes; raw and brief modes still use --max-turns",
    )
    p_hb.add_argument("--skills", default="")
    p_hb.add_argument("--candidate-top-k", type=int, default=10, help="inject top Jikji search candidates into Jikji tool-mode prompts")
    p_hb.add_argument("--retries", type=int, default=1, help="retry a case when Hermes returns no parseable paths")
    p_hb.add_argument("--yolo", action="store_true", help="pass Hermes --yolo --accept-hooks; benchmark will still detect mutations")
    p_hb.add_argument("--allow-leak", action="store_true", help="allow eval/annotation files inside root for diagnostics only")
    p_hb.add_argument("--json", action="store_true")
    p_hb.set_defaults(func=cmd_hermes_bench)

    p_hs = sub.add_parser("hermes-skill-install", help="install the Jikji skill into ~/.hermes/skills")
    p_hs.add_argument("--dest", default="")
    p_hs.add_argument("--force", action="store_true")
    p_hs.add_argument("--json", action="store_true")
    p_hs.set_defaults(func=cmd_hermes_skill_install)

    p_prep_bg = sub.add_parser("post-install-prepare", help=argparse.SUPPRESS)
    p_prep_bg.add_argument("roots", nargs="+")
    p_prep_bg.add_argument("--max-files", type=int, default=100_000)
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
        help="queue this explicit root for post-install prepare; repeatable; defaults to common user document/download roots",
    )
    p_asi.add_argument("--no-prepare", action="store_true", help="install the skill without preparing any root")
    p_asi.add_argument("--foreground-prepare", action="store_true", help="wait for post-install prepare instead of running it in the background")
    p_asi.add_argument("--max-files", type=int, default=100_000, help="post-install prepare safety limit")
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
        p_agent_alias.add_argument("--max-files", type=int, default=100_000)
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
        help="queue this explicit root for post-install prepare after writing --dest; repeatable; defaults to common user document/download roots",
    )
    p_export.add_argument("--no-prepare", action="store_true", help="write --dest without preparing any root")
    p_export.add_argument("--foreground-prepare", action="store_true")
    p_export.add_argument("--max-files", type=int, default=100_000)
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

    args = parser.parse_args(argv)
    if args.cmd is None:
        # Default to safe prepare for agent-skill ergonomics.
        args = parser.parse_args(["prepare", *(argv or [])])
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
