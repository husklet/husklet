/*
 * libwayland-egl.so.1 — the wayland-egl ABI a real Wayland GUI app links against.
 *
 * A wayland-egl app (weston-simple-egl, GTK, Chrome) does NOT talk to EGL about its wl_surface directly:
 * it wraps the wl_surface in a `wl_egl_window` via this library, then hands that wl_egl_window* to
 * eglCreateWindowSurface. This library is the standard wayland-egl ABI — `wl_egl_window_create` /
 * `_resize` / `_get_attached_size` / `_destroy` — and it is deliberately a THIN wrapper: a wl_egl_window
 * is just a wl_surface plus a size. Our libEGL reads this exact struct back in eglCreateWindowSurface
 * (via the HL_WL_EGL_MAGIC tag) to size the surface + recover the wl_surface.
 *
 * This is a SEPARATE object from libEGL.so.1 (these symbols are NOT part of libEGL's 402-symbol ABI).
 * The top-level hl-gl/build.rs compiles it (linked against libc for malloc/free) and stages it as
 * ~/.hl/gl/<arch>/libwayland-egl.so.1.
 *
 * LAYOUT CONTRACT: `struct hl_wl_egl_window` MUST match `hl_gl::adapter::wayland::WlEglWindow` field for
 * field (64 bytes on LP64). The Rust side asserts size/align + the magic; the dlopen integration test
 * drives these symbols end to end, so a drift is caught at test time.
 */

#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>

/* "hlwl_egl" — must equal hl_gl::adapter::wayland::HL_WL_EGL_MAGIC. */
#define HL_WL_EGL_MAGIC ((intptr_t)0x686c776c5f65676cLL)

struct hl_wl_egl_window {
    intptr_t version;        /* HL_WL_EGL_MAGIC */
    int32_t  width;
    int32_t  height;
    int32_t  dx;
    int32_t  dy;
    int32_t  attached_width;
    int32_t  attached_height;
    void    *driver_private;
    void    *resize_cb;
    void    *destroy_cb;
    void    *surface;        /* the app's wl_surface* */
};

struct hl_wl_egl_window *
wl_egl_window_create(void *surface, int width, int height)
{
    struct hl_wl_egl_window *w;
    if (width <= 0 || height <= 0)
        return NULL;
    w = (struct hl_wl_egl_window *)calloc(1, sizeof(*w));
    if (!w)
        return NULL;
    w->version = HL_WL_EGL_MAGIC;
    w->width = width;
    w->height = height;
    w->surface = surface;
    return w;
}

void
wl_egl_window_resize(struct hl_wl_egl_window *w, int width, int height, int dx, int dy)
{
    if (!w)
        return;
    w->width = width;
    w->height = height;
    w->dx = dx;
    w->dy = dy;
}

void
wl_egl_window_get_attached_size(struct hl_wl_egl_window *w, int *width, int *height)
{
    if (!w)
        return;
    if (width)
        *width = w->attached_width != 0 ? w->attached_width : w->width;
    if (height)
        *height = w->attached_height != 0 ? w->attached_height : w->height;
}

void
wl_egl_window_destroy(struct hl_wl_egl_window *w)
{
    if (w)
        free(w);
}
