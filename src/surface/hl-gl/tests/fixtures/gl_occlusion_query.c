/* REAL GL OCCLUSION QUERY — a real EGL + GLES3 offscreen program that wraps an occlusion query
 * (GL_ANY_SAMPLES_PASSED) around two draws and proves glGetQueryObjectuiv reports coverage that reflects
 * REALITY: a VISIBLE draw makes the query read GL_TRUE (non-zero), a fully-SCISSORED draw makes it read
 * GL_FALSE (0). This is the GL analogue of the Vulkan occlusion fix (commit 5551f63a): before it, the shim
 * resolved every occlusion query to a fake constant 0 regardless of what was drawn.
 *
 * The scene, over a BLUE clear:
 *   1. Query q1 wraps a GREEN full-screen quad with NO scissor → it rasterizes samples → q1 == GL_TRUE (1).
 *   2. Query q2 wraps a RED full-screen quad under a scissor box entirely OUTSIDE the 64x64 framebuffer
 *      (glScissor(200,200,16,16)) → nothing can pass → q2 == GL_FALSE (0), and the RED never reaches the
 *      framebuffer.
 * So the rendered frame is solid GREEN (the visible draw), the scissored RED is gone, and the two query
 * results split exactly on visibility. A shim that faked the query would report the SAME value for both
 * (the old constant 0, or a constant 1) — this test rejects that by requiring q1 != q2, q1 != 0, q2 == 0.
 *
 * No hl-specific calls, no vendor headers; the GLES3/EGL ABI is self-declared. Prints "GL_OCCLUSION_OK". */
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
extern GLint glGetUniformLocation(GLuint, const GLchar *);
extern void glUniform4f(GLint, GLfloat, GLfloat, GLfloat, GLfloat);
extern void glGenBuffers(GLsizei, GLuint *);
extern void glBindBuffer(GLenum, GLuint);
extern void glBufferData(GLenum, long, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, unsigned char, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glEnable(GLenum);
extern void glDisable(GLenum);
extern void glScissor(GLint, GLint, GLsizei, GLsizei);
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glViewport(GLint, GLint, GLsizei, GLsizei);
extern void glDrawArrays(GLenum, GLint, GLsizei);
extern void glReadPixels(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *);
extern GLenum glGetError(void);
extern void glGenQueries(GLsizei, GLuint *);
extern void glBeginQuery(GLenum, GLuint);
extern void glEndQuery(GLenum);
extern void glGetQueryObjectuiv(GLuint, GLenum, GLuint *);

#define EGL_OPENGL_ES_API 0x30A0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_STATIC_DRAW 0x88E4
#define GL_FLOAT 0x1406
#define GL_TRIANGLES 0x0004
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_SCISSOR_TEST 0x0C11
#define GL_RGBA 0x1908
#define GL_UNSIGNED_BYTE 0x1401
#define GL_NO_ERROR 0
#define GL_ANY_SAMPLES_PASSED 0x8C2F
#define GL_QUERY_RESULT 0x8866

#define W 64
#define H 64

static const char *VS =
    "attribute vec2 aPos;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char *FS =
    "precision mediump float;\n"
    "uniform vec4 uColor;\n"
    "void main() { gl_FragColor = uColor; }\n";

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 4;
}

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
    glViewport(0, 0, W, H);

    GLuint vs = glCreateShader(GL_VERTEX_SHADER);
    glShaderSource(vs, 1, &VS, 0);
    glCompileShader(vs);
    GLuint fs = glCreateShader(GL_FRAGMENT_SHADER);
    glShaderSource(fs, 1, &FS, 0);
    glCompileShader(fs);
    GLuint prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);
    glLinkProgram(prog);
    glUseProgram(prog);
    GLint locColor = glGetUniformLocation(prog, "uColor");

    /* A full-NDC quad [-1,1]^2 as two triangles. */
    float verts[12] = { -1,-1,  1,-1,  -1,1,   1,-1,  1,1,  -1,1 };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    glClearColor(0.0f, 0.0f, 1.0f, 1.0f); /* blue clear */
    glClear(GL_COLOR_BUFFER_BIT);

    GLuint q[2];
    glGenQueries(2, q);

    /* Query 1: a VISIBLE green draw, no scissor → samples pass → GL_TRUE. */
    glBeginQuery(GL_ANY_SAMPLES_PASSED, q[0]);
    glUniform4f(locColor, 0.0f, 1.0f, 0.0f, 1.0f); /* GREEN */
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glEndQuery(GL_ANY_SAMPLES_PASSED);

    /* Query 2: a fully-SCISSORED red draw (scissor box entirely outside the framebuffer) → nothing
     * passes → GL_FALSE, and the RED is clipped away. */
    glEnable(GL_SCISSOR_TEST);
    glScissor(200, 200, 16, 16);
    glBeginQuery(GL_ANY_SAMPLES_PASSED, q[1]);
    glUniform4f(locColor, 1.0f, 0.0f, 0.0f, 1.0f); /* RED (never reaches the framebuffer) */
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glEndQuery(GL_ANY_SAMPLES_PASSED);
    glDisable(GL_SCISSOR_TEST);

    GLuint r1 = 0xdead, r2 = 0xdead;
    glGetQueryObjectuiv(q[0], GL_QUERY_RESULT, &r1);
    glGetQueryObjectuiv(q[1], GL_QUERY_RESULT, &r2);
    printf("GL_OCCLUSION_Q1: %u\n", r1);
    printf("GL_OCCLUSION_Q2: %u\n", r2);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_OCCLUSION_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

    unsigned char *center = &px[((32) * W + 32) * 4];
    printf("GL_OCCLUSION_CENTER: %u %u %u %u\n", center[0], center[1], center[2], center[3]);

    int visible = (r1 != 0);         /* the un-scissored draw passed samples */
    int occluded = (r2 == 0);        /* the zero-scissor draw passed none */
    int split = (r1 != r2);          /* the query is NOT a constant */
    int center_green = near(center[0], 0) && near(center[1], 255) && near(center[2], 0) && center[3] == 255;
    if (visible && occluded && split && center_green) {
        printf("GL_OCCLUSION_OK\n");
        free(px);
        return 0;
    }
    printf("GL_OCCLUSION_WRONG q1=%u q2=%u center=(%u %u %u %u) — the occlusion query did not reflect "
           "coverage (faked constant?)\n",
           r1, r2, center[0], center[1], center[2], center[3]);
    free(px);
    return 3;
}
