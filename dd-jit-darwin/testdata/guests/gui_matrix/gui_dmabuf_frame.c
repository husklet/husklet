#include "gui_probe_wayland.h"

#include <sys/ioctl.h>

#define DD_IOCTL_GPU_ALLOC 0xC020DD01u
#define DD_DMABUF_MOD_MAGIC 0x6464u
#define DRM_FMT_XRGB8888 0x34325258u

struct dd_gpu_alloc {
    uint32_t width;
    uint32_t height;
    uint32_t format;
    uint32_t stride;
    uint32_t id;
    int32_t fd;
    uint64_t ptr;
};

static void fill_xor_frame(struct dd_gpu_alloc *a) {
    uint8_t *px = (uint8_t *)(uintptr_t)a->ptr;
    for (uint32_t y = 0; y < a->height; y++) {
        for (uint32_t x = 0; x < a->width; x++) {
            uint8_t *p = px + y * a->stride + x * 4;
            if (x < 6 || y < 6 || x + 6 >= a->width || y + 6 >= a->height) {
                p[0] = 0x20;
                p[1] = (uint8_t)(x * 255 / a->width);
                p[2] = 0xff;
                p[3] = 0;
            } else {
                uint8_t v = (uint8_t)((x ^ (y * 3)) & 0xff);
                p[0] = v;
                p[1] = (uint8_t)(255 - v);
                p[2] = (uint8_t)(x * 255 / a->width);
                p[3] = 0;
            }
        }
    }
}

static int alloc_frame(struct dd_gpu_alloc *a) {
    int rnode = open("/dev/dri/renderD128", O_RDWR);
    if (rnode < 0) {
        printf("gui_dmabuf_frame open_render=0 errno=%d\n", errno);
        return -1;
    }
    memset(a, 0, sizeof(*a));
    a->width = 128;
    a->height = 80;
    a->format = 0;
    if (ioctl(rnode, DD_IOCTL_GPU_ALLOC, a) != 0) {
        printf("gui_dmabuf_frame alloc=0 errno=%d\n", errno);
        close(rnode);
        return -1;
    }
    close(rnode);
    if (a->fd < 0 || a->ptr == 0 || a->id == 0 || a->stride < a->width * 4) {
        printf("gui_dmabuf_frame bad_alloc id=%u fd=%d ptr=%p stride=%u\n",
               a->id, a->fd, (void *)(uintptr_t)a->ptr, a->stride);
        return -1;
    }
    fill_xor_frame(a);
    return 0;
}

static int commit_dmabuf_frame(struct gp_conn *c, const struct dd_gpu_alloc *a) {
    gp_bind(c, GP_GLOBAL_DMABUF, "zwp_linux_dmabuf_v1", 3, GP_DMABUF);
    gp_send_u32(c, GP_DMABUF, 1, &(uint32_t){GP_DMABUF_PARAMS}, 1);
    if (gp_flush(c) != 0) return -1;

    uint32_t addw[5] = {
        0,
        0,
        a->stride,
        DD_DMABUF_MOD_MAGIC,
        a->id,
    };
    gp_send_u32(c, GP_DMABUF_PARAMS, 1, addw, 5);
    if (gp_flush_fd(c, a->fd) != 0) return -1;

    uint32_t ci[5] = {GP_BUFFER, a->width, a->height, DRM_FMT_XRGB8888, 0};
    gp_send_u32(c, GP_DMABUF_PARAMS, 3, ci, 5);
    uint32_t attach[3] = {GP_BUFFER, 0, 0};
    gp_send_u32(c, GP_SURFACE, 1, attach, 3);
    uint32_t damage[4] = {0, 0, a->width, a->height};
    gp_send_u32(c, GP_SURFACE, 2, damage, 4);
    gp_send_u32(c, GP_SURFACE, 3, &(uint32_t){GP_FRAME}, 1);
    gp_send_empty(c, GP_SURFACE, 6);
    return gp_flush(c);
}

int main(void) {
    struct dd_gpu_alloc a;
    if (alloc_frame(&a) != 0) return 1;

    struct gp_conn c;
    struct gp_events ev;
    memset(&ev, 0, sizeof(ev));
    if (gp_connect(&c) != 0) {
        close(a.fd);
        return 2;
    }
    if (gp_xdg_setup(&c, &ev, "gui_dmabuf_frame", 0) != 1) {
        printf("gui_dmabuf_frame configure=0 alloc_id=%u\n", a.id);
        close(a.fd);
        return 3;
    }
    if (commit_dmabuf_frame(&c, &a) != 0) {
        close(a.fd);
        return 4;
    }
    close(a.fd);

    int ok = gp_wait_frame_release(&c, &ev, 1500);
    printf("gui_dmabuf_frame configure=%u alloc_id=%u release=%d frame_done=%d ok=%d\n",
           ev.xdg_configure_serial, a.id, ev.got_buffer_release, ev.got_frame_done, ok == 1);
    return ok == 1 ? 0 : 5;
}
