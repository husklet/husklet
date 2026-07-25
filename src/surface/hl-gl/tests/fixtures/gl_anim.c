/* REAL GL ANIMATION — a real EGL + GLES2 offscreen program that draws the SAME quad across 3 frames,
 * translating it via a uniform each frame, and reads each frame back with glReadPixels, rasterized on
 * lavapipe. Proves per-frame fluency: a uniform changed between frames moves the geometry, and each frame
 * is an independent render (eglSwapBuffers resets the draw-list, so frames do not accumulate).
 *
 * Frame k (k = 0,1,2) sets uniform uOff.x = -0.5 + 0.5*k and draws a small GREEN quad, placing its center
 * at NDC x = -0.5, 0.0, +0.5 -> pixel columns 16, 32, 48 (all at row 32, orientation-independent). Each
 * frame we assert the quad is GREEN at ITS column and BLACK (clear) at the other two columns — so the quad
 * is at 3 successively different, correct positions and did NOT smear/accumulate.
 *
 * If HL_ANIM_DUMP is set, each frame's full RGBA readback (W*H*4, GL bottom-left row order) is written to
 * $HL_ANIM_DUMP/frame<k>.bin so the Rust harness can turn each into a PNG. No hl-specific calls, no vendor
 * headers. Prints "GL_ANIM_OK" on success. */
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
extern GLint glGetUniformLocation(GLuint, const GLchar *);
extern void glUniform2f(GLint, GLfloat, GLfloat);
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
#define GL_NO_ERROR 0

#define W 64
#define H 64

static const char *VS =
    "attribute vec2 aPos;\n"
    "uniform vec2 uOff;\n"
    "void main() { gl_Position = vec4(aPos * 0.15 + uOff, 0.0, 1.0); }\n";
static const char *FS =
    "precision mediump float;\n"
    "void main() { gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0); }\n"; /* green */

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 4;
}
static int is_green(const unsigned char *p) {
    return near(p[0], 0) && near(p[1], 255) && near(p[2], 0) && p[3] == 255;
}
static int is_black(const unsigned char *p) {
    return near(p[0], 0) && near(p[1], 0) && near(p[2], 0) && p[3] == 255;
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
    GLint locOff = glGetUniformLocation(prog, "uOff");

    float quad[12] = { -1,-1,  1,-1,  -1,1,  1,-1,  1,1,  -1,1 };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(quad), quad, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    const char *dump = getenv("HL_ANIM_DUMP");
    int cols[3] = {16, 32, 48}; /* the quad's center column for frames 0,1,2 */
    unsigned char *px = malloc(W * H * 4);

    for (int k = 0; k < 3; k++) {
        float ox = -0.5f + 0.5f * (float)k;
        glClearColor(0.0f, 0.0f, 0.0f, 1.0f); /* black */
        glClear(GL_COLOR_BUFFER_BIT);
        glUniform2f(locOff, ox, 0.0f);
        glDrawArrays(GL_TRIANGLES, 0, 6);

        memset(px, 0xAB, W * H * 4);
        glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
        GLenum e = glGetError();
        if (e != GL_NO_ERROR) { printf("GL_ANIM_READBACK_FAILED frame=%d err=0x%x\n", k, e); free(px); return 2; }

        if (dump && *dump) {
            char path[512];
            snprintf(path, sizeof(path), "%s/frame%d.bin", dump, k);
            FILE *f = fopen(path, "wb");
            if (f) { fwrite(px, 1, W * H * 4, f); fclose(f); }
        }

        unsigned char *here = &px[((32 * W) + cols[k]) * 4];
        int here_ok = is_green(here);
        int elsewhere_ok = 1;
        for (int j = 0; j < 3; j++) {
            if (j == k) continue;
            unsigned char *other = &px[((32 * W) + cols[j]) * 4];
            if (!is_black(other)) elsewhere_ok = 0;
        }
        printf("GL_ANIM_FRAME %d col=%d here=(%u %u %u %u) here_ok=%d elsewhere_ok=%d\n",
               k, cols[k], here[0], here[1], here[2], here[3], here_ok, elsewhere_ok);
        if (!here_ok || !elsewhere_ok) {
            printf("GL_ANIM_WRONG frame=%d — quad not at its expected column (or it accumulated)\n", k);
            free(px);
            return 3;
        }

        eglSwapBuffers(dpy, surf); /* present + reset the draw-list so the next frame is independent */
    }

    printf("GL_ANIM_OK\n");
    free(px);
    return 0;
}
