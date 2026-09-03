#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
specification="$root/src/workspaces/hl-extension/protocol/v1.json"

[[ ! -e "$root/apps/storybook" ]] || {
  echo "extension contracts: retired apps/storybook must remain absent; Storybook lives in extensions/storybook" >&2
  exit 1
}
node -e '
  const fs = require("node:fs");
  const path = require("node:path");
  const root = process.argv[1];
  const workspaces = JSON.parse(fs.readFileSync(path.join(root, "extensions/package.json"))).workspaces;
  if (!Array.isArray(workspaces) || workspaces.filter((value) => value === "storybook").length !== 1) {
    throw new Error("extensions/package.json must include the storybook workspace exactly once");
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(root, "extensions/storybook/package.json")));
  if (manifest.name !== "@husklet/storybook") {
    throw new Error("extensions/storybook must remain the @husklet/storybook package");
  }
' "$root"

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
