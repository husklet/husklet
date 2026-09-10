#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/bin"

cat >"$temporary/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ -n "${DOCKER_CONFIG:-}" ]]
[[ "$DOCKER_CONFIG" != "${PUBLISHER_DOCKER_CONFIG:-}" ]]
[[ -d "$DOCKER_CONFIG" ]]
[[ -z "$(find "$DOCKER_CONFIG" -mindepth 1 -print -quit)" ]]
printf '%s\n' "$*" >>"${DOCKER_CALLS:?}"
case "$1" in
  buildx)
    [[ "$*" == 'buildx imagetools inspect --raw registry.example/husklet/extension-storybook:1.2.3' ]]
    [[ "${ANONYMOUS_INDEX_READABLE:-}" == true ]]
    ;;
  pull)
    [[ "$*" == 'pull --platform linux/amd64 registry.example/husklet/extension-storybook:1.2.3' \
      || "$*" == 'pull --platform linux/arm64 registry.example/husklet/extension-storybook:1.2.3' ]]
    [[ "${ANONYMOUS_IMAGES_READABLE:-}" == true ]]
    ;;
  *) exit 64 ;;
esac
EOF
chmod +x "$temporary/bin/docker"

export PATH="$temporary/bin:$PATH"
export PUBLISHER_DOCKER_CONFIG="$temporary/publisher-credentials"
export DOCKER_CONFIG="$PUBLISHER_DOCKER_CONFIG"
export DOCKER_CALLS="$temporary/docker-calls"
export ANONYMOUS_INDEX_READABLE=true
export ANONYMOUS_IMAGES_READABLE=true
output="$("$root/.github/scripts/verify-anonymous-extension-image.sh" \
  registry.example/husklet/extension-storybook:1.2.3)"
[[ "$output" == 'registry.example/husklet/extension-storybook:1.2.3 and both runtime platforms are anonymously readable' ]]
[[ "$(grep -Fxc 'pull --platform linux/amd64 registry.example/husklet/extension-storybook:1.2.3' "$DOCKER_CALLS")" == 1 ]]
[[ "$(grep -Fxc 'pull --platform linux/arm64 registry.example/husklet/extension-storybook:1.2.3' "$DOCKER_CALLS")" == 1 ]]

export ANONYMOUS_IMAGES_READABLE=false
if "$root/.github/scripts/verify-anonymous-extension-image.sh" \
  registry.example/husklet/extension-storybook:1.2.3 >/dev/null 2>&1; then
  echo 'an image with anonymously unreadable runtime platforms passed the anonymous-read gate' >&2
  exit 1
fi

echo 'anonymous extension image verifier contracts pass'
