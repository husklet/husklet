#include "gui_egl_render_probe.h"

static GLuint make_content_texture(void) {
    uint8_t pixels[4 * 4 * 4];
    for (int y = 0; y < 4; y++) {
        for (int x = 0; x < 4; x++) {
            uint8_t *p = pixels + (y * 4 + x) * 4;
            if (y < 2 && x < 2) {
                p[0] = 220; p[1] = 40;  p[2] = 40;  p[3] = 255;
            } else if (y < 2) {
                p[0] = 40;  p[1] = 210; p[2] = 70;  p[3] = 255;
            } else if (x < 2) {
                p[0] = 40;  p[1] = 90;  p[2] = 230; p[3] = 255;
            } else {
                p[0] = 230; p[1] = 200; p[2] = 40;  p[3] = 255;
            }
        }
    }
    return gr_make_rgba_texture(4, 4, pixels);
}

int main(void) {
    const char *name = "gui_egl_viewport_scale_clip";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 151, 103, 2);
    if (r != 0) return r;

    GLuint program = 0;
    GLuint vbo = 0;
    if (gr_make_program(name, GR_FS_TEX, &program) != 0 ||
        gr_make_quad(&vbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    GLuint tex = make_content_texture();
    if (!tex) {
        printf("%s texture_alloc=0\n", name);
        gr_close_window(&gw);
        return 10;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glDisable(GL_SCISSOR_TEST);
    glClearColor(7.0f / 255.0f, 9.0f / 255.0f, 13.0f / 255.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    glUseProgram(program);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    if (gr_bind_quad(name, program) != 0) {
        gr_close_window(&gw);
        return 11;
    }
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, tex);

    glViewport(23, 17, 96, 64);
    glEnable(GL_SCISSOR_TEST);
    glScissor(35, 25, 72, 44);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_SCISSOR_TEST);
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "scaled_top_left", 47, 61, 220, 40, 40, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "scaled_top_right", 95, 61, 40, 210, 70, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "scaled_bottom_left", 47, 33, 40, 90, 230, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "scaled_bottom_right", 95, 33, 230, 200, 40, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "clip_left_gutter", 34, 47, 7, 9, 13, 255, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "clip_right_gutter", 107, 47, 7, 9, 13, 255, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "clip_bottom_gutter", 71, 24, 7, 9, 13, 255, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "clip_top_gutter", 71, 69, 7, 9, 13, 255, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "viewport_left_outside", 22, 47, 7, 9, 13, 255, 2) != 0) ok = -1;

    GLenum err = glGetError();
    if (err) {
        printf("%s gl_error=0x%x\n", name, err);
        ok = -1;
    }
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("%s FAIL\n", name);
        return 12;
    }
    printf("%s PASS configure=%u egl=%d.%d window=151x103 viewport=23,17,96,64 scissor=35,25,72,44 texture=4x4 scaled_quadrants=4 clipped_gutters=5\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
