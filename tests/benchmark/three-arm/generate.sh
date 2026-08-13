#!/usr/bin/env bash
set -euo pipefail

readonly image='alpine@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce'
readonly image_id='sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce'
readonly source_dir="$(cd -- "$(dirname -- "$0")" && pwd -P)"
readonly workspace="$(cd -- "$source_dir/../../.." && pwd -P)"
readonly linux_cc="${THREE_ARM_LINUX_CC:-x86_64-linux-gnu-gcc}"
readonly testing="${THREE_ARM_TESTING:-$workspace/target/debug/testing}"
readonly testing_library_path="${THREE_ARM_TESTING_LIBRARY_PATH:-}"

mac_run() {
    local command
    printf -v command '%q ' "$@"
    /usr/local/bin/mac sh -lc "$command"
}

if [[ $# -ne 1 || "$1" != /* ]]; then
    echo "usage: $0 /absolute/new/output-directory" >&2
    exit 64
fi
readonly output=$1
if [[ -e "$output" ]]; then
    echo "output must not exist: $output" >&2
    exit 65
fi
case "$output" in
    "$workspace"/*) ;;
    *) echo "output must be beneath workspace $workspace" >&2; exit 65 ;;
esac
if [[ ! -x "$testing" ]]; then
    echo "testing binary is unavailable; build it or set THREE_ARM_TESTING: $testing" >&2
    exit 66
fi

mkdir -p "$output/rootfs/benchmark" "$output/native" "$output/tools"
"$linux_cc" -O3 -static "$source_dir/malloc_plain.c" -o "$output/rootfs/benchmark/malloc-plain"
mac_run clang -O3 -arch x86_64 "$source_dir/malloc_plain.c" -o "$output/native/malloc-plain"
mac_run cp /usr/bin/arch "$output/tools/arch"
mac_run cp /usr/local/bin/docker "$output/tools/docker"

readonly observed_image_id="$(mac_run "$output/tools/docker" image inspect "$image" --format '{{.Id}}')"
if [[ "$observed_image_id" != "$image_id" ]]; then
    echo "pinned image identity mismatch: expected $image_id observed $observed_image_id" >&2
    exit 66
fi

mac_run "$output/tools/arch" -x86_64 "$output/native/malloc-plain" >"$output/native.out"
mac_run "$output/tools/docker" run --rm --platform linux/amd64 \
    --mount "type=bind,source=$output/rootfs,target=$output/rootfs,readonly" \
    "$image" "$output/rootfs/benchmark/malloc-plain" >"$output/docker.out"
sed -E 's/us=[0-9]+/us=<time>/g' "$output/native.out" >"$output/native.frame"
sed -E 's/us=[0-9]+/us=<time>/g' "$output/docker.out" >"$output/docker.frame"
cmp "$output/native.frame" "$output/docker.frame"

{
    printf 'artifact\tidentity\n'
    for artifact in \
        "$output/rootfs" \
        "$output/rootfs/benchmark/malloc-plain" \
        "$output/native/malloc-plain" \
        "$output/tools/arch" \
        "$output/tools/docker"
    do
        printf '%s\t' "$artifact"
        LD_LIBRARY_PATH="$testing_library_path" "$testing" benchmark-hash "$artifact"
    done
    printf 'docker-image\t%s\n' "$observed_image_id"
} >"$output/artifacts.tsv"
cat >"$output/BLOCKERS.txt" <<EOF
Campaign not emitted: the strict schema requires real malloc/python/sqlite workloads across plain/sqlite layouts.
Available and exact-output matched: malloc/plain Linux x86_64 ELF and x86_64 Mach-O.
Missing: malloc/sqlite, python/plain, python/sqlite, sqlite/sqlite paired artifacts and their declared phases.
Missing: selected, built Husklet x86 command profile and its smoke proof.
Pinned Docker image: $image ($image_id).
EOF
printf 'READY malloc/plain\nBLOCKED campaign: see %s/BLOCKERS.txt\n' "$output"
