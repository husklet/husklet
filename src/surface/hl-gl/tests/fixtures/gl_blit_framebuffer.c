/* REAL GL BLIT FRAMEBUFFER — a real EGL + GLES3 offscreen program that renders into a source FBO, clears a
 * destination FBO, then glBlitFramebuffer()s an exact centered sub-rect from the source to the destination
 * (equal size, no scaling), rasterized on lavapipe. The equal-size blit lowers to Enc::CopyTextureToTexture
 * (which the executor implements), copying the src attachment's rendered pixels into the dst attachment.
 *
 * The scene: the SOURCE FBO is filled solid RED; the DEST FBO is cleared solid BLUE. A centered 32x32
 * sub-rect [16,48] x [16,48] is blitted src -> dst at the SAME coordinates. So the destination reads back
 * RED inside the centered square and BLUE everywhere outside — the copied region AND the untouched border
 * are both asserted, at exact pixel boundaries (x=10 outside=BLUE, x=20 inside=RED, corners BLUE). A broken
 * blit (nothing copied, wrong rect, or the whole frame overwritten) fails one of these.
 *
 * Exercises the hl_wip-gl blit path added for this demo: record.rs blit_framebuffer records a BlitOp;
 * frame.rs build_multi_pass_frame applies it as a CopyTextureToTexture after the render passes.
 *
 * No hl-specific calls, no vendor headers. Prints "GL_BLIT_OK" on success. */
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
extern void glBlitFramebuffer(GLint, GLint, GLint, GLint, GLint, GLint, GLint, GLint, GLbitfield, GLenum);
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
#define GL_READ_FRAMEBUFFER 0x8CA8
#define GL_DRAW_FRAMEBUFFER 0x8CA9
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5
#define GL_NO_ERROR 0

#define W 64
#define H 64

static const char *VS =
    "attribute vec2 aPos;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char *FS =
    "precision mediump float;\n"
    "void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n"; /* solid RED */

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 4;
}

static GLuint make_fbo(GLuint *tex_out) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    GLuint f;
    glGenFramebuffers(1, &f);
    glBindFramebuffer(GL_FRAMEBUFFER, f);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t, 0);
    *tex_out = t;
    return f;
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

    GLuint texSrc, texDst;
    GLuint fboSrc = make_fbo(&texSrc);
    GLuint fboDst = make_fbo(&texDst);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) { printf("GL_BLIT_INCOMPLETE\n"); return 1; }

    float full[12] = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(full), full, GL_STATIC_DRAW);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    /* Source FBO: fill solid RED. */
    glBindFramebuffer(GL_FRAMEBUFFER, fboSrc);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    /* Destination FBO: clear solid BLUE. */
    glBindFramebuffer(GL_FRAMEBUFFER, fboDst);
    glClearColor(0.0f, 0.0f, 1.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    /* Blit a centered 32x32 sub-rect [16,48]x[16,48] from src -> dst, equal size (no scaling). */
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboSrc);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, fboDst);
    glBlitFramebuffer(16, 16, 48, 48, 16, 16, 48, 48, GL_COLOR_BUFFER_BIT, GL_NEAREST);

    /* Read back the destination FBO. */
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboDst);
    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLIT_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    unsigned char *inside = AT(32, 32);  /* center of the copied square -> RED  */
    unsigned char *left = AT(10, 32);    /* left of the square (x=10 < 16)      -> BLUE */
    unsigned char *right = AT(54, 32);   /* right of the square (x=54 > 48)     -> BLUE */
    unsigned char *corner = AT(4, 4);    /* untouched corner                    -> BLUE */
    unsigned char *edge_in = AT(20, 32); /* just inside the square (x=20 > 16)  -> RED  */
    printf("GL_BLIT_INSIDE: %u %u %u %u\n", inside[0], inside[1], inside[2], inside[3]);
    printf("GL_BLIT_OUTSIDE: L(%u %u %u) R(%u %u %u) C(%u %u %u)\n",
           left[0], left[1], left[2], right[0], right[1], right[2], corner[0], corner[1], corner[2]);

#define RED(p) (near((p)[0], 255) && near((p)[1], 0) && near((p)[2], 0))
#define BLUE(p) (near((p)[0], 0) && near((p)[1], 0) && near((p)[2], 255))
    int copied_ok = RED(inside) && RED(edge_in);
    int border_ok = BLUE(left) && BLUE(right) && BLUE(corner);
    if (copied_ok && border_ok) {
        printf("GL_BLIT_OK\n");
        free(px);
        return 0;
    }
    printf("GL_BLIT_WRONG copied_ok=%d border_ok=%d (copied square must be RED, border must stay BLUE) — the "
           "blit did not copy the exact sub-rect\n", copied_ok, border_ok);
    free(px);
    return 3;
}
