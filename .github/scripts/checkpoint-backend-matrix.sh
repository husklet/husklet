#!/usr/bin/env bash
# Run already-built checkpoint test artifacts. Building and copying are intentionally outside this
# script: the caller supplies settled binaries, and the receipt binds every verdict to their bytes.
set -euo pipefail

usage() {
  echo "usage: $0 MANIFEST RECEIPT_DIR [LANE_ID ...]" >&2
  exit 64
}

[[ $# -ge 2 ]] || usage
manifest=$1
receipt_dir=$2
shift 2
[[ -f "$manifest" && ! -L "$manifest" ]] || { echo "matrix manifest is not a regular file: $manifest" >&2; exit 66; }
[[ ! -e "$receipt_dir" ]] || { echo "receipt directory already exists: $receipt_dir" >&2; exit 73; }

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

canonical_file() {
  local directory base
  directory=$(dirname "$1")
  base=$(basename "$1")
  printf '%s/%s\n' "$(cd -P "$directory" && pwd)" "$base"
}

case "$(uname -s):$(uname -m)" in
  Linux:x86_64) host=linux-x86_64 ;;
  Linux:aarch64|Linux:arm64) host=linux-aarch64 ;;
  Darwin:arm64|Darwin:aarch64) host=macos-aarch64 ;;
  *) echo "unsupported checkpoint matrix host: $(uname -m)" >&2; exit 69 ;;
esac

requested=$'\n'
for lane in "$@"; do
  [[ "$lane" != *$'\n'* && -n "$lane" ]] || usage
  requested+="$lane"$'\n'
done

stage=$(mktemp -d "${receipt_dir}.stage.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
printf 'schema\thusklet-checkpoint-backend-matrix-receipt-v1\n' >"$stage/results.tsv"
printf 'host\t%s\n' "$host" >>"$stage/results.tsv"
printf 'manifest_sha256\t%s\n' "$(hash_file "$manifest")" >>"$stage/results.tsv"

seen=0
failed=0
while IFS=$'\t' read -r id lane_host guest backend contract kind artifact_env selector set_env proof extra; do
  [[ -z "${extra:-}" ]] || { echo "invalid extra manifest column in $id" >&2; exit 65; }
  [[ -n "$id" && "$id" != \#* ]] || continue
  [[ "$lane_host" == "$host" ]] || continue
  if [[ $# -gt 0 && "$requested" != *$'\n'"$id"$'\n'* ]]; then
    continue
  fi
  seen=$((seen + 1))
  if [[ "$kind" == gap ]]; then
    printf '%s\t%s\t%s\t%s\tGAP\t-\t-\t-\t%s\n' "$id" "$guest" "$backend" "$contract" "$proof" >>"$stage/results.tsv"
    echo "GAP $id: $proof" >&2
    failed=$((failed + 1))
    continue
  fi

  artifact=$(printenv "$artifact_env" 2>/dev/null || true)
  executable=1
  [[ "$kind" == evidence ]] && executable=0
  [[ -n "$artifact" && -f "$artifact" && ! -L "$artifact" && ( $executable -eq 0 || -x "$artifact" ) ]] || {
    echo "$id: $artifact_env must name a regular $([[ $executable -eq 1 ]] && echo executable || echo evidence) file" >&2
    failed=$((failed + 1))
    printf '%s\t%s\t%s\t%s\tMISSING\t-\t-\t-\t%s\n' "$id" "$guest" "$backend" "$contract" "$artifact_env" >>"$stage/results.tsv"
    continue
  }
  artifact=$(canonical_file "$artifact")
  artifact_hash=$(hash_file "$artifact")
  stdout="$stage/$id.stdout"
  stderr="$stage/$id.stderr"

  status=0
  case "$kind" in
    rust-test)
      if [[ "$set_env" == - ]]; then
        timeout 360s "$artifact" --exact "$selector" --nocapture >"$stdout" 2>"$stderr" || status=$?
      else
        [[ "$set_env" == *=* && "$set_env" != *$'\n'* ]] || { echo "$id: invalid environment assignment" >&2; exit 65; }
        env "$set_env" timeout 360s "$artifact" --exact "$selector" --nocapture >"$stdout" 2>"$stderr" || status=$?
      fi
      ;;
    nested)
      [[ "$set_env" == - ]] || { echo "$id: nested lanes do not accept set_env" >&2; exit 65; }
      timeout 360s "$artifact" nested run "$selector" >"$stdout" 2>"$stderr" || status=$?
      ;;
    evidence)
      [[ "$set_env" == - && "$selector" == evidence ]] || { echo "$id: invalid evidence lane" >&2; exit 65; }
      cp "$artifact" "$stdout"
      : >"$stderr"
      ;;
    *) echo "$id: unknown matrix kind $kind" >&2; exit 65 ;;
  esac

  stdout_hash=$(hash_file "$stdout")
  stderr_hash=$(hash_file "$stderr")
  proof_count=$(grep -Fxc "$proof" "$stdout" || true)
  verdict=PASS
  if [[ $status -ne 0 || $proof_count -ne 1 ]]; then
    verdict=FAIL
    failed=$((failed + 1))
    echo "$id: exit=$status proof_count=$proof_count" >&2
  else
    echo "PASS $id artifact=$artifact_hash output=$stdout_hash"
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\texit=%s proof_count=%s\n' \
    "$id" "$guest" "$backend" "$contract" "$verdict" "$artifact_hash" "$stdout_hash" "$stderr_hash" "$status" "$proof_count" \
    >>"$stage/results.tsv"
done <"$manifest"

[[ $seen -gt 0 ]] || { echo "no checkpoint matrix lanes selected for $host" >&2; exit 65; }
if [[ $# -gt 0 && $seen -ne $# ]]; then
  echo "one or more requested lane IDs were absent or belong to another host" >&2
  exit 65
fi
mv "$stage" "$receipt_dir"
trap - EXIT
[[ $failed -eq 0 ]] || exit 1
