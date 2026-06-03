---
name: jikji
description: Use Jikji to find local files, folders, metadata, and parsed document text through a non-destructive prebuilt agent map/search index.
---

# Jikji Local File Discovery Skill

Use this skill in any coding, desktop, or local-agent environment when the user
asks you to find, identify, compare, or verify local files, folders, filenames,
metadata, or document contents under an explicit root path. Jikji is for agent
discovery, not physical file organization.

When this skill is installed, it should be selected automatically for local
file/folder/document discovery requests in any agent that supports skills,
persistent instructions, project memory, or prompt snippets. Do not wait for the
user to mention "Jikji" if the task is plainly about finding local paths,
filenames, metadata, or document contents under a bounded root.

Trigger examples:

- "Find the proposal PDF in this project."
- "Search this drive for the document that mentions the vendor renewal."
- "Which local file contains this phrase?"
- "Locate the spreadsheet about last quarter's budget."

## Safety contract

- Never move, rename, delete, or reorganize source files.
- Never scan all drives by default; require or infer a bounded explicit root.
- Treat `.jikji/doc_text/` as sensitive because it may contain extracted document
  text.
- Do not commit `.jikji/` or `000_JIKJI_AGENT_MAP.md` unless the user explicitly
  wants generated artifacts tracked.
- Open original files only for final verification after Jikji has returned
  likely paths.

## Fast agent protocol

Default to `brief` for autonomous work:

```bash
jikji brief /explicit/root "natural language file clue" --top-k 10 --json
```

Use `search` when you only need ranked candidates:

```bash
jikji search /explicit/root "natural language file clue" --top-k 10 --json
```

Interpretation:

- `candidates[].path` is the relative path to return or inspect.
- `evidence` and `matched_terms` explain why the candidate was ranked.
- `candidate_folders` gives folder context.
- `commands` contains fallback searches if the candidate list is insufficient.
- Preserve paths exactly as returned.

## Direct handoff rule

If the candidate list is plausible, do **not** perform a new broad `find`, `ls`,
`rg`, or manual filesystem crawl. Use Jikji's ranked paths directly and verify
only the top candidates when needed.

This is the intended speed benefit: Jikji has already done the repeated file-map
work before the agent receives the task.

## Prepare/refresh/admin commands

`brief` and `search` can auto-prepare a missing explicit root. Use these admin
commands when the user asks for setup, refresh, diagnostics, or cleanup:

```bash
jikji prepare /explicit/root --json
jikji refresh /explicit/root --json
jikji doctor /explicit/root --json
jikji map /explicit/root
jikji clean /explicit/root --dry-run --json
jikji clean /explicit/root --json
```

## Fallback route

Only when `brief`/`search` is empty or clearly wrong:

```bash
cat /explicit/root/000_JIKJI_AGENT_MAP.md
cat /explicit/root/.jikji/agent_routes.md
rg "keyword" /explicit/root/.jikji/*.jsonl
rg "keyword" /explicit/root/.jikji/doc_text
rg "keyword" /explicit/root --glob '!**/.jikji/**'
```

Use parser-extracted `.jikji/doc_text/` for PDF/HWP/HWPX/Office document bodies.
Search native text-like files in original locations as a final fallback.

## Evaluation

To test whether Jikji helps on a root:

```bash
jikji eval-generate /explicit/root --cases 80 --json
jikji eval /explicit/root --json
```

For actual-agent comparison with Hermes:

```bash
jikji hermes-skill-install --json
jikji hermes-bench /benchmark/root \
  --eval-set /external/eval.jsonl \
  --modes raw,jikji-fast,jikji-direct \
  --candidate-top-k 10 --skills jikji --json
```

`raw` means no Jikji. `jikji-fast` gives Hermes a compact map-first handoff.
`jikji-direct` measures the tool/skill behavior where the agent accepts Jikji's
ranked candidates without an extra exploratory chat turn.
