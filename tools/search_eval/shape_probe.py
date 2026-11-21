#!/usr/bin/env python3
"""Blind probe of the regex/code/line-number detector with queries written independently of it.

Usage: python3 tools/search_eval/shape_probe.py --bin NAME=PATH [--bin ...]

`SIGNAL` holds requests grep answers better than an index of words; `PLAIN` holds ordinary
file-finding requests whose names contain punctuation. A binary is judged on whether
`pattern_syntax_not_supported` appears exactly for the first group. Lists are frozen: when a
case fails, record it in the report; do not edit the lists to make it pass.
"""

from __future__ import annotations

import argparse
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402

SIGNAL = [
    r"\bTODO\b", r"^\s*def\s+\w+", r"error\[E\d+\]", r"[a-f0-9]{7,40}", r"https?://\S+", r"(foo|bar)baz",
    r"\d{3}-\d{4}-\d{4}", "pub fn new(", "impl<T> Drop for", "console.log(", "foo.bar.baz()", "std::mem::swap",
    "HashMap<String, Vec<u8>>", "a -> b", "x == y", "if (x && y)", "$PATH", "${HOME}/bin", "main.rs:42", "lib.rs:10:5",
    "line 7", "lines 3-9", "줄 12", "12번째 줄", "#L120", "[0-9]+", "[^abc]", "foo{2,3}", "(?i)hello", "a|b|c",
    "fn main()", "self.items[0]", "git log --oneline | head", "import os.path",
]
PLAIN = [
    "meeting notes (draft)", "2024 budget [final]", "what is the lease term?", "Q&A session notes", "100% coverage report",
    "release-notes v1.2.3", "invoice #1042", "tax 2023: summary", "사업계획서 (수정본)", "회의록 [3월]", "김철수 이력 정리 ?",
    "report_final_v2", "design doc - phase 1", "a+b comparison", "Smith, John letter", "notes (copy) (1)", "C++ primer summary",
    "who signed the contract?", "계약서, 최종", "file-name_with-dashes", "version 2.0 spec", "R&D roadmap", "todo list for march",
    "대한민국 2024년 예산안", "foo bar baz", "e-mail template", "minutes 2024-03-15", "lecture 3: graphs", "price $20 list", "my (best) photos",
]

# Second frozen batch (written for the request-plan work, before it was run against any binary).
SIGNAL += [
    r"\w+@\w+\.com", r"v\d+\.\d+", "(a|b)+", "^#include", "ls -la /tmp", "docker run --rm alpine", "rm -rf ./build",
    "SELECT * FROM orders", "items.map(x => x.id)", "Foo::bar::baz", "ptr->next", "if (a == b)", "a && b || c",
    "src/lib.rs:88", "main.go:7:2", "line 100", "줄 3", "5번째 행", "#L7-L12", "$USER", "${PWD}/x", "async fn run(",
    "def f(x):", "vec![1, 2]", "Map<K, V>", "obj.method()", "arr[0].name", "[a-z]+", "\\.txt$", "x{2}",
]
PLAIN += [
    "2024 연말정산 자료", "회의록 (3월 15일)", "Project Plan [v3]", "What is the budget?", "Q4 results & outlook", "40% 할인 목록",
    "resume - final", "Kim, Soo-jin portfolio", "trip to Jeju (2023)", "사진 모음 [여름]", "invoice 2023-11 #88", "meeting: kickoff",
    "A/B test results", "계약서 사본 (1)", "mom's recipes", "backup copy of notes", "lecture notes week 3", "서울-부산 일정",
    "photo_2024_01", "IMG 4021", "report.final.v2", "최종_최종_진짜최종", "how to renew passport?", "tax return 2022",
    "slide deck (draft)", "week-12 summary", "ㅋㅋ 메모", "e-mail 정리", "대출 상환 계획", "birthday party plan!",
]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", action="append", required=True)
    a = ap.parse_args()
    work = Path(tempfile.mkdtemp(prefix="jikji-shape-"))
    root = work / "r"
    root.mkdir()
    (root / "note.txt").write_text("hello world", encoding="utf-8")
    code = 0
    for spec in a.bin:
        b = common.Binary.parse(spec)
        sb = common.Sandbox(work / b.name)
        common.prepare(b, sb, root)

        def flagged(q: str, binary: common.Binary = b, sandbox: common.Sandbox = sb) -> bool:
            v = common.find(binary, sandbox, root, q)
            return "pattern_syntax_not_supported" in v.get("note_codes", [])

        missed = [q for q in SIGNAL if not flagged(q)]
        false_alarm = [q for q in PLAIN if flagged(q)]
        print(f"{b.name}: signal detected {len(SIGNAL) - len(missed)}/{len(SIGNAL)}, ordinary names wrongly flagged {len(false_alarm)}/{len(PLAIN)}")
        for q in missed:
            print(f"   missed: {q!r}")
        for q in false_alarm:
            print(f"   false alarm: {q!r}")
        code |= bool(false_alarm)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
