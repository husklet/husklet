/* EXACT-PRESENT CAPSTONE GUEST — a real EGL + GLES2 program that renders a KNOWN, deterministic quadrant
 * pattern to a real WINDOW surface and PRESENTS it through the ENTIRE hl_wip stack, so a test can assert the
 * composited output pixels EXACTLY (not a percentage).
 *
 * No hl-specific calls and no vendor headers — the GLES2/EGL ABI is self-declared exactly as a `-lEGL
 * -lGLESv2` app sees it, and `LD_LIBRARY_PATH=~/.hl/gl/<arch>` makes the `libEGL.so.1` / `libGLESv2.so.2`
 * it resolves OURS. Every GL call lowers to hl-GPU IR shipped over `$HL_GPU_EXEC` to the host WgpuExecutor
 * on lavapipe (software Vulkan); the GLSL-ES vertex+fragment source is forwarded verbatim and naga compiles
 * it on the host, so the quads genuinely rasterize.
 *
 * WHY A WINDOW SURFACE (not a pbuffer): `eglCreateWindowSurface` is passed a STOCK two-int native window
 * `{width, height}` (NOT one of our `wl_egl_window` structs), so the shim reports `wl_surface == 0`. With
 * `$WAYLAND_DISPLAY` set, that makes our libEGL stand up its OWN `wl_shm` xdg_toplevel and, at
 * `eglSwapBuffers`, read the rendered frame back off lavapipe, flip it to top-left `WL_SHM_FORMAT_XRGB8888`,
 * and commit it onto that toplevel. The compositor composes it and the test's PngPresenter captures the
 * exact pixels. No xdg_shell glue is needed in this guest — the shim's self-owned wayland client provides it.
 *
 * THE KNOWN PATTERN (deterministic; no animation, no time dependence): the NDC square is tiled by four
 * solid-color quads. Accounting for the readback's GL-bottom-left -> wayland-top-left vertical flip, the
 * COMPOSITED frame reads:
 *     top-left = RED (255,0,0)      top-right = GREEN (0,255,0)
 *     bottom-left = BLUE (0,0,255)  bottom-right = YELLOW (255,255,0)
 * Each quad is a single flat color (all four corners identical), so rasterization yields exact solid colors
 * with no interpolation gradient. Prints "GL_PRESENT_PATTERN_OK <frames>" once it has presented. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t EGLint;
typedef unsigned int EGLBoolean, EGLenum, GLenum, GLbitfield, GLuint;
typedef int GLint, GLsizei;
typedef float GLfloat;
typedef char GLchar;
typedef void *EGLDisplay, *EGLConfig, *EGLContext, *EGLSurface, *EGLNativeDisplayType, *EGLNativeWindowType;

extern EGLDisplay eglGetDisplay(EGLNativeDisplayType);
extern EGLBoolean eglInitialize(EGLDisplay, EGLint *, EGLint *);
extern EGLBoolean eglChooseConfig(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *);
extern EGLBoolean eglBindAPI(EGLenum);
extern EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint *);
extern EGLSurface eglCreateWindowSurface(EGLDisplay, EGLConfig, EGLNativeWindowType, const EGLint *);
extern EGLBoolean eglMakeCurrent(EGLDisplay, EGLSurface, EGLSurface, EGLContext);
extern EGLBoolean eglSwapBuffers(EGLDisplay, EGLSurface);
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
extern GLenum glGetError(void);

#define EGL_OPENGL_ES_API 0x30A0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_STATIC_DRAW 0x88E4
#define GL_FLOAT 0x1406
#define GL_TRIANGLES 0x0004
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_NO_ERROR 0

#define W 64
#define H 64
#define FRAMES 5

/* aPos (vec2) + aColor (vec3) interleaved, flat per-vertex color -> solid quads (mirrors gl_instanced.c,
 * a known-good shader shape for the shim's GLSL forwarding). */
static const char *VS =
    "attribute vec2 aPos;\n"
    "attribute vec3 aColor;\n"
    "varying vec3 vColor;\n"
    "void main() { vColor = aColor; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char *FS =
    "precision mediump float;\n"
    "varying vec3 vColor;\n"
    "void main() { gl_FragColor = vec4(vColor, 1.0); }\n";

/* Emit one axis-aligned quad (two triangles) with a single flat color into `buf` at `*n` floats. */
static void emit_quad(float *buf, int *n, float x0, float y0, float x1, float y1, float r, float g, float b) {
    const float corners[6][2] = {
        {x0, y0}, {x1, y0}, {x1, y1}, /* triangle 1 */
        {x0, y0}, {x1, y1}, {x0, y1}, /* triangle 2 */
    };
    for (int i = 0; i < 6; i++) {
        buf[(*n)++] = corners[i][0];
        buf[(*n)++] = corners[i][1];
        buf[(*n)++] = r;
        buf[(*n)++] = g;
        buf[(*n)++] = b;
    }
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

    /* A STOCK two-int native window {W, H} (not a wl_egl_window) -> shim reports wl_surface==0 -> self-owned
     * xdg_toplevel present path. Extra padding words keep any wider struct read in-bounds. */
    int win[4] = {W, H, 0, 0};
    EGLSurface surf = eglCreateWindowSurface(dpy, cfg, (EGLNativeWindowType)win, 0);
    if (!surf) { fprintf(stderr, "eglCreateWindowSurface failed 0x%x\n", eglGetError()); return 1; }
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

    /* Tile the NDC square (GL bottom-left origin, y up). After the readback Y-flip these land as:
     *   GL top-left  -> composited TOP-LEFT  = RED
     *   GL top-right -> composited TOP-RIGHT = GREEN
     *   GL bot-left  -> composited BOT-LEFT  = BLUE
     *   GL bot-right -> composited BOT-RIGHT = YELLOW */
    float verts[4 * 6 * 5];
    int n = 0;
    emit_quad(verts, &n, -1.f, 0.f, 0.f, 1.f, 1.f, 0.f, 0.f); /* GL top-left  -> RED    */
    emit_quad(verts, &n, 0.f, 0.f, 1.f, 1.f, 0.f, 1.f, 0.f);  /* GL top-right -> GREEN  */
    emit_quad(verts, &n, -1.f, -1.f, 0.f, 0.f, 0.f, 0.f, 1.f); /* GL bot-left  -> BLUE   */
    emit_quad(verts, &n, 0.f, -1.f, 1.f, 0.f, 1.f, 1.f, 0.f);  /* GL bot-right -> YELLOW */

    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    if (aPos < 0) aPos = 0;
    glVertexAttribPointer((GLuint)aPos, 2, GL_FLOAT, 0, 20, (const void *)0);
    glEnableVertexAttribArray((GLuint)aPos);
    GLint aColor = glGetAttribLocation(prog, "aColor");
    if (aColor < 0) aColor = 1;
    glVertexAttribPointer((GLuint)aColor, 3, GL_FLOAT, 0, 20, (const void *)8);
    glEnableVertexAttribArray((GLuint)aColor);

    /* The pattern is static; re-issue clear+draw+swap each frame (the draw-list resets per swap) so the
     * compositor has several identical presents to capture. */
    int presented = 0;
    for (int f = 0; f < FRAMES; f++) {
        glClearColor(0.f, 0.f, 0.f, 1.f);
        glClear(GL_COLOR_BUFFER_BIT);
        glDrawArrays(GL_TRIANGLES, 0, 24);
        GLenum e = glGetError();
        if (e != GL_NO_ERROR) { printf("GL_PRESENT_PATTERN_DRAW_FAILED err=0x%x\n", e); return 2; }
        if (!eglSwapBuffers(dpy, surf)) {
            printf("GL_PRESENT_PATTERN_SWAP_FAILED err=0x%x frame=%d\n", eglGetError(), f);
            return 3;
        }
        presented++;
    }

    printf("GL_PRESENT_PATTERN_OK %d\n", presented);
    return 0;
}
