"""Hard mixed-document benchmark for local-agent file discovery.

This builder crawls public KOGL (공공누리) education/resource attachments,
downloads a bounded mix of PDF/HWP/HWPX/PPTX/XLSX files, places them into
deep human-ish folder trees, and generates no-leak train/valid/test file-search
cases from filename, folder, metadata, and parsed document text clues.
"""
from __future__ import annotations

import html
import random
import re
import shutil
import time
import urllib.parse
import urllib.request
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .agent_index import build_agent_index
from .config import Config
from .eval import _write_json, _write_jsonl
from .hippocamp import BenchResult, run_benchmark
from .parsers.registry import extract_excerpt

KOGL_BASE = "https://www.kogl.or.kr"
KOGL_VIEW = f"{KOGL_BASE}/edu/eduDataView.do"
KOGL_FILE = f"{KOGL_BASE}/edu/eduDataFileDown.do"
DEFAULT_HARDBENCH_SEED = 20260603
DEFAULT_ALLOWED_EXTENSIONS = (".pdf", ".hwp", ".hwpx", ".pptx", ".xlsx")
HARDBENCH_LEAK_NAMES = ("eval", "metadata", "manifest.json", "source_downloads")

_KOREAN_STOP = {
    "공공",
    "공공누리",
    "공공저작물",
    "자료",
    "문서",
    "파일",
    "관련",
    "내용",
    "교육",
    "소개",
    "안내",
    "활용",
    "제도",
    "정책",
    "관리",
    "이용",
    "저작권",
    "한국",
    "문화",
    "정보원",
    "담당자",
    "최종",
    "수정",
    "붙임",
    "발표",
    "사업",
    "사례",
}

_DOC_TYPE_PATTERNS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("form", ("서식", "양식", "계약서", "동의서", "신청서")),
    ("manual", ("매뉴얼", "메뉴얼", "지침", "가이드", "가이드라인", "해설서")),
    ("casebook", ("사례집", "상담", "분쟁", "100문100답", "질의응답")),
    ("training", ("교육", "교안", "워크숍", "연수", "설명회", "발표자료")),
    ("policy", ("시책", "정책", "고시", "개정", "추진방향")),
    ("report", ("보고서", "연구", "조사", "성과")),
    ("brochure", ("브로슈어", "리플릿", "리플렛", "방방곡곡")),
)

_COMPLEX_HANGUL_FINALS = {3, 5, 6, 9, 10, 11, 12, 13, 14, 15, 18}


def _hangul_final_index(ch: str) -> int | None:
    code = ord(ch)
    if not 0xAC00 <= code <= 0xD7A3:
        return None
    return (code - 0xAC00) % 28


def _looks_garbled_token(token: str) -> bool:
    if len(token) > 36:
        return True
    if re.fullmatch(r"[a-z]+", token or "") and len(token) > 24:
        return True
    hangul = [ch for ch in token if 0xAC00 <= ord(ch) <= 0xD7A3]
    if len(hangul) >= 3:
        complex_finals = sum(
            1
            for ch in hangul
            if (_hangul_final_index(ch) or 0) in _COMPLEX_HANGUL_FINALS
        )
        if complex_finals / len(hangul) >= 0.4:
            return True
    return False


def _looks_garbled_phrase(text: str) -> bool:
    compact = re.sub(r"\s+", "", (text or "").casefold())
    if re.search(r"[a-z]{28,}", compact):
        return True
    hangul = [ch for ch in compact if 0xAC00 <= ord(ch) <= 0xD7A3]
    if len(hangul) >= 5:
        complex_finals = sum(
            1
            for ch in hangul
            if (_hangul_final_index(ch) or 0) in _COMPLEX_HANGUL_FINALS
        )
        if complex_finals / len(hangul) >= 0.35:
            return True
    return False


@dataclass
class HardBenchBuildResult:
    dest: Path
    train_root: Path
    valid_root: Path
    test_root: Path
    train_eval_set_path: Path
    valid_eval_set_path: Path
    eval_set_path: Path
    manifest_path: Path
    docs_downloaded: int
    train_docs: int
    valid_docs: int
    test_docs: int
    eval_cases: int


@dataclass
class HardBenchSuiteResult:
    build: HardBenchBuildResult
    reports: dict[str, Path]
    metrics: dict[str, dict[str, Any]]
    prepare_seconds: float
    report_path: Path


def _http_get(url: str, *, timeout: int = 45) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 Jikji-hardbench"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 - public benchmark URL.
        return resp.read()


def _slug(value: str, *, max_len: int = 96) -> str:
    value = re.sub(r"[\ufeff]+", "", value or "")
    value = re.sub(r"[\\/:*?\"<>|]+", " ", value)
    value = re.sub(r"\s+", "_", value).strip("._ ")
    return (value or "untitled")[:max_len]


def _clean_text(value: str) -> str:
    value = html.unescape(value or "")
    value = re.sub(r"<[^>]+>", " ", value)
    value = re.sub(r"[\ufeff\u200b]+", "", value)
    return re.sub(r"\s+", " ", value).strip()


def _view_html(data_idx: int, cache_dir: Path) -> str:
    cache_dir.mkdir(parents=True, exist_ok=True)
    path = cache_dir / f"kogl_{data_idx}.html"
    if path.exists() and path.stat().st_size > 1000:
        return path.read_text(encoding="utf-8", errors="ignore")
    url = f"{KOGL_VIEW}?dataIdx={data_idx}"
    text = _http_get(url, timeout=20).decode("utf-8", "ignore")
    path.write_text(text, encoding="utf-8")
    return text


def _title_from_html(page: str, data_idx: int) -> str:
    patterns = (
        r'<h3[^>]*class="view-title"[^>]*>(.*?)</h3>',
        r'<h4[^>]*class="view-title"[^>]*>(.*?)</h4>',
        r'<dd[^>]*class="title"[^>]*>(.*?)</dd>',
    )
    for pattern in patterns:
        match = re.search(pattern, page, flags=re.DOTALL)
        if match:
            title = _clean_text(match.group(1))
            if title and "급상승" not in title:
                return title
    previous = re.search(r'<dd><a href="/edu/eduDataView\.do\?dataIdx=\d+">(.*?)</a></dd>', page, re.DOTALL)
    return _clean_text(previous.group(1)) if previous else f"KOGL resource {data_idx}"


def crawl_kogl_attachments(
    dest: Path,
    *,
    max_data_idx: int = 180,
    allowed_extensions: tuple[str, ...] = DEFAULT_ALLOWED_EXTENSIONS,
) -> list[dict[str, Any]]:
    """Return public KOGL attachment rows without downloading file bodies."""
    cache_dir = Path(dest).expanduser().resolve() / "source_pages"
    rows: list[dict[str, Any]] = []
    seen: set[tuple[int, int, str]] = set()
    allowed = {ext.lower() for ext in allowed_extensions}
    for data_idx in range(1, max_data_idx + 1):
        try:
            page = _view_html(data_idx, cache_dir)
        except Exception:
            continue
        title = _title_from_html(page, data_idx)
        for match in re.finditer(r'href="([^"]*eduDataFileDown\.do[^"]*)"[^>]*>(.*?)</a>', page, re.DOTALL):
            href = html.unescape(match.group(1))
            name = _clean_text(match.group(2))
            if not name or "." not in name:
                continue
            ext = "." + name.rsplit(".", 1)[-1].lower().strip()
            if ext not in allowed:
                continue
            parsed = urllib.parse.urlparse(urllib.parse.urljoin(KOGL_BASE, href))
            query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
            file_idx = int((query.get("dataFileIdx") or ["0"])[0] or 0)
            row_data_idx = int((query.get("dataIdx") or [str(data_idx)])[0] or data_idx)
            fixed_query = urllib.parse.urlencode({"dataIdx": row_data_idx, "dataFileIdx": file_idx})
            url = f"{KOGL_FILE}?{fixed_query}"
            key = (row_data_idx, file_idx, name)
            if file_idx <= 0 or key in seen:
                continue
            seen.add(key)
            rows.append({
                "source": "KOGL public resource attachment",
                "source_url": f"{KOGL_VIEW}?dataIdx={row_data_idx}",
                "download_url": url,
                "data_idx": row_data_idx,
                "data_file_idx": file_idx,
                "page_title": title,
                "filename": name,
                "ext": ext,
                "license_note": "Public KOGL resource page attachment; use only for reproducible benchmark materialization.",
            })
    return rows


def _download_attachment(row: dict[str, Any], source_dir: Path, *, max_file_bytes: int) -> Path | None:
    ext = str(row["ext"])
    name = _slug(str(row["filename"]), max_len=120)
    target = source_dir / f"kogl_{row['data_idx']}_{row['data_file_idx']}_{name}"
    if target.exists() and target.stat().st_size > 1024:
        return target
    try:
        data = _http_get(str(row["download_url"]), timeout=120)
    except Exception:
        return None
    if len(data) < 1024 or len(data) > max_file_bytes:
        return None
    target.write_bytes(data)
    # Some legacy endpoints return HTML error bodies. Keep a cheap signature
    # check for the main binary document types.
    head = data[:16].lower()
    if ext in {".hwp", ".hwpx", ".pptx", ".xlsx"} and not (head.startswith(b"\xd0\xcf") or head.startswith(b"pk")):
        target.unlink(missing_ok=True)
        return None
    if ext == ".pdf" and not data[:8].startswith(b"%PDF"):
        target.unlink(missing_ok=True)
        return None
    return target


def _tokens(text: str, *, min_len: int = 2) -> list[str]:
    out: list[str] = []
    seen: set[str] = set()
    for match in re.finditer(r"[0-9A-Za-z가-힣][0-9A-Za-z가-힣_.+-]*", text or ""):
        token = match.group(0).strip("._+-").casefold()
        if len(token) < min_len or token in _KOREAN_STOP or token in seen:
            continue
        if _looks_garbled_token(token):
            continue
        seen.add(token)
        out.append(token)
    return out


def _doc_type(text: str) -> str:
    for kind, needles in _DOC_TYPE_PATTERNS:
        if any(needle in text for needle in needles):
            return kind
    return "reference"


def _doc_type_label(doc_type: str) -> str:
    return {
        "form": "서식·계약·동의서",
        "manual": "지침·매뉴얼·해설서",
        "casebook": "상담·분쟁 사례집",
        "training": "교육·워크숍 발표자료",
        "policy": "정책·고시·개정안",
        "report": "조사·연구 보고서",
        "brochure": "홍보 브로슈어",
    }.get(doc_type, "참고자료")


def _split_docs(docs: list[dict[str, Any]], *, seed: int) -> dict[str, list[dict[str, Any]]]:
    shuffled = list(docs)
    random.Random(seed).shuffle(shuffled)
    n = len(shuffled)
    train_end = max(1, int(n * 0.6))
    valid_end = max(train_end + 1, int(n * 0.8))
    return {
        "train": shuffled[:train_end],
        "valid": shuffled[train_end:valid_end],
        "test": shuffled[valid_end:],
    }


def _messy_relpath(doc: dict[str, Any], split: str, idx: int, rng: random.Random) -> str:
    type_bucket = {
        "form": "서식_계약_동의서",
        "manual": "지침_매뉴얼_해설",
        "casebook": "상담_분쟁_사례",
        "training": "교육_워크숍_발표",
        "policy": "정책_고시_개정",
        "report": "보고서_조사_연구",
        "brochure": "홍보_브로슈어",
    }.get(str(doc.get("doc_type")), "참고자료")
    top = rng.choice(["공유드라이브", "내문서_백업", "팀자료실", "인수인계", "외부기관_수신"])
    year = rng.choice(["2019", "2020", "2021", "2022", "2023", "2024", "2025", "연도미상"])
    state = rng.choice(["정리전", "검토중", "임시보관", "나중에_정리", "원본_섞임"])
    ext_name = str(doc.get("ext") or ".bin").lstrip(".").upper()
    title = _slug(str(doc.get("filename") or f"doc-{idx}"), max_len=78)
    prefix = rng.choice(["", "복사본_", "최종_", "받은자료_", f"{idx:03d}_"])
    if rng.random() < 0.35:
        title = re.sub(r"공공저작물|공공누리|저작권", "공공자료", title)
    return "/".join([
        top,
        split,
        rng.choice(["업무", "자료", "참고", "회의", "민원_문의"]),
        type_bucket,
        year,
        state,
        ext_name,
        f"{prefix}{title}",
    ])


def _materialize_split(dest: Path, split: str, docs: list[dict[str, Any]], *, seed: int) -> list[dict[str, Any]]:
    rng = random.Random(seed + len(split) * 17)
    root = dest / "corpus" / split
    root.mkdir(parents=True, exist_ok=True)
    materialized: list[dict[str, Any]] = []
    for idx, doc in enumerate(docs, 1):
        rel = _messy_relpath(doc, split, idx, rng)
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(Path(str(doc["source_file"])), target)
        row = dict(doc)
        row["split"] = split
        row["bench_path"] = rel
        row["materialized_path"] = str(target)
        materialized.append(row)
        if idx % 3 == 0:
            note = target.parent / f"{target.stem}_검토메모.txt"
            note.write_text(
                f"임시 메모: {doc.get('page_title')} / {doc.get('filename')} 관련 자료 후보가 같은 폴더에 있음.\n",
                encoding="utf-8",
            )
        if idx % 7 == 0:
            decoy = target.parent / f"{target.stem}_링크만.txt"
            decoy.write_text(
                "원문 파일은 아님. 비슷한 제목 때문에 헷갈릴 수 있는 링크 메모.\n",
                encoding="utf-8",
            )
    return materialized


def _rare_terms(docs: list[dict[str, Any]], *, limit_per_doc: int = 8) -> dict[str, list[str]]:
    freq: Counter[str] = Counter()
    by_path: dict[str, list[str]] = {}
    for doc in docs:
        terms = _tokens(" ".join([str(doc.get("filename") or ""), str(doc.get("page_title") or ""), str(doc.get("text_excerpt") or "")]), min_len=3)
        by_path[str(doc["bench_path"])] = terms
        freq.update(set(terms))
    out: dict[str, list[str]] = {}
    for path, terms in by_path.items():
        ranked = sorted(set(terms), key=lambda term: (freq[term], -len(term), term))
        out[path] = [term for term in ranked if freq[term] <= 3][:limit_per_doc]
    return out


def _content_phrase(text: str) -> str:
    lines = [
        re.sub(r"\s+", " ", line).strip()
        for line in re.split(r"[\n\r.。]", text or "")
        if 12 <= len(re.sub(r"\s+", " ", line).strip()) <= 90
    ]
    for line in lines:
        toks = _tokens(line, min_len=3)
        if _looks_garbled_phrase(line):
            continue
        if len(toks) >= 2 and not line.startswith(("http", "www")):
            return line[:70]
    return ""


def _case_templates(rows: list[dict[str, Any]], *, max_cases: int, seed: int) -> list[dict[str, Any]]:
    rng = random.Random(seed + 104729)
    rare_by_path = _rare_terms(rows)
    cases: list[dict[str, Any]] = []
    for idx, doc in enumerate(rows, 1):
        if len(cases) >= max_cases:
            break
        path = str(doc["bench_path"])
        filename = str(doc.get("filename") or Path(path).name)
        title = str(doc.get("page_title") or filename)
        ext = str(doc.get("ext") or Path(path).suffix).lower()
        ext_label = {"pdf": "PDF", "hwp": "한글 HWP", "hwpx": "한글 HWPX", "pptx": "파워포인트", "xlsx": "엑셀"}.get(ext.lstrip("."), ext)
        doc_type = str(doc.get("doc_type") or "reference")
        doc_type_label = _doc_type_label(doc_type)
        rare = rare_by_path.get(path) or _tokens(filename + " " + title, min_len=3)
        phrase = _content_phrase(str(doc.get("text_excerpt") or ""))
        folder_parts = Path(path).parts[:5]
        scenario_rows = [
            (
                "body_rare_phrase",
                f"본문 어딘가에 '{phrase or (rare[0] if rare else title[:20])}'라는 단서가 나오는 {ext_label} 파일을 찾아줘",
            ),
            (
                "format_doc_type_semantic",
                f"파일명은 정확히 모르는데 {doc_type_label} 성격의 {ext_label} 공공자료를 찾아줘. 단서는 {', '.join(rare[:3]) or title[:30]} 정도야",
            ),
            (
                "messy_folder_context",
                f"{'/'.join(folder_parts)} 아래 정리전 자료 중 {title[:34]} 비슷한 문서를 찾아줘",
            ),
            (
                "multi_clue_hard",
                f"{ext_label} 형식이고 {rare[0] if rare else title[:14]} / {rare[1] if len(rare) > 1 else doc_type} 단서가 같이 보이는 원본 파일",
            ),
        ]
        rng.shuffle(scenario_rows)
        per_doc = 2 if len(cases) + 2 <= max_cases else 1
        for scenario, query in scenario_rows[:per_doc]:
            if len(cases) >= max_cases:
                break
            cases.append({
                "id": f"hardbench-{idx:04d}-{scenario}",
                "scenario": scenario,
                "query": query,
                "expected_paths": [path],
                "dataset": "KOGL mixed hard document benchmark",
                "source_url": doc.get("source_url", ""),
                "source_filename": filename,
                "ext": ext,
                "doc_type": doc_type,
                "public_benchmark": True,
            })
    return cases


def build_hard_benchmark(
    dest: Path,
    *,
    target_docs: int = 180,
    max_data_idx: int = 180,
    max_cases_per_split: int = 240,
    seed: int = DEFAULT_HARDBENCH_SEED,
    max_file_bytes: int = 80 * 1024 * 1024,
) -> HardBenchBuildResult:
    dest = Path(dest).expanduser().resolve()
    dest.mkdir(parents=True, exist_ok=True)
    source_dir = dest / "source_downloads"
    source_dir.mkdir(parents=True, exist_ok=True)
    rows = crawl_kogl_attachments(dest, max_data_idx=max_data_idx)
    docs: list[dict[str, Any]] = []
    failures: list[dict[str, str]] = []
    for row in rows:
        if len(docs) >= target_docs:
            break
        source = _download_attachment(row, source_dir, max_file_bytes=max_file_bytes)
        if source is None:
            failures.append({"filename": str(row.get("filename")), "reason": "download_or_signature_failed"})
            continue
        enriched = dict(row)
        enriched["source_file"] = str(source)
        enriched["bytes"] = source.stat().st_size
        enriched["text_excerpt"] = extract_excerpt(source, max_chars=18_000, timeout=8.0)
        enriched["doc_type"] = _doc_type(
            " ".join([str(enriched.get("filename") or ""), str(enriched.get("page_title") or ""), str(enriched.get("text_excerpt") or "")])
        )
        docs.append(enriched)
    if len(docs) < 40:
        raise RuntimeError(f"Too few hardbench documents downloaded: {len(docs)}")
    splits = _split_docs(docs, seed=seed)
    materialized: dict[str, list[dict[str, Any]]] = {}
    eval_sets: dict[str, Path] = {}
    eval_counts: dict[str, int] = {}
    for split, split_docs in splits.items():
        materialized[split] = _materialize_split(dest, split, split_docs, seed=seed)
        _write_jsonl(dest / "metadata" / f"{split}_docs.jsonl", materialized[split])
        cases = _case_templates(materialized[split], max_cases=max_cases_per_split, seed=seed + len(split))
        eval_path = dest / "eval" / f"hardbench_{split}_eval.jsonl"
        _write_jsonl(eval_path, cases)
        eval_sets[split] = eval_path
        eval_counts[split] = len(cases)
    manifest = dest / "manifest.json"
    _write_json(manifest, {
        "source_family": "KOGL public resource attachments",
        "source_url": "https://www.kogl.or.kr/edu/eduDataList.do",
        "seed": seed,
        "target_docs": target_docs,
        "docs_downloaded": len(docs),
        "extension_counts": dict(Counter(str(doc.get("ext") or "") for doc in docs)),
        "doc_type_counts": dict(Counter(str(doc.get("doc_type") or "") for doc in docs)),
        "splits": {split: len(rows) for split, rows in materialized.items()},
        "eval_sets": {split: str(path) for split, path in eval_sets.items()},
        "eval_case_counts": eval_counts,
        "eval_set": str(eval_sets["test"]),
        "failures": failures[:200],
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "honesty_note": (
            "Eval metadata lives outside corpus split roots. Corpus folders contain public downloaded "
            "documents plus human-ish clutter notes; original files are not moved or modified."
        ),
    })
    return HardBenchBuildResult(
        dest=dest,
        train_root=dest / "corpus" / "train",
        valid_root=dest / "corpus" / "valid",
        test_root=dest / "corpus" / "test",
        train_eval_set_path=eval_sets["train"],
        valid_eval_set_path=eval_sets["valid"],
        eval_set_path=eval_sets["test"],
        manifest_path=manifest,
        docs_downloaded=len(docs),
        train_docs=len(materialized["train"]),
        valid_docs=len(materialized["valid"]),
        test_docs=len(materialized["test"]),
        eval_cases=eval_counts["test"],
    )


def run_hard_benchmark_suite(
    dest: Path,
    *,
    target_docs: int = 180,
    max_data_idx: int = 180,
    max_cases_per_split: int = 240,
    seed: int = DEFAULT_HARDBENCH_SEED,
    top_k: int = 10,
    max_file_bytes: int = 80 * 1024 * 1024,
) -> HardBenchSuiteResult:
    build = build_hard_benchmark(
        dest,
        target_docs=target_docs,
        max_data_idx=max_data_idx,
        max_cases_per_split=max_cases_per_split,
        seed=seed,
        max_file_bytes=max_file_bytes,
    )
    cfg = Config(include_hidden=False)
    cfg.max_files = 1_000_000
    t0 = time.perf_counter()
    for root in (build.train_root, build.valid_root, build.test_root):
        build_agent_index(root, cfg)
    prepare_seconds = time.perf_counter() - t0
    reports: dict[str, Path] = {}
    metrics: dict[str, dict[str, Any]] = {}
    for split, root, eval_set in (
        ("train", build.train_root, build.train_eval_set_path),
        ("valid", build.valid_root, build.valid_eval_set_path),
        ("test", build.test_root, build.eval_set_path),
    ):
        bench: BenchResult = run_benchmark(
            root,
            eval_set=eval_set,
            modes=("raw", "jikji"),
            top_k=top_k,
            prepare=False,
            allow_leak=False,
        )
        reports[split] = bench.report_path
        metrics[split] = bench.metrics
    report_path = build.dest / "reports" / "hardbench_suite_report.json"
    _write_json(report_path, {
        "build": {
            "dest": str(build.dest),
            "train_root": str(build.train_root),
            "valid_root": str(build.valid_root),
            "test_root": str(build.test_root),
            "train_eval_set": str(build.train_eval_set_path),
            "valid_eval_set": str(build.valid_eval_set_path),
            "eval_set": str(build.eval_set_path),
            "docs_downloaded": build.docs_downloaded,
            "train_docs": build.train_docs,
            "valid_docs": build.valid_docs,
            "test_docs": build.test_docs,
            "eval_cases": build.eval_cases,
        },
        "prepare_seconds": round(prepare_seconds, 3),
        "reports": {split: str(path) for split, path in reports.items()},
        "metrics": metrics,
    })
    return HardBenchSuiteResult(
        build=build,
        reports=reports,
        metrics=metrics,
        prepare_seconds=round(prepare_seconds, 3),
        report_path=report_path,
    )
