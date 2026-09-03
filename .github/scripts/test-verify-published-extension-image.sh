#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/bin"

cat >"$temporary/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1 $2 $3" == "buildx imagetools inspect" && "$4" == --raw ]]; then
  printf '%s' "$TEST_MANIFEST"
elif [[ "$1" == pull ]]; then
  printf '%s\n' "$*" >> "$TEST_CALLS"
else
  exit 91
fi
EOF
cat >"$temporary/smoke" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'smoke %s\n' "$*" >> "$TEST_CALLS"
EOF
chmod +x "$temporary/bin/docker" "$temporary/smoke"

export PATH="$temporary/bin:$PATH"
export TEST_CALLS="$temporary/calls"
export HUSKLET_IMAGE_SMOKE="$temporary/smoke"
export TEST_MANIFEST='{"schemaVersion":2,"manifests":[{"platform":{"os":"linux","architecture":"amd64"}},{"platform":{"os":"unknown","architecture":"unknown"}},{"platform":{"os":"linux","architecture":"arm64"}}]}'
"$root/.github/scripts/verify-published-extension-image.sh" registry.example/extension:1.2.3 1.2.3 storybook

expected="$temporary/expected"
cat >"$expected" <<'EOF'
pull --platform linux/amd64 registry.example/extension:1.2.3
smoke registry.example/extension:1.2.3 1.2.3 storybook amd64
pull --platform linux/arm64 registry.example/extension:1.2.3
smoke registry.example/extension:1.2.3 1.2.3 storybook arm64
EOF
cmp "$expected" "$TEST_CALLS"

export TEST_MANIFEST='{"schemaVersion":2,"manifests":[{"platform":{"os":"linux","architecture":"amd64"}}]}'
if "$root/.github/scripts/verify-published-extension-image.sh" registry.example/broken:1.2.3 1.2.3 base \
  >"$temporary/out" 2>"$temporary/error"; then
  echo "single-architecture manifest was accepted" >&2
  exit 1
fi
grep -Fq 'expected exactly linux/amd64,linux/arm64' "$temporary/error"

echo "published extension image verifier contracts pass"
