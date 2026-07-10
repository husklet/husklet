#include "gui_egl_render_probe.h"

static void draw_solid_rect(const char *name, GLuint program, int x, int y, int w, int h,
                            float r, float g, float b, float a) {
    glUseProgram(program);
    gr_bind_quad(name, program);
    glUniform4f(glGetUniformLocation(program, "uColor"), r, g, b, a);
    glScissor(x, y, w, h);
    glDrawArrays(GL_TRIANGLES, 0, 6);
}

int main(void) {
    const char *name = "gui_egl_alpha_composite_readback";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 160, 112, 2);
    if (r != 0) return r;

    GLuint solid = 0;
    GLuint textured = 0;
    GLuint vbo = 0;
    GLuint comp_tex = 0;
    GLuint comp_fbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &solid) != 0 ||
        gr_make_program(name, GR_FS_TEX, &textured) != 0 ||
        gr_make_quad(&vbo) != 0 ||
        gr_make_fbo(name, 96, 48, &comp_tex, &comp_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, comp_fbo);
    glViewport(0, 0, 96, 48);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glEnable(GL_SCISSOR_TEST);
    glDisable(GL_BLEND);
    draw_solid_rect(name, solid, 64, 0, 32, 48, 1.0f, 1.0f, 1.0f, 1.0f);

    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    draw_solid_rect(name, solid, 0, 0, 32, 48, 0.50f, 0.0f, 0.0f, 0.50f);
    draw_solid_rect(name, solid, 32, 0, 32, 48, 0.50f, 0.0f, 0.0f, 0.50f);
    draw_solid_rect(name, solid, 32, 0, 32, 48, 0.0f, 0.0f, 0.25f, 0.25f);
    draw_solid_rect(name, solid, 64, 0, 32, 48, 0.0f, 0.25f, 0.0f, 0.25f);
    glDisable(GL_BLEND);
    glDisable(GL_SCISSOR_TEST);
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "premul_red_over_black", 16, 24, 128, 0, 0, 255, 6) != 0) ok = -1;
    if (gr_expect_pixel(name, "premul_blue_over_red", 48, 24, 96, 0, 64, 255, 6) != 0) ok = -1;
    if (gr_expect_pixel(name, "premul_green_over_white", 80, 24, 191, 255, 191, 255, 6) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.015f, 0.015f, 0.02f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(textured);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    if (gr_bind_quad(name, textured) != 0) {
        gr_close_window(&gw);
        return 10;
    }
    glUniform1i(glGetUniformLocation(textured, "uTex"), 0);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, comp_tex);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("%s FAIL\n", name);
        return 11;
    }
    printf("%s PASS configure=%u egl=%d.%d premul_alpha=3 readback=3 visible_columns=3 blend_func=one_one_minus_src_alpha composite_to_default=1 comp_tex=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, comp_tex != 0);
    return 0;
}
