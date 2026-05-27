# Jikji

Jikji makes local files legible to AI agents without moving, renaming, or deleting the user's files.

```bash
jikji search ~/Documents "contract pdf from last spring" --json
```

`search` is the normal entry point: it auto-prepares an explicit root when no
instant index exists, returns existing results immediately when the index is
stale, and can refresh in the background. `prepare`, `refresh`, `map`, and
`doctor` remain manual/admin commands. `clean` removes Jikji-generated artifacts
from one prepared root when you want to leave no trace.

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
search result is empty or clearly insufficient.

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
```

In `hermes-bench`, `jikji` is a tool-first mode: Jikji search candidates are
provided up front so Hermes can choose from ranked paths instead of spending
turns manually browsing `.jikji` indexes. Use `jikji-passive` only for legacy
map-reading diagnostics.

`map` is only the route guide. Full metadata lives in `.jikji/*.jsonl`; extracted parser text lives in `.jikji/doc_text/`.
`prepare` also builds `.jikji/search_index.sqlite`, an Everything-style
precomputed lexical/content/metadata index used by `jikji search` for instant
lookup without changing original files or folders.

## Public benchmark snapshot

Measured on 2026-05-27 on this project workstation. These results use public or
publishable benchmark data only. Private local-folder results are treated as
development diagnostics and are not used as headline evidence.

No embeddings, vector DB, or cloud parsing are used by Jikji indexing/search.

### Actual local-agent comparison

Claude Code was run as the local agent on a public HippoCamp subset. `raw` means
Claude Code searched the original files/folders and was instructed not to use
Jikji. `claude+jikji` means Claude Code received `jikji search` candidates and
selected final paths from them.

```text
Dataset              Agent mode     Cases  Hit@1   Hit@5   Hit@10  Seconds  Avg sec/case
HippoCamp public     raw            6      1.0000  1.0000  1.0000  325.979  54.330
HippoCamp public     claude+jikji   6      1.0000  1.0000  1.0000   49.033   8.172
```

This small actual-agent run shows equal accuracy with about 6.6x lower elapsed
agent time when Jikji provides the candidate list.

### Public deterministic retrieval suites

The deterministic harness is not a replacement for an agent benchmark; it is a
wide regression test for the Jikji map/search layer. `raw` is a map-free lexical
candidate baseline over the same public local files; `jikji` uses
`.jikji/search_index.sqlite` plus Jikji map cards.

BEIR public suite materialized each corpus document as a local Markdown file:
SciFact, NFCorpus, and ArguAna, 200 qrel-backed queries each.

```text
Dataset            Mode   Cases  Hit@1   Hit@3   Hit@5   Hit@10  MRR     Seconds
BEIR 3 datasets    raw    600    0.2200  0.3217  0.3783  0.4500  0.2874  295.939
BEIR 3 datasets    jikji  600    0.2283  0.4350  0.5033  0.5967  0.3485  264.868
```

Per-dataset BEIR results:

```text
Dataset   Docs   Cases  Mode   Hit@1   Hit@5   Hit@10  MRR
SciFact   5,183  200    raw    0.3500  0.5550  0.6400  0.4344
SciFact   5,183  200    jikji  0.3350  0.5400  0.6100  0.4205
NFCorpus  3,633  200    raw    0.3100  0.5100  0.5800  0.3940
NFCorpus  3,633  200    jikji  0.3500  0.5750  0.6350  0.4507
ArguAna   8,674  200    raw    0.0000  0.0700  0.1300  0.0338
ArguAna   8,674  200    jikji  0.0000  0.3950  0.5450  0.1743
```

HippoCamp public no-leak deterministic check:

```text
Dataset           Mode   Cases  Hit@1   Hit@5   Hit@10  MRR
HippoCamp public  raw    18     0.6667  0.7778  0.8889  0.7238
HippoCamp public  jikji  18     0.6111  0.8333  0.9444  0.6935
```

Reproducible commands:

```bash
jikji beir-suite .benchmarks/public_beir \
  --datasets scifact,nfcorpus,arguana \
  --cases 200 --top-k 10 --json

jikji bench-run .benchmarks/hippocamp-large/Adam_Subset \
  --eval-set .benchmarks/hippocamp_eval_set_220_noleak.jsonl \
  --modes raw,jikji --top-k 10 --json
```

Validation commands for this snapshot:

```bash
.venv/bin/ruff check src tests
.venv/bin/pytest -q
.venv/bin/python -m compileall -q src tests
```

Result: `ruff` passed, `pytest` passed with 31 tests, and `compileall` passed.

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
- Generic skill template: `skills/jikji/SKILL.md`

Jikji is separate from Folder1004:

- **Folder1004**: GUI software for reorganizing messy Desktop/Downloads folders for people.
- **Jikji**: CLI/agent skill for non-destructive local document knowledge maps for agents.
