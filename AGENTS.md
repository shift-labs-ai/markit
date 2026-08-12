# markit

Convert anything to markdown. PDF, DOCX, PPTX, XLSX, HTML, EPUB, Jupyter notebooks, RSS, images, audio, URLs, Wikipedia, GitHub, iWork, XML, YAML, CSV, JSON, ZIP — everything gets milled. Rust is the sole conversion engine; TypeScript is only the thin Node SDK and CLI shell.

## Commands

```bash
bun run build:native                   # Build the Rust N-API addon
bun run dev -- <file-or-url>           # Dev — convert through native Rust
bun run dev -- convert <file-or-url>   # Explicit convert command
bun run dev -- formats                 # List supported formats
bun test                               # Node SDK contract tests
bun run check                          # Biome lint + format (thin TS shell)
cd rust && cargo test                  # Engine tests
bun run check:rust                     # rustfmt --check + clippy -D warnings
bun run check:all                      # Both sides
```

The sole conversion implementation lives in `rust/`; `src/` is only the typed
N-API loader and npm CLI shell. Run Rust tests for conversion changes and both
suites for binding/public API changes. Lint policy is in `rust/Cargo.toml`
`[lints.clippy]`.

## Architecture

- `rust/src/markit.rs` — sole converter registry and URL/file/buffer SDK
- `rust/src/converters/` — one Rust implementation per format
- `rust/src/bindings.rs` — async N-API surface
- `src/native-markit.ts` — thin typed wrappers over the native addon
- `src/main.ts` / `src/commands/` — npm CLI shell
- `src/types.ts` — public Node SDK interfaces
- `native.cjs` — platform-native addon loader

## Key Patterns

- **Converter interface**: Each Rust converter implements `name`, `accepts(info)`, and `convert(input, info)`. Optional `convert_url()` handles URL-first converters.
- **Priority order**: Specific formats first (pdf, docx), generic last (plain-text as catch-all)
- **Output triple**: Every command supports `--json`, `--quiet`, and human-readable output
- **URL support**: `markit https://example.com` fetches and converts. Converters with `convertUrl()` can handle fetching themselves.
- **Optional deps**: xlsx is a dynamic import — fails gracefully with install instructions

## Adding a New Converter

1. Create `rust/src/converters/<format>.rs`
2. Implement the Rust `Converter` trait
3. Add it to `rust/src/markit.rs`
4. Expose a named Node wrapper in `src/native-markit.ts` when public
5. Add it to the CLI formats list
