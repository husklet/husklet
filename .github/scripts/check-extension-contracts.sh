#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
specification="$root/src/workspaces/hl-extension/protocol/v1.json"

for retired in "$root/apps/storybook" "$root/src/apps/storybook"; do
  [[ ! -e "$retired" ]] || {
    echo "extension contracts: retired ${retired#"$root/"} must remain absent; Storybook lives in extensions/storybook" >&2
    exit 1
  }
done
node -e '
  const fs = require("node:fs");
  const path = require("node:path");
  const root = process.argv[1];
  const workspaces = JSON.parse(fs.readFileSync(path.join(root, "package.json"))).workspaces;
  if (!Array.isArray(workspaces)) {
    throw new Error("package.json workspaces must be an array");
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(root, "extensions/storybook/package.json")));
  if (manifest.name !== "@husklet/storybook") {
    throw new Error("extensions/storybook must remain the @husklet/storybook package");
  }
  for (const forbidden of ["client", "react", "tools"]) {
    if (fs.existsSync(path.join(root, "extensions", forbidden))) {
      throw new Error(`extensions/${forbidden} is not a runnable extension`);
    }
  }
  const walk = (directory) => fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const location = path.join(directory, entry.name);
    return entry.isDirectory() ? walk(location) : [location];
  });
  const runnable = fs.readdirSync(path.join(root, "extensions"), { withFileTypes: true })
    .filter((directory) => directory.isDirectory() && directory.name !== "base" && directory.name !== "node_modules");
  for (const directory of runnable) {
    const extension = path.join(root, "extensions", directory.name);
    if (!fs.existsSync(path.join(extension, "extension.toml"))) {
      throw new Error(`extensions/${directory.name} has no runnable extension manifest`);
    }
    const javascript = walk(path.join(extension, "src")).filter((file) => /\.[cm]?jsx?$/.test(file));
    if (javascript.length !== 0) {
      throw new Error(`extension source must be TypeScript: ${javascript.join(", ")}`);
    }
  }
  for (const directory of runnable) {
    const expected = `extensions/${directory.name}`;
    if (workspaces.filter((value) => value === expected).length !== 1) {
      throw new Error(`package.json must include runnable ${expected} exactly once`);
    }
  }
  if (fs.existsSync(path.join(root, "extensions/base/extension.toml"))) {
    throw new Error("extensions/base is a build base, not a runnable extension");
  }
' "$root"

# `cargo check` runs hl-extension's no-write source/artifact fingerprint gate.
# Comparing the native generator's stdout as well means this same script, run
# on AMD64 Linux and ARM64 macOS, proves both compilation paths produce the one
# checked-in byte stream consumed by the client generator below.
cargo check --locked --offline -q -p hl-extension
cargo run --locked --offline -q -p hl-extension --bin hl-extension-spec -- \
  | cmp - "$specification"
node "$root/packages/client/tools/protocol-spec.js"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/husklet-extension-contracts.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT

catalogue="$scratch/catalogue.json"
declarations="$scratch/index.d.ts"

cargo run --locked --offline -q --manifest-path "$root/Cargo.toml" \
  -p hl-gui --bin catalogue >"$catalogue"
HUSKLET_CATALOGUE="$catalogue" HUSKLET_DECLARATIONS="$declarations" \
  node "$root/packages/react/tools/types.js"

cmp "$catalogue" "$root/packages/react/catalogue.json"
cmp "$catalogue" "$root/extensions/storybook/src/catalogue.json"
cmp "$declarations" "$root/packages/react/src/index.d.ts"
