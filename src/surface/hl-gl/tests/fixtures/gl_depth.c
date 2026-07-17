/* REAL GL DEPTH TEST — a real EGL + GLES2 offscreen program that proves the depth test occludes correctly,
 * rasterized on lavapipe. Exercises glEnable(GL_DEPTH_TEST) + a real Depth32Float depth attachment.
 *
 * The scene, over a BLACK color clear + depth clear to 1.0 (far):
 *   1. a NEAR GREEN quad (z = 0.2) over the LEFT half, drawn FIRST — it writes depth 0.2.
 *   2. a FAR RED quad (z = 0.8) over the WHOLE frame, drawn SECOND — depth 0.8.
 * With GL_LESS the FAR quad FAILS the depth test wherever the NEAR quad already wrote (the left half), so
 * the left stays GREEN and only the right half (no near geometry) becomes RED. This ordering makes it a
 * genuine depth test, not paint order: WITHOUT a working depth buffer the later fullscreen FAR quad would
 * overwrite everything RED. So GREEN-on-the-left is provable evidence the near fragment's depth occluded
 * the far one. (z stays in [0,1] — the wgpu/Vulkan clip-space depth range the forwarded GLSL runs under.)
 *
 * Position z comes from a `uniform float uZ`, color from a `uniform vec4 uColor`. No hl-specific calls, no
 * vendor headers. Prints "GL_DEPTH_OK" on success. */
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
extern void glUniform1f(GLint, GLfloat);
extern void glUniform4f(GLint, GLfloat, GLfloat, GLfloat, GLfloat);
extern void glGenBuffers(GLsizei, GLuint *);
extern void glBindBuffer(GLenum, GLuint);
extern void glBufferData(GLenum, long, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, unsigned char, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glEnable(GLenum);
extern void glDepthFunc(GLenum);
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
#define GL_DEPTH_BUFFER_BIT 0x0100
#define GL_DEPTH_TEST 0x0B71
#define GL_LESS 0x0201
#define GL_RGBA 0x1908
#define GL_UNSIGNED_BYTE 0x1401
#define GL_NO_ERROR 0

#define W 64
#define H 64

static const char *VS =
    "attribute vec2 aPos;\n"
    "uniform float uZ;\n"
    "void main() { gl_Position = vec4(aPos, uZ, 1.0); }\n";
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
    GLint locZ = glGetUniformLocation(prog, "uZ");
    GLint locColor = glGetUniformLocation(prog, "uColor");

    /* NEAR quad: left half x in [-1,0]. FAR quad: fullscreen [-1,1]^2. */
    float left[12] = { -1,-1,  0,-1,  -1,1,  0,-1,  0,1,  -1,1 };
    float full[12] = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    GLuint vboLeft, vboFull;
    glGenBuffers(1, &vboLeft);
    glBindBuffer(GL_ARRAY_BUFFER, vboLeft);
    glBufferData(GL_ARRAY_BUFFER, sizeof(left), left, GL_STATIC_DRAW);
    glGenBuffers(1, &vboFull);
    glBindBuffer(GL_ARRAY_BUFFER, vboFull);
    glBufferData(GL_ARRAY_BUFFER, sizeof(full), full, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;

    glEnable(GL_DEPTH_TEST);
    glDepthFunc(GL_LESS);

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);

    /* Draw 1: NEAR (z=0.2) GREEN over the left half — writes the near depth. */
    glBindBuffer(GL_ARRAY_BUFFER, vboLeft);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glUniform1f(locZ, 0.2f);
    glUniform4f(locColor, 0.0f, 1.0f, 0.0f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    /* Draw 2: FAR (z=0.8) RED fullscreen — depth test rejects it over the near left half. */
    glBindBuffer(GL_ARRAY_BUFFER, vboFull);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glUniform1f(locZ, 0.8f);
    glUniform4f(locColor, 1.0f, 0.0f, 0.0f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_DEPTH_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    unsigned char *nearpx = AT(16, 32); /* left: NEAR occludes FAR -> GREEN */
    unsigned char *farpx = AT(48, 32);  /* right: FAR only         -> RED   */
    printf("GL_DEPTH_NEAR: %u %u %u %u\n", nearpx[0], nearpx[1], nearpx[2], nearpx[3]);
    printf("GL_DEPTH_FAR: %u %u %u %u\n", farpx[0], farpx[1], farpx[2], farpx[3]);

    int near_ok = near(nearpx[0], 0) && near(nearpx[1], 255) && near(nearpx[2], 0) && nearpx[3] == 255;
    int far_ok = near(farpx[0], 255) && near(farpx[1], 0) && near(farpx[2], 0) && farpx[3] == 255;
    if (near_ok && far_ok) {
        printf("GL_DEPTH_OK\n");
        free(px);
        return 0;
    }
    printf("GL_DEPTH_WRONG near=(%u %u %u %u want 0 255 0 255) far=(%u %u %u %u want 255 0 0 255) — the depth "
           "test did not occlude the far quad behind the near one (missing depth attachment?)\n",
           nearpx[0], nearpx[1], nearpx[2], nearpx[3], farpx[0], farpx[1], farpx[2], farpx[3]);
    free(px);
    return 3;
}
