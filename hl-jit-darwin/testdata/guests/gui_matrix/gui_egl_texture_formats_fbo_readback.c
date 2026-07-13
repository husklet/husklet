#include "gui_egl_render_probe.h"

#define GL_RGB 0x1907
#define GL_EXTENSIONS 0x1F03

extern const unsigned char *glGetString(GLenum);
extern void glTexSubImage2D(GLenum, GLint, GLint, GLint, GLsizei, GLsizei,
                            GLenum, GLenum, const void *);

static int has_ext(const char *needle) {
    const char *ext = (const char *)glGetString(GL_EXTENSIONS);
    size_t n = strlen(needle);
    if (!ext) return 0;
    while (*ext) {
        while (*ext == ' ') ext++;
        const char *end = strchr(ext, ' ');
        size_t len = end ? (size_t)(end - ext) : strlen(ext);
        if (len == n && memcmp(ext, needle, n) == 0) return 1;
        if (!end) break;
        ext = end + 1;
    }
    return 0;
}

static void drain_gl_errors(void) {
    while (glGetError() != 0) {
    }
}

static int make_upload_fbo(const char *name, int width, int height, GLuint tex, GLuint *fbo) {
    *fbo = 0;
    glGenFramebuffers(1, fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, *fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    GLenum status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (status != GL_FRAMEBUFFER_COMPLETE) {
        printf("%s upload_fbo incomplete status=0x%x size=%dx%d tex=%u fbo=%u\n",
               name, status, width, height, tex, *fbo);
        return -1;
    }
    glViewport(0, 0, width, height);
    return 0;
}

static GLuint make_rgba_npot_with_subimage(void) {
    enum { W = 5, H = 3 };
    uint8_t base[W * H * 4];
    for (int i = 0; i < W * H; i++) {
        base[i * 4 + 0] = 17;
        base[i * 4 + 1] = 31;
        base[i * 4 + 2] = 211;
        base[i * 4 + 3] = 255;
    }

    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, base);

    enum { ROW_PIXELS = 6 };
    uint8_t patch[ROW_PIXELS * 2 * 4];
    for (int i = 0; i < (int)sizeof(patch); i++) patch[i] = 3;
    uint8_t *p0 = &patch[(1 * ROW_PIXELS + 2) * 4];
    uint8_t *p1 = &patch[(1 * ROW_PIXELS + 3) * 4];
    p0[0] = 201; p0[1] = 19;  p0[2] = 41;  p0[3] = 255;
    p1[0] = 29;  p1[1] = 199; p1[2] = 53;  p1[3] = 255;
    glPixelStorei(GL_UNPACK_ROW_LENGTH, ROW_PIXELS);
    glPixelStorei(GL_UNPACK_SKIP_ROWS, 1);
    glPixelStorei(GL_UNPACK_SKIP_PIXELS, 2);
    glTexSubImage2D(GL_TEXTURE_2D, 0, 1, 1, 2, 1, GL_RGBA, GL_UNSIGNED_BYTE, patch);
    glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
    glPixelStorei(GL_UNPACK_SKIP_ROWS, 0);
    glPixelStorei(GL_UNPACK_SKIP_PIXELS, 0);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    gr_texture_params();
    return tex;
}

static GLuint make_rgb_alignment_texture(void) {
    enum { W = 3, H = 2 };
    uint8_t pixels[W * H * 3] = {
        5,  7,  9,     21, 23, 25,    41, 43, 45,
        61, 63, 65,    81, 83, 85,    101, 103, 105,
    };
    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, W, H, 0, GL_RGB, GL_UNSIGNED_BYTE, pixels);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    gr_texture_params();
    return tex;
}

static GLuint make_bgra_texture(void) {
    uint8_t bgra[4] = {13, 71, 233, 191};
    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 1, 1, 0, GL_BGRA_EXT, GL_UNSIGNED_BYTE, bgra);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    gr_texture_params();
    return tex;
}

static int check_texture_pixels(const char *name) {
    int ok = 0;

    GLuint npot = make_rgba_npot_with_subimage();
    GLuint npot_fbo = 0;
    if (!npot || make_upload_fbo(name, 5, 3, npot, &npot_fbo) != 0) return -1;
    if (gr_expect_pixel(name, "npot_base_bottom_left", 0, 0, 17, 31, 211, 255, 1) != 0) ok = -1;
    if (gr_expect_pixel(name, "subimage_first", 1, 1, 201, 19, 41, 255, 1) != 0) ok = -1;
    if (gr_expect_pixel(name, "subimage_second", 2, 1, 29, 199, 53, 255, 1) != 0) ok = -1;
    if (gr_expect_pixel(name, "subimage_neighbor_clean", 3, 1, 17, 31, 211, 255, 1) != 0) ok = -1;

    GLuint rgb = make_rgb_alignment_texture();
    GLuint rgb_fbo = 0;
    if (!rgb || make_upload_fbo(name, 3, 2, rgb, &rgb_fbo) != 0) return -1;
    if (gr_expect_pixel(name, "rgb_alignment_row0", 2, 0, 41, 43, 45, 255, 1) != 0) ok = -1;
    if (gr_expect_pixel(name, "rgb_alignment_row1", 1, 1, 81, 83, 85, 255, 1) != 0) ok = -1;

    if (has_ext("GL_EXT_texture_format_BGRA8888") || has_ext("GL_APPLE_texture_format_BGRA8888")) {
        drain_gl_errors();
        GLuint bgra = make_bgra_texture();
        GLenum err = glGetError();
        GLuint bgra_fbo = 0;
        if (!bgra || err != 0 || make_upload_fbo(name, 1, 1, bgra, &bgra_fbo) != 0) {
            printf("%s bgra_upload failed tex=%u err=0x%x\n", name, bgra, err);
            return -1;
        }
        if (gr_expect_pixel(name, "bgra_ext_swizzle", 0, 0, 233, 71, 13, 191, 1) != 0) ok = -1;
    } else {
        printf("%s bgra_ext=missing skip_bgra_upload\n", name);
        drain_gl_errors();
    }

    return ok;
}

static int check_premul_sample_blend(const char *name, GLuint program, GLuint vbo) {
    (void)vbo;
    uint8_t premul[] = {64, 16, 0, 128};
    GLuint src = gr_make_rgba_texture(1, 1, premul);
    GLuint dst_tex = 0;
    GLuint dst_fbo = 0;
    if (!src || gr_make_fbo(name, 32, 32, &dst_tex, &dst_fbo) != 0) return -1;

    glBindFramebuffer(GL_FRAMEBUFFER, dst_fbo);
    glViewport(0, 0, 32, 32);
    glClearColor(20.0f / 255.0f, 60.0f / 255.0f, 100.0f / 255.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(program);
    if (gr_bind_quad(name, program) != 0) return -1;
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, src);
    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_BLEND);
    glFinish();

    return gr_expect_pixel(name, "sampled_premul_blend", 16, 16, 74, 46, 50, 255, 3);
}

int main(void) {
    const char *name = "gui_egl_texture_formats_fbo_readback";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 160, 120, 3);
    if (r != 0) {
        printf("FAIL %s open_window step=%d\n", name, r);
        return r;
    }

    GLuint program = 0;
    GLuint vbo = 0;
    if (gr_make_program(name, GR_FS_TEX, &program) != 0 || gr_make_quad(&vbo) != 0) {
        printf("FAIL %s setup\n", name);
        gr_close_window(&gw);
        return 9;
    }

    int ok = 0;
    if (check_texture_pixels(name) != 0) ok = -1;
    if (check_premul_sample_blend(name, program, vbo) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.04f, 0.06f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("FAIL %s assertions\n", name);
        return 10;
    }
    printf("PASS %s configure=%u egl=%d.%d rgba_npot=5x3 subimage=2x1 rgb_alignment=1 row_length=1 fbo_readback=1 premul_blend=1\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor);
    return 0;
}
