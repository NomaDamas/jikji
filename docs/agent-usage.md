# Agent Usage

Jikji is the local-file discovery skill. Use it when a task asks for a file,
folder, filename, metadata, parsed document text, or the source file that answers
a question under an explicit root.

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

## Do Not Crawl First

Do not start file discovery with `ls`, `find`, `rg`, `grep`, `tree`, or broad
manual opening. Those are fallback actions only after the Jikji handoff contract
allows them.

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
