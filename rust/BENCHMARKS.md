# Benchmark targets

## Current standing (commit 03a0d7f, 2026-08-11)

olmOCR-bench, 1,403 single-page PDFs, 8,413 checks:

- **Overall: 27.1% ± 0.9%** (macro over categories)
- Throughput: 154.9 docs/s (9.06 s total, one warm native process, no concurrency)
- Conversion failures: 7 (0.5%) — all "text ops decoded to nothing"

Caveats carried forward:
- The 99.5% baseline is inflated: comment-only outputs (image placeholders)
  pass the benchmark's alphanumeric check. All 98 old-scan outputs were
  comment-only, plus 21/36 old-scan-math and 23/62 long-tiny-text.
- Speed is not quality-adjusted (no OCR, near-zero formula reconstruction).

## Targets

### 1. olmOCR-bench — beat liteparse (primary)

[liteparse](https://github.com/run-llama/liteparse) (LlamaIndex, Rust) is the
direct competitor: open-source, model-free, PDF→markdown, OCR disabled when
benchmarked. Published scores (v2.1):

- olmOCR-bench: **0.391** ← the number to beat (we're at 0.271)
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
