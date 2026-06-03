# Jikji Local Agent Search Standard

Status: Draft v0.2 baseline  
Scope: Jikji CLI, generated `.jikji/` artifacts, and local-agent skill usage

## 1. Purpose

Jikji is a non-destructive local discovery layer for AI coding/desktop agents.
A local agent such as Claude Code, Codex, Hermes, OpenCode/OpenClone, or another
CLI-capable assistant should be able to install Jikji from a GitHub checkout or
Python package, run it on an explicitly supplied local folder/drive, and then use
standard command-line tools (`rg`, `jq`, `cat`, shell globbing) to discover files,
folders, metadata, and extracted document text.

Jikji does **not** replace an agent's permissions. It only prepares indexes for
paths the user or agent already has permission to read.

## 2. Product Boundary

Jikji must:

- create/update agent-readable artifacts under `.jikji/`;
- create/update a root signpost named `000_JIKJI_AGENT_MAP.md`;
- preserve original user file paths and folder layout;
- work through explicit CLI commands and generated skill context;
- prefer deterministic text-first, grep-friendly outputs.

Jikji must not:

- move, rename, delete, or reorganize original user files;
- auto-scan every drive without an explicit user-supplied target path;
- hide parser failures from agents;
- require cloud services, LLM calls, or network APIs for local search preparation;
- merge with Folder1004's physical file-organization role.

## 3. Core CLI Contract

The stable commands are:

```bash
jikji search /path/to/folder "query" [--top-k N] [--json]
jikji agent-skill-install [--agent NAME|all] [--prepare-root PATH] [--foreground-prepare] [--no-prepare] [--json]
jikji skill-export [--dest /agent/skills/jikji/SKILL.md] [--json]
jikji prepare /path/to/folder [--json]
jikji refresh /path/to/folder [--json]   # alias for prepare
jikji map /path/to/folder
jikji doctor /path/to/folder [--json]
jikji eval-generate /path/to/folder [--cases N] [--json]
jikji eval /path/to/folder [--top-k N] [--json]
```

`search` is the primary local-agent entry point. It may auto-prepare a missing
instant index for the explicit root, use a stale-but-present index for immediate
results, and launch a background refresh unless disabled. Manual
`prepare`/`refresh` are administrative controls, not something users should need
to run before every lookup.

Important prepare/refresh options:

- `--max-files N` — refuse unexpectedly huge scans.
- `--include-hidden` — include hidden dotfiles except safety-denied names.
- `--include-sensitive` — explicitly include safety-denied names such as `.env`,
  private keys, and certificate material.
- `--exclude PATTERN` — additional fnmatch exclude; repeatable.
- `--max-hash-bytes N` — skip SHA256/cache identity for files larger than N.
- `--doc-text-max-chars N` — cap extracted parser text per document.
- `--doc-text-chunk-chars N` — chunk large extracted text caches.

Defaults must remain safe and non-destructive.

## 4. Generated Artifact Contract

### 4.1 Root signpost

`000_JIKJI_AGENT_MAP.md` is intentionally short. It is a visible entry point for
humans and agents, not a dump of every file or every document body.

### 4.2 `.jikji/agent_map.md`

`.jikji/agent_map.md` is a concise route map. It should include generation time,
schema version, root path, counts, parser error count, representative folders,
representative document cache candidates, and exact search instructions.

### 4.3 Route and guide documents

- `.jikji/agent_routes.md` — step-by-step route for local agents.
- `.jikji/agent_skill_context.md` — compact context a local agent skill can read.
- `.jikji/human_guide.md` — human-facing safety and privacy note.
- `.jikji/eval/` — generated local evaluation sets, corpus profiles, and reports.

These are generated artifacts and may be regenerated.

### 4.4 JSONL indexes

Jikji's machine-readable indexes are newline-delimited JSON so agents can stream,
filter, and grep large folders.

Required indexes:

- `.jikji/file_index.jsonl` — one row per present file plus visible deleted rows
  from the previous index generation.
- `.jikji/folder_index.jsonl` — one row per discovered folder, including root.
- `.jikji/document_index.jsonl` — one row per parser-required document.
- `.jikji/parse_errors.jsonl` — non-fatal parser/metadata/hash failures.
- `.jikji/file_cards.jsonl` — one row per file with map-facing lexical,
  structural, duplicate, and filename lookup hints.
- `.jikji/chunk_map.jsonl` — bounded per-document/per-text chunk clues for
  map-only content discovery.
- `.jikji/search_index.sqlite` — Everything-style prebuilt lexical search
  accelerator generated from the JSONL map; it is disposable and may be
  regenerated from the JSONL artifacts.
- `.jikji/duplicate_map.jsonl` — duplicate/hash/family groups for copy-aware
  hit@k evaluation and agent navigation.

`keywords` and `summary` fields are local deterministic heuristics only. They
must never require LLM calls, cloud services, or network access.

### 4.5 Document text cache

`.jikji/doc_text/` stores extracted text for parser-required documents. Caches
may be capped or chunked. Extraction may fail for encrypted, malformed,
oversized, or unsupported files; failures must be visible in `parse_errors.jsonl`,
`document_index.jsonl`, and summary counts.

Parser-required extensions in v0.2:

```text
.pdf .doc .docx .ppt .pptx .pps .ppsx .xls .xlsx .hwp .hwpx .odt .rtf
```

Native text-like files such as `.txt`, `.md`, `.csv`, `.json`, `.yaml`, and logs
are not required to be copied into `.jikji/doc_text/`. Agents should search those
files in their original locations and search parser-required document bodies in
`.jikji/doc_text/`.

### 4.6 Rich metadata

Rich metadata that does not belong in the flat file index should live in
`.jikji/doc_meta/sha256_<hash>.json`. v0.2 uses a minimal envelope with empty
values allowed and free-form flat dictionaries for `exif`, `office`, and
`parser`.

## 5. `.jikji/` Ownership and Safety Contract

Jikji owns and may regenerate only the paths listed in `manifest.json` under
`owned_paths`. Retired generated artifacts listed under `retired_cleanup_paths`
may be removed by prepare for compatibility cleanup. Future generated artifacts
must be added there before implementation. Jikji must not delete arbitrary
user-created files under `.jikji/`.

Generated stale-pruning is limited to `sha256_*` artifacts under generated cache
folders.

## 6. Cache Key, Lock, and Refresh Policy

The cache key for document text is `sha256:<content-hash>`.

Refresh policy:

- Use `(size, mtime_ns)` to detect unchanged files.
- Do not recompute SHA256 for unchanged files.
- Recompute SHA256 and parser caches only when size or mtime changes, or when a
  previous cache is missing.
- Keep deleted rows visible with `status: "deleted"`.
- Prune only generated `sha256_*` doc caches/cards that are no longer referenced
  by current document rows.

Write policy:

- Use atomic tmp-file replacement for generated files.
- Use `.jikji/.lock` to prevent overlapping write phases for the same root.
- Record `cache_key_policy` and `owned_paths` in `manifest.json`.

## 7. Local Agent Search Procedure

A local agent using Jikji should follow this sequence:

1. Run `jikji brief ROOT "query" --json` for a compact query-specific route
   sheet. Use its candidate paths first when evidence/reasons match.
2. Run `jikji search ROOT "query" --json` when only ranked candidates are
   needed or when refining the query.
3. Read `000_JIKJI_AGENT_MAP.md`.
4. Read `.jikji/agent_map.md` and `.jikji/agent_routes.md`.
5. Query `.jikji/file_index.jsonl`, `.jikji/folder_index.jsonl`, and
   `.jikji/document_index.jsonl` with `rg`/`jq`.
6. Search parser-required document bodies in `.jikji/doc_text/`.
7. Search native text-like files in their original locations, excluding `.jikji`.
8. Open the original file only through the `path` field after finding a match.
9. Never move, rename, delete, or reorganize source files as part of search.

Example:

```bash
jikji brief . "contract pdf from last spring" --top-k 10 --json
rg "contract|계약" .jikji/doc_text .jikji/*.jsonl
jq -r 'select(.ext==".pdf") | [.path, .text_cache_path] | @tsv' .jikji/document_index.jsonl
rg "TODO|회의" . --glob '!**/.jikji/**'
```

## 8. Multi-root / Local-drive Guidance

Jikji can be run against any folder or mounted drive path that the caller can
read. To prepare multiple drives or roots, run `jikji prepare` once per explicit
root. Jikji must not silently enumerate all system drives by default.

Nested explicit roots are allowed but independent: each root owns only its own
`.jikji/` artifacts, and scanner ignores nested `.jikji/` folders.

## 9. Privacy and Security

- `.jikji/doc_text/` can contain sensitive extracted text.
- `.jikji/*.jsonl` can contain file names, paths, timestamps, and hashes.
- Defaults skip hidden files and safety-denied names such as `.env`, private
  keys, certificate material, `.git`, `node_modules`, and virtualenv/cache dirs.
- Users should review before committing `.jikji/` or `000_JIKJI_AGENT_MAP.md` to
  Git.
- Recommended `.gitignore`:

```gitignore
.jikji/
000_JIKJI_AGENT_MAP.md
```

- Jikji does not bypass filesystem permissions.
- Symlinks are not followed by default.
- Permission and parser errors are recorded but should not abort the whole index.

## 10. Doctor Verification Checklist

`jikji doctor ROOT` verifies:

- required artifacts exist;
- `manifest.json` parses and has supported `schema_version`;
- JSONL rows parse as JSON objects;
- successful document rows have existing `text_cache_path` targets;
- `doc_meta_path` targets are present or warned;
- dangling generated `sha256_*` cache artifacts are warned;
- `non_destructive` is true.

Exit codes:

- `0`: no errors or warnings;
- `1`: missing/broken required artifact or schema error;
- `2`: warnings only.

## 11. P0 Acceptance Criteria

A Jikji build satisfies the local-agent search baseline when:

- `jikji prepare ROOT --json` creates `.jikji/` and `000_JIKJI_AGENT_MAP.md`.
- Original files and folders remain unchanged.
- `jikji map ROOT` prints a route to the full indexes.
- `jikji doctor ROOT` verifies required artifacts and cache references.
- `file_index.jsonl` contains all scanned file names and core metadata.
- `folder_index.jsonl` contains all scanned folders.
- `document_index.jsonl` links parser-required documents to caches or visible
  failure/empty statuses.
- `.jikji/doc_text/` contains reusable extracted text where parsing succeeds.
- `agent_routes.md` tells agents exactly where to search native text vs parser
  cache text.
- Validation passes: `ruff`, `pytest`, and `compileall`.


## 12. Local Search Evaluation

Jikji should be evaluated against the actual folder/file characteristics it is
preparing. `jikji eval-generate ROOT` reads the current Jikji index and creates a
deterministic local eval set under `.jikji/eval/eval_set.jsonl`.

Scenario families:

- `filename_exact` — find a file by its complete file name.
- `filename_partial` — find a file by a distinctive file-name token.
- `lexical_content` — find a file by exact tokens from native text or parsed
  document cache text.
- `semantic_description` — find a file from a natural-language description built
  from local content/folder/name signals; no LLM or embeddings are required.
- `file_description` — find a file from folder, extension, and descriptive name
  clues.

`jikji eval ROOT` runs an agent-like deterministic ranker over `.jikji` indexes,
`.jikji/doc_text/`, and native text files, then writes `.jikji/eval/eval_report.json`.
Metrics include hit@1, hit@3, hit@5, MRR, and per-scenario scores. These metrics
are intended to compare map/index changes over time, not to claim perfect
semantic retrieval.
