/* REAL GL PIXEL-PACK-BUFFER (PBO) READBACK — a real EGL + GLES3 program that reads the rendered frame back
 * into a GL_PIXEL_PACK_BUFFER via glReadPixels, then MAPS that PBO with glMapBufferRange and asserts the
 * pixels are exact. This exercises the async-readback-to-PBO path: with a buffer bound to
 * GL_PIXEL_PACK_BUFFER, glReadPixels does NOT write a client pointer — its `pixels` argument is a BYTE
 * OFFSET into the bound pack buffer, and the packed pixels land in the buffer's storage.
 *
 * The detector: glReadPixels is called with offset 0 (a NULL client pointer). If the shim ignored the
 * bound PBO and treated `pixels` as a client pointer, offset 0 is NULL → GL_INVALID_VALUE and NOTHING is
 * written; the mapped PBO would then read back all-zero and the exact-pixel assert fails. Only a correct
 * PBO route makes the mapped bytes equal the rendered GREEN frame.
 *
 * The scene: a BLUE clear + a GREEN full-screen quad → the frame is solid GREEN, so every packed RGBA
 * pixel must read (0,255,0,255). Prints "GL_PBO_OK" on success. */
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
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glViewport(GLint, GLint, GLsizei, GLsizei);
extern void glDrawArrays(GLenum, GLint, GLsizei);
extern void glReadPixels(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *);
extern GLenum glGetError(void);
extern void *glMapBufferRange(GLenum, long, long, GLbitfield);
extern unsigned char glUnmapBuffer(GLenum);

#define EGL_OPENGL_ES_API 0x30A0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_PIXEL_PACK_BUFFER 0x88EB
#define GL_STATIC_DRAW 0x88E4
#define GL_STREAM_READ 0x88E1
#define GL_MAP_READ_BIT 0x0001
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

    float verts[12] = { -1,-1,  1,-1,  -1,1,   1,-1,  1,1,  -1,1 };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    /* Allocate the pixel-pack buffer (the async readback destination). */
    GLuint pbo;
    glGenBuffers(1, &pbo);
    glBindBuffer(GL_PIXEL_PACK_BUFFER, pbo);
    glBufferData(GL_PIXEL_PACK_BUFFER, (long)(W * H * 4), 0, GL_STREAM_READ);

    glClearColor(0.0f, 0.0f, 1.0f, 1.0f); /* blue clear */
    glClear(GL_COLOR_BUFFER_BIT);
    glUniform4f(locColor, 0.0f, 1.0f, 0.0f, 1.0f); /* GREEN */
    glDrawArrays(GL_TRIANGLES, 0, 6);

    /* Read the rendered frame back INTO the bound PBO (offset 0 — a NULL client pointer, which is only
     * valid because a PBO is bound; the packed pixels go into the PBO's storage). */
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, (void *)0);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_PBO_READBACK_FAILED err=0x%x\n", e); return 2; }

    /* Map the PBO and read the exact pixels straight out of its storage. */
    unsigned char *m = (unsigned char *)glMapBufferRange(GL_PIXEL_PACK_BUFFER, 0, (long)(W * H * 4), GL_MAP_READ_BIT);
    if (!m) { printf("GL_PBO_MAP_FAILED\n"); return 3; }

    unsigned char *c = &m[((32) * W + 32) * 4];
    printf("GL_PBO_CENTER: %u %u %u %u\n", c[0], c[1], c[2], c[3]);

    int all_green = 1;
    for (int i = 0; i < W * H; i++) {
        unsigned char *p = &m[i * 4];
        if (!(near(p[0], 0) && near(p[1], 255) && near(p[2], 0) && p[3] == 255)) {
            all_green = 0;
            if (i == W * H / 2) printf("GL_PBO_BADPX i=%d: %u %u %u %u\n", i, p[0], p[1], p[2], p[3]);
            break;
        }
    }
    glUnmapBuffer(GL_PIXEL_PACK_BUFFER);

    if (all_green) {
        printf("GL_PBO_OK\n");
        return 0;
    }
    printf("GL_PBO_WRONG — the mapped PBO did not hold the rendered GREEN frame (readback ignored the "
           "bound GL_PIXEL_PACK_BUFFER?)\n");
    return 4;
}
