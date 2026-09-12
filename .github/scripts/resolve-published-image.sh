#!/usr/bin/env bash
set -euo pipefail

reference="${1:?published image reference is required}"
inspection="$(docker buildx imagetools inspect "$reference")"
digest="$({ sed -n 's/^Digest:[[:space:]]*//p' <<<"$inspection"; } | head -n 1)"

[[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] || {
  echo "$reference did not resolve to an immutable OCI manifest digest" >&2
  exit 1
}

printf '%s@%s\n' "$reference" "$digest"
