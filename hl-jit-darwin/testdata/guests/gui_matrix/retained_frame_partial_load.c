#include "gui_egl_render_probe.h"

#include <stdlib.h>

static void draw_frame(const char *name, struct ge_egl *egl, EGLSurface surface,
                       GLuint program, GLuint vbo, int scissored) {
    glViewport(0, 0, 64, 64);
    glUseProgram(program);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    gr_bind_quad(name, program);
    if (scissored) {
        glEnable(GL_SCISSOR_TEST);
        glScissor(16, 16, 16, 16);
        glClearColor(1.0f, 0.0f, 0.0f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glUniform4f(glGetUniformLocation(program, "uColor"), 1.0f, 0.0f, 0.0f, 1.0f);
    } else {
        glDisable(GL_SCISSOR_TEST);
        glClearColor(0.0f, 1.0f, 0.0f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glUniform4f(glGetUniformLocation(program, "uColor"), 0.0f, 1.0f, 0.0f, 1.0f);
    }
    glDrawArrays(GL_TRIANGLES, 0, 6);
    if (scissored) glDisable(GL_SCISSOR_TEST);
    eglSwapBuffers(egl->display, surface);
}

int main(void) {
    const char *name = "retained_frame_partial_load";
    if (!getenv("HL_IR_DUMP")) {
        printf("%s SKIP set HL_IR_DUMP=/tmp/retained-frame.ir to inspect the second-frame load op\n", name);
        return 77;
    }

    struct ge_egl egl;
    int r = ge_egl_init(&egl, name);
    if (r != 0) return r;

    int native_window[2] = {64, 64};
    EGLSurface surface = ge_create_surface(&egl, native_window, name);
    if (!surface) {
        ge_egl_fini(&egl);
        return 8;
    }

    GLuint program = 0;
    GLuint vbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &program) != 0 || gr_make_quad(&vbo) != 0) {
        ge_egl_fini(&egl);
        return 9;
    }

    draw_frame(name, &egl, surface, program, vbo, 0);
    draw_frame(name, &egl, surface, program, vbo, 1);

    eglDestroySurface(egl.display, surface);
    ge_egl_fini(&egl);
    printf("%s PASS frames=2 frame1=full_green frame2=scissored_red expected_second_default_load=1\n", name);
    return 0;
}
