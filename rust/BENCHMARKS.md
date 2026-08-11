# Benchmark targets

## Current standing (2026-08-11, end of sprint 4) — markit leads both axes

olmOCR-bench, 1,403 single-page PDFs, 8,413 checks. Same-machine
liteparse 2.11.1 (no OCR, markdown mode, one warm batch process).

|  | markit | liteparse |
|---|---:|---:|
| **Quality (macro)** | **40.8% ± 0.9** | 38.8% ± 0.9 |
| **Speed (1,403 PDFs, 1 core)** | **1.08 s · 1,300 docs/s** | 5.9 s · 237 docs/s |
| **Speed (8 threads)** | **0.26 s · 5,470 docs/s** | n/a (single-threaded CLI) |
| Conversion failures | 0 | 0 |

markit leads quality by **+2.0 points, outside the confidence
interval**, and is **5.5× faster per core** (both tools measured
single-threaded: liteparse `batch-parse` runs 5.42s user / 5.93s
real, ~1 core). Conversion is stateless per document, so markit
additionally scales with threads (`CORPUS_BENCH_THREADS=8` → 0.26s,
23× liteparse's wall time). markit was 27.1% and 155 docs/s at
03a0d7f.

Head-to-head by category:

| Category | markit | liteparse |
|---|---:|---:|
| headers_footers | **73.6** | 56.3 |
| long_tiny_text | **36.9** | 23.8 |
| arxiv_math | **0.6** | 0.0 |
| old_scans | 13.3 | 13.3 |
| old_scans_math | 0.0 | 0.0 |
| multi_column | 58.1 | **65.3** |
| table_tests | 43.8 | **51.5** |

Measurement notes: markit timed in-process over the corpus
(examples/corpus_bench, one warm process, sequential); liteparse via
`lit batch-parse` wall time (one process, includes ~0.1 s startup),
best of 3 each, same machine, same corpus. Honest asymmetry: their
number includes process startup and per-file logging; ours excludes
startup. At a 4.4 s margin the ranking is robust to both.

Remaining quality pools (to extend the lead): multi_column magazine
layouts (−~100 head-to-head), tables cell structure (−~95);
old_scans/math capped without OCR / formula reconstruction for both
tools.

liteparse artifacts: /tmp/liteparse-venv (pip 2.11.1),
/tmp/liteparse-out (their outputs), staged as bench candidate
`liteparse`, scored in /tmp/olmOCR-liteparse-run1.out.

Caveats carried forward:
- The 100% baseline is still partly comment-only outputs (image
  placeholders) passing the alphanumeric check — all 98 old-scan outputs,
  plus 21/36 old-scan-math and ~23/62 long-tiny-text.
- Speed is not quality-adjusted (no OCR, near-zero formula reconstruction).

## Targets

### 1. olmOCR-bench — beat liteparse (primary)

[liteparse](https://github.com/run-llama/liteparse) (LlamaIndex, Rust) is the
direct competitor: open-source, model-free, PDF→markdown, OCR disabled when
benchmarked. Published scores (v2.1):

- olmOCR-bench: **0.391** ← beaten: markit 0.408 (their same-machine
  measurement is 0.388)
- opendataloader-bench: 0.875
- ParseBench: 0.3279

The ~12-point gap maps to roadmap items 2–5 (reading order, tables,
header/footer classification, tiny-text recovery). Math categories score
near zero for every model-free tool, so formula reconstruction is not
required to win — but it is the largest absolute deficit.

Claim to make when won: *fastest model-free PDF→markdown at the highest
model-free olmOCR-bench score*. Requires a same-machine liteparse run for
the speed side (their "45x faster" claims are against different baselines).

Secondary same-suite runs once the adapter exists: opendataloader-bench,
ParseBench (charts/visual-grounding columns are ML-only noise for every
model-free tool; report for completeness).

### 2. anydoc bench — office formats (secondary)

[anydoc](https://github.com/firecrawl/anydoc) (Firecrawl, Rust) benchmarks
docx/xlsx/xls/pptx/epub/csv/odt/rtf — **PDFs are out of scope** of their
harness. Quality is an LLM judge (Claude Sonnet 5, pairwise, position-swapped)
against LibreOffice-rendered page images, plus deterministic structure counts.
Their corpus is **not redistributable**; the harness converts whatever is in
`bench/samples/`.

To compete: assemble our own corpus, add a markit adapter to their harness
(or replicate its methodology), publish our own run. Exercises the office
converters (TS + Rust), not the PDF engine. Their published quality score:
81 overall, judged on their corpus.

## Reproducibility (roadmap item 8)

Not yet committed: pinned olmOCR-bench revision, markit adapter, category
floors, failure-rate ceilings, quality/speed Pareto report. Last run's
artifacts live in /tmp only:

- /tmp/markit-quality-2026-08-11.{md,json}
- /tmp/olmOCR-markit-full.out
- /tmp/olmOCR-bench-data/bench_data/markit/

Conversion-failure fixtures (the 7 PDFs) are committed under
`testdata/pdf/regression/` (from olmOCR-bench, ODC-BY-1.0 — attribution:
Allen Institute for AI, olmOCR-bench).
