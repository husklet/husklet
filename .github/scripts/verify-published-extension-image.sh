#!/usr/bin/env bash
set -euo pipefail

image="${1:?published image is required}"
version="${2:?version is required}"
kind="${3:?base or extension name is required}"
root="$(git rev-parse --show-toplevel)"
smoke="${HUSKLET_IMAGE_SMOKE:-$root/.github/scripts/smoke-extension-image.sh}"

manifest="$(docker buildx imagetools inspect --raw "$image")"
MANIFEST="$manifest" IMAGE="$image" node <<'NODE'
const document = JSON.parse(process.env.MANIFEST);
const manifests = Array.isArray(document.manifests) ? document.manifests : [];
const actual = new Set(manifests
  .map(({ platform = {} }) => `${platform.os || ''}/${platform.architecture || ''}`)
  .filter((platform) => platform !== '/' && !platform.endsWith('/unknown')));
const expected = new Set(['linux/amd64', 'linux/arm64']);
if (actual.size !== expected.size || [...expected].some((platform) => !actual.has(platform))) {
  throw new Error(`${process.env.IMAGE} publishes [${[...actual].sort()}], expected exactly linux/amd64,linux/arm64`);
}
NODE

for architecture in amd64 arm64; do
  docker pull --platform "linux/$architecture" "$image"
  "$smoke" "$image" "$version" "$kind" "$architecture"
done

echo "$image published manifest and both registry images are valid"
