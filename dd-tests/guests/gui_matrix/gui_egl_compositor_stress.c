#include "gui_egl_probe.h"

static const char *VS =
    "attribute vec2 aPos;\n"
    "attribute vec2 aUV;\n"
    "varying vec2 vUV;\n"
    "void main() { gl_Position = vec4(aPos, 0.0, 1.0); vUV = aUV; }\n";

static const char *FS_RGBA =
    "precision mediump float;\n"
    "uniform sampler2D uTex;\n"
    "varying vec2 vUV;\n"
    "void main() { gl_FragColor = texture2D(uTex, vUV); }\n";

static const char *FS_GLYPH =
    "precision mediump float;\n"
    "uniform sampler2D uGlyph;\n"
    "uniform vec4 uColor;\n"
    "varying vec2 vUV;\n"
    "void main() { float a = texture2D(uGlyph, vUV).r; gl_FragColor = uColor * a; }\n";

static int compile_shader(GLenum type, const char *source, const char *name, GLuint *out) {
    GLuint shader = glCreateShader(type);
    if (!shader) {
        printf("%s glCreateShader=0\n", name);
        return -1;
    }
    glShaderSource(shader, 1, &source, NULL);
    glCompileShader(shader);
    GLint ok = 0;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        printf("%s glCompileShader=0\n", name);
        return -1;
    }
    *out = shader;
    return 0;
}

static int make_program(const char *name, const char *fs, GLuint *out) {
    GLuint vs = 0;
    GLuint frag = 0;
    if (compile_shader(GL_VERTEX_SHADER, VS, name, &vs) != 0) return -1;
    if (compile_shader(GL_FRAGMENT_SHADER, fs, name, &frag) != 0) return -1;

    GLuint program = glCreateProgram();
    if (!program) {
        printf("%s glCreateProgram=0\n", name);
        return -1;
    }
    glAttachShader(program, vs);
    glAttachShader(program, frag);
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

static void fill_premul_texture(uint8_t tex[16 * 16 * 4], int phase) {
    for (int y = 0; y < 16; y++) {
        for (int x = 0; x < 16; x++) {
            int i = (y * 16 + x) * 4;
            int a = 64 + ((x * 9 + y * 5 + phase * 37) & 127);
            int r = phase ? 32 : 220;
            int g = phase ? 210 : 70;
            int b = phase ? 240 : 30;
            tex[i + 0] = (uint8_t)(r * a / 255);
            tex[i + 1] = (uint8_t)(g * a / 255);
            tex[i + 2] = (uint8_t)(b * a / 255);
            tex[i + 3] = (uint8_t)a;
        }
    }
}

static void fill_glyph_atlas(uint8_t atlas[32 * 16]) {
    for (int y = 0; y < 16; y++) {
        for (int x = 0; x < 32; x++) {
            int stroke = (x == 2 || x == 15 || y == 2 || y == 13 ||
                          (x > 17 && x < 30 && (x + y) % 7 < 3));
            int soft = stroke ? 255 : ((x * 13 + y * 17) & 63);
            atlas[y * 32 + x] = (uint8_t)soft;
        }
    }
}

static GLuint upload_rgba_texture(int phase) {
    uint8_t tex[16 * 16 * 4];
    fill_premul_texture(tex, phase);
    GLuint texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 16, 16, 0, GL_RGBA, GL_UNSIGNED_BYTE, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    return texture;
}

static GLuint upload_glyph_texture(void) {
    uint8_t atlas[32 * 16];
    fill_glyph_atlas(atlas);
    GLuint texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_R8, 32, 16, 0, GL_RED, GL_UNSIGNED_BYTE, atlas);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    return texture;
}

static void draw_quad(GLint a_pos, GLint a_uv, GLint first) {
    glVertexAttribPointer((GLuint)a_pos, 2, GL_FLOAT, GL_FALSE, 16, (void *)0);
    glEnableVertexAttribArray((GLuint)a_pos);
    glVertexAttribPointer((GLuint)a_uv, 2, GL_FLOAT, GL_FALSE, 16, (void *)8);
    glEnableVertexAttribArray((GLuint)a_uv);
    glDrawArrays(GL_TRIANGLES, first, 6);
}

int main(void) {
    const char *name = "gui_egl_compositor_stress";
    struct gp_conn c;
    struct gp_events ev;
    int r = ge_xdg_connect(&c, &ev, name);
    if (r != 0) {
        printf("%s xdg_configure=0 step=%d\n", name, r);
        return r;
    }

    struct ge_egl egl;
    r = ge_egl_init_version(&egl, name, 3);
    if (r != 0) return r;

    void *window = wl_egl_window_create((void *)(uintptr_t)GP_SURFACE, 256, 160);
    if (!window) {
        printf("%s wl_egl_window_create=0\n", name);
        ge_egl_fini(&egl);
        return 7;
    }
    EGLSurface surface = ge_create_surface(&egl, window, name);
    if (!surface) {
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 8;
    }

    GLuint rgba_program = 0;
    GLuint glyph_program = 0;
    if (make_program(name, FS_RGBA, &rgba_program) != 0 ||
        make_program(name, FS_GLYPH, &glyph_program) != 0) {
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 9;
    }

    glActiveTexture(GL_TEXTURE0);
    GLuint tex_a = upload_rgba_texture(0);
    GLuint tex_b = upload_rgba_texture(1);
    GLuint glyph = upload_glyph_texture();

    float vertices[] = {
        -0.94f,  0.78f, 0.0f, 0.0f,
        -0.94f, -0.12f, 0.0f, 1.0f,
        -0.18f,  0.78f, 1.0f, 0.0f,
        -0.18f,  0.78f, 1.0f, 0.0f,
        -0.94f, -0.12f, 0.0f, 1.0f,
        -0.18f, -0.12f, 1.0f, 1.0f,

        -0.36f,  0.38f, 0.0f, 0.0f,
        -0.36f, -0.78f, 0.0f, 1.0f,
         0.50f,  0.38f, 1.0f, 0.0f,
         0.50f,  0.38f, 1.0f, 0.0f,
        -0.36f, -0.78f, 0.0f, 1.0f,
         0.50f, -0.78f, 1.0f, 1.0f,

         0.02f,  0.86f, 0.0f, 0.0f,
         0.02f, -0.02f, 0.0f, 1.0f,
         0.94f,  0.86f, 1.0f, 0.0f,
         0.94f,  0.86f, 1.0f, 0.0f,
         0.02f, -0.02f, 0.0f, 1.0f,
         0.94f, -0.02f, 1.0f, 1.0f,
    };
    GLuint vbo = 0;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);

    GLint rgba_pos = glGetAttribLocation(rgba_program, "aPos");
    GLint rgba_uv = glGetAttribLocation(rgba_program, "aUV");
    GLint glyph_pos = glGetAttribLocation(glyph_program, "aPos");
    GLint glyph_uv = glGetAttribLocation(glyph_program, "aUV");
    if (rgba_pos < 0 || rgba_uv < 0 || glyph_pos < 0 || glyph_uv < 0) {
        printf("%s attribs rgba=%d/%d glyph=%d/%d\n", name, rgba_pos, rgba_uv, glyph_pos, glyph_uv);
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 10;
    }

    GLint rgba_sampler = glGetUniformLocation(rgba_program, "uTex");
    GLint glyph_sampler = glGetUniformLocation(glyph_program, "uGlyph");
    GLint glyph_color = glGetUniformLocation(glyph_program, "uColor");
    glViewport(0, 0, 256, 160);
    glClearColor(0.02f, 0.025f, 0.035f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    glEnable(GL_SCISSOR_TEST);

    glUseProgram(rgba_program);
    glUniform1i(rgba_sampler, 0);
    glScissor(8, 18, 112, 124);
    glBindTexture(GL_TEXTURE_2D, tex_a);
    draw_quad(rgba_pos, rgba_uv, 0);

    glScissor(68, 8, 118, 112);
    glBindTexture(GL_TEXTURE_2D, tex_b);
    draw_quad(rgba_pos, rgba_uv, 6);

    glUseProgram(glyph_program);
    glUniform1i(glyph_sampler, 0);
    glUniform4f(glyph_color, 0.10f, 0.62f, 0.82f, 0.82f);
    glScissor(132, 70, 112, 78);
    glBindTexture(GL_TEXTURE_2D, glyph);
    draw_quad(glyph_pos, glyph_uv, 12);

    if (!eglSwapBuffers(egl.display, surface)) {
        printf("%s swap=0 err=0x%x\n", name, eglGetError());
        wl_egl_window_destroy(window);
        ge_egl_fini(&egl);
        return 11;
    }

    glDisable(GL_SCISSOR_TEST);
    wl_egl_window_destroy(window);
    ge_egl_fini(&egl);
    printf("%s configure=%u egl=%d.%d programs=2 textures=3 r8_glyph=32x16 draws=3 scissor=3 premul_blend=1 swaps=1\n",
           name, ev.xdg_configure_serial, egl.major, egl.minor);
    return 0;
}
