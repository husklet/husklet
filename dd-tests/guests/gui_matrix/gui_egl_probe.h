#ifndef DD_GUI_EGL_PROBE_H
#define DD_GUI_EGL_PROBE_H

#include "gui_probe_wayland.h"

typedef int32_t EGLint;
typedef unsigned int EGLBoolean;
typedef unsigned int EGLenum;
typedef unsigned int GLenum;
typedef unsigned int GLbitfield;
typedef unsigned int GLuint;
typedef unsigned char GLboolean;
typedef int GLint;
typedef int GLsizei;
typedef float GLfloat;
typedef char GLchar;
typedef intptr_t GLsizeiptr;
typedef void *EGLDisplay;
typedef void *EGLConfig;
typedef void *EGLContext;
typedef void *EGLSurface;
typedef void *EGLNativeDisplayType;
typedef void *EGLNativeWindowType;

#define EGL_FALSE 0
#define EGL_TRUE 1
#define EGL_NONE 0x3038
#define EGL_NO_CONTEXT ((EGLContext)0)
#define EGL_NO_DISPLAY ((EGLDisplay)0)
#define EGL_NO_SURFACE ((EGLSurface)0)
#define EGL_OPENGL_ES_API 0x30A0
#define EGL_CONTEXT_CLIENT_VERSION 0x3098
#define EGL_SURFACE_TYPE 0x3033
#define EGL_WINDOW_BIT 0x0004
#define EGL_RENDERABLE_TYPE 0x3040
#define EGL_OPENGL_ES2_BIT 0x0004
#define EGL_OPENGL_ES3_BIT_KHR 0x0040

#define GL_FALSE 0
#define GL_TRUE 1
#define GL_ONE 1
#define GL_ZERO 0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_COMPILE_STATUS 0x8B81
#define GL_LINK_STATUS 0x8B82
#define GL_ARRAY_BUFFER 0x8892
#define GL_ELEMENT_ARRAY_BUFFER 0x8893
#define GL_STATIC_DRAW 0x88E4
#define GL_FLOAT 0x1406
#define GL_UNSIGNED_BYTE 0x1401
#define GL_UNSIGNED_SHORT 0x1403
#define GL_TRIANGLES 0x0004
#define GL_BLEND 0x0BE2
#define GL_SCISSOR_TEST 0x0C11
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_TEXTURE_2D 0x0DE1
#define GL_TEXTURE0 0x84C0
#define GL_RGBA 0x1908
#define GL_BGRA_EXT 0x80E1
#define GL_RED 0x1903
#define GL_R8 0x8229
#define GL_LUMINANCE 0x1909
#define GL_UNPACK_ALIGNMENT 0x0CF5
#define GL_UNPACK_ROW_LENGTH 0x0CF2
#define GL_UNPACK_SKIP_ROWS 0x0CF3
#define GL_UNPACK_SKIP_PIXELS 0x0CF4
#define GL_ONE_MINUS_SRC_ALPHA 0x0303
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_TEXTURE_WRAP_S 0x2802
#define GL_TEXTURE_WRAP_T 0x2803
#define GL_NEAREST 0x2600
#define GL_LINEAR 0x2601
#define GL_CLAMP_TO_EDGE 0x812F
#define GL_FRAMEBUFFER 0x8D40
#define GL_READ_FRAMEBUFFER 0x8CA8
#define GL_DRAW_FRAMEBUFFER 0x8CA9
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5

extern void *wl_egl_window_create(void *surface, int width, int height);
extern void wl_egl_window_resize(void *window, int width, int height, int dx, int dy);
extern void wl_egl_window_get_attached_size(void *window, int *width, int *height);
extern void wl_egl_window_destroy(void *window);

extern EGLDisplay eglGetDisplay(EGLNativeDisplayType);
extern EGLBoolean eglInitialize(EGLDisplay, EGLint *, EGLint *);
extern EGLBoolean eglBindAPI(EGLenum);
extern EGLBoolean eglChooseConfig(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *);
extern EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint *);
extern EGLSurface eglCreateWindowSurface(EGLDisplay, EGLConfig, EGLNativeWindowType, const EGLint *);
extern EGLBoolean eglMakeCurrent(EGLDisplay, EGLSurface, EGLSurface, EGLContext);
extern EGLBoolean eglSwapBuffers(EGLDisplay, EGLSurface);
extern EGLBoolean eglDestroySurface(EGLDisplay, EGLSurface);
extern EGLBoolean eglDestroyContext(EGLDisplay, EGLContext);
extern EGLBoolean eglTerminate(EGLDisplay);
extern EGLint eglGetError(void);

extern void glViewport(GLint, GLint, GLsizei, GLsizei);
extern void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
extern void glClear(GLbitfield);
extern void glEnable(GLenum);
extern void glDisable(GLenum);
extern void glScissor(GLint, GLint, GLsizei, GLsizei);
extern void glBlendFunc(GLenum, GLenum);
extern GLuint glCreateShader(GLenum);
extern void glShaderSource(GLuint, GLsizei, const GLchar *const *, const GLint *);
extern void glCompileShader(GLuint);
extern void glGetShaderiv(GLuint, GLenum, GLint *);
extern GLuint glCreateProgram(void);
extern void glAttachShader(GLuint, GLuint);
extern void glLinkProgram(GLuint);
extern void glGetProgramiv(GLuint, GLenum, GLint *);
extern void glUseProgram(GLuint);
extern GLint glGetAttribLocation(GLuint, const GLchar *);
extern GLint glGetUniformLocation(GLuint, const GLchar *);
extern void glUniform1i(GLint, GLint);
extern void glUniform4f(GLint, GLfloat, GLfloat, GLfloat, GLfloat);
extern void glGenBuffers(GLsizei, GLuint *);
extern void glBindBuffer(GLenum, GLuint);
extern void glBufferData(GLenum, GLsizeiptr, const void *, GLenum);
extern void glVertexAttribPointer(GLuint, GLint, GLenum, GLboolean, GLsizei, const void *);
extern void glEnableVertexAttribArray(GLuint);
extern void glGenTextures(GLsizei, GLuint *);
extern void glActiveTexture(GLenum);
extern void glBindTexture(GLenum, GLuint);
extern void glTexImage2D(GLenum, GLint, GLint, GLsizei, GLsizei, GLint, GLenum, GLenum, const void *);
extern void glTexParameteri(GLenum, GLenum, GLint);
extern void glPixelStorei(GLenum, GLint);
extern void glDrawArrays(GLenum, GLint, GLsizei);
extern void glDrawElements(GLenum, GLsizei, GLenum, const void *);
extern void glGenFramebuffers(GLsizei, GLuint *);
extern void glBindFramebuffer(GLenum, GLuint);
extern GLenum glCheckFramebufferStatus(GLenum);
extern void glFramebufferTexture2D(GLenum, GLenum, GLenum, GLuint, GLint);
extern void glDeleteFramebuffers(GLsizei, const GLuint *);
extern void glCopyTexSubImage2D(GLenum, GLint, GLint, GLint, GLint, GLint, GLsizei, GLsizei);
extern void glBlitFramebuffer(GLint, GLint, GLint, GLint, GLint, GLint, GLint, GLint, GLbitfield, GLenum);
extern void glReadPixels(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *);
extern GLenum glGetError(void);
extern void glFinish(void);

struct ge_egl {
    EGLDisplay display;
    EGLConfig config;
    EGLContext context;
    EGLint major;
    EGLint minor;
};

static int ge_xdg_connect(struct gp_conn *c, struct gp_events *ev, const char *title) {
    memset(ev, 0, sizeof(*ev));
    if (gp_connect(c) != 0) return 1;
    if (gp_xdg_setup(c, ev, title, 0) != 1) return 2;
    return 0;
}

static int ge_egl_init_version(struct ge_egl *egl, const char *name, int version) {
    memset(egl, 0, sizeof(*egl));
    egl->display = eglGetDisplay(EGL_NO_DISPLAY);
    if (!egl->display || !eglInitialize(egl->display, &egl->major, &egl->minor)) {
        printf("%s eglInitialize=0 err=0x%x\n", name, eglGetError());
        return 3;
    }
    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("%s eglBindAPI=0 err=0x%x\n", name, eglGetError());
        return 4;
    }
    EGLint renderable = version >= 3 ? EGL_OPENGL_ES3_BIT_KHR : EGL_OPENGL_ES2_BIT;
    EGLint cfg_attr[] = {
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RENDERABLE_TYPE, renderable,
        EGL_NONE,
    };
    EGLint ncfg = 0;
    if (!eglChooseConfig(egl->display, cfg_attr, &egl->config, 1, &ncfg) || ncfg < 1) {
        printf("%s eglChooseConfig=0 err=0x%x\n", name, eglGetError());
        return 5;
    }
    EGLint ctx_attr[] = {EGL_CONTEXT_CLIENT_VERSION, version, EGL_NONE};
    egl->context = eglCreateContext(egl->display, egl->config, EGL_NO_CONTEXT, ctx_attr);
    if (!egl->context) {
        printf("%s eglCreateContext=0 err=0x%x\n", name, eglGetError());
        return 6;
    }
    return 0;
}

static int GP_UNUSED ge_egl_init(struct ge_egl *egl, const char *name) {
    return ge_egl_init_version(egl, name, 2);
}

static EGLSurface ge_create_surface(struct ge_egl *egl, void *window, const char *name) {
    EGLSurface surface = eglCreateWindowSurface(egl->display, egl->config, window, NULL);
    if (!surface) {
        printf("%s eglCreateWindowSurface=0 err=0x%x\n", name, eglGetError());
        return EGL_NO_SURFACE;
    }
    if (!eglMakeCurrent(egl->display, surface, surface, egl->context)) {
        printf("%s eglMakeCurrent=0 err=0x%x\n", name, eglGetError());
        return EGL_NO_SURFACE;
    }
    return surface;
}

static void ge_egl_fini(struct ge_egl *egl) {
    if (egl->display) {
        if (egl->context) eglDestroyContext(egl->display, egl->context);
        eglTerminate(egl->display);
    }
}

#endif
