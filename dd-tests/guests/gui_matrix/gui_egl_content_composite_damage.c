#include "gui_egl_render_probe.h"

extern void glTexSubImage2D(GLenum, GLint, GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, const void *);

static int make_indexed_quad(GLuint *vbo, GLuint *ebo) {
    float vertices[] = {
        -1.0f,  1.0f, 0.0f, 0.0f,
        -1.0f, -1.0f, 0.0f, 1.0f,
         1.0f, -1.0f, 1.0f, 1.0f,
         1.0f,  1.0f, 1.0f, 0.0f,
    };
    uint16_t indices[] = {0, 1, 2, 0, 2, 3};
    glGenBuffers(1, vbo);
    glBindBuffer(GL_ARRAY_BUFFER, *vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
    glGenBuffers(1, ebo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, *ebo);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, sizeof(indices), indices, GL_STATIC_DRAW);
    return (*vbo && *ebo) ? 0 : -1;
}

static void rgba_fill(uint8_t *dst, int pixels, uint8_t r, uint8_t g, uint8_t b, uint8_t a) {
    for (int i = 0; i < pixels; i++) {
        dst[i * 4 + 0] = r;
        dst[i * 4 + 1] = g;
        dst[i * 4 + 2] = b;
        dst[i * 4 + 3] = a;
    }
}

static GLuint make_damage_texture(void) {
    uint8_t empty[4 * 4 * 4];
    rgba_fill(empty, 16, 0, 0, 0, 0);
    return gr_make_rgba_texture(4, 4, empty);
}

static int draw_textured_batch(const char *name, GLuint program, int x, int y, int w, int h) {
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0) return -1;
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);
    glViewport(x, y, w, h);
    glScissor(x, y, w, h);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);
    return 0;
}

int main(void) {
    const char *name = "gui_egl_content_composite_damage";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 128, 96, 2);
    if (r != 0) return r;

    GLuint program = 0;
    GLuint vbo = 0;
    GLuint ebo = 0;
    GLuint layer_tex = 0;
    GLuint layer_fbo = 0;
    if (gr_make_program(name, GR_FS_TEX, &program) != 0 ||
        make_indexed_quad(&vbo, &ebo) != 0 ||
        gr_make_fbo(name, 80, 48, &layer_tex, &layer_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    GLuint damage_tex = make_damage_texture();
    if (!damage_tex) {
        printf("%s texture_alloc=0\n", name);
        gr_close_window(&gw);
        return 10;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, layer_fbo);
    glViewport(0, 0, 80, 48);
    glClearColor(0.03f, 0.035f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, damage_tex);
    glEnable(GL_SCISSOR_TEST);

    uint8_t green[2 * 2 * 4];
    rgba_fill(green, 4, 32, 210, 70, 255);
    glTexSubImage2D(GL_TEXTURE_2D, 0, 1, 1, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, green);
    if (draw_textured_batch(name, program, 0, 0, 40, 48) != 0) {
        gr_close_window(&gw);
        return 11;
    }

    uint8_t magenta[2 * 2 * 4];
    rgba_fill(magenta, 4, 230, 30, 210, 255);
    glTexSubImage2D(GL_TEXTURE_2D, 0, 1, 1, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, magenta);
    if (draw_textured_batch(name, program, 40, 0, 40, 48) != 0) {
        gr_close_window(&gw);
        return 12;
    }
    glDisable(GL_SCISSOR_TEST);
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "first_batch_retained", 20, 24, 32, 210, 70, 255, 3) != 0) ok = -1;
    if (gr_expect_pixel(name, "second_batch_updated", 60, 24, 230, 30, 210, 255, 3) != 0) ok = -1;
    if (gr_expect_pixel(name, "outside_damage", 2, 2, 0, 0, 0, 0, 1) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0) {
        gr_close_window(&gw);
        return 13;
    }
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);
    glBindTexture(GL_TEXTURE_2D, layer_tex);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);
    glFinish();
    if (gr_expect_pixel(name, "default_first_batch", 32, 48, 32, 210, 70, 255, 4) != 0) ok = -1;
    if (gr_expect_pixel(name, "default_second_batch", 96, 48, 230, 30, 210, 255, 4) != 0) ok = -1;
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("%s FAIL\n", name);
        return 14;
    }
    printf("%s PASS configure=%u egl=%d.%d offscreen_rgba=80x48 subimage_updates=2 indexed_batches=2 scissor_damage=2 default_composite=1\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
