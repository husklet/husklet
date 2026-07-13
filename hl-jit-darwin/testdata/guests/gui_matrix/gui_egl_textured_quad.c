#include "gui_egl_probe.h"

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

static int compile_shader(GLenum type, const char *source, const char *name, GLuint *out) {
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
        printf("%s glCompileShader=0\n", name);
        return -1;
    }
    *out = shader;
    return 0;
}

static int make_program(const char *name, GLuint *out) {
    GLuint vs = 0;
    GLuint fs = 0;
    if (compile_shader(GL_VERTEX_SHADER, VS, name, &vs) != 0) return -1;
    if (compile_shader(GL_FRAGMENT_SHADER, FS, name, &fs) != 0) return -1;

    GLuint program = glCreateProgram();
    if (!program) {
        printf("%s glCreateProgram=0\n", name);
        return -1;
    }
    glAttachShader(program, vs);
    glAttachShader(program, fs);
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

static void fill_checker_texture(uint8_t tex[8 * 8 * 4]) {
    for (int y = 0; y < 8; y++) {
        for (int x = 0; x < 8; x++) {
            int i = (y * 8 + x) * 4;
            int on = ((x >> 1) ^ (y >> 1)) & 1;
            tex[i + 0] = on ? 235 : 30;
            tex[i + 1] = on ? 80 : 180;
            tex[i + 2] = on ? 35 : 240;
            tex[i + 3] = 255;
        }
    }
}

int main(void) {
    const char *name = "gui_egl_textured_quad";
    struct gp_conn c;
    struct gp_events ev;
    int r = ge_xdg_connect(&c, &ev, name);
    if (r != 0) {
        printf("%s xdg_configure=0 step=%d\n", name, r);
        return r;
    }

    struct ge_egl egl;
    r = ge_egl_init(&egl, name);
    if (r != 0) return r;

    void *window = wl_egl_window_create((void *)(uintptr_t)GP_SURFACE, 192, 128);
    if (!window) {
        printf("%s wl_egl_window_create=0\n", name);
        ge_egl_fini(&egl);
        return 7;
    }
    EGLSurface surface = ge_create_surface(&egl, window, name);
    if (!surface) {
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 8;
    }

    GLuint program = 0;
    if (make_program(name, &program) != 0) {
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 9;
    }
    glUseProgram(program);

    uint8_t tex[8 * 8 * 4];
    fill_checker_texture(tex);
    GLuint texture = 0;
    glGenTextures(1, &texture);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, texture);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 8, 8, 0, GL_RGBA, GL_UNSIGNED_BYTE, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);

    float vertices[] = {
        -0.85f,  0.85f, 0.0f, 0.0f,
        -0.85f, -0.85f, 0.0f, 1.0f,
         0.85f,  0.85f, 1.0f, 0.0f,
         0.85f, -0.85f, 1.0f, 1.0f,
    };
    uint16_t indices[] = {0, 1, 2, 2, 1, 3};
    GLuint vbo = 0;
    GLuint ebo = 0;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
    glGenBuffers(1, &ebo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, sizeof(indices), indices, GL_STATIC_DRAW);

    GLint a_pos = glGetAttribLocation(program, "aPos");
    GLint a_uv = glGetAttribLocation(program, "aUV");
    if (a_pos < 0 || a_uv < 0) {
        printf("%s attribs pos=%d uv=%d\n", name, a_pos, a_uv);
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 10;
    }
    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, 16, (void *)0);
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_uv, 2, GL_FLOAT, GL_FALSE, 16, (void *)8);
    glEnableVertexAttribArray((GLuint)a_uv);

    int swaps = 0;
    for (int i = 0; i < 3; i++) {
        glViewport(0, 0, 192, 128);
        glClearColor(0.03f, 0.05f + 0.05f * i, 0.08f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);
        if (!eglSwapBuffers(egl.display, surface)) {
            printf("%s swap_%d=0 err=0x%x\n", name, i, eglGetError());
            wl_egl_window_destroy(window);
            ge_egl_fini(&egl);
            return 11;
        }
        swaps++;
    }

    wl_egl_window_destroy(window);
    ge_egl_fini(&egl);
    printf("%s configure=%u egl=%d.%d texture=8x8 vbo=%u ebo=%u swaps=%d\n",
           name, ev.xdg_configure_serial, egl.major, egl.minor, vbo != 0, ebo != 0, swaps);
    return 0;
}
