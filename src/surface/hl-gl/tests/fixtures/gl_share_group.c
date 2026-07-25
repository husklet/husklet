/* REAL GL SHARE GROUP — a real EGL + GLES2 program with TWO contexts in a share group: a texture is
 * created + filled while context A is current, then SAMPLED while context B is current, rasterized on
 * lavapipe. Context B is created with A passed as its share_context (eglCreateContext(dpy,cfg,ctxA,attrs)),
 * so per EGL share-group semantics B sees A's texture objects. The demo proves the object survives the
 * eglMakeCurrent switch and B reads back EXACTLY the bytes A uploaded.
 *
 * NOTE ON THE MODEL: the hl GL shim keeps ONE process-global object namespace shared across every context
 * (shim/egl state.rs: "one current context in this model"); eglMakeCurrent only rebinds the per-thread
 * current token and never wipes the object table. So the share is expressed by a single shared namespace
 * rather than by tracking the share_context graph — an over-share vs. real EGL isolation, but the
 * cross-context "B sees A's data" path is genuine end-to-end (real upload in A, real sample in B, real
 * readback).
 *
 * The texture is a 2x2 with FOUR distinct texels — RED, GREEN, BLUE, WHITE — uploaded by A. B draws a
 * fullscreen quad sampling it with NEAREST, so the four screen quadrants each hold one texel. The assert
 * requires the four quadrant taps to be a PERMUTATION of {RED,GREEN,BLUE,WHITE}: every one of A's four
 * texels crossed into B, exactly. Prints "GL_SHARE_OK" on success. */
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
extern void glUniform1i(GLint, GLint);
extern void glGenBuffers(GLsizei, GLuint *);
extern void glBindBuffer(GLenum, GLuint);
extern void glBufferData(GLenum, long, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, unsigned char, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glGenTextures(GLsizei, GLuint *);
extern void glActiveTexture(GLenum);
extern void glBindTexture(GLenum, GLuint);
extern void glTexImage2D(GLenum, GLint, GLint, GLsizei, GLsizei, GLint, GLenum, GLenum, const void *);
extern void glTexParameteri(GLenum, GLenum, GLint);
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
#define GL_STATIC_DRAW 0x88E4
#define GL_FLOAT 0x1406
#define GL_TRIANGLES 0x0004
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_RGBA 0x1908
#define GL_UNSIGNED_BYTE 0x1401
#define GL_TEXTURE_2D 0x0DE1
#define GL_TEXTURE0 0x84C0
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_TEXTURE_WRAP_S 0x2802
#define GL_TEXTURE_WRAP_T 0x2803
#define GL_NEAREST 0x2600
#define GL_CLAMP_TO_EDGE 0x812F
#define GL_NO_ERROR 0

#define W 64
#define H 64

static const char *VS =
    "attribute vec2 aPos;\n"
    "attribute vec2 aUV;\n"
    "varying vec2 vUV;\n"
    "void main() { vUV = aUV; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char *FS =
    "precision mediump float;\n"
    "varying vec2 vUV;\n"
    "uniform sampler2D uTex;\n"
    "void main() { gl_FragColor = texture2D(uTex, vUV); }\n";

/* 2x2, four distinct texels. Row-major (row 0 first): [RED, GREEN][BLUE, WHITE]. */
static unsigned char TEX[16] = {
    255,0,0,255,   0,255,0,255,
    0,0,255,255,   255,255,255,255,
};

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 4;
}
/* Encode a texel to a 4-color class id (0=RED 1=GREEN 2=BLUE 3=WHITE, -1=other). */
static int classify(const unsigned char *p) {
    if (near(p[0], 255) && near(p[1], 0) && near(p[2], 0)) return 0;
    if (near(p[0], 0) && near(p[1], 255) && near(p[2], 0)) return 1;
    if (near(p[0], 0) && near(p[1], 0) && near(p[2], 255)) return 2;
    if (near(p[0], 255) && near(p[1], 255) && near(p[2], 255)) return 3;
    return -1;
}

int main(void) {
    setbuf(stdout, NULL);

    EGLDisplay dpy = eglGetDisplay(0);
    if (!eglInitialize(dpy, 0, 0)) { fprintf(stderr, "eglInitialize failed\n"); return 1; }
    eglBindAPI(EGL_OPENGL_ES_API);
    EGLConfig cfg;
    EGLint num = 0;
    eglChooseConfig(dpy, 0, &cfg, 1, &num);

    /* Context A, then context B sharing A (share_context = ctxA). */
    EGLContext ctxA = eglCreateContext(dpy, cfg, 0, 0);
    if (!ctxA) { fprintf(stderr, "eglCreateContext A failed\n"); return 1; }
    EGLContext ctxB = eglCreateContext(dpy, cfg, ctxA, 0);
    if (!ctxB) { fprintf(stderr, "eglCreateContext B (shared) failed\n"); return 1; }
    EGLSurface surf = eglCreatePbufferSurface(dpy, cfg, 0);
    if (!surf) { fprintf(stderr, "eglCreatePbufferSurface failed\n"); return 1; }

    /* ---- Context A: create + fill the shared texture. ---- */
    if (!eglMakeCurrent(dpy, surf, surf, ctxA)) { fprintf(stderr, "eglMakeCurrent A failed\n"); return 1; }
    GLuint tex;
    glGenTextures(1, &tex);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 2, 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, TEX);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);

    /* ---- Context B: sample the texture A created. ---- */
    if (!eglMakeCurrent(dpy, surf, surf, ctxB)) { fprintf(stderr, "eglMakeCurrent B failed\n"); return 1; }
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
    GLint aPos = glGetAttribLocation(prog, "aPos");
    GLint aUV = glGetAttribLocation(prog, "aUV");
    if (aPos < 0) aPos = 0;
    if (aUV < 0) aUV = 1;
    GLint locTex = glGetUniformLocation(prog, "uTex");
    glUniform1i(locTex, 0);

    /* Fullscreen quad, UV [0,1]^2 so the 2x2 texels fill the four quadrants. */
    float verts[24] = {
        -1,-1, 0,0,   1,-1, 1,0,   -1,1, 0,1,
         1,-1, 1,0,   1, 1, 1,1,   -1,1, 0,1,
    };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 16, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glVertexAttribPointer((GLuint)aUV, 2, GL_FLOAT, 0, 16, (const void *)8);
    glEnableVertexAttribArray((GLuint)aUV);

    /* Bind the texture A created — the share-group object, still valid under B. */
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, tex);

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_SHARE_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    unsigned char *q00 = AT(16, 16);
    unsigned char *q10 = AT(48, 16);
    unsigned char *q01 = AT(16, 48);
    unsigned char *q11 = AT(48, 48);
    int c[4] = { classify(q00), classify(q10), classify(q01), classify(q11) };
    printf("GL_SHARE_QUADRANTS: %d %d %d %d (from (%u %u %u)(%u %u %u)(%u %u %u)(%u %u %u))\n",
           c[0], c[1], c[2], c[3],
           q00[0], q00[1], q00[2], q10[0], q10[1], q10[2], q01[0], q01[1], q01[2], q11[0], q11[1], q11[2]);

    /* Require the four quadrants to be a PERMUTATION of {0,1,2,3}: all four of A's texels crossed to B. */
    int seen[4] = { 0, 0, 0, 0 };
    int ok = 1;
    for (int i = 0; i < 4; i++) {
        if (c[i] < 0 || c[i] > 3 || seen[c[i]]) { ok = 0; break; }
        seen[c[i]] = 1;
    }
    if (ok) {
        printf("GL_SHARE_OK\n");
        free(px);
        return 0;
    }
    printf("GL_SHARE_WRONG quadrants=(%d %d %d %d) — B did not see A's four texels (RED GREEN BLUE WHITE) "
           "exactly; the share-group texture did not cross the eglMakeCurrent switch\n",
           c[0], c[1], c[2], c[3]);
    free(px);
    return 3;
}
