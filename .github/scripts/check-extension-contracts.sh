#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
specification="$root/src/workspaces/hl-extension/protocol/v1.json"

# `cargo check` runs hl-extension's no-write source/artifact fingerprint gate.
# Comparing the native generator's stdout as well means this same script, run
# on AMD64 Linux and ARM64 macOS, proves both compilation paths produce the one
# checked-in byte stream consumed by the client generator below.
cargo check --locked --offline -q -p hl-extension
cargo run --locked --offline -q -p hl-extension --bin hl-extension-spec -- \
  | cmp - "$specification"
node "$root/extensions/client/tools/protocol-spec.js"
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
