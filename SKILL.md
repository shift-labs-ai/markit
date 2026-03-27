---
name: markit
description: Convert files and URLs to Markdown. Supports PDF, DOCX, PPTX, XLSX, HTML, EPUB, CSV, JSON, images, audio, ZIP, and more. Use when you need to extract content from any document format.
---

# markit

Convert anything to Markdown.

## CLI

```bash
# Convert a file
npx markit-ai report.pdf -q

# Convert a URL
npx markit-ai https://en.wikipedia.org/wiki/Markdown -q

# Write to file
npx markit-ai document.docx -q -o output.md

# See all options
npx markit-ai --help

# See supported formats
npx markit-ai formats
```

`-q` gives raw markdown. `--json` gives `{ markdown, title }`.

## SDK

```typescript
import { Markit } from "markit-ai";

const markit = new Markit();
const { markdown } = await markit.convertFile("report.pdf");
const { markdown } = await markit.convertUrl("https://example.com");
const { markdown } = await markit.convert(buffer, { extension: ".docx" });
```
