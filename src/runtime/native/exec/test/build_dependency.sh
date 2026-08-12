#!/usr/bin/env bash
set -euo pipefail

repository=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/hl-native-rebuild.XXXXXX")
worktree="$scratch/repository"
target="$worktree/target"
cleanup() {
    git -C "$repository" worktree remove --force "$worktree" >/dev/null 2>&1 || true
    git -C "$repository" worktree prune
    rmdir "$scratch" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

git -C "$repository" worktree add --detach "$worktree" HEAD >/dev/null
cp "$repository/src/containers/hl-engine/build.rs" "$worktree/src/containers/hl-engine/build.rs"
cd "$worktree"
export CARGO_TARGET_DIR="$target"
cargo build --offline --locked -q -p testing --bin hl-compat-worker
archive=$(find "$target/debug/build" -path '*/out/libhl_native_execution.a' -print -quit)
test -n "$archive"
before=$(sha256sum "$archive" | cut -d' ' -f1)

header=src/runtime/native/exec/src/executor.h
grep -q '(1u << 16)' "$header"
sed -i 's/(1u << 16)/(1u << 15)/' "$header"
grep -q '(1u << 15)' "$header"
cargo build --offline --locked -q -p testing --bin hl-compat-worker
after=$(sha256sum "$archive" | cut -d' ' -f1)
test "$before" != "$after"
printf 'native header invalidation: %s -> %s\n' "$before" "$after"
