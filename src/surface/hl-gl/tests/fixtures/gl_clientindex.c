/* REAL GL CLIENT-SIDE INDEXED DRAW — a real EGL + GLES2 offscreen program that RASTERIZES a quad from a
 * CLIENT-SIDE vertex array AND a CLIENT-SIDE index array, with NO vertex- OR element-array buffer object
 * bound.
 *
 * The indexed counterpart of `gl_clientarray.c`: `glDrawElements(mode, count, type, indices)` where
 * `indices` points into a local array (no GL_ELEMENT_ARRAY_BUFFER bound) and the vertices likewise live in
 * a stack array bound via glVertexAttribPointer with buffer 0. The shim captures BOTH the client vertex
 * array and the client index array into transient buffers at draw time (promoting the u8 indices to u16),
 * so the quad genuinely rasterizes on lavapipe and `glReadPixels` reads it back.
 *
 * Scene: clear RED, draw a GREEN quad (two triangles, 6 indices) covering the center but NOT the corners.
 * A covered interior pixel must be GREEN; an uncovered corner must be the RED clear. Prints
 * "GL_CLIENTINDEX_OK" on success. */
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
extern void glVertexAttribPointer(GLuint, GLint, GLenum, unsigned char, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glViewport(GLint, GLint, GLsizei, GLsizei);
extern void glDrawElements(GLenum, GLsizei, GLenum, const void *);
extern void glReadPixels(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *);
extern GLenum glGetError(void);

#define EGL_OPENGL_ES_API 0x30A0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
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
    "void main() { gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0); }\n"; /* green */

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

    /* A centered quad in CLIENT-SIDE arrays: 4 corner vertices + 6 u8 indices (two triangles). NO
     * glGenBuffers / glBindBuffer for either the vertices OR the indices — both pointers are client memory.
     * The quad spans [-0.6, 0.6] so it covers the center but leaves the image corners as the red clear. */
    float verts[8] = {
        -0.6f, -0.6f, /* 0 */
        0.6f,  -0.6f, /* 1 */
        0.6f,  0.6f,  /* 2 */
        -0.6f, 0.6f,  /* 3 */
    };
    unsigned char idx[6] = {0, 1, 2, 0, 2, 3};

    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 0, verts);
    glEnableVertexAttribArray((GLuint)aPos);

    glClearColor(1.0f, 0.0f, 0.0f, 1.0f); /* red clear */
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_BYTE, idx);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_CLIENTINDEX_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

    unsigned char *center = &px[((H / 2) * W + (W / 2)) * 4]; /* covered → green */
    unsigned char *corner = &px[0];                            /* uncovered → red clear */
    printf("GL_CLIENTINDEX_CENTER: %u %u %u %u\n", center[0], center[1], center[2], center[3]);
    printf("GL_CLIENTINDEX_CORNER: %u %u %u %u\n", corner[0], corner[1], corner[2], corner[3]);

    int center_green = near(center[0], 0) && near(center[1], 255) && near(center[2], 0) && near(center[3], 255);
    int corner_red = near(corner[0], 255) && near(corner[1], 0) && near(corner[2], 0) && near(corner[3], 255);
    if (center_green && corner_red) {
        printf("GL_CLIENTINDEX_OK\n");
        free(px);
        return 0;
    }
    printf("GL_CLIENTINDEX_WRONG center_green=%d corner_red=%d\n", center_green, corner_red);
    free(px);
    return 3;
}
