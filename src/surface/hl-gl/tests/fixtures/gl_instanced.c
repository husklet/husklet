/* REAL GL INSTANCED — a real EGL + GLES3 offscreen program that draws N quads in ONE glDrawArraysInstanced
 * call, each placed + colored by a PER-INSTANCE vertex attribute (glVertexAttribDivisor(., 1)), rasterized
 * on lavapipe. Proves the shim lowers instanced draws + attribute divisors into a real instanced pipeline.
 *
 * The scene: a small base quad ([-1,1]^2 scaled by 0.15) is drawn as 4 instances. A per-instance attribute
 * aOffset (divisor 1) places instance k at NDC x = -0.75 + 0.5*k, y = 0 — an exact 1x4 horizontal grid at
 * pixel columns 8, 24, 40, 56 (all on row 32, so the check is independent of readback y-orientation). A
 * second per-instance attribute aColor (divisor 1) paints each instance a distinct color: RED, GREEN, BLUE,
 * YELLOW. A correct instanced draw therefore paints those four colors at those four cells; a broken divisor
 * (per-vertex instead of per-instance) would smear instance 0's attributes across all instances or collapse
 * the grid — either changes at least one cell.
 *
 * No hl-specific calls, no vendor headers. Prints "GL_INSTANCED_OK" on success. */
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
extern void glBufferData(GLenum, long, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, unsigned char, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glVertexAttribDivisor(GLuint, GLuint);
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glViewport(GLint, GLint, GLsizei, GLsizei);
extern void glDrawArraysInstanced(GLenum, GLint, GLsizei, GLsizei);
extern void glReadPixels(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *);
extern GLenum glGetError(void);

#define EGL_OPENGL_ES_API 0x30A0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_STATIC_DRAW 0x88E4
#define GL_FLOAT 0x1406
#define GL_TRIANGLES 0x0004
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_RGBA 0x1908
#define GL_UNSIGNED_BYTE 0x1401
#define GL_NO_ERROR 0

#define W 64
#define H 64

static const char *VS =
    "attribute vec2 aPos;\n"
    "attribute vec2 aOffset;\n"
    "attribute vec3 aColor;\n"
    "varying vec3 vColor;\n"
    "void main() { vColor = aColor; gl_Position = vec4(aPos * 0.15 + aOffset, 0.0, 1.0); }\n";
static const char *FS =
    "precision mediump float;\n"
    "varying vec3 vColor;\n"
    "void main() { gl_FragColor = vec4(vColor, 1.0); }\n";

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 4;
}
static int is_rgb(const unsigned char *p, int r, int g, int b) {
    return near(p[0], r) && near(p[1], g) && near(p[2], b) && p[3] == 255;
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

    /* Per-vertex base quad (divisor 0). */
    float quad[12] = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    GLuint vboQuad;
    glGenBuffers(1, &vboQuad);
    glBindBuffer(GL_ARRAY_BUFFER, vboQuad);
    glBufferData(GL_ARRAY_BUFFER, sizeof(quad), quad, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glVertexAttribDivisor((GLuint)aPos, 0);

    /* Per-instance [ox, oy, r, g, b] * 4 (divisor 1). Offsets -> cols 8,24,40,56 at row 32. */
    float inst[20] = {
        -0.75f, 0.0f,  1.0f, 0.0f, 0.0f, /* RED    */
        -0.25f, 0.0f,  0.0f, 1.0f, 0.0f, /* GREEN  */
         0.25f, 0.0f,  0.0f, 0.0f, 1.0f, /* BLUE   */
         0.75f, 0.0f,  1.0f, 1.0f, 0.0f, /* YELLOW */
    };
    GLuint vboInst;
    glGenBuffers(1, &vboInst);
    glBindBuffer(GL_ARRAY_BUFFER, vboInst);
    glBufferData(GL_ARRAY_BUFFER, sizeof(inst), inst, GL_STATIC_DRAW);
    GLint aOffset = glGetAttribLocation(prog, "aOffset");
    GLint aColor = glGetAttribLocation(prog, "aColor");
    if (aOffset < 0) aOffset = 1;
    if (aColor < 0) aColor = 2;
    glVertexAttribPointer((GLuint)aOffset, 2, GL_FLOAT, 0, 20, (const void *)0);
    glEnableVertexAttribArray((GLuint)aOffset);
    glVertexAttribDivisor((GLuint)aOffset, 1);
    glVertexAttribPointer((GLuint)aColor, 3, GL_FLOAT, 0, 20, (const void *)8);
    glEnableVertexAttribArray((GLuint)aColor);
    glVertexAttribDivisor((GLuint)aColor, 1);

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f); /* black clear */
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArraysInstanced(GL_TRIANGLES, 0, 6, 4);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_INSTANCED_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    unsigned char *c0 = AT(8, 32);
    unsigned char *c1 = AT(24, 32);
    unsigned char *c2 = AT(40, 32);
    unsigned char *c3 = AT(56, 32);
    printf("GL_INSTANCED_CELLS: (%u %u %u)(%u %u %u)(%u %u %u)(%u %u %u)\n",
           c0[0], c0[1], c0[2], c1[0], c1[1], c1[2], c2[0], c2[1], c2[2], c3[0], c3[1], c3[2]);

    int ok = is_rgb(c0, 255, 0, 0) && is_rgb(c1, 0, 255, 0) && is_rgb(c2, 0, 0, 255) && is_rgb(c3, 255, 255, 0);
    if (ok) {
        printf("GL_INSTANCED_OK\n");
        free(px);
        return 0;
    }
    printf("GL_INSTANCED_WRONG cells did not match RED/GREEN/BLUE/YELLOW at cols 8/24/40/56 — instance "
           "divisor not honored?\n");
    free(px);
    return 3;
}
