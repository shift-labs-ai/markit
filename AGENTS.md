# markit

Convert anything to markdown. PDF, DOCX, PPTX, XLSX, HTML, EPUB, Jupyter notebooks, RSS, images, audio, URLs, Wikipedia, GitHub, iWork, XML, YAML, CSV, JSON, ZIP — everything gets milled. Rust is the sole conversion engine; TypeScript is only the thin Node SDK and CLI shell, and the browser package is the same engine compiled to WebAssembly.

## Commands

```bash
bun install                            # JS deps (CI: --frozen-lockfile)
bun run build:native                   # Build the Rust N-API addon
bun run build:wasm                     # Build the browser package into npm/wasm/
bun run build                          # tsc the Node SDK/CLI into dist/
bun run dev -- <file-or-url>           # Dev — convert through native Rust
bun run dev -- convert <file-or-url>   # Explicit convert command
bun run dev -- formats                 # List supported formats
bun test                               # Node SDK contract tests (+ wasm parity once built)
bun run check                          # Biome lint + format (thin TS shell)
cd rust && cargo test                  # Engine tests
cd rust && cargo run -- <file-or-url>  # Standalone Rust CLI, no Node
bun run check:rust                     # rustfmt --check + clippy -D warnings
bun run check:all                      # Both sides
bun run verify                         # Everything CI runs, before a release
```

The sole conversion implementation lives in `rust/`; `src/` is only the typed
N-API loader and npm CLI shell. Run Rust tests for conversion changes and both
suites for binding/public API changes. Lint policy is in `rust/Cargo.toml`
`[lints.clippy]`.

`dev` and `bun test` load the compiled addon, never the Rust source: rebuild
with `build:native` after engine changes. If the repo-root `.node` file is
missing, `native.cjs` silently falls back to an installed
`@shiftlabs/markit-*` prebuilt. If `bun install` skips Biome's platform
binary, run `npm install --no-save @biomejs/cli-linux-x64` (CI does).

`build:wasm` needs `rustup target add wasm32-unknown-unknown` and
`wasm-bindgen-cli` at exactly the `wasm-bindgen` version pinned in
`rust/Cargo.toml` (CI pins the same one). `src/wasm.test.ts` checks the wasm
build matches native output byte for byte, and skips when `npm/wasm/` has not
been built. Lint the wasm-only code with
`cd rust && cargo clippy --lib --target wasm32-unknown-unknown --no-default-features --features wasm -- -D warnings`.

## Architecture

- `rust/src/markit.rs` — sole converter registry and URL/file/buffer SDK
- `rust/src/converters/` — one Rust implementation per format
- `rust/src/bindings.rs` — async N-API surface (`napi` feature)
- `rust/src/wasm.rs` — wasm-bindgen browser surface (`wasm` feature): `convert(bytes, info)`
- `rust/src/main.rs` / `rust/src/commands/` — standalone Rust CLI (`cli` feature, default)
- `src/native-markit.ts` — thin typed wrappers over the native addon
- `src/main.ts` / `src/commands/` — npm CLI shell
- `src/types.ts` — public Node SDK interfaces
- `native.cjs` — platform-native addon loader
- `npm/<platform>/` — the six native binary packages; `npm/wasm/` — `@shiftlabs/markit-wasm` (only its `package.json` is committed)
- `legacy/markit-ai/` — deprecated shim re-exporting the SDK under the old name
- `benchmark/` — PDF benchmark harness; `skills/markit/` — the agent skill

## Key Patterns

- **Converter interface**: Each Rust converter implements `name`, `accepts(info)`, and `convert(input, info)`. Optional `convert_url()` handles URL-first converters (`convertUrl()` on the Node side).
- **Priority order**: Specific formats first (pdf, docx), generic last (plain-text as catch-all)
- **Output triple**: Every command supports `--json`, `--quiet`, and human-readable output
- **URL support**: `markit https://example.com` fetches and converts. Converters with `convert_url()` can handle fetching themselves.
- **wasm32 stays pure Rust**: C and networking dependencies (`ureq`, `mimalloc`, zip's `zstd`/`aes-crypto`) live under `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`, and code that uses them is `#[cfg]`-gated with a wasm fallback that errors clearly. A new dependency must build for `wasm32-unknown-unknown` or be gated the same way. The wasm build has no network or filesystem: no URL fetching, no `imageDir`, no `MARKIT_PDF_PASSWORD`.

## Adding a New Converter

1. Create `rust/src/converters/<format>.rs`
2. Implement the Rust `Converter` trait
3. Add it to `rust/src/markit.rs`
4. Expose a named Node wrapper in `src/native-markit.ts` when public
5. Add it to both formats lists: `src/commands/formats.ts` and `rust/src/commands/formats.rs`
6. Add a sample to `src/wasm.test.ts` so the browser build is checked against native
