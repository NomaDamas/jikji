<h1 align="center">Jikji</h1>

<p align="center">
  <strong>Local file maps for AI agents</strong><br>
  <strong>로컬 에이전트를 위한 비파괴 파일 탐색 지도</strong>
</p>

<p align="center">
  <a href="https://github.com/NomaDamas/jikji/blob/main/LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-65f2ad.svg"></a>
  <img alt="Python 3.11+" src="https://img.shields.io/badge/Python-3.11%2B-7dd9ff.svg">
  <img alt="Non destructive" src="https://img.shields.io/badge/Safety-Non--destructive-b8a1ff.svg">
  <img alt="No RAG core" src="https://img.shields.io/badge/Core-No%20RAG%20%2F%20No%20embeddings-ffd166.svg">
</p>

<p align="center">
  <a href="https://nomadamas.github.io/jikji/">
    <img src="docs/jikji-readme-hero.svg" alt="Jikji — local file maps for AI agents" width="100%" />
  </a>
</p>

<p align="center">
  <a href="https://nomadamas.github.io/jikji/"><strong>Live intro</strong></a> ·
  <a href="docs/agent-installation.md"><strong>Agent install guide</strong></a> ·
  <a href="skills/jikji/SKILL.md"><strong>Skill file</strong></a> ·
  <a href="docs/local-agent-search-standard.md"><strong>Search standard</strong></a>
</p>

---

## What is Jikji?

**English** — Jikji prepares an explicit local folder so AI agents can find files, folders, metadata, and parsed document text without repeatedly crawling the original filesystem.

**한국어** — 직지는 명시된 로컬 폴더를 에이전트가 찾기 쉬운 지도로 바꿉니다. 원본 파일을 옮기거나, 이름을 바꾸거나, 삭제하지 않습니다.

Raw agents wander:

```text
guess query → list folders → grep files → open documents → repeat
```

Jikji-equipped agents use a map:

```text
jikji brief ROOT "query" → ranked paths + evidence → verify only if needed
```

## Quick start

```bash
git clone https://github.com/nomadamas/jikji.git
cd jikji
python3 -m venv .venv
.venv/bin/pip install -e .

.venv/bin/jikji brief ~/Documents "contract pdf from last spring" --top-k 10 --json
```

Image files are indexed with lightweight local metadata (format, dimensions, and
selected EXIF datetime when available). OCR text for images and scanned PDFs is
optional and local-only; install `tesseract` and confirm with
`jikji doctor ROOT --json` (`image_support.ocr_active`) when you want image text
to contribute to search.

한국어 예시:

```bash
jikji brief ~/Documents "작년 봄 계약서 PDF" --top-k 10 --json
jikji search ~/Documents "파일명, 본문 단서, 문서 설명" --top-k 10 --json
```

## Why agents need it

| Raw local agent | Agent + Jikji |
| --- | --- |
| Keeps guessing search terms | Gets ranked candidate paths |
| Re-opens PDFs/HWP/Office files | Reuses parsed document text caches |
| Wanders through messy folders | Reads folder context and route guides |
| Confuses copies and decoys | Uses file cards and duplicate hints |
| Burns exploratory turns | Verifies only the best candidates |


## Real local agent benchmark

On June 3, 2026, Jikji was tested with the same **Hermes Agent v0.10.0** in two modes:

- `raw`: Hermes searches original folders/files without Jikji.
- `jikji-fast`: Hermes receives Jikji's prebuilt map/search candidates first.

Synthetic Office-body search, where clues lived inside DOCX/PPTX/XLSX files:

```text
mode          cases  hit@1   hit@10  avg_seconds
raw           6      0.8333  0.8333  36.171
jikji-fast    6      1.0000  1.0000  10.477
jikji-direct  6      1.0000  1.0000   0.004
```

Large real local document root, anonymized: 20k+ files, 13k+ parser-target
documents, HWP/HWPX/PDF/PPTX/XLSX-heavy. A balanced 12-case actual Hermes
sample showed:

```text
mode          cases  hit@1   hit@10  avg_seconds
raw           12     0.2500  0.2500  29.521
jikji-fast    12     0.5833  0.7500  17.834
jikji-direct  12     0.5833  0.7500   1.645
```

A 36-case deterministic diagnostic on the same root showed path-only search at
`Hit@10 = 0.4167` and Jikji at `Hit@10 = 0.8056`.

Takeaway: for messy local document folders with Korean HWP/HWPX, PDFs, Office
files, copies, and folder-context clues, Jikji gave Hermes a substantially better
starting map. Remaining weak spot: exact phrase-memory cases still need ranking
and parser-coverage improvements.

## What Jikji creates

```text
000_JIKJI_AGENT_MAP.md      root guide for humans and agents
.jikji/search_index.sqlite  instant lexical/content/metadata search index
.jikji/doc_text/            parsed PDF/HWP/HWPX/Office/etc. text cache
.jikji/file_cards.jsonl     per-file cards, tags, parse status, evidence
.jikji/folder_profile.jsonl folder roles and navigation context
.jikji/agent_routes.md      safe fallback route for autonomous agents
```

These are generated artifacts. They can be regenerated or removed with `jikji clean`.

## Agent protocol

Paste this behavior into Claude Code, Codex, Hermes, OpenCode/OpenClone-style agents, or any CLI-capable local agent:

```text
Use Jikji for local file discovery when an explicit root is available.
First call: jikji brief ROOT "query" --top-k 10 --json.
Prefer returned candidate paths and evidence over broad filesystem crawling.
Only inspect original files for final verification.
Never move, rename, delete, or reorganize user files.
```

Install the reusable skill instruction:

```bash
# Install into detected/common local-agent skill directories
jikji agent-skill-install --agent all --json

# Or install for one agent
jikji codex-skill-install --json
jikji claude-skill-install --json
jikji hermes-skill-install --json
jikji opencode-skill-install --json

# Any other coding/local agent
jikji skill-export --dest /path/to/that-agent/skills/jikji/SKILL.md --json
```

Install also queues a low-impact background prepare for existing common
user-content roots so the first agent search feels useful: Documents, Downloads,
Desktop, and common cloud-sync folders such as Google Drive, OneDrive, Dropbox,
and iCloud Drive. Jikji limits default roots from local CPU/memory and processes
them sequentially. Add more roots, wait in the foreground, or disable
post-install indexing:

```bash
jikji agent-skill-install --agent all --prepare-root /mnt/work-drive --json
jikji agent-skill-install --agent all --foreground-prepare --json
jikji agent-skill-install --agent all --no-prepare --json
```

After the skill is installed, local file/folder/document discovery requests should
trigger Jikji automatically. The agent does not need the user to say "use Jikji";
it should call `jikji brief ROOT "query" --top-k 10 --json` first when a bounded
root is available. For agents without a formal skill system, paste the output of
`jikji skill-export` into their persistent instructions or project memory.

## Core commands

```bash
jikji brief ROOT "natural language file clue" --top-k 10 --json
jikji search ROOT "keyword, filename, or document description" --top-k 10 --json
jikji prepare ROOT --json
jikji refresh ROOT --json
jikji map ROOT
jikji doctor ROOT --json
jikji clean ROOT --dry-run --json
jikji clean ROOT --json
```

## Safety boundary

- **Non-destructive by default** — no moving, renaming, deleting, or reorganizing source files.
- **Explicit roots only** — Jikji does not silently scan every drive.
- **Local-first** — no embeddings, vector DB, cloud parser, or LLM is required for core indexing/search.
- **Sensitive output warning** — `.jikji/doc_text/` may contain extracted document text.

Recommended `.gitignore` for indexed roots:

```gitignore
.jikji/
000_JIKJI_AGENT_MAP.md
```

## Docs

- [Agent installation manual](docs/agent-installation.md)
- [Local-agent search standard](docs/local-agent-search-standard.md)
- [Promo page](https://nomadamas.github.io/jikji/) / [source](docs/jikji-value.html)
- [Hardbench benchmark notes](docs/hardbench-benchmark.md)

## Development

```bash
python3 -m venv .venv
.venv/bin/pip install -e .
.venv/bin/pip install pytest ruff
.venv/bin/ruff check src tests
.venv/bin/pytest -q
.venv/bin/python -m compileall -q src tests
```

## License

MIT License. See [LICENSE](LICENSE).
