"""Shared helpers for the Jikji search evaluation harness (stdlib only).

Every Jikji process started by this harness runs with an isolated data directory
(`JIKJI_DATA_DIR`, `XDG_DATA_HOME`, `HOME`), so the user's real index is never read for
writing or modified. `guard_production_index()` records the real index file's size/mtime so a
run can prove it was left untouched.
"""

from __future__ import annotations

import hashlib
import json
import os
import shlex
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

PRODUCTION_DB = Path.home() / ".local/share/jikji/index.sqlite"


@dataclass(frozen=True)
class Binary:
    """A Jikji executable under test (`name` is the label used in reports)."""

    name: str
    path: str

    @staticmethod
    def parse(spec: str) -> Binary:
        name, _, path = spec.partition("=")
        if not path:
            raise SystemExit(f"binary must be NAME=PATH, got {spec!r}")
        return Binary(name, str(Path(path).expanduser()))


@dataclass
class Sandbox:
    """One isolated data directory + HOME for a single binary."""

    base: Path
    extra_env: dict[str, str] = field(default_factory=dict)

    def __post_init__(self) -> None:
        for sub in ("data", "home", "xdg"):
            (self.base / sub).mkdir(parents=True, exist_ok=True)

    def env(self, extra: dict[str, str] | None = None) -> dict[str, str]:
        env = {k: v for k, v in os.environ.items() if not k.startswith("JIKJI_")}
        env.update(
            JIKJI_DATA_DIR=str(self.base / "data"),
            XDG_DATA_HOME=str(self.base / "xdg"),
            HOME=str(self.base / "home"),
        )
        env.update(self.extra_env)
        env.update(extra or {})
        return env


def guard_production_index() -> dict[str, Any]:
    """Snapshot of the real index file so a run can show it was not touched."""
    if not PRODUCTION_DB.exists():
        return {"exists": False}
    st = PRODUCTION_DB.stat()
    return {"exists": True, "size": st.st_size, "mtime_ns": st.st_mtime_ns}


def sh_run(argv: list[str], env: dict[str, str], timeout: float = 600.0) -> tuple[int, str, str, float]:
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(argv, env=env, capture_output=True, timeout=timeout, text=True, errors="replace")
        return proc.returncode, proc.stdout, proc.stderr, (time.perf_counter() - t0) * 1000
    except subprocess.TimeoutExpired:
        return 124, "", "timeout", (time.perf_counter() - t0) * 1000


def prepare(binary: Binary, sandbox: Sandbox, root: Path, timeout: float = 3600.0) -> dict[str, Any]:
    rc, out, err, ms = sh_run([binary.path, "prepare", str(root), "--json", "--no-agent-rules"], sandbox.env(), timeout)
    if rc != 0 and "--no-agent-rules" in err:
        rc, out, err, ms = sh_run([binary.path, "prepare", str(root), "--json"], sandbox.env(), timeout)
    return {"rc": rc, "ms": round(ms, 1), "stderr": err[-300:]}


def find(
    binary: Binary,
    sandbox: Sandbox,
    root: str | Path,
    query: str,
    *,
    top_k: int = 20,
    extra_args: list[str] | None = None,
    extra_env: dict[str, str] | None = None,
    timeout: float = 300.0,
) -> dict[str, Any]:
    """Run `jikji find ROOT QUERY --json` exactly as an agent would; return a normalised view."""
    argv = [binary.path, "find", str(root), query, "--json", "--no-background-refresh", "--top-k", str(top_k)]
    argv += extra_args or []
    rc, out, err, ms = sh_run(argv, sandbox.env(extra_env), timeout)
    return view(argv, rc, out, err, ms)


def run_command_string(binary: Binary, sandbox: Sandbox, command: str, timeout: float = 300.0) -> dict[str, Any]:
    """Run a `next_commands[]` string (it starts with `jikji`) with this binary."""
    parts = shlex.split(command)
    if not parts or Path(parts[0]).name != "jikji":
        return {"ok": False, "error": f"not a jikji command: {command[:80]}"}
    argv = [binary.path, *parts[1:]]
    if "--no-background-refresh" not in argv:
        argv.append("--no-background-refresh")
    rc, out, err, ms = sh_run(argv, sandbox.env(), timeout)
    return view(argv, rc, out, err, ms)


def view(argv: list[str], rc: int, out: str, err: str, ms: float) -> dict[str, Any]:
    """Normalised, comparison-friendly view of one `find` response."""
    try:
        data = json.loads(out)
    except (json.JSONDecodeError, ValueError):
        return {"ok": False, "rc": rc, "ms": round(ms, 1), "argv": argv, "error": (err or out)[:200], "paths": [], "note_codes": []}
    paths = data.get("paths")
    if paths is None:
        paths = data.get("answer_paths") or []
    notes = data.get("notes") or []
    return {
        "ok": True,
        "rc": rc,
        "ms": round(ms, 1),
        "argv": argv,
        "paths": [str(p) for p in paths],
        "index_status": data.get("index_status"),
        "handoff_action": data.get("handoff_action"),
        "answerability": data.get("answerability"),
        "empty_reason": data.get("empty_reason"),
        "reason": data.get("reason"),
        "note_codes": [n.get("code") for n in notes if isinstance(n, dict)],
        "has_next_steps": bool(data.get("next_steps")),
        "has_message": bool(data.get("message")),
        "next_commands": data.get("next_commands") or [],
        "retry_proof": data.get("retry_proof") or "",
        "scope": data.get("scope"),
        "intent": (data.get("request_plan") or {}).get("intent"),
        "use_instead_tool": ((data.get("request_plan") or {}).get("use_instead") or {}).get("tool"),
        "results": data.get("results") or [],
        "indexed_files_searched": data.get("indexed_files_searched"),
        "indexed_roots_inside": data.get("indexed_roots_inside") or [],
        "version": data.get("jikji_version"),
        "empty_result_reindexed": data.get("empty_result_reindexed"),
        "foreground_prepared": data.get("foreground_prepared"),
    }


def fingerprint(v: dict[str, Any]) -> tuple:
    """What must be identical between two runs of the same query on an unchanged tree."""
    # index_status is left out: the documented one-time re-index after an empty result of a changed
    # folder legitimately moves it from `changed_using_previous_index` to `ready` between runs.
    return (tuple(v.get("paths", [])), v.get("empty_reason"), tuple(v.get("note_codes", [])))


def path_matches(returned: str, target_rel: str) -> bool:
    """`returned` may be absolute or relative to the index root; `target_rel` is relative to the asked folder."""
    r = returned.replace("\\", "/")
    t = target_rel.replace("\\", "/")
    return r == t or r.endswith("/" + t)


def rank_of(paths: list[str], targets: list[str]) -> int | None:
    for i, p in enumerate(paths, 1):
        if any(path_matches(p, t) for t in targets):
            return i
    return None


def hash_label(text: str) -> str:
    """Stable short label for sensitive names in artifacts (never store the name itself)."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:10]


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    k = max(0, min(len(ordered) - 1, round(pct / 100 * (len(ordered) - 1))))
    return ordered[k]


def query_words_hint(query: str) -> list[str]:
    """Whitespace words of a request: the harness's own count, independent of Jikji's tokenizer."""
    return [w for w in query.split() if len(w.strip("()[]{}.,:;!?'\"")) >= 2]
