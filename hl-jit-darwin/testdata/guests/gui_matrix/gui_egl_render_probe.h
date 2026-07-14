#ifndef HL_GUI_EGL_RENDER_PROBE_H
#define HL_GUI_EGL_RENDER_PROBE_H

#include "gui_egl_probe.h"

#if defined(__GNUC__)
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wunused-function"
#pragma GCC diagnostic ignored "-Wunused-variable"
#endif

struct gr_window {
    struct gp_conn conn;
    struct gp_events ev;
    struct ge_egl egl;
    EGLSurface surface;
    void *window;
    int width;
    int height;
    const char *name;
};

static const char *GR_VS =
    "attribute vec2 aPos;\n"
    "attribute vec2 aUV;\n"
    "varying vec2 vUV;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); vUV = aUV; }\n";

static const char *GR_FS_TEX =
    "precision mediump float;\n"
    "uniform sampler2D uTex;\n"
    "varying vec2 vUV;\n"
    "void main() { gl_FragColor = texture2D(uTex, vUV); }\n";

static const char *GR_FS_SOLID =
    "precision mediump float;\n"
    "uniform vec4 uColor;\n"
    "varying vec2 vUV;\n"
    "void main() { gl_FragColor = uColor + vec4(vUV.xy, 0.0, 0.0) * 0.0; }\n";

static int gr_compile_shader(GLenum type, const char *source, const char *name, GLuint *out) {
    GLuint shader = glCreateShader(type);
    if (!shader) {
        printf("%s glCreateShader=0\n", name);
        return -1;
    }
    glShaderSource(shader, 1, &source, NULL);
    glCompileShader(shader);
    GLint ok = 0;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        printf("%s glCompileShader=0 type=0x%x\n", name, type);
        return -1;
    }
    *out = shader;
    return 0;
}

static int gr_make_program(const char *name, const char *fs, GLuint *out) {
    GLuint vs = 0;
    GLuint frag = 0;
    if (gr_compile_shader(GL_VERTEX_SHADER, GR_VS, name, &vs) != 0) return -1;
    if (gr_compile_shader(GL_FRAGMENT_SHADER, fs, name, &frag) != 0) return -1;
    GLuint program = glCreateProgram();
    if (!program) {
        printf("%s glCreateProgram=0\n", name);
        return -1;
    }
    glAttachShader(program, vs);
    glAttachShader(program, frag);
    glLinkProgram(program);
    GLint linked = 0;
    glGetProgramiv(program, GL_LINK_STATUS, &linked);
    if (!linked) {
        printf("%s glLinkProgram=0\n", name);
        return -1;
    }
    *out = program;
    return 0;
}

static int gr_open_window(struct gr_window *gw, const char *name, int width, int height, int es_version) {
    memset(gw, 0, sizeof(*gw));
    gw->name = name;
    gw->width = width;
    gw->height = height;
    int r = ge_xdg_connect(&gw->conn, &gw->ev, name);
    if (r != 0) {
        printf("%s xdg_configure=0 step=%d\n", name, r);
        return r;
    }
    r = ge_egl_init_version(&gw->egl, name, es_version);
    if (r != 0) return r;
    gw->window = wl_egl_window_create((void *)(uintptr_t)GP_SURFACE, width, height);
    if (!gw->window) {
        printf("%s wl_egl_window_create=0\n", name);
        ge_egl_fini(&gw->egl);
        return 7;
    }
    gw->surface = ge_create_surface(&gw->egl, gw->window, name);
    if (!gw->surface) {
        wl_egl_window_destroy(gw->window);
        ge_egl_fini(&gw->egl);
        return 8;
    }
    return 0;
}

static void gr_close_window(struct gr_window *gw) {
    if (gw->egl.display && gw->surface) eglDestroySurface(gw->egl.display, gw->surface);
    if (gw->window) wl_egl_window_destroy(gw->window);
    ge_egl_fini(&gw->egl);
}

static void gr_texture_params(void) {
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
}

static GLuint gr_make_rgba_texture(int width, int height, const void *pixels) {
    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, width, height, 0, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
    gr_texture_params();
    return tex;
}

static int gr_make_fbo(const char *name, int width, int height, GLuint *tex, GLuint *fbo) {
    *tex = gr_make_rgba_texture(width, height, NULL);
    *fbo = 0;
    glGenFramebuffers(1, fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, *fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, *tex, 0);
    GLenum status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (status != GL_FRAMEBUFFER_COMPLETE) {
        printf("%s framebuffer incomplete status=0x%x tex=%u fbo=%u\n", name, status, *tex, *fbo);
        return -1;
    }
    return 0;
}

static int gr_make_quad(GLuint *vbo) {
    float vertices[] = {
        -1.0f,  1.0f, 0.0f, 0.0f,
        -1.0f, -1.0f, 0.0f, 1.0f,
         1.0f,  1.0f, 1.0f, 0.0f,
         1.0f,  1.0f, 1.0f, 0.0f,
        -1.0f, -1.0f, 0.0f, 1.0f,
         1.0f, -1.0f, 1.0f, 1.0f,
    };
    glGenBuffers(1, vbo);
    glBindBuffer(GL_ARRAY_BUFFER, *vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
    return *vbo ? 0 : -1;
}

static int gr_bind_quad(const char *name, GLuint program) {
    GLint a_pos = glGetAttribLocation(program, "aPos");
    GLint a_uv = glGetAttribLocation(program, "aUV");
    if (a_pos < 0 || a_uv < 0) {
        printf("%s attribs pos=%d uv=%d\n", name, a_pos, a_uv);
        return -1;
    }
    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, 16, (void *)0);
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_uv, 2, GL_FLOAT, GL_FALSE, 16, (void *)8);
    glEnableVertexAttribArray((GLuint)a_uv);
    return 0;
}

static void gr_clear_rgba(GLuint fbo, int width, int height, float r, float g, float b, float a) {
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glViewport(0, 0, width, height);
    glClearColor(r, g, b, a);
    glClear(GL_COLOR_BUFFER_BIT);
}

static int gr_abs(int v) {
    return v < 0 ? -v : v;
}

static int gr_expect_pixel(const char *name, const char *label, int x, int y,
                           uint8_t r, uint8_t g, uint8_t b, uint8_t a, int tol) {
    uint8_t px[4] = {0, 0, 0, 0};
    glReadPixels(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
    if (gr_abs((int)px[0] - (int)r) <= tol &&
        gr_abs((int)px[1] - (int)g) <= tol &&
        gr_abs((int)px[2] - (int)b) <= tol &&
        gr_abs((int)px[3] - (int)a) <= tol) {
        return 0;
    }
    printf("%s %s pixel=%u,%u,%u,%u expected=%u,%u,%u,%u tol=%d at=%d,%d\n",
           name, label, px[0], px[1], px[2], px[3], r, g, b, a, tol, x, y);
    return -1;
}

static int gr_swap(struct gr_window *gw) {
    if (!eglSwapBuffers(gw->egl.display, gw->surface)) {
        printf("%s swap=0 err=0x%x\n", gw->name, eglGetError());
        return -1;
    }
    return 0;
}

#if defined(__GNUC__)
#pragma GCC diagnostic pop
#endif

#endif
