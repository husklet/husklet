#include "gui_egl_render_probe.h"

#include <stddef.h>

extern void glGenVertexArrays(GLsizei, GLuint *);
extern void glBindVertexArray(GLuint);
extern void glDeleteVertexArrays(GLsizei, const GLuint *);

struct color_vertex {
    GLfloat pos[2];
    uint8_t color[4];
};

struct decoy_vertex {
    GLfloat pos[2];
    GLfloat uv[2];
};

static const char *VAO_VS =
    "#version 300 es\n"
    "precision highp float;\n"
    "in vec2 aPos;\n"
    "in vec4 aColor;\n"
    "out vec4 vColor;\n"
    "void main() {\n"
    "  gl_Position = vec4(aPos, 0.0, 1.0);\n"
    "  vColor = aColor;\n"
    "}\n";

static const char *VAO_FS =
    "#version 300 es\n"
    "precision highp float;\n"
    "in vec4 vColor;\n"
    "out vec4 fragColor;\n"
    "void main() { fragColor = vColor; }\n";

static int make_program(const char *name, GLuint *out) {
    GLuint vs = 0;
    GLuint fs = 0;
    if (gr_compile_shader(GL_VERTEX_SHADER, VAO_VS, name, &vs) != 0) return -1;
    if (gr_compile_shader(GL_FRAGMENT_SHADER, VAO_FS, name, &fs) != 0) return -1;
    GLuint program = glCreateProgram();
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

int main(void) {
    const char *name = "gui_egl_vao_state";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 128, 96, 3);
    if (r != 0) return r;

    GLuint program = 0;
    if (make_program(name, &program) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    glUseProgram(program);
    GLint a_pos = glGetAttribLocation(program, "aPos");
    GLint a_color = glGetAttribLocation(program, "aColor");
    if (a_pos < 0 || a_color < 0) {
        printf("%s attribs pos=%d color=%d\n", name, a_pos, a_color);
        gr_close_window(&gw);
        return 10;
    }

    GLuint tex = 0;
    GLuint fbo = 0;
    if (gr_make_fbo(name, 64, 64, &tex, &fbo) != 0) {
        gr_close_window(&gw);
        return 11;
    }

    const struct color_vertex good[] = {
        {{-1.0f, -1.0f}, {24, 180, 90, 255}},
        {{ 3.0f, -1.0f}, {24, 180, 90, 255}},
        {{-1.0f,  3.0f}, {24, 180, 90, 255}},
    };
    const struct decoy_vertex decoy[] = {
        {{ 1.4f,  1.4f}, {0.0f, 0.0f}},
        {{ 1.8f,  1.4f}, {1.0f, 0.0f}},
        {{ 1.4f,  1.8f}, {0.0f, 1.0f}},
    };

    GLuint good_vbo = 0;
    GLuint decoy_vbo = 0;
    glGenBuffers(1, &good_vbo);
    glBindBuffer(GL_ARRAY_BUFFER, good_vbo);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(good), good, GL_STATIC_DRAW);
    glGenBuffers(1, &decoy_vbo);
    glBindBuffer(GL_ARRAY_BUFFER, decoy_vbo);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(decoy), decoy, GL_STATIC_DRAW);

    GLuint vaos[2] = {0, 0};
    glGenVertexArrays(2, vaos);

    glBindVertexArray(vaos[0]);
    glBindBuffer(GL_ARRAY_BUFFER, good_vbo);
    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, sizeof(struct color_vertex),
                          (void *)offsetof(struct color_vertex, pos));
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_color, 4, GL_UNSIGNED_BYTE, GL_TRUE, sizeof(struct color_vertex),
                          (void *)offsetof(struct color_vertex, color));
    glEnableVertexAttribArray((GLuint)a_color);

    glBindVertexArray(vaos[1]);
    glBindBuffer(GL_ARRAY_BUFFER, decoy_vbo);
    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, sizeof(struct decoy_vertex),
                          (void *)offsetof(struct decoy_vertex, pos));
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_color, 2, GL_FLOAT, GL_FALSE, sizeof(struct decoy_vertex),
                          (void *)offsetof(struct decoy_vertex, uv));
    glEnableVertexAttribArray((GLuint)a_color);

    glBindVertexArray(vaos[0]);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glViewport(0, 0, 64, 64);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    glFinish();

    int ok = gr_expect_pixel(name, "vao_a_restored_after_decoy_vao", 32, 32, 24, 180, 90, 255, 4);

    glBindVertexArray(0);
    glDeleteVertexArrays(2, vaos);
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 12;
    printf("%s configure=%u egl=%d.%d vao_restore=1 good_vbo=%u decoy_vbo=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, good_vbo != 0, decoy_vbo != 0);
    return 0;
}
