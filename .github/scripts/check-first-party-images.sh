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

expect_literal extensions/react/Dockerfile 'ARG HUSKLET_REACT_VERSION'
expect_literal extensions/react/Dockerfile '    && npm pkg set type=module \'
# shellcheck disable=SC2016 # These are literal Dockerfile variable references.
expect_literal extensions/react/Dockerfile 'LABEL org.opencontainers.image.version="${HUSKLET_REACT_VERSION}"'

workflow="$root/.github/workflows/release.yml"
[[ "$(grep -Fc 'platforms: linux/amd64,linux/arm64' "$workflow")" == 2 ]] \
  || fail "release must publish exactly two multi-architecture image manifests"
[[ "$(grep -Fc 'for architecture in amd64 arm64; do' "$workflow")" == 2 ]] \
  || fail "release must build both architectures before both image publication steps"
[[ "$(grep -Fc '.github/scripts/smoke-extension-image.sh' "$workflow")" == 2 ]] \
  || fail "release must run the packaged-image smoke before both publication steps"
[[ "$(grep -Fc '.github/scripts/verify-published-extension-image.sh' "$workflow")" == 2 ]] \
  || fail "release must verify both published multi-architecture registry manifests"

for extension in storybook workspace-manager; do
  dockerfile="extensions/$extension/Dockerfile"
  manifest="extensions/$extension/extension.toml"

  expect_literal "$dockerfile" 'ARG HUSKLET_REACT_IMAGE'
  # shellcheck disable=SC2016 # This is a literal Dockerfile variable reference.
  expect_literal "$dockerfile" 'FROM ${HUSKLET_REACT_IMAGE}'
  expect_literal "$dockerfile" 'ARG HUSKLET_EXTENSION_VERSION'
  expect_literal "$dockerfile" 'ARG HUSKLET_REACT_VERSION'
  expect_literal "$dockerfile" 'LABEL husklet.extension.manifest="/etc/husklet/extension.toml"'
  # shellcheck disable=SC2016 # This is a literal Dockerfile variable reference.
  expect_literal "$dockerfile" 'LABEL org.opencontainers.image.version="${HUSKLET_EXTENSION_VERSION}"'

  [[ "$(sed -n 's/^name = "\([^"]*\)"$/\1/p' "$root/$manifest")" == "$extension" ]] \
    || fail "$manifest must declare name = \"$extension\""
  [[ "$(sed -n 's/^protocol = \([0-9][0-9]*\)$/\1/p' "$root/$manifest")" == 1 ]] \
    || fail "$manifest must declare protocol = 1"
  [[ "$(grep -c '^version = "[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*"$' "$root/$manifest")" == 1 ]] \
    || fail "$manifest must contain one static X.Y.Z version for source installs"
done

echo "first-party image Dockerfile and manifest contracts are valid"
