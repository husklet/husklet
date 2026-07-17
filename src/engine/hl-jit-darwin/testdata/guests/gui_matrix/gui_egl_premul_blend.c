#include "gui_egl_render_probe.h"

static int draw_premul_over(const char *name, GLuint program, GLuint fbo, float r, float g, float b,
                            uint8_t er, uint8_t eg, uint8_t eb) {
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glViewport(0, 0, 64, 64);
    glClearColor(r, g, b, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    glUniform4f(glGetUniformLocation(program, "uColor"), 0.50f, 0.0f, 0.0f, 0.50f);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_BLEND);
    glFinish();
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    return gr_expect_pixel(name, r > 0.5f ? "premul_over_white" : "premul_over_black",
                           32, 32, er, eg, eb, 255, 6);
}

int main(void) {
    const char *name = "gui_egl_premul_blend";
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

    GLuint black_tex = 0, black_fbo = 0;
    GLuint white_tex = 0, white_fbo = 0;
    if (gr_make_fbo(name, 64, 64, &black_tex, &black_fbo) != 0 ||
        gr_make_fbo(name, 64, 64, &white_tex, &white_fbo) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    glUseProgram(solid);
    if (gr_bind_quad(name, solid) != 0) {
        gr_close_window(&gw);
        return 11;
    }
    int ok = 0;
    if (draw_premul_over(name, solid, black_fbo, 0.0f, 0.0f, 0.0f, 128, 0, 0) != 0) ok = -1;
    if (draw_premul_over(name, solid, white_fbo, 1.0f, 1.0f, 1.0f, 255, 128, 128) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.03f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(textured);
    if (gr_bind_quad(name, textured) != 0) {
        gr_close_window(&gw);
        return 12;
    }
    glEnable(GL_SCISSOR_TEST);
    glUniform1i(glGetUniformLocation(textured, "uTex"), 0);
    glScissor(0, 0, gw.width / 2, gw.height);
    glBindTexture(GL_TEXTURE_2D, black_tex);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glScissor(gw.width / 2, 0, gw.width / 2, gw.height);
    glBindTexture(GL_TEXTURE_2D, white_tex);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_SCISSOR_TEST);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 13;
    printf("%s configure=%u egl=%d.%d premul=0.5 over_black=1 over_white=1 vbo=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, vbo != 0);
    return 0;
}
