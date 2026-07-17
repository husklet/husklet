// GPU rung 2 end-to-end guest client: allocate a HOST-IOSurface-backed buffer via the engine
// (/dev/dri/renderD128 + HL_IOCTL_GPU_ALLOC), CPU-fill the (x^y) pattern straight into the IOSurface's
// pages (guest-VA == host-VA, so the returned pointer is directly usable), then commit it to hl-display
// over linux-dmabuf carrying the IOSurface id in the modifier. hl-display resolves the id → IOSurface →
// MTLTexture and composites ZERO-copy. Proves the no-VM guest→host GPU-buffer path.
//
// Runs under the hl engine with HL_GPU_IOSURFACE set (the --gui launcher sets it). Hand-rolls the Wayland
// wire (no libwayland) so it needs no client libs. Connects to $WAYLAND_DISPLAY under $XDG_RUNTIME_DIR.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>

#define HL_IOCTL_GPU_ALLOC 0xC020DD01u
#define HL_DMABUF_MOD_MAGIC 0x6464u
#define HL_DMABUF_RENDER_BIT 0x10000u
#define DRM_FMT_XRGB8888 0x34325258u

struct hl_gpu_alloc {
    uint32_t width, height, format, stride, id;
    int32_t fd;
    uint64_t ptr;
};

// Server-assigned registry global names (hl-display fixed layout).
enum { G_COMPOSITOR = 1, G_XDG_WM_BASE = 3, G_DMABUF = 6 };

// ---- hl-gpu IR wire builder (rung 3): hand-rolled to match hl-gpu/src/wire.rs (little-endian) ----
static uint8_t ir[8192];
static size_t irn;
static void iu8(uint8_t v) { ir[irn++] = v; }
static void iu32(uint32_t v) { memcpy(ir + irn, &v, 4); irn += 4; }
static void iu64(uint64_t v) { memcpy(ir + irn, &v, 8); irn += 8; }
static void if32(float v) { memcpy(ir + irn, &v, 4); irn += 4; }
static void istr(const char *s) { uint32_t l = (uint32_t)strlen(s); iu32(l); memcpy(ir + irn, s, l); irn += l; }
static void ibytes(const uint8_t *b, uint32_t l) { iu32(l); memcpy(ir + irn, b, l); irn += l; }

// Build a hl-gpu IR stream: upload a vertex-colored QUAD, create a pipeline, and draw it into texture
// id 1 (the executor injects our IOSurface there). Returns the stream length.
static size_t build_ir_quad(void) {
    irn = 0;
    // 6 vertices (2 triangles): pos.xy + color.rgba. TL red, TR green, BL blue, BR yellow.
    float v[6][6] = {
        {-0.8f, 0.8f, 1, .2f, .2f, 1}, {-0.8f, -0.8f, .2f, .2f, 1, 1}, {0.8f, 0.8f, .2f, 1, .2f, 1},
        {0.8f, 0.8f, .2f, 1, .2f, 1}, {-0.8f, -0.8f, .2f, .2f, 1, 1}, {0.8f, -0.8f, 1, 1, .2f, 1},
    };
    uint8_t vd[144];
    memcpy(vd, v, 144);
    // CreateBuffer(10, {size=144, usage=VERTEX(1), ""})
    iu8(1); iu32(10); iu64(144); iu32(1); istr("");
    // WriteBuffer{10, 0, vd}
    iu8(3); iu32(10); iu64(0); ibytes(vd, 144);
    // CreateShader{20, []}
    iu8(8); iu32(20); iu32(0);
    // CreateRenderPipeline(30, desc)
    iu8(10); iu32(30);
    iu32(20); istr("vcmain");                 // vertex ShaderRef
    iu8(1); iu32(20); istr("fcmain");          // fragment Some(ShaderRef)
    iu32(1);                                   // vertex_buffers len
    iu32(24); iu32(0); iu32(2);                // stride, step_mode, attrs len
    iu32(0); iu32(0); iu32(0);                 // attr0: loc,fmt,off
    iu32(1); iu32(0); iu32(8);                 // attr1: loc,fmt,off
    iu32(1);                                   // color_targets len
    iu32(2); iu8(0); iu32(0xf);                // format=Bgra8Unorm, blend None, write_mask
    iu8(0);                                    // depth None
    iu32(3); iu32(0); iu32(0);                 // topology TriangleList, cull, front_face
    istr("");                                  // label
    // Submit(cb)
    iu8(19);
    iu32(5);                                   // encoder len
    iu8(1); iu32(1);                           // BeginRenderPass, color len 1
    iu32(1); iu32(1); if32(.09f); if32(.09f); if32(.14f); if32(1.f); iu8(1); // tex 1, load Clear, clear, store
    iu8(0);                                    // depth None
    iu8(3); iu32(30);                          // SetPipeline(30)
    iu8(5); iu32(0); iu32(10); iu64(0);        // SetVertexBuffer slot0 buf10 off0
    iu8(8); iu32(6); iu32(1); iu32(0); iu32(0);// Draw 6,1,0,0
    iu8(2);                                    // EndRenderPass
    iu8(0);                                    // signal None
    return irn;
}

// Stream the IR to the hl-gpu executor ($HL_GPU_EXEC) and wait for its 1-byte ack. Returns 0 on success.
static int stream_ir(uint32_t id, uint32_t w, uint32_t h) {
    const char *ep = getenv("HL_GPU_EXEC");
    if (!ep) ep = "/run/user/0/hl-gpu-0";
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un un = {0};
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s", ep);
    if (connect(fd, (struct sockaddr *)&un, sizeof un) != 0) {
        printf("gpu_dmabuf: connect executor %s failed (%m)\n", ep);
        return -1;
    }
    size_t n = build_ir_quad();
    uint32_t hdr[4] = {id, w, h, (uint32_t)n};
    if (write(fd, hdr, sizeof hdr) != (ssize_t)sizeof hdr || write(fd, ir, n) != (ssize_t)n) {
        printf("gpu_dmabuf: executor write failed (%m)\n");
        close(fd);
        return -1;
    }
    uint8_t ack = 0;
    read(fd, &ack, 1); // wait until the host has rendered the frame
    close(fd);
    printf("gpu_dmabuf: streamed %zu IR bytes to executor, ack=%u\n", n, ack);
    return ack == 1 ? 0 : -1;
}

static int g_sock;
static uint8_t txbuf[4096];
static size_t txlen;

// Append a message header + body to the tx buffer. Returns a pointer to patch the size later? We know
// sizes up front, so build fully.
static void msg(uint32_t object, uint16_t opcode, const uint32_t *words, int nwords) {
    uint32_t size = 8 + nwords * 4;
    uint32_t hdr[2] = {object, (size << 16) | opcode};
    memcpy(txbuf + txlen, hdr, 8);
    txlen += 8;
    if (nwords) {
        memcpy(txbuf + txlen, words, nwords * 4);
        txlen += nwords * 4;
    }
}

static void flush_plain(void) {
    if (txlen) {
        write(g_sock, txbuf, txlen);
        txlen = 0;
    }
}

// Flush, sending `fd` as SCM_RIGHTS ancillary with the current buffer.
static void flush_with_fd(int fd) {
    struct iovec io = {txbuf, txlen};
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr mh = {0};
    mh.msg_iov = &io;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof cbuf;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof(int));
    sendmsg(g_sock, &mh, 0);
    txlen = 0;
}

int main(void) {
    // 1. Allocate a host-IOSurface-backed buffer via the engine.
    int rnode = open("/dev/dri/renderD128", O_RDWR);
    if (rnode < 0) {
        printf("gpu_dmabuf: open renderD128 failed (%m)\n");
        return 1;
    }
    struct hl_gpu_alloc a = {0};
    a.width = 256;
    a.height = 160;
    a.format = 0;
    if (ioctl(rnode, HL_IOCTL_GPU_ALLOC, &a) != 0) {
        printf("gpu_dmabuf: HL_IOCTL_GPU_ALLOC failed (%m)\n");
        return 2;
    }
    printf("gpu_dmabuf: alloc id=%u stride=%u fd=%d ptr=%p\n", a.id, a.stride, a.fd, (void *)(uintptr_t)a.ptr);
    if (a.ptr == 0 || a.id == 0) {
        printf("gpu_dmabuf: bad alloc\n");
        return 3;
    }

    // rung 3: HL_GPU_IR streams a real hl-gpu IR command stream to the host executor (which replays it on
    // Metal into this IOSurface); HL_GPU_RENDER is the older 1-op flag; else CPU-fill (rung 2).
    int want_ir = getenv("HL_GPU_IR") != NULL;
    int want_render = !want_ir && getenv("HL_GPU_RENDER") != NULL;
    if (want_ir) {
        if (stream_ir(a.id, a.width, a.height) != 0) return 5;
    }

    // 2. CPU-fill the (x^y) pattern straight into the IOSurface pages (BGRA). Skipped for GPU paths.
    uint8_t *px = (uint8_t *)(uintptr_t)a.ptr;
    if (!want_render && !want_ir)
    for (uint32_t y = 0; y < a.height; y++) {
        for (uint32_t x = 0; x < a.width; x++) {
            uint8_t *p = px + y * a.stride + x * 4;
            uint8_t b, g, r;
            if (x < 8 || x >= a.width - 8 || y < 8 || y >= a.height - 8) {
                r = 0xff;
                g = (uint8_t)(x * 255 / a.width);
                b = (uint8_t)(y * 255 / a.height);
            } else {
                uint8_t v = (uint8_t)((x ^ y) & 0xff);
                r = v; g = v; b = v;
            }
            p[0] = b; p[1] = g; p[2] = r; p[3] = 0;
        }
    }

    // 3. Connect to hl-display.
    const char *disp = getenv("WAYLAND_DISPLAY");
    const char *rundir = getenv("XDG_RUNTIME_DIR");
    if (!disp) disp = "wayland-0";
    if (!rundir) rundir = "/run/user/0";
    char path[256];
    if (disp[0] == '/') snprintf(path, sizeof path, "%s", disp);
    else snprintf(path, sizeof path, "%s/%s", rundir, disp);
    g_sock = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un un = {0};
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s", path);
    if (connect(g_sock, (struct sockaddr *)&un, sizeof un) != 0) {
        printf("gpu_dmabuf: connect %s failed (%m)\n", path);
        return 4;
    }

    // 4. Wayland handshake (fixed object ids; hl-display uses fixed global names).
    uint32_t reg = 2, comp = 3, dmabuf = 4, wm = 5, surface = 6, xdg = 7, toplevel = 8, params = 9, buffer = 10;
    uint32_t w1[1] = {reg};
    msg(1 /*wl_display*/, 1 /*get_registry*/, w1, 1);
    // bind wl_compositor: name, iface-string, ver, new_id
    // Strings are length-prefixed + padded; build a small helper inline.
    uint8_t buf[64];
    int n;
#define BIND(name_, iface_, ver_, newid_)                                     \
    do {                                                                      \
        const char *s = iface_;                                               \
        uint32_t slen = (uint32_t)strlen(s) + 1;                              \
        uint32_t pad = (slen + 3) & ~3u;                                      \
        n = 0;                                                                \
        memcpy(buf + n, &(uint32_t){name_}, 4); n += 4;                       \
        memcpy(buf + n, &slen, 4); n += 4;                                    \
        memcpy(buf + n, s, strlen(s) + 1); memset(buf + n + strlen(s) + 1, 0, pad - slen); n += pad; \
        memcpy(buf + n, &(uint32_t){ver_}, 4); n += 4;                        \
        memcpy(buf + n, &(uint32_t){newid_}, 4); n += 4;                      \
        msg(reg, 0 /*bind*/, (uint32_t *)buf, n / 4);                         \
    } while (0)
    BIND(G_COMPOSITOR, "wl_compositor", 4, comp);
    BIND(G_DMABUF, "zwp_linux_dmabuf_v1", 3, dmabuf);
    BIND(G_XDG_WM_BASE, "xdg_wm_base", 1, wm);
    // create_surface + xdg toplevel
    msg(comp, 0, &surface, 1);
    uint32_t xw[2] = {xdg, surface};
    msg(wm, 2 /*get_xdg_surface*/, xw, 2);
    msg(xdg, 1 /*get_toplevel*/, &toplevel, 1);
    msg(surface, 6 /*commit*/, NULL, 0); // initial commit
    flush_plain();
    usleep(50000);
    // ack_configure(serial guess=1)
    uint32_t ack[1] = {1};
    msg(xdg, 4 /*ack_configure*/, ack, 1);

    // 5. linux-dmabuf: create_params, add(fd, plane, offset, stride, mod_hi=MAGIC, mod_lo=id), create_immed.
    msg(dmabuf, 1 /*create_params*/, &params, 1);
    flush_plain();
    // params.add — the fd rides SCM_RIGHTS
    uint32_t mod_hi = HL_DMABUF_MOD_MAGIC | (want_render ? HL_DMABUF_RENDER_BIT : 0);
    uint32_t addw[5] = {0 /*plane*/, 0 /*offset*/, a.stride, mod_hi, a.id /*mod_lo*/};
    msg(params, 1 /*add*/, addw, 5);
    flush_with_fd(a.fd);
    // params.create_immed(buffer, w, h, format, flags)
    uint32_t ci[5] = {buffer, a.width, a.height, DRM_FMT_XRGB8888, 0};
    msg(params, 3 /*create_immed*/, ci, 5);
    // attach + damage + commit
    uint32_t at[3] = {buffer, 0, 0};
    msg(surface, 1 /*attach*/, at, 3);
    uint32_t dm[4] = {0, 0, a.width, a.height};
    msg(surface, 2 /*damage*/, dm, 4);
    msg(surface, 6 /*commit*/, NULL, 0);
    flush_plain();
    usleep(300000); // let hl-display composite

    printf("gpu_dmabuf: committed IOSurface id=%u via linux-dmabuf\n", a.id);
    return 0;
}
