#!/usr/bin/env python3
"""Evaluate Jikji binaries against a privacy-filtered sample of local documents."""
from __future__ import annotations

import argparse
import hashlib
import html
import json
import os
import re
import shutil
import statistics
import tempfile
import zipfile
from collections import Counter, defaultdict
from pathlib import Path

from common import (
    Binary,
    Sandbox,
    find,
    fingerprint,
    guard_production_index,
    hash_label,
    percentile,
    path_matches,
    rank_of,
)

EXTS = {".txt", ".md", ".csv", ".html", ".htm", ".docx", ".pptx", ".hwpx", ".rtf", ".json"}
PII_RE = re.compile(r"급여|명세서|주민|생년월일|비밀번호|암호|연봉|통장|세금계산|개인정보|여권|주민등록|증명서|기본증명|등본|동의서|이력서|사업자등록번호|전화번호|password|passwd|secret|credential|token|\.env|id_rsa|private", re.I)
TOK_RE = re.compile(r"[A-Za-z][A-Za-z0-9_-]{2,}|[가-힣]{2,}|[0-9]{4,}")
COMMON = {"the", "and", "for", "with", "from", "this", "that", "file", "data", "document", "test", "report", "의", "및", "파일"}
XML_TEXT_RE = re.compile(r"<[^>]+>")
CTRL_RE = re.compile(r"\\[a-zA-Z]+-?\d* ?|[{}]")


def safe_rel(p: Path, roots: list[Path]) -> str:
    for root in roots:
        try:
            return p.relative_to(root).as_posix()
        except ValueError:
            pass
    return p.name


def candidates(sources: list[Path]) -> tuple[list[Path], int]:
    out: list[Path] = []
    skipped = 0
    for source in sources:
        if not source.is_dir():
            continue
        for root, dirs, files in os.walk(source, followlinks=False):
            dirs[:] = [d for d in dirs if not d.startswith(".") and d not in {"node_modules", ".git", ".jikji"} and not (Path(root) / d).is_symlink()]
            for name in files:
                p = Path(root) / name
                if p.is_symlink() or p.suffix.lower() not in EXTS:
                    continue
                try:
                    size = p.stat().st_size
                except OSError:
                    continue
                rel = safe_rel(p, sources)
                if not 200 <= size <= 2 * 1024 * 1024 or PII_RE.search(name) or PII_RE.search(rel):
                    skipped += 1
                    continue
                out.append(p)
    return out, skipped


def extract(path: Path) -> str:
    ext = path.suffix.lower()
    try:
        if ext in {".txt", ".md", ".csv", ".json"}:
            return path.read_text(encoding="utf-8", errors="replace")
        if ext in {".html", ".htm"}:
            return html.unescape(XML_TEXT_RE.sub(" ", path.read_text(encoding="utf-8", errors="replace")))
        if ext == ".rtf":
            return CTRL_RE.sub(" ", path.read_text(encoding="utf-8", errors="replace"))
        with zipfile.ZipFile(path) as z:
            chunks = []
            for n in z.namelist():
                if n.lower().endswith((".xml", ".rels")):
                    chunks.append(XML_TEXT_RE.sub(" ", z.read(n).decode("utf-8", "replace")))
            return html.unescape(" ".join(chunks))
    except (OSError, zipfile.BadZipFile, KeyError):
        return ""


def tokens(text: str) -> list[str]:
    result = []
    for tok in TOK_RE.findall(text):
        t = tok.casefold()
        if t in COMMON or t.isdigit() or (len(t) >= 8 and all(c in "0123456789abcdef" for c in t)):
            continue
        result.append(t)
    return result


def choose_cases(items: list[dict], seed: int) -> list[dict]:
    stem_sets = [set(tokens(i["path"].stem.replace("_", " "))) for i in items]
    df = Counter(t for s in stem_sets for t in s)
    text_sets = [set(t for t in tokens(i["text"]) if len(t) >= 4 and not t.isdigit()) for i in items]
    tdf = Counter(t for s in text_sets for t in s)
    by_content: dict[str, list[str]] = defaultdict(list)
    for i in items:
        by_content[hashlib.sha256(i["text"].encode("utf-8", "replace")).hexdigest()].append(i["rel"])
    cases = []
    for idx, item in enumerate(items):
        stem = stem_sets[idx]
        fts = sorted((t for t in stem if t not in COMMON), key=lambda t: (df[t], hashlib.sha256(f"{seed}:{t}".encode()).hexdigest()))[:2]
        if len(fts) >= 2:
            cases.append({"query": " ".join(fts), "kind": "filename", "item": item, "targets": [item["rel"]]})
        if len(item["text"]) >= 200:
            cts = sorted((t for t in text_sets[idx] if t not in stem), key=lambda t: (tdf[t], hashlib.sha256(f"{seed}:{t}".encode()).hexdigest()))
            cts = [t for t in cts if tdf[t] == 1][:2]
            if len(cts) >= 2:
                cases.append({"query": " ".join(cts), "kind": "content", "item": item, "targets": by_content[item["content_hash"]]})
    return cases


def indexed_count(value):
    if isinstance(value, dict):
        for k, v in value.items():
            if k in {"indexed_files", "indexed_files_count", "documents", "doc_count"} and isinstance(v, int):
                return v
            got = indexed_count(v)
            if got is not None:
                return got
    elif isinstance(value, list):
        for v in value:
            got = indexed_count(v)
            if got is not None:
                return got
    return None


def prepare(binary: Binary, sandbox: Sandbox, root: Path) -> dict:
    from common import sh_run
    argv = [binary.path, "prepare", str(root), "--json", "--no-agent-rules"]
    rc, out, err, ms = sh_run(argv, sandbox.env(), 3600)
    if rc and "--no-agent-rules" in err:
        rc, out, err, ms = sh_run(argv[:-1], sandbox.env(), 3600)
    try:
        data = json.loads(out)
    except (ValueError, TypeError):
        data = {}
    return {"rc": rc, "ms": round(ms, 1), "indexed_docs": indexed_count(data)}


def aggregate(rows: list[dict]) -> dict:
    n = len(rows); ranks = [r["rank"] for r in rows if r["rank"] is not None]; lat = [r["ms"] for r in rows]
    return {"n": n, "hit@1": sum(r["rank"] is not None and r["rank"] <= 1 for r in rows), "hit@3": sum(r["rank"] is not None and r["rank"] <= 3 for r in rows), "hit@10": sum(r["rank"] is not None and r["rank"] <= 10 for r in rows), "mrr": round(sum(1 / x for x in ranks) / n, 4) if n else 0, "miss": sum(r["rank"] is None for r in rows), "snippet_with_match": sum(r.get("snippet_ok", False) for r in rows), "no_result": sum(r["no_result"] for r in rows), "median_ms": round(statistics.median(lat), 1) if lat else 0, "p95_ms": round(percentile(lat, 95), 1), "rerun_identical_rate": round(sum(r["same"] for r in rows) / n, 4) if n else 0}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", action="append", required=True); ap.add_argument("--source", action="append")
    ap.add_argument("--sample", type=int, default=80); ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--out", default="/tmp/search-eval/real_docs_report.json"); ap.add_argument("--keep", action="store_true")
    args = ap.parse_args(); before = guard_production_index()
    sources = [Path(s).expanduser() for s in (args.source or ["~/Documents", "~/Downloads"])]
    files, skipped = candidates(sources)
    files.sort(key=lambda p: hashlib.sha256((safe_rel(p, sources) + str(args.seed)).encode()).hexdigest())
    selected = files[:max(0, args.sample)]
    base = Path(tempfile.mkdtemp(prefix="search-eval-")); corpus = base / "real-corpus"; corpus.mkdir(mode=0o700)
    items = []; parser_gaps = Counter()
    try:
        for src in selected:
            rel = safe_rel(src, sources); h8 = hashlib.sha256(rel.encode()).hexdigest()[:8]; dest = corpus / h8 / src.name; dest.parent.mkdir(mode=0o700, parents=True); shutil.copy2(src, dest)
            text = extract(src)
            if len(text) < 200: parser_gaps[src.suffix.lower()] += 1
            item = {"path": src, "rel": f"{h8}/{src.name}", "text": text, "content_hash": hashlib.sha256(text.encode("utf-8", "replace")).hexdigest(), "ext": src.suffix.lower(), "size": src.stat().st_size}
            items.append(item)
        cases = choose_cases(items, args.seed)
        for c in cases: c["split"] = "holdout" if int(hashlib.sha256(c["item"]["rel"].encode()).hexdigest(), 16) % 2 else "dev"
        report = {"seed": args.seed, "sample_requested": args.sample, "sample_selected": len(items), "sample_size_ext": dict(Counter(i["ext"] for i in items)), "parser_gaps": dict(parser_gaps), "skipped_candidates": skipped, "binaries": {}, "cases": [], "production_index_before": before}
        for spec in args.bin:
            binary = Binary.parse(spec); sb = Sandbox(base / ("sandbox-" + hash_label(binary.name))); prep = prepare(binary, sb, corpus); first = {}
            for c in cases:
                v = find(binary, sb, corpus, c["query"], top_k=20); first[id(c)] = v
            binary_rows = []
            for c in reversed(cases):
                v2 = find(binary, sb, corpus, c["query"], top_k=20); v1 = first[id(c)]; rank = rank_of(v1.get("paths", []), c["targets"]); snip = next((x.get("snippet") or "" for x in v1.get("results", []) if any(path_matches(x.get("path", ""), t) for t in c["targets"])), ""); row = {"snippet_ok": bool(c["kind"] == "content" and rank is not None and any(t in snip.lower() for t in c["query"].lower().split())), "label": hash_label(c["item"]["rel"]), "ext": c["item"]["ext"], "size_bucket": "small" if c["item"]["size"] < 4096 else "medium" if c["item"]["size"] < 65536 else "large", "kind": c["kind"], "split": c["split"], "rank": rank, "ms": v1.get("ms", 0), "same": fingerprint(v1) == fingerprint(v2), "no_result": not bool(v1.get("paths"))}
                binary_rows.append(row); report["cases"].append(row)
            groups = {}
            for split in ("dev", "holdout"):
                for kind in ("filename", "content"):
                    rr = [r for r in binary_rows if r["split"] == split and r["kind"] == kind]; groups[f"{split}/{kind}"] = aggregate(rr)
                    groups[f"{split}/{kind}"]["by_extension_hit@3"] = {e: aggregate([x for x in rr if x["ext"] == e])["hit@3"] for e in sorted({x["ext"] for x in rr})}
            report["binaries"][binary.name] = {"prepare": prep, "metrics": groups}
        report["cases"] = [{k: r[k] for k in ("label", "ext", "size_bucket", "kind", "split", "rank", "ms", "snippet_ok")} for r in report["cases"]]
        report["production_index_after"] = guard_production_index(); report["temp_dir_removed"] = False
        Path(args.out).parent.mkdir(parents=True, exist_ok=True); Path(args.out).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print("binary\tsplit/kind\tn\thit@3\tmrr\tmedian_ms\tp95_ms\tsnippet_with_match")
        for name, info in report["binaries"].items():
            for key, m in info["metrics"].items(): print(f"{name}\t{key}\t{m['n']}\t{m['hit@3']}\t{m['mrr']}\t{m['median_ms']}\t{m['p95_ms']}\t{m['snippet_with_match']}")
    finally:
        if not args.keep:
            shutil.rmtree(base, ignore_errors=True)
            if 'report' in locals():
                report["temp_dir_removed"] = not base.exists(); Path(args.out).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
