#include "gui_egl_render_probe.h"

#include <stddef.h>

extern void glVertexAttribIPointer(GLuint, GLint, GLenum, GLsizei, const void *);

struct vertex {
    GLfloat pos[2];
    uint8_t color[4];
    uint16_t texel[2];
};

static const char *VF_VS =
    "#version 300 es\n"
    "precision highp float;\n"
    "precision highp int;\n"
    "in vec2 aPos;\n"
    "in vec4 aColor;\n"
    "in uvec2 aTexel;\n"
    "out vec4 vColor;\n"
    "void main() {\n"
    "  float gate = (aTexel.x == 257u && aTexel.y == 514u) ? 1.0 : 0.0;\n"
    "  gl_Position = vec4(aPos, 0.0, 1.0);\n"
    "  vColor = aColor * gate;\n"
    "}\n";

static const char *VF_FS =
    "#version 300 es\n"
    "precision highp float;\n"
    "in vec4 vColor;\n"
    "out vec4 fragColor;\n"
    "void main() { fragColor = vColor; }\n";

int main(void) {
    const char *name = "gui_egl_vertex_formats";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 128, 96, 3);
    if (r != 0) return r;

    GLuint program = 0;
    GLuint vs = 0;
    GLuint fs = 0;
    if (gr_compile_shader(GL_VERTEX_SHADER, VF_VS, name, &vs) != 0 ||
        gr_compile_shader(GL_FRAGMENT_SHADER, VF_FS, name, &fs) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    program = glCreateProgram();
    glAttachShader(program, vs);
    glAttachShader(program, fs);
    glLinkProgram(program);
    GLint linked = 0;
    glGetProgramiv(program, GL_LINK_STATUS, &linked);
    if (!linked) {
        printf("%s glLinkProgram=0\n", name);
        gr_close_window(&gw);
        return 10;
    }

    GLuint tex = 0;
    GLuint fbo = 0;
    if (gr_make_fbo(name, 64, 64, &tex, &fbo) != 0) {
        gr_close_window(&gw);
        return 11;
    }

    const struct vertex vertices[] = {
        {{-1.0f, -1.0f}, {32, 128, 224, 255}, {257, 514}},
        {{ 3.0f, -1.0f}, {32, 128, 224, 255}, {257, 514}},
        {{-1.0f,  3.0f}, {32, 128, 224, 255}, {257, 514}},
    };
    GLuint vbo = 0;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(vertices), vertices, GL_STATIC_DRAW);

    glUseProgram(program);
    GLint a_pos = glGetAttribLocation(program, "aPos");
    GLint a_color = glGetAttribLocation(program, "aColor");
    GLint a_texel = glGetAttribLocation(program, "aTexel");
    if (a_pos < 0 || a_color < 0 || a_texel < 0) {
        printf("%s attribs pos=%d color=%d texel=%d\n", name, a_pos, a_color, a_texel);
        gr_close_window(&gw);
        return 12;
    }

    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, sizeof(struct vertex),
                          (void *)offsetof(struct vertex, pos));
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_color, 4, GL_UNSIGNED_BYTE, GL_TRUE, sizeof(struct vertex),
                          (void *)offsetof(struct vertex, color));
    glEnableVertexAttribArray((GLuint)a_color);
    glVertexAttribIPointer((GLuint)a_texel, 2, GL_UNSIGNED_SHORT, sizeof(struct vertex),
                           (void *)offsetof(struct vertex, texel));
    glEnableVertexAttribArray((GLuint)a_texel);

    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glViewport(0, 0, 64, 64);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    glFinish();

    int ok = gr_expect_pixel(name, "ubyte_norm_color_u16_integer_texel", 32, 32, 32, 128, 224, 255, 4);

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.04f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 13;
    printf("%s configure=%u egl=%d.%d vbo=%u fbo_tex=%u attrs=float2,ubyte4_norm,u16x2_integer\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, vbo != 0, tex != 0);
    return 0;
}
