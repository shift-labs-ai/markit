# markit

Convert anything to markdown. PDF, DOCX, PPTX, XLSX, HTML, EPUB, Jupyter, RSS, images, audio, URLs, and more. Works as a CLI and as a library.

```bash
npm install -g markit-ai
```

---

## Quick Start

```bash
# Documents
markit report.pdf
markit document.docx
markit slides.pptx

# Data
markit data.csv
markit config.json
markit schema.yaml

# Web
markit https://example.com/article
markit https://en.wikipedia.org/wiki/Markdown

# Media
markit photo.jpg                          # EXIF metadata
markit recording.mp3                      # Audio metadata

# Write to file
markit report.pdf -o report.md

# Pipe it
markit report.pdf | pbcopy
markit data.xlsx -q | napkin create "Imported Data"
```

---

## Supported Formats

| Format | Extensions | How |
|--------|-----------|-----|
| PDF | `.pdf` | Text extraction via unpdf |
| Word | `.docx` | mammoth → turndown, preserves headings/tables |
| PowerPoint | `.pptx` | XML parsing, slides + notes + tables |
| Excel | `.xlsx` | Each sheet → markdown table |
| HTML | `.html` `.htm` | turndown, scripts/styles stripped |
| EPUB | `.epub` | Spine-ordered chapters, metadata header |
| Jupyter | `.ipynb` | Markdown cells + code + outputs |
| RSS/Atom | `.rss` `.atom` `.xml` | Feed items with dates and content |
| CSV/TSV | `.csv` `.tsv` | Markdown tables |
| JSON | `.json` | Pretty-printed code block |
| YAML | `.yaml` `.yml` | Code block |
| XML/SVG | `.xml` `.svg` | Code block |
| Images | `.jpg` `.png` `.gif` `.webp` | EXIF metadata |
| Audio | `.mp3` `.wav` `.m4a` `.flac` | Metadata |
| ZIP | `.zip` | Recursive. converts each file inside |
| URLs | `http://` `https://` | Fetches with `Accept: text/markdown` |
| Wikipedia | `*.wikipedia.org` | Main content extraction |
| Code | `.py` `.ts` `.go` `.rs` ... | Fenced code block |
| Plain text | `.txt` `.md` `.rst` `.log` | Pass-through |


---

## For Agents

Every command supports `--json`. Raw markdown with `-q`.

```bash
markit report.pdf --json       # Structured output for parsing
markit report.pdf -q           # Raw markdown, nothing else
markit onboard                 # Add instructions to CLAUDE.md
```

---

## SDK

markit is also a library:

```typescript
import { Markit } from "markit-ai";

const markit = new Markit();
const { markdown } = await markit.convertFile("report.pdf");
const { markdown } = await markit.convertUrl("https://example.com");
const { markdown } = await markit.convert(buffer, { extension: ".docx" });
```

---

## CLI Reference

```bash
markit <source>                          # Convert file or URL
markit <source> -o output.md             # Write to file
markit <source> --json                   # JSON output
markit <source> -q                       # Raw markdown only
cat file.pdf | markit -                  # Read from stdin
markit formats                           # List supported formats
markit onboard                           # Add to CLAUDE.md
```

---

## Development

```bash
bun install
bun run dev -- report.pdf
bun test
bun run check
```

## License

MIT
