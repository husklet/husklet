// A real GLES2 app (no dd-specific calls) exercising the TEXTURE + INDEXED-DRAW path: it uploads a 4x4
// RGBA texture, binds it to a sampler2D, and draws a textured quad via glDrawElements (a 4-vertex VBO +
// a 6-entry index buffer). Linked -lEGL -lGLESv2, it runs UNMODIFIED on the dd GL shim, which forwards
// glTexImage2D/glDrawElements as dd-gpu IR (CreateTexture+CopyBufferToTexture+SetIndexBuffer+DrawIndexed)
// to the host Metal executor. Prototypes are the stock GLES2/EGL ABI, declared locally (no vendor headers).
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
extern GLint glGetUniformLocation(GLuint, const GLchar *);
extern void glUniform1i(GLint, GLint);
extern void glGenBuffers(GLsizei, GLuint *);
extern void glBindBuffer(GLenum, GLuint);
extern void glBufferData(GLenum, long, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, unsigned char, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glGenTextures(GLsizei, GLuint *);
extern void glBindTexture(GLenum, GLuint);
extern void glActiveTexture(GLenum);
extern void glTexImage2D(GLenum, GLint, GLint, GLsizei, GLsizei, GLint, GLenum, GLenum, const void *);
extern void glTexParameteri(GLenum, GLenum, GLint);
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glDrawElements(GLenum, GLsizei, GLenum, const void *);

#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_ARRAY_BUFFER 0x8892
#define GL_ELEMENT_ARRAY_BUFFER 0x8893
#define GL_STATIC_DRAW 0x88E4
#define GL_FLOAT 0x1406
#define GL_UNSIGNED_SHORT 0x1403
#define GL_TRIANGLES 0x0004
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_TEXTURE_2D 0x0DE1
#define GL_TEXTURE0 0x84C0
#define GL_RGBA 0x1908
#define GL_UNSIGNED_BYTE 0x1401
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_NEAREST 0x2600

static const char *VS =
    "attribute vec2 aPos;\n"
    "attribute vec2 aUV;\n"
    "varying vec2 vUV;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); vUV = aUV; }\n";
static const char *FS =
    "precision mediump float;\n"
    "uniform sampler2D uTex;\n"
    "varying vec2 vUV;\n"
    "void main() { gl_FragColor = texture2D(uTex, vUV); }\n";

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

    // A 4x4 RGBA texture: a 2-color checkerboard so the sampling is obvious.
    unsigned char tex[4 * 4 * 4];
    for (int y = 0; y < 4; y++)
        for (int x = 0; x < 4; x++) {
            int i = (y * 4 + x) * 4;
            int on = (x + y) & 1;
            tex[i] = on ? 240 : 30;
            tex[i + 1] = on ? 80 : 30;
            tex[i + 2] = on ? 30 : 200;
            tex[i + 3] = 255;
        }
    GLuint t;
    glGenTextures(1, &t);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 4, 4, 0, GL_RGBA, GL_UNSIGNED_BYTE, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glUniform1i(glGetUniformLocation(prog, "uTex"), 0);

    // quad: 4 corners (pos.xy + uv), 6 indices
    float v[] = {
        -0.9f, 0.9f, 0.0f, 0.0f,
        -0.9f, -0.9f, 0.0f, 1.0f,
        0.9f, 0.9f, 1.0f, 0.0f,
        0.9f, -0.9f, 1.0f, 1.0f,
    };
    unsigned short idx[] = {0, 1, 2, 2, 1, 3};
    GLuint vbo, ebo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof v, v, GL_STATIC_DRAW);
    glGenBuffers(1, &ebo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, sizeof idx, idx, GL_STATIC_DRAW);

    GLint aPos = glGetAttribLocation(prog, "aPos");
    GLint aUV = glGetAttribLocation(prog, "aUV");
    glVertexAttribPointer(aPos, 2, GL_FLOAT, 0, 16, (void *)0);
    glEnableVertexAttribArray(aPos);
    glVertexAttribPointer(aUV, 2, GL_FLOAT, 0, 16, (void *)8);
    glEnableVertexAttribArray(aUV);

    int frames = 2;
    const char *fenv = getenv("FRAMES");
    if (fenv) frames = atoi(fenv);
    for (int f = 0; f < frames; f++) {
        glClearColor(0.1f, 0.1f, 0.12f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);
        eglSwapBuffers(dpy, surf);
        printf("es2tex: frame %d drawn (textured quad via glDrawElements)\n", f);
        usleep(150000);
    }
    printf("es2tex: done\n");
    return 0;
}
