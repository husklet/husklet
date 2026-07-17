#include "gui_egl_render_probe.h"

#include <stddef.h>

enum {
    SURF_W = 514,
    SURF_H = 257,
    ATLAS_W = 8,
    ATLAS_H = 8,
};

struct coverage_vertex {
    GLfloat pos[2];
    uint8_t color[4];
    uint16_t uv[2];
    uint16_t coverage;
    uint16_t pad;
};

static const char *COV_VS =
    "attribute vec2 aPos;\n"
    "attribute vec4 aColor;\n"
    "attribute vec2 aUV;\n"
    "attribute float aCoverage;\n"
    "varying vec2 vUV;\n"
    "varying vec4 vColor;\n"
    "varying float vCoverage;\n"
    "void main() {\n"
    "  gl_Position = vec4(aPos, 0.0, 1.0);\n"
    "  vUV = aUV;\n"
    "  vColor = aColor;\n"
    "  vCoverage = aCoverage;\n"
    "}\n";

static const char *COV_FS =
    "precision mediump float;\n"
    "uniform sampler2D uAtlas;\n"
    "varying vec2 vUV;\n"
    "varying vec4 vColor;\n"
    "varying float vCoverage;\n"
    "void main() {\n"
    "  float mask = texture2D(uAtlas, vUV).a * vCoverage;\n"
    "  gl_FragColor = vec4(vColor.rgb * mask, vColor.a * mask);\n"
    "}\n";

static GLfloat ndc_x(int x) {
    return (2.0f * (GLfloat)x / (GLfloat)SURF_W) - 1.0f;
}

static GLfloat ndc_y(int y) {
    return (2.0f * (GLfloat)y / (GLfloat)SURF_H) - 1.0f;
}

static int make_custom_program(const char *name, const char *vs_src, const char *fs_src, GLuint *out) {
    GLuint vs = 0;
    GLuint fs = 0;
    if (gr_compile_shader(GL_VERTEX_SHADER, vs_src, name, &vs) != 0) return -1;
    if (gr_compile_shader(GL_FRAGMENT_SHADER, fs_src, name, &fs) != 0) return -1;

    GLuint program = glCreateProgram();
    if (!program) {
        printf("%s glCreateProgram=0\n", name);
        return -1;
    }
    glAttachShader(program, vs);
    glAttachShader(program, fs);
    glLinkProgram(program);
    GLint linked = 0;
    glGetProgramiv(program, GL_LINK_STATUS, &linked);
    if (!linked) {
        printf("%s glLinkProgram=0\n", name);
        return -1;
    }
    *out = program;
    return 0;
}

static GLuint upload_atlas(void) {
    uint8_t atlas[ATLAS_W * ATLAS_H * 4];
    for (int y = 0; y < ATLAS_H; y++) {
        for (int x = 0; x < ATLAS_W; x++) {
            int i = (y * ATLAS_W + x) * 4;
            int hard_box = (x >= 2 && x <= 5 && y >= 2 && y <= 5);
            int diagonal = (x + y == 7 || x == y);
            atlas[i + 0] = 255;
            atlas[i + 1] = 255;
            atlas[i + 2] = 255;
            atlas[i + 3] = (uint8_t)(hard_box ? 255 : (diagonal ? 96 : 0));
        }
    }

    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, ATLAS_W, ATLAS_H, 0, GL_RGBA, GL_UNSIGNED_BYTE, atlas);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    gr_texture_params();
    return tex;
}

static int make_strip(GLuint *vbo, GLuint *ebo) {
    const GLfloat vertices[] = {
        ndc_x(28),  ndc_y(88),  0.0f, 0.0f,
        ndc_x(28),  ndc_y(170), 0.0f, 1.0f,
        ndc_x(486), ndc_y(170), 1.0f, 1.0f,
        ndc_x(486), ndc_y(88),  1.0f, 0.0f,
    };
    const uint16_t indices[] = {0, 1, 2, 0, 2, 3};

    glGenBuffers(1, vbo);
    glBindBuffer(GL_ARRAY_BUFFER, *vbo);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(vertices), vertices, GL_STATIC_DRAW);
    glGenBuffers(1, ebo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, *ebo);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, (GLsizeiptr)sizeof(indices), indices, GL_STATIC_DRAW);
    return (*vbo && *ebo) ? 0 : -1;
}

static void set_cov_vertex(struct coverage_vertex *v, int x, int y, uint8_t r, uint8_t g, uint8_t b,
                           uint16_t u, uint16_t t, uint16_t coverage) {
    v->pos[0] = ndc_x(x);
    v->pos[1] = ndc_y(y);
    v->color[0] = r;
    v->color[1] = g;
    v->color[2] = b;
    v->color[3] = 255;
    v->uv[0] = u;
    v->uv[1] = t;
    v->coverage = coverage;
    v->pad = 0;
}

static int make_coverage_geometry(GLuint *vbo, GLuint *ebo) {
    struct coverage_vertex vertices[8];
    const uint16_t indices[] = {
        0, 1, 2, 0, 2, 3,
        4, 5, 6, 4, 6, 7,
    };

    set_cov_vertex(&vertices[0], 84,  78, 24,  90, 190, 0,     0,     65535);
    set_cov_vertex(&vertices[1], 84,  214, 24,  90, 190, 0,     65535, 65535);
    set_cov_vertex(&vertices[2], 220, 214, 24,  90, 190, 65535, 65535, 65535);
    set_cov_vertex(&vertices[3], 220, 78,  24,  90, 190, 65535, 0,     65535);

    set_cov_vertex(&vertices[4], 278, 78,  240, 120, 32,  0,     0,     32768);
    set_cov_vertex(&vertices[5], 278, 214, 240, 120, 32,  0,     65535, 32768);
    set_cov_vertex(&vertices[6], 434, 214, 240, 120, 32,  65535, 65535, 32768);
    set_cov_vertex(&vertices[7], 434, 78,  240, 120, 32,  65535, 0,     32768);

    glGenBuffers(1, vbo);
    glBindBuffer(GL_ARRAY_BUFFER, *vbo);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(vertices), vertices, GL_STATIC_DRAW);
    glGenBuffers(1, ebo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, *ebo);
    glBufferData(GL_ELEMENT_ARRAY_BUFFER, (GLsizeiptr)sizeof(indices), indices, GL_STATIC_DRAW);
    return (*vbo && *ebo) ? 0 : -1;
}

static int make_present_quad(GLuint *vbo) {
    const GLfloat vertices[] = {
        -1.0f, -1.0f, 0.0f, 0.0f,
        -1.0f,  1.0f, 0.0f, 1.0f,
         1.0f, -1.0f, 1.0f, 0.0f,
         1.0f, -1.0f, 1.0f, 0.0f,
        -1.0f,  1.0f, 0.0f, 1.0f,
         1.0f,  1.0f, 1.0f, 1.0f,
    };
    glGenBuffers(1, vbo);
    glBindBuffer(GL_ARRAY_BUFFER, *vbo);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(vertices), vertices, GL_STATIC_DRAW);
    return *vbo ? 0 : -1;
}

static int bind_coverage_attrs(const char *name, GLuint program) {
    GLint a_pos = glGetAttribLocation(program, "aPos");
    GLint a_color = glGetAttribLocation(program, "aColor");
    GLint a_uv = glGetAttribLocation(program, "aUV");
    GLint a_coverage = glGetAttribLocation(program, "aCoverage");
    if (a_pos < 0 || a_color < 0 || a_uv < 0 || a_coverage < 0) {
        printf("%s attribs pos=%d color=%d uv=%d coverage=%d\n", name, a_pos, a_color, a_uv, a_coverage);
        return -1;
    }

    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, sizeof(struct coverage_vertex),
                          (void *)offsetof(struct coverage_vertex, pos));
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_color, 4, GL_UNSIGNED_BYTE, GL_TRUE, sizeof(struct coverage_vertex),
                          (void *)offsetof(struct coverage_vertex, color));
    glEnableVertexAttribArray((GLuint)a_color);
    glVertexAttribPointer((GLuint)a_uv, 2, GL_UNSIGNED_SHORT, GL_TRUE, sizeof(struct coverage_vertex),
                          (void *)offsetof(struct coverage_vertex, uv));
    glEnableVertexAttribArray((GLuint)a_uv);
    glVertexAttribPointer((GLuint)a_coverage, 1, GL_UNSIGNED_SHORT, GL_TRUE, sizeof(struct coverage_vertex),
                          (void *)offsetof(struct coverage_vertex, coverage));
    glEnableVertexAttribArray((GLuint)a_coverage);
    return 0;
}

static int draw_textured_surface(const char *name, GLuint program, GLuint texture, GLuint present_vbo) {
    glUseProgram(program);
    glBindBuffer(GL_ARRAY_BUFFER, present_vbo);
    if (gr_bind_quad(name, program) != 0) return -1;
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, texture);
    glUniform1i(glGetUniformLocation(program, "uTex"), 0);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    return 0;
}

int main(void) {
    const char *name = "chrome_coverage_path";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, SURF_W, SURF_H, 2);
    if (r != 0) return r;

    GLuint solid = 0;
    GLuint coverage = 0;
    GLuint textured = 0;
    GLuint strip_vbo = 0, strip_ebo = 0;
    GLuint cov_vbo = 0, cov_ebo = 0;
    GLuint present_vbo = 0;
    GLuint atlas = 0;
    GLuint fbo_tex = 0, fbo = 0;
    GLuint check_tex = 0, check_fbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &solid) != 0 ||
        make_custom_program(name, COV_VS, COV_FS, &coverage) != 0 ||
        gr_make_program(name, GR_FS_TEX, &textured) != 0 ||
        make_strip(&strip_vbo, &strip_ebo) != 0 ||
        make_coverage_geometry(&cov_vbo, &cov_ebo) != 0 ||
        make_present_quad(&present_vbo) != 0 ||
        gr_make_fbo(name, SURF_W, SURF_H, &fbo_tex, &fbo) != 0 ||
        gr_make_fbo(name, SURF_W, SURF_H, &check_tex, &check_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }

    atlas = upload_atlas();
    if (!atlas) {
        printf("%s atlas=0\n", name);
        gr_close_window(&gw);
        return 10;
    }

    gr_clear_rgba(fbo, SURF_W, SURF_H, 230.0f / 255.0f, 235.0f / 255.0f, 218.0f / 255.0f, 1.0f);

    glUseProgram(solid);
    glBindBuffer(GL_ARRAY_BUFFER, strip_vbo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, strip_ebo);
    if (gr_bind_quad(name, solid) != 0) {
        gr_close_window(&gw);
        return 11;
    }
    glUniform4f(glGetUniformLocation(solid, "uColor"), 46.0f / 255.0f, 64.0f / 255.0f, 78.0f / 255.0f, 1.0f);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (void *)0);

    glUseProgram(coverage);
    glBindBuffer(GL_ARRAY_BUFFER, cov_vbo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, cov_ebo);
    if (bind_coverage_attrs(name, coverage) != 0) {
        gr_close_window(&gw);
        return 12;
    }
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, atlas);
    glUniform1i(glGetUniformLocation(coverage, "uAtlas"), 0);
    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    glDrawElements(GL_TRIANGLES, 12, GL_UNSIGNED_SHORT, (void *)0);
    glDisable(GL_BLEND);
    glFinish();

    int ok = 0;
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    if (gr_expect_pixel(name, "offscreen_bg", 20, 20, 230, 235, 218, 255, 3) != 0) ok = -1;
    if (gr_expect_pixel(name, "offscreen_strip", 250, 120, 46, 64, 78, 255, 3) != 0) ok = -1;
    if (gr_expect_pixel(name, "offscreen_glyph_full", 152, 146, 24, 90, 190, 255, 5) != 0) ok = -1;
    if (gr_expect_pixel(name, "offscreen_glyph_half", 356, 146, 143, 92, 55, 255, 6) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, check_fbo);
    glViewport(0, 0, SURF_W, SURF_H);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (draw_textured_surface(name, textured, fbo_tex, present_vbo) != 0) ok = -1;
    glFinish();
    if (gr_expect_pixel(name, "sampled_fbo_glyph", 152, 146, 24, 90, 190, 255, 5) != 0) ok = -1;
    if (gr_expect_pixel(name, "sampled_fbo_half", 356, 146, 143, 92, 55, 255, 6) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.03f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (draw_textured_surface(name, textured, fbo_tex, present_vbo) != 0) ok = -1;
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 13;
    printf("%s configure=%u egl=%d.%d coverage_path=1 offscreen_rgba=%dx%d atlas=%dx%d indexed_tris=4 attrs=float2,ubyte4_norm,u16x2_norm,u16_norm sampled_fbo=1 sampled_default=1 strip=1 vbo=%u ebo=%u tex=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, SURF_W, SURF_H, ATLAS_W, ATLAS_H,
           cov_vbo != 0 && strip_vbo != 0 && present_vbo != 0, cov_ebo != 0 && strip_ebo != 0,
           fbo_tex != 0 && atlas != 0);
    return 0;
}
