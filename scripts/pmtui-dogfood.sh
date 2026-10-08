#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SIZES="80x24,100x28,120x32"
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
OUTPUT="target/pmtui-ui/$STAMP"
PRINT_MAIN=1

usage() {
  cat <<'EOF'
Usage: scripts/pmtui-dogfood.sh [options]

Render the real pmtui dashboard on isolated tmux sockets and save reviewable
plain-text plus ANSI frames. No live registry, pmd, Claude, or Codex is used.

Options:
  --size WIDTHxHEIGHT       render one terminal size
  --sizes A,B,C             render a comma-separated size matrix
  --output DIR              capture directory (default: target/pmtui-ui/<UTC>)
  --quiet                   do not print the main frames after capture
  -h, --help                show this help

Laptop defaults: 80x24 constrained, 100x28 primary, 120x32 maximized.
EOF
}

while (($#)); do
  case "$1" in
    --size)
      [[ $# -ge 2 ]] || { echo "--size requires WIDTHxHEIGHT" >&2; exit 2; }
      SIZES=$2
      shift 2
      ;;
    --sizes)
      [[ $# -ge 2 ]] || { echo "--sizes requires a comma-separated matrix" >&2; exit 2; }
      SIZES=$2
      shift 2
      ;;
    --output)
      [[ $# -ge 2 ]] || { echo "--output requires a directory" >&2; exit 2; }
      OUTPUT=$2
      shift 2
      ;;
    --quiet)
      PRINT_MAIN=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ $OUTPUT != /* ]]; then
  OUTPUT="$ROOT/$OUTPUT"
fi

mkdir -p "$OUTPUT"
cd "$ROOT"

PMTUI_UI_OUT="$OUTPUT" PMTUI_UI_SIZES="$SIZES" \
  cargo test --test integration \
    ui_gallery::laptop_ui_gallery_renders_real_main_interaction_states -- \
    --ignored --exact --nocapture --test-threads=1

echo "pmtui UI captures: $OUTPUT"
echo "  *.txt  plain terminal frames"
echo "  *.ansi color-preserving terminal frames"

if ((PRINT_MAIN)); then
  for frame in "$OUTPUT"/*-main.txt; do
    [[ -e $frame ]] || continue
    printf '\n===== %s =====\n' "$(basename "$frame")"
    sed -e 's/[[:space:]]*$//' "$frame"
  done
fi
