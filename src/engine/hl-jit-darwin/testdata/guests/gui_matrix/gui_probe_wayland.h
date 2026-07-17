#ifndef HL_GUI_PROBE_WAYLAND_H
#define HL_GUI_PROBE_WAYLAND_H

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#if defined(__GNUC__)
#define GP_UNUSED __attribute__((unused))
#else
#define GP_UNUSED
#endif

enum {
    GP_WL_DISPLAY = 1,
    GP_REG = 2,
    GP_COMP = 3,
    GP_SHM = 4,
    GP_WM = 5,
    GP_SURFACE = 6,
    GP_XDG = 7,
    GP_TOPLEVEL = 8,
    GP_FRAME = 9,
    GP_POOL = 10,
    GP_BUFFER = 11,
    GP_DMABUF = 12,
    GP_DMABUF_PARAMS = 13,
};

enum {
    GP_GLOBAL_COMPOSITOR = 1,
    GP_GLOBAL_SHM = 2,
    GP_GLOBAL_XDG_WM_BASE = 3,
    GP_GLOBAL_DMABUF = 6,
};

struct gp_msg {
    uint32_t object;
    uint16_t opcode;
    uint32_t size;
    uint8_t body[2048];
};

struct gp_events {
    uint32_t xdg_configure_serial;
    int got_toplevel_configure;
    int got_xdg_configure;
    int got_frame_done;
    int got_buffer_release;
    int got_delete_id;
};

struct gp_conn {
    int fd;
    uint8_t tx[8192];
    size_t tx_len;
    uint8_t rx[8192];
    size_t rx_len;
};

static uint64_t gp_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static int gp_connect(struct gp_conn *c) {
    memset(c, 0, sizeof(*c));
    const char *disp = getenv("WAYLAND_DISPLAY");
    const char *rundir = getenv("XDG_RUNTIME_DIR");
    if (!disp) disp = "wayland-0";
    if (!rundir) rundir = "/run/user/0";

    char path[256];
    if (disp[0] == '/') snprintf(path, sizeof(path), "%s", disp);
    else snprintf(path, sizeof(path), "%s/%s", rundir, disp);

    c->fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (c->fd < 0) {
        perror("socket");
        return -1;
    }
    struct sockaddr_un un;
    memset(&un, 0, sizeof(un));
    un.sun_family = AF_UNIX;
    if (strlen(path) >= sizeof(un.sun_path)) {
        fprintf(stderr, "wayland socket path too long: %s\n", path);
        close(c->fd);
        c->fd = -1;
        return -1;
    }
    memcpy(un.sun_path, path, strlen(path) + 1);
    if (connect(c->fd, (struct sockaddr *)&un, sizeof(un)) != 0) {
        fprintf(stderr, "connect %s failed: %s\n", path, strerror(errno));
        close(c->fd);
        c->fd = -1;
        return -1;
    }
    fprintf(stderr, "connected wayland %s fd=%d\n", path, c->fd);
    return 0;
}

static void gp_put(struct gp_conn *c, const void *p, size_t n) {
    if (c->tx_len + n > sizeof(c->tx)) {
        fprintf(stderr, "tx overflow\n");
        exit(2);
    }
    memcpy(c->tx + c->tx_len, p, n);
    c->tx_len += n;
}

static void gp_msg_begin(struct gp_conn *c, uint32_t object, uint16_t opcode, size_t body_len) {
    uint32_t hdr[2] = {object, (((uint32_t)(8 + body_len)) << 16) | opcode};
    gp_put(c, hdr, sizeof(hdr));
}

static void gp_u32(struct gp_conn *c, uint32_t v) {
    gp_put(c, &v, 4);
}

static size_t gp_string_size(const char *s) {
    size_t len = strlen(s) + 1;
    return 4 + ((len + 3) & ~3u);
}

static void gp_string_body(struct gp_conn *c, const char *s) {
    uint32_t len = (uint32_t)strlen(s) + 1;
    uint32_t pad = (len + 3u) & ~3u;
    gp_u32(c, len);
    gp_put(c, s, len);
    for (uint32_t i = len; i < pad; i++) {
        uint8_t z = 0;
        gp_put(c, &z, 1);
    }
}

static int gp_flush(struct gp_conn *c) {
    size_t off = 0;
    while (off < c->tx_len) {
        ssize_t n = write(c->fd, c->tx + off, c->tx_len - off);
        if (n < 0) {
            if (errno == EINTR) continue;
            perror("write");
            return -1;
        }
        off += (size_t)n;
    }
    c->tx_len = 0;
    return 0;
}

static int gp_flush_fd(struct gp_conn *c, int fd) {
    struct iovec io = {c->tx, c->tx_len};
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof(cbuf));
    struct msghdr mh;
    memset(&mh, 0, sizeof(mh));
    mh.msg_iov = &io;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof(cbuf);
    struct cmsghdr *cm = CMSG_FIRSTHDR(&mh);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &fd, sizeof(int));
    ssize_t n = sendmsg(c->fd, &mh, 0);
    c->tx_len = 0;
    if (n < 0) {
        perror("sendmsg");
        return -1;
    }
    return 0;
}

static void gp_send_u32(struct gp_conn *c, uint32_t object, uint16_t opcode, const uint32_t *w, int n) {
    gp_msg_begin(c, object, opcode, (size_t)n * 4);
    for (int i = 0; i < n; i++) gp_u32(c, w[i]);
}

static void gp_send_empty(struct gp_conn *c, uint32_t object, uint16_t opcode) {
    gp_msg_begin(c, object, opcode, 0);
}

static void gp_bind(struct gp_conn *c, uint32_t name, const char *iface, uint32_t ver, uint32_t id) {
    size_t body = 4 + gp_string_size(iface) + 4 + 4;
    gp_msg_begin(c, GP_REG, 0, body);
    gp_u32(c, name);
    gp_string_body(c, iface);
    gp_u32(c, ver);
    gp_u32(c, id);
}

static void gp_set_title(struct gp_conn *c, const char *title) {
    gp_msg_begin(c, GP_TOPLEVEL, 2, gp_string_size(title));
    gp_string_body(c, title);
}

static int gp_read_one(struct gp_conn *c, struct gp_msg *m, int timeout_ms) {
    uint64_t deadline = gp_now_ms() + (uint64_t)timeout_ms;
    for (;;) {
        if (c->rx_len >= 8) {
            uint32_t object, word;
            memcpy(&object, c->rx, 4);
            memcpy(&word, c->rx + 4, 4);
            uint32_t size = word >> 16;
            uint16_t opcode = (uint16_t)(word & 0xffff);
            if (size < 8 || size > sizeof(m->body) + 8) {
                fprintf(stderr, "bad wayland message size=%u object=%u opcode=%u\n", size, object, opcode);
                return -1;
            }
            if (c->rx_len >= size) {
                m->object = object;
                m->opcode = opcode;
                m->size = size - 8;
                memcpy(m->body, c->rx + 8, m->size);
                memmove(c->rx, c->rx + size, c->rx_len - size);
                c->rx_len -= size;
                return 1;
            }
        }

        int rem = (int)(deadline - gp_now_ms());
        if (rem <= 0) return 0;
        struct pollfd pfd = {c->fd, POLLIN, 0};
        int pr = poll(&pfd, 1, rem);
        if (pr < 0) {
            if (errno == EINTR) continue;
            perror("poll");
            return -1;
        }
        if (pr == 0) return 0;
        if (pfd.revents & (POLLERR | POLLHUP | POLLNVAL)) return -1;
        if (pfd.revents & POLLIN) {
            if (c->rx_len == sizeof(c->rx)) {
                fprintf(stderr, "rx overflow\n");
                return -1;
            }
            ssize_t n = read(c->fd, c->rx + c->rx_len, sizeof(c->rx) - c->rx_len);
            if (n < 0) {
                if (errno == EINTR) continue;
                perror("read");
                return -1;
            }
            if (n == 0) return -1;
            c->rx_len += (size_t)n;
        }
    }
}

static uint32_t gp_body_u32(const struct gp_msg *m, size_t off) {
    uint32_t v = 0;
    if (off + 4 <= m->size) memcpy(&v, m->body + off, 4);
    return v;
}

static void gp_note_event(const struct gp_msg *m, struct gp_events *ev) {
    if (m->object == GP_TOPLEVEL && m->opcode == 0) {
        ev->got_toplevel_configure = 1;
        int32_t w = (int32_t)gp_body_u32(m, 0);
        int32_t h = (int32_t)gp_body_u32(m, 4);
        fprintf(stderr, "event xdg_toplevel.configure %dx%d\n", w, h);
    } else if (m->object == GP_XDG && m->opcode == 0) {
        ev->got_xdg_configure = 1;
        ev->xdg_configure_serial = gp_body_u32(m, 0);
        fprintf(stderr, "event xdg_surface.configure serial=%u\n", ev->xdg_configure_serial);
    } else if (m->object == GP_FRAME && m->opcode == 0) {
        ev->got_frame_done = 1;
        fprintf(stderr, "event wl_callback.done callback=%u time=%u\n", m->object, gp_body_u32(m, 0));
    } else if (m->object == GP_BUFFER && m->opcode == 0) {
        ev->got_buffer_release = 1;
        fprintf(stderr, "event wl_buffer.release\n");
    } else if (m->object == GP_WL_DISPLAY && m->opcode == 1) {
        ev->got_delete_id = 1;
    }
}

static int GP_UNUSED gp_drain(struct gp_conn *c, struct gp_events *ev, int timeout_ms) {
    uint64_t deadline = gp_now_ms() + (uint64_t)timeout_ms;
    for (;;) {
        int rem = (int)(deadline - gp_now_ms());
        if (rem <= 0) return 0;
        struct gp_msg m;
        int r = gp_read_one(c, &m, rem);
        if (r <= 0) return r;
        gp_note_event(&m, ev);
    }
}

static int gp_wait_xdg_configure(struct gp_conn *c, struct gp_events *ev, int timeout_ms) {
    uint64_t deadline = gp_now_ms() + (uint64_t)timeout_ms;
    while (!ev->got_xdg_configure) {
        int rem = (int)(deadline - gp_now_ms());
        if (rem <= 0) return 0;
        struct gp_msg m;
        int r = gp_read_one(c, &m, rem);
        if (r <= 0) return r;
        gp_note_event(&m, ev);
    }
    return 1;
}

static int GP_UNUSED gp_wait_frame_release(struct gp_conn *c, struct gp_events *ev, int timeout_ms) {
    uint64_t deadline = gp_now_ms() + (uint64_t)timeout_ms;
    while (!ev->got_frame_done || !ev->got_buffer_release) {
        int rem = (int)(deadline - gp_now_ms());
        if (rem <= 0) return 0;
        struct gp_msg m;
        int r = gp_read_one(c, &m, rem);
        if (r <= 0) return r;
        gp_note_event(&m, ev);
    }
    return 1;
}

static int gp_xdg_setup(struct gp_conn *c, struct gp_events *ev, const char *title, int bind_shm) {
    uint32_t reg = GP_REG;
    gp_send_u32(c, GP_WL_DISPLAY, 1, &reg, 1);
    gp_bind(c, GP_GLOBAL_COMPOSITOR, "wl_compositor", 4, GP_COMP);
    if (bind_shm) gp_bind(c, GP_GLOBAL_SHM, "wl_shm", 1, GP_SHM);
    gp_bind(c, GP_GLOBAL_XDG_WM_BASE, "xdg_wm_base", 1, GP_WM);
    gp_send_u32(c, GP_COMP, 0, &(uint32_t){GP_SURFACE}, 1);
    uint32_t xs[2] = {GP_XDG, GP_SURFACE};
    gp_send_u32(c, GP_WM, 2, xs, 2);
    gp_send_u32(c, GP_XDG, 1, &(uint32_t){GP_TOPLEVEL}, 1);
    gp_set_title(c, title);
    gp_send_empty(c, GP_SURFACE, 6);
    if (gp_flush(c) != 0) return -1;
    int r = gp_wait_xdg_configure(c, ev, 1500);
    if (r != 1) return r;
    gp_send_u32(c, GP_XDG, 4, &ev->xdg_configure_serial, 1);
    if (gp_flush(c) != 0) return -1;
    return 1;
}

static int gp_make_memfd(const char *name, size_t size) {
#ifdef __linux__
    int fd = memfd_create(name, 0);
#else
    int fd = -1;
    (void)name;
#endif
    if (fd < 0) return -1;
    if (ftruncate(fd, (off_t)size) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int GP_UNUSED gp_commit_shm_frame(struct gp_conn *c, int width, int height, uint32_t frame_id) {
    int stride = width * 4;
    size_t size = (size_t)stride * (size_t)height;
    int fd = gp_make_memfd("hl-gui-probe-shm", size);
    if (fd < 0) {
        perror("memfd_create/ftruncate");
        return -1;
    }
    uint8_t *px = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (px == MAP_FAILED) {
        perror("mmap");
        close(fd);
        return -1;
    }
    for (int y = 0; y < height; y++) {
        for (int x = 0; x < width; x++) {
            uint8_t *p = px + y * stride + x * 4;
            p[0] = (uint8_t)(x * 255 / width);
            p[1] = (uint8_t)(y * 255 / height);
            p[2] = (uint8_t)((x ^ y) & 0xff);
            p[3] = 0;
        }
    }

    uint32_t pool_words[2] = {GP_POOL, (uint32_t)size};
    gp_send_u32(c, GP_SHM, 0, pool_words, 2);
    if (gp_flush_fd(c, fd) != 0) {
        munmap(px, size);
        close(fd);
        return -1;
    }
    close(fd);

    uint32_t buf_words[6] = {GP_BUFFER, 0, (uint32_t)width, (uint32_t)height, (uint32_t)stride, 1};
    gp_send_u32(c, GP_POOL, 0, buf_words, 6);
    uint32_t attach[3] = {GP_BUFFER, 0, 0};
    gp_send_u32(c, GP_SURFACE, 1, attach, 3);
    uint32_t damage[4] = {0, 0, (uint32_t)width, (uint32_t)height};
    gp_send_u32(c, GP_SURFACE, 2, damage, 4);
    gp_send_u32(c, GP_SURFACE, 3, &frame_id, 1);
    gp_send_empty(c, GP_SURFACE, 6);
    int ok = gp_flush(c);
    munmap(px, size);
    return ok;
}

#endif
