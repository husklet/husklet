#include "gui_egl_render_probe.h"

typedef uint64_t GLuint64;
typedef void *GLsync;

#define GL_SYNC_GPU_COMMANDS_COMPLETE 0x9117
#define GL_ALREADY_SIGNALED 0x911A
#define GL_CONDITION_SATISFIED 0x911C
#define GL_SYNC_FLUSH_COMMANDS_BIT 0x00000001

extern void glFlush(void);
extern void (*eglGetProcAddress(const char *procname))(void);

typedef GLsync (*PFNGLFENCESYNCPROC)(GLenum, GLbitfield);
typedef GLenum (*PFNGLCLIENTWAITSYNCPROC)(GLsync, GLbitfield, GLuint64);
typedef void (*PFNGLDELETESYNCPROC)(GLsync);

static int wait_optional_fence(const char *name, int frame) {
    PFNGLFENCESYNCPROC fence_sync =
        (PFNGLFENCESYNCPROC)(void *)eglGetProcAddress("glFenceSync");
    PFNGLCLIENTWAITSYNCPROC client_wait_sync =
        (PFNGLCLIENTWAITSYNCPROC)(void *)eglGetProcAddress("glClientWaitSync");
    PFNGLDELETESYNCPROC delete_sync =
        (PFNGLDELETESYNCPROC)(void *)eglGetProcAddress("glDeleteSync");
    if (!fence_sync || !client_wait_sync || !delete_sync) return 0;

    GLsync sync = fence_sync(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);
    if (!sync) {
        printf("%s frame_%d fence=0\n", name, frame);
        return -1;
    }
    GLenum wait = client_wait_sync(sync, GL_SYNC_FLUSH_COMMANDS_BIT, 1000000000ull);
    delete_sync(sync);
    if (wait != GL_ALREADY_SIGNALED && wait != GL_CONDITION_SATISFIED) {
        printf("%s frame_%d fence_wait=0x%x\n", name, frame, wait);
        return -1;
    }
    return 0;
}

static void draw_frame(GLuint program, int width, int height, int frame) {
    static const float bg[][3] = {
        {0.03f, 0.04f, 0.08f},
        {0.11f, 0.05f, 0.02f},
        {0.02f, 0.10f, 0.07f},
        {0.09f, 0.03f, 0.12f},
    };
    int c = frame & 3;
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, width, height);
    glDisable(GL_SCISSOR_TEST);
    glClearColor(bg[c][0], bg[c][1], bg[c][2], 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    glUseProgram(program);
    glEnable(GL_SCISSOR_TEST);
    glScissor(20 + frame * 3, 18, 70, 48);
    glUniform4f(glGetUniformLocation(program, "uColor"), 0.82f, 0.18f, 0.06f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glScissor(width - 70, height - 54, 44, 34);
    glUniform4f(glGetUniformLocation(program, "uColor"), 0.06f, 0.58f, 0.86f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_SCISSOR_TEST);
}

int main(void) {
    const char *name = "gui_egl_swap_lifecycle_repeated";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 180, 120, 2);
    if (r != 0) return r;

    GLuint solid = 0;
    GLuint vbo = 0;
    if (gr_make_program(name, GR_FS_SOLID, &solid) != 0 || gr_make_quad(&vbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    glUseProgram(solid);
    if (gr_bind_quad(name, solid) != 0) {
        gr_close_window(&gw);
        return 10;
    }

    int ok = 0;
    for (int frame = 0; frame < 8; frame++) {
        draw_frame(solid, gw.width, gw.height, frame);
        glFlush();
        if ((frame & 1) == 0 && wait_optional_fence(name, frame) != 0) ok = -1;
        if ((frame & 1) != 0) glFinish();
        if (gr_swap(&gw) != 0) ok = -1;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glUseProgram(solid);
    glEnable(GL_SCISSOR_TEST);
    glScissor(72, 44, 36, 24);
    glUniform4f(glGetUniformLocation(solid, "uColor"), 0.09f, 0.83f, 0.32f, 1.0f);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    glDisable(GL_SCISSOR_TEST);
    glFinish();
    if (gr_expect_pixel(name, "partial_redraw_scissor", 80, 52, 23, 212, 82, 255, 5) != 0) ok = -1;
    if (gr_swap(&gw) != 0) ok = -1;

    draw_frame(solid, gw.width, gw.height, 8);
    glFinish();
    if (gr_expect_pixel(name, "final_left_scissor", 52, 42, 209, 46, 15, 255, 5) != 0) ok = -1;
    if (gr_expect_pixel(name, "final_right_scissor", 124, 82, 15, 148, 219, 255, 5) != 0) ok = -1;
    if (gr_expect_pixel(name, "final_background", 8, 8, 8, 10, 20, 255, 4) != 0) ok = -1;
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) return 11;
    printf("%s configure=%u egl=%d.%d swaps=10 flush=1 finish=1 optional_fence=1 scissor_frames=10 partial_redraw=1 final_pixels=4 vbo=%u\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, vbo != 0);
    return 0;
}
