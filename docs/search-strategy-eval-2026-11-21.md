# Jikji search strategy and evaluation: 0.2.6 vs baselines (2026-11-21)

Follows `search-eval-2026-11-21.md` (0.2.3). Question: can `jikji find` state what kind of request it
served and what it actually searched, for documents, code, non-text files, Korean paths, sub-folders,
several words, missing/stale indexes and empty results, without rules written for particular queries?

## Strategy

Every signal is the shape of the query or a stored fact about the returned files; none is a list of
known queries or file names.

1. Classify the request (`request_plan.intent`):
   * `exact_text`: regex, code syntax, line position, by shape (`crates/jikji-cli/src/query_shape.rs`:
     escapes, quantified groups, calls, generics, `::`, `->`, `$VAR`, command options, a lone `*`,
     `file:LINE`, `line N`, `줄 N`, `#L12`). Jikji does not pretend: note
     `pattern_syntax_not_supported` and `request_plan.use_instead` (a ready `grep -rnE/-rnF` command, or
     a file read for line numbers, with `narrow_to` the files Jikji found).
   * `code_identifier`: `snake_case`/`camelCase` token with a source file among the top results:
     files, not lines (note `identifier_files_only`).
   * `non_text_files`: the best result is an image/audio/video indexed by name only, or the query is an
     extension token (`.mp4`).
   * `files_by_name_or_text` (default), `empty_query`.
2. Per result (`results[]`, from the index): `file_kind` (by extension), `text_indexed`, `matched_in`
   (`name_path` / `text`), `matched_words` (which request words the file contains), `snippet` (passage
   around the match, first 20 results, Unicode-composition and case insensitive).
3. Non-text files are found by name, folder and extension only; the response says so
   (`non_text_name_match_only`) and an empty answer to "what a photo shows" carries
   `non_text_content_not_searchable` plus `coverage` (files by kind and how many have indexed text).
4. Existing state notes stay: `searched_enclosing_index` (sub-folder), `freshness_not_checked`,
   `no_match_in_index`, `scope_has_no_indexed_files`, `path_not_found`, `empty_query`, missing-index
   `reason`/`next_steps`.
5. Tried and dropped: a "some words missing" warning. Filler words (`please`, `찾아줘`) made it fire
   on 5 of 12 natural phrasings (`wrapper_probe.py`); `matched_words` states the facts without it.
6. Stop words gained request verbs (`find`, `search`, `show`, `찾아`, `알려줘`, `대한` ...).

Contracts: new additive JSON fields only (`request_plan`, `results`, `coverage`); the existing
`paths`, `handoff_action`, retry contract and notes are unchanged. Existing indexes need no migration.
Skill (`skills/jikji/SKILL.md`, embedded copy in `crates/jikji-agent/assets/jikji/`) and the routing
block describe the fields; `docs/agent-usage.md` lists them.

## Data and reproduction

Harness `tools/search_eval/` (README there). New cases on both profiles: `media_name`, `media_content`,
`code_identifier` (also inside a differently named file), `partial_match`, snippets, `matched_words`.
`general` is the development profile, `holdout` a different vocabulary/format/Unicode profile; extra
seeds regenerate every name. Frozen blind lists: `shape_probe.py` (64 signal + 60 ordinary queries),
`wrapper_probe.py` (12 natural phrasings).

```bash
B="--bin v0.2.1=/tmp/jikji-baseline-target/release/jikji --bin installed=$HOME/.local/bin/jikji"
python3 tools/search_eval/run_eval.py $B --dataset general --dataset holdout --seed 9001 --large-files 3000 --out /tmp/search-eval/final6-9001
python3 tools/search_eval/shape_probe.py --bin installed=$HOME/.local/bin/jikji
python3 tools/search_eval/wrapper_probe.py --bin installed=$HOME/.local/bin/jikji
python3 tools/search_eval/real_docs.py $B --sample 80 --seed 7
```

## Results (installed 0.2.6 vs 0.2.1 baseline; seeds 9001 and 424242, 3000-file large root)

| metric | baseline general | 0.2.6 general | baseline holdout | 0.2.6 holdout |
|---|---|---|---|---|
| success | 68% | 99% | 61% | 99-100% |
| expected file found | 91% | 100% | 86% | 100% |
| silent misses | 8 | 0 | 13 | 0 |
| empty/missing cases explained | 0/21 | 21/21 | 0/21 | 20-21/21 |
| regex/code/line requests flagged | 0/8 | 7/8 | 0/16 | 16/16 |
| ... with grep/read guidance | 0 | 7/8 | 0 | 16/16 |
| content hits with a snippet containing the word | 0/12 | 12/12 | 0/12 | 12/12 |
| `matched_words` exact | 0/14 | 14/14 | 0/14 | 14/14 |
| media found and labelled name-only | 0/7 | 7/7 | 0/7 | 7/7 |
| code identifier found and labelled | 0/6 | 6/6 | 0/6 | 6/6 |
| media-content question explained | 0/3 | 3/3 | 0/3 | 3/3 |
| ordinary names wrongly flagged | 0 | 0 | 0 | 0 |
| latency p50 / p95 (ms) | 7 / 470 | 8 / 29 | 8 / 420 | 8 / 26 |

Blind lists: regex/code/line 61/64 flagged, ordinary names 0/60 flagged (was 57/64 in 0.2.3);
natural phrasings 12/12 target first. Real documents (80 files, 131 queries): hit@3 and MRR identical
to 0.2.3 (ranking unchanged); content hits with a snippet containing the word: 34/37 dev, 27/30 holdout
(0.2.3: none). A frozen-list result is recorded, not tuned.

Agent through the installed skill (`claude` headless, sonnet, 16 tasks per fixture, prompts never
mention Jikji; 0.2.4 build and skill): Jikji first on 83% of discovery tasks with the new skill vs 75%
(general) and 83% vs 83% (holdout) with the previous skill; no Jikji call on regex/known-path tasks in
either; answers 16/16 and 15/16. That difference is within noise: the effect of the skill text on a strong
model is small here.

## Problems found on the way

* One evaluation run showed a document hit followed by a media file classified `non_text_files`
  (looser "half the results are media" rule). Fixed to "best result is name-only media"; regression test
  `a_document_hit_followed_by_a_media_file_is_still_a_text_request`.
* A first version flagged `partial_word_match`; removed (see strategy 5).
* The 0.2.4 evaluation of `real_docs.py` initially could not read snippets; added `snippet_with_match`.

## Remaining limits

* `TODO: handle misses`, `import os.path`, `vec![1, 2]`, `arr[0].name` look like words or names: not
  flagged (3 of 64 blind probes; 1 of 8 in `general`). Flagging them needs keyword lists.
* Korean nonsense words match through character n-grams and are returned confidently (1 `no_match`
  case per seed in one profile; same in the baseline).
* One retry on a low-confidence result is still requested by the existing contract; in the evaluation
  it never helped (0 helped).
* Media content (OCR/transcription) is searchable only after an authorised `jikji deep-index`; the
  index stores no EXIF/ID3 metadata, so only name, folder and extension are searched for media.
* Folders created after the enclosing index was built return nothing until `jikji refresh`.
* The agent A/B used the 0.2.4 build; sample sizes are small and model dependent.
