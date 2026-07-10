#include "gui_egl_render_probe.h"

/*
 * Uses five sampler2D uniforms in one fragment shader. The expected pixel
 * depends on texture unit 4, so a four-sampler backend cap or binding mismatch
 * produces a deterministic readback failure.
 */

static const char *FS_FIVE_SAMPLERS =
    "precision mediump float;\n"
    "uniform sampler2D uTex0;\n"
    "uniform sampler2D uTex1;\n"
    "uniform sampler2D uTex2;\n"
    "uniform sampler2D uTex3;\n"
    "uniform sampler2D uTex4;\n"
    "varying vec2 vUV;\n"
    "void main() {\n"
    "  vec4 c0 = texture2D(uTex0, vUV);\n"
    "  vec4 c1 = texture2D(uTex1, vUV);\n"
    "  vec4 c2 = texture2D(uTex2, vUV);\n"
    "  vec4 c3 = texture2D(uTex3, vUV);\n"
    "  vec4 c4 = texture2D(uTex4, vUV);\n"
    "  gl_FragColor = vec4(c0.r + c3.r, c1.g + c4.g, c2.b + c4.b, 1.0);\n"
    "}\n";

static GLuint make_rgba1(uint8_t r, uint8_t g, uint8_t b, uint8_t a) {
    uint8_t px[4] = {r, g, b, a};
    return gr_make_rgba_texture(1, 1, px);
}

static int set_sampler(GLuint program, const char *name, GLint unit) {
    GLint loc = glGetUniformLocation(program, name);
    if (loc < 0) return -1;
    glUniform1i(loc, unit);
    return 0;
}

int main(void) {
    const char *name = "gui_egl_fifth_sampler";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 96, 64, 2);
    if (r != 0) return r;

    GLuint program = 0;
    GLuint vbo = 0;
    GLuint target_tex = 0;
    GLuint target_fbo = 0;
    if (gr_make_program(name, FS_FIVE_SAMPLERS, &program) != 0 ||
        gr_make_quad(&vbo) != 0 ||
        gr_make_fbo(name, 16, 16, &target_tex, &target_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    (void)vbo;

    GLuint textures[5];
    textures[0] = make_rgba1(25, 0, 0, 255);
    textures[1] = make_rgba1(0, 40, 0, 255);
    textures[2] = make_rgba1(0, 0, 70, 255);
    textures[3] = make_rgba1(95, 0, 0, 255);
    textures[4] = make_rgba1(0, 105, 35, 255);
    for (int i = 0; i < 5; i++) {
        if (!textures[i]) {
            printf("%s texture_alloc index=%d\n", name, i);
            gr_close_window(&gw);
            return 10;
        }
    }

    glBindFramebuffer(GL_FRAMEBUFFER, target_fbo);
    glViewport(0, 0, 16, 16);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0 ||
        set_sampler(program, "uTex0", 0) != 0 ||
        set_sampler(program, "uTex1", 1) != 0 ||
        set_sampler(program, "uTex2", 2) != 0 ||
        set_sampler(program, "uTex3", 3) != 0 ||
        set_sampler(program, "uTex4", 4) != 0) {
        printf("%s sampler_uniform_setup failed\n", name);
        gr_close_window(&gw);
        return 11;
    }
    for (int i = 0; i < 5; i++) {
        glActiveTexture(GL_TEXTURE0 + i);
        glBindTexture(GL_TEXTURE_2D, textures[i]);
    }
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "five_sampler_mix", 8, 8, 120, 145, 105, 255, 3) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.03f, 0.02f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("FAIL %s assertions\n", name);
        return 12;
    }
    printf("PASS %s configure=%u egl=%d.%d samplers=5 unit4_required=1 expected=120,145,105,255\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
