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
    const char *name = "gui_egl_damage_scissor_readback";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 160, 120, 2);
    if (r != 0) return r;

    GLuint solid = 0;
    GLuint textured = 0;
    GLuint vbo = 0;
    GLuint layer_tex = 0;
    GLuint layer_fbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &solid) != 0 ||
        gr_make_program(name, GR_FS_TEX, &textured) != 0 ||
        gr_make_quad(&vbo) != 0 ||
        gr_make_fbo(name, 96, 64, &layer_tex, &layer_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, layer_fbo);
    glViewport(0, 0, 96, 64);
    glClearColor(0.02f, 0.03f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glEnable(GL_SCISSOR_TEST);

    draw_solid_rect(name, solid, 8, 8, 24, 20, 0.90f, 0.10f, 0.12f, 1.0f);
    draw_solid_rect(name, solid, 40, 20, 24, 24, 0.12f, 0.78f, 0.20f, 1.0f);
    draw_solid_rect(name, solid, 68, 36, 16, 16, 0.10f, 0.38f, 0.95f, 1.0f);
    glDisable(GL_SCISSOR_TEST);
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "damage_red", 18, 18, 230, 26, 31, 255, 5) != 0) ok = -1;
    if (gr_expect_pixel(name, "damage_green", 52, 32, 31, 199, 51, 255, 5) != 0) ok = -1;
    if (gr_expect_pixel(name, "damage_blue", 76, 44, 26, 97, 242, 255, 5) != 0) ok = -1;
    if (gr_expect_pixel(name, "retained_bg", 2, 2, 5, 8, 10, 255, 5) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(textured);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    if (gr_bind_quad(name, textured) != 0) {
        gr_close_window(&gw);
        return 10;
    }
    glUniform1i(glGetUniformLocation(textured, "uTex"), 0);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, layer_tex);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glFinish();
    if (gr_expect_pixel(name, "default_marker_green", 86, 60, 31, 199, 51, 255, 8) != 0) ok = -1;
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("%s FAIL\n", name);
        return 11;
    }
    printf("%s PASS configure=%u egl=%d.%d partial_updates=3 scissor_damage=3 retained_readback=1 default_marker=1 layer=96x64 vbo=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, vbo != 0);
    return 0;
}
