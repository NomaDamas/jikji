"""Hermes local-agent benchmark runner for Jikji.

This module executes Hermes in non-interactive mode against an external eval set
and records enough evidence to compare raw filesystem search with Jikji-assisted
search. It is intentionally conservative about no-leak runs: expected paths must
come from an eval set outside the target root, and generated eval files inside the
root are rejected unless the caller explicitly opts out.
"""
from __future__ import annotations

import hashlib
import json
import re
import subprocess
import time
from collections import defaultdict
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .agent_brief import build_agent_brief_payload
from .agent_index import AGENT_DIR_NAME, _atomic_write_text
from .agent_skill_install import install_agent_skill
from .eval import _path_fingerprints, _rank_for_expected, _read_jsonl, search


@dataclass
class HermesBenchResult:
    report_path: Path
    metrics: dict[str, Any]


@dataclass
class HermesSkillInstallResult:
    path: Path
    installed: bool
    message: str


def _now_stamp() -> str:
    return datetime.now(tz=UTC).strftime("%Y%m%dT%H%M%SZ")


def _clean_prompt_text(value: Any) -> str:
    """Return subprocess-safe prompt text from parser/index evidence."""
    return str(value).replace("\x00", " ").replace("\r", " ")


def _is_relative_to(path: Path, base: Path) -> bool:
    try:
        path.resolve().relative_to(base.resolve())
        return True
    except ValueError:
        return False


def assert_no_leak_root(
    root: Path,
    eval_set: Path,
    *,
    out: Path | None = None,
    allow_leak: bool = False,
) -> None:
    """Reject benchmark setups where answer files are visible to Hermes."""
    root = Path(root).expanduser().resolve()
    eval_set = Path(eval_set).expanduser().resolve()
    if allow_leak:
        return
    problems: list[str] = []
    if _is_relative_to(eval_set, root):
        problems.append(f"eval set is inside benchmark root: {eval_set}")
    if out is not None and _is_relative_to(Path(out).expanduser().resolve(), root):
        problems.append(f"report/evidence output is inside benchmark root: {out}")
    eval_dir = root / AGENT_DIR_NAME / "eval"
    if eval_dir.exists():
        problems.append(f"generated eval directory is visible inside root: {eval_dir}")
    for pattern in ("*_Subset.json", "*.annotation.json", "hippocamp_eval_set*.jsonl", "eval_set*.jsonl", "*_gold.json", "*.qa.json"):
        for candidate in root.rglob(pattern):
            problems.append(f"possible answer/annotation leak inside root: {candidate}")
    if problems:
        joined = "\n- ".join(problems)
        raise RuntimeError(
            "Hermes benchmark no-leak check failed. Move eval/annotation files outside ROOT "
            "or pass --allow-leak for an explicitly non-comparable diagnostic run.\n- " + joined
        )


def _candidate_lines(root: Path, query: str, *, top_k: int) -> list[str]:
    if top_k <= 0:
        return []
    candidates = search(root, query, top_k=top_k)
    lines = [
        "JIKJI SEARCH RESULT:",
        f"`jikji search {root} {json.dumps(query, ensure_ascii=False)} --top-k {top_k} --json` returned these candidates.",
        "Return paths from this list when any candidate is relevant. Preserve Jikji's order unless there is an obvious reason to rerank.",
        "For broad, duplicate, or generic clues, return several candidates (normally the first 5) instead of only one path; hit@5 matters for ambiguous local-file discovery.",
        "Do not inspect .jikji JSONL/doc_text or browse the filesystem unless no candidate can answer the question.",
    ]
    for idx, item in enumerate(candidates, 1):
        reasons = ",".join(str(x) for x in (item.get("reasons") or []))
        lines.append(f"{idx}. {item.get('path')} | score={item.get('score')} | reasons={reasons}")
        for preview in list(item.get("evidence") or [])[:2]:
            lines.append(f"   evidence: {str(preview)[:240]}")
    return lines


def _fast_candidate_lines(root: Path, query: str, *, top_k: int) -> list[str]:
    if top_k <= 0:
        return [
            "JIKJI MAP-FIRST FAST PATH:",
            "No pre-ranked candidates were requested; return an empty JSON path list.",
        ]
    candidates = search(root, query, top_k=top_k)
    selection_rule = (
        "More than 10 candidates are listed: select the 10 candidate paths whose path/evidence best matches the QUESTION; "
        "never use any path outside this list."
        if len(candidates) > 10
        else "10 or fewer candidates are listed: copy every candidate path into the JSON paths array exactly in the same order."
    )
    lines = [
        "JIKJI MAP-FIRST FAST PATH:",
        "Jikji already did the expensive local discovery pass before Hermes was called.",
        "Do not browse, list, grep, cat, or inspect any filesystem path.",
        selection_rule,
        "Do not invent, summarize, or replace candidates; this is a bounded map handoff.",
        "Candidates:",
    ]
    for idx, item in enumerate(candidates, 1):
        reasons = ",".join(str(x) for x in (item.get("reasons") or [])[:5])
        evidence = "; ".join(
            _clean_prompt_text(x).replace("\n", " ")[:140]
            for x in list(item.get("evidence") or [])[:1]
        )
        line = f"{idx}. {item.get('path')} | score={item.get('score')} | reasons={reasons}"
        if evidence:
            line += f" | evidence={evidence}"
        lines.append(line)
    return lines


def _brief_lines(root: Path, query: str, *, top_k: int) -> list[str]:
    candidates = search(root, query, top_k=top_k)
    payload = build_agent_brief_payload(
        root,
        query,
        top_k=top_k,
        index_status="ready",
        foreground_prepared=False,
        background_refresh_started=False,
        candidates=candidates,
    )
    lines = [
        "JIKJI AGENT BRIEF:",
        f"`jikji brief {root} {json.dumps(query, ensure_ascii=False)} --top-k {top_k} --json` is the intended agent interface.",
        "Actual brief payload follows. Treat it as the canonical Jikji agent-map handoff for this query.",
        json.dumps(payload, ensure_ascii=False, indent=2),
        "Policy: use candidate paths first, preserve relative paths exactly, read original files only for final verification, and never mutate files.",
        "Route order: candidates -> rerun jikji search with sharper query -> .jikji/file_cards.jsonl + chunk_map.jsonl -> .jikji/doc_text -> original files excluding .jikji.",
    ]
    return lines


def _mode_family(mode: str) -> str:
    normalized = mode.strip().lower().replace("_", "-")
    if normalized in {
        "jikji-direct",
        "direct",
        "skill-direct",
        "tool-direct",
        "jikji-skill-direct",
    }:
        return "jikji-direct"
    if normalized in {
        "jikji-fast",
        "fast",
        "map-first",
        "jikji-map-first",
        "jikji-pass-through",
        "pass-through",
    }:
        return "jikji-fast"
    if normalized in {"jikji", "jikji-brief", "brief", "map", "jikji-map"}:
        return "jikji-brief"
    if normalized in {"jikji-tool", "tool", "tool-first"}:
        return "jikji-tool"
    if normalized in {"jikji-passive", "passive"}:
        return "jikji-passive"
    return normalized


def _prompt(root: Path, mode: str, case: dict, *, candidate_top_k: int = 0, retry: bool = False) -> str:
    mode_family = _mode_family(mode)
    base = [
        "You are benchmarking local file discovery. Do not modify, move, rename, or delete files.",
        f"ROOT: {root}",
        f"QUESTION: {case.get('query')}",
        "Return up to 10 relevant paths ranked best-first; return the best path first.",
        "Respond with JSON only: {\"paths\":[\"relative/path\"],\"reason\":\"short\"}",
        "Use relative paths exactly as they appear under ROOT.",
    ]
    if mode_family == "raw":
        base.append("RAW MODE: Do not read or use .jikji or 000_JIKJI_AGENT_MAP.md. Search only original user files/folders.")
    elif mode_family == "jikji-fast":
        base = [
            "You are benchmarking local file discovery. Do not modify, move, rename, or delete files.",
            "Jikji-equipped Hermes mode: answer from the prebuilt map/search handoff, not by exploring.",
            f"ROOT: {root}",
            f"QUESTION: {case.get('query')}",
            "Return JSON only: {\"paths\":[\"relative/path\"],\"reason\":\"short\"}",
            "Use relative paths exactly as shown in the candidate list.",
            "If candidates are present, return at most 10 listed candidate paths.",
        ]
        base.extend(_fast_candidate_lines(root, str(case.get("query") or ""), top_k=candidate_top_k))
    elif mode_family == "jikji-tool":
        base.extend([
            "JIKJI TOOL-FIRST MODE: Treat Jikji as a fast local search tool, not as a pile of files to manually read.",
            "A Jikji search result is provided below. Prefer answering directly from it.",
            "Your job is mostly to pass through the best candidate paths, not to perform a new search.",
            "Do not call rg/find/ls/cat over ROOT and do not read .jikji artifacts unless the candidate list is empty or clearly irrelevant.",
            "This benchmark measures whether a local agent can skip exploratory filesystem work when Jikji has already ranked candidates.",
        ])
        base.extend(_candidate_lines(root, str(case.get("query") or ""), top_k=candidate_top_k))
    elif mode_family == "jikji-brief":
        base.extend([
            "JIKJI BRIEF MODE: Treat Jikji as an agent map/router, not a one-shot answer oracle.",
            "A compact query-specific brief is provided below. Use it to avoid slow raw filesystem exploration.",
            "If the brief contains plausible candidates, return those ranked paths directly.",
            "Only inspect original files or generated Jikji artifacts when the brief is ambiguous or empty.",
            "This benchmark measures whether Jikji can make agent exploration shorter while preserving accuracy.",
        ])
        base.extend(_brief_lines(root, str(case.get("query") or ""), top_k=candidate_top_k))
    elif mode_family == "jikji-passive":
        base.extend([
            "JIKJI PASSIVE MODE: First read 000_JIKJI_AGENT_MAP.md and .jikji/agent_routes.md if present.",
            "Use .jikji/file_index.jsonl, .jikji/folder_index.jsonl, .jikji/document_index.jsonl, and .jikji/doc_text for search.",
            "Only use map/index/cache artifacts needed for file discovery; ignore unrelated generated reports.",
        ])
        base.extend(_candidate_lines(root, str(case.get("query") or ""), top_k=candidate_top_k))
    else:
        raise ValueError(f"unsupported Hermes benchmark mode: {mode}")
    if retry:
        base.extend([
            "RETRY: Your previous attempt returned no parseable file paths.",
            "Do not explain. Output JSON only with at least one relative file path if any candidate is relevant.",
        ])
    return _clean_prompt_text("\n".join(base))


def _extract_json(text: str) -> dict[str, Any]:
    text = text.strip()
    try:
        data = json.loads(text)
        return data if isinstance(data, dict) else {}
    except json.JSONDecodeError:
        pass
    match = re.search(r"\{.*\}", text, flags=re.DOTALL)
    if not match:
        return {}
    try:
        data = json.loads(match.group(0))
    except json.JSONDecodeError:
        return {}
    return data if isinstance(data, dict) else {}


def _normalise_paths(data: dict[str, Any]) -> list[str]:
    raw = data.get("paths") or data.get("path") or data.get("predicted_paths") or []
    if isinstance(raw, str):
        raw = [raw]
    if not isinstance(raw, list):
        return []
    out: list[str] = []
    for item in raw:
        p = str(item).strip().strip("`'")
        if p and p not in out:
            out.append(p)
    return out


def _safe_case_id(value: Any) -> str:
    raw = str(value or "case")
    safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", raw).strip("._") or "case"
    digest = hashlib.sha1(raw.encode("utf-8", "ignore")).hexdigest()[:10]
    return f"{safe[:70]}_{digest}"


def _inventory(root: Path) -> dict[str, tuple[int, int]]:
    out: dict[str, tuple[int, int]] = {}
    for path in root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        try:
            st = path.stat()
            out[path.relative_to(root).as_posix()] = (int(st.st_size), int(st.st_mtime_ns))
        except OSError:
            continue
    return out


def _inventory_delta(before: dict[str, tuple[int, int]], after: dict[str, tuple[int, int]]) -> list[str]:
    changed = []
    for path in sorted(set(before) | set(after)):
        if before.get(path) != after.get(path):
            changed.append(path)
        if len(changed) >= 50:
            break
    return changed


def _metrics(details: list[dict[str, Any]], seconds: float) -> dict[str, Any]:
    total = len(details)
    hits = sum(1 for d in details if d.get("hit"))
    hits_at_1 = sum(1 for d in details if d.get("rank") == 1)
    hits_at_3 = sum(1 for d in details if d.get("rank") is not None and d["rank"] <= 3)
    hits_at_5 = sum(1 for d in details if d.get("rank") is not None and d["rank"] <= 5)
    hits_at_10 = sum(1 for d in details if d.get("rank") is not None and d["rank"] <= 10)
    duplicate_hits_at_10 = sum(
        1 for d in details if d.get("duplicate_rank") is not None and d["duplicate_rank"] <= 10
    )
    by_scenario: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for detail in details:
        by_scenario[str(detail.get("scenario") or "unknown")].append(detail)
    return {
        "cases": total,
        "accuracy": round(hits / total, 4) if total else 0.0,
        "hit_at_1": round(hits_at_1 / total, 4) if total else 0.0,
        "hit_at_3": round(hits_at_3 / total, 4) if total else 0.0,
        "hit_at_5": round(hits_at_5 / total, 4) if total else 0.0,
        "hit_at_10": round(hits_at_10 / total, 4) if total else 0.0,
        "duplicate_or_exact_hit_at_10": round(duplicate_hits_at_10 / total, 4) if total else 0.0,
        "seconds": round(seconds, 3),
        "avg_seconds": round(seconds / total, 3) if total else 0.0,
        "by_scenario": {
            scenario: {
                "cases": len(items),
                "accuracy": round(sum(1 for d in items if d.get("hit")) / len(items), 4),
                "hit_at_3": round(
                    sum(1 for d in items if d.get("rank") is not None and d["rank"] <= 3) / len(items), 4
                ),
                "hit_at_5": round(
                    sum(1 for d in items if d.get("rank") is not None and d["rank"] <= 5) / len(items), 4
                ),
                "hit_at_10": round(
                    sum(1 for d in items if d.get("rank") is not None and d["rank"] <= 10) / len(items), 4
                ),
                "duplicate_or_exact_hit_at_10": round(
                    sum(1 for d in items if d.get("duplicate_rank") is not None and d["duplicate_rank"] <= 10)
                    / len(items),
                    4,
                ),
            }
            for scenario, items in sorted(by_scenario.items())
        },
    }


def run_hermes_benchmark(
    root: Path,
    *,
    eval_set: Path,
    modes: tuple[str, ...] = ("raw", "jikji"),
    cases_limit: int | None = None,
    out: Path | None = None,
    hermes_bin: str = "hermes",
    timeout_s: int = 240,
    max_turns: int = 20,
    fast_max_turns: int = 1,
    skills: str = "",
    candidate_top_k: int = 10,
    retries: int = 1,
    allow_leak: bool = False,
    yolo: bool = False,
) -> HermesBenchResult:
    root = Path(root).expanduser().resolve()
    eval_set = Path(eval_set).expanduser().resolve()
    if out is None:
        out = eval_set.parent / f"hermes_benchmark_{root.name}_{_now_stamp()}.json"
    out = Path(out).expanduser().resolve()
    assert_no_leak_root(root, eval_set, out=out, allow_leak=allow_leak)
    cases = _read_jsonl(eval_set)
    if cases_limit is not None:
        cases = cases[: max(0, cases_limit)]
    if not cases:
        raise FileNotFoundError(f"No Hermes benchmark cases found: {eval_set}")
    fingerprints = _path_fingerprints(root)
    evidence_dir = out.with_suffix("")
    evidence_dir.mkdir(parents=True, exist_ok=True)

    report: dict[str, Any] = {
        "root": str(root),
        "eval_set": str(eval_set),
        "hermes_bin": hermes_bin,
        "mode_protocols": {
            "raw": "Hermes searches original files/folders and must ignore Jikji artifacts.",
            "jikji-fast": "Map-first Jikji handoff; Hermes receives only ranked paths/evidence and is told not to browse.",
            "jikji": "Alias for jikji-brief: query-specific Jikji route brief and candidates are provided to avoid raw browsing.",
            "jikji-brief": "Agent-map brief handoff; Hermes receives candidate paths, evidence, and fallback route order.",
            "jikji-tool": "Tool-first Jikji handoff; candidate list replaces exploratory filesystem work.",
            "jikji-direct": (
                "Skill-direct Jikji handoff; the agent invokes Jikji search and accepts "
                "the ranked map candidates without an exploratory Hermes chat turn."
            ),
            "jikji-passive": "Legacy/passive map-reading mode; Hermes may inspect Jikji artifacts.",
        },
        "modes": {},
        "no_leak": not allow_leak,
    }
    for mode in modes:
        mode = mode.strip()
        mode_family = _mode_family(mode)
        details: list[dict[str, Any]] = []
        started = time.perf_counter()
        for idx, case in enumerate(cases, 1):
            case_started = time.perf_counter()
            # Direct mode is an in-process Jikji search call: it never invokes
            # the agent shell and cannot mutate the benchmark corpus. Avoid a
            # full per-case filesystem inventory here because that would hide
            # the actual "Everything-style prebuilt map" latency benefit.
            before = {} if mode_family == "jikji-direct" else _inventory(root)
            timeout = False
            returncode = 0
            attempts: list[dict[str, Any]] = []
            stdout = ""
            stderr = ""
            predicted: list[str] = []
            max_attempts = max(1, 1 + int(retries or 0))
            attempt_max_turns = 0
            candidates: list[dict[str, Any]] = []
            if mode_family == "jikji-direct":
                attempt_started = time.perf_counter()
                try:
                    candidates = search(root, str(case.get("query") or ""), top_k=candidate_top_k)
                    predicted = [str(item.get("path") or "") for item in candidates if str(item.get("path") or "")]
                    attempt_stdout = json.dumps(
                        {
                            "paths": predicted,
                            "reason": "Jikji skill-direct mode returned prebuilt map/search candidates without a Hermes exploratory chat turn.",
                            "candidates": candidates,
                        },
                        ensure_ascii=False,
                        indent=2,
                    )
                    attempt_stderr = ""
                    attempt_returncode = 0
                except Exception as exc:  # pragma: no cover - defensive benchmark reporting
                    attempt_stdout = ""
                    attempt_stderr = str(exc)
                    attempt_returncode = -1
                stdout = attempt_stdout
                stderr = attempt_stderr
                returncode = attempt_returncode
                attempts.append({
                    "attempt": 1,
                    "returncode": attempt_returncode,
                    "timeout": False,
                    "seconds": round(time.perf_counter() - attempt_started, 3),
                    "predicted_paths": predicted,
                    "stdout_tail": attempt_stdout[-800:],
                    "tool": "jikji search",
                })
            else:
                for attempt in range(max_attempts):
                    prompt = _prompt(
                        root,
                        mode,
                        case,
                        candidate_top_k=candidate_top_k if mode_family.startswith("jikji") else 0,
                        retry=attempt > 0,
                    )
                    attempt_max_turns = fast_max_turns if mode_family == "jikji-fast" else max_turns
                    cmd = [hermes_bin, "chat", "-Q", "--max-turns", str(attempt_max_turns)]
                    if yolo:
                        cmd.extend(["--yolo", "--accept-hooks"])
                    if skills:
                        cmd.extend(["--skills", skills])
                    cmd.extend(["-q", prompt])
                    attempt_started = time.perf_counter()
                    attempt_timeout = False
                    try:
                        proc = subprocess.run(cmd, cwd=str(root), text=True, capture_output=True, timeout=timeout_s, check=False)
                        attempt_stdout = proc.stdout or ""
                        attempt_stderr = proc.stderr or ""
                        attempt_returncode = proc.returncode
                    except subprocess.TimeoutExpired as exc:
                        attempt_timeout = True
                        attempt_stdout = (exc.stdout or "") if isinstance(exc.stdout, str) else ""
                        attempt_stderr = (exc.stderr or "") if isinstance(exc.stderr, str) else ""
                        attempt_returncode = -1
                    except (FileNotFoundError, OSError) as exc:
                        attempt_stdout = ""
                        attempt_stderr = str(exc)
                        attempt_returncode = -1
                    parsed = _extract_json(attempt_stdout or attempt_stderr)
                    predicted = _normalise_paths(parsed)
                    stdout = attempt_stdout
                    stderr = attempt_stderr
                    returncode = attempt_returncode
                    timeout = timeout or attempt_timeout
                    attempts.append({
                        "attempt": attempt + 1,
                        "returncode": attempt_returncode,
                        "timeout": attempt_timeout,
                        "seconds": round(time.perf_counter() - attempt_started, 3),
                        "predicted_paths": predicted,
                        "stdout_tail": attempt_stdout[-800:],
                    })
                    if predicted or attempt_returncode == -1:
                        break
            after = {} if mode_family == "jikji-direct" else _inventory(root)
            mutated_paths = [] if mode_family == "jikji-direct" else _inventory_delta(before, after)
            elapsed = time.perf_counter() - case_started
            raw_output = "\n\n".join(
                [
                    f"=== attempt {attempt['attempt']} rc={attempt['returncode']} timeout={attempt['timeout']} ===\n"
                    f"{attempt['stdout_tail']}"
                    for attempt in attempts
                ]
            )
            if stderr:
                raw_output += "\nSTDERR:\n" + stderr
            evidence_path = evidence_dir / f"{mode}_{idx:04d}_{_safe_case_id(case.get('id'))}.txt"
            _atomic_write_text(evidence_path, raw_output)
            expected = {str(p) for p in (case.get("expected_paths") or [])}
            ranked_predicted = [{"path": p} for p in predicted]
            rank = _rank_for_expected(ranked_predicted, expected, fingerprints, mode="exact")
            hash_rank = _rank_for_expected(ranked_predicted, expected, fingerprints, mode="hash")
            duplicate_rank = _rank_for_expected(ranked_predicted, expected, fingerprints, mode="duplicate")
            if mutated_paths:
                rank = None
                hash_rank = None
                duplicate_rank = None
            hit = rank is not None
            details.append({
                "id": case.get("id"),
                "scenario": case.get("scenario"),
                "query": case.get("query"),
                "expected_count": len(expected),
                "expected_paths": sorted(expected),
                "predicted_paths": predicted,
                "rank": rank,
                "hash_rank": hash_rank,
                "duplicate_rank": duplicate_rank,
                "hit": hit,
                "returncode": returncode,
                "timeout": timeout,
                "mutated_paths": mutated_paths,
                "attempts": attempts,
                "mode_family": mode_family,
                "candidate_top_k": candidate_top_k if mode_family.startswith("jikji") else 0,
                "max_turns": attempt_max_turns,
                "agent_chat_turns": 0 if mode_family == "jikji-direct" else attempt_max_turns,
                "seconds": round(elapsed, 3),
                "output_path": str(evidence_path),
                "stdout_tail": (stdout or raw_output)[-1200:],
            })
        seconds = time.perf_counter() - started
        report["modes"][mode] = {"metrics": _metrics(details, seconds), "details": details}
    _atomic_write_text(out, json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    return HermesBenchResult(out, {mode: data["metrics"] for mode, data in report["modes"].items()})


def install_hermes_skill(*, dest: Path | None = None, force: bool = False) -> HermesSkillInstallResult:
    result = install_agent_skill("hermes", dest=dest, force=force)
    return HermesSkillInstallResult(result.path, result.installed, result.message)
