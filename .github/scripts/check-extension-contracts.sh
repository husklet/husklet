#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/husklet-extension-contracts.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT

catalogue="$scratch/catalogue.json"
declarations="$scratch/index.d.ts"

cargo run --locked --offline -q --manifest-path "$root/Cargo.toml" \
  -p hl-gui --bin catalogue >"$catalogue"
HUSKLET_CATALOGUE="$catalogue" HUSKLET_DECLARATIONS="$declarations" \
  node "$root/extensions/react/tools/types.js"

cmp "$catalogue" "$root/extensions/react/catalogue.json"
cmp "$catalogue" "$root/extensions/storybook/src/catalogue.json"
cmp "$declarations" "$root/extensions/react/src/index.d.ts"
