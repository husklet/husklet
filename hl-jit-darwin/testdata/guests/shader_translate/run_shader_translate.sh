#!/bin/sh
set -eu

cd "$(dirname "$0")"

ROOT="$(cd ../../.. && pwd)"
GL_SHIM="$ROOT/dd-tests/guests/gl_shim.c"
HL_DISPLAY="${HL_DISPLAY:-$ROOT/target/release/dd-display}"
TMPBASE="${HL_SHADER_TRANSLATE_TMP:-$ROOT/target-chrome-codex/shader-translate-tmp}"
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
    "$HL_DISPLAY" selftest-msl "$out"
}

run_pair chrome_local_decl chrome_local_decl.vert.glsl chrome_local_decl.frag.glsl
run_pair chrome_fragcoord_relational chrome_fragcoord_relational.vert.glsl chrome_fragcoord_relational.frag.glsl
# mat3 uniform followed by more uniforms (float3x3 = 48B in MSL) + a spread of square/non-square matrices —
# the emitted Uniforms struct and matrix ops must compile as valid MSL.
run_pair chrome_mat3_uniforms chrome_mat3_uniforms.vert.glsl chrome_mat3_uniforms.frag.glsl
run_pair chrome_uniform_layout chrome_uniform_layout.vert.glsl chrome_uniform_layout.frag.glsl
run_pair chrome_matrix_types chrome_matrix_types.vert.glsl chrome_matrix_types.frag.glsl
# GLSL-ES builtins with a different MSL spelling: mod / dFdx / dFdy / inversesqrt / atan(y,x).
run_pair chrome_builtins chrome_builtins.vert.glsl chrome_builtins.frag.glsl

# Proves uni_layout()'s byte offsets match Metal's real struct layout (no Mac required).
sh ./run_uniform_layout_proof.sh

echo "[PASS] shader translator Chrome regressions"
