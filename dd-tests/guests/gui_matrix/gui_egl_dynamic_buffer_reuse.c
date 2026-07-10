#include "gui_egl_render_probe.h"

#ifndef GL_DYNAMIC_DRAW
#define GL_DYNAMIC_DRAW 0x88E8
#endif

extern void glBufferSubData(GLenum, GLsizeiptr, GLsizeiptr, const void *);

static int make_indexed_quad(GLuint *vbo, GLuint *ebo, const float *vertices, const uint16_t *indices) {
    glGenBuffers(1, vbo);
    glBindBuffer(GL_ARRAY_BUFFER, *vbo);
    glBufferData(GL_ARRAY_BUFFER, 16 * (GLsizeiptr)sizeof(float), vertices, GL_DYNAMIC_DRAW);

    glGenBuffers(1, ebo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, *ebo);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, 6 * (GLsizeiptr)sizeof(uint16_t), indices, GL_DYNAMIC_DRAW);
    return (*vbo && *ebo) ? 0 : -1;
}

int main(void) {
    const char *name = "gui_egl_dynamic_buffer_reuse";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 160, 120, 2);
    if (r != 0) return r;

    GLuint program = 0;
    if (gr_make_program(name, GR_FS_SOLID, &program) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    const float left_green[] = {
        -0.95f,  0.82f, 0.0f, 0.0f,
        -0.95f, -0.82f, 0.0f, 1.0f,
        -0.05f, -0.82f, 1.0f, 1.0f,
        -0.05f,  0.82f, 1.0f, 0.0f,
    };
    const float right_blue[] = {
         0.05f,  0.82f, 0.0f, 0.0f,
         0.05f, -0.82f, 0.0f, 1.0f,
         0.95f, -0.82f, 1.0f, 1.0f,
         0.95f,  0.82f, 1.0f, 0.0f,
    };
    const float poison_center[] = {
        -0.18f,  0.18f, 0.0f, 0.0f,
        -0.18f, -0.18f, 0.0f, 1.0f,
         0.18f, -0.18f, 1.0f, 1.0f,
         0.18f,  0.18f, 1.0f, 0.0f,
    };
    const uint16_t first_indices[] = {0, 1, 2, 0, 2, 3};
    const uint16_t second_indices[] = {3, 2, 1, 3, 1, 0};
    const uint16_t poison_indices[] = {0, 1, 2, 0, 2, 3};

    GLuint vbo = 0;
    GLuint ebo = 0;
    if (make_indexed_quad(&vbo, &ebo, left_green, first_indices) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.03f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    glUseProgram(program);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
    if (gr_bind_quad(name, program) != 0) {
        gr_close_window(&gw);
        return 11;
    }

    glUniform4f(glGetUniformLocation(program, "uColor"), 0.08f, 0.70f, 0.22f, 1.0f);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);

    glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(right_blue), right_blue);
    glBufferSubData(GL_ELEMENT_ARRAY_BUFFER, 0, sizeof(second_indices), second_indices);
    glUniform4f(glGetUniformLocation(program, "uColor"), 0.08f, 0.32f, 0.95f, 1.0f);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);

    glBufferData(GL_ARRAY_BUFFER, sizeof(poison_center), poison_center, GL_DYNAMIC_DRAW);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, sizeof(poison_indices), poison_indices, GL_DYNAMIC_DRAW);

    if (gr_swap(&gw) != 0) {
        gr_close_window(&gw);
        return 12;
    }

    gr_close_window(&gw);
    printf("%s PASS configure=%u egl=%d.%d reused_vbo=1 reused_ebo=1 draws_before_swap=2 orphan_poison=1 expected=left_green_right_blue\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
