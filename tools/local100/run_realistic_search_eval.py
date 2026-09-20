#!/usr/bin/env python3
"""Build ~100 realistic local search scenarios and compare Jikji vs find/grep."""

from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import subprocess
import time
from collections import Counter
from pathlib import Path
from typing import Any

ROOTS = {
    274: Path("/home/cheol/projects/jikji"),
    591: Path("/home/cheol/Documents"),
    593: Path("/home/cheol/Downloads"),
    594: Path("/home/cheol/GoogleDrive"),
}
ALLOWED_EXT = {
    ".hwp",
    ".hwpx",
    ".pdf",
    ".rs",
    ".py",
    ".md",
    ".txt",
    ".docx",
    ".pptx",
    ".toml",
}
SKIP_DIR = {
    "target",
    ".venv",
    "node_modules",
    "__pycache__",
    ".git",
    ".jikji",
    ".cargo",
    ".munocode",
    ".omx",
    ".omo",
    "site-packages",
    "dist-packages",
    "flaticon",
    "effect_constructor",
    ".tox",
    "vendor",
    "egg-info",
}
SKIP_NAME = {
    "000_JIKJI_AGENT_MAP.md",
    ".jikji_agent_map.md",
}
PII_RE = re.compile(
    r"급여|명세서|주민|생년월일|비밀번호|암호|연봉|통장|세금계산|개인정보|여권|주민등록|증명서|기본증명|등본|동의서|이력서|사업자등록번호|전화번호"
)
TOKEN_RE = re.compile(r"[A-Za-z][A-Za-z0-9_+-]{2,}|[가-힣]{2,}|[0-9]{4,}")
DATE_RE = re.compile(
    r"^(?:20)?\d{6}$|^\d{8}$|^\d{4}[-_.]?\d{2}(?:[-_.]?\d{2})?$|^fy\d{2,4}$",
    re.I,
)
HEX_RE = re.compile(r"^[0-9a-f]{8,}$", re.I)
NOISE_LINE_RE = re.compile(
    r"^(?:#|MEMO/|memo\d*$|kyand$|65535$|Root Entry|FileHeader|\d+$)",
    re.I,
)
STOP = {
    "file",
    "files",
    "folder",
    "document",
    "문서",
    "파일",
    "폴더",
    "관련",
    "내용",
    "the",
    "and",
    "for",
    "with",
    "from",
    "this",
    "that",
    "src",
    "lib",
    "mod",
    "test",
    "tests",
    "copy",
    "final",
    "draft",
    "version",
    "home",
    "cheol",
    "downloads",
    "documents",
    "googledrive",
    "projects",
    "jikji",
    "backup",
    "백업",
    "받은",
    "노트북백업",
    "과거백업",
    "다운로드",
}
SOURCE_GENERIC = {
    "struct",
    "enum",
    "impl",
    "trait",
    "collections",
    "process",
    "signal",
    "support",
    "default",
    "option",
    "result",
    "string",
    "pathbuf",
    "serde_json",
    "bufread",
    "errorkind",
    "systemtime",
    "component",
    "fixture",
    "contracts",
    "helpers",
    "storage",
    "routing",
    "registry",
    "mod",
}
EXT_QUERY = {
    ".hwp": "한글 문서",
    ".hwpx": "한글 문서",
    ".pdf": "pdf",
    ".rs": "rust 소스",
    ".py": "python 소스",
    ".md": "markdown",
    ".txt": "텍스트",
    ".docx": "워드",
    ".pptx": "발표자료",
    ".toml": "toml",
}
TEXT_EXT = {".rs", ".py", ".md", ".txt", ".toml", ".json", ".html"}
ROOT_QUOTA = {
    "/home/cheol/Documents": 10,
    "/home/cheol/projects/jikji": 36,
    "/home/cheol/Downloads": 28,
    "/home/cheol/GoogleDrive": 26,
}
EXT_QUOTA = {
    ".hwp": 18,
    ".hwpx": 12,
    ".pdf": 16,
    ".rs": 14,
    ".py": 10,
    ".md": 8,
    ".docx": 8,
    ".pptx": 8,
    ".toml": 3,
    ".txt": 3,
}
SCENARIO_ROOT_QUOTA = {
    "/home/cheol/Documents": 2,
    "/home/cheol/projects/jikji": 9,
    "/home/cheol/Downloads": 7,
    "/home/cheol/GoogleDrive": 7,
}
SCENARIO_EXT_QUOTA = {
    ".hwp": 5,
    ".hwpx": 4,
    ".pdf": 4,
    ".rs": 4,
    ".py": 3,
    ".md": 2,
    ".docx": 2,
    ".pptx": 2,
    ".toml": 1,
    ".txt": 1,
}
PHONE_RE = re.compile(r"010[-.\s]?\d{3,4}[-.\s]?\d{4}")
OCR_JUNK_RE = re.compile(r"^[A-Za-z0-9]{1,4}[-._+][A-Za-z0-9._+-]{1,10}$")
ADDR_RE = re.compile(r"(시|구|군|읍|면|동|로|길|번지)$")
SHA_MAPS: dict[int, dict[str, str]] = {}


def default_out_dir() -> Path:
    return Path("/home/cheol/projects/jikji/.jikji/eval/local100")


def db_path() -> Path:
    return Path.home() / ".local/share/jikji/index.sqlite"


def tokens(text: str) -> list[str]:
    seen: set[str] = set()
    out: list[str] = []
    for raw in TOKEN_RE.findall(text or ""):
        tok = raw.strip("_-+")
        key = tok.casefold()
        if len(key) < 2 or key in STOP or key in seen:
            continue
        seen.add(key)
        out.append(tok)
    return out


def skip_path(rel: str) -> bool:
    parts = Path(rel).parts
    if parts and re.fullmatch(r"tent\.\d+", parts[0] or ""):
        return True
    if any(part == "#OLD" for part in parts):
        return True
    if Path(rel).name in SKIP_NAME:
        return True
    if PII_RE.search(rel):
        return True
    return False


def weak_token(tok: str, df: Counter[str] | None = None, *, body: bool = False) -> bool:
    key = tok.casefold().strip()
    if not key or key in STOP or len(key) < 3:
        return True
    if tok.isdigit() or DATE_RE.fullmatch(tok) or re.search(r"\d{6,}", tok):
        return True
    if HEX_RE.fullmatch(tok) or OCR_JUNK_RE.fullmatch(tok):
        return True
    if PHONE_RE.search(tok) or PII_RE.search(tok):
        return True
    hangul = all("가" <= ch <= "힣" for ch in tok)
    if body and hangul and len(tok) <= 2:
        return True
    if key in SOURCE_GENERIC:
        return True
    ascii_ident = all(ch.isascii() and (ch.isalnum() or ch == "_") for ch in tok)
    if ascii_ident and "_" not in tok and len(tok) < 10:
        return True
    if df is not None and df[key] > 40:
        return True
    return False


def dedupe_words(query: str) -> str:
    seen: set[str] = set()
    out: list[str] = []
    for word in query.split():
        key = word.casefold()
        if key in seen:
            continue
        seen.add(key)
        out.append(word)
    return " ".join(out)
def load_docs(con: sqlite3.Connection) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for root_id, root in ROOTS.items():
        cur = con.execute(
            """
            SELECT id, path, name, ext
            FROM search_docs
            WHERE root_id = ?
            """,
            (root_id,),
        )
        for doc_id, path, name, ext in cur:
            rel = str(path or "").replace("\\", "/")
            ext = str(ext or "").lower()
            if ext not in ALLOWED_EXT or skip_path(rel):
                continue
            rows.append(
                {
                    "root_id": root_id,
                    "root": str(root),
                    "doc_id": int(doc_id),
                    "path": rel,
                    "name": str(name or Path(rel).name),
                    "ext": ext,
                    "folder": str(Path(rel).parent).replace("\\", "/"),
                }
            )
    return rows


def distinctive_token(row: dict[str, Any], df: Counter[str]) -> str | None:
    cands = []
    for tok in tokens(row["name"]) + tokens(Path(row["path"]).stem):
        if weak_token(tok, df):
            continue
        key = tok.casefold()
        cands.append((df[key], -len(tok), tok))
    if not cands:
        return None
    cands.sort()
    return cands[0][2]


def folder_label(folder: str) -> str | None:
    if folder in {"", "."}:
        return None
    parts = [part for part in Path(folder).parts if part not in SKIP_DIR]
    useful = []
    for part in parts:
        cleaned = part.replace("_", " ").replace("-", " ")
        if weak_token(cleaned.replace(" ", ""), body=False) and not tokens(cleaned):
            continue
        label_tokens = [tok for tok in tokens(cleaned) if not weak_token(tok)]
        if not label_tokens:
            continue
        useful.append(" ".join(label_tokens[:3]))
    if not useful:
        return None
    if len(useful) >= 2:
        return f"{useful[-2]} {useful[-1]}"
    return useful[-1]


def sha_map(root_id: int) -> dict[str, str]:
    cached = SHA_MAPS.get(root_id)
    if cached is not None:
        return cached
    path = Path.home() / f".local/share/jikji/roots/{root_id}/file_cards.jsonl"
    out: dict[str, str] = {}
    if path.exists():
        with path.open(encoding="utf-8") as handle:
            for line in handle:
                if not line.strip():
                    continue
                obj = json.loads(line)
                rel = str(obj.get("path") or "").replace("\\", "/")
                sha = str(obj.get("sha256") or "")
                if rel and sha:
                    out[rel] = sha
    SHA_MAPS[root_id] = out
    return out


def is_fuse_root(root: str) -> bool:
    return "GoogleDrive" in root or "Google Drive" in root


def read_body_text(row: dict[str, Any], limit: int = 12_000) -> str:
    if row["ext"] in TEXT_EXT and not is_fuse_root(row["root"]):
        source = Path(row["root"]) / row["path"]
        try:
            if source.exists() and source.stat().st_size <= 512_000:
                return source.read_text(encoding="utf-8", errors="ignore")[:limit]
        except OSError:
            pass
    sha = sha_map(row["root_id"]).get(row["path"], "")
    if not sha:
        return ""
    cache = Path.home() / f".local/share/jikji/roots/{row['root_id']}/doc_text/sha256_{sha}.txt"
    chunk_dir = cache.with_suffix("")
    try:
        if cache.exists():
            return cache.read_text(encoding="utf-8", errors="ignore")[:limit]
        if chunk_dir.is_dir():
            parts = []
            for child in sorted(chunk_dir.glob("*.txt"))[:4]:
                parts.append(child.read_text(encoding="utf-8", errors="ignore"))
                if sum(len(part) for part in parts) >= limit:
                    break
            if parts:
                return "".join(parts)[:limit]
    except OSError:
        pass
    return ""


def body_terms(con: sqlite3.Connection, row: dict[str, Any]) -> list[str]:
    cur = con.execute(
        """
        SELECT term
        FROM search_field_terms
        WHERE root_id = ? AND doc_id = ? AND field = 'body'
          AND length(term) >= 5
        ORDER BY tf DESC
        LIMIT 40
        """,
        (row["root_id"], row["doc_id"]),
    )
    name_fold = row["name"].casefold()
    path_fold = row["path"].casefold()
    out: list[str] = []
    for (term,) in cur:
        tok = str(term)
        if weak_token(tok, body=True):
            continue
        if tok.casefold() in name_fold or tok in path_fold:
            continue
        if PII_RE.search(tok):
            continue
        out.append(tok)
        if len(out) >= 4:
            break
    return out


def content_query(con: sqlite3.Connection, row: dict[str, Any]) -> str | None:
    text = read_body_text(row)
    if not text:
        terms = body_terms(con, row)
        joined = " ".join(terms[:2]).strip()
        return joined or None
    name_fold = row["name"].casefold()
    path_fold = row["path"].casefold().replace("/", "")
    lines = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or NOISE_LINE_RE.match(line):
            continue
        if PII_RE.search(line):
            continue
        lines.append(line)
    blob = "\n".join(lines[:80])
    phrases: list[str] = []
    if row["ext"] in {".rs", ".py"}:
        for match in re.finditer(
            r"\b(?:fn|struct|enum|mod|class|def)\s+([A-Za-z_][A-Za-z0-9_]{7,})",
            blob,
        ):
            ident = match.group(1)
            if ident.casefold() in name_fold or weak_token(ident, body=True):
                continue
            phrases.append(ident)
            if len(phrases) >= 6:
                break
    if not phrases:
        for match in re.finditer(r"[가-힣]{2,10}(?:\s+[가-힣]{2,10}){1,2}", blob):
            phrase = re.sub(r"\s+", " ", match.group()).strip()
            compact = phrase.replace(" ", "").casefold()
            if compact in name_fold.replace(" ", "") or compact in path_fold:
                continue
            if any(weak_token(part, body=True) for part in phrase.split()):
                continue
            phrases.append(phrase)
            if len(phrases) >= 8:
                break
    if not phrases:
        for match in re.finditer(r"[가-힣]{5,14}|[A-Za-z][A-Za-z0-9_]{7,24}", blob):
            phrase = match.group()
            if phrase.casefold() in name_fold or phrase.casefold() in path_fold:
                continue
            if weak_token(phrase, body=True):
                continue
            phrases.append(phrase)
            if len(phrases) >= 6:
                break
    office = row["ext"] in {".hwp", ".hwpx", ".pdf", ".docx", ".pptx"}
    for phrase in phrases:
        if PHONE_RE.search(phrase) or OCR_JUNK_RE.fullmatch(phrase):
            continue
        if office and not re.search(r"[가-힣]{3,}", phrase):
            continue
        parts = phrase.split()
        if parts and all(ADDR_RE.search(part) or part.isdigit() for part in parts):
            continue
        return phrase
    joined = " ".join(body_terms(con, row)[:2]).strip()
    if joined and (not office or re.search(r"[가-힣]{3,}", joined)):
        return joined
    return None


def add_case(
    cases: list[dict[str, Any]],
    used: set[tuple[str, str, str]],
    counts: Counter[str],
    root_counts: Counter[str],
    ext_counts: Counter[str],
    folder_counts: Counter[tuple[str, str]],
    file_counts: Counter[tuple[str, str]],
    *,
    scenario: str,
    query: str,
    row: dict[str, Any],
    evidence: str,
    cap: int,
    relax: bool = False,
    relax_ext: bool = False,
) -> bool:
    query = dedupe_words(re.sub(r"\s+", " ", query).strip())
    if not query or counts[scenario] >= cap:
        return False
    key = (scenario, row["root"], row["path"])
    qkey = (scenario, row["root"], query.casefold())
    if key in used or qkey in used:
        return False
    if file_counts[(row["root"], row["path"])] >= 2:
        return False
    if not relax:
        root_cap = SCENARIO_ROOT_QUOTA.get(row["root"], ROOT_QUOTA.get(row["root"], 40))
        ext_cap = SCENARIO_EXT_QUOTA.get(row["ext"], EXT_QUOTA.get(row["ext"], 20))
        if root_counts[(scenario, row["root"])] >= root_cap:
            return False
        if not relax_ext and ext_counts[(scenario, row["ext"])] >= ext_cap:
            return False
        if folder_counts[(scenario, row["root"], row["folder"])] >= 3:
            return False
    used.add(key)
    used.add(qkey)
    counts[scenario] += 1
    root_counts[(scenario, row["root"])] += 1
    ext_counts[(scenario, row["ext"])] += 1
    folder_counts[(scenario, row["root"], row["folder"])] += 1
    file_counts[(row["root"], row["path"])] += 1
    cases.append(
        {
            "id": f"{scenario}-{counts[scenario]:03d}",
            "scenario": scenario,
            "query": query,
            "root": row["root"],
            "expected_paths": [row["path"]],
            "ext": row["ext"],
            "evidence": evidence[:300],
        }
    )
    return True


def make_query(
    scenario: str,
    row: dict[str, Any],
    df: Counter[str],
    con: sqlite3.Connection,
) -> str | None:
    tok = distinctive_token(row, df)
    if scenario == "filename":
        if not tok:
            return None
        if df[tok.casefold()] <= 8:
            return tok
        extras = [item for item in tokens(row["name"]) if item.casefold() != tok.casefold() and not weak_token(item, df)]
        if extras:
            return f"{tok} {extras[0]}"
        stem = Path(row["name"]).stem.replace("_", " ").replace("-", " ")
        return stem if not DATE_RE.fullmatch(stem.replace(" ", "")) else None
    if scenario == "folder":
        folder = folder_label(row["folder"])
        if not tok or not folder:
            return None
        return f"{folder} {tok}"
    if scenario == "extension":
        if not tok:
            return None
        return f"{tok} {EXT_QUERY.get(row['ext'], row['ext'].lstrip('.'))}"
    if scenario == "content":
        return content_query(con, row)
    return None


def build_scenarios(con: sqlite3.Connection, target: int = 100) -> list[dict[str, Any]]:
    docs = load_docs(con)
    df: Counter[str] = Counter()
    folder_size: Counter[tuple[str, str]] = Counter()
    for row in docs:
        folder_size[(row["root"], row["folder"])] += 1
        for tok in tokens(row["name"]):
            df[tok.casefold()] += 1

    def rank_key(row: dict[str, Any]) -> tuple[Any, ...]:
        tok = distinctive_token(row, df)
        root_rank = {
            "/home/cheol/Documents": 0,
            "/home/cheol/GoogleDrive": 1,
            "/home/cheol/Downloads": 2,
            "/home/cheol/projects/jikji": 3,
        }.get(row["root"], 9)
        ext_rank = {
            ".hwpx": 0,
            ".hwp": 1,
            ".pdf": 2,
            ".rs": 3,
            ".py": 4,
            ".md": 5,
            ".docx": 6,
            ".pptx": 7,
            ".toml": 8,
            ".txt": 9,
        }.get(row["ext"], 20)
        backup = 1 if any(mark in row["path"] for mark in ("백업", "카카오톡 받은", "USB백업")) else 0
        return (
            backup,
            root_rank,
            ext_rank,
            folder_size[(row["root"], row["folder"])],
            df[tok.casefold()] if tok else 10_000,
            row["path"],
        )

    ranked = sorted(docs, key=rank_key)
    cases: list[dict[str, Any]] = []
    used: set[tuple[str, str, str]] = set()
    counts: Counter[str] = Counter()
    root_counts: Counter[str] = Counter()
    ext_counts: Counter[str] = Counter()
    folder_counts: Counter[tuple[str, str]] = Counter()
    file_counts: Counter[tuple[str, str]] = Counter()
    per = max(1, target // 4)

    for scenario in ("filename", "folder", "extension", "content"):
        pool = ranked
        if scenario == "content":
            office = {".hwp", ".hwpx", ".pdf", ".docx", ".pptx"}
            pool = sorted(
                ranked,
                key=lambda row: (0 if row["ext"] in office else 1, rank_key(row)),
            )
        for row in pool:
            if counts[scenario] >= per:
                break
            query = make_query(scenario, row, df, con)
            if not query:
                continue
            add_case(
                cases,
                used,
                counts,
                root_counts,
                ext_counts,
                folder_counts,
                file_counts,
                scenario=scenario,
                query=query,
                row=row,
                evidence=scenario,
                cap=per,
                relax_ext=False,
            )

    if len(cases) < target:
        for relax in (False, True):
            for row in ranked:
                if len(cases) >= target:
                    break
                scenario = min(("filename", "folder", "extension", "content"), key=lambda name: counts[name])
                query = make_query(scenario, row, df, con)
                if not query:
                    stem = Path(row["name"]).stem.replace("_", " ").replace("-", " ")
                    if scenario == "folder":
                        query = f"{folder_label(row['folder']) or '문서'} {stem}"
                    elif scenario == "extension":
                        query = f"{stem} {EXT_QUERY.get(row['ext'], row['ext'])}"
                    else:
                        query = stem
                add_case(
                    cases,
                    used,
                    counts,
                    root_counts,
                    ext_counts,
                    folder_counts,
                    file_counts,
                    scenario=scenario,
                    query=query,
                    row=row,
                    evidence="fill to 100",
                    cap=per if not relax else target,
                    relax=relax,
                    relax_ext=True,
                )
            if len(cases) >= target:
                break
    cases.sort(key=lambda item: (item["scenario"], item["id"]))
    for idx, case in enumerate(cases, 1):
        case["id"] = f"{case['scenario']}-{sum(1 for item in cases[:idx] if item['scenario'] == case['scenario']):03d}"
    return cases[:target]


def jikji_bin() -> str:
    debug = Path("/home/cheol/projects/jikji/target/debug/jikji")
    if debug.exists():
        return str(debug)
    return "jikji"


def run_jikji(case: dict[str, Any], top_k: int, timeout: float) -> dict[str, Any]:
    cmd = [
        jikji_bin(),
        "find",
        case["root"],
        case["query"],
        "--json",
        "--top-k",
        str(top_k),
        "--no-background-refresh",
    ]
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        elapsed = time.perf_counter() - t0
        payload = json.loads(proc.stdout) if proc.stdout.strip().startswith("{") else {}
        paths = [str(p) for p in payload.get("paths") or []]
        if not paths:
            paths = [str(item.get("path") or "") for item in payload.get("candidates") or [] if item.get("path")]
        return {
            "ok": proc.returncode == 0,
            "ms": round(elapsed * 1000, 1),
            "paths": paths[:top_k],
            "index_status": payload.get("index_status"),
            "error": None if proc.returncode == 0 else (proc.stderr or proc.stdout)[:500],
        }
    except subprocess.TimeoutExpired:
        return {"ok": False, "ms": round(timeout * 1000, 1), "paths": [], "error": "timeout"}
    except Exception as exc:  # noqa: BLE001 - harness must keep going
        return {"ok": False, "ms": round((time.perf_counter() - t0) * 1000, 1), "paths": [], "error": str(exc)}


def walk_cache(root: Path) -> list[tuple[str, str, str]]:
    rows: list[tuple[str, str, str]] = []
    root_s = str(root)
    for dirpath, dirnames, filenames in os.walk(root_s):
        dirnames[:] = [name for name in dirnames if name not in SKIP_DIR and not name.startswith(".")]
        rel_dir = os.path.relpath(dirpath, root_s)
        for name in filenames:
            rel = name if rel_dir == "." else f"{rel_dir}/{name}"
            if skip_path(rel):
                continue
            ext = Path(name).suffix.lower()
            rows.append((rel.replace("\\", "/"), name, ext))
    return rows


def listing_from_index(root: str) -> list[tuple[str, str, str]]:
    root_id = next((rid for rid, path in ROOTS.items() if str(path) == root), None)
    if root_id is None:
        return []
    con = sqlite3.connect(f"file:{db_path()}?mode=ro", uri=True)
    con.execute("PRAGMA busy_timeout=30000")
    rows: list[tuple[str, str, str]] = []
    cur = con.execute(
        "SELECT path, name, ext FROM search_docs WHERE root_id = ?",
        (root_id,),
    )
    for path, name, ext in cur:
        rel = str(path or "").replace("\\", "/")
        if skip_path(rel):
            continue
        rows.append((rel, str(name or Path(rel).name), str(ext or "").lower()))
    con.close()
    return rows


def load_listing(root: str, cache_dir: Path) -> list[tuple[str, str, str]]:
    cache_dir.mkdir(parents=True, exist_ok=True)
    stamp = cache_dir / (root.replace("/", "_") + ".json")
    if stamp.exists():
        data = json.loads(stamp.read_text(encoding="utf-8"))
        return [(item[0], item[1], item[2]) for item in data]
    rows = listing_from_index(root) if is_fuse_root(root) else walk_cache(Path(root))
    stamp.write_text(json.dumps(rows, ensure_ascii=False), encoding="utf-8")
    return rows


def raw_rank(case: dict[str, Any], listing: list[tuple[str, str, str]], top_k: int) -> dict[str, Any]:
    q_tokens = [tok.casefold() for tok in tokens(case["query"])]
    if not q_tokens:
        q_tokens = [case["query"].casefold()]
    scored: list[tuple[float, str]] = []
    scenario = case["scenario"]
    want_ext = str(case.get("ext") or "").lower()
    t0 = time.perf_counter()
    for rel, name, ext in listing:
        name_l = name.casefold()
        path_l = rel.casefold()
        hits = sum(1 for tok in q_tokens if tok in name_l or tok in path_l)
        if scenario == "filename":
            score = 3 * sum(1 for tok in q_tokens if tok in name_l) + hits
        elif scenario == "folder":
            score = 3 * sum(1 for tok in q_tokens if tok in path_l) + hits
        elif scenario == "extension":
            ext_hit = 4 if want_ext and ext == want_ext else 0
            score = ext_hit + 2 * sum(1 for tok in q_tokens if tok in name_l) + hits
        else:
            score = hits
            if ext in TEXT_EXT and hits and not is_fuse_root(case["root"]):
                abs_path = str(Path(case["root"]) / rel)
                try:
                    if os.path.getsize(abs_path) <= 512_000:
                        text = Path(abs_path).read_text(encoding="utf-8", errors="ignore").casefold()
                        score += 5 * sum(1 for tok in q_tokens if tok in text)
                except OSError:
                    pass
        if score > 0:
            scored.append((score, rel))
    scored.sort(key=lambda item: (-item[0], item[1]))
    return {
        "ok": True,
        "ms": round((time.perf_counter() - t0) * 1000, 1),
        "paths": [rel for _score, rel in scored[:top_k]],
        "error": None,
    }


def rank_of(paths: list[str], expected: list[str]) -> int | None:
    want = {item.replace("\\", "/") for item in expected}
    want_names = {Path(item).name for item in expected}
    for idx, path in enumerate(paths, 1):
        norm = path.replace("\\", "/")
        if norm in want or Path(norm).name in want_names and any(norm.endswith(item) or item.endswith(norm) for item in want):
            return idx
        if Path(norm).name in want_names and sum(1 for item in paths if Path(item).name == Path(norm).name) == 1:
            return idx
    return None


def metrics(details: list[dict[str, Any]]) -> dict[str, Any]:
    n = len(details) or 1

    def hit(k: int) -> float:
        return sum(1 for row in details if row["rank"] is not None and row["rank"] <= k) / n

    lat = [row["ms"] for row in details]
    return {
        "cases": len(details),
        "hit_at_1": round(hit(1), 4),
        "hit_at_5": round(hit(5), 4),
        "hit_at_10": round(hit(10), 4),
        "miss_rate": round(sum(1 for row in details if row["rank"] is None) / n, 4),
        "mean_ms": round(sum(lat) / n, 1),
        "p50_ms": round(sorted(lat)[len(lat) // 2], 1) if lat else 0,
        "p95_ms": round(sorted(lat)[max(0, int(len(lat) * 0.95) - 1)], 1) if lat else 0,
        "timeouts": sum(1 for row in details if row.get("error") == "timeout"),
        "by_scenario": {
            name: {
                "cases": sum(1 for row in details if row["scenario"] == name),
                "hit_at_1": round(
                    sum(1 for row in details if row["scenario"] == name and row["rank"] == 1)
                    / max(1, sum(1 for row in details if row["scenario"] == name)),
                    4,
                ),
                "hit_at_10": round(
                    sum(1 for row in details if row["scenario"] == name and row["rank"] is not None and row["rank"] <= 10)
                    / max(1, sum(1 for row in details if row["scenario"] == name)),
                    4,
                ),
            }
            for name in ("filename", "folder", "extension", "content")
        },
    }


def write_json(path: Path, obj: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(obj, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def write_jsonl(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows), encoding="utf-8")


def load_cases(path: Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, default=default_out_dir())
    parser.add_argument("--target", type=int, default=100)
    parser.add_argument("--top-k", type=int, default=10)
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--skip-raw", action="store_true")
    parser.add_argument("--scenarios-only", action="store_true")
    parser.add_argument("--reuse-scenarios", action="store_true")
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--iteration", type=int, default=0)
    args = parser.parse_args()
    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)
    scenario_path = out / "scenarios.jsonl"

    if args.reuse_scenarios or (args.iteration > 0 and scenario_path.exists()):
        cases = load_cases(scenario_path)
    else:
        con = sqlite3.connect(f"file:{db_path()}?mode=ro", uri=True)
        con.execute("PRAGMA busy_timeout=30000")
        cases = build_scenarios(con, target=args.target)
        con.close()
        write_jsonl(scenario_path, cases)
        write_json(
            out / "scenarios_meta.json",
            {
                "cases": len(cases),
                "by_scenario": dict(Counter(case["scenario"] for case in cases)),
                "by_root": dict(Counter(case["root"] for case in cases)),
                "by_ext": dict(Counter(case["ext"] for case in cases)),
            },
        )
    if args.limit:
        cases = cases[: args.limit]
    if args.scenarios_only:
        print(json.dumps({"cases": len(cases), "out": str(scenario_path)}, ensure_ascii=False))
        return 0

    listings: dict[str, list[tuple[str, str, str]]] = {}
    if not args.skip_raw:
        for root in sorted({case["root"] for case in cases}):
            t0 = time.perf_counter()
            listings[root] = load_listing(root, out / "walk")
            print(f"walk {root}: {len(listings[root])} files in {time.perf_counter()-t0:.1f}s", flush=True)

    jikji_details = []
    raw_details = []
    for idx, case in enumerate(cases, 1):
        jikji = run_jikji(case, args.top_k, args.timeout)
        j_rank = rank_of(jikji["paths"], case["expected_paths"])
        jikji_details.append(
            {
                **{k: case[k] for k in ("id", "scenario", "query", "root", "expected_paths", "ext")},
                "rank": j_rank,
                "ms": jikji["ms"],
                "paths": jikji["paths"][:5],
                "error": jikji.get("error"),
            }
        )
        if not args.skip_raw:
            raw = raw_rank(case, listings[case["root"]], args.top_k)
            raw_details.append(
                {
                    **{k: case[k] for k in ("id", "scenario", "query", "root", "expected_paths", "ext")},
                    "rank": rank_of(raw["paths"], case["expected_paths"]),
                    "ms": raw["ms"],
                    "paths": raw["paths"][:5],
                    "error": raw.get("error"),
                }
            )
        if idx % 10 == 0 or idx == len(cases):
            print(f"jikji {idx}/{len(cases)} last={jikji['ms']}ms rank={j_rank}", flush=True)
        write_jsonl(out / f"jikji_iter{args.iteration:02d}.jsonl", jikji_details)
        if raw_details:
            write_jsonl(out / f"raw_iter{args.iteration:02d}.jsonl", raw_details)

    report = {
        "iteration": args.iteration,
        "jikji": metrics(jikji_details),
        "raw": metrics(raw_details) if raw_details else None,
        "misses": [row for row in jikji_details if row["rank"] is None or row["rank"] > 10],
    }
    write_json(out / f"report_iter{args.iteration:02d}.json", report)
    print(json.dumps({"iteration": args.iteration, "jikji": report["jikji"], "raw": report["raw"], "misses": len(report["misses"])}, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
