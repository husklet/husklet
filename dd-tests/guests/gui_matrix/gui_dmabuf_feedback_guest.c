#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static int put32(uint8_t *p, uint32_t v) { memcpy(p, &v, 4); return 4; }

static int send_get_registry(int fd) {
    uint32_t msg[3] = {1, (12u << 16) | 1u, 2};
    return write(fd, msg, sizeof(msg)) == (ssize_t)sizeof(msg) ? 0 : -1;
}

static int send_bind(int fd, uint32_t name, uint32_t version) {
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
    put32(msg + 20 + padded, 3);
    return write(fd, msg, size) == (ssize_t)size ? 0 : -1;
}

static int send_feedback_request(int fd) {
    uint32_t msg[3] = {3, (12u << 16) | 2u, 4};
    return write(fd, msg, sizeof(msg)) == (ssize_t)sizeof(msg) ? 0 : -1;
}

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
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int recv_chunk(int fd, uint8_t *buf, size_t cap, int *received_fd) {
    struct iovec iov = {.iov_base = buf, .iov_len = cap};
    char control[CMSG_SPACE(sizeof(int))] = {0};
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1,
                         .msg_control = control, .msg_controllen = sizeof(control)};
    ssize_t n = recvmsg(fd, &msg, 0);
    if (n <= 0) return -1;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&msg); c; c = CMSG_NXTHDR(&msg, c)) {
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS)
            memcpy(received_fd, CMSG_DATA(c), sizeof(int));
    }
    return (int)n;
}

int main(void) {
    int fd = connect_wayland();
    if (fd < 0 || send_get_registry(fd) != 0) return 2;
    uint32_t dmabuf_name = 0, dmabuf_version = 0;
    uint8_t buf[16384];
    for (int attempts = 0; attempts < 8 && !dmabuf_name; attempts++) {
        struct pollfd p = {fd, POLLIN, 0};
        if (poll(&p, 1, 1000) <= 0) return 3;
        int ignored = -1, n = recv_chunk(fd, buf, sizeof(buf), &ignored);
        if (ignored >= 0) close(ignored);
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
    if (!dmabuf_name || dmabuf_version < 4) return 4;
    if (send_bind(fd, dmabuf_name, 4) != 0 || send_feedback_request(fd) != 0) return 5;

    int table_fd = -1;
    uint32_t table_size = 0;
    uint8_t device[8] = {0};
    int got_device = 0;
    for (int attempts = 0; attempts < 12 && (table_fd < 0 || !got_device); attempts++) {
        struct pollfd p = {fd, POLLIN, 0};
        if (poll(&p, 1, 1000) <= 0) return 6;
        int passed = -1, n = recv_chunk(fd, buf, sizeof(buf), &passed);
        for (int off = 0; off + 8 <= n;) {
            uint32_t object, word;
            memcpy(&object, buf + off, 4); memcpy(&word, buf + off + 4, 4);
            uint32_t size = word >> 16, opcode = word & 0xffff;
            if (size < 8 || off + (int)size > n) break;
            if (object == 4 && opcode == 1 && size >= 12 && passed >= 0) {
                memcpy(&table_size, buf + off + 8, 4); table_fd = passed; passed = -1;
            } else if (object == 4 && opcode == 2 && size >= 20) {
                uint32_t len; memcpy(&len, buf + off + 8, 4);
                if (len == 8) { memcpy(device, buf + off + 12, 8); got_device = 1; }
            }
            off += size;
        }
        if (passed >= 0) close(passed);
    }
    if (table_fd < 0 || !got_device || table_size < 16 || table_size % 16) return 7;
    uint64_t dev; memcpy(&dev, device, 8);
    if (dev != ((226ull << 8) | 128)) return 8;
    uint8_t *table = mmap(NULL, table_size, PROT_READ, MAP_PRIVATE, table_fd, 0);
    if (table == MAP_FAILED) return 9;
    int ar = 0, xr = 0, truthful = 1;
    for (uint32_t off = 0; off < table_size; off += 16) {
        uint32_t format; uint64_t modifier;
        memcpy(&format, table + off, 4); memcpy(&modifier, table + off + 8, 8);
        if (modifier != (0x6464ull << 32)) truthful = 0;
        if (format == 0x34325241u) ar = 1;
        if (format == 0x34325258u) xr = 1;
    }
    munmap(table, table_size); close(table_fd); close(fd);
    printf("gui_dmabuf_feedback_guest v=%u device=%llu ar=%d xr=%d truthful=%d\n",
           dmabuf_version, (unsigned long long)dev, ar, xr, truthful);
    return ar && xr && truthful ? 0 : 10;
}
