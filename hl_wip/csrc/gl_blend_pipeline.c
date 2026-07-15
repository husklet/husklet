/* REAL GL FIXED-FUNCTION BLEND — a real EGL + GLES2 offscreen program that proves the GPU's fixed-function
 * framebuffer blend (glEnable(GL_BLEND) + glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA)) genuinely
 * composites, rasterized on lavapipe. This SUPERSEDES the fragment-shader-composite workaround in
 * gl_blend.c: the executor now honors the protocol blend field (commit ce66ccba) and the GL driver already
 * lowers glBlendFunc (service/frame.rs blend_factor_wire), so the composite is done by the blend unit, not
 * by arithmetic in the shader.
 *
 * The scene over a BLACK clear is three vertical strips, all driven by the SAME 50%-alpha GREEN fragment
 * output (0,1,0,0.5) over an opaque RED background — the ONLY difference is the blend enable:
 *   LEFT  third  : just the opaque RED background          -> (255,0,0,255)
 *   MIDDLE third : GREEN a=0.5 drawn with GL_BLEND DISABLED -> OVERWRITES to the raw output (0,255,0,128)
 *   RIGHT third  : GREEN a=0.5 drawn with GL_BLEND ENABLED  -> src-over COMPOSITE over red = (128,128,0,191)
 * The middle vs right strips are the proof: identical geometry + identical fragment output, yet one
 * OVERWRITES and one COMPOSITES — only a live framebuffer-blend unit can produce that difference. src-over:
 *   rgb = fg.rgb*fg.a + dst.rgb*(1-fg.a) = green*0.5 + red*0.5 = (0.5,0.5,0)   -> (128,128,0)
 *   a   = fg.a*fg.a   + dst.a*(1-fg.a)   = 0.5*0.5 + 1.0*0.5   = 0.75          -> 191
 *
 * No hl-specific calls, no vendor headers. Prints "GL_BLEND_PIPELINE_OK" on success. */
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
static const char *FS =
    "precision mediump float;\n"
    "uniform vec4 uColor;\n"
    "void main() { gl_FragColor = uColor; }\n";

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 4;
}

static GLuint make_vbo(const float *v, long n) {
    GLuint b;
    glGenBuffers(1, &b);
    glBindBuffer(GL_ARRAY_BUFFER, b);
    glBufferData(GL_ARRAY_BUFFER, n, v, GL_STATIC_DRAW);
    return b;
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
    GLint locColor = glGetUniformLocation(prog, "uColor");

    /* Fullscreen bg; middle third x in [-1/3, 1/3]; right third x in [1/3, 1]. */
    float full[12]   = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    float mid[12]    = { -0.333f,-1,  0.333f,-1,  -0.333f,1,  0.333f,-1,  0.333f,1,  -0.333f,1 };
    float right[12]  = {  0.333f,-1,  1,-1,  0.333f,1,  1,-1,  1,1,  0.333f,1 };
    GLuint vboFull = make_vbo(full, sizeof(full));
    GLuint vboMid = make_vbo(mid, sizeof(mid));
    GLuint vboRight = make_vbo(right, sizeof(right));

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    /* Draw 1: opaque RED background over the whole frame — blend disabled. */
    glDisable(GL_BLEND);
    glBindBuffer(GL_ARRAY_BUFFER, vboFull);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glUniform4f(locColor, 1.0f, 0.0f, 0.0f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    /* Draw 2: GREEN a=0.5 over the MIDDLE third with blend DISABLED — OVERWRITES to the raw (0,255,0,128). */
    glDisable(GL_BLEND);
    glBindBuffer(GL_ARRAY_BUFFER, vboMid);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glUniform4f(locColor, 0.0f, 1.0f, 0.0f, 0.5f);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    /* Draw 3: GREEN a=0.5 over the RIGHT third with blend ENABLED — src-over COMPOSITE over red = (128,128,0,191). */
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    glBindBuffer(GL_ARRAY_BUFFER, vboRight);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glUniform4f(locColor, 0.0f, 1.0f, 0.0f, 0.5f);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLEND_PIPELINE_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    unsigned char *bg = AT(10, 32);   /* left third: opaque RED bg only    -> (255,0,0,255)   */
    unsigned char *ov = AT(32, 32);   /* middle: blend OFF overwrite       -> (0,255,0,128)   */
    unsigned char *cp = AT(54, 32);   /* right: blend ON src-over composite -> (128,128,0,191) */
    printf("GL_BLEND_PIPELINE_BG: %u %u %u %u\n", bg[0], bg[1], bg[2], bg[3]);
    printf("GL_BLEND_PIPELINE_OVERWRITE: %u %u %u %u\n", ov[0], ov[1], ov[2], ov[3]);
    printf("GL_BLEND_PIPELINE_COMPOSITE: %u %u %u %u\n", cp[0], cp[1], cp[2], cp[3]);

    int bg_ok = near(bg[0], 255) && near(bg[1], 0) && near(bg[2], 0) && near(bg[3], 255);
    int ov_ok = near(ov[0], 0) && near(ov[1], 255) && near(ov[2], 0) && near(ov[3], 128);
    int cp_ok = near(cp[0], 128) && near(cp[1], 128) && near(cp[2], 0) && near(cp[3], 191);
    if (bg_ok && ov_ok && cp_ok) {
        printf("GL_BLEND_PIPELINE_OK\n");
        free(px);
        return 0;
    }
    printf("GL_BLEND_PIPELINE_WRONG bg=(%u %u %u %u want 255 0 0 255) overwrite=(%u %u %u %u want 0 255 0 128) "
           "composite=(%u %u %u %u want 128 128 0 191) — fixed-function blend did not composite\n",
           bg[0], bg[1], bg[2], bg[3], ov[0], ov[1], ov[2], ov[3], cp[0], cp[1], cp[2], cp[3]);
    free(px);
    return 3;
}
