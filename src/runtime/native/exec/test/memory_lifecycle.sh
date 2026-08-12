#!/usr/bin/env bash
set -euo pipefail

script_directory=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository=$(CDPATH= cd -- "$script_directory/../../../../.." && pwd)
cd "$repository"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/hl-native-memory.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM
export CARGO_TARGET_DIR="$scratch/target"
export HL_NATIVE_ALLOCATION_TEST=1
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
cargo build --offline --locked -q -p hl-engine
archive=$(find "$CARGO_TARGET_DIR/debug/build" -path '*/out/libhl_native_execution.a' -print -quit)
test -n "$archive"
entry=$(find "$CARGO_TARGET_DIR/debug/build" -path '*/out/libhl_native_execution_entry.a' -print -quit)
entry_argument=()
if [[ -n "$entry" ]]; then
    entry_argument+=("$entry")
fi
cc -std=c11 -Wall -Wextra -Werror -fsanitize=address \
    -I "$repository/src/runtime/native/exec/include" \
    -I "$repository/src/runtime/native/exec/src" \
    -I "$repository/src/runtime/native/cpu/include" \
    "$repository/src/runtime/native/exec/test/memory_lifecycle.c" "$archive" "${entry_argument[@]}" -lpthread \
    -o "$scratch/memory_lifecycle"
ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 HL_NATIVE_SKIP_RESOURCE_COUNTS=1 "$scratch/memory_lifecycle"
cc -std=c11 -Wall -Wextra -Werror \
    -I "$repository/src/runtime/native/exec/include" \
    -I "$repository/src/runtime/native/exec/src" \
    -I "$repository/src/runtime/native/cpu/include" \
    "$repository/src/runtime/native/exec/test/memory_lifecycle.c" "$archive" "${entry_argument[@]}" -lpthread \
    -o "$scratch/memory_lifecycle_resources"
"$scratch/memory_lifecycle_resources"
