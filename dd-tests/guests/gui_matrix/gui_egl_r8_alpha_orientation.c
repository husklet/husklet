#include "gui_egl_render_probe.h"

#define GL_ALPHA 0x1906

/*
 * Chrome-adjacent single-channel texture probe:
 * - GL_R8/GL_RED must sample as (R, 0, 0, 1).
 * - GL_ALPHA must sample as (0, 0, 0, A).
 * - A 2x2 upload must keep bottom/top row orientation through offscreen FBO sampling.
 */

static const char *FS_SAMPLE =
    "precision mediump float;\n"
    "uniform sampler2D uTex;\n"
    "varying vec2 vUV;\n"
    "void main() { gl_FragColor = texture2D(uTex, vUV); }\n";

static int make_identity_quad(GLuint *vbo) {
    float vertices[] = {
        -1.0f, -1.0f, 0.0f, 0.0f,
         1.0f, -1.0f, 1.0f, 0.0f,
        -1.0f,  1.0f, 0.0f, 1.0f,
        -1.0f,  1.0f, 0.0f, 1.0f,
         1.0f, -1.0f, 1.0f, 0.0f,
         1.0f,  1.0f, 1.0f, 1.0f,
    };
    glGenBuffers(1, vbo);
    glBindBuffer(GL_ARRAY_BUFFER, *vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
    return *vbo ? 0 : -1;
}

static GLuint make_single_channel_texture(GLenum internal_format, GLenum format,
                                          const uint8_t pixels[4]) {
    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, (GLint)internal_format, 2, 2, 0,
                 format, GL_UNSIGNED_BYTE, pixels);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    gr_texture_params();
    return tex;
}

static int sample_texture(const char *name, GLuint program, GLuint fbo, GLuint texture) {
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glViewport(0, 0, 16, 16);
    glClearColor(0.0f, 0.0f, 0.0f, 0.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0) return -1;
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, texture);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glFinish();
    return 0;
}

int main(void) {
    const char *name = "gui_egl_r8_alpha_orientation";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 96, 64, 3);
    if (r != 0) return r;

    GLuint program = 0;
    GLuint vbo = 0;
    GLuint target_tex = 0;
    GLuint target_fbo = 0;
    if (gr_make_program(name, FS_SAMPLE, &program) != 0 ||
        make_identity_quad(&vbo) != 0 ||
        gr_make_fbo(name, 16, 16, &target_tex, &target_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    (void)vbo;

    uint8_t red_pixels[4] = {
        31,  97,
        163, 229,
    };
    uint8_t alpha_pixels[4] = {
        47,  111,
        175, 239,
    };
    GLuint red_tex = make_single_channel_texture(GL_R8, GL_RED, red_pixels);
    GLuint alpha_tex = make_single_channel_texture(GL_ALPHA, GL_ALPHA, alpha_pixels);
    if (!red_tex || !alpha_tex) {
        printf("%s texture_alloc red=%u alpha=%u\n", name, red_tex, alpha_tex);
        gr_close_window(&gw);
        return 10;
    }

    int ok = 0;
    if (sample_texture(name, program, target_fbo, red_tex) != 0) ok = -1;
    if (gr_expect_pixel(name, "r8_bottom_left",  4,  4, 31,  0, 0, 255, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "r8_bottom_right", 12, 4, 97,  0, 0, 255, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "r8_top_left",     4, 12, 163, 0, 0, 255, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "r8_top_right",   12, 12, 229, 0, 0, 255, 2) != 0) ok = -1;

    if (sample_texture(name, program, target_fbo, alpha_tex) != 0) ok = -1;
    if (gr_expect_pixel(name, "alpha_bottom_left",  4,  4, 0, 0, 0, 47,  2) != 0) ok = -1;
    if (gr_expect_pixel(name, "alpha_bottom_right", 12, 4, 0, 0, 0, 111, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "alpha_top_left",     4, 12, 0, 0, 0, 175, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "alpha_top_right",   12, 12, 0, 0, 0, 239, 2) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.03f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("FAIL %s assertions\n", name);
        return 11;
    }
    printf("PASS %s configure=%u egl=%d.%d r8_swizzle=1 alpha_swizzle=1 orientation_2x2=1 offscreen_readback=1\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
