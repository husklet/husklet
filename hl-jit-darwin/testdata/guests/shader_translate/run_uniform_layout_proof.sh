#!/bin/sh
# Proves the shim's uniform byte offsets (uni_layout) match Metal's real MSL struct layout, WITHOUT needing
# a Mac: gl_tr --print-layout is diffed against offsets the C compiler derives from MSL-faithful types
# (layout_proof.c). A mismatch — e.g. sizing a mat3 as 36B instead of Metal's 48B — fails the diff.
set -eu

cd "$(dirname "$0")"

ROOT="$(cd ../../.. && pwd)"
GL_SHIM="$ROOT/dd-tests/guests/gl_shim.c"
TMPBASE="${HL_SHADER_TRANSLATE_TMP:-$ROOT/target-chrome-codex/shader-translate-tmp}"
TMPDIR="$TMPBASE/dd-uniform-layout.$$"

cleanup() { rm -rf "$TMPDIR"; }
trap cleanup EXIT INT TERM
mkdir -p "$TMPDIR"

cc -DDD_TR_TOOL "$GL_SHIM" -o "$TMPDIR/gl_tr"
cc layout_proof.c -o "$TMPDIR/layout_proof"

"$TMPDIR/gl_tr" chrome_uniform_layout.vert.glsl chrome_uniform_layout.frag.glsl --print-layout > "$TMPDIR/shim.txt"
"$TMPDIR/layout_proof" > "$TMPDIR/truth.txt"

if ! diff -u "$TMPDIR/truth.txt" "$TMPDIR/shim.txt"; then
    echo "[FAIL] uni_layout() byte offsets DO NOT match Metal's MSL struct layout" >&2
    exit 1
fi

echo "[PASS] uni_layout() byte offsets match Metal MSL struct layout"
