#include "gui_egl_render_probe.h"

#define GL_SRC_ALPHA 0x0302

static int bind_quad_program(const char *name, GLuint program, GLint *u_color) {
    glUseProgram(program);
    GLint a_pos = glGetAttribLocation(program, "aPos");
    GLint a_uv = glGetAttribLocation(program, "aUV");
    *u_color = glGetUniformLocation(program, "uColor");
    if (a_pos < 0 || a_uv < 0 || *u_color < 0) {
        printf("%s attribs pos=%d uv=%d color=%d\n", name, a_pos, a_uv, *u_color);
        return -1;
    }
    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, 16, (void *)0);
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_uv, 2, GL_FLOAT, GL_FALSE, 16, (void *)8);
    glEnableVertexAttribArray((GLuint)a_uv);
    return 0;
}

int main(void) {
    const char *name = "gui_egl_state_churn_pipeline";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 128, 96, 2);
    if (r != 0) return r;

    GLuint program = 0;
    if (gr_make_program(name, GR_FS_SOLID, &program) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    GLuint tex = 0;
    GLuint fbo = 0;
    if (gr_make_fbo(name, 96, 64, &tex, &fbo) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    GLuint vbo = 0;
    if (gr_make_quad(&vbo) != 0) {
        gr_close_window(&gw);
        return 11;
    }

    GLint u_color = -1;
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    if (bind_quad_program(name, program, &u_color) != 0) {
        gr_close_window(&gw);
        return 12;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    glViewport(0, 0, 48, 64);
    glEnable(GL_SCISSOR_TEST);
    glScissor(8, 8, 24, 48);
    glDisable(GL_BLEND);
    glUniform4f(u_color, 1.0f, 0.0f, 0.0f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    glViewport(48, 0, 48, 64);
    glDisable(GL_SCISSOR_TEST);
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    glUniform4f(u_color, 0.0f, 0.0f, 1.0f, 0.5f);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    glViewport(0, 0, 96, 64);
    glDisable(GL_BLEND);
    glEnable(GL_SCISSOR_TEST);
    glScissor(40, 24, 16, 16);
    glUniform4f(u_color, 0.0f, 1.0f, 0.0f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_SCISSOR_TEST);
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "left_scissor_inside", 16, 32, 255, 0, 0, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "left_scissor_outside", 36, 32, 0, 0, 0, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "right_blended_viewport", 72, 32, 0, 0, 128, 255, 6) != 0) ok = -1;
    if (gr_expect_pixel(name, "center_scissor_after_blend_disable", 48, 32, 0, 255, 0, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "right_viewport_edge_unchanged", 94, 4, 0, 0, 128, 255, 6) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 13;
    printf("%s configure=%u egl=%d.%d PASS viewport=1 scissor=1 blend=1 state_restore=1\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
