#include "gui_egl_render_probe.h"

int main(void) {
    const char *name = "gui_egl_copytex_sample";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 192, 128, 2);
    if (r != 0) return r;

    GLuint textured = 0;
    GLuint vbo = 0;
    if (gr_make_program(name, GR_FS_TEX, &textured) != 0 || gr_make_quad(&vbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    GLuint src_tex = 0, src_fbo = 0;
    GLuint dst_tex = 0, dst_fbo = 0;
    if (gr_make_fbo(name, 64, 64, &src_tex, &src_fbo) != 0 ||
        gr_make_fbo(name, 64, 64, &dst_tex, &dst_fbo) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    gr_clear_rgba(src_fbo, 64, 64, 0.20f, 0.80f, 0.35f, 1.0f);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, src_fbo);
    glBindTexture(GL_TEXTURE_2D, dst_tex);
    glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 0, 0, 32, 64);

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.85f, 0.15f, 0.10f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindTexture(GL_TEXTURE_2D, dst_tex);
    glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 32, 0, 0, 0, 32, 64);

    glBindFramebuffer(GL_FRAMEBUFFER, dst_fbo);
    int ok = 0;
    if (gr_expect_pixel(name, "copy_from_fbo", 16, 32, 51, 204, 89, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "copy_from_default", 48, 32, 217, 38, 26, 255, 4) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.03f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(textured);
    if (gr_bind_quad(name, textured) != 0) {
        gr_close_window(&gw);
        return 11;
    }
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, dst_tex);
    glUniform1i(glGetUniformLocation(textured, "uTex"), 0);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 12;
    printf("%s configure=%u egl=%d.%d copy_fbo=32x64 copy_default=32x64 sampled=1 vbo=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, vbo != 0);
    return 0;
}
