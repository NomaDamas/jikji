#!/usr/bin/env python3
"""Run synthetic search tasks through the installed Claude agent skill path."""
from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))
from common import Binary, Sandbox, guard_production_index, prepare

DISCOVERY = ("filename", "content", "content_multi", "subfolder", "korean_path", "media_name")
SIGNAL = "signal"


def _case_prompt(case: dict[str, Any], fixture: Path) -> tuple[str, bool, bool, Path]:
    root = fixture / case["root"]
    category = case["category"]
    query = str(case.get("query", ""))
    if category in DISCOVERY:
        if category == "filename":
            text = f'Find the file under {root} whose NAME matches "{query}" and tell me its path.'
        elif category in ("content", "content_multi"):
            text = f'Which document under {root} contains the words "{query}"? Give the path.'
        elif category == "media_name":
            text = f'Find the photo, recording or video under {root} whose name matches "{query}" and give its path.'
        elif category == "subfolder":
            text = f'Find the document under {root} that matches "{query}" and give its path.'
        else:
            text = f'{root} 아래에서 이름이나 내용에 "{query}"가 포함된 파일을 찾아 경로를 알려 주세요.'
        return text, True, False, root
    if category == SIGNAL:
        if query.startswith("/") or any(c in query for c in "[](){}*+?|^"):
            text = f'In {root}, list every line matching the regular expression {query}'
        else:
            text = f'In {root}, find where this exact symbol or code snippet appears: {query}'
        return text, False, True, root
    targets = case.get("expect", {}).get("targets") or []
    target = targets[0] if targets else ""
    path = root / target
    return f'Read {path} and quote its first sentence.', False, False, root


def _select(cases: list[dict[str, Any]], total: int, fixture: Path) -> list[dict[str, Any]]:
    groups: dict[str, list[dict[str, Any]]] = {}
    for c in cases:
        groups.setdefault(c.get("category", ""), []).append(c)
    order = [c for c in DISCOVERY if groups.get(c)] + ([SIGNAL] if groups.get(SIGNAL) else [])
    # Known-path cases are deliberately appended as a separate deterministic queue.
    known = [c for c in cases if c.get("category") not in DISCOVERY and c.get("category") != SIGNAL]
    if known:
        order.append("__known__")
        groups["__known__"] = known
    chosen: list[dict[str, Any]] = []
    pos = {k: 0 for k in order}
    while len(chosen) < total and order:
        progressed = False
        for category in order:
            queue = groups[category]
            if pos[category] < len(queue):
                chosen.append(queue[pos[category]])
                pos[category] += 1
                progressed = True
                if len(chosen) >= total:
                    break
        if not progressed:
            break
    return chosen


def _event_tool_calls(obj: Any) -> list[tuple[str, str]]:
    found: list[tuple[str, str]] = []
    if not isinstance(obj, dict):
        return found
    typ = obj.get("type")
    if typ == "tool_use":
        name = str(obj.get("name", ""))
        inp = obj.get("input", {})
        cmd = str(inp.get("command", "")) if isinstance(inp, dict) else ""
        found.append((name, cmd))
    message = obj.get("message")
    content = obj.get("content") or (message.get("content") if isinstance(message, dict) else None)
    if isinstance(content, list):
        for item in content:
            found.extend(_event_tool_calls(item))
    return found


def _classify(name: str, command: str) -> str:
    if name.lower() != "bash":
        return {"read": "read", "grep": "grep", "glob": "glob"}.get(name.lower(), name.lower() or "other")
    try:
        parts = shlex.split(command)
    except ValueError:
        parts = command.split()
    if not parts:
        return "other"
    base = Path(parts[0]).name
    if base == "jikji" or " jikji " in f" {command} ":
        return "jikji_find" if "find" in parts else "jikji"
    return {"grep": "grep", "rg": "rg", "find": "find", "ls": "ls", "tree": "tree", "fd": "fd", "cat": "cat"}.get(base, "other")


def _run_agent(command: list[str], env: dict[str, str], cwd: Path) -> tuple[list[dict[str, Any]], str, float, float, str]:
    start = time.perf_counter()
    proc = subprocess.run(command, cwd=cwd, env=env, capture_output=True, text=True, errors="replace", timeout=900)
    duration = (time.perf_counter() - start) * 1000
    events: list[dict[str, Any]] = []
    for line in proc.stdout.splitlines():
        try:
            value = json.loads(line)
            if isinstance(value, dict):
                events.append(value)
        except json.JSONDecodeError:
            continue
    answer = ""
    cost = 0.0
    for event in events:
        if event.get("type") == "result":
            answer = str(event.get("result", ""))
            cost = float(event.get("total_cost_usd") or event.get("cost_usd") or 0.0)
    if not answer and events:
        answer = str(events[-1].get("result", ""))
    return events, answer, cost, duration, proc.stderr[-1000:]


def _evaluate(case: dict[str, Any], prompt: str, discovery: bool, signal: bool, events: list[dict[str, Any]], answer: str, cost: float, duration: float, stderr: str, asked_root: Path) -> dict[str, Any]:
    calls: list[tuple[str, str, str]] = []
    pattern_seen = False
    for event in events:
        raw = json.dumps(event, ensure_ascii=False)
        if "pattern_syntax_not_supported" in raw:
            pattern_seen = True
        for name, cmd in _event_tool_calls(event):
            calls.append((name, cmd, _classify(name, cmd)))
    first = next((x[2] for x in calls if x[2] != "skill"), None)
    jikji = [x for x in calls if x[2] == "jikji_find"]
    raw_classes = {"grep", "rg", "find", "ls", "glob", "tree", "fd", "cat"}
    raw_before = any(x[2] in raw_classes for x in calls[: next((i for i, x in enumerate(calls) if x[2] == "jikji_find"), len(calls))])
    scope = False
    root_s = str(asked_root)
    for _, cmd, cls in jikji:
        if root_s in cmd or str(asked_root.resolve()) in cmd:
            scope = True
    targets = case.get("expect", {}).get("targets") or []
    answer_correct = (not discovery) or any(Path(t).name in answer for t in targets)
    signal_followed = bool(signal and pattern_seen and any(x[2] in {"grep", "rg"} for x in calls))
    return {"id": case.get("id"), "category": case.get("category"), "prompt": prompt, "tool_calls": [{"tool": n, "command": c[:160], "class": k} for n, c, k in calls], "first_discovery_tool": first, "jikji_calls": len(jikji), "raw_search_before_jikji": raw_before, "used_scope_command": scope, "answer_correct": answer_correct, "jikji_expected": discovery, "signal_followed": signal_followed if signal else None, "answer": answer, "cost_usd": cost, "duration_ms": round(duration, 1), "stderr": stderr}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True, dest="binary")
    ap.add_argument("--fixture", required=True, type=Path)
    ap.add_argument("--tasks", type=int, default=12)
    ap.add_argument("--model", default="haiku")
    ap.add_argument("--max-budget-usd", type=float, default=0.5)
    ap.add_argument("--out", type=Path, default=Path("/tmp/search-eval/agent_live_report.json"))
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    fixture = args.fixture.resolve()
    cases_data = json.loads((fixture / "cases.json").read_text(encoding="utf-8"))
    cases = cases_data["cases"]
    selected = _select(cases, max(0, args.tasks), fixture)
    binary = Binary.parse(args.binary)
    prompts = []
    for case in selected:
        prompt, discovery, signal, root = _case_prompt(case, fixture)
        prompts.append((case, prompt, discovery, signal, root))
    shim = Path(tempfile.mkdtemp(prefix="agent-live-"))
    (shim / "jikji").symlink_to(Path(binary.path).resolve())
    command_base = ["/home/cheol/.local/bin/claude", "-p", "<PROMPT>", "--output-format", "stream-json", "--verbose", "--no-session-persistence", "--model", args.model, "--max-budget-usd", str(args.max_budget_usd), "--permission-mode", "dontAsk", "--permission-prompts", "none", "--add-dir", str(fixture), "--tools", "Bash,Read,Grep,Glob,Skill", "--allowedTools", "Bash(jikji:*) Bash(grep:*) Bash(rg:*) Bash(ls:*) Bash(find:*) Bash(cat:*) Read(*) Grep(*) Glob(*) Skill(*)"]
    if args.dry_run:
        print(json.dumps({"tasks": [{"id": c.get("id"), "prompt": p, "command": [x if x != "<PROMPT>" else p for x in command_base]} for c, p, *_ in prompts]}, ensure_ascii=False, indent=2))
        import shutil
        shutil.rmtree(shim, ignore_errors=True)
        return 0
    root_dir = shim / "cwd"
    root_dir.mkdir()
    sandbox = Sandbox(shim / "sandbox")
    before = guard_production_index()
    env = {k: v for k, v in os.environ.items() if not k.startswith("JIKJI_")}
    env.update(PATH=str(shim) + os.pathsep + os.environ.get("PATH", ""), JIKJI_DATA_DIR=str(sandbox.base / "data"), XDG_DATA_HOME=str(sandbox.base / "xdg"))
    results = []
    try:
        roots = {str(v) for v in cases_data.get("roots", {}).values()}
        for rel in roots:
            prepare(binary, sandbox, fixture / rel)
        for case, prompt, discovery, signal, root in prompts:
            events, answer, cost, duration, stderr = _run_agent([x if x != "<PROMPT>" else prompt for x in command_base], env, root_dir)
            results.append(_evaluate(case, prompt, discovery, signal, events, answer, cost, duration, stderr, root))
    finally:
        after = guard_production_index()
        if not os.environ.get("SEARCH_EVAL_KEEP"):
            import shutil
            shutil.rmtree(shim, ignore_errors=True)
    disc = [r for r in results if r["jikji_expected"]]
    aggregate = {"discovery_count": len(disc), "jikji_first_rate": sum(r["first_discovery_tool"] == "jikji_find" for r in disc) / len(disc) if disc else 0.0, "answer_correct_rate": sum(bool(r["answer_correct"]) for r in results) / len(results) if results else 0.0, "avg_jikji_calls": sum(r["jikji_calls"] for r in results) / len(results) if results else 0.0, "non_discovery_jikji_rate": sum(r["jikji_calls"] > 0 for r in results if not r["jikji_expected"]) / max(1, sum(not r["jikji_expected"] for r in results)), "total_cost_usd": sum(r["cost_usd"] for r in results)}
    report = {"binary": binary.name, "fixture": str(fixture), "tasks": results, "aggregate": aggregate, "production_index_before": before, "production_index_after": after, "production_index_unchanged": before == after}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
