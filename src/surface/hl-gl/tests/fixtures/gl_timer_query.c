/* REAL GL TIMER QUERY (HONEST-ERROR demo) — a real EGL + GLES3 program that attempts an
 * EXT_disjoint_timer_query GL_TIME_ELAPSED_EXT query and confirms the driver reports an HONEST error
 * instead of faking a timestamp.
 *
 * Timer queries (GL_TIME_ELAPSED_EXT / GL_TIMESTAMP_EXT) are NOT part of OpenGL ES 3.0 core — they are the
 * EXT_disjoint_timer_query extension. This driver advertises GLES 3.0 core with an EMPTY extension list, so
 * glBeginQuery(GL_TIME_ELAPSED_EXT, …) is an invalid target and MUST raise GL_INVALID_ENUM. Faking a
 * monotonic counter for an unadvertised extension would be a false success (the exact anti-pattern the
 * codex audit forbids). This demo proves the driver stays honest: it verifies GL_TIME_ELAPSED_EXT is NOT
 * in glGetString(GL_EXTENSIONS), then verifies glBeginQuery on it raises GL_INVALID_ENUM.
 *
 * Prints "GL_TIMER_HONEST_ERROR_OK" when the driver correctly refuses the unsupported timer query. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t EGLint;
typedef unsigned int EGLBoolean, EGLenum, GLenum, GLbitfield, GLuint;
typedef int GLint, GLsizei;
typedef unsigned char GLubyte;
typedef void *EGLDisplay, *EGLConfig, *EGLContext, *EGLSurface, *EGLNativeDisplayType;

extern EGLDisplay eglGetDisplay(EGLNativeDisplayType);
extern EGLBoolean eglInitialize(EGLDisplay, EGLint *, EGLint *);
extern EGLBoolean eglChooseConfig(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *);
extern EGLBoolean eglBindAPI(EGLenum);
extern EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint *);
extern EGLSurface eglCreatePbufferSurface(EGLDisplay, EGLConfig, const EGLint *);
extern EGLBoolean eglMakeCurrent(EGLDisplay, EGLSurface, EGLSurface, EGLContext);

extern void glGenQueries(GLsizei, GLuint *);
extern void glBeginQuery(GLenum, GLuint);
extern void glEndQuery(GLenum);
extern GLenum glGetError(void);
extern const GLubyte *glGetString(GLenum);

#define EGL_OPENGL_ES_API 0x30A0
#define GL_NO_ERROR 0
#define GL_INVALID_ENUM 0x0500
#define GL_EXTENSIONS 0x1F03
#define GL_TIME_ELAPSED_EXT 0x88BF

int main(void) {
    setbuf(stdout, NULL);

    EGLDisplay dpy = eglGetDisplay(0);
    if (!eglInitialize(dpy, 0, 0)) { fprintf(stderr, "eglInitialize failed\n"); return 1; }
    eglBindAPI(EGL_OPENGL_ES_API);
    EGLConfig cfg;
    EGLint num = 0;
    eglChooseConfig(dpy, 0, &cfg, 1, &num);
    EGLContext ctx = eglCreateContext(dpy, cfg, 0, 0);
    if (!ctx) { fprintf(stderr, "eglCreateContext failed\n"); return 1; }
    EGLSurface surf = eglCreatePbufferSurface(dpy, cfg, 0);
    if (!surf) { fprintf(stderr, "eglCreatePbufferSurface failed\n"); return 1; }
    if (!eglMakeCurrent(dpy, surf, surf, ctx)) { fprintf(stderr, "eglMakeCurrent failed\n"); return 1; }

    /* The driver must NOT advertise EXT_disjoint_timer_query. */
    const char *ext = (const char *)glGetString(GL_EXTENSIONS);
    if (!ext) ext = "";
    int advertises_timer = strstr(ext, "GL_EXT_disjoint_timer_query") != 0;
    printf("GL_TIMER_EXTENSIONS: \"%s\"\n", ext);

    /* Clear any prior error, then attempt a timer query — it must be rejected with GL_INVALID_ENUM. */
    (void)glGetError();
    GLuint q = 0;
    glGenQueries(1, &q);
    glBeginQuery(GL_TIME_ELAPSED_EXT, q);
    GLenum e = glGetError();
    printf("GL_TIMER_BEGIN_ERR: 0x%x\n", e);

    if (!advertises_timer && e == GL_INVALID_ENUM) {
        printf("GL_TIMER_HONEST_ERROR_OK\n");
        return 0;
    }
    printf("GL_TIMER_WRONG advertises=%d begin_err=0x%x — the driver either advertised or faked an "
           "unsupported timer query\n",
           advertises_timer, e);
    return 2;
}
