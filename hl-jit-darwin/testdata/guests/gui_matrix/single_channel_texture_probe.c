#include "gui_egl_render_probe.h"

#ifndef GL_ALPHA
#define GL_ALPHA 0x1906
#endif

static int make_texture_space_quad(GLuint *vbo) {
    float vertices[] = {
        -1.0f, -1.0f, 0.0f, 0.0f,
         1.0f, -1.0f, 1.0f, 0.0f,
        -1.0f,  1.0f, 0.0f, 1.0f,
         1.0f, -1.0f, 1.0f, 0.0f,
         1.0f,  1.0f, 1.0f, 1.0f,
        -1.0f,  1.0f, 0.0f, 1.0f,
    };
    glGenBuffers(1, vbo);
    glBindBuffer(GL_ARRAY_BUFFER, *vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
    return *vbo ? 0 : -1;
}

static GLuint upload_2x2_single(GLenum internal_format, GLenum format) {
    uint8_t pixels[] = {
         40,  80,
        160, 220,
    };
    GLuint texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, internal_format, 2, 2, 0, format, GL_UNSIGNED_BYTE, pixels);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    gr_texture_params();
    return texture;
}

static int expect_2x2(const char *name, const char *label, const uint8_t rgba[4][4]) {
    int ok = 0;
    if (gr_expect_pixel(name, label,  4,  4, rgba[0][0], rgba[0][1], rgba[0][2], rgba[0][3], 1) != 0) ok = -1;
    if (gr_expect_pixel(name, label, 12,  4, rgba[1][0], rgba[1][1], rgba[1][2], rgba[1][3], 1) != 0) ok = -1;
    if (gr_expect_pixel(name, label,  4, 12, rgba[2][0], rgba[2][1], rgba[2][2], rgba[2][3], 1) != 0) ok = -1;
    if (gr_expect_pixel(name, label, 12, 12, rgba[3][0], rgba[3][1], rgba[3][2], rgba[3][3], 1) != 0) ok = -1;
    return ok;
}

static int draw_and_check(const char *name, GLuint program, GLuint fbo, GLuint texture,
                          const char *label, const uint8_t rgba[4][4]) {
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glViewport(0, 0, 16, 16);
    glClearColor(1.0f, 0.0f, 1.0f, 0.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(program);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, texture);
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glFinish();
    return expect_2x2(name, label, rgba);
}

int main(void) {
    const char *name = "single_channel_texture_probe";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 96, 64, 3);
    if (r != 0) return r;

    GLuint program = 0;
    GLuint vbo = 0;
    GLuint target_tex = 0;
    GLuint target_fbo = 0;
    if (gr_make_program(name, GR_FS_TEX, &program) != 0 ||
        make_texture_space_quad(&vbo) != 0 ||
        gr_make_fbo(name, 16, 16, &target_tex, &target_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    GLuint red = upload_2x2_single(GL_R8, GL_RED);
    GLuint alpha = upload_2x2_single(GL_ALPHA, GL_ALPHA);
    GLuint luminance = upload_2x2_single(GL_LUMINANCE, GL_LUMINANCE);
    if (!red || !alpha || !luminance) {
        printf("%s texture_alloc red=%u alpha=%u luminance=%u\n", name, red, alpha, luminance);
        gr_close_window(&gw);
        return 11;
    }

    static const uint8_t expect_red[4][4] = {
        { 40, 0, 0, 255 }, { 80, 0, 0, 255 }, { 160, 0, 0, 255 }, { 220, 0, 0, 255 },
    };
    static const uint8_t expect_alpha[4][4] = {
        { 0, 0, 0, 40 }, { 0, 0, 0, 80 }, { 0, 0, 0, 160 }, { 0, 0, 0, 220 },
    };
    static const uint8_t expect_luminance[4][4] = {
        { 40, 40, 40, 255 }, { 80, 80, 80, 255 }, { 160, 160, 160, 255 }, { 220, 220, 220, 255 },
    };

    int ok = 0;
    if (draw_and_check(name, program, target_fbo, red, "red_2x2", expect_red) != 0) ok = -1;
    if (draw_and_check(name, program, target_fbo, alpha, "alpha_2x2", expect_alpha) != 0) ok = -1;
    if (draw_and_check(name, program, target_fbo, luminance, "luminance_2x2", expect_luminance) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.03f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("FAIL %s assertions\n", name);
        return 12;
    }
    printf("PASS %s configure=%u egl=%d.%d red=1 alpha=1 luminance=1 pattern=2x2 target_tex=%u vbo=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, target_tex, vbo);
    return 0;
}
