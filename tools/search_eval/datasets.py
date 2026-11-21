"""Deterministic synthetic fixtures + ground-truth cases for the Jikji search evaluation.

Two PROFILES are defined on purpose with different vocabulary, token shapes, folder naming,
file formats and query phrasing, so the improved binary can be compared on data it was not
developed against:

* ``general``  - office-style corpus, bare keyword queries (the shape of the failures seen in logs).
* ``holdout``  - lab/legal/recipe corpus, NFD Korean names, code-like token shapes, natural-language
                 wrappers, more punctuation in names. Never tune behaviour on it: run it, record it.

``build_fixture(profile, out_dir, seed)`` writes the tree plus ``cases.json`` (schema documented in
``tools/search_eval/README.md``). Everything is generated from the seed; no real user data.
"""

from __future__ import annotations

import json
import random
import shutil
import unicodedata
import zipfile
from pathlib import Path
from typing import Any

SYL_EN = ["kel", "vor", "in", "tas", "qu", "mer", "dal", "zif", "bro", "nex", "lum", "pry", "ost", "vak", "jun", "wex", "hul", "tor", "fim", "gar"]
SYL_KO = list("가강거고구기나노누다더도두라러로루마머모무바버보부사서소수아어오우자저조주하한")

PROFILES: dict[str, dict[str, Any]] = {
    "general": {
        "seed": 11,
        "en": ["report", "summary", "meeting", "budget", "review", "draft", "schedule", "notes", "plan", "status", "update", "proposal", "invoice", "contract", "design", "release", "analysis", "project", "team", "quarterly"],
        "ko": ["회의", "보고", "계획", "예산", "검토", "일정", "결과", "정리", "초안", "제안", "분석", "현황"],
        "dirs": ["projects/alpha", "projects/beta", "projects/beta/docs", "contracts", "contracts/2024", "회의록", "회의록/2024년 3월", "재무 보고서", "재무 보고서/결산 (최종)", "archive/old_v1", "100%_done", "a_b"],
        "korean_root": "한글 폴더 (복사본)",
        "large_root": "대형 저장소 (large)",
        "corpus_files": 240,
        "doc_exts": ["txt", "md", "csv", "html", "json", "docx"],
        "token_shape": "word",
        "media_exts": ["jpg", "mp3", "mp4"],
        "nfd": False,
        "wrap": False,
    },
    "holdout": {
        "seed": 97,
        "en": ["protocol", "specimen", "laboratory", "assay", "dosage", "cohort", "patient", "trial", "sample", "lease", "clause", "tenant", "recipe", "harvest", "irrigation", "telescope", "orbit", "survey", "grant", "manuscript"],
        "ko": ["임상", "검체", "판결", "임대차", "수확", "관개", "원고", "관측", "논문", "연구비", "실험", "시료"],
        "dirs": ["Lab Notes/Protocols", "Lab Notes/Protocols/Assays/2023", "Legal/Leases (signed)", "Legal/Cases/CASE-2291", "research papers/drafts", "recipes & menus", "grants/NIH", "survey data/raw", "현장 답사/2022 가을/사진 설명", "tmp.work/stage"],
        "korean_root": "자료 모음 100%_final (v2) 'x'",
        "large_root": "Archive 2019-2024 [mirror]",
        "corpus_files": 200,
        "doc_exts": ["md", "txt", "yaml", "xml", "docx", "rtf", "log"],
        "token_shape": "code",
        "media_exts": ["png", "wav", "mov", "m4a"],
        "nfd": True,
        "wrap": True,
    },
}


class Gen:
    def __init__(self, profile: str, seed: int | None) -> None:
        self.cfg = PROFILES[profile]
        self.profile = profile
        self.rng = random.Random(self.cfg["seed"] if seed is None else seed)
        self.used: set[str] = set()
        self.counter = 0

    # ---- unique tokens -------------------------------------------------
    def _fresh(self, make) -> str:
        while True:
            tok = make()
            if tok.lower() not in self.used and tok.lower() not in {w.lower() for w in self.cfg["en"] + self.cfg["ko"]}:
                self.used.add(tok.lower())
                return tok

    def en(self) -> str:
        if self.cfg["token_shape"] == "code" and self.rng.random() < 0.5:
            return self._fresh(lambda: "".join(self.rng.choices("ABCDEFGHJKLMNPQRSTVWXZ", k=2)) + "-" + str(self.rng.randint(1000, 9999)))
        return self._fresh(lambda: "".join(self.rng.choices(SYL_EN, k=3)))

    def en_plain(self) -> str:
        return self._fresh(lambda: "".join(self.rng.choices(SYL_EN, k=3)))

    def ko(self) -> str:
        n = 4 if self.cfg["token_shape"] == "code" else 3
        return self._fresh(lambda: "".join(self.rng.choices(SYL_KO, k=n)))

    def nfd(self, text: str) -> str:
        return unicodedata.normalize("NFD", text) if self.cfg["nfd"] else text

    # ---- text ------------------------------------------------------------
    def filler(self, words: int = 60, ko_ratio: float = 0.25) -> str:
        out = []
        for _ in range(words):
            pool = self.cfg["ko"] if self.rng.random() < ko_ratio else self.cfg["en"]
            out.append(self.rng.choice(pool))
        return " ".join(out)

    def body(self, extra: str = "", words: int = 60) -> str:
        parts = [self.filler(words // 2), extra, self.filler(words // 2)]
        return " ".join(p for p in parts if p)

    def stamp(self) -> str:
        self.counter += 1
        return f"{self.counter:04d}"


def write_doc(path: Path, ext: str, title: str, body: str, ascii_only: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if ext == "docx":
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
            z.writestr("[Content_Types].xml", '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
            z.writestr("_rels/.rels", '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
            z.writestr("word/document.xml", f'<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{title}</w:t></w:r></w:p><w:p><w:r><w:t>{body}</w:t></w:r></w:p></w:body></w:document>')
        return
    if ext == "rtf":
        safe = "".join(ch if ord(ch) < 128 else " " for ch in f"{title} {body}")
        path.write_text("{\\rtf1\\ansi\\deff0 {\\fonttbl{\\f0 Arial;}} " + safe + "}", encoding="ascii")
        return
    if ext == "csv":
        text = f'id,title,description\n1,{title.replace(",", " ")},"{body}"\n'
    elif ext == "html":
        text = f"<html><head><title>{title}</title></head><body><p>{body}</p></body></html>"
    elif ext == "json":
        text = json.dumps({"title": title, "body": body}, ensure_ascii=False)
    elif ext == "yaml":
        text = f"title: {title}\nbody: {body}\n"
    elif ext == "xml":
        text = f"<doc><title>{title}</title><body>{body}</body></doc>"
    elif ext == "log":
        text = f"INFO {title}\nINFO {body}\n"
    elif ext == "md":
        text = f"# {title}\n\n{body}\n"
    else:
        text = f"{title}\n\n{body}\n"
    path.write_text(text, encoding="utf-8")


def write_code(path: Path, ident: str, lang: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if lang == "py":
        text = f"import requests\n\nclass Cache:\n    def __init__(self):\n        self.cache = {{}}\n\n    def {ident}(self, key):\n        return self.cache[key]  # TODO: handle misses\n"
    else:
        text = f"use std::collections::HashMap;\n\nfn {ident}(map: &HashMap<String, Vec<String>>) -> Option<String> {{\n    map.get(\"k\").map(|v| v.join(\",\"))\n}}\n"
    path.write_text(text, encoding="utf-8")


def build_fixture(profile: str, out: Path, seed: int | None = None, large_files: int = 5000) -> dict[str, Any]:
    g = Gen(profile, seed)
    cfg = g.cfg
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    corpus = out / "corpus"
    cases: list[dict[str, Any]] = []
    mutations: list[dict[str, Any]] = []
    dirs = [g.nfd(d) if cfg["nfd"] and any(ord(c) > 127 for c in d) else d for d in cfg["dirs"]]
    exts = cfg["doc_exts"]

    def add_case(cat: str, root: str, query: str, expect: dict[str, Any], **extra: Any) -> None:
        cases.append({"id": f"{cat}-{len([c for c in cases if c['category'] == cat]) + 1:02d}", "category": cat, "root": root, "query": query, "top_k": extra.pop("top_k", 20), "expect": expect, **extra})

    def place(rel_dir: str, name: str, ext: str, title: str, body: str) -> str:
        rel = f"{rel_dir}/{name}.{ext}" if rel_dir else f"{name}.{ext}"
        write_doc(corpus / rel, ext, title, body)
        return rel

    # ---- filler ----------------------------------------------------------
    for i in range(cfg["corpus_files"]):
        d = dirs[i % len(dirs)]
        ext = exts[i % len(exts)]
        place(d, f"{g.rng.choice(cfg['en'])}_{g.rng.choice(cfg['en'])}_{g.stamp()}", ext, g.filler(4, 0.2), g.body())
    empty_dir = corpus / ("빈 폴더" if profile == "general" else "empty stage")
    empty_dir.mkdir(parents=True)

    def wrap(q: str, kind: str) -> str:
        if not cfg["wrap"]:
            return q
        style = g.rng.randrange(4)
        if style == 0:
            return f"find the document about {q}"
        if style == 1:
            return q.upper() if q.isascii() else f"{q} 에 대한 문서 찾아줘"
        if style == 2:
            return f"{q} 관련 파일"
        return q

    # ---- filename targets --------------------------------------------------
    for n in range(10):
        tok = g.ko() if n % 3 == 2 else g.en_plain() if n % 2 else g.en()
        d = dirs[(n * 5) % len(dirs)]
        rel = place(d, f"{tok}_{g.rng.choice(cfg['en'])}", exts[n % len(exts)], g.filler(4), g.body())
        add_case("filename", "corpus", wrap(tok, "name"), {"kind": "find", "targets": [rel], "rank_max": 1})
    for n in range(6):
        a, b = (g.ko(), g.ko()) if n % 3 == 2 else (g.en_plain(), g.en_plain())
        d = dirs[(n * 7 + 1) % len(dirs)]
        rel = place(d, f"{a}_{b}", exts[(n + 2) % len(exts)], g.filler(4), g.body())
        decoy = place(dirs[(n + 3) % len(dirs)], f"{a}_{g.rng.choice(cfg['en'])}_{g.stamp()}", "txt", g.filler(4), g.body())
        add_case("filename_multi", "corpus", wrap(f"{a} {b}", "name"), {"kind": "find", "targets": [rel], "rank_max": 1}, decoy=decoy)

    # ---- content targets ---------------------------------------------------
    content_exts = [e for e in exts if e != "log"]
    for n in range(12):
        tok = g.ko() if n % 3 == 2 else g.en()
        d = dirs[(n * 3 + 2) % len(dirs)]
        ext = content_exts[n % len(content_exts)]
        if ext == "rtf" and not tok.isascii():
            ext = "txt"
        rel = place(d, f"{g.rng.choice(cfg['en'])}_{g.rng.choice(cfg['en'])}_{g.stamp()}", ext, g.filler(4), g.body(f"the {g.rng.choice(cfg['en'])} confirmed {tok} on site"))
        add_case("content", "corpus", wrap(tok, "text"), {"kind": "find", "targets": [rel], "rank_max": 3, "snippet": tok})
    for n in range(6):
        t1, t2, t3 = (g.ko(), g.ko(), g.ko()) if n % 3 == 2 else (g.en_plain(), g.en_plain(), g.en_plain())
        d = dirs[(n * 2 + 4) % len(dirs)]
        full = place(d, f"{g.rng.choice(cfg['en'])}_{g.stamp()}", "md" if n % 2 else "txt", g.filler(4), g.body(f"{t1} {t2} {t3}"))
        p2 = place(dirs[(n + 1) % len(dirs)], f"{g.rng.choice(cfg['en'])}_{g.stamp()}", "txt", g.filler(4), g.body(f"{t1} {t2}"))
        p1 = place(dirs[(n + 5) % len(dirs)], f"{g.rng.choice(cfg['en'])}_{g.stamp()}", "txt", g.filler(4), g.body(f"{t1}"))
        add_case("content_multi", "corpus", f"{t1} {t2} {t3}", {"kind": "find", "targets": [full], "rank_max": 1}, partials=[p2, p1])
    for n in range(4):
        tok = g.en_plain()
        word = g.rng.choice(cfg["en"])
        rel = place(dirs[(n * 4) % len(dirs)], f"{word}_{g.stamp()}", "md", g.filler(4), g.body(f"{tok}"))
        add_case("content_multi", "corpus", f"{word} {tok}", {"kind": "find", "targets": [rel], "rank_max": 3}, note="common word + rare word")

    # ---- multi-word requests where one word exists nowhere ----------------------------------
    for n in range(4):
        real = g.ko() if n % 2 else g.en_plain()
        ghost = g.ko() if n % 2 else g.en_plain()
        rel = place(dirs[(n * 5 + 3) % len(dirs)], f"{g.rng.choice(cfg['en'])}_{g.stamp()}", "txt", g.filler(4), g.body(f"{real}"))
        add_case("partial_match", "corpus", f"{real} {ghost}", {"kind": "find", "targets": [rel], "rank_max": 3, "partial": real.lower(), "ghost": ghost.lower()})

    # ---- sub-folders of an indexed root --------------------------------------
    sub_dirs = [dirs[i] for i in (0, 3, 5, 7)]
    for n, d in enumerate(sub_dirs):
        tok_in, tok_name = g.en_plain(), g.ko() if n % 2 else g.en_plain()
        rel_c = place(d, f"{g.rng.choice(cfg['en'])}_{g.stamp()}", "md", g.filler(4), g.body(f"{tok_in}"))
        rel_n = place(d, f"{tok_name}_{g.rng.choice(cfg['en'])}", "txt", g.filler(4), g.body())
        add_case("subfolder", f"corpus/{d}", tok_in, {"kind": "find", "targets": [Path(rel_c).name], "rank_max": 3})
        add_case("subfolder", f"corpus/{d}", tok_name, {"kind": "find", "targets": [Path(rel_n).name], "rank_max": 1})
        outside_tok = g.en_plain()
        other = next(x for x in dirs if not x.startswith(d + "/") and not d.startswith(x + "/") and x != d)
        out_rel = place(other, f"{g.rng.choice(cfg['en'])}_{g.stamp()}", "txt", g.filler(4), g.body(f"{outside_tok}"))
        add_case("subfolder_outside", f"corpus/{d}", outside_tok, {"kind": "empty", "reasons": ["no_match_in_index"], "forbidden": [Path(out_rel).name]})
    add_case("subfolder_outside", f"corpus/{empty_dir.name}", g.en_plain(), {"kind": "empty", "reasons": ["scope_has_no_indexed_files", "no_match_in_index"], "forbidden": []})

    # ---- Korean / special-character root ----------------------------------
    kroot_name = g.nfd(cfg["korean_root"])
    kroot = out / kroot_name
    for _i in range(30):
        write_doc(kroot / f"{g.rng.choice(cfg['en'])}_{g.stamp()}.txt", "txt", g.filler(4), g.body())
    for n in range(4):
        tok = g.ko()
        nm = g.nfd(f"{tok} 보고서 {n}")
        write_doc(kroot / f"{nm}.txt", "txt", g.filler(4), g.body())
        add_case("korean_path", kroot_name, tok, {"kind": "find", "targets": [f"{nm}.txt"], "rank_max": 1})
        ctok = g.ko() if n % 2 else g.en_plain()
        nm2 = f"{g.rng.choice(cfg['en'])}_{g.stamp()}"
        write_doc(kroot / "하위 폴더" / f"{nm2}.md", "md", g.filler(4), g.body(f"{ctok}"))
        add_case("korean_path", kroot_name, ctok, {"kind": "find", "targets": [f"하위 폴더/{nm2}.md"], "rank_max": 3})
    if cfg["nfd"]:
        # An NFD file name (macOS-synced folders) queried with the NFC spelling an agent types.
        tok = g.ko()
        nm = unicodedata.normalize("NFD", f"{tok} 계약서")
        write_doc(kroot / f"{nm}.txt", "txt", g.filler(4), g.body())
        add_case("korean_path", kroot_name, unicodedata.normalize("NFC", tok), {"kind": "find", "targets": [f"{nm}.txt"], "rank_max": 1}, note="NFD name, NFC query")

    # ---- large root --------------------------------------------------------
    large = out / cfg["large_root"]
    per_dir = 25
    for i in range(large_files):
        sub = f"g{i // per_dir // 20:02d}/s{i // per_dir:03d}"
        write_doc(large / sub / f"{g.rng.choice(cfg['en'])}_{g.stamp()}.txt", "txt", g.filler(3), g.body(words=40))
    n_dirs = max(1, large_files // per_dir)
    def large_dir(k: int) -> str:
        j = (k * 37 + 11) % n_dirs
        return f"g{j // 20:02d}/s{j:03d}"

    for n in range(5):
        tok = g.en_plain() if n % 2 else g.ko()
        nm = f"{tok}_{g.rng.choice(cfg['en'])}.txt"
        write_doc(large / large_dir(n) / nm, "txt", g.filler(3), g.body(words=40))
        add_case("large", cfg["large_root"], tok, {"kind": "find", "targets": [f"{large_dir(n)}/{nm}"], "rank_max": 1})
    for n in range(5):
        tok = g.en_plain()
        nm = f"{g.rng.choice(cfg['en'])}_{g.stamp()}.txt"
        write_doc(large / large_dir(n + 7) / nm, "txt", g.filler(3), g.body(f"{tok}", words=40))
        add_case("large", cfg["large_root"], tok, {"kind": "find", "targets": [f"{large_dir(n + 7)}/{nm}"], "rank_max": 3})
    common_word = cfg["en"][0]
    for k, floor in ((100, 100), (500, 101)):
        add_case("topk", cfg["large_root"], common_word, {"kind": "count", "min_results": floor}, top_k=k)

    # ---- empty / whitespace / no-match -----------------------------------------
    for q in ("", "   ", "\t\n"):
        add_case("empty_query", "corpus", q, {"kind": "empty", "reasons": ["empty_query"], "forbidden": []})
    for _n in range(3):
        add_case("no_match", "corpus", g.en_plain() + g.en_plain(), {"kind": "empty", "reasons": ["no_match_in_index"], "forbidden": []})
    add_case("no_match", "corpus", g.ko() + g.ko(), {"kind": "empty", "reasons": ["no_match_in_index"], "forbidden": []})
    add_case("no_match", cfg["large_root"], g.en_plain() + g.en_plain(), {"kind": "empty", "reasons": ["no_match_in_index"], "forbidden": []})
    add_case("no_match", "corpus", " ".join(g.en_plain() for _ in range(6)), {"kind": "empty", "reasons": ["no_match_in_index"], "forbidden": []}, note="long nonsense sentence")

    # ---- non-discovery requests: code symbols, regex, line numbers ------------------
    sig_files = []
    for lang in ["py", "rs", "py", "rs"]:
        ident = f"{g.rng.choice(['parse', 'load', 'merge', 'check'])}_{g.en_plain()}"
        rel = f"src/{ident}.{lang}"
        write_code(corpus / rel, ident, lang)
        sig_files.append((ident, rel, lang))
    # An exact identifier is still a word Jikji indexes: it must return the file (not the line) and
    # say so. snake_case and camelCase, each in a source file of its own.
    for ident, rel, _lang in sig_files[:2]:
        add_case("code_identifier", "corpus", ident, {"kind": "find", "targets": [rel], "rank_max": 3, "identifier": True})
    for lang in ("py", "rs"):
        inner = f"{g.rng.choice(['parse', 'sync', 'build'])}_{g.en_plain()}"
        rel_inner = f"src/helpers_{g.stamp()}.{lang}"
        write_code(corpus / rel_inner, inner, lang)
        add_case("code_identifier", "corpus", inner, {"kind": "find", "targets": [rel_inner], "rank_max": 3, "identifier": True}, note="identifier defined inside a file with another name")
        camel = f"load{g.en_plain().capitalize()}Cache"
        rel = f"src/{camel}.{lang}"
        write_code(corpus / rel, camel, lang)
        add_case("code_identifier", "corpus", camel, {"kind": "find", "targets": [rel], "rank_max": 3, "identifier": True})
    id0, rel0, _ = sig_files[0]
    id1, rel1, _ = sig_files[1]
    if profile == "general":
        signals = [
            (f"fn {id1}(", rel1), ("^import .*requests$", rel0), (r"foo\d+bar", None), ("Vec<String>", rel1),
            ("self.cache[key]", rel0), (f"line 42 of {Path(rel0).name}", rel0), (f"{Path(rel0).name}:120", rel0), ("TODO: handle misses", rel0),
        ]
        negatives = [
            ("{u}_report_2024", "{u}_report_2024.md", "md"), ("{k} 회의록 2024-03-15", "{k} 회의록 2024-03-15.txt", "txt"), ("budget-final {u} v2.1", "budget-final-{u}-v2.1.md", "md"),
            ("{u} report (final)", "{u} report (final).docx", "docx"), ("where is the {u} file?", "{u} plan.txt", "txt"), ("[초안] {k} 계획", "[초안] {k} 계획.md", "md"),
        ]
    else:
        signals = [
            ("std::collections::HashMap", rel1), (f"{Path(rel1).name}:120:5", rel1), ("#L88", rel0), ("[A-Z]{3}-\\d{4}", None),
            ("SELECT * FROM users WHERE id=", None), (f"{id0}()", rel0), ("$HOME/.config", None), (r"\bneedle\b", None),
            ("grep -rn needle", None), ("def __init__(self", rel0), ("줄 57", None), ("line 300", None), ("3번째 줄", None), ("x => x + 1", None), ("a.b.c()", None), ("lines 10-20 of README", None),
        ]
        negatives = [
            ("{u} (v2) draft", "{u} (v2) draft.md", "md"), ("what is {u}?", "{u} summary.txt", "txt"), ("R&D {u} notes", "R&D {u} notes.md", "md"),
            ("{k} [사본] 정리", "{k} [사본] 정리.txt", "txt"), ("don't {u} panic", "don't {u} panic.md", "md"), ("{u}-draft_v3.2", "{u}-draft_v3.2.md", "md"),
            ("100% {u} done", "100% {u} done.txt", "txt"), ("{k}, {u}: 최종본", "{k} {u} 최종본.md", "md"),
        ]
    for q, rel in signals:
        add_case("signal", "corpus", q, {"kind": "signal", "codes": ["pattern_syntax_not_supported"]}, truth=rel)
    for qt, nt, _ext in negatives:
        u, k = g.en_plain(), g.ko()
        name = nt.format(u=u, k=k)
        stem, ext_ = name.rsplit(".", 1)
        write_doc(corpus / "names" / f"{stem}.{ext_}", ext_, g.filler(4), g.body())
        add_case("no_signal", "corpus", qt.format(u=u, k=k), {"kind": "find", "targets": [f"names/{name}"], "rank_max": 3, "no_signal": True})

    # ---- non-text files: found by name, folder or extension, never by content --------------
    mexts = cfg["media_exts"]
    for n in range(6):
        tok = g.ko() if n % 3 == 2 else g.en_plain()
        ext = mexts[n % len(mexts)]
        d = dirs[(n * 3 + 1) % len(dirs)]
        rel = f"{d}/{tok}_{g.rng.choice(cfg['en'])}.{ext}"
        (corpus / rel).parent.mkdir(parents=True, exist_ok=True)
        (corpus / rel).write_bytes(bytes([n + 1]) * 64)
        add_case("media_name", "corpus", wrap(tok, "name"), {"kind": "find", "targets": [rel], "rank_max": 1, "media": True})
    ext = mexts[0]
    folder_tok = g.ko()
    mrel = f"{folder_tok} 사진/{g.rng.choice(cfg['en'])}_{g.stamp()}.{ext}"
    (corpus / mrel).parent.mkdir(parents=True, exist_ok=True)
    (corpus / mrel).write_bytes(b"\x01" * 64)
    add_case("media_name", "corpus", folder_tok, {"kind": "find", "targets": [mrel], "rank_max": 3, "media": True}, note="match by folder name")
    for n in range(3):
        what = g.en_plain()
        q = f"{what} 이미지 속 장면" if (cfg["wrap"] and n % 2) else f"photo showing {what}"
        add_case("media_content", "corpus", q, {"kind": "empty", "reasons": ["no_match_in_index"], "forbidden": [], "needs_code": "non_text_content_not_searchable"})

    # ---- missing / not indexed ---------------------------------------------------
    stranger = out / "stranger"
    write_doc(stranger / "note.txt", "txt", "note", g.body("webchat"))
    add_case("missing", "stranger", "webchat", {"kind": "missing", "reasons": ["not_indexed"]})
    add_case("missing", "no-such-folder", "webchat", {"kind": "missing", "reasons": ["path_not_found"]})
    add_case("missing", ".", "webchat", {"kind": "missing", "reasons": ["not_indexed"], "lists_indexed_roots": True}, note="parent of indexed roots")

    # ---- changes after indexing --------------------------------------------------
    fresh_tok = g.en_plain()
    mutations.append({"op": "add", "path": f"corpus/{dirs[0]}/{fresh_tok}_new.md", "text": f"# new\n{g.filler(10)}\n"})
    add_case("fresh", "corpus", fresh_tok, {"kind": "find", "targets": [f"{dirs[0]}/{fresh_tok}_new.md"], "rank_max": 1, "explain_ok": True}, note="new file in an indexed folder")
    tok2 = g.en_plain()
    mutations.append({"op": "add", "path": f"corpus/신규 폴더 {profile}/note.md", "text": f"# new folder\n{tok2}\n{g.filler(10)}\n"})
    add_case("fresh", "corpus", tok2, {"kind": "find", "targets": [f"신규 폴더 {profile}/note.md"], "rank_max": 3, "explain_ok": True}, note="new folder with a file")
    tok3 = g.en_plain()
    victim = place(dirs[1], f"victim_{g.stamp()}", "txt", g.filler(4), g.body())
    mutations.append({"op": "append", "path": f"corpus/{victim}", "text": f"\n{tok3}\n"})
    add_case("fresh", "corpus", tok3, {"kind": "find", "targets": [victim], "rank_max": 3, "explain_ok": True}, note="edited file gains a word")
    tok4 = g.en_plain()
    gone = place(dirs[2], f"gone_{g.stamp()}", "txt", g.filler(4), g.body(f"{tok4}"))
    mutations.append({"op": "delete", "path": f"corpus/{gone}"})
    add_case("fresh", "corpus", tok4, {"kind": "empty", "reasons": ["no_match_in_index"], "forbidden": [Path(gone).name]}, note="deleted file must not be returned")

    doc = {"dataset": profile, "seed": cfg["seed"] if seed is None else seed, "large_files": large_files,
           "roots": {"corpus": "corpus", "large": cfg["large_root"], "korean": kroot_name}, "mutations": mutations, "cases": cases}
    (out / "cases.json").write_text(json.dumps(doc, ensure_ascii=False, indent=1), encoding="utf-8")
    return doc


if __name__ == "__main__":
    import argparse

    ap = argparse.ArgumentParser(description="Generate a synthetic Jikji search fixture + cases.json")
    ap.add_argument("profile", choices=sorted(PROFILES))
    ap.add_argument("out", type=Path)
    ap.add_argument("--seed", type=int)
    ap.add_argument("--large-files", type=int, default=5000)
    a = ap.parse_args()
    d = build_fixture(a.profile, a.out, a.seed, a.large_files)
    print(f"{a.profile}: {len(d['cases'])} cases, {len(d['mutations'])} mutations -> {a.out}")
