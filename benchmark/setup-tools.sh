#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TOOLS="$ROOT/.benchmark/tools"
mkdir -p "$TOOLS"

cargo build --release --manifest-path "$ROOT/rust/Cargo.toml"
npm install --prefix "$TOOLS" --no-save \
  @firecrawl/anydoc@0.1.9 \
  @llamaindex/liteparse@2.12.0

cat <<EOF
Tools ready. Export:
  export MARKIT_BIN="$ROOT/rust/target/release/markit"
  export ANYDOC_BIN="$TOOLS/node_modules/.bin/anydoc"
  export LITEPARSE_BIN="$TOOLS/node_modules/.bin/lit"
EOF
