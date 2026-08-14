# markit three-way benchmark

Release benchmark for three local, model-free document-to-Markdown converters:

- markit 0.6.0 (working tree)
- `@firecrawl/anydoc` 0.1.9
- `@llamaindex/liteparse` 2.12.0

The suites intentionally remain separate. There is no synthetic overall score.

| Suite | Corpus | Quality evaluator | Purpose |
| --- | --- | --- | --- |
| Office | Public DOCX/PPTX/XLSX/EPUB/CSV corpus | Blind, position-swapped Sonnet visual judge | Ordinary office fidelity |
| olmOCR-bench | AllenAI's pinned 1,403-page dataset | Official 8,413 deterministic checks | Standardized PDF quality |
| Horrible PDFs | Eight semiconductor manuals | Curated format-neutral golden assertions | Adversarial correctness and robustness |

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
ANTHROPIC_API_KEY=... python3 benchmark/judge_office.py benchmark/results/office
```

The judge evaluates every pair (markit/anydoc, markit/liteparse, anydoc/liteparse) twice with A/B positions swapped. `JUDGE_MODEL` defaults to `claude-sonnet-5`.

## 2. olmOCR-bench

The setup pins both the evaluator commit and Hugging Face dataset revision.

```bash
./benchmark/setup-olmocr.sh "$PWD/.benchmark/olmocr"
python3 benchmark/run_olmocr.py "$PWD/.benchmark/olmocr"
```

Candidate Markdown is written into the dataset's required directory layout, then each tool is scored by `python -m olmocr.bench.benchmark`. Raw official scorer output lands in `benchmark/results/olmocr/`.

## 3. Horrible PDFs

The PDFs are vendor documents and are not committed. Put the exact eight files named in `horrible-assertions.json` in one directory. Their SHA-256 values are part of the assertion manifest.

```bash
export HORRIBLE_PDF_DIR=/path/to/chip-pdfs
python3 benchmark/bench.py horrible
python3 benchmark/score_horrible.py benchmark/results/horrible
```

The score is assertion pass rate, accompanied by conversion failure count, deterministic-output hashes, and end-to-end timings. Assertions must describe source truth and remain converter-neutral.

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
6. Never merge office, olmOCR, and horrible-PDF quality into one number.
7. Report end-to-end timings as such; do not call them engine-only throughput.
