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
  horrible)
    python3 "$ROOT/benchmark/bench.py" horrible "${@:2}"
    python3 "$ROOT/benchmark/score_horrible.py" "$ROOT/benchmark/results/horrible"
    ;;
  *)
    echo "usage: benchmark/run.sh {office|olmocr|horrible} [options]" >&2
    exit 2
    ;;
esac
