#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"

fail() {
  echo "first-party image contract: $*" >&2
  exit 1
}

expect_literal() {
  local file="$1"
  local text="$2"
  grep -Fqx "$text" "$root/$file" || fail "$file must contain: $text"
}

expect_literal extensions/base/Dockerfile 'ARG HUSKLET_REACT_VERSION'
expect_literal extensions/base/Dockerfile 'ARG HUSKLET_PROTOCOL_FINGERPRINT'
expect_literal extensions/base/Dockerfile 'LABEL org.opencontainers.image.source="https://github.com/husklet/husklet"'
expect_literal .dockerignore 'node_modules'
expect_literal .dockerignore '**/node_modules'
expect_literal .dockerignore 'npm-debug.log*'
expect_literal .dockerignore '**/npm-debug.log*'
expect_literal extensions/base/Dockerfile 'ARG NODE_IMAGE=node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5'
expect_literal extensions/base/Dockerfile 'ARG NODE_VERSION=22.23.2'
expect_literal extensions/base/Dockerfile 'ARG NPM_VERSION=10.9.8'
expect_literal extensions/base/Dockerfile 'COPY extensions/base/package.json extensions/base/package-lock.json ./'
expect_literal extensions/base/Dockerfile 'RUN npm install --global --ignore-scripts --no-audit --no-fund \'
expect_literal extensions/base/Dockerfile '    && ln -s "$(npm root --global)/@husklet/client" node_modules/@husklet/client \'
expect_literal extensions/base/Dockerfile '    && ln -s "$(npm root --global)/@husklet/react" node_modules/@husklet/react \'
# shellcheck disable=SC2016 # These are literal Dockerfile variable references.
expect_literal extensions/base/Dockerfile 'LABEL org.opencontainers.image.version="${HUSKLET_REACT_VERSION}"'
expect_literal extensions/base/Dockerfile 'LABEL husklet.extension.node.version="${NODE_VERSION}"'
expect_literal extensions/base/Dockerfile 'LABEL husklet.extension.protocol.fingerprint="${HUSKLET_PROTOCOL_FINGERPRINT}"'
expect_literal extensions/base/Dockerfile 'LABEL husklet.extension.npm.version="${NPM_VERSION}"'
expect_literal .github/scripts/smoke-extension-image.sh '[[ "$node_version" == 22.23.2 ]] || fail "$image does not carry the pinned Node version"'
expect_literal .github/scripts/smoke-extension-image.sh '[[ "$npm_version" == 10.9.8 ]] || fail "$image does not carry the pinned npm version"'
expect_literal .github/workflows/release.yml '              --build-arg "HUSKLET_PROTOCOL_FINGERPRINT=$protocol_fingerprint" \'
expect_literal .github/workflows/release.yml '            HUSKLET_PROTOCOL_FINGERPRINT=${{ steps.version.outputs.protocol-fingerprint }}'
expect_literal .github/scripts/smoke-extension-image.sh '      import { connect as clientConnect } from "@husklet/client";'
expect_literal .github/scripts/smoke-extension-image.sh '  [[ "$(inspect '"'"'{{json .Config.Entrypoint}}'"'"')" == '"'"'["/usr/local/bin/node"]'"'"' ]] \'
expect_literal .github/scripts/smoke-extension-image.sh '  [[ "$(inspect '"'"'{{json .Config.Cmd}}'"'"')" == '"'"'["/app/dist/main.js"]'"'"' ]] \'
expect_literal .github/scripts/smoke-extension-image.sh '      if (!fs.statSync("/app/dist/main.js").isFile()) throw new Error("entrypoint missing");'

node -e '
  const fs = require("node:fs");
  const root = process.argv[1];
  const manifest = JSON.parse(fs.readFileSync(`${root}/extensions/base/package.json`));
  const lock = JSON.parse(fs.readFileSync(`${root}/extensions/base/package-lock.json`));
  if (lock.lockfileVersion !== 3 || lock.packages[""].dependencies.react !== manifest.dependencies.react || lock.packages[""].dependencies["react-reconciler"] !== manifest.dependencies["react-reconciler"]) process.exit(1);
  for (const [name, entry] of Object.entries(lock.packages)) {
    if (name && (!entry.version || !entry.integrity)) throw new Error(`${name} is not immutable`);
    if (!name) continue;
    for (const field of ["cpu", "os", "libc"]) {
      if (entry[field] !== undefined) throw new Error(`${name} restricts ${field}; the React base must run unchanged on amd64 and arm64`);
    }
    if (entry.hasInstallScript || entry.gypfile) {
      throw new Error(`${name} requires native or lifecycle installation despite the base image using npm install --ignore-scripts`);
    }
  }
' "$root"

fingerprint="$(tr -d '\n' < "$root/src/workspaces/hl-extension/protocol/v1.fnv1a64")"
[[ "$fingerprint" =~ ^[0-9a-f]{16}$ ]] || fail "generated protocol fingerprint is not canonical"
expect_literal extensions/base/client/examples/starter/Dockerfile "ARG HUSKLET_PROTOCOL_FINGERPRINT=$fingerprint"

workflow="$root/.github/workflows/release.yml"
expect_literal .github/workflows/release.yml '          export HL_TOP_IMAGE="$(.github/scripts/resolve-published-image.sh "$top_reference")"'
expect_literal .github/workflows/release.yml '          export HL_STORYBOOK_IMAGE="$(.github/scripts/resolve-published-image.sh "$storybook_reference")"'
expect_literal src/apps/husklet/package/bundle.sh '  require_pinned_image HL_TOP_IMAGE'
expect_literal src/apps/husklet/package/bundle.sh '  require_pinned_image HL_STORYBOOK_IMAGE'
expect_literal .github/workflows/release.yml '        extension: [storybook, top]'
expect_literal .github/workflows/release.yml '        run: .github/scripts/verify-anonymous-extension-image.sh "$IMAGE"'
node -e '
  const fs = require("node:fs");
  const workflow = fs.readFileSync(process.argv[1], "utf8");
  const job = workflow.match(/^  extension-base:\n(?<body>[\s\S]*?)(?=^  [a-z][a-z0-9-]*:\n)/m)?.groups.body;
  if (!job) throw new Error("release lacks extension-base job");
  const needs = job.match(/^    needs: \[(?<jobs>[^\]]+)\]$/m)?.groups.jobs.split(",").map((item) => item.trim()) ?? [];
  if (!needs.includes("react-package")) throw new Error("React base publication must wait for the exact published npm package pair");
' "$workflow"
[[ "$(grep -Fc '.github/scripts/require-release-version.sh "$RELEASE_VERSION"' "$workflow")" == 4 ]] \
  || fail "npm packages, all images, and the application bundle must be bound to one release"
[[ "$(grep -Fc 'platforms: linux/amd64,linux/arm64' "$workflow")" == 2 ]] \
  || fail "release must publish exactly two multi-architecture image manifests"
[[ "$(grep -Fc 'for architecture in amd64 arm64; do' "$workflow")" == 2 ]] \
  || fail "release must build both architectures before both image publication steps"
[[ "$(grep -Fc '.github/scripts/smoke-extension-image.sh' "$workflow")" == 2 ]] \
  || fail "release must run the packaged-image smoke before both publication steps"
[[ "$(grep -Fc '.github/scripts/verify-published-extension-image.sh' "$workflow")" == 2 ]] \
  || fail "release must verify both published multi-architecture registry manifests"
[[ "$(grep -Fc '.github/scripts/verify-anonymous-extension-image.sh' "$workflow")" == 2 ]] \
  || fail "release must prove the base and every first-party extension are anonymously readable"

node - "$root" <<'NODE'
const fs = require('node:fs');
const path = require('node:path');
const root = process.argv[2];
const management = fs.readFileSync(
  path.join(root, 'src/apps/husklet/src/extension/management.rs'),
  'utf8',
);
if (!management.includes('"ghcr.io/husklet/husklet/extension-storybook:",\n        env!("CARGO_PKG_VERSION")')) {
  throw new Error('the development catalogue is not pinned to the application version');
}
if (management.includes('extension-storybook:latest')) {
  throw new Error('the first-party catalogue must never advertise a moving latest tag');
}
NODE

for extension in storybook top; do
  dockerfile="extensions/$extension/Dockerfile"
  manifest="extensions/$extension/extension.toml"

  expect_literal "$dockerfile" 'ARG HUSKLET_BASE_IMAGE'
  # shellcheck disable=SC2016 # This is a literal Dockerfile variable reference.
  expect_literal "$dockerfile" 'FROM ${HUSKLET_BASE_IMAGE}'
  expect_literal "$dockerfile" 'ARG HUSKLET_EXTENSION_VERSION'
  expect_literal "$dockerfile" 'ARG HUSKLET_REACT_VERSION'
  expect_literal "$dockerfile" 'ENTRYPOINT ["/usr/local/bin/node"]'
  expect_literal "$dockerfile" 'CMD ["/app/dist/main.js"]'
  expect_literal "$dockerfile" '    && test "$(node -p "require('"'"'@husklet/client/package.json'"'"').version")" = "${HUSKLET_REACT_VERSION}" \'
  expect_literal "$dockerfile" '    && test "$(node -p "require('"'"'@husklet/react/package.json'"'"').version")" = "${HUSKLET_REACT_VERSION}" \'
  expect_literal "$dockerfile" 'LABEL husklet.extension.manifest="/etc/husklet/extension.toml"'
  # shellcheck disable=SC2016 # This is a literal Dockerfile variable reference.
  expect_literal "$dockerfile" 'LABEL org.opencontainers.image.version="${HUSKLET_EXTENSION_VERSION}"'
  # shellcheck disable=SC2016 # This is a literal Dockerfile variable reference.
  expect_literal "$dockerfile" 'LABEL org.opencontainers.image.base.name="${HUSKLET_BASE_IMAGE}"'
  expect_literal "$dockerfile" 'LABEL org.opencontainers.image.source="https://github.com/husklet/husklet"'

  [[ "$(sed -n 's/^name = "\([^"]*\)"$/\1/p' "$root/$manifest")" == "$extension" ]] \
    || fail "$manifest must declare name = \"$extension\""
  [[ "$(sed -n 's/^protocol = \([0-9][0-9]*\)$/\1/p' "$root/$manifest")" == 1 ]] \
    || fail "$manifest must declare protocol = 1"
  [[ "$(grep -c '^version = "[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*"$' "$root/$manifest")" == 1 ]] \
    || fail "$manifest must contain one static X.Y.Z version for source installs"
done

"$root/.github/scripts/test-verify-anonymous-extension-image.sh"
"$root/.github/scripts/test-resolve-published-image.sh"

echo "first-party image Dockerfile and manifest contracts are valid"
