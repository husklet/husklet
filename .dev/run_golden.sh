#!/usr/bin/env bash
# Deterministic golden-image regression runner for the dd-display Metal renderer.
#
# Replays the captured dd-gpu IR streams under target-chrome-codex/*.ir through the REAL MetalBackend,
# reads each composited surface back to a PNG, and pixel-diffs it against target-chrome-codex/golden/.
# No live Chrome, no window server — reproducible on any machine with a Metal device.
#
# Usage:
#   ./run_golden.sh                 # check: PASS/FAIL per case, non-zero exit on any regression
#   ./run_golden.sh --update-goldens  # (re)write golden/*.png from this run (review the diff!)
#   ./run_golden.sh --seed          # only regenerate the synthesized *.ir captures (no Metal needed)
#
# Env passthrough: DD_GOLDEN_TOL (per-channel abs tolerance, default 4),
#   DD_GOLDEN_MAXPCT (max fraction of pixels over tol, default 0.0), DD_ORACLE_STRICT=1.
#
# Metal only runs on macOS. On the Linux dev host this delegates to the `mac` runner (same worktree
# path, shared filesystem); when already on macOS it runs cargo directly.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

SEED_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --update-goldens) export DD_UPDATE_GOLDENS=1 ;;
    --seed) SEED_ONLY=1 ;;
    -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

TEST=(cargo test -p dd-display --test golden_image_regression)

# Regenerate the synthesized captures (pure Rust, runs anywhere).
run_cargo() { bash -c "ulimit -n 100000 2>/dev/null; \"\$@\"" _ "$@"; }

if [[ "$SEED_ONLY" == 1 ]]; then
  run_cargo "${TEST[@]}" seed_captures -- --nocapture
  exit 0
fi

# The Metal golden pass. Prefer the `mac` runner if present (Linux dev host), else run locally (macOS).
CMD="ulimit -n 100000 2>/dev/null; cd '$ROOT' && ${DD_UPDATE_GOLDENS:+DD_UPDATE_GOLDENS=1 }${DD_GOLDEN_TOL:+DD_GOLDEN_TOL=$DD_GOLDEN_TOL }${DD_GOLDEN_MAXPCT:+DD_GOLDEN_MAXPCT=$DD_GOLDEN_MAXPCT }${DD_ORACLE_STRICT:+DD_ORACLE_STRICT=1 }cargo test -p dd-display --test golden_image_regression golden_suite -- --nocapture --test-threads=1"

if command -v mac >/dev/null 2>&1 && [[ "$(uname)" != "Darwin" ]]; then
  exec mac bash -lc "$CMD"
else
  exec bash -lc "$CMD"
fi
