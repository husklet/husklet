#include "gui_egl_render_probe.h"

static int clear_expect_swap(struct gr_window *gw, const char *label, int width, int height,
                             float r, float g, float b,
                             uint8_t er, uint8_t eg, uint8_t eb) {
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, width, height);
    glDisable(GL_SCISSOR_TEST);
    glClearColor(r, g, b, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();
    if (gr_expect_pixel(gw->name, label, width / 2, height / 2, er, eg, eb, 255, 5) != 0) {
        return -1;
    }
    return gr_swap(gw);
}

static int recreate_surface(struct gr_window *gw, int width, int height) {
    if (!eglMakeCurrent(gw->egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, gw->egl.context)) {
        printf("%s make_no_surface=0 err=0x%x\n", gw->name, eglGetError());
        return -1;
    }
    if (gw->surface && !eglDestroySurface(gw->egl.display, gw->surface)) {
        printf("%s destroy_surface=0 err=0x%x\n", gw->name, eglGetError());
        return -1;
    }
    gw->surface = ge_create_surface(&gw->egl, gw->window, gw->name);
    if (!gw->surface) return -1;
    gw->width = width;
    gw->height = height;
    return 0;
}

int main(void) {
    const char *name = "gui_egl_swap_lifecycle_resize_recreate";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 128, 96, 2);
    if (r != 0) return r;

    int ok = 0;
    if (clear_expect_swap(&gw, "initial", 128, 96, 0.04f, 0.11f, 0.18f, 10, 28, 46) != 0) {
        ok = -1;
    }

    wl_egl_window_resize(gw.window, 240, 150, 7, 5);
    int attached_w = 0;
    int attached_h = 0;
    wl_egl_window_get_attached_size(gw.window, &attached_w, &attached_h);
    if (attached_w != 240 || attached_h != 150) {
        printf("%s resize_attached=%dx%d expected=240x150\n", name, attached_w, attached_h);
        ok = -1;
    }

    if (recreate_surface(&gw, 240, 150) != 0 ||
        clear_expect_swap(&gw, "resized_large", 240, 150, 0.15f, 0.05f, 0.20f, 38, 13, 51) != 0) {
        ok = -1;
    }

    wl_egl_window_resize(gw.window, 96, 176, -3, 11);
    wl_egl_window_get_attached_size(gw.window, &attached_w, &attached_h);
    if (attached_w != 96 || attached_h != 176) {
        printf("%s second_resize_attached=%dx%d expected=96x176\n", name, attached_w, attached_h);
        ok = -1;
    }

    if (recreate_surface(&gw, 96, 176) != 0 ||
        clear_expect_swap(&gw, "resized_tall", 96, 176, 0.02f, 0.22f, 0.12f, 5, 56, 31) != 0) {
        ok = -1;
    }

    eglDestroySurface(gw.egl.display, gw.surface);
    gw.surface = EGL_NO_SURFACE;
    wl_egl_window_destroy(gw.window);
    gw.window = NULL;

    gw.window = wl_egl_window_create((void *)(uintptr_t)GP_SURFACE, 160, 112);
    if (!gw.window) {
        printf("%s recreated_window=0\n", name);
        ok = -1;
    } else {
        gw.surface = ge_create_surface(&gw.egl, gw.window, name);
        gw.width = 160;
        gw.height = 112;
        if (!gw.surface ||
            clear_expect_swap(&gw, "recreated_window", 160, 112, 0.20f, 0.16f, 0.04f, 51, 41, 10) != 0) {
            ok = -1;
        }
    }

    gr_close_window(&gw);
    if (ok != 0) return 9;
    printf("%s configure=%u egl=%d.%d swaps=4 resizes=2 attached=240x150,96x176 surface_recreates=3 window_recreate=1 final_pixels=4\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
