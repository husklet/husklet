/* REAL GL TRANSFORM FEEDBACK (DOCUMENTED-GAP demo) — a real EGL + GLES3 program that drives the full
 * transform-feedback lifecycle and reflection, and HONESTLY documents that per-vertex varying CAPTURE into
 * the GL_TRANSFORM_FEEDBACK_BUFFER is not modeled by this deferred driver.
 *
 * What IS modeled and asserted EXACTLY here:
 *   - glTransformFeedbackVaryings records the capture list, and glGetTransformFeedbackVarying round-trips
 *     the varying NAME back verbatim ("vValue") — real observable state.
 *   - The begin/draw/end lifecycle is valid (raises NO GL error).
 *
 * What is NOT modeled (the honest gap): the driver lowers draws to GPU IR and has no CPU vertex-shader
 * executor to evaluate each vertex's varyings, so it does NOT write captured values into the bound
 * GL_TRANSFORM_FEEDBACK_BUFFER. Rather than FAKE captured data (the exact anti-pattern the audit forbids),
 * this demo pre-fills the TF buffer with a sentinel and confirms the buffer is UNCHANGED after the capture
 * — i.e. no fabricated values appear. See hl_wip-gl/src/model/es3.rs (TransformFeedbacks) for the gap note.
 *
 * Prints "GL_TF_DOCUMENTED_GAP_OK" when the reflection round-trips exactly, the lifecycle is error-free,
 * and no fake capture occurred. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t EGLint;
typedef unsigned int EGLBoolean, EGLenum, GLenum, GLbitfield, GLuint;
typedef int GLint, GLsizei;
typedef float GLfloat;
typedef char GLchar;
typedef void *EGLDisplay, *EGLConfig, *EGLContext, *EGLSurface, *EGLNativeDisplayType;

extern EGLDisplay eglGetDisplay(EGLNativeDisplayType);
extern EGLBoolean eglInitialize(EGLDisplay, EGLint *, EGLint *);
extern EGLBoolean eglChooseConfig(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *);
extern EGLBoolean eglBindAPI(EGLenum);
extern EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint *);
extern EGLSurface eglCreatePbufferSurface(EGLDisplay, EGLConfig, const EGLint *);
extern EGLBoolean eglMakeCurrent(EGLDisplay, EGLSurface, EGLSurface, EGLContext);

extern GLuint glCreateShader(GLenum);
extern void glShaderSource(GLuint, GLsizei, const GLchar *const *, const GLint *);
extern void glCompileShader(GLuint);
extern GLuint glCreateProgram(void);
extern void glAttachShader(GLuint, GLuint);
extern void glLinkProgram(GLuint);
extern void glUseProgram(GLuint);
extern GLint glGetAttribLocation(GLuint, const GLchar *);
extern void glGenBuffers(GLsizei, GLuint *);
extern void glBindBuffer(GLenum, GLuint);
extern void glBindBufferBase(GLenum, GLuint, GLuint);
extern void glBufferData(GLenum, long, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, unsigned char, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glEnable(GLenum);
extern void glDisable(GLenum);
extern void glDrawArrays(GLenum, GLint, GLsizei);
extern GLenum glGetError(void);
extern void glTransformFeedbackVaryings(GLuint, GLsizei, const GLchar *const *, GLenum);
extern void glGetTransformFeedbackVarying(GLuint, GLuint, GLsizei, GLsizei *, GLsizei *, GLenum *, GLchar *);
extern void glBeginTransformFeedback(GLenum);
extern void glEndTransformFeedback(void);
extern void *glMapBufferRange(GLenum, long, long, GLbitfield);
extern unsigned char glUnmapBuffer(GLenum);

#define EGL_OPENGL_ES_API 0x30A0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_TRANSFORM_FEEDBACK_BUFFER 0x8C8E
#define GL_INTERLEAVED_ATTRIBS 0x8C8C
#define GL_STATIC_DRAW 0x88E4
#define GL_STREAM_READ 0x88E1
#define GL_MAP_READ_BIT 0x0001
#define GL_FLOAT 0x1406
#define GL_FLOAT_VEC4 0x8B52
#define GL_POINTS 0x0000
#define GL_RASTERIZER_DISCARD 0x8C89
#define GL_NO_ERROR 0

#define N 4 /* points captured */

static const char *VS =
    "attribute vec2 aPos;\n"
    "out vec4 vValue;\n"
    "void main() {\n"
    "  vValue = vec4(aPos, 1.0, 2.0);\n"
    "  gl_Position = vec4(aPos, 0.0, 1.0);\n"
    "}\n";
static const char *FS =
    "precision mediump float;\n"
    "void main() { gl_FragColor = vec4(1.0); }\n";

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

    GLuint vs = glCreateShader(GL_VERTEX_SHADER);
    glShaderSource(vs, 1, &VS, 0);
    glCompileShader(vs);
    GLuint fs = glCreateShader(GL_FRAGMENT_SHADER);
    glShaderSource(fs, 1, &FS, 0);
    glCompileShader(fs);
    GLuint prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);

    /* Record the varying to capture BEFORE linking (as GLES requires). */
    const char *varyings[1] = { "vValue" };
    glTransformFeedbackVaryings(prog, 1, varyings, GL_INTERLEAVED_ATTRIBS);
    glLinkProgram(prog);
    glUseProgram(prog);

    /* Reflection round-trip: the captured varying's NAME must come back verbatim. */
    char name[32];
    memset(name, 0, sizeof(name));
    GLsizei len = 0, size = 0;
    GLenum type = 0;
    glGetTransformFeedbackVarying(prog, 0, sizeof(name), &len, &size, &type, name);
    printf("GL_TF_VARYING: \"%s\" size=%d type=0x%x\n", name, size, type);
    int name_ok = strcmp(name, "vValue") == 0;

    /* Input positions. */
    float verts[2 * N] = { -0.5f, -0.5f,  0.5f, -0.5f,  0.5f, 0.5f,  -0.5f, 0.5f };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    /* The transform-feedback buffer, pre-filled with a SENTINEL float pattern (13.0). If capture were
     * modeled it would be overwritten with the vec4 vValue of each vertex; we assert it stays the sentinel. */
    float sentinel[4 * N];
    for (int i = 0; i < 4 * N; i++) sentinel[i] = 13.0f;
    GLuint tfbuf;
    glGenBuffers(1, &tfbuf);
    glBindBuffer(GL_TRANSFORM_FEEDBACK_BUFFER, tfbuf);
    glBufferData(GL_TRANSFORM_FEEDBACK_BUFFER, sizeof(sentinel), sentinel, GL_STREAM_READ);
    glBindBufferBase(GL_TRANSFORM_FEEDBACK_BUFFER, 0, tfbuf);

    /* Drive the lifecycle — it must be error-free. */
    (void)glGetError();
    glEnable(GL_RASTERIZER_DISCARD);
    glBeginTransformFeedback(GL_POINTS);
    glDrawArrays(GL_POINTS, 0, N);
    glEndTransformFeedback();
    glDisable(GL_RASTERIZER_DISCARD);
    GLenum life_err = glGetError();
    printf("GL_TF_LIFECYCLE_ERR: 0x%x\n", life_err);

    /* Read the TF buffer back — it must be UNCHANGED (the sentinel), proving no fake capture happened. */
    float *m = (float *)glMapBufferRange(GL_TRANSFORM_FEEDBACK_BUFFER, 0, (long)sizeof(sentinel), GL_MAP_READ_BIT);
    if (!m) { printf("GL_TF_MAP_FAILED\n"); return 2; }
    int unchanged = 1;
    for (int i = 0; i < 4 * N; i++) {
        if (m[i] != 13.0f) { unchanged = 0; break; }
    }
    printf("GL_TF_BUFFER_FIRST4: %.1f %.1f %.1f %.1f (sentinel 13.0 => capture not modeled)\n",
           m[0], m[1], m[2], m[3]);
    glUnmapBuffer(GL_TRANSFORM_FEEDBACK_BUFFER);

    if (name_ok && life_err == GL_NO_ERROR && unchanged) {
        printf("GL_TF_DOCUMENTED_GAP_OK\n");
        return 0;
    }
    printf("GL_TF_WRONG name_ok=%d life_err=0x%x unchanged=%d — reflection/lifecycle regressed, or fake "
           "capture appeared\n",
           name_ok, life_err, unchanged);
    return 3;
}
