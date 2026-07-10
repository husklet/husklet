#include "gui_egl_render_probe.h"

/*
 * Emits more than gl_shim.c's historical MAXDRAWS-sized frame queue. The final
 * red full-screen draw is intentionally above draw 512; if the queue silently
 * drops later draws, the checked pixel remains blue.
 */

enum { DRAW_COUNT = 520 };

static int bind_solid_quad(const char *name, GLuint program, GLint *u_color) {
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0) return -1;
    *u_color = glGetUniformLocation(program, "uColor");
    if (*u_color < 0) {
        printf("%s uColor missing\n", name);
        return -1;
    }
    return 0;
}

int main(void) {
    const char *name = "gui_egl_draw_count_sentinel";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 96, 64, 2);
    if (r != 0) return r;

    GLuint program = 0;
    GLuint vbo = 0;
    GLuint target_tex = 0;
    GLuint target_fbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &program) != 0 ||
        gr_make_quad(&vbo) != 0 ||
        gr_make_fbo(name, 16, 16, &target_tex, &target_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    (void)vbo;

    GLint u_color = -1;
    glBindFramebuffer(GL_FRAMEBUFFER, target_fbo);
    glViewport(0, 0, 16, 16);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (bind_solid_quad(name, program, &u_color) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    for (int i = 0; i < DRAW_COUNT; i++) {
        if (i == DRAW_COUNT - 1) {
            glUniform4f(u_color, 1.0f, 0.0f, 0.0f, 1.0f);
        } else {
            glUniform4f(u_color, 0.0f, 0.0f, 1.0f, 1.0f);
        }
        glDrawArrays(GL_TRIANGLES, 0, 6);
    }
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "draw_after_512", 8, 8, 255, 0, 0, 255, 3) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.04f, 0.03f, 0.02f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("FAIL %s assertions draws=%d sentinel_index=%d\n",
               name, DRAW_COUNT, DRAW_COUNT - 1);
        return 11;
    }
    printf("PASS %s configure=%u egl=%d.%d draws=%d sentinel_after_512=1 final_red=1\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, DRAW_COUNT);
    return 0;
}
