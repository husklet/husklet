// EGL context identity, share-group isolation, and thread-current conformance. Explicit red gate:
// current hl-shim-gl uses one process-global GL state and one sentinel context handle.
#include "gui_egl_probe.h"

#include <pthread.h>

#define EGL_SUCCESS 0x3000

extern EGLContext eglGetCurrentContext(void);
extern EGLDisplay eglGetCurrentDisplay(void);
extern GLboolean glIsBuffer(GLuint);
extern void glDeleteBuffers(GLsizei, const GLuint *);

struct thread_check {
    EGLDisplay display;
    EGLContext context;
    pthread_barrier_t *barrier;
    int make_ok;
    int current_ok;
    int display_ok;
    int unbind_ok;
};

static void *check_thread_current(void *opaque) {
    struct thread_check *c = opaque;
    c->make_ok = eglMakeCurrent(c->display, EGL_NO_SURFACE, EGL_NO_SURFACE, c->context) == EGL_TRUE;
    pthread_barrier_wait(c->barrier);
    c->current_ok = eglGetCurrentContext() == c->context;
    c->display_ok = eglGetCurrentDisplay() == c->display;
    pthread_barrier_wait(c->barrier);
    c->unbind_ok = eglMakeCurrent(c->display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT) == EGL_TRUE &&
                   eglGetCurrentContext() == EGL_NO_CONTEXT;
    return NULL;
}

static EGLContext create_es2(EGLDisplay dpy, EGLConfig cfg, EGLContext share) {
    EGLint attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    return eglCreateContext(dpy, cfg, share, attrs);
}

int main(void) {
    const char *name = "gui_egl_sharegroup_threads";
    struct ge_egl egl;
    int r = ge_egl_init(&egl, name);
    if (r != 0) return r;
    EGLContext a = egl.context;
    EGLContext b = create_es2(egl.display, egl.config, a);
    EGLContext c = create_es2(egl.display, egl.config, EGL_NO_CONTEXT);
    if (!b || !c) {
        printf("%s FAIL create_contexts error=0x%x\n", name, eglGetError());
        ge_egl_fini(&egl);
        return 7;
    }
    if (a == b || a == c || b == c) {
        printf("%s FAIL context_identity a=%p b=%p c=%p\n", name, a, b, c);
        ge_egl_fini(&egl);
        return 8;
    }

    // A and B share object namespaces. C must not see A's object even if it allocates the same numeric name.
    if (!eglMakeCurrent(egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, a)) return 9;
    GLuint shared_buffer = 0;
    glGenBuffers(1, &shared_buffer);
    glBindBuffer(GL_ARRAY_BUFFER, shared_buffer);
    const uint32_t payload = 0x12345678u;
    glBufferData(GL_ARRAY_BUFFER, sizeof(payload), &payload, GL_STATIC_DRAW);
    if (!shared_buffer || !glIsBuffer(shared_buffer)) {
        printf("%s FAIL context_a_buffer=%u\n", name, shared_buffer);
        return 10;
    }

    if (!eglMakeCurrent(egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, b)) return 11;
    if (!glIsBuffer(shared_buffer)) {
        printf("%s FAIL shared_context_cannot_see buffer=%u\n", name, shared_buffer);
        return 12;
    }

    if (!eglMakeCurrent(egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, c)) return 13;
    if (glIsBuffer(shared_buffer)) {
        printf("%s FAIL unshared_context_leaked buffer=%u\n", name, shared_buffer);
        return 14;
    }

    // Current bindings are per-thread. Two different contexts may be current concurrently and each thread
    // must observe its own context/display, then EGL_NO_CONTEXT after unbinding.
    eglMakeCurrent(egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    pthread_barrier_t barrier;
    if (pthread_barrier_init(&barrier, NULL, 2) != 0) return 15;
    struct thread_check ta = {egl.display, a, &barrier, 0, 0, 0, 0};
    struct thread_check tb = {egl.display, b, &barrier, 0, 0, 0, 0};
    pthread_t th_a, th_b;
    if (pthread_create(&th_a, NULL, check_thread_current, &ta) != 0 ||
        pthread_create(&th_b, NULL, check_thread_current, &tb) != 0) return 16;
    pthread_join(th_a, NULL);
    pthread_join(th_b, NULL);
    pthread_barrier_destroy(&barrier);
    if (!ta.make_ok || !ta.current_ok || !ta.display_ok || !ta.unbind_ok ||
        !tb.make_ok || !tb.current_ok || !tb.display_ok || !tb.unbind_ok) {
        printf("%s FAIL thread_current a=%d,%d,%d,%d b=%d,%d,%d,%d\n", name,
               ta.make_ok, ta.current_ok, ta.display_ok, ta.unbind_ok,
               tb.make_ok, tb.current_ok, tb.display_ok, tb.unbind_ok);
        return 17;
    }

    // Deleting through B removes the shared object from A, but cannot affect C's independent namespace.
    eglMakeCurrent(egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, b);
    glDeleteBuffers(1, &shared_buffer);
    eglMakeCurrent(egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, a);
    if (glIsBuffer(shared_buffer)) {
        printf("%s FAIL shared_delete_not_visible buffer=%u\n", name, shared_buffer);
        return 18;
    }

    eglMakeCurrent(egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(egl.display, b);
    eglDestroyContext(egl.display, c);
    ge_egl_fini(&egl);
    if (eglGetError() != EGL_SUCCESS) {
        printf("%s FAIL final_egl_error=0x%x\n", name, eglGetError());
        return 19;
    }
    printf("%s PASS unique_contexts=3 shared_visibility=1 isolation=1 thread_current=2 shared_delete=1\n", name);
    return 0;
}
