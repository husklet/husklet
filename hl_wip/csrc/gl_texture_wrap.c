/* REAL GL TEXTURE WRAP — a real EGL + GLES2 offscreen program that proves the sampler address mode
 * (GL_TEXTURE_WRAP_S = GL_REPEAT vs GL_CLAMP_TO_EDGE) is honored end-to-end, rasterized on lavapipe. The GL
 * driver lowers the texture's wrap state to the IR sampler's address_u (service/frame.rs ir_wrap_s), and
 * lavapipe's sampler does the wrap.
 *
 * A 2x2 texture whose two COLUMNS are RED (texel 0, u in [0,0.5)) and GREEN (texel 1, u in [0.5,1)) is
 * sampled with NEAREST over a UV.x that RAMPS 0 -> 2 across each half of the frame:
 *   LEFT half  samples a GL_REPEAT texture: past u=1 the coordinate WRAPS, so the RED/GREEN pattern TILES a
 *              second time -> vertical stripes R G R G across the half.
 *   RIGHT half samples a GL_CLAMP_TO_EDGE texture: past u=1 the coordinate CLAMPS to the right edge texel
 *              (GREEN), so after the single [0,1] R->G transition it stays GREEN -> R G G G.
 * The decisive taps are u≈1.28 (past 1.0): REPEAT wraps to the RED texel, CLAMP holds the GREEN edge. So the
 * SAME UV yields RED under REPEAT and GREEN under CLAMP — an exact-pixel detector for the address mode.
 *
 * No hl-specific calls, no vendor headers. Prints "GL_TEXWRAP_OK" on success. */
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
#define GL_REPEAT 0x2901
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

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 6;
}

/* 2x2 texture: column 0 RED (texel 0), column 1 GREEN (texel 1); both rows identical. */
static unsigned char TEX[16] = {
    255,0,0,255, 0,255,0,255,
    255,0,0,255, 0,255,0,255,
};

static GLuint make_wrap_tex(int wrap) {
    GLuint t;
    glGenTextures(1, &t);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 2, 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, TEX);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, wrap);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, wrap);
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
    GLint aUV = glGetAttribLocation(prog, "aUV");
    if (aPos < 0) aPos = 0;
    if (aUV < 0) aUV = 1;
    GLint locTex = glGetUniformLocation(prog, "uTex");
    glUniform1i(locTex, 0);

    /* Interleaved [x, y, u, v]; UV.x ramps 0 -> 2 across each half so REPEAT tiles a second time. */
    float leftQ[24] = {
        -1,-1, 0,0.5f,   0,-1, 2,0.5f,   -1,1, 0,0.5f,
         0,-1, 2,0.5f,   0, 1, 2,0.5f,   -1,1, 0,0.5f,
    };
    float rightQ[24] = {
        0,-1, 0,0.5f,   1,-1, 2,0.5f,   0,1, 0,0.5f,
        1,-1, 2,0.5f,   1, 1, 2,0.5f,   0,1, 0,0.5f,
    };
    GLuint vboL, vboR;
    glGenBuffers(1, &vboL);
    glBindBuffer(GL_ARRAY_BUFFER, vboL);
    glBufferData(GL_ARRAY_BUFFER, sizeof(leftQ), leftQ, GL_STATIC_DRAW);
    glGenBuffers(1, &vboR);
    glBindBuffer(GL_ARRAY_BUFFER, vboR);
    glBufferData(GL_ARRAY_BUFFER, sizeof(rightQ), rightQ, GL_STATIC_DRAW);

    GLuint texRepeat = make_wrap_tex(GL_REPEAT);
    GLuint texClamp = make_wrap_tex(GL_CLAMP_TO_EDGE);

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    /* LEFT half: GL_REPEAT texture. */
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, texRepeat);
    glBindBuffer(GL_ARRAY_BUFFER, vboL);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 16, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glVertexAttribPointer((GLuint)aUV, 2, GL_FLOAT, 0, 16, (const void *)8);
    glEnableVertexAttribArray((GLuint)aUV);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    /* RIGHT half: GL_CLAMP_TO_EDGE texture. */
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, texClamp);
    glBindBuffer(GL_ARRAY_BUFFER, vboR);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 16, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glVertexAttribPointer((GLuint)aUV, 2, GL_FLOAT, 0, 16, (const void *)8);
    glEnableVertexAttribArray((GLuint)aUV);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_TEXWRAP_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    /* LEFT (REPEAT), y=32. u = ((x+0.5)/32)*2. Taps: R@u.28, G@u.78, R@u1.28(WRAP), G@u1.78. */
    unsigned char *l0 = AT(4, 32);   /* u≈0.28 -> RED   */
    unsigned char *l1 = AT(12, 32);  /* u≈0.78 -> GREEN */
    unsigned char *l2 = AT(20, 32);  /* u≈1.28 -> WRAP -> RED   */
    unsigned char *l3 = AT(28, 32);  /* u≈1.78 -> WRAP -> GREEN */
    /* RIGHT (CLAMP), y=32. local u = ((x-32+0.5)/32)*2. Taps: R@u.28, G@u.78, EDGE@u1.28, EDGE@u1.78. */
    unsigned char *r0 = AT(36, 32);  /* u≈0.28 -> RED   */
    unsigned char *r1 = AT(44, 32);  /* u≈0.78 -> GREEN */
    unsigned char *r2 = AT(52, 32);  /* u≈1.28 -> CLAMP -> GREEN edge */
    unsigned char *r3 = AT(60, 32);  /* u≈1.78 -> CLAMP -> GREEN edge */
    printf("GL_TEXWRAP_REPEAT: (%u %u %u)(%u %u %u)(%u %u %u)(%u %u %u)\n",
           l0[0], l0[1], l0[2], l1[0], l1[1], l1[2], l2[0], l2[1], l2[2], l3[0], l3[1], l3[2]);
    printf("GL_TEXWRAP_CLAMP: (%u %u %u)(%u %u %u)(%u %u %u)(%u %u %u)\n",
           r0[0], r0[1], r0[2], r1[0], r1[1], r1[2], r2[0], r2[1], r2[2], r3[0], r3[1], r3[2]);

    int is_red = 1, is_green = 1;
#define RED(p) (near((p)[0], 255) && near((p)[1], 0) && near((p)[2], 0))
#define GREEN(p) (near((p)[0], 0) && near((p)[1], 255) && near((p)[2], 0))
    /* REPEAT tiles: R G R G. */
    int repeat_ok = RED(l0) && GREEN(l1) && RED(l2) && GREEN(l3);
    /* CLAMP holds the edge past u=1: R G G G. */
    int clamp_ok = RED(r0) && GREEN(r1) && GREEN(r2) && GREEN(r3);
    (void)is_red; (void)is_green;
    if (repeat_ok && clamp_ok) {
        printf("GL_TEXWRAP_OK\n");
        free(px);
        return 0;
    }
    printf("GL_TEXWRAP_WRONG repeat_ok=%d clamp_ok=%d (REPEAT must tile R G R G; CLAMP must hold R G G G) — "
           "the sampler address mode was not honored\n", repeat_ok, clamp_ok);
    free(px);
    return 3;
}
