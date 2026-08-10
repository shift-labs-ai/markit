# markit

Convert anything to markdown. PDF, DOCX, PPTX, XLSX, HTML, EPUB, Jupyter notebooks, RSS, images, audio, URLs, Wikipedia, GitHub, iWork, XML, YAML, CSV, JSON, ZIP — everything gets milled.

## Commands

```bash
bun run dev -- <file-or-url>           # Dev — convert something
bun run dev -- convert <file-or-url>   # Explicit convert command
bun run dev -- formats                 # List supported formats
bun test                               # Tests (TS)
bun run check                          # Biome lint + format (TS)
cd rust && cargo test                  # Tests (Rust port)
bun run check:rust                     # rustfmt --check + clippy -D warnings
bun run check:all                      # Both sides
```

The Rust port lives in `rust/` and mirrors the TS pipeline byte-for-byte;
run both test suites when touching conversion logic. Lint policy is in
`rust/Cargo.toml` `[lints.clippy]` (index loops in ported algorithm code
are intentionally allowed to keep the ports auditable against their sources).

## Architecture

- `src/main.ts` — Commander entry point, global --json/--quiet flags
- `src/markit.ts` — `Markit` class: converter registry. Tries converters in priority order.
- `src/types.ts` — StreamInfo, ConversionResult, Converter, MarkitOptions interfaces
- `src/converters/` — One file per format (20 converters: pdf, docx, pptx, xlsx, html, epub, ipynb, rss, image, audio, csv, json, xml, yaml, zip, github, wikipedia, iwork, plain-text)
- `src/commands/` — CLI commands (convert, formats, onboard)
- `src/utils/output.ts` — Chalk output helpers, triple output (json/quiet/human)

## Key Patterns

- **Converter interface**: Each converter implements `name`, `accepts(streamInfo)`, and `convert(buffer, streamInfo, options)`. Optional `convertUrl()` hook for URL-first converters (e.g. GitHub, Wikipedia).
- **Priority order**: Specific formats first (pdf, docx), generic last (plain-text as catch-all)
- **Output triple**: Every command supports `--json`, `--quiet`, and human-readable output
- **URL support**: `markit https://example.com` fetches and converts. Converters with `convertUrl()` can handle fetching themselves.
- **Optional deps**: xlsx is a dynamic import — fails gracefully with install instructions

## Adding a New Converter

1. Create `src/converters/<format>.ts`
2. Implement the `Converter` interface (name, accepts, convert)
3. Import and add to the converters array in `src/markit.ts`
4. Add to the formats list in `src/commands/formats.ts`
