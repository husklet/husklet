#include "gui_egl_render_probe.h"

int main(void) {
    const char *name = "gui_egl_scissored_quad";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 160, 160, 2);
    if (r != 0) return r;

    GLuint solid = 0;
    GLuint vbo = 0;
    GLuint tex = 0, fbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &solid) != 0 ||
        gr_make_quad(&vbo) != 0 ||
        gr_make_fbo(name, 80, 80, &tex, &fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    gr_clear_rgba(fbo, 80, 80, 0.0f, 0.0f, 0.0f, 1.0f);
    glUseProgram(solid);
    if (gr_bind_quad(name, solid) != 0) {
        gr_close_window(&gw);
        return 10;
    }
    glEnable(GL_SCISSOR_TEST);
    glScissor(24, 24, 32, 32);
    glUniform4f(glGetUniformLocation(solid, "uColor"), 0.05f, 0.90f, 0.25f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_SCISSOR_TEST);
    glFinish();

    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    int ok = 0;
    if (gr_expect_pixel(name, "center", 40, 40, 13, 230, 64, 255, 5) != 0) ok = -1;
    if (gr_expect_pixel(name, "outside", 8, 8, 0, 0, 0, 255, 2) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glEnable(GL_SCISSOR_TEST);
    glScissor(48, 48, 64, 64);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_SCISSOR_TEST);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 11;
    printf("%s configure=%u egl=%d.%d fbo=80x80 scissor=24,24,32,32 default_scissor=48,48,64,64 vbo=%u tex=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, vbo != 0, tex != 0);
    return 0;
}
