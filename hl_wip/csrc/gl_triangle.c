/* REAL SOFTWARE — a real EGL + GLES2 offscreen program driven through OUR libEGL/libGLESv2.
 *
 * No hl-specific calls and no vendor headers: the GLES2/EGL ABI is self-declared (exactly as a real app
 * linked against -lEGL -lGLESv2 sees it), and `LD_LIBRARY_PATH=~/.hl/gl/aarch64` makes the `libEGL.so.1` /
 * `libGLESv2.so.2` it resolves OURS. Each GL call is lowered by our shim into hl-GPU IR and shipped over
 * `$HL_GPU_EXEC` to the host WgpuExecutor on lavapipe (software Vulkan). `glReadPixels` triggers a REAL
 * device→host readback (render → CopyTextureToBuffer → read_buffer over the socket — the same port cuda's
 * DtoH uses), so this program observes what lavapipe actually produced.
 *
 * PHASE 1 (asserted): surfaceless EGL bring-up, glClearColor + glClear, glReadPixels → assert the CLEAR
 * color came back (a real clear rasterized on lavapipe + read back over the socket). Prints
 * "GL_CLEAR_READBACK_OK r g b a".
 *
 * PHASE 2 (best-effort, reproduces a known gap): compile a GLES2 vertex+fragment shader, upload a VBO, draw
 * a triangle, glReadPixels. Our GL shim translates GLSL→MSL and tags the shader payload `LegacyMsl`, which
 * the wgpu/naga host executor REJECTS (it only consumes SPIR-V/GLSL/WGSL) — so the geometry frame is
 * Nacked and the readback fails. The program reports the outcome ("GL_TRIANGLE_READBACK_FAILED" with the
 * GL error, or the sampled center pixel) so the Rust test can surface the gap precisely without crashing. */
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
extern EGLint eglGetError(void);

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
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char *FS =
    "precision mediump float;\n"
    "void main() { gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0); }\n";

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

    /* ---- PHASE 1: clear + readback (the real device→host readback on lavapipe) ------------------ */
    unsigned char *px = malloc(W * H * 4);
    glClearColor(0.25f, 0.5f, 0.75f, 1.0f); /* → ~(64,128,191,255) */
    glClear(GL_COLOR_BUFFER_BIT);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e1 = glGetError();
    if (e1 != GL_NO_ERROR) { fprintf(stderr, "clear glReadPixels error 0x%x\n", e1); return 2; }
    /* center pixel */
    unsigned char *c = &px[((H / 2) * W + (W / 2)) * 4];
    printf("GL_CLEAR_CENTER: %u %u %u %u\n", c[0], c[1], c[2], c[3]);
    if (!(near(c[0], 64) && near(c[1], 128) && near(c[2], 191) && near(c[3], 255))) {
        fprintf(stderr, "clear color mismatch\n");
        return 2;
    }
    printf("GL_CLEAR_READBACK_OK %u %u %u %u\n", c[0], c[1], c[2], c[3]);

    /* ---- PHASE 2: real triangle (best-effort; reproduces the LegacyMsl-shader gap) -------------- */
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

    /* a centered triangle: covers the middle, leaves the corners as the clear color */
    float verts[6] = {0.0f, 0.8f, -0.8f, -0.8f, 0.8f, -0.8f};
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 0, 0);
    glEnableVertexAttribArray((GLuint)aPos);

    glDrawArrays(GL_TRIANGLES, 0, 3);
    memset(px, 0xAB, W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    GLenum e2 = glGetError();
    if (e2 != GL_NO_ERROR) {
        printf("GL_TRIANGLE_READBACK_FAILED err=0x%x\n", e2);
    } else {
        unsigned char *tc = &px[((H / 2) * W + (W / 2)) * 4];
        printf("GL_TRIANGLE_CENTER: %u %u %u %u\n", tc[0], tc[1], tc[2], tc[3]);
        if (tc[0] == 0 && tc[1] == 255 && tc[2] == 0)
            printf("GL_TRIANGLE_DRAW_OK\n");
        else
            printf("GL_TRIANGLE_WRONG_COLOR\n");
    }

    free(px);
    return 0;
}
