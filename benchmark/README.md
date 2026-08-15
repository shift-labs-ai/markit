# markit three-way benchmark

Release benchmark for three local, non-OCR document-to-Markdown converters:

- markit 0.6.0 (working tree)
- `@firecrawl/anydoc` 0.1.9
- `@llamaindex/liteparse` 2.12.0

The suites intentionally remain separate. There is no synthetic overall score.

| Suite | Corpus | Quality evaluator | Purpose |
| --- | --- | --- | --- |
| Office | Public DOCX/PPTX/XLSX/EPUB/CSV corpus | Blind, position-swapped OpenAI visual judge | Ordinary office fidelity |
| olmOCR-bench | AllenAI's pinned 1,403-page dataset | Official 8,413 deterministic checks | Standardized PDF quality |
| shitty-pdf-bench | 40 public adversarial PDFs across 50,884 pages | Blind content anchors plus curated golden assertions | Adversarial correctness, recovery, and throughput |

Every converter runs through its end-to-end CLI. Liteparse OCR is disabled for PDF comparisons. Liteparse's office conversion requires LibreOffice and is reported as an end-to-end route rather than a native office parser.

## Install

```bash
./benchmark/setup-tools.sh
python3 -m pip install huggingface_hub olmocr

export MARKIT_BIN=$PWD/rust/target/release/markit
export ANYDOC_BIN=$PWD/.benchmark/tools/node_modules/.bin/anydoc
export LITEPARSE_BIN=$PWD/.benchmark/tools/node_modules/.bin/lit
```

For office visual truth, install LibreOffice and Poppler (`soffice` and `pdftoppm`). Set executable overrides when needed:

```bash
export MARKIT_BIN=$PWD/rust/target/release/markit
export ANYDOC_BIN=/path/to/anydoc
export LITEPARSE_BIN=/path/to/lit
```

## 1. Public office corpus

The committed manifest records sources and SHA-256 hashes. The current seven-file corpus is the reproducible seed, **not yet the final release corpus**; expand it before making broad office-quality claims.

```bash
./benchmark/setup-corpus.sh
python3 benchmark/verify_corpus.py benchmark/office-corpus.json benchmark/corpus
python3 benchmark/bench.py office
python3 benchmark/score_office.py benchmark/results/office
python3 benchmark/render_office_truth.py
python3 benchmark/judge_office.py benchmark/results/office --dry-run
OPENAI_API_KEY=... python3 benchmark/judge_office.py benchmark/results/office
```

The judge evaluates every pair (markit/anydoc, markit/liteparse, anydoc/liteparse) twice with A/B positions swapped. The default model is `gpt-5.6-terra`; task IDs include the model, prompt, truth, and candidate hashes so changed inputs cannot reuse stale verdicts.

## 2. olmOCR-bench

The setup pins both the evaluator commit and Hugging Face dataset revision.

```bash
./benchmark/setup-olmocr.sh "$PWD/.benchmark/olmocr"
python3 benchmark/run_olmocr.py "$PWD/.benchmark/olmocr"
```

Candidate Markdown is written into the dataset's required directory layout, then each tool is scored by `python -m olmocr.bench.benchmark`. Raw official scorer output lands in `benchmark/results/olmocr/`.

## 3. shitty-pdf-bench

`shitty-pdf-bench.json` pins 40 public PDFs by direct source URL, byte size, page count, and SHA-256. The 602.5 MB / 50,884-page corpus spans semiconductor manuals, formal standards, math papers, forms, RTL/CJK documents, dense financial tables, rotated vector charts, scans, tagged PDFs, malformed files, and an encrypted failure control. The PDFs are not redistributed.

```bash
export SHITTY_PDF_BENCH_DIR=$PWD/.benchmark/shitty-pdf-bench
python3 benchmark/setup_shitty_pdf_bench.py
python3 benchmark/verify_corpus.py benchmark/shitty-pdf-bench.json "$SHITTY_PDF_BENCH_DIR"
python3 benchmark/generate_shitty_pdf_bench_anchors.py "$SHITTY_PDF_BENCH_DIR"
python3 benchmark/bench.py shitty-pdf-bench
python3 benchmark/score_shitty_pdf_bench.py benchmark/results/shitty-pdf-bench
python3 benchmark/score_shitty_pdf_bench.py benchmark/results/shitty-pdf-bench \
  --assertions benchmark/shitty-pdf-bench-anchors.json \
  --output-stem anchors-quality \
  --label "shitty-pdf-bench blind anchor recall"
```

Use `--category semiconductor` or repeated `--file NAME` options for a smaller download. `shitty-pdf-bench-anchors.json` freezes 155 blind content anchors selected from fixed 5%, 25%, 50%, 75%, and 95% page samples. The generator reads only source PDFs and the manifest—never candidate outputs—and excludes repeated chrome, title-like lines, and RTL lines whose Poppler ordering is ambiguous.

The current `shitty-pdf-bench-assertions.json` is a preliminary 38-check structural gate over the eight semiconductor documents; it still needs page-specific table, register, reading-order, corruption, and expected-failure assertions across the full corpus. Report blind anchor recall and curated assertion pass rate separately, alongside conversion failures, deterministic-output hashes, and end-to-end timings. Assertions must describe source truth and remain converter-neutral.

## Output contract

Each suite writes under `benchmark/results/<suite>/`:

- `provenance.json` — host, executable paths, versions, iteration policy
- `results.json` / `timings.json` — per-document raw measurements
- `<tool>/*.md` — raw candidate outputs
- `report.md` — timing summary
- `quality.json` and `quality.md` — quality results when scored

`benchmark/results/` and corpora remain gitignored. Publish frozen result artifacts with the GitHub release rather than committing machine-specific output.

## Fairness rules

1. Pin corpus revisions and tool versions.
2. Disable OCR for all PDF tools; none may call a model or service during conversion.
3. Preserve failures as failures—never silently drop a document.
4. Rotate tool order per document and report warmups/iterations.
5. Keep raw output and per-document data with the release.
6. Never merge office, olmOCR, and shitty-pdf-bench quality into one number.
7. Report end-to-end timings as such; do not call them engine-only throughput.
