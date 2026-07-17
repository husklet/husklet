// A real GLES2 app exercising the DEPTH BUFFER: two overlapping triangles at different z, the NEAR (red)
// one specified BEFORE the FAR (green) one in a single draw. With glEnable(GL_DEPTH_TEST) the near red
// wins the overlap; without depth the later-drawn far green would paint over it. So red-in-the-overlap
// proves depth testing works. Runs unmodified on the hl GL shim (z in [0,1] = Metal NDC depth range).
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

typedef int32_t EGLint;
typedef unsigned int EGLBoolean, GLenum, GLbitfield, GLuint;
typedef int GLint, GLsizei;
typedef float GLfloat;
typedef char GLchar;
typedef unsigned char GLboolean;
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
extern void glVertexAttribPointer(GLuint, GLint, GLenum, GLboolean, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glEnable(GLenum);
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glDrawArrays(GLenum, GLint, GLsizei);

#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_STATIC_DRAW 0x88E4
#define GL_FLOAT 0x1406
#define GL_FALSE 0
#define GL_TRIANGLES 0x0004
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_DEPTH_BUFFER_BIT 0x0100
#define GL_DEPTH_TEST 0x0B71

static const char *VS =
    "attribute vec3 aPos;\n"
    "attribute vec4 aColor;\n"
    "varying vec4 vColor;\n"
    "void main() { gl_Position = vec4(aPos, 1.0); vColor = aColor; }\n";
static const char *FS =
    "precision mediump float;\n"
    "varying vec4 vColor;\n"
    "void main() { gl_FragColor = vColor; }\n";

int main(void) {
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

    // NEAR triangle (red, z=0.2) FIRST, then FAR triangle (green, z=0.7). They overlap in the center.
    float v[6][7] = {
        {-0.7f, 0.6f, 0.2f, 1, 0.2f, 0.2f, 1},  {-0.7f, -0.6f, 0.2f, 1, 0.2f, 0.2f, 1}, {0.5f, 0.0f, 0.2f, 1, 0.2f, 0.2f, 1},
        {0.7f, 0.6f, 0.7f, 0.2f, 1, 0.2f, 1},   {0.7f, -0.6f, 0.7f, 0.2f, 1, 0.2f, 1},  {-0.5f, 0.0f, 0.7f, 0.2f, 1, 0.2f, 1},
    };
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof v, v, GL_STATIC_DRAW);
    GLint aPos = glGetAttribLocation(prog, "aPos");
    GLint aColor = glGetAttribLocation(prog, "aColor");
    glVertexAttribPointer(aPos, 3, GL_FLOAT, GL_FALSE, 28, (void *)0);
    glEnableVertexAttribArray(aPos);
    glVertexAttribPointer(aColor, 4, GL_FLOAT, GL_FALSE, 28, (void *)12);
    glEnableVertexAttribArray(aColor);
    glEnable(GL_DEPTH_TEST);

    glClearColor(0.08f, 0.08f, 0.13f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    eglSwapBuffers(dpy, surf);
    printf("es2depth: drew near(red,z=.2)+far(green,z=.7) with depth test; near should win the overlap\n");
    usleep(120000);
    return 0;
}
