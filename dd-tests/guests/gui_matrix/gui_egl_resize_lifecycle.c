#include "gui_egl_probe.h"

static int clear_swap(struct ge_egl *egl, EGLSurface surface, int width, int height,
                      float red, float green, float blue, const char *name, const char *step) {
    glViewport(0, 0, width, height);
    glClearColor(red, green, blue, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (!eglSwapBuffers(egl->display, surface)) {
        printf("%s %s_swap=0 err=0x%x\n", name, step, eglGetError());
        return -1;
    }
    return 0;
}

int main(void) {
    const char *name = "gui_egl_resize_lifecycle";
    struct gp_conn c;
    struct gp_events ev;
    int r = ge_xdg_connect(&c, &ev, name);
    if (r != 0) {
        printf("%s xdg_configure=0 step=%d\n", name, r);
        return r;
    }

    struct ge_egl egl;
    r = ge_egl_init(&egl, name);
    if (r != 0) return r;

    void *window = wl_egl_window_create((void *)(uintptr_t)GP_SURFACE, 128, 80);
    if (!window) {
        printf("%s first_window=0\n", name);
        ge_egl_fini(&egl);
        return 7;
    }
    EGLSurface surface = ge_create_surface(&egl, window, name);
    if (!surface) {
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 8;
    }
    if (clear_swap(&egl, surface, 128, 80, 0.12f, 0.18f, 0.28f, name, "initial") != 0) {
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 9;
    }

    wl_egl_window_resize(window, 224, 144, 5, 7);
    int attached_w = 0;
    int attached_h = 0;
    wl_egl_window_get_attached_size(window, &attached_w, &attached_h);
    if (attached_w != 224 || attached_h != 144) {
        printf("%s resize_attached=%dx%d expected=224x144\n", name, attached_w, attached_h);
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 10;
    }

    eglDestroySurface(egl.display, surface);
    surface = ge_create_surface(&egl, window, name);
    if (!surface) {
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 11;
    }
    if (clear_swap(&egl, surface, 224, 144, 0.18f, 0.09f, 0.34f, name, "resized") != 0) {
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 12;
    }
    eglDestroySurface(egl.display, surface);
    wl_egl_window_destroy(window);

    void *window2 = wl_egl_window_create((void *)(uintptr_t)GP_SURFACE, 96, 160);
    if (!window2) {
        printf("%s second_window=0\n", name);
        ge_egl_fini(&egl);
        return 13;
    }
    EGLSurface surface2 = ge_create_surface(&egl, window2, name);
    if (!surface2) {
        wl_egl_window_destroy(window2);
        ge_egl_fini(&egl);
        return 14;
    }
    if (clear_swap(&egl, surface2, 96, 160, 0.06f, 0.24f, 0.16f, name, "recreated") != 0) {
        wl_egl_window_destroy(window2);
        ge_egl_fini(&egl);
        return 15;
    }

    eglDestroySurface(egl.display, surface2);
    wl_egl_window_destroy(window2);
    ge_egl_fini(&egl);
    printf("%s configure=%u egl=%d.%d swaps=3 resize_attached=224x144 recreate=1\n",
           name, ev.xdg_configure_serial, egl.major, egl.minor);
    return 0;
}
