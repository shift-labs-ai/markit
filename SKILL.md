---
name: markit
description: Convert files and URLs to Markdown. Supports PDF, DOCX, PPTX, XLSX, HTML, EPUB, CSV, JSON, GitHub URLs, images, audio, ZIP, and more. Use when you need to extract content from any document format.
---

# markit

Convert anything to Markdown.

## CLI

```bash
# Convert a file
npx @shiftlabs/markit report.pdf -q

# Convert a URL
npx @shiftlabs/markit https://en.wikipedia.org/wiki/Markdown -q

# GitHub URLs (repos, files, gists, issues, PRs)
npx @shiftlabs/markit https://github.com/owner/repo -q
npx @shiftlabs/markit https://github.com/owner/repo/issues/42 -q
npx @shiftlabs/markit https://gist.github.com/user/id -q

# Write to file
npx @shiftlabs/markit document.docx -q -o output.md

# See all options
npx @shiftlabs/markit --help

# See supported formats
npx @shiftlabs/markit formats
```

`-q` gives raw markdown. `--json` gives `{ markdown, title }`.

## SDK

```typescript
import { Markit } from "@shiftlabs/markit";

const markit = new Markit();
const { markdown } = await markit.convertFile("report.pdf");
const { markdown } = await markit.convertUrl("https://example.com");
const { markdown } = await markit.convert(buffer, { extension: ".docx" });
```
