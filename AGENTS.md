# markit

Convert anything to markdown. PDF, DOCX, PPTX, XLSX, HTML, EPUB, Jupyter notebooks, RSS, images, audio, URLs, Wikipedia, GitHub, iWork, XML, YAML, CSV, JSON, ZIP — everything gets milled.

## Commands

```bash
bun run dev -- <file-or-url>           # Dev — convert something
bun run dev -- convert <file-or-url>   # Explicit convert command
bun run dev -- formats                 # List supported formats
bun run dev -- init                    # Create .markit/ config directory
bun run dev -- config show             # Show configuration
bun run dev -- plugin install <src>    # Install a plugin (npm:, git:, or path)
bun run dev -- plugin list             # List installed plugins
bun test                               # Tests
bun run check                          # Biome lint + format
```

## Architecture

- `src/main.ts` — Commander entry point, global --json/--quiet/--prompt flags
- `src/markit.ts` — `Markit` class: converter registry. Tries converters in priority order.
- `src/types.ts` — StreamInfo, ConversionResult, Converter, MarkitOptions interfaces
- `src/converters/` — One file per format (20 converters: pdf, docx, pptx, xlsx, html, epub, ipynb, rss, image, audio, csv, json, xml, yaml, zip, github, wikipedia, iwork, plain-text)
- `src/commands/` — CLI commands (convert, formats, onboard, init, config, plugin)
- `src/plugins/` — Plugin system (install from npm/git/local, loader, API)
- `src/utils/output.ts` — Chalk output helpers, triple output (json/quiet/human)

## Key Patterns

- **Converter interface**: Each converter implements `name`, `accepts(streamInfo)`, and `convert(buffer, streamInfo, options)`. Optional `convertUrl()` hook for URL-first converters (e.g. GitHub, Wikipedia).
- **Priority order**: Specific formats first (pdf, docx), generic last (plain-text as catch-all)
- **Output triple**: Every command supports `--json`, `--quiet`, and human-readable output
- **URL support**: `markit https://example.com` fetches and converts. Converters with `convertUrl()` can handle fetching themselves.
- **LLM providers**: `MarkitOptions.describe` for image description, `MarkitOptions.transcribe` for audio transcription. Passed through via `--prompt`.
- **Plugin system**: Extend with custom converters via `markit plugin install npm:pkg` / `git:url` / local path
- **Optional deps**: xlsx is a dynamic import — fails gracefully with install instructions

## Adding a New Converter

1. Create `src/converters/<format>.ts`
2. Implement the `Converter` interface (name, accepts, convert)
3. Import and add to the converters array in `src/markit.ts`
4. Add to the formats list in `src/commands/formats.ts`
