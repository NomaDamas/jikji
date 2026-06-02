"""Local evaluation sets for Jikji agent-search quality.

The evaluator is intentionally local and deterministic: no LLM calls, no network,
and no source-file mutation. It creates cases from the current folder/index and
measures whether a simple agent-like ranker can recover the expected path from
Jikji artifacts plus native text files.
"""
from __future__ import annotations

import json
import math
import re
import sqlite3
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .agent_index import (
    INTENT_TAXONOMY,
    TEXT_LIKE_EXTENSIONS,
    _atomic_write_text,
    _filename_lookup_keys,
    _tokens_from_text,
)
from .search_index import INSTANT_SEARCH_SCHEMA_VERSION, instant_index_path

EVAL_DIR = ".jikji/eval"
EVAL_SET_NAME = "eval_set.jsonl"
EVAL_REPORT_NAME = "eval_report.json"
EVAL_PROFILE_NAME = "corpus_profile.json"

_CONTENT_BYTES = 256_000
_SEARCH_CHARS = 96_000
_STOPWORDS = {
    "file", "folder", "document", "문서", "파일", "폴더", "관련", "내용", "있는", "찾기", "확장자",
    "a", "an", "and", "are", "as", "at", "be", "been", "before", "by", "can", "check", "clarify",
    "do", "does", "for", "from", "have", "help", "how", "i", "if", "in", "into", "is", "it", "me",
    "my", "need", "needs", "not", "of", "on", "or", "please", "provide", "should", "summarize",
    "that", "the", "there", "this", "to", "up", "usual", "what", "when", "where", "which", "with",
    "you", "your", "about", "after", "all", "any", "asked", "based", "details", "records",
    "파일명", "기억", "본문", "단서", "함께", "같이", "나오던", "나오는", "정도였던", "실무", "검토용",
    "문서를", "자료를", "찾아줘", "정확한", "제목은", "몰라", "가장", "가까운", "후보", "후보를",
    "다시", "확인해야", "제목보다", "의미가", "중에서", "보던", "때", "비슷한", "사본이", "여러",
    "있을", "있어", "우선순위로",
    "계약", "발주", "교육", "강의", "발표", "준비", "점검", "수행", "상황", "기록", "프로젝트",
    "맥락", "파악", "참고", "원본", "평가", "점수", "산정", "근거",
}

# Small, generic local-search expansions. These are not benchmark labels; they
# encode common human descriptions that local agents frequently use when they do
# not remember an exact filename.
_SEMANTIC_ALIASES: dict[str, tuple[str, ...]] = {
    # Keep default expansion deliberately tiny and language-level. Corpus- or
    # domain-specific aliases belong in a future per-root/user config, not in
    # the benchmarkable core scorer.
    "judgment": ("judgement",),
    "judgement": ("judgment",),
}

_FORMAT_QUERY_ALIASES: dict[str, tuple[str, ...]] = {
    ".hwp": ("hwp", "한글", "한글문서", "한글 문서"),
    ".hwpx": ("hwpx", "한글", "한글문서", "한글 문서"),
    ".pdf": ("pdf", "PDF"),
    ".ppt": ("ppt", "파워포인트", "발표자료"),
    ".pptx": ("pptx", "파워포인트", "발표자료"),
    ".xls": ("xls", "엑셀", "스프레드시트"),
    ".xlsx": ("xlsx", "엑셀", "스프레드시트"),
    ".doc": ("doc", "워드"),
    ".docx": ("docx", "워드"),
}

_DOCUMENT_TYPE_PATTERNS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("notice", ("공고문", "공고", "announcement", "notice")),
    ("list", ("목록", "리스트", "대상과제", "list")),
    ("form", ("서식", "양식", "신청서식", "form", "template")),
    ("proposal_request", ("제안요청서", "rfp", "요구사항", "request for proposal")),
    ("proposal", ("제안서", "사업계획서", "proposal")),
    ("plan", ("계획서", "수행계획", "project plan")),
    ("architecture", ("아키텍처", "설계서", "architecture", "design")),
    ("requirements", ("요구사항정의", "요구사항", "requirements")),
    ("contract", ("계약서", "협약서", "contract", "agreement")),
    ("presentation", ("발표", "강의", "ppt", "presentation")),
    ("evaluation", ("평가", "평가지", "점수", "evaluation")),
    ("report", ("보고서", "완료보고", "결과서", "report")),
    ("manual", ("매뉴얼", "지침", "가이드", "manual", "guide")),
    ("archive", ("zip", "압축", "archive")),
)

_KOREAN_PARTICLE_SUFFIXES = (
    "이라고",
    "라고",
    "으로",
    "에서",
    "에게",
    "까지",
    "부터",
    "처럼",
    "보다",
    "이나",
    "나",
    "과",
    "와",
    "은",
    "는",
    "이",
    "가",
    "을",
    "를",
    "에",
    "의",
    "도",
    "만",
    "로",
)


@dataclass
class EvalResult:
    eval_set_path: Path
    report_path: Path | None = None
    cases: int = 0
    scenarios: dict[str, int] | None = None
    metrics: dict[str, Any] | None = None


@dataclass
class BenchAnalysisResult:
    analysis_path: Path
    cases: int
    summary: dict[str, Any]


@dataclass
class SearchIndex:
    """Reusable deterministic search surface built from Jikji map artifacts."""

    root: Path
    rows: list[dict]
    idf: dict[str, float]
    map_backed: bool = False
    inverted: dict[str, list[int]] | None = None


def _read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    rows: list[dict] = []
    for line in path.read_text(encoding="utf-8", errors="ignore").split("\n"):
        if not line.strip():
            continue
        row = json.loads(line)
        if isinstance(row, dict):
            rows.append(row)
    return rows


def _write_json(path: Path, obj: Any) -> None:
    _atomic_write_text(path, json.dumps(obj, ensure_ascii=False, indent=2, sort_keys=True) + "\n")


def _write_jsonl(path: Path, rows: list[dict]) -> None:
    _atomic_write_text(path, "".join(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n" for row in rows))


_COPY_SUFFIX_RE = re.compile(r"(?:\s*\(\d+\)|\s*-\s*copy|\s+copy|_copy)$", re.IGNORECASE)


def _duplicate_key(path: str) -> str:
    """Return a conservative filename key for duplicate diagnostics."""
    stem = Path(Path(path).name).stem.casefold().strip()
    while True:
        cleaned = _COPY_SUFFIX_RE.sub("", stem).strip()
        if cleaned == stem:
            return cleaned
        stem = cleaned


def _path_fingerprints(root: Path) -> dict[str, dict[str, str]]:
    """Map relative paths to stable fingerprints from Jikji's file index."""
    fingerprints: dict[str, dict[str, str]] = {}
    for row in _read_jsonl(root / ".jikji" / "file_index.jsonl"):
        path = str(row.get("path") or "")
        if not path:
            continue
        fingerprints[path] = {
            "sha256": str(row.get("sha256") or ""),
            "duplicate_key": _duplicate_key(path),
        }
    return fingerprints


def _rank_for_expected(
    ranked: list[dict],
    expected: set[str],
    fingerprints: dict[str, dict[str, str]],
    *,
    mode: str = "exact",
) -> int | None:
    """Find the first rank under exact, same-hash, or duplicate-name matching."""
    expected_hashes = {
        fingerprints.get(path, {}).get("sha256", "")
        for path in expected
        if fingerprints.get(path, {}).get("sha256")
    }
    expected_keys = {
        fingerprints.get(path, {}).get("duplicate_key") or _duplicate_key(path)
        for path in expected
    }
    for idx, item in enumerate(ranked, 1):
        path = str(item.get("path") or "")
        if path in expected:
            return idx
        if mode in {"hash", "duplicate"}:
            sha = fingerprints.get(path, {}).get("sha256", "")
            if sha and sha in expected_hashes:
                return idx
        if mode == "duplicate":
            key = fingerprints.get(path, {}).get("duplicate_key") or _duplicate_key(path)
            if key and key in expected_keys:
                return idx
    return None


def _tokenize(text: str, *, limit: int = 64) -> list[str]:
    out: list[str] = []
    seen: set[str] = set()
    for token in _tokens_from_text(text, limit=limit * 4):
        norm = token.casefold()
        if norm in seen or norm in _STOPWORDS or len(norm) < 2:
            continue
        seen.add(norm)
        out.append(token)
        if len(out) >= limit:
            break
    return out


def _query_tokens(query: str, *, limit: int = 32, expand: bool = True) -> list[str]:
    base = [t.casefold() for t in _tokenize(query, limit=limit)]
    out: list[str] = []
    seen: set[str] = set()
    for token in base:
        if token not in seen:
            seen.add(token)
            out.append(token)
        if expand:
            for alias in _SEMANTIC_ALIASES.get(token, ()):
                alias = alias.casefold()
                if alias not in seen and alias not in _STOPWORDS:
                    seen.add(alias)
                    out.append(alias)
            for gram in _tokens_from_text(token, limit=limit):
                gram = gram.casefold()
                if gram not in seen and gram not in _STOPWORDS:
                    seen.add(gram)
                    out.append(gram)
    return out[: limit * 2]


def _read_text_file(path: Path, *, max_bytes: int = _CONTENT_BYTES) -> str:
    try:
        raw = path.read_bytes()[:max_bytes]
    except OSError:
        return ""
    for enc in ("utf-8", "utf-8-sig", "cp949", "euc-kr", "latin-1"):
        try:
            return raw.decode(enc, errors="strict")
        except UnicodeDecodeError:
            continue
    return raw.decode("utf-8", errors="ignore")


def _read_doc_cache(root: Path, cache_path: str) -> str:
    if not cache_path:
        return ""
    path = root / cache_path
    try:
        if path.is_file():
            return path.read_text(encoding="utf-8", errors="ignore")[:_SEARCH_CHARS]
        if path.is_dir():
            parts: list[str] = []
            total = 0
            for chunk in sorted(path.glob("chunk_*.txt")):
                text = chunk.read_text(encoding="utf-8", errors="ignore")
                parts.append(text)
                total += len(text)
                if total >= _SEARCH_CHARS:
                    break
            return "\n".join(parts)[:_SEARCH_CHARS]
    except OSError:
        return ""
    return ""


def _source_text(root: Path, row: dict) -> str:
    cache = row.get("text_cache_path") or ""
    if cache:
        return _read_doc_cache(root, cache)
    ext = str(row.get("ext") or "").lower()
    if ext in TEXT_LIKE_EXTENSIONS:
        return _read_text_file(root / str(row.get("path") or ""))[:_SEARCH_CHARS]
    return ""


def _present_file_rows(root: Path) -> list[dict]:
    rows = _read_jsonl(root / ".jikji" / "file_index.jsonl")
    return sorted([row for row in rows if row.get("status", "present") == "present"], key=lambda row: str(row.get("path") or ""))


def _profile(root: Path, rows: list[dict]) -> dict:
    exts = Counter(str(row.get("ext") or "[noext]").lower() for row in rows)
    parser_required = sum(1 for row in rows if row.get("parser_required"))
    native_text = sum(1 for row in rows if str(row.get("ext") or "").lower() in TEXT_LIKE_EXTENSIONS)
    folders = {str(row.get("path") or "").rsplit("/", 1)[0] for row in rows if "/" in str(row.get("path") or "")}
    return {"root": str(root), "files": len(rows), "folders_with_files": len(folders), "parser_required_files": parser_required, "native_text_files": native_text, "top_extensions": dict(exts.most_common(20))}


def _case(case_id: str, scenario: str, query: str, path: str, evidence: str) -> dict:
    return {"id": case_id, "scenario": scenario, "query": query, "expected_paths": [path], "evidence": evidence[:300]}


def _case_set(case_id: str, scenario: str, query: str, paths: list[str], evidence: str, **extra: Any) -> dict:
    row = {
        "id": case_id,
        "scenario": scenario,
        "query": re.sub(r"\s+", " ", query).strip(),
        "expected_paths": sorted(dict.fromkeys(paths)),
        "evidence": evidence[:500],
    }
    row.update(extra)
    return row


def generate_eval_set(root: Path, *, max_cases: int = 80) -> EvalResult:
    """Create `.jikji/eval/eval_set.jsonl` from the current indexed corpus."""
    root = Path(root).expanduser().resolve()
    rows = _present_file_rows(root)
    if not rows:
        raise FileNotFoundError("No Jikji file index found. Run `jikji prepare ROOT` first.")

    eval_dir = root / EVAL_DIR
    eval_dir.mkdir(parents=True, exist_ok=True)
    profile = _profile(root, rows)
    cases: list[dict] = []
    per_scenario_cap = max(1, max_cases // 5)
    counts: Counter[str] = Counter()

    def add(scenario: str, query: str, row: dict, evidence: str) -> None:
        if len(cases) >= max_cases or counts[scenario] >= per_scenario_cap:
            return
        query = re.sub(r"\s+", " ", query).strip()
        if not query:
            return
        counts[scenario] += 1
        cases.append(_case(f"{scenario}-{counts[scenario]:04d}", scenario, query, str(row.get("path")), evidence))

    for row in rows:
        add("filename_exact", str(row.get("name") or ""), row, "exact file name")
    for row in rows:
        name_tokens = _tokenize(str(row.get("name") or ""), limit=8)
        if name_tokens:
            add("filename_partial", max(name_tokens, key=len), row, "distinct token from file name")

    text_cache: dict[str, str] = {}
    for row in rows:
        text = _source_text(root, row)
        if not text.strip():
            continue
        text_cache[str(row.get("path"))] = text
        content_tokens = [t for t in _tokenize(text, limit=12) if t.casefold() not in str(row.get("path")).casefold()]
        if content_tokens:
            add("lexical_content", " ".join(content_tokens[:2]), row, "exact token(s) from file body/cache")

    for row in rows:
        text = text_cache.get(str(row.get("path"))) or _source_text(root, row)
        tokens = _tokenize(" ".join([str(row.get("summary") or ""), text[:2000], str(row.get("path") or "")]), limit=8)
        if len(tokens) >= 2:
            ext = str(row.get("ext") or "파일")
            add("semantic_description", f"{tokens[0]} {tokens[1]} 관련 {ext} 문서", row, "natural-language description from content/folder tokens")

    for row in rows:
        path = str(row.get("path") or "")
        parent = path.rsplit("/", 1)[0] if "/" in path else "."
        name_tokens = _tokenize(str(row.get("name") or path), limit=6)
        if name_tokens:
            ext = str(row.get("ext") or "확장자없음")
            add("file_description", f"{parent} 폴더에 있는 {ext} {name_tokens[0]} 파일", row, "description from folder, extension, and name token")

    _write_json(eval_dir / EVAL_PROFILE_NAME, profile | {"generated_cases": len(cases), "scenarios": dict(counts)})
    _write_jsonl(eval_dir / EVAL_SET_NAME, cases)
    return EvalResult(eval_set_path=eval_dir / EVAL_SET_NAME, cases=len(cases), scenarios=dict(counts))


_CURATION_NOISE_TERMS = _STOPWORDS | {
    "원본",
    "그림",
    "그림의",
    "이름",
    "크기",
    "현재",
    "이미지",
    "생성",
    "설명",
    "문서",
    "자료",
    "파일",
    "내용",
    "확인",
    "관련",
}


def _curation_terms(card: dict, *, limit: int = 24) -> list[str]:
    terms: list[str] = []
    seen: set[str] = set()
    for source in (
        card.get("rare_terms") or [],
        card.get("content_terms") or [],
        card.get("name_terms") or [],
        card.get("folder_terms") or [],
    ):
        for raw in source:
            term = str(raw).strip()
            norm = term.casefold()
            if (
                not term
                or norm in seen
                or norm in _CURATION_NOISE_TERMS
                or len(norm) < 3
                or re.fullmatch(r"\d+", norm)
            ):
                continue
            seen.add(norm)
            terms.append(term)
            if len(terms) >= limit:
                return terms
    return terms


def _format_label(ext: str) -> str:
    return {
        ".hwp": "한글 문서",
        ".hwpx": "한글 문서",
        ".pdf": "PDF",
        ".ppt": "파워포인트",
        ".pptx": "파워포인트",
        ".xls": "엑셀",
        ".xlsx": "엑셀",
        ".doc": "워드",
        ".docx": "워드",
        ".zip": "압축파일",
    }.get(ext.lower(), ext.lstrip(".") or "파일")


def _term_index_for_cards(cards: list[dict]) -> tuple[dict[str, set[str]], dict[str, set[str]], dict[str, dict]]:
    term_to_paths: dict[str, set[str]] = defaultdict(set)
    name_to_paths: dict[str, set[str]] = defaultdict(set)
    cards_by_path: dict[str, dict] = {}
    for card in cards:
        path = str(card.get("path") or "")
        if not path:
            continue
        cards_by_path[path] = card
        for term in _curation_terms(card, limit=64):
            for variant in _term_variants(term):
                if variant:
                    term_to_paths[variant].add(path)
        for term in _tokenize(str(card.get("name") or ""), limit=16):
            norm = term.casefold()
            if norm not in _CURATION_NOISE_TERMS and len(norm) >= 3:
                name_to_paths[norm].add(path)
    return term_to_paths, name_to_paths, cards_by_path


def _intersect_paths(term_to_paths: dict[str, set[str]], terms: list[str]) -> set[str]:
    sets = [term_to_paths.get(term.casefold(), set()) for term in terms]
    if not sets:
        return set()
    out = set(sets[0])
    for paths in sets[1:]:
        out &= paths
    return out


def generate_realistic_eval_set(root: Path, *, max_cases: int = 240, out: Path | None = None) -> EvalResult:
    """Generate a richer, realistic map-only eval set from Jikji file cards.

    This curation rejects intent-only and metadata-noise cases. A case is added
    only when its clue combination has a bounded ground-truth set, so hit@5 and
    recall@5 are meaningful instead of asking for telepathy.
    """
    root = Path(root).expanduser().resolve()
    cards = [
        row
        for row in _read_jsonl(root / ".jikji" / "file_cards.jsonl")
        if row.get("path") and row.get("parse_status") in {None, "", "success", "archive_listing", "native_text"}
    ]
    if not cards:
        raise FileNotFoundError("No Jikji file_cards.jsonl found. Run `jikji prepare ROOT` first.")

    term_to_paths, name_to_paths, cards_by_path = _term_index_for_cards(cards)
    filename_keys_by_path = {
        path: _filename_lookup_keys(str(card.get("name") or path))
        for path, card in cards_by_path.items()
    }
    filename_anchor_cache: dict[tuple[str, str], set[str]] = {}

    def filename_anchor_matches(raw_anchor: str, *, ext_filter: str = "", cap: int = 10) -> set[str]:
        anchor = _compact_lookup_text(raw_anchor)
        cache_key = (anchor, ext_filter)
        if cache_key in filename_anchor_cache:
            return set(filename_anchor_cache[cache_key])
        matches: set[str] = set()
        if not anchor:
            filename_anchor_cache[cache_key] = matches
            return matches
        for candidate_path, candidate_card in cards_by_path.items():
            if ext_filter and str(candidate_card.get("ext") or "").lower() != ext_filter:
                continue
            if any(anchor in key or key in anchor for key in filename_keys_by_path.get(candidate_path, set()) if len(key) >= 3):
                matches.add(candidate_path)
                if len(matches) > cap:
                    break
        filename_anchor_cache[cache_key] = matches
        return set(matches)

    df = {term: len(paths) for term, paths in term_to_paths.items()}
    cases: list[dict] = []
    counts: Counter[str] = Counter()
    used_query_keys: set[tuple[str, tuple[str, ...]]] = set()
    used_primary_paths: Counter[str] = Counter()
    per_scenario_cap = max(4, max_cases // 7)

    def add(scenario: str, query: str, expected: set[str] | list[str], evidence: str, *, primary: str = "", **extra: Any) -> None:
        if len(cases) >= max_cases or counts[scenario] >= per_scenario_cap:
            return
        paths = sorted(p for p in expected if p in cards_by_path)
        if not paths or len(paths) > 10:
            return
        primary_path = primary or paths[0]
        if used_primary_paths[primary_path] >= 2:
            return
        key = (scenario, tuple(paths), query)
        if key in used_query_keys:
            return
        used_query_keys.add(key)
        used_primary_paths[primary_path] += 1
        counts[scenario] += 1
        cases.append(_case_set(
            f"realistic-{scenario}-{counts[scenario]:04d}",
            scenario,
            query,
            paths,
            evidence,
            ground_truth_type="set" if len(paths) > 1 else "unique",
            expected_count=len(paths),
            **extra,
        ))

    sorted_cards = sorted(cards, key=lambda c: (str(c.get("path") or ""), str(c.get("sha256") or "")))
    for card in sorted_cards:
        path = str(card.get("path") or "")
        ext = str(card.get("ext") or "").lower()
        terms = [t for t in _curation_terms(card, limit=16) if 1 <= df.get(t.casefold(), 999999) <= 40]
        specific_terms = [t for t in terms if df.get(t.casefold(), 999999) <= 12]
        evidence = " / ".join(str(x) for x in (card.get("evidence_previews") or [])[:2]) or path
        if len(specific_terms) >= 3:
            chosen = specific_terms[:3]
            expected = _intersect_paths(term_to_paths, chosen)
            if 1 <= len(expected) <= 5:
                add(
                    "content_three_clue",
                    f"본문에 ‘{chosen[0]}’, ‘{chosen[1]}’, ‘{chosen[2]}’ 단서가 함께 나오는 자료를 찾아줘.",
                    expected,
                    evidence,
                    primary=path,
                    clue_terms=chosen,
                )
        if ext and len(specific_terms) >= 2:
            chosen = specific_terms[:2]
            expected = {p for p in _intersect_paths(term_to_paths, chosen) if str(cards_by_path[p].get("ext") or "").lower() == ext}
            if 1 <= len(expected) <= 5:
                add(
                    "format_content_clue",
                    f"{_format_label(ext)} 중에서 본문에 ‘{chosen[0]}’, ‘{chosen[1]}’ 단서가 같이 나오는 파일을 찾아줘.",
                    expected,
                    evidence,
                    primary=path,
                    clue_terms=chosen,
                    ext=ext,
                )
        phrases = [
            str(p)
            for p in (card.get("phrase_signatures") or [])
            if 8 <= len(str(p)) <= 80 and not any(noise in str(p).casefold() for noise in _CURATION_NOISE_TERMS)
        ]
        if phrases:
            phrase = phrases[0]
            phrase_terms = [t for t in _tokenize(phrase, limit=5) if t.casefold() in term_to_paths]
            expected = _intersect_paths(term_to_paths, phrase_terms[:3]) if phrase_terms else {path}
            if 1 <= len(expected) <= 5:
                add(
                    "exact_phrase_memory",
                    f"‘{phrase}’라는 표현이 보였던 문서를 찾아줘.",
                    expected,
                    evidence,
                    primary=path,
                    phrase=phrase,
                )
        name_terms = [
            t.casefold()
            for t in _tokenize(str(card.get("name") or ""), limit=8)
            if 1 <= len(name_to_paths.get(t.casefold(), set())) <= 8 and t.casefold() not in _CURATION_NOISE_TERMS
        ]
        if name_terms and counts["filename_partial_format"] < per_scenario_cap:
            term = name_terms[0]
            expected = filename_anchor_matches(term, ext_filter=ext, cap=10)
            if 1 <= len(expected) <= 10:
                add(
                    "filename_partial_format",
                    f"파일명에 ‘{term}’가 들어간 {_format_label(ext)} 파일을 찾아줘.",
                    expected,
                    str(card.get("name") or path),
                    primary=path,
                    clue_terms=[term],
                    ext=ext,
                )
        folder_terms = [
            t
            for t in (card.get("folder_terms") or [])
            if t and t.casefold() not in _CURATION_NOISE_TERMS and len(str(t)) >= 3
        ]
        if folder_terms and specific_terms:
            folder_term = str(folder_terms[0])
            body_term = specific_terms[0]
            expected = {
                p
                for p in term_to_paths.get(body_term.casefold(), set())
                if folder_term.casefold() in str(Path(p).parent).casefold()
            }
            if ext:
                expected = {p for p in expected if str(cards_by_path[p].get("ext") or "").lower() == ext}
            if 1 <= len(expected) <= 8:
                add(
                    "folder_context_content",
                    f"`{folder_term}` 폴더 맥락에서 ‘{body_term}’ 단서가 있는 {_format_label(ext)} 파일을 찾아줘.",
                    expected,
                    path,
                    primary=path,
                    clue_terms=[folder_term, body_term],
                    ext=ext,
                )
        if len(cases) >= max_cases:
            break

    for group in _read_jsonl(root / ".jikji" / "duplicate_map.jsonl"):
        if counts["duplicate_cluster"] >= per_scenario_cap:
            break
        members = [str(p) for p in (group.get("members") or []) if str(p) in cards_by_path]
        if not (2 <= len(members) <= 10):
            continue
        rep = str(group.get("representative") or members[0])
        near_duplicate_members = {
            path
            for path in cards_by_path
            if _duplicate_key(path) == _duplicate_key(rep)
        }
        stem = Path(rep).stem[:120].strip()
        anchor_members = filename_anchor_matches(stem, cap=10)
        expected_members = set(members) | near_duplicate_members | anchor_members
        if not (2 <= len(expected_members) <= 10):
            continue
        add(
            "duplicate_cluster",
            f"‘{stem}’와 내용이 같은 사본이나 백업 파일들을 찾아줘.",
            expected_members,
            "duplicate group",
            primary=rep,
            duplicate_group_id=group.get("group_id", ""),
        )
        if len(cases) >= max_cases:
            break

    eval_dir = root / EVAL_DIR
    eval_dir.mkdir(parents=True, exist_ok=True)
    out_path = Path(out).expanduser().resolve() if out else eval_dir / "realistic_eval_set.jsonl"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    _write_jsonl(out_path, cases)
    _write_json(out_path.with_suffix(".profile.json"), {
        "root": str(root),
        "cases": len(cases),
        "scenarios": dict(counts),
        "curation_rules": {
            "reject_intent_only": True,
            "reject_metadata_noise_terms": sorted(_CURATION_NOISE_TERMS),
            "max_expected_paths": 10,
            "max_cases_per_primary_path": 2,
            "ground_truth": "set",
        },
    })
    return EvalResult(eval_set_path=out_path, cases=len(cases), scenarios=dict(counts))


def _candidate_docs(root: Path) -> list[dict]:
    doc_by_path = {row.get("path"): row for row in _read_jsonl(root / ".jikji" / "document_index.jsonl")}
    out: list[dict] = []
    for row in _present_file_rows(root):
        merged = dict(row)
        doc = doc_by_path.get(row.get("path"))
        if doc:
            merged.update({k: v for k, v in doc.items() if v not in (None, "")})
        merged["_source_text"] = _source_text(root, merged)
        out.append(merged)
    return out


def _map_candidate_docs(root: Path) -> list[dict]:
    """Read Jikji's AutoRAG-ready map artifacts as search candidates.

    This intentionally avoids reading full source documents/doc_text during
    ranking. `file_cards.jsonl` and `chunk_map.jsonl` are the map.
    """
    cards = _read_jsonl(root / ".jikji" / "file_cards.jsonl")
    if not cards:
        return []
    chunks_by_path: dict[str, list[dict]] = defaultdict(list)
    for chunk in _read_jsonl(root / ".jikji" / "chunk_map.jsonl"):
        path = str(chunk.get("path") or "")
        if path:
            chunks_by_path[path].append(chunk)

    rows: list[dict] = []
    for card in cards:
        path = str(card.get("path") or "")
        chunks = chunks_by_path.get(path, [])
        chunk_text = "\n".join(
            " ".join([
                str(chunk.get("preview") or ""),
                " ".join(str(x) for x in chunk.get("content_terms") or []),
                " ".join(str(x) for x in chunk.get("rare_terms") or []),
                " ".join(str(x) for x in chunk.get("phrase_signatures") or []),
                " ".join(str(x) for x in chunk.get("intent_tags") or []),
            ])
            for chunk in chunks[:48]
        )
        content_terms = [str(x) for x in (card.get("content_terms") or [])]
        rare_terms = [str(x) for x in (card.get("rare_terms") or [])]
        phrase_signatures = [str(x) for x in (card.get("phrase_signatures") or [])]
        intent_tags = [str(x) for x in (card.get("intent_tags") or [])]
        format_hints = [str(x) for x in (card.get("format_hints") or [])]
        evidence_previews = [str(x) for x in (card.get("evidence_previews") or [])]
        row = {
            "path": path,
            "name": card.get("name", ""),
            "ext": card.get("ext", ""),
            "sha256": card.get("sha256", ""),
            "duplicate_group_id": card.get("duplicate_group_id", ""),
            "filename_lookup_keys": list(card.get("filename_lookup_keys") or []),
            "keywords": (
                content_terms
                + rare_terms
                + phrase_signatures
            ),
            "semantic_hints": (
                intent_tags
                + list(card.get("folder_roles") or [])
                + format_hints
                + list(card.get("path_terms") or [])
                + list(card.get("name_terms") or [])
                + list(card.get("folder_terms") or [])
            ),
            "summary": card.get("summary", ""),
            "_source_text": "\n".join(str(x) for x in card.get("evidence_previews") or []),
            "_map_card": card,
            "_map_chunks": chunks,
            "_map_text": chunk_text,
            "_map_content_text": " ".join(content_terms).casefold(),
            "_map_rare_text": " ".join(rare_terms).casefold(),
            "_map_phrase_text": " ".join(phrase_signatures).casefold(),
            "_map_intent_text": " ".join(intent_tags).casefold(),
            "_map_format_text": " ".join(format_hints).casefold(),
            "_map_evidence_text": " ".join(evidence_previews + [str(card.get("summary") or "")]).casefold(),
            "_map_all_text": " ".join([
                " ".join(content_terms),
                " ".join(rare_terms),
                " ".join(phrase_signatures),
                " ".join(intent_tags),
                " ".join(format_hints),
                " ".join(evidence_previews),
                str(card.get("summary") or ""),
                chunk_text,
            ]).casefold(),
        }
        rows.append(row)
    return rows


def _field_tokens(text: str, *, limit: int = 256) -> set[str]:
    return {t.casefold() for t in _tokenize(text, limit=limit)}


def _term_variants(term: str) -> set[str]:
    term = term.casefold().strip()
    variants = {term} if term else set()
    if not term:
        return variants
    for suffix in _KOREAN_PARTICLE_SUFFIXES:
        if term.endswith(suffix) and len(term) > len(suffix) + 1:
            variants.add(term[: -len(suffix)])
            break
    # Korean/English mixed identifiers often appear with punctuation stripped
    # differently between filenames, map terms, and user queries.
    compact = re.sub(r"[\s_·ㆍ.,;:()\[\]{}<>/\\\\|+-]+", "", term)
    if compact and compact != term and len(compact) >= 2:
        variants.add(compact)
    return variants


def _idf_for_rows(rows: list[dict]) -> dict[str, float]:
    df: Counter[str] = Counter()
    for row in rows:
        card = row.get("_map_card") or {}
        fields = " ".join([
            str(row.get("path") or ""),
            str(row.get("name") or ""),
            " ".join(str(x) for x in (row.get("keywords") or [])),
            " ".join(str(x) for x in (row.get("semantic_hints") or [])),
            str(row.get("summary") or "")[:1000],
            str(row.get("_source_text") or "")[:2000],
            str(row.get("_map_text") or "")[:3000],
            " ".join(str(x) for x in (card.get("rare_terms") or [])),
            " ".join(str(x) for x in (card.get("content_terms") or [])),
            " ".join(str(x) for x in (card.get("phrase_signatures") or [])),
            " ".join(str(x) for x in (card.get("intent_tags") or [])),
        ])
        df.update(_field_tokens(fields, limit=512))
    total = max(1, len(rows))
    return {term: math.log((1 + total) / (1 + freq)) + 1.0 for term, freq in df.items()}


def _index_terms_for_row(row: dict) -> set[str]:
    card = row.get("_map_card") or {}
    fields = " ".join([
        str(row.get("path") or ""),
        str(row.get("name") or ""),
        " ".join(str(x) for x in (row.get("keywords") or [])),
        " ".join(str(x) for x in (row.get("semantic_hints") or [])),
        str(row.get("summary") or "")[:1000],
        str(row.get("_source_text") or "")[:2000],
        str(row.get("_map_text") or "")[:3000],
        " ".join(str(x) for x in (card.get("rare_terms") or [])),
        " ".join(str(x) for x in (card.get("content_terms") or [])),
        " ".join(str(x) for x in (card.get("phrase_signatures") or [])),
        " ".join(str(x) for x in (card.get("intent_tags") or [])),
    ])
    terms = _field_tokens(fields, limit=512)
    expanded = set(terms)
    for term in terms:
        expanded.update(_term_variants(term))
    persisted = {str(x) for x in (card.get("filename_lookup_keys") or row.get("filename_lookup_keys") or []) if str(x)}
    filename_keys = persisted or (
        set(_filename_lookup_keys(str(row.get("name") or "")))
        | set(_filename_lookup_keys(str(row.get("path") or "")))
    )
    row["_filename_lookup_keys"] = filename_keys
    expanded.update(filename_keys)
    return expanded


def _inverted_for_rows(rows: list[dict]) -> dict[str, list[int]]:
    buckets: dict[str, list[int]] = defaultdict(list)
    for idx, row in enumerate(rows):
        terms = _index_terms_for_row(row)
        row["_index_terms"] = terms
        for term in terms:
            buckets[term].append(idx)
    return dict(buckets)


def _contains(text: str, token: str) -> bool:
    return token in text


def _score(query: str, row: dict, *, idf: dict[str, float] | None = None) -> tuple[float, list[str]]:
    q = query.casefold().strip()
    tokens = _query_tokens(query, limit=32, expand=True)
    original_tokens = set(_query_tokens(query, limit=32, expand=False))
    query_raw_pairs = [
        (t, t.casefold()) for t in _tokens_from_text(query, limit=48) if t.casefold() not in _STOPWORDS
    ]
    query_raw_tokens = [norm for _, norm in query_raw_pairs]
    name = str(row.get("name") or "").casefold()
    path = str(row.get("path") or "").casefold()
    parent = path.rsplit("/", 1)[0] if "/" in path else ""
    ext = str(row.get("ext") or "").casefold().lstrip(".")
    keywords = " ".join(str(x) for x in (row.get("keywords") or [])).casefold()
    hints = " ".join(str(x) for x in (row.get("semantic_hints") or [])).casefold()
    summary = str(row.get("summary") or "").casefold()
    body = str(row.get("_source_text") or "").casefold()

    score = 0.0
    reasons: list[str] = []
    if q and q == name:
        score += 140
        reasons.append("exact-name")
    elif q and q in name:
        score += 80
        reasons.append("name-contains-query")
    if q and q in path:
        score += 70
        reasons.append("path-contains-query")
    if q and q in body:
        score += 35
        reasons.append("body-contains-query")

    original_hits = 0
    for token in tokens:
        rare = min(3.0, (idf or {}).get(token, 1.0))
        is_original = token in original_tokens
        original_mult = 1.15 if is_original else 0.72
        token_score = 0.0
        if _contains(name, token):
            token_score += 28
        if _contains(parent, token):
            token_score += 24
        elif _contains(path, token):
            token_score += 18
        if _contains(keywords, token):
            token_score += 13
        if _contains(hints, token):
            token_score += 16
        if _contains(summary, token):
            token_score += 8
        if _contains(body, token):
            # Body matches are useful but very noisy in large personal corpora;
            # let rarity matter and avoid common-word dominance.
            token_score += 2.6 * rare
        if ext and token == ext:
            token_score += 8
        if token_score:
            score += token_score * original_mult
            if is_original:
                original_hits += 1

    for raw_token, token in query_raw_pairs:
        if 2 <= len(raw_token) <= 6 and raw_token.upper() == raw_token and re.fullmatch(r"[a-z0-9]+", token):
            # Short IDs/acronyms such as UMF, SSIC, PTP are usually decisive
            # when present in a filename or folder path.
            if token in name or token in path:
                score += 90
                reasons.append("short-id-in-path")

    for left, right in zip(query_raw_tokens, query_raw_tokens[1:], strict=False):
        phrase = f"{left} {right}"
        underscored = f"{left}_{right}"
        hyphenated = f"{left}-{right}"
        if phrase in path or underscored in path or hyphenated in path or phrase in name:
            score += 80
            reasons.append("query-bigram-in-path")
        elif phrase in hints:
            score += 35

    # Reward files whose path/name covers several meaningful query concepts.
    compact_path = f"{parent} {name} {hints}"
    path_hits = sum(1 for token in original_tokens if token and token in compact_path)
    if path_hits >= 2:
        score += 24 + 12 * path_hits
        reasons.append("multi-token-path-hints")
    elif path_hits == 1:
        score += 8

    if original_hits >= 3:
        score += min(42, 7 * original_hits)
        reasons.append("multi-token-overlap")
    if score > 0 and not reasons:
        reasons.append("token-overlap")
    return score, reasons


def _query_intents(query: str) -> list[str]:
    compact = re.sub(r"\s+", "", query or "").casefold()
    tags: list[str] = []
    for tag, needles in INTENT_TAXONOMY.items():
        if tag.casefold() in compact or any(re.sub(r"\s+", "", needle).casefold() in compact for needle in needles):
            tags.append(tag)
    return tags


def _query_formats(query: str) -> set[str]:
    q = (query or "").casefold()
    out: set[str] = set()
    for ext, aliases in _FORMAT_QUERY_ALIASES.items():
        if ext.lstrip(".") in q or any(alias.casefold() in q for alias in aliases):
            out.add(ext)
    return out


def _query_document_types(query: str) -> set[str]:
    compact = re.sub(r"\s+", "", query or "").casefold()
    out: set[str] = set()
    for bucket, needles in _DOCUMENT_TYPE_PATTERNS:
        if any(re.sub(r"\s+", "", needle.casefold()) in compact for needle in needles):
            out.add(bucket)
    if "설계" in compact:
        out.add("architecture")
    if "요구" in compact:
        out.update({"requirements", "proposal_request"})
    if "목록" in compact or "리스트" in compact:
        out.add("list")
    return out


def _is_filename_query(query: str) -> bool:
    compact = re.sub(r"\s+", "", query or "").casefold()
    return any(needle in compact for needle in ("파일명", "제목", "이름", "filename", "name"))


def _is_duplicate_query(query: str) -> bool:
    compact = re.sub(r"\s+", "", query or "").casefold()
    explicit = ("사본", "중복", "동일", "duplicate", "copy")
    same_content = ("내용이같", "같은내용", "같은파일", "samecontent", "samefile")
    if any(needle in compact for needle in explicit + same_content):
        return True
    # "백업" often appears in folder names such as USB백업; by itself it should
    # not turn a folder-context query into a duplicate-search request.
    return ("백업" in compact or "backup" in compact) and any(
        needle in compact for needle in ("같은", "내용", "사본", "중복", "동일", "copy", "duplicate")
    )


def _is_folder_context_query(query: str) -> bool:
    compact = re.sub(r"\s+", "", query or "").casefold()
    return "/" in (query or "") or any(
        needle in compact
        for needle in (
            "폴더",
            "경로",
            "아래",
            "안에",
            "정리전",
            "검토중",
            "임시보관",
            "공유드라이브",
            "팀자료실",
            "내문서",
            "받은자료",
            "folder",
            "path",
            "directory",
        )
    )


def _is_original_document_query(query: str) -> bool:
    compact = re.sub(r"\s+", "", query or "").casefold()
    return any(
        needle in compact
        for needle in (
            "원본",
            "문서",
            "자료",
            "파일",
            "document",
            "sourcefile",
            "original",
        )
    )


def _quoted_query_terms(query: str) -> list[str]:
    terms: list[str] = []
    seen: set[str] = set()
    for match in re.finditer(r"[‘'\"“]([^’'\"”]{2,240})[’'\"”]", query or ""):
        term = match.group(1).strip().casefold()
        if term and term not in seen:
            seen.add(term)
            terms.append(term)
    return terms


def _compact_lookup_text(text: str) -> str:
    """Normalize a human-remembered filename/title for deterministic lookup.

    This is deliberately lexical: it removes punctuation/spacing variation but
    does not call embeddings, LLMs, or a semantic model.
    """
    return re.sub(r"[^0-9a-z가-힣ぁ-ゟ゠-ヿ一-鿿]+", "", (text or "").casefold())

def _query_filename_anchors(query: str) -> list[str]:
    """Extract likely remembered filename/title anchors from a query."""
    anchors: list[str] = []
    seen: set[str] = set()

    def add(raw: str) -> None:
        for key in _filename_lookup_keys(raw):
            if len(key) >= 2 and key not in seen:
                seen.add(key)
                anchors.append(key)

    for term in _quoted_query_terms(query):
        add(term)
    if anchors:
        return anchors

    # Fallback for unquoted filename/duplicate requests: short, meaningful
    # alnum/Hangul identifiers can still be a decisive lexical anchor.
    if _is_filename_query(query) or _is_duplicate_query(query):
        for token in _query_tokens(query, limit=16, expand=False):
            if token not in _STOPWORDS and len(_compact_lookup_text(token)) >= 3:
                add(token)
    return anchors


def _score_map(query: str, row: dict, *, idf: dict[str, float] | None = None) -> tuple[float, list[str], list[str], list[str]]:
    """Score a candidate using only Jikji map-card/chunk features."""
    card = row.get("_map_card") or {}
    chunks = row.get("_map_chunks") or []
    if not card:
        return 0.0, [], [], []

    q = query.casefold().strip()
    tokens = _query_tokens(query, limit=40, expand=True)
    original_tokens = set(_query_tokens(query, limit=40, expand=False))
    quoted_terms = _quoted_query_terms(query)
    for term in quoted_terms:
        if term not in tokens:
            tokens.insert(0, term)
        original_tokens.add(term)
    query_intents = _query_intents(query)
    query_formats = _query_formats(query)
    query_doc_types = _query_document_types(query)
    filename_query = _is_filename_query(query)
    duplicate_query = _is_duplicate_query(query)
    folder_context_query = _is_folder_context_query(query)
    original_document_query = _is_original_document_query(query)
    filename_anchors = _query_filename_anchors(query) if (filename_query or duplicate_query) else []
    format_mismatch = False
    ext = str(card.get("ext") or "").lower()
    name_text = str(card.get("name") or row.get("name") or "").casefold()
    path_text = str(card.get("path") or row.get("path") or "").casefold()
    persisted_filename_keys = {
        str(x) for x in (card.get("filename_lookup_keys") or row.get("_filename_lookup_keys") or []) if str(x)
    }
    compact_name_keys = persisted_filename_keys or _filename_lookup_keys(str(card.get("name") or row.get("name") or ""))
    compact_path_keys = persisted_filename_keys or _filename_lookup_keys(str(card.get("path") or row.get("path") or ""))

    rare_terms = [str(x) for x in (card.get("rare_terms") or [])]
    content_terms = [str(x) for x in (card.get("content_terms") or [])]
    phrases = [str(x) for x in (card.get("phrase_signatures") or [])]
    intents = [str(x) for x in (card.get("intent_tags") or [])]
    evidence_previews = [str(x) for x in (card.get("evidence_previews") or [])]
    map_text = str(row.get("_map_all_text") or "").casefold()
    folder_text = " ".join(str(x) for x in (card.get("folder_terms") or []) + (card.get("folder_roles") or [])).casefold()
    path_lookup_text = " ".join([
        path_text,
        folder_text,
        " ".join(str(x) for x in (card.get("path_terms") or [])),
        " ".join(str(x) for x in (card.get("name_terms") or [])),
    ]).casefold()

    score = 0.0
    reasons: list[str] = []
    matched_terms: list[str] = []
    matched_intents: list[str] = []

    if query_formats:
        if ext in query_formats:
            # Structural prior: an explicit "Excel/PDF/HWP" request should
            # matter, but it must not fully override body evidence.
            score += 180
            reasons.append("format-match")
        else:
            # If user explicitly asks for a format, non-matching formats should
            # be discounted, not cliff-penalized. Format detection is lexical
            # and may be spurious in natural queries.
            format_mismatch = True
            reasons.append("format-mismatch")

    doc_type = _document_type_bucket(str(card.get("path") or row.get("path") or ""), str(card.get("name") or row.get("name") or ""))
    if query_doc_types:
        if doc_type in query_doc_types:
            # Structural prior from filename/path, not a semantic model.
            score += 170
            reasons.append("doc-type-match")
        elif doc_type == "unknown":
            score -= 25

    for intent in query_intents:
        if intent in intents:
            score += 48
            reasons.append("intent-tag")
            matched_intents.append(intent)
        elif intent in " ".join(str(x) for x in (card.get("folder_roles") or [])):
            score += 18
            matched_intents.append(intent)

    content_text = str(row.get("_map_content_text") or " ".join(content_terms)).casefold()
    rare_text = str(row.get("_map_rare_text") or " ".join(rare_terms)).casefold()
    phrase_text = str(row.get("_map_phrase_text") or " ".join(phrases)).casefold()
    evidence_text = str(row.get("_map_evidence_text") or " ".join(evidence_previews + [str(card.get("summary") or "")])).casefold()
    original_hits = 0
    quoted_hits = 0
    for token in tokens:
        rare = min(4.0, (idf or {}).get(token, 1.0))
        is_original = token in original_tokens
        mult = 1.25 if is_original else 0.65
        token_score = 0.0
        if token in content_text:
            token_score += 24 * rare
        if token in rare_text:
            token_score += 35 * rare
        if token in phrase_text:
            token_score += 42 * rare
        if token in evidence_text:
            token_score += 18 * rare
        if token in map_text:
            token_score += 5 * rare
        if token in path_lookup_text:
            # Folder/path clues are structural evidence. They are especially
            # important for local-agent discovery tasks where the user often
            # remembers "that folder under shared drive" rather than content.
            token_score += (34 if folder_context_query else 11) * rare
        if token_score:
            score += token_score * mult
            if token not in matched_terms and is_original:
                matched_terms.append(token)
                original_hits += 1
            if token in quoted_terms:
                quoted_hits += 1

    for term in quoted_terms:
        term_score = 0.0
        if term in name_text:
            term_score += 420 if filename_query else 180
        elif term in path_text:
            term_score += 300 if filename_query else (240 if folder_context_query else 130)
        if term in path_lookup_text:
            term_score += 160 if folder_context_query else 45
        if term in evidence_text:
            term_score += 90
        if term in content_text:
            term_score += 90
        if term in rare_text:
            term_score += 110
        if term in phrase_text:
            term_score += 120
        if term in map_text:
            term_score += 45
        if term_score:
            score += term_score
            if term not in matched_terms:
                matched_terms.append(term)
            reasons.append("quoted-term")

    for anchor in filename_anchors:
        anchor_score = 0.0
        if anchor in compact_name_keys:
            anchor_score += 1200
        elif any(anchor in key or key in anchor for key in compact_name_keys if len(key) >= 3):
            anchor_score += 980
        elif anchor in compact_path_keys:
            anchor_score += 640
        elif any(anchor in key or key in anchor for key in compact_path_keys if len(key) >= 3):
            anchor_score += 420
        if anchor_score:
            score += anchor_score
            reasons.append("filename-anchor")
            if anchor not in matched_terms:
                matched_terms.append(anchor)

    if duplicate_query and str(card.get("duplicate_group_id") or ""):
        score += 220
        reasons.append("duplicate-group")

    query_raw = [t.casefold() for t in _tokens_from_text(query, limit=64) if t.casefold() not in _STOPWORDS]
    for left, right in zip(query_raw, query_raw[1:], strict=False):
        phrase = f"{left} {right}"
        slash_phrase = f"{left}/{right}"
        if phrase in phrase_text:
            score += 80
            reasons.append("map-phrase")
        elif phrase in evidence_text:
            score += 35
            reasons.append("evidence-phrase")
        if folder_context_query and (phrase in path_lookup_text or slash_phrase in path_lookup_text):
            score += 150
            reasons.append("path-phrase")

    if folder_context_query:
        path_hits = sum(1 for token in original_tokens if token and token in path_lookup_text)
        folder_hits = sum(1 for token in original_tokens if token and token in folder_text)
        if path_hits >= 2:
            score += 120 + 45 * path_hits + 20 * folder_hits
            reasons.append("folder-context-path")
        elif path_hits == 1:
            score += 35
            reasons.append("folder-context-token")

    best_chunk_score = 0.0
    best_evidence = ""
    for chunk in chunks[:64]:
        chunk_text = " ".join([
            str(chunk.get("preview") or ""),
            " ".join(str(x) for x in (chunk.get("content_terms") or [])),
            " ".join(str(x) for x in (chunk.get("rare_terms") or [])),
            " ".join(str(x) for x in (chunk.get("phrase_signatures") or [])),
            " ".join(str(x) for x in (chunk.get("intent_tags") or [])),
        ]).casefold()
        chunk_hits = sum(1 for token in original_tokens if token and token in chunk_text)
        quoted_chunk_hits = sum(1 for token in quoted_terms if token and token in chunk_text)
        chunk_intents = sum(1 for intent in query_intents if intent in (chunk.get("intent_tags") or []))
        chunk_score = 0.0
        if chunk_hits:
            chunk_score += 32 * chunk_hits + 7 * chunk_hits * chunk_hits
        if quoted_chunk_hits:
            chunk_score += 110 * quoted_chunk_hits + 45 * quoted_chunk_hits * quoted_chunk_hits
            if quoted_terms and quoted_chunk_hits >= len(set(quoted_terms)):
                chunk_score += 220
        if chunk_intents:
            chunk_score += 30 * chunk_intents
        if q and q in chunk_text:
            chunk_score += 70
        if chunk_score > best_chunk_score:
            best_chunk_score = chunk_score
            best_evidence = str(chunk.get("preview") or "")
    if best_chunk_score:
        score += best_chunk_score
        reasons.append("chunk-map")
        if best_evidence and best_evidence not in evidence_previews:
            evidence_previews = [best_evidence] + evidence_previews

    if original_hits >= 2:
        score += 40 + 18 * original_hits
        reasons.append("multi-map-term")
    if original_hits >= 3:
        score += 45
        reasons.append("strong-map-term")
    if quoted_terms and quoted_hits >= len(set(quoted_terms)):
        score += 220 + 55 * len(set(quoted_terms))
        reasons.append("all-quoted-terms")
    if score > 0 and not reasons:
        reasons.append("map-overlap")
    if format_mismatch and score > 0:
        # Bounded multiplicative discount: preserves strong filename/body
        # evidence while still preferring the requested extension when
        # otherwise comparable.
        score *= 0.24
    if original_document_query and ext in {".txt", ".md"} and re.search(r"(메모|memo|링크|link|shortcut)", name_text):
        score *= 0.18
        reasons.append("note-decoy-discount")

    return score, reasons, matched_terms[:12], matched_intents[:8]


def build_search_index(root: Path) -> SearchIndex:
    root = Path(root).expanduser().resolve()
    rows = _map_candidate_docs(root)
    map_backed = bool(rows)
    if not rows:
        rows = _candidate_docs(root)
    return SearchIndex(
        root=root,
        rows=rows,
        idf=_idf_for_rows(rows),
        map_backed=map_backed,
        inverted=_inverted_for_rows(rows),
    )


def _candidate_row_ids(index: SearchIndex, query: str) -> list[int]:
    inverted = index.inverted or {}
    if not inverted:
        return list(range(len(index.rows)))
    needles: set[str] = set(_query_tokens(query, limit=48, expand=True))
    needles.update(_quoted_query_terms(query))
    filename_anchors = _query_filename_anchors(query)
    for term in list(needles):
        needles.update(t.casefold() for t in _tokenize(term, limit=8))
        needles.update(_filename_lookup_keys(term))
    needles.update(intent.casefold() for intent in _query_intents(query))
    needles.update(ext.lstrip(".") for ext in _query_formats(query))

    counts: Counter[int] = Counter()
    direct_ids: list[int] = []
    if filename_anchors and (_is_filename_query(query) or _is_duplicate_query(query)):
        for idx, row in enumerate(index.rows):
            row_keys = row.get("_filename_lookup_keys") or (
                set(_filename_lookup_keys(str(row.get("name") or "")))
                | set(_filename_lookup_keys(str(row.get("path") or "")))
            )
            if any(
                anchor in row_keys
                or any(anchor in key or key in anchor for key in row_keys if len(key) >= 3)
                for anchor in filename_anchors
            ):
                direct_ids.append(idx)
    for needle in needles:
        if not needle:
            continue
        for idx in inverted.get(needle.casefold(), []):
            counts[idx] += 1
    if not counts:
        if direct_ids:
            seen_direct = set(direct_ids)
            return direct_ids + [idx for idx in range(len(index.rows)) if idx not in seen_direct]
        return list(range(len(index.rows)))

    # Score only plausible rows first. This keeps large personal corpora
    # benchmarkable while preserving deterministic fallback for very broad
    # queries that match too much of the corpus.
    cap = 8000
    ranked_ids = [idx for idx, _ in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:cap]]
    if not direct_ids:
        return ranked_ids
    seen: set[int] = set()
    merged: list[int] = []
    for idx in direct_ids + ranked_ids:
        if idx not in seen:
            seen.add(idx)
            merged.append(idx)
    return merged[: max(cap, len(direct_ids))]


def _document_type_bucket(path: str, name: str = "") -> str:
    text = f"{path} {name}".casefold()
    compact = re.sub(r"\s+", "", text)
    for bucket, needles in _DOCUMENT_TYPE_PATTERNS:
        if any(re.sub(r"\s+", "", needle.casefold()) in compact for needle in needles):
            return bucket
    ext = Path(name or path).suffix.lower().lstrip(".")
    return f"ext:{ext}" if ext else "unknown"


def _folder_bucket(path: str) -> str:
    parts = [p for p in Path(path).parts[:-1] if p not in {"", "."}]
    return "/".join(parts[-2:]) if parts else "."


def _family_bucket(path: str) -> str:
    stem = Path(path).stem.casefold()
    stem = _COPY_SUFFIX_RE.sub("", stem)
    stem = re.sub(r"\d{4,}", "", stem)
    stem = re.sub(r"[\s_·ㆍ.,;:()\[\]{}<>/\\\\|+-]+", "", stem)
    return stem or Path(path).stem.casefold()


def _diversify_ranked(ranked: list[dict], query: str, *, top_k: int) -> list[dict]:
    """Diversify top results for clue-only map queries.

    When many files share all quoted/body clues, agents benefit more from a
    top-k slate that covers document types and nearby folders than from five
    almost-identical notices/forms. This is still map-only deterministic
    ranking; it does not inspect source files or use embeddings.
    """
    if not ranked or not _quoted_query_terms(query) or top_k <= 3:
        return ranked[:top_k]
    duplicate_query = _is_duplicate_query(query)
    if duplicate_query:
        # For "find copies/backups" tasks, repeated duplicate groups are the
        # desired output rather than redundancy to suppress.
        return ranked[:top_k]

    def diversity_sort_score(item: dict) -> float:
        path = str(item.get("path") or "")
        doc_type = _document_type_bucket(path, str(item.get("name") or ""))
        score = float(item.get("score") or 0.0)
        if doc_type != "unknown":
            # Tie-breaker only: prefer a slate with recognizable document
            # types, but leave query-specific doc-type scoring in _score_map.
            score += 120
        return score

    pool = sorted(
        ranked[: max(top_k * 12, 80)],
        key=lambda item: (-diversity_sort_score(item), str(item.get("path") or "")),
    )
    chosen: list[dict] = []
    used_paths: set[str] = set()
    type_counts: Counter[str] = Counter()
    folder_counts: Counter[str] = Counter()
    group_counts: Counter[str] = Counter()
    family_counts: Counter[str] = Counter()
    spreadsheet_format_query = bool(_query_formats(query) & {".xls", ".xlsx"})

    def can_take(item: dict, *, relaxed: bool = False) -> bool:
        path = str(item.get("path") or "")
        if path in used_paths:
            return False
        doc_type = _document_type_bucket(path, str(item.get("name") or ""))
        folder = _folder_bucket(path)
        group = str(item.get("duplicate_group_id") or "")
        family = _family_bucket(path)
        if relaxed:
            return True
        if group and group_counts[group] >= 1:
            return False
        if family_counts[family] >= 1:
            return False
        if not spreadsheet_format_query and type_counts[doc_type] >= 2:
            return False
        if folder_counts[folder] >= 3:
            return False
        return True

    for item in pool:
        if len(chosen) >= top_k:
            break
        if not can_take(item):
            continue
        chosen.append(item)
        path = str(item.get("path") or "")
        used_paths.add(path)
        type_counts[_document_type_bucket(path, str(item.get("name") or ""))] += 1
        folder_counts[_folder_bucket(path)] += 1
        family_counts[_family_bucket(path)] += 1
        group = str(item.get("duplicate_group_id") or "")
        if group:
            group_counts[group] += 1

    for item in pool:
        if len(chosen) >= top_k:
            break
        if can_take(item, relaxed=True):
            chosen.append(item)
            used_paths.add(str(item.get("path") or ""))

    return chosen[:top_k]


def _expand_duplicate_ranked(index: SearchIndex, ranked: list[dict], query: str, *, top_k: int) -> list[dict]:
    if not ranked or not _is_duplicate_query(query):
        return ranked[:top_k]
    row_by_path = {str(row.get("path") or ""): row for row in index.rows}
    rows_by_group: dict[str, list[dict]] = defaultdict(list)
    rows_by_family: dict[str, list[dict]] = defaultdict(list)
    for row in index.rows:
        path = str(row.get("path") or "")
        card = row.get("_map_card") or {}
        group = str(card.get("duplicate_group_id") or row.get("duplicate_group_id") or "")
        if group:
            rows_by_group[group].append(row)
        rows_by_family[_family_bucket(path)].append(row)

    out: list[dict] = []
    seen: set[str] = set()

    def append_result(item: dict) -> None:
        path = str(item.get("path") or "")
        if path and path not in seen and len(out) < top_k:
            seen.add(path)
            out.append(item)

    def row_to_result(row: dict, seed: dict) -> dict:
        card = row.get("_map_card") or {}
        return {
            "path": row.get("path"),
            "name": row.get("name"),
            "score": max(0.001, float(seed.get("score") or 0.0) - 0.001),
            "reasons": sorted(set((seed.get("reasons") or []) + ["duplicate-expansion"])),
            "matched_terms": seed.get("matched_terms") or [],
            "matched_intents": seed.get("matched_intents") or [],
            "duplicate_group_id": card.get("duplicate_group_id", ""),
            "evidence": list(card.get("evidence_previews") or [])[:3],
        }

    anchors = _query_filename_anchors(query)

    def anchor_score(row: dict) -> float:
        if not anchors:
            return 0.0
        path = str(row.get("path") or "")
        name = str(row.get("name") or "")
        name_keys = _filename_lookup_keys(name)
        path_keys = row.get("_filename_lookup_keys") or (
            set(_filename_lookup_keys(name)) | set(_filename_lookup_keys(path))
        )
        best = 0.0
        for anchor in anchors:
            if anchor in name_keys:
                best = max(best, 20000.0 + len(anchor))
            elif any(anchor in key or key in anchor for key in name_keys if len(key) >= 3):
                best = max(best, 18000.0 + len(anchor))
            elif anchor in path_keys:
                best = max(best, 14000.0 + len(anchor))
            elif any(anchor in key or key in anchor for key in path_keys if len(key) >= 3):
                best = max(best, 12000.0 + len(anchor))
        return best

    def append_expanded_seed(seed: dict) -> None:
        append_result(seed)
        path = str(seed.get("path") or "")
        seed_row = row_by_path.get(path, {})
        seed_card = seed_row.get("_map_card") or {}
        group = str(seed_card.get("duplicate_group_id") or seed.get("duplicate_group_id") or "")
        family = _family_bucket(path)
        expansion_rows = []
        if group:
            expansion_rows.extend(rows_by_group.get(group, []))
        expansion_rows.extend(rows_by_family.get(family, []))
        for row in sorted(expansion_rows, key=lambda r: str(r.get("path") or "")):
            append_result(row_to_result(row, seed))
            if len(out) >= top_k:
                break

    direct_seeds: list[dict] = []
    for row in index.rows:
        score = anchor_score(row)
        if score <= 0:
            continue
        card = row.get("_map_card") or {}
        direct_seeds.append({
            "path": row.get("path"),
            "name": row.get("name"),
            "score": round(score, 3),
            "reasons": ["duplicate-anchor"],
            "matched_terms": anchors,
            "matched_intents": [],
            "duplicate_group_id": card.get("duplicate_group_id", ""),
            "evidence": list(card.get("evidence_previews") or [])[:3],
        })

    sorted_direct_seeds = sorted(
        direct_seeds,
        key=lambda item: (-float(item.get("score") or 0.0), str(item.get("path") or "")),
    )

    # First return all rows whose filename/path directly matches the remembered
    # anchor. Only then fill remaining slots with duplicate-group/family
    # neighbors. This prevents a broad same-content duplicate group from
    # burying the second exact filename match below hit@5.
    for seed in sorted_direct_seeds:
        append_result(seed)
        if len(out) >= top_k:
            break

    for seed in sorted_direct_seeds:
        if len(out) >= top_k:
            break
        append_expanded_seed(seed)
        if len(out) >= top_k:
            break

    for seed in ranked[: max(top_k, 8)]:
        if len(out) >= top_k:
            break
        append_expanded_seed(seed)
    return out[:top_k]


def search_with_index(index: SearchIndex, query: str, *, top_k: int = 10) -> list[dict]:
    ranked: list[dict] = []
    quoted_terms = set(_quoted_query_terms(query))
    for idx in _candidate_row_ids(index, query):
        row = index.rows[idx]
        score, reasons = _score(query, row, idf=index.idf)
        map_score, map_reasons, matched_terms, matched_intents = _score_map(query, row, idf=index.idf)
        if row.get("_map_card"):
            # When Jikji map cards exist, the map is the primary product. Keep
            # a small legacy lexical contribution for filename/path ergonomics,
            # but avoid letting generic full-text overlap drown map evidence.
            legacy_weight = 0.05 if quoted_terms else 0.22
            score = map_score + legacy_weight * score
        else:
            score += map_score
        reasons.extend(reason for reason in map_reasons if reason not in reasons)
        if score <= 0:
            continue
        card = row.get("_map_card") or {}
        evidence = list(card.get("evidence_previews") or [])
        if row.get("_map_chunks"):
            for chunk in row.get("_map_chunks") or []:
                preview = str(chunk.get("preview") or "")
                if preview and any(term in preview.casefold() for term in matched_terms):
                    evidence.insert(0, preview)
                    break
        ranked.append({
            "path": row.get("path"),
            "name": row.get("name"),
            "score": round(score, 3),
            "reasons": reasons,
            "matched_terms": matched_terms,
            "matched_intents": matched_intents,
            "duplicate_group_id": card.get("duplicate_group_id", ""),
            "evidence": evidence[:3],
        })
    ranked.sort(key=lambda item: (-float(item["score"]), str(item.get("path") or "")))
    ranked = _expand_duplicate_ranked(index, ranked, query, top_k=top_k)
    return _diversify_ranked(ranked, query, top_k=top_k)


def _instant_search_index(root: Path, query: str, *, top_k: int = 10) -> SearchIndex | None:
    path = instant_index_path(root)
    if not path.exists():
        return None
    needles: set[str] = set(_query_tokens(query, limit=48, expand=True))
    needles.update(_quoted_query_terms(query))
    filename_anchors = _query_filename_anchors(query)
    for term in list(needles):
        needles.update(t.casefold() for t in _tokenize(term, limit=8))
        needles.update(_filename_lookup_keys(term))
    needles.update(intent.casefold() for intent in _query_intents(query))
    needles.update(ext.lstrip(".") for ext in _query_formats(query))
    needles = {needle.casefold() for needle in needles if needle}
    # Everything-style instant mode should keep the candidate window tight.
    # JSONL fallback still exists for diagnostic full-map scoring; the SQLite
    # path optimizes everyday agent lookup latency.
    cap = max(800, min(2000, top_k * 100))

    try:
        con = sqlite3.connect(str(path))
        con.row_factory = sqlite3.Row
        try:
            schema = con.execute("SELECT value FROM meta WHERE key='schema_version'").fetchone()
            if not schema or int(schema["value"]) != INSTANT_SEARCH_SCHEMA_VERSION:
                return None
            direct_ids: list[int] = []
            if filename_anchors and (_is_filename_query(query) or _is_duplicate_query(query)):
                for anchor in filename_anchors:
                    direct_ids.extend(
                        int(row["doc_id"])
                        for row in con.execute(
                            "SELECT doc_id FROM filename_keys WHERE key=? ORDER BY doc_id LIMIT ?",
                            (anchor, cap),
                        )
                    )
                if not direct_ids:
                    for anchor in filename_anchors:
                        if len(anchor) < 3:
                            continue
                        direct_ids.extend(
                            int(row["doc_id"])
                            for row in con.execute(
                                "SELECT doc_id FROM filename_keys WHERE key LIKE ? ORDER BY doc_id LIMIT ?",
                                (f"%{anchor}%", cap),
                            )
                        )
            ranked_ids: list[int] = []
            if needles:
                placeholders = ",".join("?" for _ in needles)
                ranked_ids = [
                    int(row["doc_id"])
                    for row in con.execute(
                        f"SELECT doc_id, COUNT(*) AS c FROM terms WHERE term IN ({placeholders}) "
                        "GROUP BY doc_id ORDER BY c DESC, doc_id LIMIT ?",
                        (*sorted(needles), cap),
                    )
                ]
            ids: list[int] = []
            seen: set[int] = set()
            for doc_id in direct_ids + ranked_ids:
                if doc_id not in seen:
                    seen.add(doc_id)
                    ids.append(doc_id)
            if not ids:
                return None
            placeholders = ",".join("?" for _ in ids)
            rows_by_id = {
                int(row["id"]): json.loads(str(row["row_json"]))
                for row in con.execute(f"SELECT id,row_json FROM docs WHERE id IN ({placeholders})", ids)
            }
            rows = [rows_by_id[doc_id] for doc_id in ids if doc_id in rows_by_id]
            if not rows:
                return None
            idf: dict[str, float] = {}
            idf_terms = sorted(needles)
            if idf_terms:
                placeholders = ",".join("?" for _ in idf_terms)
                idf = {
                    str(row["term"]): float(row["value"])
                    for row in con.execute(f"SELECT term,value FROM idf WHERE term IN ({placeholders})", idf_terms)
                }
            return SearchIndex(root=Path(root).expanduser().resolve(), rows=rows, idf=idf, map_backed=True, inverted=None)
        finally:
            con.close()
    except (OSError, sqlite3.Error, json.JSONDecodeError, ValueError):
        return None


def search(root: Path, query: str, *, top_k: int = 10) -> list[dict]:
    instant = _instant_search_index(root, query, top_k=top_k)
    if instant is not None:
        return search_with_index(instant, query, top_k=top_k)
    return search_with_index(build_search_index(root), query, top_k=top_k)



def _metrics_from_details(cases: list[dict], details: list[dict], *, top_k: int) -> dict[str, Any]:
    hits_at: Counter[int] = Counter()
    hash_hits_at: Counter[int] = Counter()
    duplicate_hits_at: Counter[int] = Counter()
    reciprocal_sum = 0.0
    recall_sums: Counter[int] = Counter()
    precision_sums: Counter[int] = Counter()
    by_scenario: dict[str, list[dict]] = defaultdict(list)
    for detail in details:
        rank = detail["rank"]
        if rank is not None:
            reciprocal_sum += 1.0 / rank
            for k in (1, 3, 5, 10):
                if rank <= k:
                    hits_at[k] += 1
        hash_rank = detail.get("hash_rank")
        duplicate_rank = detail.get("duplicate_rank")
        for k in (1, 3, 5, 10):
            if hash_rank is not None and hash_rank <= k:
                hash_hits_at[k] += 1
            if duplicate_rank is not None and duplicate_rank <= k:
                duplicate_hits_at[k] += 1
        expected = set(str(p) for p in (detail.get("expected_paths") or []))
        ranked_paths = [str(item.get("path") or "") for item in (detail.get("top_results") or [])]
        if expected:
            for k in (1, 3, 5, 10):
                top = ranked_paths[:k]
                overlap = len(expected.intersection(top))
                recall_sums[k] += overlap / min(k, len(expected))
                precision_sums[k] += overlap / k
        by_scenario[str(detail.get("scenario") or "unknown")].append(detail)
    total = len(cases)

    def ratio(n: int) -> float:
        return round(n / total, 4) if total else 0.0

    scenario_metrics = {}
    for scenario, items in sorted(by_scenario.items()):
        n = len(items)
        def scenario_recall(k: int, *, scenario_items=items, scenario_n=n) -> float:
            total_recall = 0.0
            for item in scenario_items:
                expected = set(str(p) for p in (item.get("expected_paths") or []))
                if not expected:
                    continue
                ranked_paths = [str(r.get("path") or "") for r in (item.get("top_results") or [])[:k]]
                total_recall += len(expected.intersection(ranked_paths)) / min(k, len(expected))
            return round(total_recall / scenario_n, 4)

        scenario_metrics[scenario] = {
            "cases": n,
            "hit_at_1": round(sum(1 for item in items if item["rank"] == 1) / n, 4),
            "hit_at_5": round(sum(1 for item in items if item["rank"] is not None and item["rank"] <= 5) / n, 4),
            "hit_at_10": round(sum(1 for item in items if item["rank"] is not None and item["rank"] <= 10) / n, 4),
            "set_recall_at_5": scenario_recall(5),
            "set_recall_at_10": scenario_recall(10),
            "hash_or_exact_hit_at_10": round(sum(1 for item in items if item.get("hash_rank") is not None and item["hash_rank"] <= 10) / n, 4),
            "duplicate_or_exact_hit_at_10": round(sum(1 for item in items if item.get("duplicate_rank") is not None and item["duplicate_rank"] <= 10) / n, 4),
            "mrr": round(sum((1.0 / item["rank"]) for item in items if item["rank"]) / n, 4),
        }
    return {
        "cases": total,
        "top_k": top_k,
        "hit_at_1": ratio(hits_at[1]),
        "hit_at_3": ratio(hits_at[3]),
        "hit_at_5": ratio(hits_at[5]),
        "hit_at_10": ratio(hits_at[10]),
        "set_recall_at_5": round(recall_sums[5] / total, 4) if total else 0.0,
        "set_recall_at_10": round(recall_sums[10] / total, 4) if total else 0.0,
        "set_precision_at_5": round(precision_sums[5] / total, 4) if total else 0.0,
        "set_precision_at_10": round(precision_sums[10] / total, 4) if total else 0.0,
        "hash_or_exact_hit_at_5": ratio(hash_hits_at[5]),
        "hash_or_exact_hit_at_10": ratio(hash_hits_at[10]),
        "duplicate_or_exact_hit_at_5": ratio(duplicate_hits_at[5]),
        "duplicate_or_exact_hit_at_10": ratio(duplicate_hits_at[10]),
        "mrr": round(reciprocal_sum / total, 4) if total else 0.0,
        "by_scenario": scenario_metrics,
    }


def _card_text(card: dict) -> str:
    return " ".join([
        str(card.get("path") or ""),
        str(card.get("name") or ""),
        " ".join(str(x) for x in (card.get("content_terms") or [])),
        " ".join(str(x) for x in (card.get("rare_terms") or [])),
        " ".join(str(x) for x in (card.get("phrase_signatures") or [])),
        " ".join(str(x) for x in (card.get("intent_tags") or [])),
        " ".join(str(x) for x in (card.get("evidence_previews") or [])),
        str(card.get("summary") or ""),
    ]).casefold()


def _analysis_category(
    case: dict,
    *,
    rank: int | None,
    duplicate_rank: int | None,
    expected_cards: list[dict],
    top_results: list[dict],
) -> str:
    query = str(case.get("query") or "")
    scenario = str(case.get("scenario") or "")
    quoted_terms = _quoted_query_terms(query)
    query_tokens = _query_tokens(query, limit=16, expand=False)

    if not expected_cards:
        return "expected_missing_from_map"
    if any(card.get("parse_status") not in {"", "success", "archive_listing"} and card.get("is_document") for card in expected_cards):
        return "expected_parse_failed"
    if scenario == "task_intent_query" or (not quoted_terms and len(query_tokens) <= 5 and _query_intents(query)):
        return "ambiguous_intent_only"
    if duplicate_rank is not None and duplicate_rank <= 5 and (rank is None or rank > 5):
        return "duplicate_or_near_duplicate_hit"
    if quoted_terms:
        expected_text = " ".join(_card_text(card) for card in expected_cards)
        missing = [term for term in quoted_terms if term not in expected_text]
        if missing:
            return "quoted_terms_missing_expected_map"
        top_all_quoted = sum(
            1
            for item in top_results[:20]
            if all(term in " ".join(str(x) for x in (item.get("matched_terms") or [])).casefold() for term in quoted_terms)
        )
        if top_all_quoted >= 8 and (rank is None or rank > 5):
            return "highly_ambiguous_shared_clues"
    if rank is not None and rank <= 5:
        return "hit_at_5"
    if rank is not None:
        return "top50_but_not_top5"
    return "not_in_top50"


def analyze_eval_failures(root: Path, *, eval_set: Path | None = None, top_k: int = 50) -> BenchAnalysisResult:
    """Analyze Jikji map-only failures and answerability without external models."""
    root = Path(root).expanduser().resolve()
    eval_set_path = eval_set or (root / EVAL_DIR / EVAL_SET_NAME)
    cases = _read_jsonl(eval_set_path)
    if not cases:
        raise FileNotFoundError(f"No eval set found: {eval_set_path}")

    fingerprints = _path_fingerprints(root)
    index = build_search_index(root)
    cards_by_path = {str(row.get("path") or ""): row for row in _read_jsonl(root / ".jikji" / "file_cards.jsonl")}
    details: list[dict] = []
    category_counts: Counter[str] = Counter()
    non_task_intent_details: list[dict] = []
    non_task_intent_cases: list[dict] = []

    for case in cases:
        ranked = search_with_index(index, str(case.get("query") or ""), top_k=top_k)
        expected = set(str(p) for p in (case.get("expected_paths") or []))
        rank = _rank_for_expected(ranked, expected, fingerprints, mode="exact")
        hash_rank = _rank_for_expected(ranked, expected, fingerprints, mode="hash")
        duplicate_rank = _rank_for_expected(ranked, expected, fingerprints, mode="duplicate")
        expected_cards = [cards_by_path[p] for p in expected if p in cards_by_path]
        category = _analysis_category(
            case,
            rank=rank,
            duplicate_rank=duplicate_rank,
            expected_cards=expected_cards,
            top_results=ranked,
        )
        category_counts[category] += 1
        detail = {
            "id": case.get("id"),
            "scenario": case.get("scenario"),
            "query": case.get("query"),
            "expected_paths": sorted(expected),
            "rank": rank,
            "hash_rank": hash_rank,
            "duplicate_rank": duplicate_rank,
            "category": category,
            "quoted_terms": _quoted_query_terms(str(case.get("query") or "")),
            "query_intents": _query_intents(str(case.get("query") or "")),
            "query_formats": sorted(_query_formats(str(case.get("query") or ""))),
            "query_document_types": sorted(_query_document_types(str(case.get("query") or ""))),
            "top_results": ranked[:10],
        }
        details.append(detail)
        if category != "ambiguous_intent_only":
            non_task_intent_cases.append(case)
            non_task_intent_details.append(detail)

    metrics = _metrics_from_details(cases, details, top_k=top_k)
    non_task_intent_metrics = (
        _metrics_from_details(non_task_intent_cases, non_task_intent_details, top_k=top_k)
        if non_task_intent_cases
        else {}
    )
    summary = {
        "cases": len(cases),
        "top_k": top_k,
        "categories": dict(category_counts.most_common()),
        "metrics": metrics,
        "metrics_excluding_task_intent_query": non_task_intent_metrics,
    }
    out = root / EVAL_DIR / "bench_analysis.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    _write_json(out, {
        "root": str(root),
        "eval_set": str(eval_set_path),
        "summary": summary,
        "details": details,
    })
    return BenchAnalysisResult(out, len(cases), summary)


def run_eval(root: Path, *, eval_set: Path | None = None, top_k: int = 5) -> EvalResult:
    root = Path(root).expanduser().resolve()
    eval_set_path = eval_set or (root / EVAL_DIR / EVAL_SET_NAME)
    cases = _read_jsonl(eval_set_path)
    if not cases:
        raise FileNotFoundError(f"No eval set found: {eval_set_path}. Run `jikji eval-generate ROOT` first.")

    fingerprints = _path_fingerprints(root)
    search_index = build_search_index(root)
    details: list[dict] = []
    for case in cases:
        ranked = search_with_index(search_index, str(case.get("query") or ""), top_k=top_k)
        expected = set(str(p) for p in (case.get("expected_paths") or []))
        rank = _rank_for_expected(ranked, expected, fingerprints, mode="exact")
        hash_rank = _rank_for_expected(ranked, expected, fingerprints, mode="hash")
        duplicate_rank = _rank_for_expected(ranked, expected, fingerprints, mode="duplicate")
        details.append({
            "id": case.get("id"),
            "scenario": case.get("scenario"),
            "query": case.get("query"),
            "expected_paths": sorted(expected),
            "rank": rank,
            "hash_rank": hash_rank,
            "duplicate_rank": duplicate_rank,
            "top_results": ranked,
        })

    metrics = _metrics_from_details(cases, details, top_k=top_k)
    report = {"root": str(root), "eval_set": str(eval_set_path), "metrics": metrics, "details": details}
    report_path = root / EVAL_DIR / EVAL_REPORT_NAME
    _write_json(report_path, report)
    return EvalResult(eval_set_path=eval_set_path, report_path=report_path, cases=len(cases), scenarios={k: v["cases"] for k, v in metrics["by_scenario"].items()}, metrics=metrics)
