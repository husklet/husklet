#!/usr/bin/env bash
set -euo pipefail

release_version="${1:?release version is required}"
root="$(git rev-parse --show-toplevel)"
application_manifest="$root/src/apps/husklet/Cargo.toml"
application_version="$({
  sed -n '/^\[package\]$/,/^\[/{s/^version = "\([^"]*\)"$/\1/p;}' "$application_manifest"
} | head -n 1)"

[[ -n "$application_version" ]] || {
  echo "could not read the Husklet application version from $application_manifest" >&2
  exit 1
}
[[ "$release_version" == "$application_version" ]] || {
  echo "release version $release_version does not match Husklet application version $application_version" >&2
  exit 1
}
