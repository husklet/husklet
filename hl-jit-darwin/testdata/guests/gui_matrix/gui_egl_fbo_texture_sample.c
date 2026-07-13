#include "gui_egl_render_probe.h"

int main(void) {
    const char *name = "gui_egl_fbo_texture_sample";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 192, 128, 2);
    if (r != 0) return r;

    GLuint solid = 0;
    GLuint textured = 0;
    GLuint vbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &solid) != 0 ||
        gr_make_program(name, GR_FS_TEX, &textured) != 0 ||
        gr_make_quad(&vbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    GLuint src_tex = 0, src_fbo = 0;
    GLuint check_tex = 0, check_fbo = 0;
    if (gr_make_fbo(name, 64, 64, &src_tex, &src_fbo) != 0 ||
        gr_make_fbo(name, 64, 64, &check_tex, &check_fbo) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    gr_clear_rgba(src_fbo, 64, 64, 0.02f, 0.02f, 0.02f, 1.0f);
    glUseProgram(solid);
    if (gr_bind_quad(name, solid) != 0) {
        gr_close_window(&gw);
        return 11;
    }
    glUniform4f(glGetUniformLocation(solid, "uColor"), 0.12f, 0.75f, 0.18f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);

    gr_clear_rgba(check_fbo, 64, 64, 0.0f, 0.0f, 0.0f, 1.0f);
    glUseProgram(textured);
    if (gr_bind_quad(name, textured) != 0) {
        gr_close_window(&gw);
        return 12;
    }
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, src_tex);
    glUniform1i(glGetUniformLocation(textured, "uTex"), 0);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glFinish();

    glBindFramebuffer(GL_FRAMEBUFFER, check_fbo);
    int ok = gr_expect_pixel(name, "sampled_fbo", 32, 32, 31, 191, 46, 255, 6);

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.03f, 0.05f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindTexture(GL_TEXTURE_2D, src_tex);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 13;
    printf("%s configure=%u egl=%d.%d fbo_tex=%u check_tex=%u sample_to_default=1 vbo=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor,
           src_tex != 0, check_tex != 0, vbo != 0);
    return 0;
}
