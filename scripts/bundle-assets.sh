#!/bin/bash
# EverEvo — one-click asset bundler (macOS / Linux).
#
# Usage:
#   ./scripts/bundle.sh                               # Bundle for current host
#   ./scripts/bundle.sh --target aarch64-unknown-linux-gnu  # Bundle for ARM64 Linux
#   ./scripts/bundle.sh --all                          # Bundle for ALL 5 platforms
#   ./scripts/bundle.sh --release --skip-git --skip-reranker-cn

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

RELEASE=""
ALL=false
TARGET=""
SKIP_GIT=""
SKIP_RERANKER=""

for arg in "$@"; do
    case "$arg" in
        --release) RELEASE="--release" ;;
        --all) ALL=true ;;
        --target) shift; TARGET="$1" ;;
        --skip-git) SKIP_GIT="--skip-git" ;;
        --skip-reranker-cn) SKIP_RERANKER="--skip-reranker-cn" ;;
    esac
done

if $ALL; then
    TARGETS=(
        "x86_64-pc-windows-msvc"
        "aarch64-apple-darwin"
        "x86_64-apple-darwin"
        "x86_64-unknown-linux-gnu"
        "aarch64-unknown-linux-gnu"
    )
elif [ -n "$TARGET" ]; then
    TARGETS=("$TARGET")
else
    TARGETS=($(rustc -vV | grep "host:" | awk '{print $2}'))
fi

TOTAL=${#TARGETS[@]}
DONE=0

for t in "${TARGETS[@]}"; do
    DONE=$((DONE + 1))
    echo ""
    echo -e "\033[36m========================================"
    echo "[$DONE/$TOTAL] Target: $t"
    echo -e "========================================\033[0m"

    OUTDIR="resources/bundled/$t"
    cargo run --bin everevo-bundler $RELEASE \
        -- --target "$t" --output "$OUTDIR" $SKIP_GIT $SKIP_RERANKER

    echo -e "\033[32mOutput:\033[0m"
    ls -lh "$OUTDIR"
done

echo ""
echo -e "\033[32mDone: $TOTAL target(s) bundled.\033[0m"
