#include "gui_probe_wayland.h"

typedef int32_t EGLint;
typedef unsigned int EGLBoolean;
typedef unsigned int EGLenum;
typedef unsigned int GLenum;
typedef unsigned int GLbitfield;
typedef int GLint;
typedef int GLsizei;
typedef void *EGLDisplay;
typedef void *EGLConfig;
typedef void *EGLContext;
typedef void *EGLSurface;
typedef void *EGLNativeDisplayType;
typedef void *EGLNativeWindowType;

#define EGL_FALSE 0
#define EGL_TRUE 1
#define EGL_NONE 0x3038
#define EGL_NO_CONTEXT ((EGLContext)0)
#define EGL_NO_DISPLAY ((EGLDisplay)0)
#define EGL_OPENGL_ES_API 0x30A0
#define EGL_CONTEXT_CLIENT_VERSION 0x3098
#define EGL_SURFACE_TYPE 0x3033
#define EGL_WINDOW_BIT 0x0004
#define EGL_RENDERABLE_TYPE 0x3040
#define EGL_OPENGL_ES2_BIT 0x0004
#define GL_COLOR_BUFFER_BIT 0x4000

extern void *wl_egl_window_create(void *surface, int width, int height);
extern void wl_egl_window_destroy(void *window);
extern EGLDisplay eglGetDisplay(EGLNativeDisplayType);
extern EGLBoolean eglInitialize(EGLDisplay, EGLint *, EGLint *);
extern EGLBoolean eglBindAPI(EGLenum);
extern EGLBoolean eglChooseConfig(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *);
extern EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint *);
extern EGLSurface eglCreateWindowSurface(EGLDisplay, EGLConfig, EGLNativeWindowType, const EGLint *);
extern EGLBoolean eglMakeCurrent(EGLDisplay, EGLSurface, EGLSurface, EGLContext);
extern EGLBoolean eglSwapBuffers(EGLDisplay, EGLSurface);
extern EGLint eglGetError(void);
extern void glViewport(GLint, GLint, GLsizei, GLsizei);
extern void glClearColor(float, float, float, float);
extern void glClear(GLbitfield);

int main(void) {
    struct gp_conn c;
    struct gp_events ev;
    memset(&ev, 0, sizeof(ev));
    if (gp_connect(&c) != 0) return 1;
    if (gp_xdg_setup(&c, &ev, "gui_egl_window_swap", 0) != 1) {
        printf("gui_egl_window_swap xdg_configure=0\n");
        return 2;
    }

    EGLDisplay dpy = eglGetDisplay(EGL_NO_DISPLAY);
    EGLint maj = 0, min = 0;
    if (!dpy || !eglInitialize(dpy, &maj, &min)) {
        printf("gui_egl_window_swap eglInitialize=0 err=0x%x\n", eglGetError());
        return 3;
    }
    eglBindAPI(EGL_OPENGL_ES_API);
    EGLint cfg_attr[] = {
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_NONE
    };
    EGLConfig cfg = 0;
    EGLint ncfg = 0;
    if (!eglChooseConfig(dpy, cfg_attr, &cfg, 1, &ncfg) || ncfg < 1) {
        printf("gui_egl_window_swap choose=0 err=0x%x\n", eglGetError());
        return 4;
    }
    EGLint ctx_attr[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attr);
    if (!ctx) {
        printf("gui_egl_window_swap context=0 err=0x%x\n", eglGetError());
        return 5;
    }

    void *win = wl_egl_window_create((void *)(uintptr_t)GP_SURFACE, 160, 96);
    if (!win) {
        printf("gui_egl_window_swap wl_egl_window_create=0\n");
        return 6;
    }
    EGLSurface surf = eglCreateWindowSurface(dpy, cfg, win, NULL);
    if (!surf) {
        printf("gui_egl_window_swap window_surface=0 err=0x%x\n", eglGetError());
        return 7;
    }
    if (!eglMakeCurrent(dpy, surf, surf, ctx)) {
        printf("gui_egl_window_swap make_current=0 err=0x%x\n", eglGetError());
        return 8;
    }

    int swaps = 0;
    for (int i = 0; i < 3; i++) {
        glViewport(0, 0, 160, 96);
        glClearColor(0.12f + 0.18f * i, 0.20f, 0.42f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        if (!eglSwapBuffers(dpy, surf)) {
            printf("gui_egl_window_swap swap_%d=0 err=0x%x\n", i, eglGetError());
            wl_egl_window_destroy(win);
            return 9;
        }
        swaps++;
    }
    wl_egl_window_destroy(win);
    printf("gui_egl_window_swap xdg_configure=%u egl=%d.%d swaps=%d\n",
           ev.xdg_configure_serial, maj, min, swaps);
    return 0;
}
