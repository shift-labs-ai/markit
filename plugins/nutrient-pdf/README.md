# nutrient-pdf

A [markit](https://github.com/Michaelliv/markit) plugin that uses [Nutrient's pdf-to-markdown](https://github.com/PSPDFKit/pdf-to-markdown) CLI for PDF conversion.

## Prerequisites

Install the Nutrient CLI:

```bash
curl -fsSL https://raw.githubusercontent.com/PSPDFKit/pdf-to-markdown/main/install.sh | sh
```

## Install

```bash
markit plugin install git:github.com/Michaelliv/markit#plugins/nutrient-pdf
```

## What it does

Replaces markit's built-in PDF converter with Nutrient's extraction engine:

- **90x faster** than docling, **37x faster** than pymupdf4llm
- **0.92** reading order accuracy (best in class)
- Runs locally — PDFs never leave your machine
- Free for up to 1,000 documents per calendar month

When installed, this plugin takes priority over the built-in PDF converter for all `.pdf` files.
