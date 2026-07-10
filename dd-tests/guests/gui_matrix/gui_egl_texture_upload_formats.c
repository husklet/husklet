#include "gui_egl_render_probe.h"

static GLuint upload_bgra_texture(void) {
    uint8_t bgra[] = {11, 37, 221, 255};
    GLuint texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 1, 1, 0, GL_BGRA_EXT, GL_UNSIGNED_BYTE, bgra);
    gr_texture_params();
    return texture;
}

static GLuint upload_red_texture(void) {
    uint8_t red[] = {91};
    GLuint texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_R8, 1, 1, 0, GL_RED, GL_UNSIGNED_BYTE, red);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    gr_texture_params();
    return texture;
}

static GLuint upload_luminance_with_pixel_store(void) {
    enum { ROW_LENGTH = 5, ROW_STRIDE = 8, ROWS = 4 };
    uint8_t pixels[ROW_STRIDE * ROWS];
    for (int i = 0; i < (int)sizeof(pixels); i++) pixels[i] = 9;

    pixels[1 * ROW_STRIDE + 3] = 77;
    pixels[2 * ROW_STRIDE + 3] = 181;

    GLuint texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glPixelStorei(GL_UNPACK_ROW_LENGTH, ROW_LENGTH);
    glPixelStorei(GL_UNPACK_SKIP_ROWS, 2);
    glPixelStorei(GL_UNPACK_SKIP_PIXELS, 3);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_LUMINANCE, 1, 1, 0, GL_LUMINANCE, GL_UNSIGNED_BYTE, pixels);
    glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
    glPixelStorei(GL_UNPACK_SKIP_ROWS, 0);
    glPixelStorei(GL_UNPACK_SKIP_PIXELS, 0);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    gr_texture_params();
    return texture;
}

static int sample_expect(const char *name, GLuint program, GLuint fbo, GLuint texture,
                         const char *label, uint8_t r, uint8_t g, uint8_t b, uint8_t a) {
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glViewport(0, 0, 16, 16);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(program);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, texture);
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glFinish();
    return gr_expect_pixel(name, label, 8, 8, r, g, b, a, 1);
}

int main(void) {
    const char *name = "gui_egl_texture_upload_formats";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 128, 96, 3);
    if (r != 0) return r;

    GLuint program = 0;
    GLuint vbo = 0;
    GLuint target_tex = 0;
    GLuint target_fbo = 0;
    if (gr_make_program(name, GR_FS_TEX, &program) != 0 ||
        gr_make_quad(&vbo) != 0 ||
        gr_make_fbo(name, 16, 16, &target_tex, &target_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    GLuint bgra = upload_bgra_texture();
    GLuint red = upload_red_texture();
    GLuint lum = upload_luminance_with_pixel_store();
    if (!bgra || !red || !lum) {
        printf("%s texture_alloc bgra=%u red=%u lum=%u\n", name, bgra, red, lum);
        gr_close_window(&gw);
        return 11;
    }

    int ok = 0;
    if (sample_expect(name, program, target_fbo, bgra, "bgra_swizzle", 221, 37, 11, 255) != 0) ok = -1;
    if (sample_expect(name, program, target_fbo, red, "red_upload", 91, 0, 0, 255) != 0) ok = -1;
    if (sample_expect(name, program, target_fbo, lum, "luminance_pixel_store", 181, 181, 181, 255) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.03f, 0.03f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindTexture(GL_TEXTURE_2D, bgra);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 12;
    printf("%s configure=%u egl=%d.%d bgra_swizzle=1 red=1 luminance=1 row_length=5 skip=3,2 alignment=4 sampled_fbo=1 vbo=%u target_tex=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, vbo != 0, target_tex != 0);
    return 0;
}
