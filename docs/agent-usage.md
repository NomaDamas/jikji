# Agent Usage

Jikji is only a local-file discovery and indexed document-content search tool.
Call it only when all three conditions hold: the task needs local files/folders
or evidence inside local documents; the target paths are unknown or a bounded
corpus must be searched; and a specific local root is supplied or reasonably inferred.

## Invocation gate — before choosing a tool

- Use Jikji: find a lost invoice in Downloads; locate the folder containing a
  project; find local HWP documents mentioning a contract clause.
- Do not use Jikji for general questions, explanations, drafting, translation of
  supplied text, web research, or coding advice without a local discovery need.
- Do not rediscover a known file to read, edit, compare, or inspect it. Use the
  file reader/editor/stat tool directly, including paths returned by Jikji.
- Do not substitute Jikji for shell commands, process/port/disk diagnostics,
  Git, builds, tests, dependency installation, directory listings, or LSP symbol
  navigation. Use the relevant tool directly; no prerequisite Jikji call.
- A root path, a working directory, or words such as "file", "find", "check",
  and "search" alone do not establish discovery intent.
- In mixed tasks, use Jikji only for the missing-path discovery subtask. Its
  retry/stop rules do not restrict subsequent reading, editing, execution, or tests.
- Explicit Jikji installation, indexing, diagnostics, and cleanup requests are
  administrative tasks, not discovery; use the appropriate admin command without
  running `find` first. Do not auto-install or index for unrelated requests.

If the gate fails, skip Jikji entirely. Do not turn a general question into a
local search just because this skill is installed.

## First Command

```bash
jikji find /path/to/folder "natural language file clue" --json
```

This is the public agent interface. It combines Jikji metadata, file maps, parser
caches, graph routes, and local search routes into one candidate slate so the
agent can verify top evidence instead of crawling blindly.

`jikji find` is read-only with respect to indexing. It searches prepared indexes
only. If it reports that no Jikji search index exists for the requested root,
tell the user that this range is not prepared yet and ask before running:

```bash
jikji prepare /path/to/folder --json
```

After agent skill installation, Jikji may queue a background prepare for common
user material folders and document-heavy folders under the user's home
directory. That is the expected fast-start path; it is separate from `find`.

## JSON Contract

- `answer_paths[]`: primary ordered answer paths.
- `paths[]`: public path list for simple return.
- `candidates[]`: merged top-k slate.
- `evidence_pack[]`: compact evidence and next-read hints.
- `handoff_action=direct_use`: accept the payload; no broad filesystem crawl.
- `handoff_action=jikji_retry`: run exactly one sharper `jikji find` retry.
- `handoff_action=raw_fallback_after_retry`: raw search is allowed only after the
  retry failed, stayed empty, or stayed clearly wrong.
- `agent_should_not_rerank=true`: preserve Jikji order.
- `request_plan.intent`: `files_by_name_or_text`, `code_identifier`, `exact_text`,
  `non_text_files` or `empty_query`; `request_plan.use_instead` (grep or file read) accompanies
  `exact_text`; `request_plan.not_searched` lists what the request could not look at.
- `results[]`: per returned path, `file_kind`, `text_indexed`, `matched_in`
  (`name_path`/`text`), `matched_words` (which of `request_plan.query_words` the file contains)
  and `snippet` (the passage around the match, first 20 results).
- `scan_incomplete` (field and note): the whole-index body scan stopped at its time limit
  (`JIKJI_FALLBACK_SCAN_BUDGET_MS`, default 4000); results may be missing. `freshness_not_checked`
  is reported at once for trees too large to compare in the freshness budget
  (`JIKJI_FRESHNESS_BUDGET_MS`, default 2000, about 20000 files per second).
- `--brief` (with `--json`): drops internal ranking diagnostics (`strategy_results`,
  `llm_judge_input`, `judge_candidate_slate`, `search_plan`, `llm_search_plan`, ...); keeps paths,
  `results`, `request_plan`, `notes`, index status and the retry contract. `next_commands[]` are
  shell-quoted and run as written.
- `cloud_content` (cloud-mounted folders only): `mode` is `filename_only`, `approved_folder` or `indexed_before_policy` (text kept from an older index);
  note `cloud_filename_only` means contents of that folder are not indexed. Approve one folder
  with `jikji prepare FOLDER --approve-cloud-content` (ask the user first); withdraw with
  `--revoke-cloud-content`.
- `notes[]` codes: `pattern_syntax_not_supported`, `identifier_files_only`,
  `non_text_name_match_only`, `non_text_content_not_searchable`, `freshness_not_checked`,
  `searched_enclosing_index`, `no_match_in_index`, `scope_has_no_indexed_files`,
  `path_not_found`, `empty_query`. `coverage` (empty results) counts indexed files by kind.

## Do Not Crawl First

Only after the invocation gate passes, prefer Jikji over a blind discovery crawl.
The retry and fallback contract governs that discovery subtask only; it is not
a global ban on file reads, shell commands, or other tools.

## Project-Local Routing Rules

`jikji prepare` writes a clearly-delimited routing block into the project-local
agent rule files at the prepared root so coding agents discover Jikji without any
extra configuration:

- `AGENTS.md`
- `CLAUDE.md`
- `.cursorrules`

The block is delimited by `<!-- BEGIN JIKJI ROUTING -->` / `<!-- END JIKJI
ROUTING -->`, is refreshed in place on every prepare (idempotent), and preserves
any surrounding user-authored content. `jikji clean` removes just the block again
(deleting the file only when Jikji created it). Skip this with
`jikji prepare ROOT --no-agent-rules`.

## Admin Commands

```bash
jikji prepare /path/to/folder --json
jikji refresh /path/to/folder --json
jikji doctor /path/to/folder --json
jikji map /path/to/folder
```

## Benchmark Positioning

Public comparisons should be labeled as:

```text
raw local agent vs the same agent with Jikji find
```

Jikji find is the file-search skill that cuts repeated LLM calls, tokens, and
wall-clock time by 10-30x in the main raw-Hermes comparison, while preserving or
improving retrieval accuracy.
