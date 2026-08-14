#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-$PWD/.benchmark/olmocr}"
OLMOCR_COMMIT="f7cfe4c22098b154c76b6ec950d1c0a464eecf8d"
DATA_REVISION="eaa828947384ffce68f08c223a0f5f4e2f2df624"

command -v git >/dev/null || { echo "git is required" >&2; exit 1; }
command -v huggingface-cli >/dev/null || {
  echo "huggingface-cli is required: pip install huggingface_hub" >&2
  exit 1
}

mkdir -p "$ROOT"
if [[ ! -d "$ROOT/olmocr/.git" ]]; then
  git clone https://github.com/allenai/olmocr.git "$ROOT/olmocr"
fi
git -C "$ROOT/olmocr" fetch origin "$OLMOCR_COMMIT"
git -C "$ROOT/olmocr" checkout --detach "$OLMOCR_COMMIT"

huggingface-cli download \
  --repo-type dataset \
  --revision "$DATA_REVISION" \
  allenai/olmOCR-bench \
  --local-dir "$ROOT/data"

cat > "$ROOT/PINNED" <<EOF
olmocr=$OLMOCR_COMMIT
dataset=$DATA_REVISION
EOF

echo "olmOCR benchmark ready at $ROOT"
