#!/usr/bin/env python3
"""Run the Jikji search evaluation: same fixtures, several binaries, real CLI.

Example (baseline vs improved on the general and the held-out dataset)::

    python3 tools/search_eval/run_eval.py \\
        --bin v0.2.1=/tmp/jikji-baseline-target/release/jikji \\
        --bin v0.2.2=target/release/jikji \\
        --dataset general --dataset holdout --out /tmp/search-eval/run1

Each binary gets its own sandbox (isolated JIKJI_DATA_DIR/HOME) and its own copy of the fixture, so
no binary sees another's index and the user's real index is never touched.
"""

from __future__ import annotations

import argparse
import json
import shutil
import statistics
import sys
import tempfile
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402
import datasets  # noqa: E402

SIGNAL = "pattern_syntax_not_supported"
STALE_HINTS = ("freshness_not_checked",)


def explained(v: dict[str, Any], reasons: list[str]) -> bool:
    got = {v.get("empty_reason"), v.get("reason"), *v.get("note_codes", [])}
    return bool(got & set(reasons))


def judge(case: dict[str, Any], v: dict[str, Any]) -> dict[str, Any]:
    """Score one response against the case's ground truth. All booleans, no thresholds on latency."""
    exp = case["expect"]
    kind = exp["kind"]
    paths = v.get("paths", [])
    out: dict[str, Any] = {"kind": kind, "ok": v.get("ok", False), "n_paths": len(paths)}
    forbidden = exp.get("forbidden", [])
    out["fp"] = any(common.path_matches(p, f) for p in paths for f in forbidden)
    if not v.get("ok"):
        out.update(passed=False, explained=False, error=v.get("error", "")[:120])
        return out
    if kind == "find":
        rank = common.rank_of(paths, exp["targets"])
        hit = rank is not None and rank <= exp["rank_max"]
        out.update(rank=rank, hit=hit)
        out["false_signal"] = bool(exp.get("no_signal") and SIGNAL in v["note_codes"])
        if exp.get("media"):
            # a name match on a non-text file must be labelled as one, from what the index stored
            top = next((r for r in v.get("results", []) if common.path_matches(r.get("path", ""), exp["targets"][0])), {})
            out["media_labelled"] = hit and "non_text_name_match_only" in v["note_codes"] and top.get("file_kind") in ("image", "audio", "video") and top.get("matched_in") == ["name_path"]
        if exp.get("identifier"):
            out["identifier_labelled"] = hit and v.get("intent") == "code_identifier" and "identifier_files_only" in v["note_codes"]
        if exp.get("snippet"):
            top = next((r for r in v.get("results", []) if common.path_matches(r.get("path", ""), exp["targets"][0])), {})
            out["snippet_ok"] = hit and exp["snippet"].lower() in (top.get("snippet") or "").lower()
        if exp.get("partial") or case["category"] == "content_multi":
            top = next((r for r in v.get("results", []) if common.path_matches(r.get("path", ""), exp["targets"][0])), {})
            got = {w.lower() for w in top.get("matched_words", [])}
            want = {exp["partial"]} if exp.get("partial") else {w.lower() for w in case["query"].split()}
            # the words the file really contains are listed, and a word it lacks never is
            out["words_ok"] = hit and got == want
        out["wrong_non_text_intent"] = bool(not exp.get("media") and v.get("intent") == "non_text_files")
        out["explained"] = hit or bool(v.get("empty_reason") or v.get("note_codes"))
        out["explained_miss"] = (not hit) and exp.get("explain_ok") and (
            any(c in v["note_codes"] for c in STALE_HINTS) or str(v.get("index_status", "")).startswith(("changed", "stale")) or v.get("empty_result_reindexed")
        )
        out["silent_miss"] = (not hit) and not out["explained_miss"]
        out["passed"] = hit and not out["false_signal"] and not out["fp"]
        out["wrong_confident"] = (not hit) and bool(paths) and v.get("handoff_action") == "direct_use"
    elif kind == "empty":
        out["explained"] = explained(v, exp["reasons"])
        out["fp"] = bool(paths)  # nothing exists to find: every returned file is wrong
        out["passed"] = not paths and out["explained"]
        if exp.get("needs_code"):
            out["media_empty_explained"] = exp["needs_code"] in v["note_codes"]
    elif kind == "missing":
        ok_reason = explained(v, exp["reasons"])
        roots_ok = (not exp.get("lists_indexed_roots")) or bool(v.get("indexed_roots_inside"))
        out["explained"] = ok_reason
        out["roots_listed"] = roots_ok
        out["fp"] = bool(paths)
        out["passed"] = (not paths) and ok_reason and roots_ok
    elif kind == "count":
        out["passed"] = len(paths) >= exp["min_results"]
        out["explained"] = True
    elif kind == "signal":
        sig = SIGNAL in v["note_codes"]
        out["signaled"] = sig
        out["guided"] = sig and v.get("use_instead_tool") in ("grep", "read")
        out["confident_wrong"] = (not sig) and v.get("handoff_action") == "direct_use" and bool(paths)
        out["explained"] = sig
        out["passed"] = sig
    return out


def apply_mutations(fx: Path, mutations: list[dict[str, Any]]) -> None:
    for m in mutations:
        target = fx / m["path"]
        if m["op"] == "add":
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(m["text"], encoding="utf-8")
        elif m["op"] == "append":
            with target.open("a", encoding="utf-8") as fh:
                fh.write(m["text"])
        elif m["op"] == "delete":
            target.unlink()


def run_binary(binary: common.Binary, fixture: Path, doc: dict[str, Any], work: Path, large_budget: float) -> dict[str, Any]:
    sandbox = common.Sandbox(work / binary.name)
    fx = work / binary.name / "fx"
    shutil.copytree(fixture, fx)
    prep: dict[str, Any] = {}
    for key in ("corpus", "large", "korean"):
        prep[key] = common.prepare(binary, sandbox, fx / doc["roots"][key], timeout=large_budget)
    apply_mutations(fx, doc["mutations"])

    cases = doc["cases"]
    first: dict[str, dict[str, Any]] = {}
    verdict: dict[str, dict[str, Any]] = {}
    calls: dict[str, dict[str, Any]] = {}
    for case in cases:
        root = fx / case["root"]
        v = common.find(binary, sandbox, root, case["query"], top_k=case["top_k"])
        first[case["id"]] = v
        verdict[case["id"]] = judge(case, v)
        call = {"calls": 1, "retried": False, "retry_helped": False, "retry_wasted": False}
        if v.get("ok") and v.get("handoff_action") == "jikji_retry" and v.get("next_commands"):
            v2 = common.run_command_string(binary, sandbox, v["next_commands"][0])
            j2 = judge(case, v2) if v2.get("ok") else {"passed": False}
            call.update(calls=2, retried=True, retry_helped=j2["passed"] and not verdict[case["id"]]["passed"],
                        retry_wasted=not j2["passed"] or j2["passed"] == verdict[case["id"]]["passed"] and common.fingerprint(v2) == common.fingerprint(v))
        elif v.get("ok") and v.get("handoff_action") == "jikji_retry":
            call["retry_without_command"] = True
        calls[case["id"]] = call

    # Re-run everything in reverse order: same answers on an unchanged tree?
    same = {}
    for case in reversed(cases):
        v = common.find(binary, sandbox, fx / case["root"], case["query"], top_k=case["top_k"])
        same[case["id"]] = common.fingerprint(v) == common.fingerprint(first[case["id"]])

    # Parallel agents: several finds at once must agree with the serial answers.
    probe = [c for c in cases if c["category"] in ("filename", "content") ][:8]
    with ThreadPoolExecutor(max_workers=4) as pool:
        conc = list(pool.map(lambda c: common.find(binary, sandbox, fx / c["root"], c["query"], top_k=c["top_k"]), probe))
    concurrent_same = [common.fingerprint(v) == common.fingerprint(first[c["id"]]) for c, v in zip(probe, conc, strict=True)]

    return {
        "binary": binary.name,
        "version": next((v.get("version") for v in first.values() if v.get("version")), None),
        "prepare_ms": {k: v["ms"] for k, v in prep.items()},
        "prepare_rc": {k: v["rc"] for k, v in prep.items()},
        "cases": [
            {"id": c["id"], "category": c["category"], "root": c["root"], "query": c["query"], "ms": first[c["id"]].get("ms"),
             "verdict": verdict[c["id"]], "calls": calls[c["id"]], "rerun_same": same[c["id"]],
             "top": first[c["id"]].get("paths", [])[:3], "codes": first[c["id"]].get("note_codes"), "status": first[c["id"]].get("index_status"),
             "handoff": first[c["id"]].get("handoff_action"), "empty_reason": first[c["id"]].get("empty_reason")}
            for c in cases
        ],
        "concurrent_same": concurrent_same,
    }


def summarize(res: dict[str, Any]) -> dict[str, Any]:
    cases = res["cases"]
    n = len(cases)
    v = [c["verdict"] for c in cases]
    lat = [c["ms"] for c in cases if c["ms"] is not None]
    large_lat = [c["ms"] for c in cases if c["category"] in ("large",) and c["ms"] is not None]
    find_cases = [x for x in v if x["kind"] == "find"]
    retries = [c["calls"] for c in cases if c["calls"]["retried"]]
    by_cat: dict[str, list[bool]] = defaultdict(list)
    for c in cases:
        by_cat[c["category"]].append(bool(c["verdict"]["passed"]))
    sig = [x for x in v if x["kind"] == "signal"]
    return {
        "binary": res["binary"], "version": res["version"], "cases": n,
        "success": sum(1 for x in v if x["passed"]), "success_rate": round(sum(1 for x in v if x["passed"]) / n, 3),
        "find_hit_rate": round(sum(1 for x in find_cases if x.get("hit")) / max(1, len(find_cases)), 3),
        "top1_rate": round(sum(1 for x in find_cases if x.get("rank") == 1) / max(1, len(find_cases)), 3),
        "false_positive_cases": sum(1 for x in v if x["fp"]),
        "false_signal_cases": sum(1 for x in v if x.get("false_signal")),
        "misses": sum(1 for x in find_cases if not x.get("hit")),
        "wrong_confident": sum(1 for x in find_cases if x.get("wrong_confident")),
        "silent_misses": sum(1 for x in find_cases if x.get("silent_miss")),
        "explained_empty_or_missing": f"{sum(1 for x in v if x['kind'] in ('empty', 'missing') and x['explained'])}/{sum(1 for x in v if x['kind'] in ('empty', 'missing'))}",
        "signal_detected": f"{sum(1 for x in sig if x['signaled'])}/{len(sig)}",
        "confident_wrong_on_signal": sum(1 for x in sig if x["confident_wrong"]),
        "media_found_labelled": f"{sum(1 for x in v if x.get('hit') and 'media_labelled' in x)}/{sum(1 for x in v if 'media_labelled' in x)} labelled {sum(1 for x in v if x.get('media_labelled'))}",
        "identifier_found_labelled": f"found {sum(1 for x in v if x.get('hit') and 'identifier_labelled' in x)}/{sum(1 for x in v if 'identifier_labelled' in x)} labelled {sum(1 for x in v if x.get('identifier_labelled'))}",
        "media_empty_explained": f"{sum(1 for x in v if x.get('media_empty_explained'))}/{sum(1 for x in v if 'media_empty_explained' in x)}",
        "snippet_with_match": f"{sum(1 for x in v if x.get('snippet_ok'))}/{sum(1 for x in v if 'snippet_ok' in x)}",
        "matched_words_exact": f"{sum(1 for x in v if x.get('words_ok'))}/{sum(1 for x in v if 'words_ok' in x)}",
        "signal_guided": f"{sum(1 for x in sig if x.get('guided'))}/{len(sig)}",
        "wrong_non_text_intent": sum(1 for x in v if x.get("wrong_non_text_intent")),
        "errors": sum(1 for x in v if not x["ok"]),
        "latency_p50_ms": round(statistics.median(lat), 1) if lat else 0,
        "latency_p95_ms": round(common.percentile(lat, 95), 1),
        "latency_large_p50_ms": round(statistics.median(large_lat), 1) if large_lat else 0,
        "jikji_calls": sum(c["calls"]["calls"] for c in cases),
        "retries": len(retries), "retries_helped": sum(1 for r in retries if r["retry_helped"]), "retries_wasted": sum(1 for r in retries if r["retry_wasted"]),
        "retry_without_command": sum(1 for c in cases if c["calls"].get("retry_without_command")),
        "rerun_identical": f"{sum(1 for c in cases if c['rerun_same'])}/{n}",
        "concurrent_identical": f"{sum(res['concurrent_same'])}/{len(res['concurrent_same'])}",
        "by_category": {k: f"{sum(x)}/{len(x)}" for k, x in sorted(by_cat.items())},
        "prepare_ms": res["prepare_ms"],
    }


def markdown(report: dict[str, Any]) -> str:
    lines = [f"# Jikji search evaluation ({report['label']})", ""]
    for ds, block in report["datasets"].items():
        sums = block["summary"]
        names = [s["binary"] for s in sums]
        lines += [f"## Dataset `{ds}` ({sums[0]['cases']} cases, large root {block['large_files']} files)", "", "| metric | " + " | ".join(f"{s['binary']} ({s['version']})" for s in sums) + " |", "|---|" + "---|" * len(sums)]
        keys = ["success", "success_rate", "find_hit_rate", "top1_rate", "misses", "silent_misses", "wrong_confident", "false_positive_cases", "false_signal_cases", "signal_detected", "signal_guided", "matched_words_exact", "snippet_with_match", "media_found_labelled", "identifier_found_labelled", "media_empty_explained", "wrong_non_text_intent", "confident_wrong_on_signal", "explained_empty_or_missing", "errors", "latency_p50_ms", "latency_p95_ms", "latency_large_p50_ms", "jikji_calls", "retries", "retries_helped", "retries_wasted", "retry_without_command", "rerun_identical", "concurrent_identical"]
        for k in keys:
            lines.append(f"| {k} | " + " | ".join(str(s[k]) for s in sums) + " |")
        lines += ["", "| category | " + " | ".join(names) + " |", "|---|" + "---|" * len(names)]
        for cat in sorted({c for s in sums for c in s["by_category"]}):
            lines.append(f"| {cat} | " + " | ".join(s["by_category"].get(cat, "-") for s in sums) + " |")
        lines.append("")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", action="append", required=True, help="NAME=PATH (repeatable); first one is the baseline")
    ap.add_argument("--dataset", action="append", choices=sorted(datasets.PROFILES), help="default: general and holdout")
    ap.add_argument("--seed", type=int, help="override the profile seed (use a fresh seed for an unseen sample)")
    ap.add_argument("--large-files", type=int, default=5000)
    ap.add_argument("--out", type=Path, default=Path("/tmp/search-eval/run"))
    ap.add_argument("--label", default="run")
    ap.add_argument("--keep", action="store_true", help="keep the sandboxes")
    a = ap.parse_args()
    binaries = [common.Binary.parse(s) for s in a.bin]
    for b in binaries:
        if not Path(b.path).exists():
            raise SystemExit(f"binary not found: {b.path}")
    before = common.guard_production_index()
    work = Path(tempfile.mkdtemp(prefix="jikji-eval-"))
    a.out.mkdir(parents=True, exist_ok=True)
    report: dict[str, Any] = {"label": a.label, "binaries": {b.name: b.path for b in binaries}, "datasets": {}}
    try:
        for ds in a.dataset or ["general", "holdout"]:
            fixture = work / f"fx-{ds}"
            doc = datasets.build_fixture(ds, fixture, a.seed, a.large_files)
            runs = []
            for b in binaries:
                print(f"[{ds}] {b.name} ...", flush=True)
                runs.append(run_binary(b, fixture, doc, work / ds, 3600.0))
            report["datasets"][ds] = {"seed": doc["seed"], "large_files": doc["large_files"], "summary": [summarize(r) for r in runs], "runs": runs}
    finally:
        after = common.guard_production_index()
        report["production_index_before"], report["production_index_after"] = before, after
        report["production_index_untouched"] = before == after
        if not a.keep:
            shutil.rmtree(work, ignore_errors=True)
    (a.out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=1), encoding="utf-8")
    md = markdown(report)
    (a.out / "report.md").write_text(md, encoding="utf-8")
    print(md)
    print(f"\nproduction index untouched: {report['production_index_untouched']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
