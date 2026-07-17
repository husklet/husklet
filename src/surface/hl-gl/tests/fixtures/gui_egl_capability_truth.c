// Capability-advertisement gate. A GUI toolkit selects its renderer before drawing based on these
// strings and context-creation results; a false ES3/extension claim sends it into stubbed entry points.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t EGLint;
typedef uint32_t EGLBoolean;
typedef void *EGLDisplay;
typedef void *EGLConfig;
typedef void *EGLContext;

#define EGL_FALSE 0
#define EGL_TRUE 1
#define EGL_NONE 0x3038
#define EGL_SUCCESS 0x3000
#define EGL_BAD_MATCH 0x3009
#define EGL_EXTENSIONS 0x3055
#define EGL_VERSION 0x3054
#define EGL_CLIENT_APIS 0x308D
#define EGL_CONTEXT_CLIENT_VERSION 0x3098
#define EGL_CONTEXT_MINOR_VERSION_KHR 0x30FB
#define EGL_RENDERABLE_TYPE 0x3040
#define EGL_CONFORMANT 0x3042
#define EGL_OPENGL_ES2_BIT 0x0004
#define EGL_OPENGL_ES3_BIT_KHR 0x0040

extern EGLDisplay eglGetDisplay(void *native_display);
extern EGLBoolean eglInitialize(EGLDisplay, EGLint *, EGLint *);
extern const char *eglQueryString(EGLDisplay, EGLint);
extern EGLBoolean eglChooseConfig(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *);
extern EGLBoolean eglGetConfigAttrib(EGLDisplay, EGLConfig, EGLint, EGLint *);
extern EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint *);
extern EGLBoolean eglDestroyContext(EGLDisplay, EGLContext);
extern EGLint eglGetError(void);
extern void *eglGetProcAddress(const char *);

static int has_token(const char *list, const char *token) {
    size_t n = strlen(token);
    if (!list) return 0;
    for (const char *p = list; (p = strstr(p, token)) != NULL; p += n) {
        if ((p == list || p[-1] == ' ') && (p[n] == '\0' || p[n] == ' ')) return 1;
    }
    return 0;
}

static int fail(const char *why, EGLint value) {
    printf("gui_egl_capability_truth FAIL %s value=0x%x\n", why, value);
    return 1;
}

int main(void) {
    EGLDisplay dpy = eglGetDisplay(NULL);
    EGLint major = 0, minor = 0;
    if (!dpy || !eglInitialize(dpy, &major, &minor)) return fail("initialize", eglGetError());
    if (major != 1 || minor < 4) return fail("egl_version_tuple", (major << 16) | minor);

    const char *version = eglQueryString(dpy, EGL_VERSION);
    const char *apis = eglQueryString(dpy, EGL_CLIENT_APIS);
    const char *exts = eglQueryString(dpy, EGL_EXTENSIONS);
    if (!version || strncmp(version, "1.4", 3) != 0) return fail("version_string", 0);
    if (!has_token(apis, "OpenGL_ES")) return fail("client_api", 0);
    if (!has_token(exts, "EGL_KHR_create_context")) return fail("create_context_not_advertised", 0);

    // Every extension named by this shim must have its load-bearing entry point resolvable.
    if (!eglGetProcAddress("eglCreateContext")) return fail("create_context_proc", 0);

    EGLConfig cfg = NULL;
    EGLint ncfg = 0;
    EGLint cfg_attrs[] = {EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_NONE};
    if (!eglChooseConfig(dpy, cfg_attrs, &cfg, 1, &ncfg) || ncfg != 1 || !cfg)
        return fail("choose_es2", eglGetError());

    EGLint renderable = 0, conformant = 0;
    if (!eglGetConfigAttrib(dpy, cfg, EGL_RENDERABLE_TYPE, &renderable))
        return fail("renderable_query", eglGetError());
    if (!eglGetConfigAttrib(dpy, cfg, EGL_CONFORMANT, &conformant))
        return fail("conformant_query", eglGetError());
    if (!(renderable & EGL_OPENGL_ES2_BIT) || !(conformant & EGL_OPENGL_ES2_BIT))
        return fail("es2_not_advertised", renderable | conformant);

    EGLint es2_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext es2 = eglCreateContext(dpy, cfg, NULL, es2_attrs);
    if (!es2) return fail("es2_context", eglGetError());
    if (!eglDestroyContext(dpy, es2)) return fail("destroy_es2", eglGetError());

    int es3_enabled = getenv("HL_SHIM_ES3") != NULL;
    int advertises_es3 = (renderable & EGL_OPENGL_ES3_BIT_KHR) != 0 &&
                         (conformant & EGL_OPENGL_ES3_BIT_KHR) != 0;
    if (advertises_es3 != es3_enabled) return fail("es3_advertisement_env_mismatch", renderable);

    EGLint es3_attrs[] = {
        EGL_CONTEXT_CLIENT_VERSION, 3,
        EGL_CONTEXT_MINOR_VERSION_KHR, 0,
        EGL_NONE
    };
    EGLContext es3 = eglCreateContext(dpy, cfg, NULL, es3_attrs);
    EGLint es3_error = eglGetError();
    if (es3_enabled) {
        if (!es3 || es3_error != EGL_SUCCESS) return fail("enabled_es3_rejected", es3_error);
        eglDestroyContext(dpy, es3);
    } else {
        if (es3) return fail("disabled_es3_accepted", es3_error);
        if (es3_error != EGL_BAD_MATCH) return fail("disabled_es3_wrong_error", es3_error);
    }

    // EGL error is read-and-clear. Toolkits depend on the next unrelated call seeing success.
    if (eglGetError() != EGL_SUCCESS) return fail("error_not_cleared", eglGetError());

    printf("gui_egl_capability_truth PASS egl=%d.%d es2=1 es3_enabled=%d es3_advertised=%d error_clear=1\n",
           major, minor, es3_enabled, advertises_es3);
    return 0;
}
