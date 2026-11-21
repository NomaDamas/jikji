---
name: jikji
description: Find local files, folders and documents by name, location, metadata or by what is written inside them (PDF, HWP, Office, text, code), and answer "which file mentions X" questions. Run `jikji find` BEFORE grep, find, glob or ls whenever the path is unknown and a folder must be searched. 파일·폴더·문서 찾기, 문서 내용 검색. Not for regular expressions, code symbols or line numbers (use grep), known-path reads, or general questions. Explicit Jikji administration is also supported.
---

# Jikji Local File Discovery Skill

## Scope Gate: Jikji Find First

Before calling Jikji, require ALL of these conditions:

1. The task has local file/folder discovery intent, including locating filenames,
   metadata, or documents by their contents.
2. The target path is unknown, or the task requires a corpus-content search to
   identify matching local files or documents.
3. An explicit or reasonably inferred bounded root is available.

A root's existence alone, or the words "file" or "search" alone, is not enough.
Do not use keyword matching to block legitimate discovery queries. Select this
skill for the intent of the discovery substep, not every task in a repository.

Do not gate general QA, explanations, coding advice, web search, known-path
reads or edits, file stats, process/git/build/test/package commands, directory
listings (`ls`), or LSP symbol navigation behind Jikji. A targeted text lookup
inside a known file is not corpus discovery. In a mixed task, apply this gate
only to the discovery substep; later reads, edits, and tests proceed normally.
Explicit Jikji admin requests are allowed separately and need no preliminary find.

Examples:

- Use Jikji: "Find last year's budget spreadsheet in ~/Documents."
- Use Jikji: "Which documents under /work/contracts mention renewal?"
- Mixed task: "Find the report and summarize it" uses Jikji to locate the
  report, then reads the identified report to summarize it.
- No Jikji: "Read /work/docs/report.pdf" or "Edit src/main.rs."
- No Jikji: "Explain Rust ownership", "Search the web for Rust docs",
  "Run cargo test", "Show git status", "Stat report.pdf", "List /work",
  "Install a package", or "Go to this symbol's definition with LSP."
- Explicit admin: "Refresh Jikji's index for /work/docs" runs refresh directly.

## Discovery Protocol

When the scope gate holds, the first discovery call MUST be:

```bash
jikji find /explicit/root "natural language file clue" --json
```

Do not begin that discovery with raw `grep`, `rg`, `glob`, `find`, `fd`, `ls`,
`cat`, or `tree`. This is not a general ban on those tools.

`jikji find` searches existing indexes only. Inspect `index_status`,
`handoff_action`, `answerability`, `retry_proof`, and `tool_call_policy`.

If the index is missing, stale, unavailable, or the result is empty/clearly
wrong, run exactly one sharper Jikji retry using `next_commands[]` (or an
alternate query), with `--after-jikji-retry --retry-proof RETRY_PROOF` when
supported. Never run a second Jikji retry. Raw fallback is allowed only when
that retry yields `handoff_action=raw_fallback_after_retry` and permits it.

Interpret the JSON contract within this discovery request only:

- `answer_paths[]` is the primary ordered answer list; `paths[]` is the public
  path list. `candidates[]` is the merged top-k slate.
- `evidence_pack[].next_read` and `candidates[].next_read` identify bounded
  verification targets: `cache`, `wiki`, `original`, or `none`.
- `handoff_action=direct_use` or `answerability=answerable_from_payload` means
  accept the payload and do not repeat discovery with raw tools.
- `handoff_action=jikji_retry` means run the one sharper discovery retry.
- `index_status` values such as `missing`, `stale`, or `failure` are recovery
  signals, not permission for immediate raw discovery.
- `max_jikji_retries` is `1` before fallback; `max_raw_fallback_commands` is
  the fallback command ceiling. `raw_fallback_allowed=false` prohibits raw
  discovery fallback, not subsequent reading, editing, or testing.
- If `agent_should_not_rerank` is true, preserve Jikji's order.

## Reading Results From Any Folder

`jikji find` accepts any folder, not only the folder that was indexed:

- A sub-folder of an indexed root is answered from that root's index, limited to the
  sub-folder. `scope` and `index_root` name what was searched, and `paths[]` are relative to
  the folder you asked about. Nothing is indexed or written for this.
- Read `notes[]` (each has `code` and `message`) before trusting an empty or short result:
  `freshness_not_checked` means files edited since indexing may be missing, so read recently
  edited files directly; `pattern_syntax_not_supported` means Jikji ranks files by words and
  does not evaluate regular expressions, exact code symbols or punctuation, and returns
  files, not line numbers; use the returned files to narrow, then grep inside them.
- An empty result carries `empty_reason`: `no_match_in_index` (searched
  `indexed_files_searched` files, try fewer or different words), `scope_has_no_indexed_files`,
  or `path_not_found`.
- `index_status=missing` carries `reason` and `next_steps[]`. `not_indexed` means no index
  covers the folder; `indexed_roots_inside[]` lists indexed folders below it that can be
  searched instead. `find` never indexes by itself: ask the user before running
  `jikji prepare`.
- `jikji_version` is in every result. If `jikji --version` differs from the build that
  produced the skill or the index, say so instead of guessing; a stale installed binary
  lacks the behaviour above.

## Request Types And Result Fields

Before trusting a hit, read `request_plan.intent` and the matching `results[]` entry
(`file_kind`, `text_indexed`, `matched_in`, `snippet`). Jikji matches the words of a request
against file names, folders and the text of indexed files; the plan says what that meant here.

- `files_by_name_or_text` (default): documents and plain text are searched by file name, folder
  and indexed text together. `results[].snippet` is the passage around the match (up to the first
  20 results): quote it instead of opening the file when it answers the question. A `null`
  snippet means the file matched by name or path only. `results[].matched_words` lists which of
  `request_plan.query_words` the file contains (in its path or text); with several words, a word
  missing from the top result is either filler of your request or not in the file you want, so
  check it before trusting that result.
- `code_identifier`: a `snake_case`/`camelCase` word that hit source files. Jikji returns the
  files, not lines and not definition vs use (note `identifier_files_only`): run `grep -n` for the
  identifier inside the returned files to get the lines.
- `exact_text`: a regular expression, code syntax or a line position (note
  `pattern_syntax_not_supported`). Jikji cannot answer it exactly. Follow
  `request_plan.use_instead`: `tool=grep` gives a ready `command` and `narrow_to` files to grep
  first; `tool=read` means open the named file at that line. Do not retry Jikji with other words.
- `non_text_files`: images, audio and video are found by file name, folder and extension only
  (`file_kind=image|audio|video`, `text_indexed=false`, `matched_in=["name_path"]`, note
  `non_text_name_match_only`). Never claim such a file's content matches the query. An empty
  result for a question about what a picture shows or a recording says carries
  `non_text_content_not_searchable` and `coverage` (files by kind and how many have indexed text):
  search by the file's name, folder or date instead, or ask the user before running
  `jikji deep-index` (OCR/transcription) on the folder.
- `request_plan.not_searched` lists what this request could not look at; `coverage` (on empty
  results) shows what the index can search by text at all.

## Stop Rule: Stop Discovery, Not The Task

Retry proof and stop restrictions apply only to the discovery substep.
When `tool_call_policy.stop_after_find` is true, do not make another discovery
call to re-find or re-rank sufficient results. Interpret
`tool_call_policy.forbidden_tools` only as restrictions on repeated discovery,
not blanket tool bans. Within discovery, use `allowed_followups` for bounded
verification of the top path or return `answer_paths[]`/`paths[]` as-is.

Known-path reads, edits, tests, and other requested work remain allowed after
successful discovery, even if their tools appear in `forbidden_tools`.
Another discovery call requires `stop_after_find=false` and an explicit
`jikji_retry` or `raw_fallback_after_retry` handoff.

For a single path-only answer, `--first` is optional; the default protocol is
`jikji find ROOT "query" --json`.

## Availability And Authorized Setup

If Jikji is missing, report that limitation; do not automatically clone, pull,
or install it. Installation requires explicit user authorization, including
any installation outside the repository. If no command can run, report that
no retry proof is available rather than pretending the retry succeeded.

If the root is not prepared, run `jikji prepare /explicit/root --json` only
when the user authorizes setup. Discovery itself does not authorize indexing
new locations or changing the environment.

## Safety Contract

- Never move, rename, delete, or reorganize source files as part of discovery.
  This does not block a separately requested edit of an identified file.
- Never scan all drives by default; require or infer a bounded root.
- Treat `.jikji/doc_text/` as sensitive extracted document text.
- Do not commit `.jikji/` or `.jikji_agent_map.md` unless explicitly requested.
- Original files may be read directly when their paths are already known,
  including for the requested work after discovery.

## Admin Commands

For explicit setup, refresh, diagnostics, or cleanup requests, without a find:

```bash
jikji prepare /explicit/root --json
jikji refresh /explicit/root --json
jikji doctor /explicit/root --json
jikji map /explicit/root
jikji clean /explicit/root --dry-run --json
jikji clean /explicit/root --json
```

For an explicit visual management request:

```bash
jikji gui /explicit/root --background --json
```

Return the JSON `url`. The dashboard provides status, refresh/root-switch
controls, and optional search/open/download actions.

## Bounded Discovery Fallback

Only after the discovery JSON contract permits raw fallback, use its bounded
command allowance. Prefer `.jikji_agent_map.md`, `.jikji/wiki/index.md`,
`.jikji/graph_routes.jsonl`, and parser-extracted `.jikji/doc_text/` evidence
before searching original document bodies. Search native text-like files in
original locations as the final discovery fallback. These restrictions do not
apply to normal known-path reads or edits.

For explicitly authorized media OCR/ASR or archive-member body indexing:

```bash
jikji deep-index /explicit/root --json
```

Default `prepare` indexes document and code contents; media and archives remain
metadata/name searchable. `deep-index` enables configured `JIKJI_OCR_ENGINE`
or `JIKJI_ASR_ENGINE` and bounded safe archive parsing incrementally.

## Evaluation

Compare the same agent with and without Jikji for actual local discovery tasks.
Label the Jikji side `Jikji find`.
