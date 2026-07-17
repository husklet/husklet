/* REAL GL UNIFORM BLOCK (UBO) — a real EGL + GLES3 offscreen program whose per-draw transform lives in a
 * std140 uniform BLOCK bound the UBO way (glBufferData + glBindBufferBase), rasterized on lavapipe. The
 * regression guard for GskGpu/GTK4, which carries its `mat4 mvp` in exactly this kind of block.
 *
 * This exercises the bug that left GTK4 frames BLANK: a GLES program reads its transform from
 * `layout(std140, binding = 0) uniform Block { mat4 mvp; vec4 uColor; };`, set NOT via glUniform* but by
 * uploading std140 bytes into a buffer and binding it with glBindBufferBase(GL_UNIFORM_BUFFER, 0, buf). If
 * the shim does not route THAT bound buffer's bytes to the shader's IR binding 0 — instead shipping the
 * empty default-uniform block (all zeros, since glUniform* is never called) — then `mvp` is the zero matrix,
 * every vertex collapses to gl_Position = 0 (w = 0, degenerate), and NOTHING rasterizes: the frame stays the
 * clear color. That is the exact GTK "presents but blank" symptom.
 *
 * The scene: a full-NDC quad [-1,1]^2 (two triangles), transformed by an MVP that squeezes it into the
 * RIGHT HALF of NDC (x in [0,1], full height), filled with uColor = solid RED — both the matrix AND the
 * color come from the block. A correct route therefore paints RED on the right half (e.g. pixel (48,32)) and
 * leaves the BLUE clear on the left half (pixel (16,32)). With the bug (mvp = 0) the quad is degenerate, so
 * the right half stays BLUE — a direct detector: the transformed geometry is visible ONLY if the block's
 * bytes reached the shader. (Splitting on X, not Y, makes the check independent of readback y-orientation.)
 * Prints "GL_UBO_OK" on success.
 *
 * No hl-specific calls, no vendor headers: the GLES3/EGL ABI is self-declared, and
 * LD_LIBRARY_PATH=~/.hl/gl/<arch> makes libEGL.so.1/libGLESv2.so.2 OURS. Every GL call lowers to hl-GPU IR
 * shipped over $HL_GPU_EXEC to the host WgpuExecutor on lavapipe. */
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

/* aPos.xy in local clip space; gl_Position = mvp * vec4(aPos, 0, 1). The block (mvp + uColor) is anonymous
 * so its members are referenced by plain name — matching the shim's translated std140 block emission. */
static const char *VS =
    "attribute vec2 aPos;\n"
    "layout(std140, binding = 0) uniform Block { mat4 mvp; vec4 uColor; };\n"
    "void main() { gl_Position = mvp * vec4(aPos, 0.0, 1.0); }\n";

/* The fill color ALSO comes from the block — so a correct read of the block is visible in the pixel color,
 * not just the geometry position. */
static const char *FS =
    "precision mediump float;\n"
    "layout(std140, binding = 0) uniform Block { mat4 mvp; vec4 uColor; };\n"
    "void main() { gl_FragColor = uColor; }\n";

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

    /* A full-NDC quad [-1,1]^2 as two triangles. The MVP (below) squeezes it into the right half, so the
     * transform is PROVABLE by where the geometry ends up. */
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

    /* The std140 block bytes: mat4 mvp @0 (64 bytes, column-major), vec4 uColor @64 (16 bytes) = 80 bytes.
     * mvp maps NDC x -> [0,1], y unchanged:  x' = 0.5x + 0.5,  y' = y  (right half, full height). uColor is
     * solid RED. The app lays these out std140 itself and hands them over via glBindBufferBase — the shim
     * must route THESE bytes to binding 0. */
    float block[20] = {
        /* mvp, column-major */
        0.5f, 0.0f, 0.0f, 0.0f, /* col0 */
        0.0f, 1.0f, 0.0f, 0.0f, /* col1 */
        0.0f, 0.0f, 1.0f, 0.0f, /* col2 */
        0.5f, 0.0f, 0.0f, 1.0f, /* col3 (translation in x) */
        /* uColor = RED */
        1.0f, 0.0f, 0.0f, 1.0f,
    };
    GLuint ubo;
    glGenBuffers(1, &ubo);
    glBindBuffer(GL_UNIFORM_BUFFER, ubo);
    glBufferData(GL_UNIFORM_BUFFER, sizeof(block), block, GL_STATIC_DRAW);
    /* Bind the block to binding point 0 (the shader declares binding = 0), the UBO way — no glUniform*. */
    GLuint bidx = glGetUniformBlockIndex(prog, "Block");
    glUniformBlockBinding(prog, bidx, 0);
    glBindBufferBase(GL_UNIFORM_BUFFER, 0, ubo);

    glClearColor(0.0f, 0.0f, 1.0f, 1.0f); /* blue clear (distinct from the red fill) */
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_UBO_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

    /* (48,32): right half, inside the transformed quad → RED (the block's mvp moved geometry here AND its
     * uColor painted it). (16,32): left half, outside the transformed quad → the BLUE clear. */
    unsigned char *hit = &px[((32) * W + 48) * 4];
    unsigned char *miss = &px[((32) * W + 16) * 4];
    printf("GL_UBO_HIT: %u %u %u %u\n", hit[0], hit[1], hit[2], hit[3]);
    printf("GL_UBO_MISS: %u %u %u %u\n", miss[0], miss[1], miss[2], miss[3]);

    int hit_red = near(hit[0], 255) && near(hit[1], 0) && near(hit[2], 0) && hit[3] == 255;
    int miss_blue = near(miss[0], 0) && near(miss[1], 0) && near(miss[2], 255) && miss[3] == 255;
    if (hit_red && miss_blue) {
        printf("GL_UBO_OK\n");
        free(px);
        return 0;
    }
    printf("GL_UBO_WRONG hit=(%u %u %u %u) miss=(%u %u %u %u) — the std140 block's mvp/uColor did not reach "
           "the shader (blank: mvp = 0 -> degenerate geometry)\n",
           hit[0], hit[1], hit[2], hit[3], miss[0], miss[1], miss[2], miss[3]);
    free(px);
    return 3;
}
