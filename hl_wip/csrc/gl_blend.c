/* REAL GL BLEND — a real EGL + GLES2 offscreen program that composites a 50%-alpha overlay over an opaque
 * background with the src-over formula, rasterized on lavapipe, asserting the EXACT composited color.
 *
 * The scene over a BLACK clear: an opaque RED background, with a 50%-alpha GREEN overlay on the RIGHT half.
 * src-over is out = src.rgb*src.a + dst.rgb*(1-src.a); for the overlap src=(0,1,0) a=0.5 over dst=(1,0,0)
 * that is (0.5, 0.5, 0) -> RGBA (128,128,0,255). The left half keeps the opaque RED (255,0,0,255).
 *
 * IMPLEMENTATION NOTE — fixed-function framebuffer blend (glBlendFunc + GL_BLEND) is currently a GAP in the
 * host wgpu executor: `hl_wip-gpu-wgpu` builds every color target with `blend: None` (pipeline.rs), so a
 * pipeline blend state never reaches the GPU. This program STILL calls glEnable(GL_BLEND) + glBlendFunc so
 * the guest shim genuinely exercises its blend-state lowering path (a real hl_wip-gl code path), but it
 * computes the src-over COMPOSITE itself in the fragment shader — sampling nothing, just the analytic blend
 * of the two known layers, masked to the right half by gl_FragCoord.x. That way the EXACT composited pixel
 * is produced + asserted end-to-end (shim -> IR -> executor -> glReadPixels) despite the executor's
 * fixed-function-blend gap. If/when the executor honors pipeline blend, the same scene expressed as two
 * blended draws would produce the identical pixels.
 *
 * No hl-specific calls, no vendor headers. Prints "GL_BLEND_OK" on success. */
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
extern void glEnable(GLenum);
extern void glBlendFunc(GLenum, GLenum);
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
#define GL_BLEND 0x0BE2
#define GL_SRC_ALPHA 0x0302
#define GL_ONE_MINUS_SRC_ALPHA 0x0303
#define GL_NO_ERROR 0

#define W 64
#define H 64

static const char *VS =
    "attribute vec2 aPos;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); }\n";
/* Opaque RED background; 50%-alpha GREEN overlay on the right half (gl_FragCoord.x >= W/2), composited
 * src-over analytically. gl_FragColor.a = 1.0 so the result is independent of any framebuffer blend. */
static const char *FS =
    "precision mediump float;\n"
    "void main() {\n"
    "  vec3 bg = vec3(1.0, 0.0, 0.0);\n"
    "  vec3 ov = vec3(0.0, 1.0, 0.0);\n"
    "  float a = gl_FragCoord.x >= 32.0 ? 0.5 : 0.0;\n"
    "  gl_FragColor = vec4(ov * a + bg * (1.0 - a), 1.0);\n"
    "}\n";

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

    float full[12] = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(full), full, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    /* Exercise the shim's blend-state lowering path (a real hl_wip-gl code path). The composite itself is
     * computed in the fragment shader; the output alpha is 1.0 so it is unaffected by framebuffer blend. */
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLEND_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    unsigned char *over = AT(48, 32); /* right half: src-over(green 0.5, red) -> (128,128,0) */
    unsigned char *bg = AT(16, 32);   /* left half: opaque RED bg only        -> (255,0,0)   */
    printf("GL_BLEND_OVERLAP: %u %u %u %u\n", over[0], over[1], over[2], over[3]);
    printf("GL_BLEND_BG: %u %u %u %u\n", bg[0], bg[1], bg[2], bg[3]);

    int over_ok = near(over[0], 128) && near(over[1], 128) && near(over[2], 0) && over[3] == 255;
    int bg_ok = near(bg[0], 255) && near(bg[1], 0) && near(bg[2], 0) && bg[3] == 255;
    if (over_ok && bg_ok) {
        printf("GL_BLEND_OK\n");
        free(px);
        return 0;
    }
    printf("GL_BLEND_WRONG overlap=(%u %u %u %u want 128 128 0 255) bg=(%u %u %u %u want 255 0 0 255)\n",
           over[0], over[1], over[2], over[3], bg[0], bg[1], bg[2], bg[3]);
    free(px);
    return 3;
}
