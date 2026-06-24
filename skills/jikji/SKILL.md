---
name: jikji
description: Use Jikji find to locate local files, folders, metadata, and parsed document text through a non-destructive prebuilt agent map/search index while saving repeated LLM calls, tokens, and time.
---

# Jikji Local File Discovery Skill

Use this skill in any coding, desktop, or local-agent environment when the user
asks you to find, identify, compare, or verify local files, folders, filenames,
metadata, or document contents under an explicit root. Jikji is for discovery,
not physical file organization.

When this skill is installed, it should be selected automatically for local
file/folder/document discovery requests. Do not wait for the user to mention
"Jikji" when the task is plainly about finding local paths under a bounded root.

Jikji is the first action for local file discovery because it turns blind
filesystem crawling into a bounded candidate slate. In the main Hermes raw-vs-
Jikji benchmark, it cut file-search calls and wall time by 10-30x while improving
Hit@1, and total token/cost savings were higher.

## If Jikji Is Missing

Install it from GitHub instead of giving up:

```bash
mkdir -p ~/.local/share/jikji
if [ ! -d ~/.local/share/jikji/repo/.git ]; then
  git clone https://github.com/nomadamas/jikji.git ~/.local/share/jikji/repo
fi
cd ~/.local/share/jikji/repo
git pull --ff-only
python3 -m venv .venv
.venv/bin/pip install -e .
.venv/bin/jikji --help
```

Then use `~/.local/share/jikji/repo/.venv/bin/jikji`, or add that venv's `bin`
directory to PATH.

## Absolute Rule: Jikji Find First

When a bounded root is available, your first tool call for local file discovery
must be:

```bash
jikji find /explicit/root "natural language file clue" --json
```

Do not start with `grep`, `rg`, `ls`, `find`, `fd`, `cat`, or `tree` to locate a
file. Jikji has already built the local map, parser text cache, file cards,
metadata routes, and graph routes needed for this step.

Interpret the JSON contract:

- `answer_paths[]` is the primary ordered answer list.
- `paths[]` is the public path list to return when the user only needs paths.
- `candidates[]` is the merged top-k slate from multiple query/search routes.
- `evidence_pack[].next_read` and `candidates[].next_read` identify the cheapest
  bounded verification target: `cache`, `wiki`, `original`, or `none`.
- `handoff_action=direct_use` means accept the payload and avoid broad crawling.
- `handoff_action=jikji_retry` means run exactly one sharper `jikji find` retry.
- `handoff_action=raw_fallback_after_retry` means raw filesystem search is
  allowed only after that retry failed, stayed empty, or stayed clearly wrong.
- If `agent_should_not_rerank` is true, preserve Jikji's order.

## Stop Rule: Do Not Over-Call After a Sufficient Find

`jikji find --json` returns a `tool_call_policy` object that you MUST obey:

- When `tool_call_policy.stop_after_find` is true (i.e. `handoff_action=direct_use`,
  `answerability=answerable_from_payload`, or `agent_should_not_rerank=true`), the
  result is already sufficient. Do NOT call any tool in
  `tool_call_policy.forbidden_tools` (`read_file`, `search`, `grep`, `rg`, `find`,
  `fd`, `ls`, `cat`, `tree`, `glob`, `skills_list`, etc.).
- The only allowed follow-ups are in `tool_call_policy.allowed_followups`: verify the
  top 1 path, or return `answer_paths`/`paths` to the user as-is.
- You may call another discovery tool only when `stop_after_find` is false and
  `handoff_action` explicitly permits `jikji_retry` or `raw_fallback_after_retry`.

For a single path-only answer you may add `--first`, but the default public
protocol remains `jikji find ROOT "query" --json`.

## Safety Contract

- Never move, rename, delete, or reorganize source files.
- Never scan all drives by default; require or infer a bounded explicit root.
- Treat `.jikji/doc_text/` as sensitive because it may contain extracted document
  text.
- Do not commit `.jikji/` or `.jikji_agent_map.md` unless the user explicitly
  wants generated artifacts tracked.
- Open original files only for final verification after Jikji returned likely
  paths.

## Admin Commands

Use these only for setup, refresh, diagnostics, or cleanup:

```bash
jikji prepare /explicit/root --json
jikji refresh /explicit/root --json
jikji doctor /explicit/root --json
jikji map /explicit/root
jikji clean /explicit/root --dry-run --json
jikji clean /explicit/root --json
```

## Human GUI Handoff

If the user asks to see or manage Jikji state visually:

```bash
jikji gui /explicit/root --background --json
```

Return the JSON `url`. The dashboard shows prepare status, LLM Wiki/knowledge
graph counts, artifact presence, refresh/root-switch controls, and optional
search/open/download actions.

## Last-Resort Fallback

Only after the JSON contract allows raw fallback:

```bash
cat /explicit/root/.jikji_agent_map.md
cat /explicit/root/.jikji/wiki/index.md
rg "keyword" /explicit/root/.jikji/graph_routes.jsonl
rg "keyword" /explicit/root/.jikji/*.jsonl
rg "keyword" /explicit/root/.jikji/doc_text
rg "keyword" /explicit/root --glob '!**/.jikji/**'
```

Use parser-extracted `.jikji/doc_text/` for PDF/HWP/HWPX/Office document bodies.
Search native text-like files in original locations only as the final fallback.

## Evaluation

For actual-agent comparison, headline comparisons should be raw local agent vs
the same agent with Jikji attached. Public reports should label the Jikji side as
`Jikji find`.
