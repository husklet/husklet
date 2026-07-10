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

static GLuint make_updated_1x1_texture(void) {
    uint8_t blue[] = {28, 68, 200, 255};
    uint8_t red[] = {220, 40, 30, 255};
    GLuint tex = gr_make_rgba_texture(1, 1, blue);
    glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, red);
    return tex;
}

static int bind_program_quad(const char *name, GLuint program) {
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0) return -1;
    return 0;
}

int main(void) {
    const char *name = "gui_egl_content_composite_layer";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 128, 96, 2);
    if (r != 0) return r;

    GLuint tex_program = 0;
    GLuint solid_program = 0;
    GLuint vbo = 0;
    GLuint ebo = 0;
    GLuint layer_tex = 0;
    GLuint layer_fbo = 0;
    if (gr_make_program(name, GR_FS_TEX, &tex_program) != 0 ||
        gr_make_program(name, GR_FS_SOLID, &solid_program) != 0 ||
        make_indexed_quad(&vbo, &ebo) != 0 ||
        gr_make_fbo(name, 64, 64, &layer_tex, &layer_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    GLuint upload_tex = make_updated_1x1_texture();
    if (!upload_tex) {
        printf("%s texture_alloc=0\n", name);
        gr_close_window(&gw);
        return 10;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, layer_fbo);
    glViewport(0, 0, 64, 64);
    glClearColor(0.0f, 0.0f, 0.0f, 0.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
    if (bind_program_quad(name, tex_program) != 0) {
        gr_close_window(&gw);
        return 11;
    }
    glUniform1i(glGetUniformLocation(tex_program, "uTex"), 0);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, upload_tex);
    glEnable(GL_SCISSOR_TEST);
    glViewport(8, 8, 48, 40);
    glScissor(8, 8, 48, 40);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);

    if (bind_program_quad(name, solid_program) != 0) {
        gr_close_window(&gw);
        return 12;
    }
    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    glUniform4f(glGetUniformLocation(solid_program, "uColor"), 0.5f, 0.5f, 0.5f, 0.5f);
    glViewport(0, 0, 64, 64);
    glScissor(20, 20, 24, 16);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);
    glDisable(GL_BLEND);
    glDisable(GL_SCISSOR_TEST);
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "offscreen_clear", 4, 4, 0, 0, 0, 0, 1) != 0) ok = -1;
    if (gr_expect_pixel(name, "offscreen_uploaded_red", 12, 12, 220, 40, 30, 255, 2) != 0) ok = -1;
    if (gr_expect_pixel(name, "offscreen_premul_blend", 24, 24, 238, 148, 143, 255, 4) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.025f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (bind_program_quad(name, tex_program) != 0) {
        gr_close_window(&gw);
        return 13;
    }
    glUniform1i(glGetUniformLocation(tex_program, "uTex"), 0);
    glBindTexture(GL_TEXTURE_2D, layer_tex);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);
    glFinish();
    if (gr_expect_pixel(name, "default_composite", 48, 60, 238, 148, 143, 255, 5) != 0) ok = -1;
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("%s FAIL\n", name);
        return 14;
    }
    printf("%s PASS configure=%u egl=%d.%d offscreen_rgba=64x64 subimage=1 indexed=1 viewport_scissor=2 premul_blend=1 default_composite=1\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
