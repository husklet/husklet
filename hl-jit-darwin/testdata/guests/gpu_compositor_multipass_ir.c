// Direct dd-gpu Chrome-compositor-like replay probe. It renders a solid quad
// into an RGBA8 offscreen texture, blends alpha/coverage-like content into that
// texture in a second load/store pass, samples it into the final BGRA IOSurface
// target, then validates target pixels from the guest CPU mapping.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define DD_IOCTL_GPU_ALLOC 0xC020DD01u

struct hl_gpu_alloc {
    uint32_t width, height, format, stride, id;
    int32_t fd;
    uint64_t ptr;
};

enum {
    CBUF = 1, WBUF = 3, CTEX = 4, CSMP = 6, CSH = 8, CPIPE = 10, CBG = 13, SUBMIT = 19,
    BEGIN = 1, END = 2, SETPIPE = 3, SETBG = 4, SETVB = 5, VIEW = 7, DRAW = 8,
    RGBA8 = 1, BGRA8 = 2, D2 = 2, SAMPLED = 1, RT = 4, COPY_SRC = 8, COPY_DST = 16,
    VERTEX = 1, NEAREST = 0, CLAMP = 0, TRIANGLES = 3, LOAD = 0, CLEAR = 1,
};

static uint8_t ir[65536];
static size_t irn;
static int over;

static int res(size_t n) {
    if (irn + n > sizeof(ir)) {
        over = 1;
        return 0;
    }
    return 1;
}

static void u8(uint8_t v) { if (res(1)) ir[irn++] = v; }
static void u32(uint32_t v) { if (res(4)) { memcpy(ir + irn, &v, 4); irn += 4; } }
static void u64(uint64_t v) { if (res(8)) { memcpy(ir + irn, &v, 8); irn += 8; } }
static void f32(float v) { if (res(4)) { memcpy(ir + irn, &v, 4); irn += 4; } }
static void bytes(const void *p, uint32_t n) { u32(n); if (res(n)) { memcpy(ir + irn, p, n); irn += n; } }
static void str0(const char *s) { bytes(s, (uint32_t)strlen(s)); }

static int write_full(int fd, const void *buf, size_t len) {
    const uint8_t *p = (const uint8_t *)buf;
    while (len) {
        ssize_t n = write(fd, p, len);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) return -1;
        p += (size_t)n;
        len -= (size_t)n;
    }
    return 0;
}

static void shader_msl(uint32_t id, const char *msl) {
    uint32_t len = (uint32_t)strlen(msl), nwords = 1 + (len + 3) / 4;
    u8(CSH); u32(id); u32(nwords); u32(len);
    for (uint32_t i = 0; i < nwords - 1; i++) {
        uint32_t w = 0, off = i * 4, rem = len - off;
        memcpy(&w, msl + off, rem < 4 ? rem : 4);
        u32(w);
    }
}

static void create_buffer(uint32_t id, const void *data, uint32_t len) {
    u8(CBUF); u32(id); u64(len); u32(VERTEX); str0("");
    u8(WBUF); u32(id); u64(0); bytes(data, len);
}

static void create_texture(uint32_t id, uint32_t w, uint32_t h) {
    u8(CTEX); u32(id); u32(w); u32(h); u32(1); u32(1); u32(1); u32(D2); u32(RGBA8);
    u32(SAMPLED | RT | COPY_SRC | COPY_DST); str0("offscreen-rgba");
}

static void create_sampler(uint32_t id) {
    u8(CSMP); u32(id);
    u32(NEAREST); u32(NEAREST); u32(NEAREST);
    u32(CLAMP); u32(CLAMP); u32(CLAMP);
}

static void color_target(uint32_t fmt, int blend) {
    u32(fmt); u8(blend ? 1 : 0);
    if (blend) {
        u32(4); u32(5); u32(0); // RGB: src-alpha, one-minus-src-alpha, add
        u32(1); u32(5); u32(0); // A: one, one-minus-src-alpha, add
    }
    u32(0xf);
}

static void pipeline(uint32_t id, const char *vs, const char *fs,
                     uint32_t target_fmt, uint32_t stride, uint32_t attr1_components, int blend) {
    u8(CPIPE); u32(id);
    u32(20); str0(vs);
    u8(1); u32(20); str0(fs);
    u32(1); u32(stride); u32(0); u32(2);
    u32(0); u32(2); u32(0);
    u32(1); u32(attr1_components); u32(8);
    u32(1); color_target(target_fmt, blend);
    u8(0); u32(TRIANGLES); u32(0); u32(0); str0("");
}

static void bind_group_sample(uint32_t id, uint32_t tex, uint32_t sampler) {
    u8(CBG); u32(id); u32(0); u32(2);
    u32(0); u8(1); u32(tex);
    u32(0); u8(2); u32(sampler);
}

static void begin_pass(uint32_t tex, uint32_t load, float r, float g, float b, float a) {
    u8(BEGIN); u32(1); u32(tex); u32(load);
    f32(r); f32(g); f32(b); f32(a);
    u8(1); u8(0);
}

static void viewport(uint32_t w, uint32_t h) {
    u8(VIEW); f32(0); f32(0); f32((float)w); f32((float)h); f32(0); f32(1);
}

static void draw_pass(uint32_t pipe, uint32_t vbo, uint32_t w, uint32_t h) {
    viewport(w, h);
    u8(SETPIPE); u32(pipe);
    u8(SETVB); u32(0); u32(vbo); u64(0);
    u8(DRAW); u32(6); u32(1); u32(0); u32(0);
    u8(END);
}

static size_t build_ir(uint32_t w, uint32_t h) {
    static const char MSL[] =
        "#include <metal_stdlib>\n"
        "using namespace metal;\n"
        "struct VCIn{float2 p [[attribute(0)]];float4 c [[attribute(1)]];};\n"
        "struct VCOut{float4 position [[position]];float4 c [[user(v0)]];};\n"
        "vertex VCOut vsolid(VCIn in [[stage_in]]){VCOut o;o.position=float4(in.p,0,1);o.c=in.c;return o;}\n"
        "fragment float4 fsolid(VCOut in [[stage_in]]){return in.c;}\n"
        "struct VTIn{float2 p [[attribute(0)]];float2 uv [[attribute(1)]];};\n"
        "struct VTOut{float4 position [[position]];float2 uv [[user(v1)]];};\n"
        "vertex VTOut vtex(VTIn in [[stage_in]]){VTOut o;o.position=float4(in.p,0,1);o.uv=in.uv;return o;}\n"
        "fragment float4 ftex(VTOut in [[stage_in]],texture2d<float> t [[texture(0)]],sampler s [[sampler(0)]]){return t.sample(s,in.uv);}\n";
    const float base[] = {
        -1,-1,.20f,.40f,.80f,1,  1,-1,.20f,.40f,.80f,1, -1, 1,.20f,.40f,.80f,1,
         1,-1,.20f,.40f,.80f,1,  1, 1,.20f,.40f,.80f,1, -1, 1,.20f,.40f,.80f,1,
    };
    const float cover[] = {
        -.35f,-.65f,0,0,0,.5f, .35f,-.65f,0,0,0,.5f, -.35f,.65f,0,0,0,.5f,
         .35f,-.65f,0,0,0,.5f, .35f, .65f,0,0,0,.5f, -.35f,.65f,0,0,0,.5f,
    };
    const float sample[] = {
        -1,-1,0,1, 1,-1,1,1, -1, 1,0,0,
         1,-1,1,1, 1, 1,1,0, -1, 1,0,0,
    };
    irn = 0; over = 0;
    create_buffer(10, base, sizeof(base));
    create_buffer(11, cover, sizeof(cover));
    create_buffer(12, sample, sizeof(sample));
    create_texture(2, w, h);
    create_sampler(3);
    shader_msl(20, MSL);
    pipeline(30, "vsolid", "fsolid", RGBA8, 24, 4, 0);
    pipeline(31, "vsolid", "fsolid", RGBA8, 24, 4, 1);
    pipeline(32, "vtex", "ftex", BGRA8, 16, 2, 0);
    bind_group_sample(40, 2, 3);

    u8(SUBMIT); u32(19);
    begin_pass(2, CLEAR, .01f, .02f, .03f, 1); draw_pass(30, 10, w, h);
    begin_pass(2, LOAD, 0, 0, 0, 1); draw_pass(31, 11, w, h);
    begin_pass(1, CLEAR, .90f, .10f, .10f, 1);
    viewport(w, h);
    u8(SETPIPE); u32(32);
    u8(SETBG); u32(0); u32(40);
    u8(SETVB); u32(0); u32(12); u64(0);
    u8(DRAW); u32(6); u32(1); u32(0); u32(0);
    u8(END);
    u8(0);
    return over ? 0 : irn;
}

static int stream_ir(uint32_t surface_id, uint32_t w, uint32_t h, const uint8_t *body, size_t len) {
    const char *ep = getenv("DD_GPU_EXEC");
    if (!ep) ep = "/run/user/0/dd-gpu-0";
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) {
        printf("gpu_compositor_multipass: socket failed (%m)\n");
        return -1;
    }
    struct sockaddr_un un;
    memset(&un, 0, sizeof(un));
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof(un.sun_path), "%s", ep);
    if (connect(fd, (struct sockaddr *)&un, sizeof(un)) != 0) {
        printf("gpu_compositor_multipass: connect executor %s failed (%m)\n", ep);
        close(fd);
        return -1;
    }
    uint32_t hdr[4] = {surface_id, w, h, (uint32_t)len};
    int ok = write_full(fd, hdr, sizeof(hdr)) == 0 && write_full(fd, body, len) == 0;
    uint8_t ack = 0;
    if (ok) {
        ssize_t n = read(fd, &ack, 1);
        ok = n == 1 && ack == 1;
    }
    close(fd);
    printf("gpu_compositor_multipass: streamed %zu IR bytes ack=%u\n", len, ack);
    return ok ? 0 : -1;
}

static int near_u8(uint8_t got, int want, int tol) {
    int d = (int)got - want;
    return (d < 0 ? -d : d) <= tol;
}

static int pixel_ok(const uint8_t *px, uint32_t stride, uint32_t x, uint32_t y,
                    int want_b, int want_g, int want_r) {
    const uint8_t *p = px + y * stride + x * 4;
    return near_u8(p[0], want_b, 18) && near_u8(p[1], want_g, 18) && near_u8(p[2], want_r, 18);
}

static int validate_pixels(const struct hl_gpu_alloc *a) {
    const uint8_t *px = (const uint8_t *)(uintptr_t)a->ptr;
    uint32_t bx = a->width / 8, by = a->height / 5, cx = a->width / 2, cy = a->height / 2;
    int base = pixel_ok(px, a->stride, bx, by, 204, 102, 51);
    int cover = pixel_ok(px, a->stride, cx, cy, 102, 51, 26);
    const uint8_t *pb = px + by * a->stride + bx * 4;
    const uint8_t *pc = px + cy * a->stride + cx * 4;
    printf("gpu_compositor_multipass: sample base bgr=%u,%u,%u cover bgr=%u,%u,%u\n",
           pb[0], pb[1], pb[2], pc[0], pc[1], pc[2]);
    return base && cover;
}

int main(void) {
    struct hl_gpu_alloc a;
    memset(&a, 0, sizeof(a));
    a.width = 128;
    a.height = 96;
    int rnode = open("/dev/dri/renderD128", O_RDWR);
    if (rnode < 0) {
        printf("gpu_compositor_multipass: open renderD128 failed (%m)\n");
        return 1;
    }
    if (ioctl(rnode, DD_IOCTL_GPU_ALLOC, &a) != 0) {
        printf("gpu_compositor_multipass: DD_IOCTL_GPU_ALLOC failed (%m)\n");
        close(rnode);
        return 2;
    }
    close(rnode);
    if (a.id == 0 || a.ptr == 0 || a.stride < a.width * 4) {
        printf("gpu_compositor_multipass: bad alloc id=%u stride=%u ptr=%p\n",
               a.id, a.stride, (void *)(uintptr_t)a.ptr);
        return 3;
    }
    size_t n = build_ir(a.width, a.height);
    if (n == 0) {
        printf("gpu_compositor_multipass: IR build overflow\n");
        return 4;
    }
    if (stream_ir(a.id, a.width, a.height, ir, n) != 0) return 5;
    for (int i = 0; i < 40; i++) {
        if (validate_pixels(&a)) {
            printf("gpu_compositor_multipass: ok offscreen_rgba=1 load_store=1 sample_to_bgra=1\n");
            return 0;
        }
        usleep(25000);
    }
    printf("gpu_compositor_multipass: FAIL pixel validation\n");
    return 6;
}
