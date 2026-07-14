#!/bin/sh
set -u

cd "$(dirname "$0")"

RAW_PROBES="gui_xdg_ack gui_frame_nil gui_shm_frame gui_dmabuf_frame"
EGL_PROBES="gui_egl_surfaceless gui_egl_window_swap gui_egl_textured_quad gui_egl_resize_lifecycle gui_egl_fbo_texture_sample gui_egl_copytex_sample gui_egl_premul_blend gui_egl_scissored_quad gui_egl_vertex_formats gui_egl_vao_state gui_egl_dynamic_buffer_reuse chrome_coverage_path gui_egl_content_composite_layer gui_egl_content_composite_damage gui_egl_state_churn_vao_elements gui_egl_state_churn_pipeline gui_egl_swap_lifecycle_repeated gui_egl_swap_lifecycle_resize_recreate gui_egl_damage_scissor_readback gui_egl_copy_texture_bridge gui_egl_alpha_composite_readback gui_egl_viewport_scale_clip"
EGL_ES3_PROBES="gui_egl_blit_framebuffer gui_egl_compositor_stress gui_egl_texture_upload_formats gui_egl_texture_formats_fbo_readback"
DEFAULT_PROBES="$RAW_PROBES $EGL_PROBES"
TIMEOUT_SEC="${GUI_MATRIX_TIMEOUT:-5}"

if [ "$#" -gt 0 ]; then
    PROBES="$*"
else
    PROBES="$DEFAULT_PROBES"
fi

run_one() {
    probe="$1"
    if [ ! -x "./$probe" ]; then
        echo "[MISS] $probe is not executable; run make first"
        return 127
    fi

    echo "== $probe =="
    if command -v timeout >/dev/null 2>&1; then
        timeout "$TIMEOUT_SEC" "./$probe"
        rc=$?
    else
        "./$probe"
        rc=$?
    fi

    if [ "$rc" -eq 0 ]; then
        echo "[PASS] $probe"
    else
        echo "[FAIL] $probe rc=$rc"
    fi
    echo
    return "$rc"
}

echo "WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0}"
echo "XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/0}"
echo "HL_GPU_EXEC=${HL_GPU_EXEC:-/run/user/0/dd-gpu-0}"
echo "GUI_MATRIX_TIMEOUT=${TIMEOUT_SEC}"
echo

fail=0
for probe in $PROBES; do
    if ! run_one "$probe"; then
        fail=1
    fi
done

exit "$fail"
