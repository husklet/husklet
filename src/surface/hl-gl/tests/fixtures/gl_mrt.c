/* REAL GL MULTIPLE RENDER TARGETS (MRT) — a real EGL + GLES3 offscreen program that binds an FBO with TWO
 * color attachments, calls glDrawBuffers([GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT1]), and a single draw
 * whose fragment shader writes BOTH outputs (o0, o1) — rasterized on lavapipe as one pass with two color
 * targets. Each attachment is then read back with glReadBuffer(...) + glReadPixels and asserted to hold its
 * own distinct value: attachment 0 = RED, attachment 1 = GREEN. A broken MRT lowering (only the first
 * target materialized, or both fed the same output) fails the per-attachment assert.
 *
 * This exercises the hl_wip-gl MRT path added for this demo: framebuffer.rs multi-attachment storage +
 * frame.rs build_mrt_geometry_frame (one pass, N color targets) + adapter/glsl.rs multi-`out` fragment
 * emission + readpixels.rs glReadBuffer selection.
 *
 * No hl-specific calls, no vendor headers. Prints "GL_MRT_OK" on success. */
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
extern void glGenTextures(GLsizei, GLuint *);
extern void glBindTexture(GLenum, GLuint);
extern void glTexImage2D(GLenum, GLint, GLint, GLsizei, GLsizei, GLint, GLenum, GLenum, const void *);
extern void glTexParameteri(GLenum, GLenum, GLint);
extern void glGenFramebuffers(GLsizei, GLuint *);
extern void glBindFramebuffer(GLenum, GLuint);
extern void glFramebufferTexture2D(GLenum, GLenum, GLenum, GLuint, GLint);
extern GLenum glCheckFramebufferStatus(GLenum);
extern void glDrawBuffers(GLsizei, const GLenum *);
extern void glReadBuffer(GLenum);
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
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_NEAREST 0x2600
#define GL_FRAMEBUFFER 0x8D40
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_COLOR_ATTACHMENT1 0x8CE1
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5
#define GL_NO_ERROR 0

#define W 64
#define H 64

/* GLES3: two explicit fragment outputs written to two attachments. */
static const char *VS =
    "#version 300 es\n"
    "in vec2 aPos;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char *FS =
    "#version 300 es\n"
    "precision mediump float;\n"
    "layout(location = 0) out vec4 o0;\n"
    "layout(location = 1) out vec4 o1;\n"
    "void main() {\n"
    "  o0 = vec4(1.0, 0.0, 0.0, 1.0);\n"  /* RED   -> attachment 0 */
    "  o1 = vec4(0.0, 1.0, 0.0, 1.0);\n"  /* GREEN -> attachment 1 */
    "}\n";

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 4;
}

static GLuint make_rt(void) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    return t;
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
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;

    /* FBO with two color attachments. */
    GLuint tex0 = make_rt();
    GLuint tex1 = make_rt();
    GLuint fbo;
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex0, 0);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT1, GL_TEXTURE_2D, tex1, 0);
    GLenum fbs = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (fbs != GL_FRAMEBUFFER_COMPLETE) { printf("GL_MRT_INCOMPLETE 0x%x\n", fbs); return 1; }
    GLenum bufs[2] = { GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT1 };
    glDrawBuffers(2, bufs);

    float full[12] = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(full), full, GL_STATIC_DRAW);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *p0 = malloc(W * H * 4);
    unsigned char *p1 = malloc(W * H * 4);
    memset(p0, 0xAB, W * H * 4);
    memset(p1, 0xAB, W * H * 4);

    glReadBuffer(GL_COLOR_ATTACHMENT0);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, p0);
    glReadBuffer(GL_COLOR_ATTACHMENT1);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, p1);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_MRT_READBACK_FAILED err=0x%x\n", e); free(p0); free(p1); return 2; }

    unsigned char *a0 = &p0[((H / 2) * W + (W / 2)) * 4]; /* attachment 0 center -> RED   */
    unsigned char *a1 = &p1[((H / 2) * W + (W / 2)) * 4]; /* attachment 1 center -> GREEN */
    printf("GL_MRT_ATTACH0: %u %u %u %u\n", a0[0], a0[1], a0[2], a0[3]);
    printf("GL_MRT_ATTACH1: %u %u %u %u\n", a1[0], a1[1], a1[2], a1[3]);

    int a0_ok = near(a0[0], 255) && near(a0[1], 0) && near(a0[2], 0) && a0[3] == 255;
    int a1_ok = near(a1[0], 0) && near(a1[1], 255) && near(a1[2], 0) && a1[3] == 255;
    if (a0_ok && a1_ok) {
        printf("GL_MRT_OK\n");
        free(p0);
        free(p1);
        return 0;
    }
    printf("GL_MRT_WRONG attach0=(%u %u %u %u want 255 0 0 255) attach1=(%u %u %u %u want 0 255 0 255) — the "
           "two render targets did not each receive their own fragment output\n",
           a0[0], a0[1], a0[2], a0[3], a1[0], a1[1], a1[2], a1[3]);
    free(p0);
    free(p1);
    return 3;
}
