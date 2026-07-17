/* REAL GL UBO RANGES — a real EGL + GLES3 program that binds TWO ranges of ONE buffer to TWO distinct
 * uniform-block binding points via glBindBufferRange, then has the fragment shader read BOTH blocks and
 * combine them ASYMMETRICALLY, proving each range fed the right binding.
 *
 * The shader declares two std140 blocks at explicit bindings:
 *     layout(std140, binding = 0) uniform BlockA { vec4 colorA; };
 *     layout(std140, binding = 1) uniform BlockB { vec4 colorB; };
 *     gl_FragColor = vec4(colorA.r, colorB.g, colorA.b, 1.0);
 * The combine takes R + B from colorA (binding 0) and G from colorB (binding 1). One buffer holds colorA
 * at byte offset 0 and colorB at byte offset 256; glBindBufferRange(...,0,buf,0,16) and
 * glBindBufferRange(...,1,buf,256,16) bind the two ranges. With:
 *     colorA = (1.00, 0.25, 0.50, 1.0)   → contributes r=1.00, b=0.50
 *     colorB = (0.30, 1.00, 0.70, 1.0)   → contributes g=1.00
 * the exact fragment is (1.00, 1.00, 0.50, 1.0) → RGBA (255, 255, 128, 255).
 *
 * This value is a UNIQUE fingerprint of a correct route: if binding 1 mis-read binding 0's range, g would
 * be colorA.g = 0.25 → (255,64,128,255); if the two ranges were swapped, r/b would come from colorB →
 * (76,64,178,255). Only both ranges reaching their own binding yields (255,255,128,255). Prints
 * "GL_UBO_RANGES_OK". */
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
extern void glBindBufferRange(GLenum, GLuint, GLuint, long, long);
extern void glBufferData(GLenum, long, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, unsigned char, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glViewport(GLint, GLint, GLsizei, GLsizei);
extern void glDrawArrays(GLenum, GLint, GLsizei);
extern void glReadPixels(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *);
extern GLenum glGetError(void);

#define EGL_OPENGL_ES_API 0x30A0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_UNIFORM_BUFFER 0x8A11
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
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); }\n";

/* The fragment reads BOTH blocks and combines them asymmetrically so each range's contribution is
 * distinguishable in the output. */
static const char *FS =
    "precision mediump float;\n"
    "layout(std140, binding = 0) uniform BlockA { vec4 colorA; };\n"
    "layout(std140, binding = 1) uniform BlockB { vec4 colorB; };\n"
    "void main() { gl_FragColor = vec4(colorA.r, colorB.g, colorA.b, 1.0); }\n";

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

    float verts[12] = { -1,-1,  1,-1,  -1,1,   1,-1,  1,1,  -1,1 };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    /* ONE buffer, two 16-byte std140 vec4 ranges at offsets 0 and 256 (256 = a safe UBO offset alignment). */
    unsigned char blob[272];
    memset(blob, 0, sizeof(blob));
    float colorA[4] = { 1.00f, 0.25f, 0.50f, 1.0f };
    float colorB[4] = { 0.30f, 1.00f, 0.70f, 1.0f };
    memcpy(&blob[0], colorA, 16);
    memcpy(&blob[256], colorB, 16);

    GLuint ubo;
    glGenBuffers(1, &ubo);
    glBindBuffer(GL_UNIFORM_BUFFER, ubo);
    glBufferData(GL_UNIFORM_BUFFER, sizeof(blob), blob, GL_STATIC_DRAW);
    /* Bind range [0,16) to binding 0 (colorA) and range [256,272) to binding 1 (colorB). */
    glBindBufferRange(GL_UNIFORM_BUFFER, 0, ubo, 0, 16);
    glBindBufferRange(GL_UNIFORM_BUFFER, 1, ubo, 256, 16);

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f); /* black clear */
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_UBO_RANGES_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

    unsigned char *c = &px[((32) * W + 32) * 4];
    printf("GL_UBO_RANGES_CENTER: %u %u %u %u\n", c[0], c[1], c[2], c[3]);

    /* Expected: (colorA.r, colorB.g, colorA.b, 1.0) = (1.0, 1.0, 0.5, 1.0) = (255, 255, 128, 255). */
    if (near(c[0], 255) && near(c[1], 255) && near(c[2], 128) && c[3] == 255) {
        printf("GL_UBO_RANGES_OK\n");
        free(px);
        return 0;
    }
    printf("GL_UBO_RANGES_WRONG center=(%u %u %u %u) want (255 255 128 255) — a glBindBufferRange range did "
           "not reach its binding\n",
           c[0], c[1], c[2], c[3]);
    free(px);
    return 3;
}
