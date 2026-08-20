#!/usr/bin/env bash
#
# Build each named flake check separately and, on failure, SAY WHICH ONE.
#
# `nix build .#a .#b .#c` reports one exit status for the set. When it failed on
# eb556a32b the only diagnosis available without repository admin was
# `Process completed with exit code 1` -- seven attributes, one bit. Building them
# one at a time costs nothing (Nix caches per derivation either way) and turns
# that into a name plus the first real error line.
#
# Usage: .github/scripts/flake-checks-named.sh <system> <attr>...
#
set -uo pipefail

system="$1"
shift

failed=()
for attr in "$@"; do
  log="${RUNNER_TEMP:-/tmp}/flake-${attr}.log"
  printf '\n=== %s ===\n' "$attr"
  if nix --extra-experimental-features 'nix-command flakes' build -L --no-link \
      ".#checks.${system}.${attr}" 2>&1 | tee "$log"; then
    echo "GREEN ${attr}"
  else
    echo "RED ${attr}"
    failed+=("$attr")
    # First line that looks like a compiler or builder error, for the annotation.
    detail="$(grep -oE '(error(\[E[0-9]+\])?|fatal error):.*' "$log" | head -1)"
    printf '%s\n' "  ${detail:-<no error line matched>}"
  fi
done

if [ ${#failed[@]} -ne 0 ]; then
  summary=""
  for attr in "${failed[@]}"; do
    log="${RUNNER_TEMP:-/tmp}/flake-${attr}.log"
    detail="$(grep -oE '(error(\[E[0-9]+\])?|fatal error):.*' "$log" | head -1)"
    summary+="${attr}: ${detail:-see step log}%0A"
  done
  summary="${summary//'%25'/%25}"
  echo "::error title=Flake checks failed: ${failed[*]}::${summary}"
  exit 1
fi
