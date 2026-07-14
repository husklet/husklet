// A real GLES2 app (no hl-specific calls): compiles a vertex+fragment shader, uploads an interleaved VBO,
// and draws an animated colored triangle, swapping buffers each frame. Linked against -lEGL -lGLESv2 — it
// runs unmodified on the dd GL shim (which forwards its GL calls as hl-gpu IR to the host Metal executor).
// Prototypes are declared here (standard GLES2/EGL ABI) so it builds without the vendor headers.
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

typedef int32_t EGLint;
typedef unsigned int EGLBoolean, EGLenum, GLenum, GLbitfield, GLuint;
typedef int GLint, GLsizei;
typedef float GLfloat;
typedef char GLchar;
typedef void *EGLDisplay, *EGLConfig, *EGLContext, *EGLSurface, *EGLNativeWindowType, *EGLNativeDisplayType;
extern EGLDisplay eglGetDisplay(EGLNativeDisplayType);
extern EGLBoolean eglInitialize(EGLDisplay, EGLint *, EGLint *);
extern EGLBoolean eglChooseConfig(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *);
extern EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint *);
extern EGLSurface eglCreateWindowSurface(EGLDisplay, EGLConfig, EGLNativeWindowType, const EGLint *);
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
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glDrawArrays(GLenum, GLint, GLsizei);

#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_STATIC_DRAW 0x88E4
#define GL_DYNAMIC_DRAW 0x88E8
#define GL_FLOAT 0x1406
#define GL_TRIANGLES 0x0004
#define GL_COLOR_BUFFER_BIT 0x4000

static const char *VS =
    "attribute vec2 aPos;\n"
    "attribute vec4 aColor;\n"
    "varying vec4 vColor;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); vColor = aColor; }\n";
static const char *FS =
    "precision mediump float;\n"
    "varying vec4 vColor;\n"
    "void main() { gl_FragColor = vColor; }\n";

int main(void) {
    // wl_egl_window-ish native window: {width, height, ...}
    uint32_t win[2] = {256, 256};
    EGLDisplay dpy = eglGetDisplay(0);
    eglInitialize(dpy, 0, 0);
    EGLConfig cfg;
    EGLint num;
    eglChooseConfig(dpy, 0, &cfg, 1, &num);
    EGLContext ctx = eglCreateContext(dpy, cfg, 0, 0);
    EGLSurface surf = eglCreateWindowSurface(dpy, cfg, win, 0);
    eglMakeCurrent(dpy, surf, surf, ctx);

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

    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    GLint aColor = glGetAttribLocation(prog, "aColor");

    int frames = 3;
    const char *fenv = getenv("FRAMES");
    if (fenv) frames = atoi(fenv);
    for (int f = 0; f < frames; f++) {
        float ang = f * 0.5f; // animate: rotate the triangle each frame
        float base[3][2] = {{0.0f, 0.7f}, {-0.7f, -0.6f}, {0.7f, -0.6f}};
        float col[3][4] = {{1, .2f, .2f, 1}, {.2f, 1, .2f, 1}, {.3f, .4f, 1, 1}};
        float v[3][6];
        for (int i = 0; i < 3; i++) {
            float x = base[i][0], y = base[i][1];
            v[i][0] = x * cosf(ang) - y * sinf(ang);
            v[i][1] = x * sinf(ang) + y * cosf(ang);
            v[i][2] = col[i][0]; v[i][3] = col[i][1]; v[i][4] = col[i][2]; v[i][5] = col[i][3];
        }
        glBufferData(GL_ARRAY_BUFFER, sizeof v, v, GL_DYNAMIC_DRAW);
        glVertexAttribPointer(aPos, 2, GL_FLOAT, 0, 24, (void *)0);
        glEnableVertexAttribArray(aPos);
        glVertexAttribPointer(aColor, 4, GL_FLOAT, 0, 24, (void *)8);
        glEnableVertexAttribArray(aColor);
        glClearColor(0.08f, 0.08f, 0.13f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glDrawArrays(GL_TRIANGLES, 0, 3);
        eglSwapBuffers(dpy, surf);
        printf("es2tri: frame %d drawn (angle %.2f)\n", f, ang);
        usleep(150000);
    }
    printf("es2tri: done\n");
    return 0;
}
