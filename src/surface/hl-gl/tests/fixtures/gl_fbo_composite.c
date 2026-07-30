/* REAL GL MULTI-FBO COMPOSITE — a real EGL + GLES2 program that renders into an OFFSCREEN framebuffer, then
 * binds the DEFAULT framebuffer and draws a fullscreen quad that SAMPLES that offscreen texture, rasterized
 * on lavapipe. The regression counterpart of `gl_geometry.c` for the multi-FBO frame-graph.
 *
 * This exercises the bug that blocked GskGL / GTK4 and every offscreen-FBO GL app: a frame that renders
 * across MORE THAN ONE framebuffer (an offscreen render target, then the window). The old frame lowering
 * collapsed the ENTIRE recorded frame onto the FIRST geometry draw's FBO — the small offscreen target — so
 * what got presented + read back was the offscreen size (here 16x16), not the window (64x64). The fix lowers
 * the frame as a SEQUENCE of render passes (one per bound FBO): pass 1 renders into the offscreen texture,
 * pass 2 (the DEFAULT framebuffer) samples it and composites into the WINDOW color target — and THAT window
 * target, at window dimensions, is what eglSwapBuffers/glReadPixels present + read back.
 *
 * The scene: pass 1 fills a 16x16 offscreen FBO with four asymmetric quadrants in GL coordinates:
 * bottom-left RED, bottom-right BLUE, top-left GREEN, top-right YELLOW. Pass 2 samples that texture into the
 * 64x64 default framebuffer. Exact quadrant checks prove both multi-FBO routing and orientation; the X
 * sentinel prevents a 180-degree rotation from masquerading as a vertical flip. Prints
 * "GL_FBO_COMPOSITE_OK" on success.
 *
 * No hl-specific calls, no vendor headers: the GLES2/EGL ABI is self-declared, and LD_LIBRARY_PATH=~/.hl/gl
 * makes libEGL.so.1/libGLESv2.so.2 OURS. Every GL call lowers to hl-GPU IR shipped over $HL_GPU_EXEC to the
 * host WgpuExecutor on lavapipe; the guest forwards its GLSL-ES verbatim. */
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
extern void glGenFramebuffers(GLsizei, GLuint *);
extern void glBindFramebuffer(GLenum, GLuint);
extern void glFramebufferTexture2D(GLenum, GLenum, GLenum, GLuint, GLint);
extern GLenum glCheckFramebufferStatus(GLenum);
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
#define GL_NEAREST 0x2600
#define GL_FRAMEBUFFER 0x8D40
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5
#define GL_NO_ERROR 0

/* The window (default framebuffer) is 64x64; the offscreen atlas is a deliberately TINY 16x16 so a window
 * pixel far outside 16x16 is proof the presented target is the window, not the collapsed offscreen. */
#define W 64
#define H 64
#define AW 16
#define AH 16

/* Four asymmetric quadrants in GL framebuffer coordinates. */
static const char *VS_FLAT =
    "attribute vec2 aPos;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char *FS_FLAT =
    "precision mediump float;\n"
    "void main() {\n"
    "  bool left = gl_FragCoord.x < 8.0;\n"
    "  bool bottom = gl_FragCoord.y < 8.0;\n"
    "  if (bottom && left) gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0);\n"
    "  else if (bottom) gl_FragColor = vec4(0.0, 0.0, 1.0, 1.0);\n"
    "  else if (left) gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0);\n"
    "  else gl_FragColor = vec4(1.0, 1.0, 0.0, 1.0);\n"
    "}\n";

/* Textured shader — samples the offscreen texture for the default-framebuffer composite. */
static const char *VS_TEX =
    "attribute vec2 aPos;\n"
    "attribute vec2 aUV;\n"
    "varying vec2 vUV;\n"
    "void main() { vUV = aUV; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char *FS_TEX =
    "precision mediump float;\n"
    "varying vec2 vUV;\n"
    "uniform sampler2D uTex;\n"
    "void main() { gl_FragColor = texture2D(uTex, vUV); }\n";

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 3;
}

static GLuint make_program(const char *vs_src, const char *fs_src) {
    GLuint vs = glCreateShader(GL_VERTEX_SHADER);
    glShaderSource(vs, 1, &vs_src, 0);
    glCompileShader(vs);
    GLuint fs = glCreateShader(GL_FRAGMENT_SHADER);
    glShaderSource(fs, 1, &fs_src, 0);
    glCompileShader(fs);
    GLuint prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);
    glLinkProgram(prog);
    return prog;
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

    /* A single interleaved [x, y, u, v] fullscreen-covering triangle, reused by both passes. */
    float verts[12] = {
        -1.0f, -1.0f, 0.0f, 0.0f,
         3.0f, -1.0f, 2.0f, 0.0f,
        -1.0f,  3.0f, 0.0f, 2.0f,
    };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);

    GLuint flat = make_program(VS_FLAT, FS_FLAT);
    GLuint tex = make_program(VS_TEX, FS_TEX);

    /* The offscreen color texture + its framebuffer object. */
    GLuint atlas;
    glGenTextures(1, &atlas);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, atlas);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, AW, AH, 0, GL_RGBA, GL_UNSIGNED_BYTE, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

    GLuint fbo;
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, atlas, 0);
    GLenum fbs = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (fbs != GL_FRAMEBUFFER_COMPLETE) { printf("GL_FBO_COMPOSITE_INCOMPLETE 0x%x\n", fbs); return 1; }

    /* ---- Pass 1: render solid RED into the 16x16 offscreen FBO (flat shader). ---- */
    glViewport(0, 0, AW, AH);
    glUseProgram(flat);
    {
        GLint aPos = glGetAttribLocation(flat, "aPos");
        if (aPos < 0) aPos = 0;
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 16, 0);
        glEnableVertexAttribArray((GLuint)aPos);
    }
    glDrawArrays(GL_TRIANGLES, 0, 3);

    /* ---- Pass 2: default framebuffer — clear BLUE, then composite the offscreen texture over the WINDOW. */
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, W, H);
    glUseProgram(tex);
    {
        GLint aPos = glGetAttribLocation(tex, "aPos");
        GLint aUV = glGetAttribLocation(tex, "aUV");
        if (aPos < 0) aPos = 0;
        if (aUV < 0) aUV = 1;
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 16, 0);
        glEnableVertexAttribArray((GLuint)aPos);
        glVertexAttribPointer((GLuint)aUV, 2, GL_FLOAT, 0, 16, (const void *)8);
        glEnableVertexAttribArray((GLuint)aUV);
    }
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, atlas); /* sample the offscreen FBO's color attachment */
    glUniform1i(glGetUniformLocation(tex, "uTex"), 0);
    glClearColor(0.0f, 0.0f, 1.0f, 1.0f); /* blue clear — any uncomposited pixel would show blue */
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 3);

    /* ---- Read back the DEFAULT framebuffer at the FULL window size. ---- */
    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_FBO_COMPOSITE_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

    unsigned char *bottom_left = &px[(8 * W + 8) * 4];
    unsigned char *bottom_right = &px[(8 * W + 56) * 4];
    unsigned char *top_left = &px[(56 * W + 8) * 4];
    unsigned char *top_right = &px[(56 * W + 56) * 4];
    printf("GL_FBO_COMPOSITE_BOTTOM_LEFT: %u %u %u %u\n",
           bottom_left[0], bottom_left[1], bottom_left[2], bottom_left[3]);
    printf("GL_FBO_COMPOSITE_BOTTOM_RIGHT: %u %u %u %u\n",
           bottom_right[0], bottom_right[1], bottom_right[2], bottom_right[3]);
    printf("GL_FBO_COMPOSITE_TOP_LEFT: %u %u %u %u\n",
           top_left[0], top_left[1], top_left[2], top_left[3]);
    printf("GL_FBO_COMPOSITE_TOP_RIGHT: %u %u %u %u\n",
           top_right[0], top_right[1], top_right[2], top_right[3]);

    int ok = near(bottom_left[0], 255) && near(bottom_left[1], 0) && near(bottom_left[2], 0)
          && near(bottom_right[0], 0) && near(bottom_right[1], 0) && near(bottom_right[2], 255)
          && near(top_left[0], 0) && near(top_left[1], 255) && near(top_left[2], 0)
          && near(top_right[0], 255) && near(top_right[1], 255) && near(top_right[2], 0)
          && bottom_left[3] == 255 && bottom_right[3] == 255
          && top_left[3] == 255 && top_right[3] == 255;
    if (ok) {
        printf("GL_FBO_COMPOSITE_OK\n");
        free(px);
        return 0;
    }
    printf("GL_FBO_COMPOSITE_WRONG bottom_left=(%u,%u,%u,%u) bottom_right=(%u,%u,%u,%u) "
           "top_left=(%u,%u,%u,%u) top_right=(%u,%u,%u,%u)\n",
           bottom_left[0], bottom_left[1], bottom_left[2], bottom_left[3],
           bottom_right[0], bottom_right[1], bottom_right[2], bottom_right[3],
           top_left[0], top_left[1], top_left[2], top_left[3],
           top_right[0], top_right[1], top_right[2], top_right[3]);
    free(px);
    return 3;
}
