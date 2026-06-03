# Hard mixed public-document benchmark

This benchmark is designed for Jikji's core purpose: helping local agents find
files in a messy local workspace without RAG, embeddings, or moving files.

It crawls public KOGL (공공누리) resource attachments, downloads a bounded mix of
real document files, splits them into train/valid/test, materializes deep
human-ish folder trees, and writes no-leak file-search eval sets outside the
corpus roots.

## What it builds

```bash
jikji hardbench-suite .benchmarks/hard_mixed_kogl_extreme_20260603_v2 \
  --target-docs 180 --max-data-idx 180 --cases 240 --top-k 10 \
  --difficulty extreme --json
```

The 2026-06-03 extreme v2 run downloaded 179 public documents:

```text
Ext    Files
-----  -----
.pdf     149
.hwp      27
.hwpx      1
.pptx      1
.xlsx      1
```

Split and eval size:

```text
Split  Docs  Cases
-----  ----  -----
train    81    162
valid    27     54
test     72    144
```

Eval scenarios:

```text
Scenario                   Meaning
-------------------------  ------------------------------------------------
body_phrase_no_filename    Find by parsed body phrase with no filename clue
decoy_note_resistant       Ignore matching txt memo/link decoys and find original
multi_body_disambiguation  Pick the one document matching several body clues
weak_folder_memory         Use weak top/year folder memory plus body clue
```

The builder filters obvious parser-noise tokens such as garbled HWP syllable
runs or very long no-space English strings before generating queries. In
`--difficulty extreme`, materialized filenames are generic, test roots are
larger, and every original document receives memo/link decoys that contain
matching clues but are invalid answers.

## Train/valid-driven improvements

The initial run showed Jikji was strong on parsed document-body clues but weak on
folder-context queries because map-backed scoring over-weighted body evidence and
under-weighted path/folder structure.

Implemented from train/valid only:

1. Folder/path-context query detection.
2. Stronger structural path/folder scoring in map-backed search.
3. Discount for memo/link decoys when the user asks for an original document.
4. Stronger format-mismatch discount.
5. Benchmark query-quality filter for parser noise and Korean doc-type labels.
6. Search-index schema v2: include bounded parsed document text in the
   Everything-style SQLite map so exact body clues in PDF/HWP/etc. are available
   to instant search without opening original files.
7. Compact exact-term scoring for no-space Korean phrases and date-like clues,
   improving multi-clue document retrieval without embeddings or RAG.

## Final deterministic extreme test result

```text
Mode   Cases  Hit@1   Hit@3   Hit@5   Hit@10  MRR     Sec     Sec/case
-----  -----  ------  ------  ------  ------  ------  ------  --------
raw      144  0.0486  0.0833  0.1042  0.1597  0.0707   6.295    0.0437
Jikji    144  0.6736  0.8472  0.9167  0.9583  0.7826  29.487    0.2048
```

Jikji is slower than the simple raw lexical diagnostic, but both are far below
1 second per case. Under the user's threshold, the important result is recall:
Jikji improved final test Hit@5 by +0.8125 absolute on the much harder corpus.

Per-scenario Jikji test result:

```text
Scenario                   Cases  Hit@5   Hit@10  MRR
-------------------------  -----  ------  ------  ------
body_phrase_no_filename       40  1.0000  1.0000  0.8633
decoy_note_resistant          37  0.9459  0.9730  0.8566
multi_body_disambiguation     34  0.9706  0.9706  0.8775
weak_folder_memory            33  0.7273  0.8788  0.5039
```

Remaining weakness: weak folder-memory cases are now intentionally difficult
because the query gives only a coarse top/year memory and generic filenames.

## Actual Hermes extreme sample

A small 4-case actual-agent sample was run because full actual-agent loops take
minutes per handful of cases.

```text
Agent mode           Cases  Hit@1   Hit@3   Hit@5   Hit@10  Seconds  Avg sec/case
-------------------  -----  ------  ------  ------  ------  -------  ------------
raw Hermes               4  0.5000  0.5000  0.5000  0.5000  415.444       103.861
Hermes + Jikji fast      4  1.0000  1.0000  1.0000  1.0000   63.156        15.789
```

Interpretation: `jikji-fast` is the closer product-mode comparison for
agent-equipped search. Hermes is still invoked, but it receives a tiny ranked
map/search handoff and is told not to browse/list/grep the filesystem. On this
sample it reached Hit@5/Hit@10 1.0000 and reduced elapsed time by about 6.58x
versus raw Hermes. Actual-agent timings are model/workstation/run dependent; use
the full 144-case deterministic test as the main regression signal and bounded
actual-agent runs as product sanity checks.

For speed-focused actual-agent testing, use map-first mode:

```bash
jikji hermes-bench .benchmarks/hard_mixed_kogl_extreme_20260603_v2/corpus/test \
  --eval-set .benchmarks/hard_mixed_kogl_extreme_20260603_v2/eval/hardbench_test_eval.jsonl \
  --modes raw,jikji-fast --cases 4 --candidate-top-k 10 \
  --fast-max-turns 1 --skills jikji --yolo --json
```

Here `raw` is raw Hermes without Jikji. `jikji-fast` is Hermes with a prebuilt
Jikji map/search candidate handoff; Hermes should choose from the ranked paths
instead of spending turns on exploratory browsing.

## Local pre-downloaded KOGL Type 1/openable benchmark

When a large public-document folder already exists locally, use `--source-dir`
to avoid crawling/downloading again:

```bash
jikji hardbench-suite .benchmarks/local_kogl_extreme_20260603_v1 \
  --source-dir /path/to/public-documents \
  --target-docs 600 --max-file-bytes 26214400 --max-total-bytes 5368709120 \
  --cases 240 --top-k 10 --difficulty extreme --json
```

The 2026-06-03 local run sampled 600 documents from 20,461 local files
(`122GB` source folder) without modifying the source folder. The materialized
benchmark corpus is about `1.3GB`.

```text
Ext    Files
-----  -----
.pdf     161
.hwp     161
.hwpx    162
.xlsx    104
.docx     12
```

```text
Split  Docs  Cases
-----  ----  -----
train   270    240
valid    90    180
test    240    240
```

Final deterministic result:

```text
Mode   Cases  Hit@1   Hit@3   Hit@5   Hit@10  MRR     Sec      Sec/case
-----  -----  ------  ------  ------  ------  ------  -------  --------
raw      240  0.0250  0.0333  0.0458  0.0583  0.0339   35.189    0.1466
Jikji    240  0.3167  0.5125  0.6125  0.8125  0.4532  207.006    0.8625
```

Per-scenario Jikji test result:

```text
Scenario                   Cases  Hit@5   Hit@10
-------------------------  -----  ------  ------
body_phrase_no_filename       66  0.6818  0.9848
decoy_note_resistant          63  0.6508  0.8095
multi_body_disambiguation     55  0.8000  0.9273
weak_folder_memory            56  0.3036  0.5000
```

Actual 4-case sample. `Hermes + Jikji direct` is the skill/tool handoff path:
the agent receives Jikji's prebuilt ranked map/search candidates and does not
spend an additional exploratory Hermes chat turn browsing the corpus.

```text
Agent mode              Cases  Hit@1   Hit@3   Hit@5   Hit@10  Seconds  Avg sec/case
----------------------  -----  ------  ------  ------  ------  -------  ------------
raw Hermes                  4  0.5000  0.7500  0.7500  0.7500  366.282        91.571
Hermes + Jikji fast         4  0.2500  0.7500  0.7500  1.0000  157.014        39.254
Hermes + Jikji direct       4  0.2500  0.7500  0.7500  1.0000    3.202         0.800
```

Hard-subset 4-case sample:

```text
Agent mode              Cases  Hit@1   Hit@3   Hit@5   Hit@10  Seconds  Avg sec/case
----------------------  -----  ------  ------  ------  ------  -------  ------------
raw Hermes                  4  0.2500  0.5000  0.7500  0.7500  428.408       107.102
Hermes + Jikji fast         4  0.2500  0.7500  0.7500  1.0000  101.228        25.307
Hermes + Jikji direct       4  0.2500  0.7500  0.7500  1.0000    3.173         0.793
```

Full 240-case direct handoff result:

```text
Mode          Cases  Hit@1   Hit@3   Hit@5   Hit@10  Seconds  Avg sec/case
------------  -----  ------  ------  ------  ------  -------  ------------
Jikji direct    240  0.3167  0.5125  0.6125  0.8125  194.350         0.810
```

This larger local benchmark is intentionally harder than the 180-document KOGL
suite. It exposes current Jikji weaknesses, especially weak folder-memory cases,
and is a better train/valid/test base for future map/search improvements.
