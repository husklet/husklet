#!/usr/bin/env bash
set -euo pipefail

image="${1:?published image is required}"
anonymous_config="$(mktemp -d)"
trap 'rm -rf "$anonymous_config"' EXIT

# A release is consumed by an unsigned-in user. Use an empty Docker configuration so a
# successful probe cannot be supplied by the publishing job's GHCR credentials.
DOCKER_CONFIG="$anonymous_config" docker buildx imagetools inspect --raw "$image" >/dev/null

echo "$image is anonymously readable"
