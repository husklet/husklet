/* REAL GL UBO TRANSFORM — a real EGL + GLES3 offscreen program whose MVP lives in a std140 uniform BLOCK
 * (set the UBO way: glGenBuffers + glBufferData(std140 bytes) + glBindBufferBase(GL_UNIFORM_BUFFER, 0, ubo)
 * + glUniformBlockBinding), rasterized on lavapipe. The EXACT-RECT counterpart of gl_ubo.c.
 *
 * Where gl_ubo.c proves the block's bytes merely REACH the shader (right-half red vs left-half blue), this
 * program pins the transform to a KNOWN on-screen rectangle: the MVP both SCALES (0.5x, 0.5x) AND TRANSLATES
 * (+0.25 in x), so a full-NDC quad [-1,1]^2 lands at NDC x in [-0.25, 0.75], y in [-0.5, 0.5]. On a 64x64
 * target that is pixel columns [24, 56) x rows [16, 48) — an off-center rectangle. We sample INSIDE (green
 * fill from uColor) and just OUTSIDE all four edges (blue clear): a direct detector that the shim binds the
 * glBindBufferBase'd UBO's std140 bytes at binding 0 (not a synthesized flat/zero block). A wrong route
 * either collapses the geometry (mvp = 0 -> nothing) or misplaces it (identity -> full frame), and either
 * moves at least one of the five sample pixels off its expected color.
 *
 * The x translate makes the rect asymmetric in X (so a swapped/zeroed translation column is visible); the
 * y transform is kept symmetric so the check is independent of readback y-orientation. uColor (also from the
 * block) paints the fill GREEN, so a correct read shows in BOTH geometry position AND pixel color.
 *
 * No hl-specific calls, no vendor headers: the GLES3/EGL ABI is self-declared, and
 * LD_LIBRARY_PATH=~/.hl/gl/<arch> makes libEGL.so.1/libGLESv2.so.2 OURS. Every GL call lowers to hl-GPU IR
 * shipped over $HL_GPU_EXEC to the host WgpuExecutor on lavapipe. Prints "GL_UBOXFORM_OK" on success. */
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
extern GLuint glGetUniformBlockIndex(GLuint, const GLchar *);
extern void glUniformBlockBinding(GLuint, GLuint, GLuint);
extern void glGenBuffers(GLsizei, GLuint *);
extern void glBindBuffer(GLenum, GLuint);
extern void glBindBufferBase(GLenum, GLuint, GLuint);
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
#define GL_UNIFORM_BUFFER 0x8A11
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
    "layout(std140, binding = 0) uniform Block { mat4 mvp; vec4 uColor; };\n"
    "void main() { gl_Position = mvp * vec4(aPos, 0.0, 1.0); }\n";

static const char *FS =
    "precision mediump float;\n"
    "layout(std140, binding = 0) uniform Block { mat4 mvp; vec4 uColor; };\n"
    "void main() { gl_FragColor = uColor; }\n";

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 4;
}
static int is_green(const unsigned char *p) {
    return near(p[0], 0) && near(p[1], 255) && near(p[2], 0) && p[3] == 255;
}
static int is_blue(const unsigned char *p) {
    return near(p[0], 0) && near(p[1], 0) && near(p[2], 255) && p[3] == 255;
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

    /* Full-NDC quad [-1,1]^2 as two triangles; the MVP relocates + shrinks it into a known rect. */
    float verts[12] = {
        -1.0f, -1.0f,  1.0f, -1.0f,  -1.0f, 1.0f,
         1.0f, -1.0f,  1.0f,  1.0f,  -1.0f, 1.0f,
    };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 8, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    /* std140 block: mat4 mvp @0 (column-major) + vec4 uColor @64. mvp: x' = 0.5x + 0.25, y' = 0.5y.
     * -> quad NDC x in [-0.25,0.75] (cols [24,56)), y in [-0.5,0.5] (rows [16,48)). uColor = GREEN. */
    float block[20] = {
        /* mvp, column-major */
        0.5f,  0.0f, 0.0f, 0.0f, /* col0 */
        0.0f,  0.5f, 0.0f, 0.0f, /* col1 */
        0.0f,  0.0f, 1.0f, 0.0f, /* col2 */
        0.25f, 0.0f, 0.0f, 1.0f, /* col3: translate +0.25 in x */
        /* uColor = GREEN */
        0.0f, 1.0f, 0.0f, 1.0f,
    };
    GLuint ubo;
    glGenBuffers(1, &ubo);
    glBindBuffer(GL_UNIFORM_BUFFER, ubo);
    glBufferData(GL_UNIFORM_BUFFER, sizeof(block), block, GL_STATIC_DRAW);
    GLuint bidx = glGetUniformBlockIndex(prog, "Block");
    glUniformBlockBinding(prog, bidx, 0);
    glBindBufferBase(GL_UNIFORM_BUFFER, 0, ubo);

    glClearColor(0.0f, 0.0f, 1.0f, 1.0f); /* blue clear */
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_UBOXFORM_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

#define AT(x, y) (&px[(((y) * W) + (x)) * 4])
    unsigned char *hit = AT(40, 32);   /* inside the rect  -> GREEN */
    unsigned char *ml = AT(12, 32);    /* left of x'=-0.25 -> BLUE  */
    unsigned char *mr = AT(58, 32);    /* right of x'=0.75 -> BLUE  */
    unsigned char *mtop = AT(40, 6);   /* above the rect   -> BLUE  */
    unsigned char *mbot = AT(40, 58);  /* below the rect   -> BLUE  */
    printf("GL_UBOXFORM_HIT: %u %u %u %u\n", hit[0], hit[1], hit[2], hit[3]);
    printf("GL_UBOXFORM_ML: %u %u %u %u\n", ml[0], ml[1], ml[2], ml[3]);
    printf("GL_UBOXFORM_MR: %u %u %u %u\n", mr[0], mr[1], mr[2], mr[3]);

    int ok = is_green(hit) && is_blue(ml) && is_blue(mr) && is_blue(mtop) && is_blue(mbot);
    if (ok) {
        printf("GL_UBOXFORM_OK\n");
        free(px);
        return 0;
    }
    printf("GL_UBOXFORM_WRONG hit=(%u %u %u %u) ml=(%u %u %u %u) mr=(%u %u %u %u) — the std140 MVP block did "
           "not place the quad at the expected rect (cols 24..56, rows 16..48)\n",
           hit[0], hit[1], hit[2], hit[3], ml[0], ml[1], ml[2], ml[3], mr[0], mr[1], mr[2], mr[3]);
    free(px);
    return 3;
}
