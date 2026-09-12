#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/bin"

cat >"$temporary/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'buildx imagetools inspect registry.example/husklet/extension-top:1.2.3' ]]
printf 'Name:      registry.example/husklet/extension-top:1.2.3\n'
printf 'MediaType: application/vnd.oci.image.index.v1+json\n'
printf 'Digest:    %s\n' "${RESOLVED_DIGEST:?}"
EOF
chmod +x "$temporary/bin/docker"
export PATH="$temporary/bin:$PATH"

digest="sha256:$(printf 'a%.0s' {1..64})"
export RESOLVED_DIGEST="$digest"
actual="$("$root/.github/scripts/resolve-published-image.sh" registry.example/husklet/extension-top:1.2.3)"
[[ "$actual" == "registry.example/husklet/extension-top:1.2.3@$digest" ]]

export RESOLVED_DIGEST='sha256:short'
if "$root/.github/scripts/resolve-published-image.sh" registry.example/husklet/extension-top:1.2.3 >/dev/null 2>&1; then
  echo 'an invalid manifest digest was accepted' >&2
  exit 1
fi

echo 'published image identity resolver contracts pass'
