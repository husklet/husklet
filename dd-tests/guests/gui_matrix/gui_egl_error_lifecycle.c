// GLES2 error-state and non-mutation conformance probe. This is an explicit semantic gate: the Rust
// shim currently returns GL_NO_ERROR unconditionally, so keep it out of the default green matrix until
// that implementation gap is fixed. It should pass unchanged on conformant GLES2 and then become default.
#include "gui_egl_probe.h"

#define GL_NO_ERROR 0
#define GL_INVALID_ENUM 0x0500
#define GL_INVALID_VALUE 0x0501
#define GL_INVALID_OPERATION 0x0502
#define GL_VIEWPORT 0x0BA2
#define GL_ARRAY_BUFFER_BINDING 0x8894

extern void glGetIntegerv(GLenum, GLint *);

static int expect_error(const char *name, const char *step, GLenum want) {
    GLenum got = glGetError();
    if (got != want) {
        printf("%s FAIL %s error got=0x%x want=0x%x\n", name, step, got, want);
        return -1;
    }
    return 0;
}

static int same4(const GLint a[4], const GLint b[4]) {
    return a[0] == b[0] && a[1] == b[1] && a[2] == b[2] && a[3] == b[3];
}

int main(void) {
    const char *name = "gui_egl_error_lifecycle";
    struct ge_egl egl;
    int r = ge_egl_init(&egl, name);
    if (r != 0) return r;
    if (!eglMakeCurrent(egl.display, EGL_NO_SURFACE, EGL_NO_SURFACE, egl.context)) {
        printf("%s FAIL surfaceless_make_current egl_error=0x%x\n", name, eglGetError());
        ge_egl_fini(&egl);
        return 7;
    }

    int ok = 0;
    if (expect_error(name, "initial", GL_NO_ERROR) != 0) ok = -1;

    // Invalid viewport dimensions must not modify the previous viewport.
    GLint viewport_before[4] = {0}, viewport_after[4] = {0};
    glViewport(3, 5, 31, 37);
    glGetIntegerv(GL_VIEWPORT, viewport_before);
    glViewport(11, 13, -1, 17);
    if (expect_error(name, "negative_viewport", GL_INVALID_VALUE) != 0) ok = -1;
    glGetIntegerv(GL_VIEWPORT, viewport_after);
    if (!same4(viewport_before, viewport_after)) {
        printf("%s FAIL negative_viewport_mutated before=%d,%d,%d,%d after=%d,%d,%d,%d\n",
               name, viewport_before[0], viewport_before[1], viewport_before[2], viewport_before[3],
               viewport_after[0], viewport_after[1], viewport_after[2], viewport_after[3]);
        ok = -1;
    }

    // Invalid capability enum must produce INVALID_ENUM and must not alias a real enable bit.
    glDisable(GL_BLEND);
    glEnable(0xdead);
    if (expect_error(name, "invalid_enable", GL_INVALID_ENUM) != 0) ok = -1;

    // Invalid buffer target must neither bind the object nor mutate ARRAY_BUFFER_BINDING.
    GLuint buffer = 0;
    glGenBuffers(1, &buffer);
    GLint binding_before = -1, binding_after = -1;
    glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &binding_before);
    glBindBuffer(0xbeef, buffer);
    if (expect_error(name, "invalid_buffer_target", GL_INVALID_ENUM) != 0) ok = -1;
    glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &binding_after);
    if (binding_before != binding_after) {
        printf("%s FAIL invalid_bind_mutated before=%d after=%d\n", name, binding_before, binding_after);
        ok = -1;
    }

    // A negative object count is INVALID_VALUE and must not write the output sentinel.
    GLuint sentinel = 0xfeed1234u;
    glGenBuffers(-1, &sentinel);
    if (expect_error(name, "negative_gen_count", GL_INVALID_VALUE) != 0) ok = -1;
    if (sentinel != 0xfeed1234u) {
        printf("%s FAIL negative_gen_wrote_output got=0x%x\n", name, sentinel);
        ok = -1;
    }

    // GLES stores only the first error until glGetError consumes it.
    glEnable(0xdead);
    glViewport(0, 0, -2, 1);
    if (expect_error(name, "first_error_retained", GL_INVALID_ENUM) != 0) ok = -1;
    if (expect_error(name, "error_cleared", GL_NO_ERROR) != 0) ok = -1;

    // Calls requiring a current program must report INVALID_OPERATION rather than silently succeeding.
    glUseProgram(0);
    glUniform1i(0, 1);
    if (expect_error(name, "uniform_without_program", GL_INVALID_OPERATION) != 0) ok = -1;

    ge_egl_fini(&egl);
    if (ok != 0) return 8;
    printf("%s PASS invalid_enum=2 invalid_value=2 invalid_operation=1 first_error=1 nonmutation=3\n", name);
    return 0;
}
