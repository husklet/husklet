#include "gui_egl_render_probe.h"

/*
 * Renderbuffer probe for Chrome FBO paths:
 * - attach a single-sample RGBA8 renderbuffer and read it back directly.
 * - attach a multisample RGBA8 renderbuffer, clear it, then resolve with
 *   glBlitFramebuffer into a texture-backed FBO and read that pixel back.
 */

#define GL_RENDERBUFFER 0x8D41
#define GL_RGBA8 0x8058
#define GL_MAX_SAMPLES 0x8D57

extern void glGenRenderbuffers(GLsizei, GLuint *);
extern void glBindRenderbuffer(GLenum, GLuint);
extern void glRenderbufferStorage(GLenum, GLenum, GLsizei, GLsizei);
extern void glRenderbufferStorageMultisample(GLenum, GLsizei, GLenum, GLsizei, GLsizei);
extern void glFramebufferRenderbuffer(GLenum, GLenum, GLenum, GLuint);
extern void glGetIntegerv(GLenum, GLint *);

static int make_renderbuffer_fbo(const char *name, int width, int height,
                                 GLsizei samples, GLuint *rbo, GLuint *fbo) {
    *rbo = 0;
    *fbo = 0;
    glGenRenderbuffers(1, rbo);
    glBindRenderbuffer(GL_RENDERBUFFER, *rbo);
    if (samples > 1) {
        glRenderbufferStorageMultisample(GL_RENDERBUFFER, samples, GL_RGBA8, width, height);
    } else {
        glRenderbufferStorage(GL_RENDERBUFFER, GL_RGBA8, width, height);
    }
    GLenum err = glGetError();
    if (err != 0) {
        printf("%s renderbuffer_storage err=0x%x samples=%d\n", name, err, samples);
        return -1;
    }

    glGenFramebuffers(1, fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, *fbo);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, *rbo);
    GLenum status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (status != GL_FRAMEBUFFER_COMPLETE) {
        printf("%s renderbuffer_fbo incomplete status=0x%x samples=%d rbo=%u fbo=%u\n",
               name, status, samples, *rbo, *fbo);
        return -1;
    }
    return 0;
}

int main(void) {
    const char *name = "gui_egl_renderbuffer_msaa_resolve";
    struct gr_window gw;
    int r = gr_open_window(&gw, name, 96, 64, 3);
    if (r != 0) return r;

    GLuint rbo = 0;
    GLuint rbo_fbo = 0;
    if (make_renderbuffer_fbo(name, 16, 16, 1, &rbo, &rbo_fbo) != 0) {
        gr_close_window(&gw);
        return 9;
    }
    glBindFramebuffer(GL_FRAMEBUFFER, rbo_fbo);
    glViewport(0, 0, 16, 16);
    glClearColor(32.0f / 255.0f, 88.0f / 255.0f, 210.0f / 255.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();

    int ok = 0;
    if (gr_expect_pixel(name, "renderbuffer_clear_readback", 8, 8, 32, 88, 210, 255, 4) != 0) ok = -1;

    GLint max_samples = 0;
    glGetIntegerv(GL_MAX_SAMPLES, &max_samples);
    GLsizei samples = max_samples >= 4 ? 4 : (max_samples >= 2 ? 2 : 0);
    if (samples < 2) {
        printf("%s max_samples=%d too_low\n", name, max_samples);
        gr_close_window(&gw);
        return 10;
    }

    GLuint msaa_rbo = 0;
    GLuint msaa_fbo = 0;
    GLuint resolve_tex = 0;
    GLuint resolve_fbo = 0;
    if (make_renderbuffer_fbo(name, 16, 16, samples, &msaa_rbo, &msaa_fbo) != 0 ||
        gr_make_fbo(name, 16, 16, &resolve_tex, &resolve_fbo) != 0) {
        gr_close_window(&gw);
        return 11;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, msaa_fbo);
    glViewport(0, 0, 16, 16);
    glClearColor(180.0f / 255.0f, 46.0f / 255.0f, 74.0f / 255.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, msaa_fbo);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, resolve_fbo);
    glBlitFramebuffer(0, 0, 16, 16, 0, 0, 16, 16, GL_COLOR_BUFFER_BIT, GL_NEAREST);
    glFinish();

    glBindFramebuffer(GL_FRAMEBUFFER, resolve_fbo);
    if (gr_expect_pixel(name, "msaa_renderbuffer_resolve", 8, 8, 180, 46, 74, 255, 4) != 0) ok = -1;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, gw.width, gw.height);
    glClearColor(0.02f, 0.02f, 0.03f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (gr_swap(&gw) != 0) ok = -1;

    gr_close_window(&gw);
    if (ok != 0) {
        printf("FAIL %s assertions samples=%d\n", name, samples);
        return 12;
    }
    printf("PASS %s configure=%u egl=%d.%d renderbuffer_rgba8=1 max_samples=%d msaa_samples=%d blit_resolve=1\n",
           name, gw.ev.xdg_configure_serial, gw.egl.major, gw.egl.minor, max_samples, samples);
    return 0;
}
