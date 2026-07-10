#include "gui_egl_render_probe.h"

#include <stddef.h>

extern void glGenVertexArrays(GLsizei, GLuint *);
extern void glBindVertexArray(GLuint);
extern void glDeleteVertexArrays(GLsizei, const GLuint *);
extern void glDisableVertexAttribArray(GLuint);

struct churn_vertex {
    GLfloat pos[2];
    uint8_t color[4];
};

static const char *CHURN_VS =
    "#version 300 es\n"
    "precision highp float;\n"
    "in vec2 aPos;\n"
    "in vec4 aColor;\n"
    "out vec4 vColor;\n"
    "void main() {\n"
    "  gl_Position = vec4(aPos, 0.0, 1.0);\n"
    "  vColor = aColor;\n"
    "}\n";

static const char *CHURN_FS =
    "#version 300 es\n"
    "precision highp float;\n"
    "in vec4 vColor;\n"
    "out vec4 fragColor;\n"
    "void main() { fragColor = vColor; }\n";

static int make_churn_program(const char *name, GLuint *out) {
    GLuint vs = 0;
    GLuint fs = 0;
    if (gr_compile_shader(GL_VERTEX_SHADER, CHURN_VS, name, &vs) != 0) return -1;
    if (gr_compile_shader(GL_FRAGMENT_SHADER, CHURN_FS, name, &fs) != 0) return -1;
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
    const char *name = "gui_egl_state_churn_vao_elements";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 128, 96, 3);
    if (r != 0) return r;

    GLuint program = 0;
    if (make_churn_program(name, &program) != 0) {
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
    if (gr_make_fbo(name, 96, 64, &tex, &fbo) != 0) {
        gr_close_window(&gw);
        return 11;
    }

    const struct churn_vertex good[] = {
        {{-0.8f, -0.7f}, {24, 180, 90, 255}},
        {{ 0.8f, -0.7f}, {24, 180, 90, 255}},
        {{ 0.8f,  0.7f}, {24, 180, 90, 255}},
        {{-0.8f,  0.7f}, {24, 180, 90, 255}},
    };
    const struct churn_vertex decoy[] = {
        {{-0.9f, -0.9f}, {210, 40, 35, 255}},
        {{-0.2f, -0.9f}, {210, 40, 35, 255}},
        {{-0.2f, -0.2f}, {210, 40, 35, 255}},
        {{-0.9f, -0.2f}, {210, 40, 35, 255}},
    };
    const uint16_t good_indices[] = {
        0, 0, 0,
        0, 1, 2, 0, 2, 3,
    };
    const uint16_t decoy_indices[] = {
        0, 1, 2, 0, 2, 3,
    };

    GLuint good_vbo = 0;
    GLuint decoy_vbo = 0;
    GLuint good_ebo = 0;
    GLuint decoy_ebo = 0;
    glGenBuffers(1, &good_vbo);
    glBindBuffer(GL_ARRAY_BUFFER, good_vbo);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(good), good, GL_STATIC_DRAW);
    glGenBuffers(1, &decoy_vbo);
    glBindBuffer(GL_ARRAY_BUFFER, decoy_vbo);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(decoy), decoy, GL_STATIC_DRAW);
    glGenBuffers(1, &good_ebo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, good_ebo);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, (GLsizeiptr)sizeof(good_indices), good_indices, GL_STATIC_DRAW);
    glGenBuffers(1, &decoy_ebo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, decoy_ebo);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, (GLsizeiptr)sizeof(decoy_indices), decoy_indices, GL_STATIC_DRAW);

    GLuint vaos[2] = {0, 0};
    glGenVertexArrays(2, vaos);

    glBindVertexArray(vaos[0]);
    glBindBuffer(GL_ARRAY_BUFFER, good_vbo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, good_ebo);
    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, sizeof(struct churn_vertex),
                          (void *)offsetof(struct churn_vertex, pos));
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_color, 4, GL_UNSIGNED_BYTE, GL_TRUE, sizeof(struct churn_vertex),
                          (void *)offsetof(struct churn_vertex, color));
    glEnableVertexAttribArray((GLuint)a_color);

    glBindBuffer(GL_ARRAY_BUFFER, decoy_vbo);
    glBindVertexArray(vaos[1]);
    glBindBuffer(GL_ARRAY_BUFFER, decoy_vbo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, decoy_ebo);
    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, sizeof(struct churn_vertex),
                          (void *)offsetof(struct churn_vertex, pos));
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_color, 4, GL_UNSIGNED_BYTE, GL_TRUE, sizeof(struct churn_vertex),
                          (void *)offsetof(struct churn_vertex, color));
    glDisableVertexAttribArray((GLuint)a_color);

    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glViewport(0, 0, 96, 64);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    glBindVertexArray(vaos[0]);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)(3 * sizeof(uint16_t)));
    glFinish();

    int ok = gr_expect_pixel(name, "vao_ebo_offset_normalized_color", 48, 32, 24, 180, 90, 255, 4);
    if (gr_expect_pixel(name, "outside_quad_untouched", 4, 4, 0, 0, 0, 255, 4) != 0) ok = -1;

    glBindVertexArray(0);
    glDeleteVertexArrays(2, vaos);
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 12;
    printf("%s configure=%u egl=%d.%d PASS vao_switch=1 ebo_per_vao=1 draw_offset=6 attrs=pos2f,color4ub_norm\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
