/* REAL GL MULTI-TEXTURE — a real EGL + GLES2 offscreen program that samples TWO textures AND a uniform
 * block in one draw, rasterized on lavapipe. The regression counterpart of `gl_geometry.c`.
 *
 * This exercises the bug that blocked multi-texture GUI apps: a GLES program with a uniform block AND 2+
 * samplers. In GLSL/MSL a uniform block and samplers live in SEPARATE binding namespaces, but the neutral
 * IR + wgpu share ONE binding namespace per bind group, and naga's `glsl-in` additionally REJECTS a
 * combined `uniform sampler2D` entirely. The old translator emitted the UBO at binding 1 and sampler `k` at
 * binding `k` as combined `sampler2D` — so (a) the 2nd sampler aliased the UBO at binding 1 and (b) naga
 * could not even compile the sampler declaration. The fix splits each sampler into a `texture2D` + `sampler`
 * pair recombined via the `sampler2D(tex, samp)` constructor, and assigns the UBO binding 0, sampler `k`
 * texture binding `1+2k` and sampler binding `2+2k` — all distinct, matching the frame's bind-group IR.
 *
 * The scene: two solid 2x2 textures — uTexA is pure RED (255,0,0), uTexB is pure GREEN (0,255,0) — plus a
 * data uniform `uTint` whose .z is 0.5. The fragment writes vec4(texA.r, texB.g, uTint.z, 1.0). A correct
 * bind therefore yields RGBA (255,255,128,255) at every covered pixel: R proves uTexA bound to its own
 * binding, G proves uTexB bound to ITS binding (not swapped/collided), B=128 proves the UBO bound to a
 * distinct binding (not aliased onto a sampler). Any swap or collision changes at least one channel, so the
 * result is a detector for the bug. Corners keep a distinct BLUE clear. Prints "GL_MULTITEX_OK" on success.
 *
 * No hl-specific calls, no vendor headers: the GLES2/EGL ABI is self-declared, and
 * LD_LIBRARY_PATH=~/.hl/gl/<arch> makes libEGL.so.1/libGLESv2.so.2 OURS. Every GL call lowers to hl-GPU IR
 * shipped over $HL_GPU_EXEC to the host WgpuExecutor on lavapipe; the guest forwards its GLSL-ES verbatim. */
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
extern void glUniform4f(GLint, GLfloat, GLfloat, GLfloat, GLfloat);
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
#define GL_TEXTURE1 0x84C1
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_NEAREST 0x2600
#define GL_NO_ERROR 0

#define W 64
#define H 64

/* aPos.xy in clip space, aUV.xy — a single fullscreen-covering triangle (covers the center pixel). */
static const char *VS =
    "attribute vec2 aPos;\n"
    "attribute vec2 aUV;\n"
    "varying vec2 vUV;\n"
    "void main() { vUV = aUV; gl_Position = vec4(aPos, 0.0, 1.0); }\n";

/* Two samplers AND a uniform block (uTint) — the multi-texture-plus-UBO shape that was broken. R comes from
 * uTexA, G from uTexB, B from the uniform: a per-channel provenance so any bind swap/collision is visible. */
static const char *FS =
    "precision mediump float;\n"
    "varying vec2 vUV;\n"
    "uniform sampler2D uTexA;\n"
    "uniform sampler2D uTexB;\n"
    "uniform vec4 uTint;\n"
    "void main() {\n"
    "  gl_FragColor = vec4(texture2D(uTexA, vUV).r, texture2D(uTexB, vUV).g, uTint.z, 1.0);\n"
    "}\n";

static int near(unsigned char a, unsigned char b) {
    int d = (int)a - (int)b;
    return (d < 0 ? -d : d) <= 3;
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

    /* Interleaved [x, y, u, v] per vertex; a big triangle that fully covers the 64x64 viewport. */
    float verts[12] = {
        -1.0f, -1.0f, 0.0f, 0.0f,
         3.0f, -1.0f, 2.0f, 0.0f,
        -1.0f,  3.0f, 0.0f, 2.0f,
    };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    GLint aUV = glGetAttribLocation(prog, "aUV");
    if (aPos < 0) aPos = 0;
    if (aUV < 0) aUV = 1;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 16, 0);
    glEnableVertexAttribArray((GLuint)aPos);
    glVertexAttribPointer((GLuint)aUV, 2, GL_FLOAT, 0, 16, (const void *)8);
    glEnableVertexAttribArray((GLuint)aUV);

    /* Texture A on unit 0: solid RED. Texture B on unit 1: solid GREEN. 2x2 so neither is confused with the
     * 64x64 render target when the host reads back by pixel count. */
    unsigned char redPix[16]   = {255,0,0,255, 255,0,0,255, 255,0,0,255, 255,0,0,255};
    unsigned char greenPix[16] = {0,255,0,255, 0,255,0,255, 0,255,0,255, 0,255,0,255};

    GLuint texA, texB;
    glGenTextures(1, &texA);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, texA);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 2, 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, redPix);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

    glGenTextures(1, &texB);
    glActiveTexture(GL_TEXTURE1);
    glBindTexture(GL_TEXTURE_2D, texB);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 2, 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, greenPix);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

    /* Bind samplers to their units + set the uniform block: uTint.z = 0.5 -> blue channel 128. */
    GLint locA = glGetUniformLocation(prog, "uTexA");
    GLint locB = glGetUniformLocation(prog, "uTexB");
    GLint locTint = glGetUniformLocation(prog, "uTint");
    glUniform1i(locA, 0);
    glUniform1i(locB, 1);
    glUniform4f(locTint, 0.0f, 0.0f, 0.5f, 1.0f);

    glClearColor(0.0f, 0.0f, 1.0f, 1.0f); /* blue clear (distinct from the yellow-ish combined result) */
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 3);

    unsigned char *px = malloc(W * H * 4);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e = glGetError();
    if (e != GL_NO_ERROR) { printf("GL_MULTITEX_READBACK_FAILED err=0x%x\n", e); free(px); return 2; }

    unsigned char *center = &px[((H / 2) * W + (W / 2)) * 4]; /* covered → combined (255,255,128,255) */
    printf("GL_MULTITEX_CENTER: %u %u %u %u\n", center[0], center[1], center[2], center[3]);

    /* R from uTexA (255), G from uTexB (255), B from uTint.z (128). A swap/collision breaks one of these. */
    int ok = near(center[0], 255) && near(center[1], 255) && near(center[2], 128) && center[3] == 255;
    if (ok) {
        printf("GL_MULTITEX_OK\n");
        free(px);
        return 0;
    }
    printf("GL_MULTITEX_WRONG r=%u g=%u b=%u a=%u (want 255 255 128 255)\n",
           center[0], center[1], center[2], center[3]);
    free(px);
    return 3;
}
