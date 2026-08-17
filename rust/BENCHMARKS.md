# Benchmark targets

## Current standing — markit leads official quality, adversarial robustness, and speed

Every parser here is non-OCR: it reads the text layer already inside the
PDF rather than running optical character recognition or a vision model.
OCR is disabled for all three.

**olmOCR-bench measured at `44a5389`; shitty-pdf-bench measured at
`5408b66` with `--no-images`.** Both on macOS 26.5 arm64 against
`@llamaindex/liteparse@2.12.0` and `@firecrawl/anydoc@0.1.9`. Results
are not stored in this repository; the pinned harness below reproduces
them. Quote the commit alongside any number on this page, because a
rerun at a different commit legitimately produces a different one.

Image extraction never changes extracted text, verified across all 39
convertible corpus documents, so the quality figures hold under either
image setting.

### Official olmOCR-bench quality

1,403 single-page PDFs and 8,413 official checks:

| Tool | Macro score | 95% CI | Converted |
|---|---:|---:|---:|
| **markit** | **45.0%** | ±1.0 | 1,403/1,403 |
| liteparse 2.12.0 | 38.8% | ±0.9 | 1,403/1,403 |
| anydoc 0.1.9 | 31.2% | ±0.9 | 1,160/1,403 |

markit leads liteparse by **6.2 points**, outside the confidence interval.

| Category | markit | liteparse | anydoc |
|---|---:|---:|---:|
| headers_footers | **76.1** | 56.1 | 50.8 |
| long_tiny_text | **36.2** | 23.8 | 17.2 |
| arxiv_math | **22.4** | 0.0 | 0.0 |
| old_scans | 13.3 | 13.3 | 13.3 |
| old_scans_math | 0.0 | 0.0 | 0.0 |
| multi_column | 61.2 | **66.0** | 41.6 |
| table_tests | 50.9 | **51.8** | 43.6 |

The latest reading-order work raised multi-column from **521/884 to
541/884** with no lost multi-column checks. Numeric-reference rendering
also raised arxiv_math from 653/2,927 to 657/2,927.

### shitty-pdf-bench

Our own adversarial corpus: 40 hash-pinned public PDFs, 50,884 pages,
602.5 MB of semiconductor manuals, standards, forms, RTL and CJK
documents, rotated vector charts, scans, tagged PDFs, and malformed
files. Final markit validation remains **155/155 blind anchors
(100.0%)** and **38/38 curated checks**. One encrypted password control
fails explicitly for every parser, as intended.

Blind anchor recall across all three:

| Tool | Anchors | Recall |
|---|---:|---:|
| **markit** | **155/155** | **100.0%** |
| anydoc | 141/155 | 91.0% |
| liteparse | 129/155 | 83.2% |

The 155 anchors are seven-word passages sampled from fixed page
quantiles of the source PDFs. The generator reads only the sources and
the manifest, never any converter output, so the anchors cannot be
tuned to a winner.

Controlled end-to-end CLI timing uses one warmup and three measured
iterations per document; each row sums the per-document medians. No
parser extracts images: anydoc writes no image files and liteparse is
given `--image-mode off`, so markit runs with `--no-images` rather than
its default, which would otherwise decode and re-encode every embedded
image and measure markit doing strictly more work than the tools it is
compared against.

| Tool | Converted | Median ms/doc | Total seconds |
|---|---:|---:|---:|
| **markit** | 39/40 (97.5%) | **118.31** | **23.19** |
| liteparse | 39/40 (97.5%) | 736.37 | 55.44 |
| anydoc | 37/40 (92.5%) | 939.44 | 322.23 |

markit is **2.4× faster than liteparse** and **13.9× faster than anydoc**
on this same-machine end-to-end run, and is the fastest parser on all 37
documents every tool converted.

### The wider non-OCR field

liteparse and anydoc are the closest comparable tools, but they are not
the whole field. The parsers below all read the embedded text layer with
no OCR and no model calls, so they answer the same question markit does.

Measured on a stratified 16-document subset of shitty-pdf-bench spanning
1 to 1,323 pages and deliberately including Arabic and Hebrew RTL, scans,
rotated pages, forms, and malformed files. Timing covers the 13 documents
every parser converted (3,476 pages); recall covers the 52 blind anchors
in the subset. Python tools are invoked per document, so interpreter and
import startup is included, as it is for any real caller; it is at most
3.8s of pymupdf4llm's total.

| Tool | Text recovered | Seconds | Pages/s |
|---|---:|---:|---:|
| **markit** | **100.0%** | **1.0** | **3,612** |
| liteparse 2.12.0 | 98.1% | 7.2 | 486 |
| anydoc 0.1.9 | 94.2% | 14.1 | 247 |
| pypdf 6.16.1 | 84.6% | 50.0 | 70 |
| pdfplumber 0.11.10 | 73.1% | 112.2 | 31 |
| markitdown 0.1.7 | 73.1% | 140.1 | 25 |
| pymupdf4llm 1.28.2 | 90.4% | 328.0 | 11 |

markit leads on both axes against every parser tested. pymupdf4llm is
commonly described as the fast option; that reputation belongs to raw
`get_text()` extraction, while its Markdown layer runs per-page layout and
table analysis and is the slowest tool here.

The four Python parsers were run with one warmup and one timed pass rather
than the median of three used above, because the margins are 51× to 336×
and extra iterations would not change the ordering. pdfplumber and pypdf
emit plain text rather than Markdown, so their recall is reported for
completeness rather than as a Markdown comparison.

Run with `python3.14`, `pymupdf 1.28.2`, and `pdfminer.six 20260107`.

Image extraction is 8% of markit's runtime over this corpus, which is
dominated by text-heavy manuals, but 97% on a figure-heavy paper such as
arxiv-gpt3 (83 images, 12 MB). Extracted text is identical either way.

Caveats:
- The olmOCR baseline check is permissive: image-placeholder-only outputs
  can pass its alphanumeric requirement.
- Speed is not quality-adjusted; OCR is disabled for every PDF tool.
- Old scans and old-scan math remain capped without OCR.

## Targets

### 1. olmOCR-bench — beat liteparse (primary)

[liteparse](https://github.com/run-llama/liteparse) (LlamaIndex, Rust) is the
direct competitor: open-source, non-OCR, PDF to Markdown, OCR disabled when
benchmarked. Published scores (v2.1):

- olmOCR-bench: **0.391** ← beaten: markit 0.450 (their same-machine
  measurement is 0.388)
- opendataloader-bench: 0.875
- ParseBench: 0.3279

The original ~12-point gap mapped to roadmap items 2–5 (reading order,
tables, header/footer classification, tiny-text recovery). markit now leads
the same-machine non-OCR baseline by 6.2 points; math reconstruction
remains the largest pool for extending that lead.

Supported claim: *fastest non-OCR PDF to Markdown parser at the highest
non-OCR olmOCR-bench score among the parsers tested*. Quality and speed
were both measured on the same machine; the suites remain reported
separately.

Secondary same-suite runs once the adapter exists: opendataloader-bench,
ParseBench (charts/visual-grounding columns are ML-only noise for every
non-OCR tool; report for completeness).

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

## Reproducibility

The committed `benchmark/` harness pins:

- olmOCR evaluator commit `f7cfe4c22098b154c76b6ec950d1c0a464eecf8d`
- olmOCR dataset revision `eaa828947384ffce68f08c223a0f5f4e2f2df624`
- `@llamaindex/liteparse@2.12.0`
- `@firecrawl/anydoc@0.1.9`
- all 40 shitty-pdf-bench URLs, sizes, page counts, and SHA-256 hashes

Raw candidates, scorer logs, timings, failures, and provenance are generated
under `benchmark/results/` for release attachment; machine-specific result
artifacts remain gitignored. Regression PDFs derived from olmOCR-bench are
committed under `rust/testdata/pdf/regression/` (ODC-BY-1.0; attribution:
Allen Institute for AI, olmOCR-bench).
