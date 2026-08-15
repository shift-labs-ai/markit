#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-$PWD/.benchmark/olmocr}"
PYTHON="${PYTHON:-python3}"
OLMOCR_COMMIT="f7cfe4c22098b154c76b6ec950d1c0a464eecf8d"
DATA_REVISION="eaa828947384ffce68f08c223a0f5f4e2f2df624"
VENV="$ROOT/.venv"

command -v git >/dev/null || { echo "git is required" >&2; exit 1; }
command -v "$PYTHON" >/dev/null || { echo "$PYTHON is required" >&2; exit 1; }

mkdir -p "$ROOT"
if [[ ! -d "$ROOT/olmocr/.git" ]]; then
  git clone https://github.com/allenai/olmocr.git "$ROOT/olmocr"
fi
git -C "$ROOT/olmocr" fetch origin "$OLMOCR_COMMIT"
git -C "$ROOT/olmocr" checkout --detach "$OLMOCR_COMMIT"

"$PYTHON" -m venv "$VENV"
"$VENV/bin/python" -m pip install --quiet --upgrade pip
"$VENV/bin/python" -m pip install --quiet huggingface_hub
"$VENV/bin/python" -m pip install --quiet -e "$ROOT/olmocr[bench]"
"$VENV/bin/playwright" install chromium

"$VENV/bin/hf" download \
  --repo-type dataset \
  --revision "$DATA_REVISION" \
  allenai/olmOCR-bench \
  --local-dir "$ROOT/data"

cat > "$ROOT/PINNED" <<EOF
olmocr=$OLMOCR_COMMIT
dataset=$DATA_REVISION
python=$($VENV/bin/python --version 2>&1)
EOF
"$VENV/bin/python" -m pip freeze > "$ROOT/python-packages.txt"

echo "olmOCR benchmark ready at $ROOT"
echo "Run with: OLMOCR_PYTHON=$VENV/bin/python python3 benchmark/run_olmocr.py $ROOT"
