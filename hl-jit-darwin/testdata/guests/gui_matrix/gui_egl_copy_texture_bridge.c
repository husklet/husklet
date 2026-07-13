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
    const char *name = "gui_egl_copy_texture_bridge";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 160, 112, 2);
    if (r != 0) return r;

    GLuint solid = 0;
    GLuint textured = 0;
    GLuint vbo = 0;
    GLuint src_tex = 0, src_fbo = 0;
    GLuint atlas_tex = 0, atlas_fbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &solid) != 0 ||
        gr_make_program(name, GR_FS_TEX, &textured) != 0 ||
        gr_make_quad(&vbo) != 0 ||
        gr_make_fbo(name, 64, 64, &src_tex, &src_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    atlas_tex = gr_make_rgba_texture(64, 64, NULL);
    glBindFramebuffer(GL_FRAMEBUFFER, src_fbo);
    glViewport(0, 0, 64, 64);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glEnable(GL_SCISSOR_TEST);
    draw_solid_rect(name, solid, 0, 0, 32, 64, 0.85f, 0.18f, 0.10f, 1.0f);
    draw_solid_rect(name, solid, 32, 0, 32, 64, 0.10f, 0.70f, 0.90f, 1.0f);
    glDisable(GL_SCISSOR_TEST);

    glBindFramebuffer(GL_FRAMEBUFFER, src_fbo);
    glBindTexture(GL_TEXTURE_2D, atlas_tex);
    glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 0, 0, 32, 64);
    GLenum copy_a = glGetError();
    glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 32, 0, 32, 0, 32, 64);
    GLenum copy_b = glGetError();

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.18f, 0.90f, 0.28f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindTexture(GL_TEXTURE_2D, atlas_tex);
    glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 48, 48, 0, 0, 16, 16);
    GLenum copy_c = glGetError();
    if (copy_a || copy_b || copy_c) {
        printf("%s copytex_error fbo_left=0x%x fbo_right=0x%x default=0x%x\n",
               name, copy_a, copy_b, copy_c);
        gr_close_window(&gw);
        return 10;
    }

    glGenFramebuffers(1, &atlas_fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, atlas_fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, atlas_tex, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        printf("%s atlas_fbo=0 tex=%u fbo=%u\n", name, atlas_tex, atlas_fbo);
        gr_close_window(&gw);
        return 11;
    }

    int ok = 0;
    if (gr_expect_pixel(name, "copy_tile_left", 16, 32, 217, 46, 26, 255, 6) != 0) ok = -1;
    if (gr_expect_pixel(name, "copy_tile_right", 48, 32, 26, 179, 230, 255, 6) != 0) ok = -1;
    if (gr_expect_pixel(name, "copy_tile_default", 56, 56, 46, 230, 71, 255, 6) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.01f, 0.01f, 0.01f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(textured);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    if (gr_bind_quad(name, textured) != 0) {
        gr_close_window(&gw);
        return 12;
    }
    glUniform1i(glGetUniformLocation(textured, "uTex"), 0);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, atlas_tex);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("%s FAIL\n", name);
        return 13;
    }
    printf("%s PASS configure=%u egl=%d.%d gles2_copytex=1 fbo_to_texture=2 default_to_texture=1 atlas_readback=3 sampled_default=1 src_tex=%u atlas_tex=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, src_tex != 0, atlas_tex != 0);
    return 0;
}
