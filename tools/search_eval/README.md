# Jikji search evaluation

Reproducible comparison of a baseline `jikji` binary with an improved one, through the real CLI
(`jikji find ROOT QUERY --json`) and, for the agent run, through the real installed skill.

Every process runs with an isolated `JIKJI_DATA_DIR` / `XDG_DATA_HOME` / `HOME` (see `common.Sandbox`).
The user's index `~/.local/share/jikji/index.sqlite` is never written; each report records its size
and mtime before and after (`production_index_untouched`).

## Scripts

| script | what it measures | data |
|---|---|---|
| `run_eval.py` | success, misses, false positives, signal detection, empty-result explanations, latency, retries, re-run and concurrent determinism | synthetic fixtures from `datasets.py` |
| `real_docs.py` | hit@1/3/10, MRR, latency on real local documents (dev/holdout split by path hash) | a private temp copy of sampled files; reports contain hashed labels only |
| `shape_probe.py` | frozen lists of 64 regex/code/line requests and 60 ordinary names with punctuation: is `pattern_syntax_not_supported` raised exactly for the first group | queries only |
| `wrapper_probe.py` | 12 natural-language phrasings around one word: target first, and its word listed in `matched_words` | tiny temp corpus |
| `agent_live.py` | whether a headless `claude` agent, given a natural prompt that does not mention Jikji, picks `jikji find`, scopes it to the asked folder, and uses grep for regex/symbol requests | synthetic fixture |

## Datasets

`datasets.py` generates two profiles from a seed. They differ in vocabulary, token shapes, folder naming,
file formats, Unicode form (the holdout uses decomposed Hangul names) and query phrasing, so the
improved binary can be judged on data it was not developed against:

* `general` - office-style corpus, bare keyword queries (the shape of the failures seen in agent logs).
* `holdout` - lab/legal/recipe corpus, `AB-1234` style tokens, NFD Korean names, natural-language
  wrappers ("find the document about X"), more punctuation in names, more regex/code/line requests.
  Behaviour was never tuned on it; its first run before any detector change is kept in the report.

Case categories: `filename`, `filename_multi`, `content`, `content_multi`, `subfolder`,
`subfolder_outside`, `korean_path`, `large` (5000 files by default), `topk` (100 and 500),
`media_name` (photo/audio/video found by name and labelled name-only), `media_content` (a question about what a photo shows: empty result must explain that media content is not searched), `code_identifier` (snake_case/camelCase, also inside a differently named file; must return the file and say lines need grep), `partial_match` (one real word plus one that exists nowhere: `matched_words` must list only the real one), `empty_query`, `no_match`, `signal` (regex, code symbol, line number: correct answer is the
`pattern_syntax_not_supported` note), `no_signal` (file names that merely contain brackets, parentheses,
question marks...), `missing` (not indexed, nonexistent), `fresh` (files added, edited, deleted after indexing).

Use `--seed N` for an unseen sample of either profile.

## Reproduce

```bash
cargo build --release -p jikji-cli --bin jikji
# baseline: build the previous commit in a worktree with its own target dir
git worktree add --detach /tmp/jikji-baseline-src <commit>
(cd /tmp/jikji-baseline-src && CARGO_TARGET_DIR=/tmp/jikji-baseline-target cargo build --release -p jikji-cli --bin jikji)

python3 tools/search_eval/run_eval.py \
  --bin v0.2.1=/tmp/jikji-baseline-target/release/jikji --bin v0.2.2=target/release/jikji \
  --dataset general --dataset holdout --large-files 5000 --out /tmp/search-eval/run
python3 tools/search_eval/run_eval.py ... --seed 20261 --large-files 2000      # unseen sample
python3 tools/search_eval/real_docs.py --bin v0.2.1=... --bin v0.2.2=... --sample 80 --seed 7
python3 tools/search_eval/datasets.py general /tmp/search-eval/fx-agent --large-files 1000
python3 tools/search_eval/agent_live.py --bin v0.2.2=target/release/jikji \
  --fixture /tmp/search-eval/fx-agent --tasks 12 --model sonnet --max-budget-usd 1.0
```

`agent_live.py` needs the `claude` CLI and the Jikji skill installed in `~/.claude/skills/jikji`; it
costs real API money (about 1 USD for 12 tasks with `sonnet`). It allows only Bash/Read/Grep/Glob/Skill
and puts the binary under test first on `PATH`.

## How to read the metrics

* `success`: the case's own pass condition (target within `rank_max`; empty result with the right reason;
  signal note present; no false signal on ordinary names).
* `silent_misses`: expected file not returned and nothing in the response explains why.
* `false_positive_cases`: files returned where nothing exists to be found.
* `false_signal_cases`: an ordinary file name wrongly flagged as regex/code (hurts the user).
* `confident_wrong_on_signal`: a regex/symbol/line request answered `direct_use` with unrelated files.
* `signal_guided`: regex/code/line requests that also carry `request_plan.use_instead` (grep command or file read).
* `snippet_with_match`: content hits whose `results[].snippet` contains the searched word.
* `media_found_labelled`, `identifier_found_labelled`, `media_empty_explained`: the file was found AND the response said what kind of match it was (`non_text_name_match_only`, `code_identifier`, `non_text_content_not_searchable`).
* `matched_words_exact`: `results[].matched_words` equals the words the file really contains.
* `wrong_non_text_intent`: a text target classified as `non_text_files`.
* `retries_wasted`: the one retry `next_commands[]` asked for changed nothing (same answer).
* `agent_live` `signal_followed` is only true when the agent called Jikji first and then grepped. An
  agent that goes straight to grep for a regex request is correct and scores `false` there.
