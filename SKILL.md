---
name: markit
description: Convert files and URLs to Markdown. Supports PDF, DOCX, PPTX, XLSX, HTML, EPUB, CSV, JSON, images, audio, ZIP, and more. Use when you need to extract content from any document format.
---

# markit

Convert anything to Markdown.

## Usage

```bash
# Convert a file
npx markit-ai report.pdf -q

# Convert a URL
npx markit-ai https://example.com -q

# See all options
npx markit-ai --help

# See supported formats
npx markit-ai formats
```

The `-q` flag gives raw markdown without decoration. Use `--json` for `{ markdown, title }` output.
