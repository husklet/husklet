#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/bin"

cat >"$temporary/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'buildx imagetools inspect --raw registry.example/husklet/extension-storybook:1.2.3' ]]
[[ -n "${DOCKER_CONFIG:-}" ]]
[[ "$DOCKER_CONFIG" != "${PUBLISHER_DOCKER_CONFIG:-}" ]]
[[ -d "$DOCKER_CONFIG" ]]
[[ -z "$(find "$DOCKER_CONFIG" -mindepth 1 -print -quit)" ]]
[[ "${ANONYMOUS_READABLE:-}" == true ]]
EOF
chmod +x "$temporary/bin/docker"

export PATH="$temporary/bin:$PATH"
export PUBLISHER_DOCKER_CONFIG="$temporary/publisher-credentials"
export DOCKER_CONFIG="$PUBLISHER_DOCKER_CONFIG"
export ANONYMOUS_READABLE=true
output="$("$root/.github/scripts/verify-anonymous-extension-image.sh" \
  registry.example/husklet/extension-storybook:1.2.3)"
[[ "$output" == 'registry.example/husklet/extension-storybook:1.2.3 is anonymously readable' ]]

export ANONYMOUS_READABLE=false
if "$root/.github/scripts/verify-anonymous-extension-image.sh" \
  registry.example/husklet/extension-storybook:1.2.3 >/dev/null 2>&1; then
  echo 'an authenticated-only image passed the anonymous-read gate' >&2
  exit 1
fi

echo 'anonymous extension image verifier contracts pass'
