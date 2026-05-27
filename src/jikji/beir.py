"""BEIR public benchmark adapter for local-file discovery.

BEIR is an information-retrieval benchmark.  Jikji's task is local file
discovery, so this adapter materializes each BEIR corpus document as a local
Markdown file and converts qrels into Jikji eval cases whose expected answers
are file paths.
"""
from __future__ import annotations

import csv
import json
import re
import shutil
import time
import urllib.request
import zipfile
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .agent_index import build_agent_index
from .config import Config
from .hippocamp import BenchResult, run_benchmark

BEIR_BASE_URL = "https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets"
DEFAULT_BEIR_DATASETS = ("scifact", "nfcorpus", "arguana")


@dataclass
class BeirMaterializeResult:
    dataset: str
    source_dir: Path
    corpus_root: Path
    eval_set_path: Path
    documents: int
    cases: int
    qrels: int


@dataclass
class BeirSuiteResult:
    report_path: Path
    datasets: dict[str, Any]
    aggregate: dict[str, Any]


def _jsonl(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open("r", encoding="utf-8") as fh:
        for line in fh:
            if line.strip():
                row = json.loads(line)
                if isinstance(row, dict):
                    rows.append(row)
    return rows


def _safe_doc_name(doc_id: str) -> str:
    safe = re.sub(r"[^0-9A-Za-z_.-]+", "_", doc_id).strip("._")
    return (safe or "doc")[:180] + ".md"


def _write_json(path: Path, obj: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(obj, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _write_jsonl(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "".join(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n" for row in rows),
        encoding="utf-8",
    )


def fetch_beir_dataset(dataset: str, dest: Path) -> Path:
    """Download and extract one public BEIR dataset zip."""
    dataset = dataset.strip().lower()
    dest = Path(dest).expanduser().resolve()
    source_parent = dest / "source"
    source_parent.mkdir(parents=True, exist_ok=True)
    source_dir = source_parent / dataset
    if (source_dir / "corpus.jsonl").exists():
        return source_dir
    zip_path = source_parent / f"{dataset}.zip"
    if not zip_path.exists():
        url = f"{BEIR_BASE_URL}/{dataset}.zip"
        urllib.request.urlretrieve(url, zip_path)  # noqa: S310 - public benchmark URL.
    tmp = source_parent / f".{dataset}.extracting"
    if tmp.exists():
        shutil.rmtree(tmp)
    tmp.mkdir(parents=True)
    with zipfile.ZipFile(zip_path) as zf:
        zf.extractall(tmp)
    extracted = tmp / dataset
    if not extracted.exists():
        matches = [p for p in tmp.iterdir() if p.is_dir()]
        if len(matches) != 1:
            raise FileNotFoundError(f"cannot locate BEIR dataset root in {zip_path}")
        extracted = matches[0]
    if source_dir.exists():
        shutil.rmtree(source_dir)
    extracted.replace(source_dir)
    shutil.rmtree(tmp, ignore_errors=True)
    return source_dir


def materialize_beir_dataset(
    dataset: str,
    dest: Path,
    *,
    split: str = "test",
    max_cases: int = 200,
) -> BeirMaterializeResult:
    """Create local files and an external Jikji eval set from a BEIR dataset."""
    dataset = dataset.strip().lower()
    source_dir = fetch_beir_dataset(dataset, dest)
    corpus_root = Path(dest).expanduser().resolve() / "corpora" / dataset
    eval_set_path = Path(dest).expanduser().resolve() / "eval" / f"{dataset}_{split}.jsonl"
    if corpus_root.exists():
        shutil.rmtree(corpus_root)
    docs_dir = corpus_root / "docs"
    docs_dir.mkdir(parents=True, exist_ok=True)

    doc_paths: dict[str, str] = {}
    for row in _jsonl(source_dir / "corpus.jsonl"):
        doc_id = str(row.get("_id") or "")
        if not doc_id:
            continue
        rel = f"docs/{_safe_doc_name(doc_id)}"
        title = str(row.get("title") or "").strip()
        text = str(row.get("text") or "").strip()
        body = f"# {title or doc_id}\n\nBEIR dataset: {dataset}\nDocument ID: {doc_id}\n\n{text}\n"
        (corpus_root / rel).write_text(body, encoding="utf-8")
        doc_paths[doc_id] = rel

    queries = {str(r.get("_id") or ""): str(r.get("text") or "") for r in _jsonl(source_dir / "queries.jsonl")}
    qrels_path = source_dir / "qrels" / f"{split}.tsv"
    if not qrels_path.exists():
        candidates = sorted((source_dir / "qrels").glob("*.tsv"))
        if not candidates:
            raise FileNotFoundError(f"no BEIR qrels found for {dataset}")
        qrels_path = candidates[-1]
    by_query: dict[str, list[str]] = {}
    qrels_count = 0
    with qrels_path.open("r", encoding="utf-8") as fh:
        reader = csv.DictReader(fh, delimiter="\t")
        for row in reader:
            try:
                score = float(row.get("score") or 0)
            except ValueError:
                score = 0
            if score <= 0:
                continue
            qid = str(row.get("query-id") or row.get("query_id") or "")
            cid = str(row.get("corpus-id") or row.get("corpus_id") or "")
            if qid in queries and cid in doc_paths:
                by_query.setdefault(qid, []).append(doc_paths[cid])
                qrels_count += 1

    cases: list[dict[str, Any]] = []
    for idx, qid in enumerate(sorted(by_query, key=lambda x: int(x) if x.isdigit() else x), 1):
        if len(cases) >= max_cases:
            break
        expected = sorted(set(by_query[qid]))
        cases.append({
            "id": f"beir-{dataset}-{split}-{idx:04d}",
            "scenario": f"beir_{dataset}",
            "query": queries[qid],
            "expected_paths": expected,
            "expected_count": len(expected),
            "source": "BEIR",
            "dataset": dataset,
            "split": split,
            "query_id": qid,
            "public_benchmark": True,
        })
    _write_jsonl(eval_set_path, cases)
    return BeirMaterializeResult(
        dataset=dataset,
        source_dir=source_dir,
        corpus_root=corpus_root,
        eval_set_path=eval_set_path,
        documents=len(doc_paths),
        cases=len(cases),
        qrels=qrels_count,
    )


def run_beir_suite(
    dest: Path,
    *,
    datasets: tuple[str, ...] = DEFAULT_BEIR_DATASETS,
    split: str = "test",
    max_cases: int = 200,
    top_k: int = 10,
    prepare: bool = True,
) -> BeirSuiteResult:
    dest = Path(dest).expanduser().resolve()
    reports_dir = dest / "reports"
    reports_dir.mkdir(parents=True, exist_ok=True)
    started = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    suite: dict[str, Any] = {}
    for dataset in datasets:
        mat = materialize_beir_dataset(dataset, dest, split=split, max_cases=max_cases)
        prepare_seconds = 0.0
        if prepare:
            t0 = time.perf_counter()
            build_agent_index(mat.corpus_root, Config(max_files=1_000_000))
            prepare_seconds = time.perf_counter() - t0
        bench: BenchResult = run_benchmark(
            mat.corpus_root,
            eval_set=mat.eval_set_path,
            modes=("raw", "jikji"),
            top_k=top_k,
            prepare=False,
        )
        suite[dataset] = {
            "dataset": dataset,
            "source_dir": str(mat.source_dir),
            "corpus_root": str(mat.corpus_root),
            "eval_set": str(mat.eval_set_path),
            "documents": mat.documents,
            "cases": mat.cases,
            "qrels": mat.qrels,
            "prepare_seconds": round(prepare_seconds, 3),
            "benchmark_report": str(bench.report_path),
            "metrics": bench.metrics,
        }

    aggregate: dict[str, Any] = {}
    for mode in ("raw", "jikji"):
        total = sum(int(item["metrics"][mode].get("cases") or 0) for item in suite.values())
        row: dict[str, Any] = {"cases": total}
        for key in ("hit_at_1", "hit_at_3", "hit_at_5", "hit_at_10", "mrr"):
            row[key] = round(
                sum(float(item["metrics"][mode].get(key) or 0) * int(item["metrics"][mode].get("cases") or 0) for item in suite.values())
                / max(1, total),
                4,
            )
        row["seconds"] = round(sum(float(item["metrics"][mode].get("seconds") or 0) for item in suite.values()), 3)
        aggregate[mode] = row
    aggregate["datasets"] = dict(Counter(datasets))
    report_path = reports_dir / f"beir_suite_{started}.json"
    _write_json(report_path, {
        "public_benchmark": True,
        "source": "BEIR",
        "datasets": suite,
        "aggregate": aggregate,
    })
    return BeirSuiteResult(report_path=report_path, datasets=suite, aggregate=aggregate)
