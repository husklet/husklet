/* REAL GL STENCIL TEST — a real EGL + GLES2 offscreen program that proves the stencil test GATES rendering
 * to a marked region, rasterized on lavapipe. Exercises glEnable(GL_STENCIL_TEST) + glStencilFunc /
 * glStencilOp / glStencilMask / glClearStencil + a real Depth24PlusStencil8 depth-stencil attachment.
 *
 * The scene, over a BLUE color clear + a stencil clear to 0:
 *   PASS A — a CENTERED rect (NDC [-0.5,0.5]^2 → pixels [16,48)) drawn with COLOR WRITES OFF
 *            (glColorMask(0,0,0,0)) and glStencilFunc(GL_ALWAYS,1,0xFF) + glStencilOp(KEEP,KEEP,REPLACE):
 *            it writes NOTHING to color but STAMPS the stencil buffer to 1 inside that rect.
 *   PASS B — a FULLSCREEN RED quad ([-1,1]^2) drawn with COLOR WRITES ON and
 *            glStencilFunc(GL_EQUAL,1,0xFF) + glStencilOp(KEEP,KEEP,KEEP): the stencil test PASSES only
 *            where pass A stamped 1 (the center rect), so ONLY the center becomes RED; everywhere else the
 *            fragment is DISCARDED and the BLUE clear survives.
 *
 * This is a genuine stencil gate, not paint order: pass A wrote no color at all, so the ONLY way the center
 * is RED and the border is BLUE is the stencil test rejecting the fullscreen quad's border fragments. With
 * the stencil test DISABLED (env HL_STENCIL_DISABLE=1 → glDisable(GL_STENCIL_TEST) before pass B) the
 * fullscreen quad is UNGATED and the WHOLE frame becomes RED — the regression proof the gate is real.
 *
 * Color from a `uniform vec4 uColor`; no depth is used (z=0). No hl-specific calls, no vendor headers.
 * Prints "GL_STENCIL_OK" on success. */
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
extern void glColorMask(unsigned char, unsigned char, unsigned char, unsigned char);
extern void glStencilFunc(GLenum, GLint, GLuint);
extern void glStencilOp(GLenum, GLenum, GLenum);
extern void glStencilMask(GLuint);
extern void glClearStencil(GLint);
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
#define GL_STENCIL_BUFFER_BIT 0x0400
#define GL_STENCIL_TEST 0x0B90
#define GL_ALWAYS 0x0207
#define GL_EQUAL 0x0202
#define GL_KEEP 0x1E00
#define GL_REPLACE 0x1E01
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
    int disable = getenv("HL_STENCIL_DISABLE") != NULL; /* regression proof: no gate → whole frame RED */

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

    /* CENTER rect: NDC [-0.5,0.5]^2. FULLSCREEN: [-1,1]^2. */
    float center[12] = { -0.5f,-0.5f,  0.5f,-0.5f,  -0.5f,0.5f,  0.5f,-0.5f,  0.5f,0.5f,  -0.5f,0.5f };
    float full[12] = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    GLuint vboCenter, vboFull;
    glGenBuffers(1, &vboCenter);
    glBindBuffer(GL_ARRAY_BUFFER, vboCenter);
    glBufferData(GL_ARRAY_BUFFER, sizeof(center), center, GL_STATIC_DRAW);
    glGenBuffers(1, &vboFull);
    glBindBuffer(GL_ARRAY_BUFFER, vboFull);
    glBufferData(GL_ARRAY_BUFFER, sizeof(full), full, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;

    glEnable(GL_STENCIL_TEST);
    glClearColor(0.0f, 0.0f, 1.0f, 1.0f); /* BLUE background */
    glClearStencil(0);
    glClear(GL_COLOR_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);

    /* PASS A: stamp stencil=1 inside the center rect, writing NO color. */
    glColorMask(0, 0, 0, 0);
    glStencilFunc(GL_ALWAYS, 1, 0xFF);
    glStencilOp(GL_KEEP, GL_KEEP, GL_REPLACE);
    glStencilMask(0xFF);
    glBindBuffer(GL_ARRAY_BUFFER, vboCenter);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glUniform4f(locColor, 0.0f, 1.0f, 0.0f, 1.0f); /* ignored (color writes off) */
    glDrawArrays(GL_TRIANGLES, 0, 6);

    /* PASS B: fullscreen RED, gated to stencil==1 (the center) — unless the gate is disabled. */
    glColorMask(1, 1, 1, 1);
    if (disable) {
        glDisable(GL_STENCIL_TEST);
    } else {
        glStencilFunc(GL_EQUAL, 1, 0xFF);
        glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
    }
    glBindBuffer(GL_ARRAY_BUFFER, vboFull);
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glUniform4f(locColor, 1.0f, 0.0f, 0.0f, 1.0f); /* RED */
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_STENCIL_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    unsigned char *cen = AT(32, 32); /* center: stamped → RED (both modes) */
    unsigned char *cor = AT(8, 8);   /* corner: gated out → BLUE (enabled), RED (disabled) */
    printf("GL_STENCIL_CENTER: %u %u %u %u\n", cen[0], cen[1], cen[2], cen[3]);
    printf("GL_STENCIL_CORNER: %u %u %u %u\n", cor[0], cor[1], cor[2], cor[3]);

    int center_red = near(cen[0], 255) && near(cen[1], 0) && near(cen[2], 0) && cen[3] == 255;
    int corner_blue = near(cor[0], 0) && near(cor[1], 0) && near(cor[2], 255) && cor[3] == 255;
    int corner_red = near(cor[0], 255) && near(cor[1], 0) && near(cor[2], 0) && cor[3] == 255;

    int ok = disable ? (center_red && corner_red) : (center_red && corner_blue);
    if (ok) {
        printf("GL_STENCIL_OK mode=%s\n", disable ? "disabled" : "enabled");
        free(px);
        return 0;
    }
    printf("GL_STENCIL_WRONG mode=%s center=(%u %u %u %u) corner=(%u %u %u %u) — the stencil test did not gate "
           "the fullscreen quad to the stamped center region\n",
           disable ? "disabled" : "enabled", cen[0], cen[1], cen[2], cen[3], cor[0], cor[1], cor[2], cor[3]);
    free(px);
    return 3;
}
