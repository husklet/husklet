/* REAL GL SCALING BLIT — a real EGL + GLES3 offscreen program that renders a tiny 2x2 SOURCE FBO, then
 * glBlitFramebuffer()s it 2x-UPSCALED into a larger DESTINATION FBO, once with GL_NEAREST and once with
 * GL_LINEAR, rasterized/resampled on lavapipe. A scaling blit (source extent != destination extent) lowers
 * to Enc::BlitTexture carrying the filter — the exact op that was DROPPED before this change (the equal-size
 * blit only lowered to Enc::CopyTextureToTexture).
 *
 * NEAREST (2x2 -> 4x4, and 2x2 -> 64x64): a 2x point-sampled upscale is pure texel replication — each of the
 * four distinct source texels must fill an exact block (2x2 in the 4x4, 32x32 in the 64x64). We read the
 * source back first (the "source truth") and assert dst(x,y) == srcTruth(x/2, y/2) EXACTLY — Y-flip-agnostic
 * because a full 0,0 blit maps dst texel (x,y) to src texel (x/2,y/2) in both readbacks.
 *
 * LINEAR (2x2 horizontal gradient -> 3x3): the source is a pure horizontal gradient (left column al, right
 * column bl, both rows identical, chosen so each channel sum is even). The ODD destination width 3 puts the
 * middle column's pixel center at exactly u=0.5 — the midpoint between the two texel centers — so a correct
 * linear filter returns EXACTLY (al+bl)/2 there, while clamp-to-edge returns al / bl at the outer columns.
 * The asserted row is [al, (al+bl)/2, bl], exact.
 *
 * Each destination is pre-cleared to a MAGENTA sentinel; a full-cover blit must overwrite every texel, so no
 * sentinel may survive — proving the scaling blit really executed (the old failure was a silent drop).
 *
 * No hl-specific calls, no vendor headers. Prints "GL_BLIT_SCALE_OK" on success. */
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
extern EGLBoolean eglSwapBuffers(EGLDisplay, EGLSurface);

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
#define GL_LINEAR 0x2601
#define GL_FRAMEBUFFER 0x8D40
#define GL_READ_FRAMEBUFFER 0x8CA8
#define GL_DRAW_FRAMEBUFFER 0x8CA9
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5
#define GL_NO_ERROR 0

/* Source is a 2x2 texel image; two upscale destinations exercise the scaling blit. */
#define SRC 2
#define BIG 64

static const char *VS =
    "attribute vec2 aPos;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); }\n";

/* 4 distinct texels selected by gl_FragCoord over the 2x2 target (centers 0.5 / 1.5). Y-orientation is
 * irrelevant: we assert against the source's OWN readback, so whatever corner each color lands in is
 * matched consistently by dst and src. */
static const char *FS_N =
    "precision highp float;\n"
    "void main() {\n"
    "  bool r = gl_FragCoord.x > 1.0;\n"
    "  bool t = gl_FragCoord.y > 1.0;\n"
    "  if (!r && !t) gl_FragColor = vec4(200.0/255.0, 30.0/255.0, 40.0/255.0, 1.0);\n"
    "  else if (r && !t) gl_FragColor = vec4(30.0/255.0, 200.0/255.0, 40.0/255.0, 1.0);\n"
    "  else if (!r && t) gl_FragColor = vec4(40.0/255.0, 30.0/255.0, 200.0/255.0, 1.0);\n"
    "  else gl_FragColor = vec4(200.0/255.0, 200.0/255.0, 30.0/255.0, 1.0);\n"
    "}\n";

/* Pure horizontal gradient: left column al=(40,80,120), right column bl=(200,160,120) — each channel sum
 * even so (al+bl)/2 is an exact integer. Both rows identical, so every destination row is identical. */
static const char *FS_G =
    "precision highp float;\n"
    "void main() {\n"
    "  if (gl_FragCoord.x > 1.0) gl_FragColor = vec4(200.0/255.0, 160.0/255.0, 120.0/255.0, 1.0);\n"
    "  else gl_FragColor = vec4(40.0/255.0, 80.0/255.0, 120.0/255.0, 1.0);\n"
    "}\n";

static GLuint make_fbo(int w, int h, GLuint *tex_out) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    GLuint f;
    glGenFramebuffers(1, &f);
    glBindFramebuffer(GL_FRAMEBUFFER, f);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t, 0);
    *tex_out = t;
    return f;
}

/* Render the source pattern (program `prog`) into the 2x2 source FBO. */
static void render_src(EGLDisplay dpy, EGLSurface surf, GLuint fboSrc, GLuint prog) {
    (void)dpy; (void)surf;
    glBindFramebuffer(GL_FRAMEBUFFER, fboSrc);
    glViewport(0, 0, SRC, SRC);
    glUseProgram(prog);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);
}

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 2;
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

    GLuint vs = glCreateShader(GL_VERTEX_SHADER);
    glShaderSource(vs, 1, &VS, 0);
    glCompileShader(vs);
    GLuint fsN = glCreateShader(GL_FRAGMENT_SHADER);
    glShaderSource(fsN, 1, &FS_N, 0);
    glCompileShader(fsN);
    GLuint fsG = glCreateShader(GL_FRAGMENT_SHADER);
    glShaderSource(fsG, 1, &FS_G, 0);
    glCompileShader(fsG);
    GLuint progN = glCreateProgram();
    glAttachShader(progN, vs);
    glAttachShader(progN, fsN);
    glLinkProgram(progN);
    GLuint progG = glCreateProgram();
    glAttachShader(progG, vs);
    glAttachShader(progG, fsG);
    glLinkProgram(progG);
    GLint aPos = glGetAttribLocation(progN, "aPos");
    if (aPos < 0) aPos = 0;

    GLuint texSrc, texDst4, texBigN, texSrcG, texDst3, texBigL;
    GLuint fboSrc = make_fbo(SRC, SRC, &texSrc);
    GLuint fboDst4 = make_fbo(4, 4, &texDst4);
    GLuint fboBigN = make_fbo(BIG, BIG, &texBigN);
    GLuint fboSrcG = make_fbo(SRC, SRC, &texSrcG);
    GLuint fboDst3 = make_fbo(3, 3, &texDst3);
    GLuint fboBigL = make_fbo(BIG, BIG, &texBigL);
    (void)texSrc; (void)texDst4; (void)texBigN; (void)texSrcG; (void)texDst3; (void)texBigL;
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) { printf("GL_BLIT_SCALE_INCOMPLETE\n"); return 1; }

    float full[12] = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(full), full, GL_STATIC_DRAW);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    unsigned char *buf = malloc(BIG * BIG * 4);

    /* ================= Frame 1: source truth (2x2, four distinct texels) ================= */
    render_src(dpy, surf, fboSrc, progN);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboSrc);
    unsigned char srcT[SRC * SRC * 4];
    memset(srcT, 0xAB, sizeof(srcT));
    glReadPixels(0, 0, SRC, SRC, GL_RGBA, GL_UNSIGNED_BYTE, srcT);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLIT_SCALE_SRC_READ_FAILED err=0x%x\n", e); return 2; }
#define SRCT(x, y) (&srcT[(((y) * SRC) + (x)) * 4])
    printf("GL_BLIT_SCALE_SRC: (0,0)=%u,%u,%u (1,0)=%u,%u,%u (0,1)=%u,%u,%u (1,1)=%u,%u,%u\n",
           SRCT(0,0)[0], SRCT(0,0)[1], SRCT(0,0)[2], SRCT(1,0)[0], SRCT(1,0)[1], SRCT(1,0)[2],
           SRCT(0,1)[0], SRCT(0,1)[1], SRCT(0,1)[2], SRCT(1,1)[0], SRCT(1,1)[1], SRCT(1,1)[2]);
    /* The four texels must be distinct (a real 2x2 pattern, not a flat fill). */
    int distinct = 1;
    for (int i = 0; i < 4 && distinct; i++)
        for (int j = i + 1; j < 4; j++)
            if (srcT[i*4] == srcT[j*4] && srcT[i*4+1] == srcT[j*4+1] && srcT[i*4+2] == srcT[j*4+2]) distinct = 0;
    if (!distinct) { printf("GL_BLIT_SCALE_SRC_NOT_DISTINCT\n"); return 3; }
    eglSwapBuffers(dpy, surf);

    /* ================= Frame 2: NEAREST 2x2 -> 4x4, exact 2x2 blocks ================= */
    render_src(dpy, surf, fboSrc, progN);
    glBindFramebuffer(GL_FRAMEBUFFER, fboDst4);
    glViewport(0, 0, 4, 4);
    glClearColor(1.0f, 0.0f, 1.0f, 1.0f); /* magenta sentinel */
    glClear(GL_COLOR_BUFFER_BIT);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboSrc);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, fboDst4);
    glBlitFramebuffer(0, 0, SRC, SRC, 0, 0, 4, 4, GL_COLOR_BUFFER_BIT, GL_NEAREST);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboDst4);
    memset(buf, 0xAB, 4 * 4 * 4);
    glReadPixels(0, 0, 4, 4, GL_RGBA, GL_UNSIGNED_BYTE, buf);
    e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLIT_SCALE_N_READ_FAILED err=0x%x\n", e); return 4; }
    int near_ok = 1;
    for (int y = 0; y < 4; y++) {
        for (int x = 0; x < 4; x++) {
            unsigned char *g = &buf[((y * 4) + x) * 4];
            unsigned char *w = SRCT(x / 2, y / 2); /* full 0,0 blit ⇒ dst(x,y) replicates src(x/2,y/2) */
            if (!near(g[0], w[0]) || !near(g[1], w[1]) || !near(g[2], w[2])) {
                printf("GL_BLIT_SCALE_N_MISMATCH at (%d,%d): got %u,%u,%u want %u,%u,%u\n",
                       x, y, g[0], g[1], g[2], w[0], w[1], w[2]);
                near_ok = 0;
            }
            /* sentinel (255,0,255) must be gone everywhere */
            if (near(g[0], 255) && near(g[1], 0) && near(g[2], 255)) {
                printf("GL_BLIT_SCALE_N_SENTINEL at (%d,%d) — blit did not write this texel\n", x, y);
                near_ok = 0;
            }
        }
    }
    if (!near_ok) return 5;
    eglSwapBuffers(dpy, surf);

    /* ================= Frame 3: NEAREST 2x2 -> 64x64, crisp 32x32 blocks ================= */
    render_src(dpy, surf, fboSrc, progN);
    glBindFramebuffer(GL_FRAMEBUFFER, fboBigN);
    glViewport(0, 0, BIG, BIG);
    glClearColor(1.0f, 0.0f, 1.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboSrc);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, fboBigN);
    glBlitFramebuffer(0, 0, SRC, SRC, 0, 0, BIG, BIG, GL_COLOR_BUFFER_BIT, GL_NEAREST);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboBigN);
    memset(buf, 0xAB, BIG * BIG * 4);
    glReadPixels(0, 0, BIG, BIG, GL_RGBA, GL_UNSIGNED_BYTE, buf);
    e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLIT_SCALE_BN_READ_FAILED err=0x%x\n", e); return 6; }
    int big_ok = 1;
    for (int y = 0; y < BIG; y++) {
        for (int x = 0; x < BIG; x++) {
            unsigned char *g = &buf[((y * BIG) + x) * 4];
            unsigned char *w = SRCT(x / 32, y / 32);
            if (!near(g[0], w[0]) || !near(g[1], w[1]) || !near(g[2], w[2])) { big_ok = 0; }
        }
    }
    if (!big_ok) { printf("GL_BLIT_SCALE_BN_BLOCKS_WRONG\n"); return 7; }
    eglSwapBuffers(dpy, surf);

    /* ================= Frame 4: gradient source truth (al | bl) ================= */
    render_src(dpy, surf, fboSrcG, progG);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboSrcG);
    unsigned char grd[SRC * SRC * 4];
    memset(grd, 0xAB, sizeof(grd));
    glReadPixels(0, 0, SRC, SRC, GL_RGBA, GL_UNSIGNED_BYTE, grd);
    e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLIT_SCALE_G_READ_FAILED err=0x%x\n", e); return 8; }
    /* both source rows identical ⇒ columns are al (x=0) and bl (x=1) */
    unsigned char al[3] = { grd[0], grd[1], grd[2] };
    unsigned char bl[3] = { grd[4], grd[5], grd[6] };
    unsigned char mid[3] = { (unsigned char)(((int)al[0] + bl[0]) / 2),
                             (unsigned char)(((int)al[1] + bl[1]) / 2),
                             (unsigned char)(((int)al[2] + bl[2]) / 2) };
    printf("GL_BLIT_SCALE_GRAD: al=%u,%u,%u bl=%u,%u,%u mid=%u,%u,%u\n",
           al[0], al[1], al[2], bl[0], bl[1], bl[2], mid[0], mid[1], mid[2]);
    if (near(al[0], bl[0]) && near(al[1], bl[1]) && near(al[2], bl[2])) { printf("GL_BLIT_SCALE_GRAD_FLAT\n"); return 9; }
    eglSwapBuffers(dpy, surf);

    /* ================= Frame 5: LINEAR 2x2 -> 3x3, exact midpoint ================= */
    render_src(dpy, surf, fboSrcG, progG);
    glBindFramebuffer(GL_FRAMEBUFFER, fboDst3);
    glViewport(0, 0, 3, 3);
    glClearColor(1.0f, 0.0f, 1.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboSrcG);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, fboDst3);
    glBlitFramebuffer(0, 0, SRC, SRC, 0, 0, 3, 3, GL_COLOR_BUFFER_BIT, GL_LINEAR);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboDst3);
    memset(buf, 0xAB, 3 * 3 * 4);
    glReadPixels(0, 0, 3, 3, GL_RGBA, GL_UNSIGNED_BYTE, buf);
    e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLIT_SCALE_L_READ_FAILED err=0x%x\n", e); return 10; }
    int lin_ok = 1;
    for (int y = 0; y < 3; y++) {
        unsigned char *c0 = &buf[((y * 3) + 0) * 4];
        unsigned char *c1 = &buf[((y * 3) + 1) * 4]; /* center column: u=0.5 ⇒ exact midpoint */
        unsigned char *c2 = &buf[((y * 3) + 2) * 4];
        if (!near(c0[0], al[0]) || !near(c0[1], al[1]) || !near(c0[2], al[2])) {
            printf("GL_BLIT_SCALE_L_LEFT row %d: got %u,%u,%u want al %u,%u,%u\n", y, c0[0], c0[1], c0[2], al[0], al[1], al[2]);
            lin_ok = 0;
        }
        if (!near(c1[0], mid[0]) || !near(c1[1], mid[1]) || !near(c1[2], mid[2])) {
            printf("GL_BLIT_SCALE_L_MID row %d: got %u,%u,%u want mid %u,%u,%u\n", y, c1[0], c1[1], c1[2], mid[0], mid[1], mid[2]);
            lin_ok = 0;
        }
        if (!near(c2[0], bl[0]) || !near(c2[1], bl[1]) || !near(c2[2], bl[2])) {
            printf("GL_BLIT_SCALE_L_RIGHT row %d: got %u,%u,%u want bl %u,%u,%u\n", y, c2[0], c2[1], c2[2], bl[0], bl[1], bl[2]);
            lin_ok = 0;
        }
    }
    if (!lin_ok) return 11;
    eglSwapBuffers(dpy, surf);

    /* ================= Frame 6: LINEAR 2x2 -> 64x64, smooth gradient ================= */
    render_src(dpy, surf, fboSrcG, progG);
    glBindFramebuffer(GL_FRAMEBUFFER, fboBigL);
    glViewport(0, 0, BIG, BIG);
    glClearColor(1.0f, 0.0f, 1.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboSrcG);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, fboBigL);
    glBlitFramebuffer(0, 0, SRC, SRC, 0, 0, BIG, BIG, GL_COLOR_BUFFER_BIT, GL_LINEAR);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, fboBigL);
    memset(buf, 0xAB, BIG * BIG * 4);
    glReadPixels(0, 0, BIG, BIG, GL_RGBA, GL_UNSIGNED_BYTE, buf);
    e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_BLIT_SCALE_BL_READ_FAILED err=0x%x\n", e); return 12; }
    /* Red channel must be non-decreasing left→right (a real gradient, not blocks). al[0] < bl[0]. */
    int mono_ok = 1;
    for (int y = 0; y < BIG; y++) {
        for (int x = 1; x < BIG; x++) {
            int l = buf[((y * BIG) + (x - 1)) * 4];
            int r = buf[((y * BIG) + x) * 4];
            if (r + 1 < l) { mono_ok = 0; }
        }
    }
    if (!mono_ok) { printf("GL_BLIT_SCALE_BL_NOT_MONOTONIC\n"); return 13; }
    eglSwapBuffers(dpy, surf);

    free(buf);
    printf("GL_BLIT_SCALE_OK\n");
    return 0;
}
