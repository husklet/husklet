// Row 3 regression (compositor_validates_dmabuf_planes_flags_and_backing_metadata_before_success):
// a guest that imports a dd IOSurface dmabuf whose modifier carries an allocation GENERATION that no
// longer matches the id's live host allocation must be REJECTED by the compositor — no partial state,
// no fake wl_buffer. This probe drives the real zwp_linux_dmabuf_v1 import handshake over the wire:
//
//   create_params -> params.add(fd, plane=0, offset=0, stride, modifier_hi=magic|generation, mod_lo=id)
//                 -> params.create(w,h,XRGB8888,0)  -> await created (accepted) | failed (rejected)
//
// Usage: argv[1] = generation to stamp in modifier_hi bits 17..=31. Prints "result=created" or
// "result=failed"; exits 0 once it observes a definitive event, non-zero on a protocol/timeout error.
// The Rust harness seeds the host's live generation and asserts a stale generation -> failed while the
// matching generation -> created.
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#define HL_MAGIC 0x6464u
#define GEN_SHIFT 17u
#define GEN_MASK 0x7fffu
#define XRGB8888 0x34325258u
#define W 16
#define H 8
#define STRIDE 64u

static int put32(uint8_t *p, uint32_t v) { memcpy(p, &v, 4); return 4; }

static int connect_wayland(void) {
    const char *runtime = getenv("XDG_RUNTIME_DIR");
    const char *display = getenv("WAYLAND_DISPLAY");
    if (!runtime) runtime = "/run/user/0";
    if (!display) display = "wayland-0";
    char path[256];
    snprintf(path, sizeof(path), "%s/%s", runtime, display);
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un addr = {.sun_family = AF_UNIX};
    if (fd < 0 || strlen(path) >= sizeof(addr.sun_path)) return -1;
    strcpy(addr.sun_path, path);
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) { close(fd); return -1; }
    return fd;
}

static int send_get_registry(int fd) {
    uint32_t msg[3] = {1, (12u << 16) | 1u, 2};
    return write(fd, msg, sizeof(msg)) == (ssize_t)sizeof(msg) ? 0 : -1;
}

static int send_bind(int fd, uint32_t name, uint32_t version, uint32_t new_id) {
    const char iface[] = "zwp_linux_dmabuf_v1";
    const uint32_t slen = sizeof(iface), padded = (slen + 3) & ~3u;
    uint8_t msg[128] = {0};
    uint32_t size = 8 + 4 + 4 + padded + 4 + 4;
    put32(msg, 2);
    put32(msg + 4, (size << 16) | 0);
    put32(msg + 8, name);
    put32(msg + 12, slen);
    memcpy(msg + 16, iface, slen);
    put32(msg + 16 + padded, version);
    put32(msg + 20 + padded, new_id);
    return write(fd, msg, size) == (ssize_t)size ? 0 : -1;
}

// zwp_linux_dmabuf_v1.create_params(new_id params)
static int send_create_params(int fd, uint32_t dmabuf, uint32_t params) {
    uint32_t msg[3] = {dmabuf, (12u << 16) | 1u, params};
    return write(fd, msg, sizeof(msg)) == (ssize_t)sizeof(msg) ? 0 : -1;
}

// zwp_linux_buffer_params_v1.add(fd, plane_idx, offset, stride, modifier_hi, modifier_lo) — fd via SCM_RIGHTS
static int send_add(int fd, uint32_t params, int plane_fd, uint32_t mod_hi, uint32_t mod_lo) {
    uint8_t body[8 + 5 * 4];
    uint32_t size = sizeof(body);
    put32(body, params);
    put32(body + 4, (size << 16) | 1u); // opcode 1 = add
    put32(body + 8, 0);                  // plane_idx
    put32(body + 12, 0);                 // offset
    put32(body + 16, STRIDE);            // stride
    put32(body + 20, mod_hi);            // modifier_hi = magic | (generation << 17)
    put32(body + 24, mod_lo);            // modifier_lo = IOSurface id
    struct iovec iov = {.iov_base = body, .iov_len = size};
    char ctl[CMSG_SPACE(sizeof(int))] = {0};
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1,
                         .msg_control = ctl, .msg_controllen = sizeof(ctl)};
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &plane_fd, sizeof(int));
    return sendmsg(fd, &msg, 0) == (ssize_t)size ? 0 : -1;
}

// zwp_linux_buffer_params_v1.create(width, height, format, flags)
static int send_create(int fd, uint32_t params) {
    uint8_t body[8 + 4 * 4];
    uint32_t size = sizeof(body);
    put32(body, params);
    put32(body + 4, (size << 16) | 2u); // opcode 2 = create
    put32(body + 8, W);
    put32(body + 12, H);
    put32(body + 16, XRGB8888);
    put32(body + 20, 0); // flags
    return write(fd, body, size) == (ssize_t)size ? 0 : -1;
}

// A plane fd of exactly STRIDE*H bytes (regular temp file — exact fstat size on macOS).
static int make_plane_fd(void) {
    const char *tmp = getenv("TMPDIR");
    char path[256];
    snprintf(path, sizeof(path), "%s/hl-stale-plane-XXXXXX", tmp && *tmp ? tmp : "/tmp");
    int fd = mkstemp(path);
    if (fd < 0) return -1;
    unlink(path);
    if (ftruncate(fd, (off_t)(STRIDE * H)) != 0) { close(fd); return -1; }
    return fd;
}

int main(int argc, char **argv) {
    uint32_t generation = (argc > 1) ? (uint32_t)strtoul(argv[1], NULL, 10) : 0;
    // argv[2] = the IOSurface id to reference (default 7); lets the harness point the import at a real
    // engine/mach-seeded id whose live generation the compositor authenticates against.
    uint32_t iosurf_id = (argc > 2) ? (uint32_t)strtoul(argv[2], NULL, 10) : 7u;
    uint32_t mod_hi = HL_MAGIC | ((generation & GEN_MASK) << GEN_SHIFT);

    int fd = connect_wayland();
    if (fd < 0 || send_get_registry(fd) != 0) return 2;

    uint32_t dmabuf_name = 0, dmabuf_version = 0;
    uint8_t buf[16384];
    for (int attempts = 0; attempts < 8 && !dmabuf_name; attempts++) {
        struct pollfd p = {fd, POLLIN, 0};
        if (poll(&p, 1, 1000) <= 0) return 3;
        int n = (int)read(fd, buf, sizeof(buf));
        for (int off = 0; off + 8 <= n;) {
            uint32_t object, word;
            memcpy(&object, buf + off, 4); memcpy(&word, buf + off + 4, 4);
            uint32_t size = word >> 16;
            if (size < 8 || off + (int)size > n) break;
            if (object == 2 && (word & 0xffff) == 0 && size >= 20) {
                uint32_t name, len, version;
                memcpy(&name, buf + off + 8, 4); memcpy(&len, buf + off + 12, 4);
                uint32_t padded = (len + 3) & ~3u;
                if (16 + padded + 4 <= size && len == sizeof("zwp_linux_dmabuf_v1") &&
                    memcmp(buf + off + 16, "zwp_linux_dmabuf_v1", len) == 0) {
                    memcpy(&version, buf + off + 16 + padded, 4);
                    dmabuf_name = name; dmabuf_version = version;
                }
            }
            off += size;
        }
    }
    if (!dmabuf_name || dmabuf_version < 3) return 4;

    uint32_t dmabuf = 3, params = 4;
    uint32_t bind_ver = dmabuf_version < 3 ? dmabuf_version : 3;
    int plane = make_plane_fd();
    if (plane < 0) return 5;
    if (send_bind(fd, dmabuf_name, bind_ver, dmabuf) != 0) return 6;
    if (send_create_params(fd, dmabuf, params) != 0) return 6;
    if (send_add(fd, params, plane, mod_hi, iosurf_id) != 0) return 6;
    if (send_create(fd, params) != 0) return 6;
    close(plane);

    // Await zwp_linux_buffer_params_v1.created (opcode 0) | .failed (opcode 1) on object `params`.
    for (int attempts = 0; attempts < 12; attempts++) {
        struct pollfd p = {fd, POLLIN, 0};
        if (poll(&p, 1, 1000) <= 0) return 7;
        int n = (int)read(fd, buf, sizeof(buf));
        for (int off = 0; off + 8 <= n;) {
            uint32_t object, word;
            memcpy(&object, buf + off, 4); memcpy(&word, buf + off + 4, 4);
            uint32_t size = word >> 16, opcode = word & 0xffff;
            if (size < 8 || off + (int)size > n) break;
            if (object == params && opcode == 0) { printf("result=created\n"); return 0; }
            if (object == params && opcode == 1) { printf("result=failed\n"); return 0; }
            off += size;
        }
    }
    return 8; // no definitive event
}
