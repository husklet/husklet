// A real GLES2 app exercising UNIFORMS + a vec3 attribute + a matrix: it uploads a triangle VBO ONCE and
// animates purely by updating a `uniform mat4 uMVP` each frame (glUniformMatrix4fv) — so a visible
// rotation across frames proves the uniform+matrix path (not VBO re-upload). Runs unmodified on the dd
// GL shim, which translates the uniform block + `uMVP * vec4(aPos,1.0)` GLSL to MSL and forwards it.
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
extern GLint glGetUniformLocation(GLuint, const GLchar *);
extern void glUniformMatrix4fv(GLint, GLsizei, GLboolean, const GLfloat *);
extern void glGenBuffers(GLsizei, GLuint *);
extern void glBindBuffer(GLenum, GLuint);
extern void glBufferData(GLenum, long, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, GLboolean, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
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

static const char *VS =
    "uniform mat4 uMVP;\n"
    "attribute vec3 aPos;\n"
    "attribute vec4 aColor;\n"
    "varying vec4 vColor;\n"
    "void main() { gl_Position = uMVP * vec4(aPos, 1.0); vColor = aColor; }\n";
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

    // interleaved vec3 pos + vec4 color, stride 28; uploaded ONCE.
    float v[3][7] = {
        {0.0f, 0.7f, 0.0f, 1, 0.2f, 0.2f, 1},
        {-0.7f, -0.6f, 0.0f, 0.2f, 1, 0.2f, 1},
        {0.7f, -0.6f, 0.0f, 0.2f, 0.4f, 1, 1},
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
    GLint uMVP = glGetUniformLocation(prog, "uMVP");
    printf("es2uniform: aPos=%d aColor=%d uMVP=%d\n", aPos, aColor, uMVP);

    int frames = 3;
    const char *fenv = getenv("FRAMES");
    if (fenv) frames = atoi(fenv);
    for (int f = 0; f < frames; f++) {
        float a = f * 0.6f;
        float c = cosf(a), s = sinf(a);
        // column-major rotation about Z (GL convention)
        float mvp[16] = {c, s, 0, 0, -s, c, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};
        glUniformMatrix4fv(uMVP, 1, GL_FALSE, mvp);
        glClearColor(0.08f, 0.08f, 0.13f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glDrawArrays(GL_TRIANGLES, 0, 3);
        eglSwapBuffers(dpy, surf);
        printf("es2uniform: frame %d (angle %.2f)\n", f, a);
        usleep(120000);
    }
    printf("es2uniform: done\n");
    return 0;
}
