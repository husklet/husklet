#!/usr/bin/env bash
# rust-tool.sh -- run a cargo tool component (clippy / fmt) with a driver that matches the active rustc.
#
#   bash tools/rust-tool.sh clippy -p hl-gl --all-targets
#   bash tools/rust-tool.sh fmt --all -- --check
#
# Why this exists. `cargo clippy` and `cargo fmt` are toolchain COMPONENTS, not part of cargo: on a host
# without them, cargo answers `no such command: clippy` on stderr and exits non-zero. An agent that ran
# clippy, grepped its output for warnings and found none read that as a clean lint and reported it as one.
# A check that could not run must be distinguishable from a check that ran and found nothing, so this
# script refuses loudly rather than letting an absent tool look like a pass.
#
# The flake's dev shell provides both, but it has no aarch64-linux devShell, so work inside a Linux
# workspace runs against the distro toolchain with no components on PATH. When that happens the matching
# component is usually still on disk (the nix store has one per toolchain); this finds it by the rustc
# commit hash, because a clippy-driver built against a different rustc fails with resolution errors that
# look like source defects rather than a toolchain mismatch.
set -euo pipefail

tool="${1:?usage: rust-tool.sh <clippy|fmt> [cargo args...]}"
shift

case "$tool" in
  clippy) binary=cargo-clippy ;;
  fmt)    binary=cargo-fmt ;;
  *) echo "rust-tool.sh: unknown tool '$tool' (expected clippy or fmt)" >&2; exit 2 ;;
esac

want="$(rustc -vV | awk '/^commit-hash: /{print $2}')"
[ -n "$want" ] || { echo "rust-tool.sh: cannot read the active rustc commit hash" >&2; exit 2; }

# Already on PATH and matching? Use it.
if command -v "$binary" >/dev/null 2>&1 && cargo "$tool" --version >/dev/null 2>&1; then
  exec cargo "$tool" "$@"
fi

# Otherwise look for a complete toolchain on disk whose rustc is the SAME BUILD as the active one.
found=""
for candidate in /nix/store/*/bin/rustc; do
  [ -x "$candidate" ] || continue
  directory="$(dirname "$candidate")"
  [ -x "$directory/$binary" ] || continue
  if [ "$("$candidate" -vV | awk '/^commit-hash: /{print $2}')" = "$want" ]; then
    found="$directory"
    break
  fi
done

if [ -z "$found" ]; then
  release="$(rustc -vV | awk '/^release: /{print $2}')"
  cat >&2 <<EOF
rust-tool.sh: cargo $tool is NOT AVAILABLE and no toolchain on this host matches the active rustc
              ($(rustc -vV | head -1)).

              This is a MISSING CHECK, not a passing one. Do not report "$tool clean" from this run.
              Fix it with one of:
                * enter the dev shell (\`nix develop\`) on a host where the flake provides one;
                * \`rustup component add $tool\` if rustup manages the toolchain;
                * install the distro's $tool package matching rustc $release.
EOF
  exit 127
fi

# A component from another toolchain tree cannot reuse artifacts built by the distro one (E0514), so give
# it its own target directory rather than invalidating the shared build.
export PATH="$found:$PATH"
export CARGO_TARGET_DIR="${HL_TOOL_TARGET_DIR:-${CARGO_TARGET_DIR:-target}/rust-tool}"
echo "rust-tool.sh: using $found (matches rustc $want), CARGO_TARGET_DIR=$CARGO_TARGET_DIR" >&2
exec "$found/cargo" "$tool" "$@"
