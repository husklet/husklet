#!/bin/sh
set -eu

cd "$(dirname "$0")"

ROOT="$(cd ../../.. && pwd)"
GL_SHIM="$ROOT/dd-tests/guests/gl_shim.c"
DD_DISPLAY="${DD_DISPLAY:-$ROOT/target/release/dd-display}"
TMPBASE="${DD_SHADER_TRANSLATE_TMP:-$ROOT/target-chrome-codex/shader-translate-tmp}"
TMPDIR="$TMPBASE/dd-shader-translate.$$"

cleanup() {
    rm -rf "$TMPDIR"
}
trap cleanup EXIT INT TERM
mkdir -p "$TMPDIR"

cc -DDD_TR_TOOL "$GL_SHIM" -o "$TMPDIR/gl_tr"

run_pair() {
    name="$1"
    vert="$2"
    frag="$3"
    out="$TMPDIR/$name.metal"

    "$TMPDIR/gl_tr" "$vert" "$frag" > "$out"
    "$DD_DISPLAY" selftest-msl "$out"
}

run_pair chrome_local_decl chrome_local_decl.vert.glsl chrome_local_decl.frag.glsl
run_pair chrome_fragcoord_relational chrome_fragcoord_relational.vert.glsl chrome_fragcoord_relational.frag.glsl

echo "[PASS] shader translator Chrome regressions"
