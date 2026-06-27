# Rust Cross-OS Single-Binary Port — Feasibility Decision

Investigation of GitHub issue #11: evaluate porting Jikji's public surface
(`jikji prepare` / `jikji refresh` / `jikji find --json`) to a single Rust
binary shipped per OS/arch, dropping the required Python runtime.

## Decision: PARTIAL GO (phased)

A Rust port of the **deterministic default core** is feasible and worth doing for
the distribution win. The optional **media (OCR/ASR)** backends should stay a
later opt-in phase, and **legacy OLE/CFB** formats are a best-effort scrape in
Rust just as they already are in Python. The Python package stays the source of
truth and remains supported until the Rust binary reaches byte-level `find
--json` parity on a shared corpus across Mac/Linux/Windows.

This matches the issue's own proposed sequencing (lock the contract → port the
deterministic core → defer media → keep Python until parity).

## What "core" means here (grounded in the current code)

The default index requires no embeddings, no LLM, and no cloud (README, "Why It
Saves Calls"). The deterministic pipeline that must port is:

1. **Scan** — `src/jikji/scanner.py`: walk the root, collect path/size/mtime;
   the content-free `source_tree_signature` (relative path + size + `mtime_ns`
   only; see `docs/schema.md`) is then computed in `agent_index.py`
   (`_tree_signature_from_paths`).
2. **Parse** — `src/jikji/parsers/*`: extension-dispatched text extraction
   (`registry.py` `SUPPORTED_EXTENSIONS`) into `.jikji/doc_text/`.
3. **Index** — `src/jikji/search_index.py` + `agent_index.py`: fielded BM25 over
   `path`, `name`, `ext`, `body`, `meta`, `semantic`, persisted to
   `.jikji/search_index.sqlite` (rebuildable, not source of truth).
4. **Graph / wiki** — `llm_wiki.py`, `graph_query.py`: deterministic local wiki
   pages + `knowledge_graph.json` + `graph_routes.jsonl`.
5. **Slate** — `discover.py` + `answer_pack.py`: multi-query, multi-route
   candidate merge → the `find` JSON envelope.

All five stages are pure file/CPU work with no network dependency, so they are
portable to Rust.

## Rust dependency mapping (core)

| Concern | Python today | Rust path | Risk |
| --- | --- | --- | --- |
| Filesystem walk / stat | stdlib | `walkdir`, `std::fs` | low |
| Hashing | hashlib sha256 | `sha2` | low |
| PDF text | `pypdf` | `pdf-extract` / `lopdf` | medium (text-layout fidelity) |
| DOCX/PPTX/XLSX (OOXML) | `python-docx`, `python-pptx`, `openpyxl` | `zip` + `quick-xml`; `calamine` for spreadsheets | low/medium |
| ODT/ODS/ODP, EPUB, HWPX | zip+XML in `parsers/` | `zip` + `quick-xml` | low |
| Text/MD/CSV/TSV/log/SRT/VTT/HTML/JSON/YAML/INI/TOML | stdlib | trivial (`scraper`/`quick-xml` for HTML) | low |
| EML / ICS | stdlib | `mailparse` / line parse | low |
| Archive member names | stdlib (no decompression) | `zip`, `tar` (names only) | low |
| SQLite read of `.sqlite`/`.db` sources | stdlib sqlite3 | `rusqlite` | low |
| Fielded BM25 index | custom (`search_index.py`) | `tantivy` **or** a faithful re-impl of the existing scoring | medium — see Contract note |
| Output index store | `.jikji/search_index.sqlite` | `rusqlite` (keep same schema) | low |
| JSON envelope | stdlib json | `serde_json` (preserve key order/shape) | low |

### BM25 / ranking caveat
The current fielded scorer (`field_idf`, `field_avg`, `field_lengths` in
`search_index.sqlite`) is bespoke. `tantivy` is a drop-in *capability* match but
not a scoring match. For byte-compatible `find` output, the Rust side must
re-implement the existing tokenizer + BM25 normalization rather than adopt
tantivy's defaults. This is the single largest correctness risk and the main
reason the recommendation is "partial / phased," not "full go now."

## OLE / CFB legacy formats — explicit scope

Legacy binary formats are reached today through `olefile`:

- `parsers/office.py::parse_legacy_office` — `.doc`, `.ppt`/`.pps`, `.xls`: opens
  the OLE compound storage and scrapes long printable runs from
  format-specific streams (`WordDocument`, `PowerPoint Document`, workbook
  streams). It is explicitly *best-effort*, not a full format decode.
- `parsers/hwp.py::parse_hwp` — HWP 5.x binary: reads `BodyText` streams via
  `olefile`, optional zlib inflate, heuristic Korean/ASCII recovery. Also
  best-effort.

**Rust path:** the `cfb` crate provides equivalent compound-file stream access;
`flate2` covers the zlib inflate for compressed HWP body streams. Because both
Python paths are heuristic printable-run scrapes (no faithful format parsing),
the Rust port only has to reproduce *the same heuristics over the same streams*,
which is tractable. Parity here should be measured as "extracts comparable text
for ranking," not character-identical body text — these are inputs to the index,
not part of the `find` JSON contract.

**Scope verdict:** PORTABLE, best-effort, low blocking risk. Cover `.doc`,
`.ppt`/`.pps`, `.xls`, and binary `.hwp` via `cfb` + `flate2` in the same
"recover printable runs" manner. Treat `.hwp` recovery quality as a known
fuzzy area in both implementations.

## Media (OCR / ASR) — explicit scope

Already optional and runtime-detected (`pyproject.toml` extras `ocr`,
`transcribe`, `media`; `parsers/media.py` lazy-imports `rapidocr` and
`faster_whisper`, falling back to system `tesseract` / `whisper` binaries).

**Rust path:** `ort` (ONNX Runtime bindings) reproduces RapidOCR; `whisper-rs`
reproduces faster-whisper-style ASR. Both pull heavy native/model dependencies
and would inflate the "single static binary" story.

**Scope verdict:** DEFER. Keep media behind the existing opt-in flags
(`--enable-media-index`). Phase 1 of the Rust binary ships with media disabled;
when media is requested it can either (a) shell out to system `tesseract` /
`whisper` exactly like the current fallback, or (b) be a later, larger,
optionally-downloaded build. Media output is not part of the deterministic
parity target.

## Output contract parity — the gating requirement

`find --json` returns a large fixed-key envelope assembled in
`discover.py` (the final `return {…}` includes `handoff_action`,
`handoff_policy`, `answer_paths`, `supporting_paths`, `paths`, `candidates`,
`evidence_pack`, `agent_should_not_rerank`, `confidence*`, `query_variants`,
`llm_search_plan`, `search_plan`, budgets, etc.). Downstream agent integrations
and the SKILL adapter depend on this shape (`docs/agent-usage.md`).

Parity strategy (issue step 1):
1. Freeze a shared corpus + query set.
2. Capture Python `prepare` + `find --json` output as golden files.
3. The Rust binary must reproduce the `find` JSON envelope key-for-key and
   value-for-value (ordering of `candidates` / `answer_paths` included, because
   `agent_should_not_rerank` instructs agents to preserve order).
4. `manifest.json` / `file_index.jsonl` / index artifacts (`docs/schema.md`)
   should match field-for-field; content-derived freshness
   (`source_tree_signature`) must match exactly since it is defined purely over
   path+size+`mtime_ns`.

The ranking re-implementation (above) is what makes value-for-value parity
non-trivial; golden tests are the only safe acceptance gate.

## Recommended phasing

- **Phase 0 (contract lock):** add golden-file capture for `find --json` +
  `manifest.json` on a checked-in mini corpus. Pure Python work; de-risks the
  port and is independently useful for regression testing. *(Not done in this
  change — see "Next step".)*
- **Phase 1 (core port):** Rust scan + native-text/OOXML/zip-XML parsing +
  fielded BM25 re-impl + graph/wiki + slate; media disabled; OLE best-effort via
  `cfb`. Ship per-OS/arch binaries with SHA-256 release assets (MinSync model).
  Gate on golden parity across Mac/Linux/Windows.
- **Phase 2 (media):** opt-in OCR/ASR via `ort` / `whisper-rs` or system-binary
  shell-out, behind the existing flags.
- **Throughout:** the Python package stays the supported reference until Phase 1
  passes parity.

## Next step (smallest actionable follow-up)

Implement Phase 0 contract-lock golden tests against the existing Python
output before any Rust code is written. That is the lowest-risk, highest-leverage
unit of work and the prerequisite the issue itself names first.
