# markit

Convert anything to markdown. PDF, DOCX, PPTX, XLSX, HTML, EPUB, Jupyter, RSS, images, audio, URLs, and more. Works as a CLI and as a library.

```bash
npm install -g @shift-labs/markit
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

# Text only, skipping image extraction
markit report.pdf --no-images

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
| PDF | `.pdf` | Built-in Rust engine |
| Word | `.docx` | Built-in Rust engine; headings, images, and tables |
| PowerPoint | `.pptx` | Built-in Rust engine; slides, notes, and tables |
| Excel | `.xlsx` | Built-in Rust engine; sheets become markdown tables |
| HTML | `.html` `.htm` | Built-in Rust engine; scripts/styles stripped |
| EPUB | `.epub` | Built-in Rust engine; spine-ordered chapters |
| Jupyter | `.ipynb` | Built-in Rust engine; cells and outputs |
| RSS/Atom | `.rss` `.atom` `.xml` | Built-in Rust engine; dated feed items |
| CSV/TSV | `.csv` `.tsv` | Built-in Rust engine; markdown tables |
| JSON | `.json` | Built-in Rust engine; formatted code block |
| YAML | `.yaml` `.yml` | Built-in Rust engine; code block |
| XML/SVG | `.xml` `.svg` | Built-in Rust engine; code block |
| Images | `.jpg` `.png` `.gif` `.webp` | Built-in Rust metadata extraction |
| Audio | `.mp3` `.wav` `.m4a` `.flac` | Built-in Rust metadata extraction |
| ZIP | `.zip` | Built-in Rust recursive conversion |
| URLs | `http://` `https://` | Rust fetcher with markdown negotiation |
| Wikipedia | `*.wikipedia.org` | Built-in Rust main-content extraction |
| Code | `.py` `.ts` `.go` `.rs` ... | Built-in Rust fenced code block |
| Plain text | `.txt` `.md` `.rst` `.log` | Built-in Rust pass-through |


---

## Benchmarks

Non-OCR PDF to Markdown: parsers that read the text layer already inside a
PDF, with no OCR and no model calls. Measured on one machine.

| | markit | liteparse | anydoc |
|---|---:|---:|---:|
| olmOCR-bench score | **45.0%** | 38.8% | 31.2% |
| shitty-pdf-bench text recovered | **100.0%** | 83.2% | 91.0% |
| shitty-pdf-bench conversion time | **25.2s** | 56.4s | 341.2s |

olmOCR-bench is AllenAI's public benchmark. shitty-pdf-bench is ours: 40
hash-pinned public PDFs, 50,884 pages of semiconductor manuals, standards,
RTL documents, scans, and malformed files.

Method, provenance, and per-category results are in
[rust/BENCHMARKS.md](rust/BENCHMARKS.md); the harness that reproduces them
is in [benchmark/](benchmark/).

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

markit is also a Node library. Every format uses the same bundled Rust engine as the CLI on supported macOS and Linux platforms—there is no second fallback implementation:

```typescript
import { Markit } from "@shift-labs/markit";

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
markit <source> --no-images              # Skip image extraction
markit <source> -i ./images              # Extract images to a directory
cat file.pdf | markit -                  # Read from stdin
markit formats                           # List supported formats
markit onboard                           # Add to CLAUDE.md
```

---

## Development

```bash
bun install
bun run build:native
bun run dev -- report.pdf
bun test
bun run check
cd rust && cargo test
```

## Distribution

Releases publish `@shift-labs/markit` plus six scoped native binary packages
for macOS and Linux (x64/ARM64, glibc/musl). npm selects the matching binary
through `optionalDependencies`. The former `markit-ai` package is a deprecated
compatibility shim that re-exports this SDK.

```bash
# 1. Update the same version in package.json, rust/Cargo.toml,
#    and npm/*/package.json.
# 2. Verify locally.
bun run verify

# 3. Tag and push. GitHub Actions builds and publishes all seven packages.
git tag v0.6.0
git push origin v0.6.0
```

The release workflow requires an npm automation token in the repository secret
`NPM_TOKEN`. It publishes platform packages first through `napi prepublish`,
then `@shift-labs/markit`, then the deprecated `markit-ai` shim, and finally
creates the GitHub release.

## License

MIT
