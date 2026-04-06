#!/usr/bin/env bash
# Downloads test fixtures from GitHub release if not already present.
set -euo pipefail

REPO="Michaelliv/markit"
TAG="test-fixtures-v1"
ASSET="markit-test-fixtures.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${ASSET}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MARKER="$ROOT/test/fixtures/pdfs/intel-743621-007.pdf"

if [ -f "$MARKER" ]; then
  echo "✓ Test fixtures already present"
  exit 0
fi

echo "Downloading test fixtures from ${URL}..."
curl -fSL --retry 3 "$URL" -o "/tmp/${ASSET}"

echo "Extracting..."
tar xzf "/tmp/${ASSET}" -C "$ROOT"
rm -f "/tmp/${ASSET}"

echo "✓ Test fixtures downloaded"
