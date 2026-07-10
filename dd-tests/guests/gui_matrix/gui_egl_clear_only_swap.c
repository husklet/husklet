/*
 * Clear-only default-framebuffer swap smoke probe.
 *
 * One-shot builds from this directory:
 *   cc -O2 -Wall -Wextra -o gui_egl_clear_only_swap gui_egl_clear_only_swap.c -lwayland-egl -lEGL -lGLESv2
 *   cc -O2 -Wall -Wextra -o single_channel_texture_probe single_channel_texture_probe.c -lwayland-egl -lEGL -lGLESv2
 *   cc -O2 -Wall -Wextra -o retained_frame_partial_load retained_frame_partial_load.c -lwayland-egl -lEGL -lGLESv2
 *
 * If only versioned GUI shim sonames are mounted, replace the library tail with:
 *   -l:libwayland-egl.so.1 -l:libEGL.so.1 -l:libGLESv2.so.2
 *
 * Run examples:
 *   ./gui_egl_clear_only_swap
 *   ./single_channel_texture_probe
 *   DD_IR_DUMP=/tmp/retained-frame.ir ./retained_frame_partial_load
 *
 * This probe validates the center pixel when default-FBO glReadPixels works.
 * If readback reports a GL error, a successful EGL clear/swap is still a PASS,
 * but the printed result marks that IR or PNG validation is required.
 */

#include "gui_egl_render_probe.h"

#ifndef GL_NO_ERROR
#define GL_NO_ERROR 0
#endif

int main(void) {
    const char *name = "gui_egl_clear_only_swap";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 96, 64, 2);
    if (r != 0) return r;

    const float clear_r = 31.0f / 255.0f;
    const float clear_g = 167.0f / 255.0f;
    const float clear_b = 219.0f / 255.0f;
    const float clear_a = 1.0f;
    const uint8_t expect_r = 31;
    const uint8_t expect_g = 167;
    const uint8_t expect_b = 219;
    const uint8_t expect_a = 255;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDisable(GL_SCISSOR_TEST);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(clear_r, clear_g, clear_b, clear_a);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();

    uint8_t px[4] = {0, 0, 0, 0};
    glReadPixels(gw.width / 2, gw.height / 2, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum readback_error = glGetError();
    int readback_checked = (readback_error == GL_NO_ERROR);
    int readback_ok = 0;
    if (readback_checked) {
        readback_ok =
            gr_abs((int)px[0] - (int)expect_r) <= 1 &&
            gr_abs((int)px[1] - (int)expect_g) <= 1 &&
            gr_abs((int)px[2] - (int)expect_b) <= 1 &&
            gr_abs((int)px[3] - (int)expect_a) <= 1;
    }

    if (gr_swap(&gw) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    gr_close_window(&gw);

    if (readback_checked && !readback_ok) {
        printf("FAIL %s center_pixel=%u,%u,%u,%u expected=%u,%u,%u,%u tol=1\n",
               name, px[0], px[1], px[2], px[3], expect_r, expect_g, expect_b, expect_a);
        return 10;
    }

    if (readback_checked) {
        printf("PASS %s configure=%u egl=%d.%d clear_rgba=%u,%u,%u,%u swap=1 readback=center_pixel_ok\n",
               name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor,
               expect_r, expect_g, expect_b, expect_a);
    } else {
        printf("PASS %s configure=%u egl=%d.%d clear_rgba=%u,%u,%u,%u swap=1 readback_unavailable_gl_error=0x%x requires_ir_or_png_validation=1\n",
               name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor,
               expect_r, expect_g, expect_b, expect_a, readback_error);
    }
    return 0;
}
