# Jikji

<p align="center">
  <a href="https://nomadamas.github.io/jikji/">
    <img src="docs/jikji-readme-hero.svg" alt="Jikji — local file maps for AI agents" width="100%" />
  </a>
</p>

**Agent-installable local file maps.** Jikji makes local files legible to AI agents without moving, renaming, or deleting the user's files.

Local agents such as Claude Code, Codex, Hermes, OpenCode/OpenClone-style tools, or any CLI-capable assistant can install Jikji from GitHub and use one tool-first command to find files by filename, folder memory, metadata, or parsed document body text.

```bash
git clone https://github.com/nomadamas/jikji.git
cd jikji
python3 -m venv .venv
.venv/bin/pip install -e .
.venv/bin/jikji brief ~/Documents "contract pdf from last spring" --top-k 10 --json
```

```text
Raw local agent:       repeatedly list/grep/open folders and documents
Agent + Jikji:         ask prebuilt map/search index for ranked paths + evidence
Jikji safety boundary: never move, rename, delete, or reorganize original files
Core artifacts:        .jikji/ + 000_JIKJI_AGENT_MAP.md
No core RAG:           no embeddings, vector DB, cloud parser, or LLM required
```

Agent manual and promotion page:

- [Agent installation manual](docs/agent-installation.md)
- [Local-agent search standard](docs/local-agent-search-standard.md)
- [Promo webpage source](docs/jikji-value.html) and [GitHub Pages entry](docs/index.html)
- [Hardbench benchmark report](docs/hardbench-benchmark.md)

## What changes for agents

```text
AS-IS raw agent                    TO-BE agent + Jikji
---------------------------------  ----------------------------------------
Guess search terms repeatedly      Ask `jikji brief ROOT "query" --json`
List/grep/open many folders        Receive ranked paths + evidence
Re-parse PDF/HWP/Office documents  Reuse `.jikji/doc_text/` parser cache
Lose time on decoys/duplicates     Use map cards, folder context, duplicates
Spend many exploratory turns       Verify only the best candidates
```

## What Jikji creates

```text
000_JIKJI_AGENT_MAP.md      visible route guide at the root
.jikji/search_index.sqlite  instant lexical/content/metadata search index
.jikji/doc_text/            extracted text cache for PDF/HWP/HWPX/Office/etc.
.jikji/file_cards.jsonl     per-file map cards, tags, parse status, evidence
.jikji/folder_profile.jsonl folder roles and local context for navigation
.jikji/agent_routes.md      safe fallback order for autonomous agents
```

## Quick commands


```bash
jikji search ~/Documents "contract pdf from last spring" --json
jikji brief ~/Documents "contract pdf from last spring" --json
```

`search` is the fast ranked-candidate entry point. `brief` wraps the same instant
index in a compact agent route sheet: candidate paths, evidence snippets, folder
context, fallback commands, and a non-destructive policy. Both commands
auto-prepare an explicit root when no instant index exists, return existing
results immediately when the index is stale, and can refresh in the background.
`prepare`, `refresh`, `map`, and `doctor` remain manual/admin commands. `clean`
removes Jikji-generated artifacts from one prepared root when you want to leave
no trace.

Jikji writes `.jikji/` and `000_JIKJI_AGENT_MAP.md` with folder/file/document
indexes, document text caches, an Everything-style instant search index, and
agent route guides.

## Why local agents use it

Local agents such as Claude Code, Codex, Hermes, and OpenCode-style tools can
install Jikji from a checkout and call one tool-first command:

```bash
jikji search ~/Documents "keyword, remembered filename, or document description" --top-k 10 --json
```

Agents should only fall back to direct `rg`/`jq` over `.jikji/` when the fast
search/brief result is empty or clearly insufficient.

Remove generated artifacts from a prepared root without touching original files:

```bash
jikji clean ~/Documents --dry-run --json
jikji clean ~/Documents --json
```

`clean` verifies `.jikji/manifest.json` before deleting `.jikji/` and
`000_JIKJI_AGENT_MAP.md`; use `--force` only when the directory is known to be a
Jikji-generated index but the manifest is missing or damaged.

Evaluate whether the generated map/indexes are helping agents find files:

```bash
jikji eval-generate ~/Documents --cases 80 --json
jikji eval ~/Documents --json

# External benchmark: raw filesystem search vs Jikji-assisted search
jikji hippocamp-fetch ./benchmarks/hippocamp --profile Adam --split Subset --json
jikji prepare ./benchmarks/hippocamp/Adam_Subset --json
jikji hippocamp-import ./benchmarks/hippocamp/Adam_Subset \
  --annotation ./benchmarks/hippocamp/Adam_Subset.annotation.json \
  --json
jikji bench-run ./benchmarks/hippocamp/Adam_Subset \
  --eval-set ./benchmarks/hippocamp/Adam_Subset_hippocamp_eval_set.jsonl \
  --modes raw,jikji --json

# Optional: repeat the same no-leak benchmark 20 times after code/index changes
jikji bench-iterate ./benchmarks/hippocamp/Adam_Subset \
  --eval-set ./benchmarks/hippocamp/Adam_Subset_hippocamp_eval_set.jsonl \
  --iterations 20 --json

# Optional: actual Hermes agent benchmark (external eval set required)
jikji hermes-skill-install --json
jikji hermes-bench ./benchmarks/hippocamp/Adam_Subset \
  --eval-set ./benchmarks/hippocamp/Adam_Subset_hippocamp_eval_set.jsonl \
  --modes raw,jikji \
  --candidate-top-k 10 \
  --skills jikji --json

# Public enterprise-PDF benchmark: EDiTh / Véracier Industries
jikji edith-summary .benchmarks/edith_public --json
jikji edith-suite .benchmarks/edith_public_run \
  --cases 3 --max-docs 42 --top-k 10 \
  --max-download-bytes 2000000000 --json

# Korean public-data messy-folder benchmark for local agents
jikji publicdata-build .benchmarks/publicdata_agent_bench/run_20260529 \
  --target-docs 90 --max-id 700 --cases 40 --json
jikji publicdata-suite .benchmarks/publicdata_agent_bench/run_20260529 \
  --target-docs 90 --max-id 700 --cases 40 --top-k 10 --json
jikji hermes-bench .benchmarks/publicdata_agent_bench/run_20260529/corpus/test \
  --eval-set .benchmarks/publicdata_agent_bench/run_20260529/eval/publicdata_test_eval.jsonl \
  --modes raw,jikji --cases 18 --candidate-top-k 10 \
  --skills jikji --yolo --json


# Hard mixed public-document benchmark: PDF/HWP/HWPX in messy folders
jikji hardbench-suite .benchmarks/hard_mixed_kogl_extreme_20260603_v2 \
  --target-docs 180 --max-data-idx 180 --cases 240 --top-k 10 \
  --difficulty extreme --json
jikji hermes-bench .benchmarks/hard_mixed_kogl_extreme_20260603_v2/corpus/test \
  --eval-set .benchmarks/hard_mixed_kogl_extreme_20260603_v2/eval/hardbench_test_eval.jsonl \
  --modes raw,jikji-fast --cases 4 --candidate-top-k 10 \
  --fast-max-turns 1 --skills jikji --yolo --json

# Local pre-downloaded KOGL Type 1/openable document benchmark
jikji hardbench-suite .benchmarks/local_kogl_extreme_20260603_v1 \
  --source-dir /path/to/public-documents \
  --target-docs 600 --max-file-bytes 26214400 --max-total-bytes 5368709120 \
  --cases 240 --top-k 10 --difficulty extreme --json
jikji hermes-bench .benchmarks/local_kogl_extreme_20260603_v1/corpus/test \
  --eval-set .benchmarks/local_kogl_extreme_20260603_v1/eval/hardbench_test_eval.jsonl \
  --modes raw,jikji-fast,jikji-direct --cases 4 --candidate-top-k 10 \
  --fast-max-turns 1 --skills jikji --yolo --json

# Workspace-Bench-Lite file-discovery adaptation
jikji workspacebench-suite .benchmarks/workspacebench_lite_jikji/run_20260602 \
  --max-tasks 12 --top-k 10 --json
jikji hermes-bench .benchmarks/workspacebench_lite_jikji/run_20260602/corpus \
  --eval-set .benchmarks/workspacebench_lite_jikji/run_20260602/eval/workspacebench_lite_eval.jsonl \
  --modes raw,jikji --cases 6 --candidate-top-k 10 \
  --skills jikji --yolo --json
```

In `hermes-bench`, `raw` means the raw Hermes agent searches original files and
must ignore Jikji. `jikji-fast` is the intended speed comparison mode: Hermes
receives a tiny map-first candidate handoff from prebuilt Jikji search and is
told not to browse the filesystem. `jikji-direct` measures the clearest skill
path: the agent invokes Jikji search and accepts the ranked map candidates
without a separate exploratory Hermes chat turn. `jikji` is a heavier brief-first mode: the actual `jikji brief`
payload is provided up front so Hermes can choose from ranked paths and evidence
instead of spending turns manually browsing `.jikji` indexes. Use `jikji-passive`
only for legacy map-reading diagnostics.

`map` is only the route guide. Full metadata lives in `.jikji/*.jsonl`; extracted parser text lives in `.jikji/doc_text/`.
`prepare` also builds `.jikji/search_index.sqlite`, an Everything-style
precomputed lexical/content/metadata index used by `jikji search` for instant
lookup without changing original files or folders.

## Public benchmark snapshot

Measured through 2026-05-29 on this project workstation. These results use public or
publishable benchmark data only. Private local-folder results are treated as
development diagnostics and are not used as headline evidence.

No embeddings, vector DB, or cloud parsing are used by Jikji indexing/search.

### Actual local-agent comparison — primary evidence

Headline benchmark claims should compare **raw local agent** vs **the same agent with Jikji attached**. The deterministic raw-vs-jikji tables below are only regression diagnostics for the map/search layer, not the main product claim.

Hermes Agent was run on a Korean public-data messy-folder corpus built from 90
public XLSX downloads. The builder attempted the requested Public Data
Portal/KOGL-style workflow but records Seoul Data Hub public XLSX downloads as
the accessible fallback source in `manifest.json`; do not overclaim this run as
verified KOGL Type 1. The corpus is intentionally spreadsheet-heavy.

```text
Dataset                     Agent mode       Cases  Hit@1   Hit@3   Hit@5   Hit@10  Seconds  Avg sec/case
--------------------------  ---------------  -----  ------  ------  ------  ------  -------  ------------
Korean public XLSX messy    raw Hermes          18  0.7778  0.8333  0.8333  0.8333  784.028        43.557
Korean public XLSX messy    Hermes + Jikji      18  0.9444  1.0000  1.0000  1.0000  522.894        29.050
```

Result: Jikji improved Hermes by +0.1666 absolute Hit@5/Hit@10 and about 1.50x
lower elapsed agent time. Evidence lives under
`.benchmarks/publicdata_agent_bench/run_20260529/` when regenerated locally.
Timing and actual-agent results are workstation-, model-, and run-dependent.

Claude Code was run as the local agent on a public HippoCamp subset. `raw` means
Claude Code searched the original files/folders and was instructed not to use
Jikji. `claude+jikji` means Claude Code received `jikji search` candidates and
selected final paths from them.

| Dataset | Agent mode | Cases | Hit@1 | Hit@5 | Hit@10 | Seconds | Avg sec/case |
|---|---:|---:|---:|---:|---:|---:|---:|
| HippoCamp public | raw Claude Code | 6 | 1.0000 | 1.0000 | 1.0000 | 325.979 | 54.330 |
| HippoCamp public | Claude Code + Jikji | 6 | 1.0000 | 1.0000 | 1.0000 | 49.033 | 8.172 |

This small actual-agent run shows equal accuracy with about 6.6x lower elapsed
agent time when Jikji provides the candidate list.

Hermes Agent was also run on a public MIRACL-VISION materialized-document subset
after adding `jikji brief`. `hermes+jikji` received the query-specific brief
(candidate paths, evidence, route order) and could still inspect files if needed.

| Dataset | Agent mode | Cases | Hit@1 | Hit@3 | Hit@5 | Hit@10 | Seconds | Avg sec/case |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| MIRACL-VISION public | raw Hermes | 8 | 0.7500 | 0.8750 | 0.8750 | 0.8750 | 268.381 | 33.548 |
| MIRACL-VISION public | Hermes + Jikji | 8 | 0.7500 | 0.8750 | 0.8750 | 1.0000 | 194.967 | 24.371 |

This small actual-agent run shows equal Hit@5, higher Hit@10, and about 1.38x
lower elapsed agent time. The public corpus/eval are under
`.benchmarks/miracl_vision_public_doc_bench/`; generated run evidence is
reproducible with `jikji hermes-bench ... --modes raw,jikji`.

Hermes was also run on a bounded EDiTh / Véracier Industries enterprise-PDF
subset: 42 public PDFs extracted from the 1.5GB archive, with 3 file-retrieval
questions from the answer key.

| Dataset | Agent mode | Cases | Hit@1 | Hit@3 | Hit@5 | Hit@10 | Seconds | Avg sec/case |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| EDiTh PDF subset | raw Hermes | 3 | 1.0000 | 1.0000 | 1.0000 | 1.0000 | 152.777 | 50.926 |
| EDiTh PDF subset | Hermes + Jikji | 3 | 1.0000 | 1.0000 | 1.0000 | 1.0000 | 120.852 | 40.284 |

This EDiTh run is intentionally small because the public archive is large and
the answer key has only a few explicit file-list retrieval questions, but it is
closer to Jikji's target than Markdown-only corpora: real PDF files,
searchable/scanned/mixed formats, multiple languages, and multi-file answers.


Hard mixed public-document benchmark in `--difficulty extreme` mode: 179 KOGL
public attachments were downloaded and split into train/valid/test. Extreme mode
makes raw-agent search meaningfully harder by using generic filenames, larger
test roots, weak natural-language clues, and decoy memo/link files that contain
matching clues but are not valid answers. The final test set stayed held out.

```text
Mode   Cases  Hit@1   Hit@3   Hit@5   Hit@10  MRR     Sec     Sec/case
-----  -----  ------  ------  ------  ------  ------  ------  --------
raw      144  0.0486  0.0833  0.1042  0.1597  0.0707   6.295    0.0437
Jikji    144  0.6736  0.8472  0.9167  0.9583  0.7826  29.487    0.2048
```

Actual Hermes sample on 4 held-out extreme test cases:

```text
Agent mode           Cases  Hit@1   Hit@3   Hit@5   Hit@10  Seconds  Avg sec/case
-------------------  -----  ------  ------  ------  ------  -------  ------------
raw Hermes               4  0.5000  0.5000  0.5000  0.5000  415.444       103.861
Hermes + Jikji fast      4  1.0000  1.0000  1.0000  1.0000   63.156        15.789
```

Local pre-downloaded KOGL Type 1/openable benchmark: 600 documents sampled from
a local public-document folder, using
generic filenames and decoy memo/link files. Extension mix: 161 PDF, 161 HWP,
162 HWPX, 104 XLSX, 12 DOCX. Split: 270 train / 90 valid / 240 test documents.

```text
Mode   Cases  Hit@1   Hit@3   Hit@5   Hit@10  MRR     Sec      Sec/case
-----  -----  ------  ------  ------  ------  ------  -------  --------
raw      240  0.0250  0.0333  0.0458  0.0583  0.0339   35.189    0.1466
Jikji    240  0.3167  0.5125  0.6125  0.8125  0.4532  207.006    0.8625
```

Actual 4-case sample on this larger local benchmark. `jikji-direct` represents
the clearest intended skill behavior: the agent invokes Jikji's prebuilt
map/search tool and accepts the ranked candidate handoff instead of spending a
new exploratory Hermes chat turn browsing files.

```text
Agent mode              Cases  Hit@1   Hit@3   Hit@5   Hit@10  Seconds  Avg sec/case
----------------------  -----  ------  ------  ------  ------  -------  ------------
raw Hermes                  4  0.5000  0.7500  0.7500  0.7500  366.282        91.571
Hermes + Jikji fast         4  0.2500  0.7500  0.7500  1.0000  157.014        39.254
Hermes + Jikji direct       4  0.2500  0.7500  0.7500  1.0000    3.202         0.800
```

On this slice, direct Jikji handoff preserves the improved Hit@10 while reducing
average discovery latency by about `114x` versus raw Hermes. Across all 240
test cases, the same direct handoff scored Hit@10 `0.8125` at `0.810` seconds
per case.

Workspace-Bench-Lite is relevant to Jikji because it stresses workspace
exploration and task-supporting file discovery. Jikji's adapter does **not**
claim full Workspace-Bench task-completion scoring; it converts each task into
\"find the source/input files needed for this workspace task\".

```text
Dataset                         Agent mode       Cases  Hit@1   Hit@3   Hit@5   Hit@10  Seconds  Avg sec/case
------------------------------  ---------------  -----  ------  ------  ------  ------  -------  ------------
Workspace-Bench-Lite file find  raw Hermes           6  1.0000  1.0000  1.0000  1.0000  249.454        41.576
Workspace-Bench-Lite file find  Hermes + Jikji       6  0.8333  1.0000  1.0000  1.0000  203.742        33.957
```

Interpretation: on this small actual-agent slice, raw Hermes already found a
required source file for every case, while Jikji preserved Hit@5/Hit@10 and cut
elapsed time by about 1.22x.

### Public deterministic retrieval suites — secondary diagnostics

The deterministic harness is not a replacement for a raw-agent vs Jikji-agent benchmark; it is a
wide regression test for the Jikji map/search layer. `raw` is a map-free lexical
candidate baseline over the same public local files; `jikji` uses
`.jikji/search_index.sqlite` plus Jikji map cards.

BEIR public suite materialized each corpus document as a local Markdown file:
SciFact, NFCorpus, and ArguAna, 200 qrel-backed queries each.

Korean public-data messy-folder deterministic diagnostic:

```text
Mode                      Cases  Hit@1   Hit@3   Hit@5   Hit@10  MRR     Seconds
------------------------  -----  ------  ------  ------  ------  ------  -------
raw lexical diagnostic       18  0.7222  0.8889  0.8889  0.9444  0.8111    0.123
Jikji index diagnostic       18  0.9444  1.0000  1.0000  1.0000  0.9722    0.423
```

Workspace-Bench-Lite file-discovery diagnostic:

```text
Mode                      Cases  Hit@1   Hit@3   Hit@5   Hit@10  SetR@5  SetR@10  MRR     Seconds
------------------------  -----  ------  ------  ------  ------  ------  -------  ------  -------
raw lexical diagnostic       12  0.4167  0.6667  0.7500  0.8333  0.5222   0.6861  0.5687    0.115
Jikji index diagnostic       12  0.5833  0.7500  0.9167  0.9167  0.6028   0.6944  0.6764    0.452
```

| Dataset | Mode | Cases | Hit@1 | Hit@3 | Hit@5 | Hit@10 | MRR | Seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| BEIR 3 datasets | raw lexical diagnostic | 600 | 0.2200 | 0.3217 | 0.3783 | 0.4500 | 0.2874 | 295.939 |
| BEIR 3 datasets | Jikji index diagnostic | 600 | 0.2283 | 0.4350 | 0.5033 | 0.5967 | 0.3485 | 264.868 |

Per-dataset BEIR results:

| Dataset | Docs | Cases | Mode | Hit@1 | Hit@5 | Hit@10 | MRR |
|---|---:|---:|---:|---:|---:|---:|---:|
| SciFact | 5,183 | 200 | raw lexical diagnostic | 0.3500 | 0.5550 | 0.6400 | 0.4344 |
| SciFact | 5,183 | 200 | Jikji index diagnostic | 0.3350 | 0.5400 | 0.6100 | 0.4205 |
| NFCorpus | 3,633 | 200 | raw lexical diagnostic | 0.3100 | 0.5100 | 0.5800 | 0.3940 |
| NFCorpus | 3,633 | 200 | Jikji index diagnostic | 0.3500 | 0.5750 | 0.6350 | 0.4507 |
| ArguAna | 8,674 | 200 | raw lexical diagnostic | 0.0000 | 0.0700 | 0.1300 | 0.0338 |
| ArguAna | 8,674 | 200 | Jikji index diagnostic | 0.0000 | 0.3950 | 0.5450 | 0.1743 |

HippoCamp public no-leak deterministic check:

| Dataset | Mode | Cases | Hit@1 | Hit@5 | Hit@10 | MRR |
|---|---:|---:|---:|---:|---:|---:|
| HippoCamp public | raw lexical diagnostic | 18 | 0.6667 | 0.7778 | 0.8889 | 0.7238 |
| HippoCamp public | Jikji index diagnostic | 18 | 0.6111 | 0.8333 | 0.9444 | 0.6935 |

MIRACL-VISION public multilingual document-file check after CJK-aware indexing
and `brief` support:

| Dataset | Mode | Cases | Hit@1 | Hit@3 | Hit@5 | Hit@10 | MRR | Seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| MIRACL-VISION ko/en/ja/fr | raw lexical diagnostic | 80 | 0.6125 | 0.7250 | 0.7875 | 0.8875 | 0.6962 | 13.760 |
| MIRACL-VISION ko/en/ja/fr | Jikji index diagnostic | 80 | 0.6875 | 0.9000 | 0.9250 | 0.9750 | 0.7903 | 7.421 |

EDiTh / Véracier Industries bounded enterprise-PDF check:

| Dataset | Mode | Cases | Hit@1 | Hit@3 | Hit@5 | Hit@10 | SetR@5 | MRR | Seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| EDiTh PDF subset | raw lexical diagnostic | 3 | 0.3333 | 0.6667 | 0.6667 | 0.6667 | 0.4000 | 0.4444 | 0.009 |
| EDiTh PDF subset | Jikji index diagnostic | 3 | 0.3333 | 1.0000 | 1.0000 | 1.0000 | 0.6667 | 0.6667 | 0.024 |

Reproducible commands:

```bash
jikji beir-suite .benchmarks/public_beir \
  --datasets scifact,nfcorpus,arguana \
  --cases 200 --top-k 10 --json

jikji bench-run .benchmarks/hippocamp-large/Adam_Subset \
  --eval-set .benchmarks/hippocamp_eval_set_220_noleak.jsonl \
  --modes raw,jikji --top-k 10 --json

jikji edith-suite .benchmarks/edith_public_run \
  --cases 3 --max-docs 42 --top-k 10 \
  --max-download-bytes 2000000000 --json
```

Validation commands for this snapshot:

```bash
.venv/bin/ruff check src tests
.venv/bin/pytest -q
.venv/bin/python -m compileall -q src tests
```

Result on 2026-06-02 after the public benchmark additions: `ruff` passed,
`pytest` passed with 44 tests, and `compileall` passed.

## Content extraction coverage

Jikji caches searchable text for common local-agent discovery targets:

- Documents: PDF, DOC/DOCX, PPT/PPTX/PPS/PPSX, XLS/XLSX, HWP/HWPX, ODT, RTF, EPUB.
- Structured local files: EML email, ICS calendar, SQLite/DB files.
- Text/config/web data: TXT/MD/CSV/TSV/LOG, HTML, JSON/JSONL, XML, YAML, INI/CFG, TOML.
- Archives: ZIP/JAR/WAR/TAR/TGZ/TBZ/TXZ plus 7Z/RAR member-name listing when local `7z` exists.
- Media: image OCR when local `tesseract` exists; audio metadata via local `ffprobe`; optional local Whisper transcription with `JIKJI_ENABLE_TRANSCRIPTION=1`.
- Scanned/odd PDFs: Poppler `pdftotext` fallback, then first-page OCR when local `pdftoppm` + `tesseract` exist.

No parser uploads content. Missing optional tools degrade to filename/metadata search rather than failing indexing.

## Safety and privacy

- Jikji is non-destructive: original files and folders are not moved, renamed, or deleted.
- Jikji only prepares explicit paths supplied to `prepare`/`refresh`; it does not auto-scan all drives.
- Defaults skip hidden files and safety-sensitive names such as `.env`, private keys, certificate files, `.git`, `node_modules`, and virtualenv/cache folders.
- `.jikji/doc_text/` may contain sensitive extracted document text. Review before committing generated artifacts.

Recommended `.gitignore` for indexed Git roots:

```gitignore
.jikji/
000_JIKJI_AGENT_MAP.md
```

## Local development

```bash
python3 -m venv .venv
.venv/bin/pip install -e .
.venv/bin/pip install pytest ruff
.venv/bin/ruff check src tests
.venv/bin/pytest -q
.venv/bin/python -m compileall -q src tests
```

## Standards and skill template

- Local-agent search standard: `docs/local-agent-search-standard.md`
- Schema reference: `docs/schema.md`
- Agent usage: `docs/agent-usage.md`
- HippoCamp benchmark adapter: `docs/hippocamp-benchmark.md`
- Public benchmark catalog: `docs/public-benchmark-catalog.md`
- Korean public-data agent benchmark: `docs/publicdata-agent-benchmark.md`
- Workspace-Bench-Lite adapter: `docs/workspacebench-benchmark.md`
- Hard mixed public-document benchmark: `docs/hardbench-benchmark.md`
- Generic skill template: `skills/jikji/SKILL.md`

Jikji is separate from Folder1004:

- **Folder1004**: GUI software for reorganizing messy Desktop/Downloads folders for people.
- **Jikji**: CLI/agent skill for non-destructive local document knowledge maps for agents.
