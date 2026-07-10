#include "gui_egl_render_probe.h"

int main(void) {
    const char *name = "gui_egl_blit_framebuffer";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 192, 128, 3);
    if (r != 0) return r;

    GLuint src_tex = 0, src_fbo = 0;
    GLuint dst_tex = 0, dst_fbo = 0;
    GLuint def_tex = 0, def_fbo = 0;
    if (gr_make_fbo(name, 64, 64, &src_tex, &src_fbo) != 0 ||
        gr_make_fbo(name, 64, 64, &dst_tex, &dst_fbo) != 0 ||
        gr_make_fbo(name, 64, 64, &def_tex, &def_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    gr_clear_rgba(src_fbo, 64, 64, 0.10f, 0.35f, 0.90f, 1.0f);
    gr_clear_rgba(dst_fbo, 64, 64, 0.0f, 0.0f, 0.0f, 1.0f);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, src_fbo);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, dst_fbo);
    glBlitFramebuffer(0, 0, 64, 64, 0, 0, 64, 64, GL_COLOR_BUFFER_BIT, GL_NEAREST);

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.95f, 0.80f, 0.12f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    gr_clear_rgba(def_fbo, 64, 64, 0.0f, 0.0f, 0.0f, 1.0f);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, 0);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, def_fbo);
    glBlitFramebuffer(0, 0, 64, 64, 0, 0, 64, 64, GL_COLOR_BUFFER_BIT, GL_NEAREST);

    int ok = 0;
    glBindFramebuffer(GL_FRAMEBUFFER, dst_fbo);
    if (gr_expect_pixel(name, "blit_fbo_to_fbo", 32, 32, 26, 89, 230, 255, 4) != 0) ok = -1;
    glBindFramebuffer(GL_FRAMEBUFFER, def_fbo);
    if (gr_expect_pixel(name, "blit_default_to_fbo", 32, 32, 242, 204, 31, 255, 4) != 0) ok = -1;

    glBindFramebuffer(GL_READ_FRAMEBUFFER, dst_fbo);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, 0);
    glBlitFramebuffer(0, 0, 64, 64, 0, 0, gw.width, gw.height, GL_COLOR_BUFFER_BIT, GL_NEAREST);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 10;
    printf("%s configure=%u egl=%d.%d blit_fbo_fbo=1 blit_fbo_default=1 blit_default_fbo=1 src_tex=%u dst_tex=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, src_tex != 0, dst_tex != 0);
    return 0;
}
