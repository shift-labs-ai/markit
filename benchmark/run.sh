#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SUITE="${1:-}"

case "$SUITE" in
  office)
    set +e
    python3 "$ROOT/benchmark/bench.py" office "${@:2}"
    code=$?
    set -e
    python3 "$ROOT/benchmark/score_office.py" "$ROOT/benchmark/results/office"
    exit "$code"
    ;;
  olmocr)
    OLMOCR_ROOT="${OLMOCR_ROOT:-$ROOT/.benchmark/olmocr}"
    python3 "$ROOT/benchmark/run_olmocr.py" "$OLMOCR_ROOT" "${@:2}"
    ;;
  shitty-pdf-bench)
    python3 "$ROOT/benchmark/bench.py" shitty-pdf-bench "${@:2}"
    python3 "$ROOT/benchmark/score_shitty_pdf_bench.py" \
      "$ROOT/benchmark/results/shitty-pdf-bench"
    ;;
  *)
    echo "usage: benchmark/run.sh {office|olmocr|shitty-pdf-bench} [options]" >&2
    exit 2
    ;;
esac
