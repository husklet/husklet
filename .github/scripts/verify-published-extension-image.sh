#!/usr/bin/env bash
set -euo pipefail

image="${1:?published image is required}"
version="${2:?version is required}"
kind="${3:?base or extension name is required}"
root="$(git rev-parse --show-toplevel)"
smoke="${HUSKLET_IMAGE_SMOKE:-$root/.github/scripts/smoke-extension-image.sh}"

manifest="$(docker buildx imagetools inspect --raw "$image")"
platform_images_text="$(MANIFEST="$manifest" IMAGE="$image" node <<'NODE'
const document = JSON.parse(process.env.MANIFEST);
const manifests = Array.isArray(document.manifests) ? document.manifests : [];
const runtime = manifests
  .map(({ platform = {}, digest }) => ({ platform: `${platform.os || ''}/${platform.architecture || ''}`, digest }))
  .filter(({ platform }) => platform !== '/' && !platform.endsWith('/unknown'));
const actual = runtime.map(({ platform }) => platform);
const expected = ['linux/amd64', 'linux/arm64'];
if (actual.length !== expected.length || expected.some((platform) => actual.filter((item) => item === platform).length !== 1)) {
  throw new Error(`${process.env.IMAGE} publishes [${actual.sort()}], expected exactly one linux/amd64 and one linux/arm64 runtime descriptor`);
}
for (const platform of expected) {
  const descriptor = runtime.find((item) => item.platform === platform);
  if (!/^sha256:[0-9a-f]{64}$/.test(descriptor.digest || '')) {
    throw new Error(`${process.env.IMAGE} ${platform} descriptor has invalid digest ${descriptor.digest || '<missing>'}`);
  }
  console.log(`${process.env.IMAGE}@${descriptor.digest}`);
}
NODE
 )"
mapfile -t platform_images <<<"$platform_images_text"

architectures=(amd64 arm64)
for index in "${!architectures[@]}"; do
  architecture="${architectures[$index]}"
  platform_image="${platform_images[$index]}"
  docker pull --platform "linux/$architecture" "$platform_image"
  "$smoke" "$platform_image" "$version" "$kind" "$architecture"
done

echo "$image published manifest and both registry images are valid"
