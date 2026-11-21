#!/usr/bin/env python3
"""Blind probe: natural-language requests around one distinctive word.

Each phrasing asks for a document that exists (`TARGET`, found by its text, and a second one by
its name). A binary should return it first however the request is worded ("where can I find",
"찾아 줄래"). The phrasings were written before the probe was run on any binary and are frozen:
record failures, do not edit the list to pass them. (A "some words are missing" warning was tried
and dropped: filler words made it fire on 5 of these 12 phrasings, and filler cannot be told from a
misspelt word. `results[].matched_words` states the facts without the judgement.)

Usage: python3 tools/search_eval/wrapper_probe.py --bin NAME=PATH [--bin ...]
"""

from __future__ import annotations

import argparse
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402

PHRASINGS = [
    "where can I find the {t} document",
    "show me the file about {t}",
    "can you look for {t} please",
    "I need the {t} notes",
    "which file mentions {t}?",
    "locate {t} for me",
    "{t} 자료 어디 있지",
    "{t} 내용이 들어있는 파일 알려줘",
    "{t} 관련해서 찾아주세요",
    "{t} 에 대한 파일 좀 보여줘",
    "give me the file that has {t}",
    "search for {t}",
]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", action="append", required=True)
    a = ap.parse_args()
    work = Path(tempfile.mkdtemp(prefix="jikji-wrap-"))
    try:
        root = work / "r"
        (root / "notes").mkdir(parents=True)
        (root / "notes" / "meeting.md").write_text("weekly sync mentions quorvex budget freeze and hiring", encoding="utf-8")
        (root / "notes" / "quorvex 회의록.txt").write_text("agenda only", encoding="utf-8")
        (root / "other.txt").write_text("unrelated text about gardening", encoding="utf-8")
        for spec in a.bin:
            b = common.Binary.parse(spec)
            sb = common.Sandbox(work / b.name)
            common.prepare(b, sb, root)
            wrong_first = alarms = 0
            for phrasing in PHRASINGS:
                v = common.find(b, sb, root, phrasing.format(t="quorvex"))
                top = v["paths"][:1]
                if not top or not top[0].startswith("notes/"):
                    wrong_first += 1
                    print(f"   {b.name}: not found first: {phrasing!r} -> {top}")
                listed = [w for r in v["results"][:1] for w in r.get("matched_words", [])]
                if "quorvex" not in listed and v["version"] != "0.2.3":
                    alarms += 1
                    print(f"   {b.name}: top result does not list the target word for {phrasing!r}: {listed}")
            print(f"{b.name}: target first {len(PHRASINGS) - wrong_first}/{len(PHRASINGS)}, target word listed in the top result's matched_words {len(PHRASINGS) - alarms}/{len(PHRASINGS)}")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
