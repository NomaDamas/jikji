# Jikji search evaluation: baseline vs 0.2.3 (2026-11-21)

Question: after generalising the agent-log failures (sub-folder roots, large roots, `--top-k`, empty
and empty-result queries, regex/symbol/line requests, decomposed Hangul, stale installs, skill
trigger), does the search work on data it was not tuned on, and which gains are real?

Harness: `tools/search_eval/` (see its README). Baselines: `v0.1.1` (the install before this work),
`v0.2.1` (HEAD `f0b4fe1`, built in a separate worktree). Improved: `0.2.3`, the installed
`~/.local/bin/jikji` (identical sha256 to `target/release/jikji`). All runs used the real CLI with an
isolated data dir; the production index was byte-for-byte untouched (size and mtime recorded in every
report: `production_index_untouched: true`).

## Reproduce

```bash
cargo build --release -p jikji-cli --bin jikji
git worktree add --detach /tmp/jikji-baseline-src f0b4fe1
(cd /tmp/jikji-baseline-src && CARGO_TARGET_DIR=/tmp/jikji-baseline-target cargo build --release -p jikji-cli --bin jikji)
B="--bin v0.2.1=/tmp/jikji-baseline-target/release/jikji --bin v0.2.3=$HOME/.local/bin/jikji"
python3 tools/search_eval/run_eval.py $B --dataset general --dataset holdout --large-files 5000 --out /tmp/search-eval/final
python3 tools/search_eval/run_eval.py $B --dataset general --dataset holdout --seed 4242 --large-files 3000 --out /tmp/search-eval/seed4242
python3 tools/search_eval/real_docs.py $B --sample 80 --seed 7
python3 tools/search_eval/shape_probe.py $B
python3 tools/search_eval/datasets.py general /tmp/search-eval/fx-agent --large-files 1000
python3 tools/search_eval/agent_live.py --bin v0.2.3=$HOME/.local/bin/jikji --fixture /tmp/search-eval/fx-agent --tasks 12 --model sonnet --max-budget-usd 1.0
```

## Data

| set | content | size |
|---|---|---|
| `general` | office-style synthetic corpus, keyword queries; sub-folders, Korean/special-character root, 5000-file root, empty/no-match/regex/code/line/missing/changed-after-index cases | 101 cases |
| `holdout` | different vocabulary and token shapes, `AB-1234` codes, decomposed (NFD) Hangul names, natural-language wrappers, more punctuation in names and more regex/code/line requests | 112 cases |
| extra seeds | 20261, 777, 4242: same profiles, new random names, tokens and file placement (the signal/no-signal query templates are fixed lists, so seeds do not vary those) | 101 + 112 cases each |
| `shape_probe` | 34 regex/code/line requests and 30 ordinary names with punctuation, written separately from the detector; frozen | 64 queries |
| real documents | 80 files sampled from the local Documents/Downloads (PII-named files excluded), copied to a private temp dir, ground truth from independent text extraction; dev/holdout split by path hash. Reports hold hashed labels only | 131 queries |
| agent | `claude` headless with the installed skill, natural prompts that never mention Jikji, synthetic fixture | 12 tasks per run |

## Results (synthetic, `--large-files 5000`; v0.1.1 and v0.2.1 behave identically, shown once)

The improved column was measured on the build before the version bump to 0.2.3 (same code); the installed
0.2.3 was re-measured on a fresh seed (4242) below and gave the same picture.

| metric | baseline general | 0.2.3 general | baseline holdout | 0.2.3 holdout |
|---|---|---|---|---|
| success | 65/101 | 99/101 | 63/112 | 110/112 |
| expected file found (find cases) | 89% | 100% | 83% | 100% |
| silent misses (not found, no explanation) | 8 | 0 | 13 | 0 |
| empty / missing cases explained | 0/18 | 17/18 | 0/18 | 18/18 |
| regex/code/line requests flagged | 0/8 | 7/8 | 0/16 | 14/16 |
| ... answered confidently with unrelated files | 6 | 1 | 9 | 1 |
| ordinary names wrongly flagged | 0 | 0 | 0 | 0 |
| `find` errors (exit/JSON) | 2 | 0 | 2 | 0 |
| latency p50 / p95 (ms) | 5.6 / 340 | 5.9 / 27 | 6.7 / 305 | 5.8 / 31 |
| large-root latency p50 (ms) | 23 | 24 | 23 | 22 |
| re-run identical / concurrent identical | 100/101, 8/8 | 100/101, 8/8 | 111/112, 8/8 | 111/112, 8/8 |

Per category the gains are: sub-folder 0/8 -> 8/8, sub-folder outside 0/5 -> 5/5, `--top-k` 100/500
0/2 -> 2/2, empty query 0/3 -> 3/3, missing/not indexed 0/3 -> 3/3, no-match 0/6 -> 5/6 (general) and
6/6 (holdout), Korean path (NFD) 4/9 -> 9/9 (holdout), fresh 3/4 -> 4/4. Filename, content, multi-term
and large-root categories were already 100% and stay 100%.

Unseen seeds (0.2.3 / baseline): seed 20261: general 99% / 64%, holdout 97% / 56%; seed 777: general
99% / 64%, holdout 98% / 56%; seed 4242 (installed binary): general 99% / 64%, holdout 97% / 56%. The
holdout's first run, before any detector change, is kept: 86% success, 3 ordinary names wrongly
flagged, 9/16 regex requests flagged, 5 misses (all decomposed Hangul). Those holdout failures drove
two fixes (a shape-based detector instead of a character list, and Unicode composition of terms), so the
holdout was not untouched after that run; the seed runs and the probes below are the unseen evidence.

Blind probe (`shape_probe.py`): 0.2.3 flags 33/34 regex/code/line requests and 0/30 ordinary names
(miss: `import os.path`). Baseline flags nothing (it has no such note).

Real documents (80 files, 131 queries, hit@3, MRR): filename dev 36/36 (0.944) and holdout 27/28
(0.845), content dev 35/37 (0.892) and holdout 26/30 (0.858) for both v0.2.1 and 0.2.3: **identical**.
Latency is unchanged within noise (median 6-19 ms; the baseline's one 1.1 s outlier did not repeat).
That is the expected result: those queries were already the supported kind, and the work did not target
ranking. The 4 content misses are the same files in both; 4 sampled files (3 html, 1 txt) had no text
the parser could extract.

Agent through the real skill (`claude` headless, `sonnet`, 12 tasks: 10 discovery, 1 regex, 1 known-path):

| skill description | discovery tasks where Jikji was used | Jikji first tool | Jikji on non-discovery tasks | answers correct |
|---|---|---|---|---|
| before (policy wording) | 6/10 | 60% | 0/2 | 10/10 |
| after (what it finds, "before grep/find/glob/ls", not for regex/symbols/lines) | 10/10 | 80% | 0/2 | 10/10 |

With the first `haiku` runs the skill was never read (0/10) until the `Skill` tool was allowed in the
harness; with it and the new description Jikji was used in 5/12 (general) and 5/12 (holdout) discovery tasks and
answers were 16/16 and 15/16 correct (one Korean-folder task failed).
Small samples: read them as direction, not as rates.

## What changed (and what each fix is not)

* Folder resolution: a path under an indexed root is answered from that root's index limited to the
  folder, with `scope`, `index_root`, `notes[]`. Nothing special-cases a name or a path.
* `--top-k` above 99 no longer stops; empty query and empty results carry `empty_reason`/`reason` and
  `next_steps`; a freshness check that cannot finish in 2 s (`JIKJI_FRESHNESS_BUDGET_MS`) says so with
  `freshness_not_checked` instead of hanging.
* Regex/code/line detection is by shape (escapes, quantified groups, calls, generics, `::`, `->`,
  `file:line`, `line N`, `줄 N`, `#L12`), not by a character list: ordinary names with brackets,
  parentheses, `?`, `&`, `%`, `+` are not flagged (`crates/jikji-cli/src/query_shape.rs`).
* Terms are composed (NFC) before tokenising so macOS-style decomposed Hangul names match what an agent
  types. Existing indexes built before this change keep their decomposed terms until `jikji refresh`.
* Skill and routing text: description says what Jikji finds and when grep is better; body documents
  `notes[]`, `empty_reason`, `next_steps`, `jikji_version`.

## Remaining limits

* 1 of 8 (general) / 2 of 16 (holdout) regex-like probes still pass as plain words
  (`TODO: handle misses`, `SELECT * FROM users WHERE id=`, `grep -rn needle`, `import os.path`):
  they are indistinguishable from a file-name phrase without a list of keywords, which would overfit.
  Jikji returns files and, for these, the agent may still need to grep.
* Korean nonsense words can match unrelated files through character n-grams and are returned as
  `direct_use` (1 `no_match` case in `general`, 0.2.3 and baseline alike).
* One retry per low-confidence result is still requested by contract; the retry never found anything new
  in the whole run (0 helped, 19-25 wasted per dataset). Removing it breaks the documented
  "exactly one retry" contract, so it is left for a deliberate contract change.
* Folders created after the enclosing index was built return nothing; the guidance says to
  `jikji refresh` the enclosing root after asking the user.
* Real-document results show no ranking gain; the improvements are coverage and explanation, not
  better ranking for queries that already worked.
* `agent_live` samples are small and model-dependent; a weaker model (`haiku`) still often skips the
  skill and greps. The skill description helps but cannot force a choice.
* Indexes built before 0.2.3 contain decomposed-Hangul terms only until refreshed.
