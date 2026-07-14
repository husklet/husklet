// hl guest GLES2 + EGL shim (GPU rung 3 ICD, first slice). A real GLES2 app links -lEGL -lGLESv2 and
// runs UNMODIFIED against these symbols (mount-injected as libEGL.so.1 + libGLESv2.so.2, like libwayland
// — NOT a specialized image). Each GL/EGL call drives a small state machine; on eglSwapBuffers the shim
// translates the accumulated GL state into a hl-gpu IR command stream, ships it to the host Metal executor
// ($HL_GPU_EXEC) which renders it into a rung-2 IOSurface, and commits that IOSurface to hl-display
// (linux-dmabuf) for zero-copy compositing.
//
// SUBSET (see RENDERING_PLAN.md M5 for the coverage map): eglGetDisplay/Initialize/ChooseConfig/
// CreateContext/CreateWindowSurface/MakeCurrent/SwapBuffers/Terminate/GetError/SwapInterval/GetProcAddress;
// glClearColor/Clear/Viewport, glCreateShader/ShaderSource/CompileShader/GetShaderiv, glCreateProgram/
// AttachShader/LinkProgram/GetProgramiv/UseProgram, glGetAttribLocation/GetUniformLocation/Uniform*,
// glGenBuffers/BindBuffer/BufferData/BufferSubData (ARRAY + ELEMENT_ARRAY), glVertexAttribPointer/
// EnableVertexAttribArray, glDrawArrays + glDrawElements (indexed), textures (glGenTextures/BindTexture/
// ActiveTexture/TexImage2D/TexParameteri/f/PixelStorei/GenerateMipmap), glEnable(DEPTH_TEST), glGetString.
// Shaders: a hand GLSL-ES→MSL translator handling attribute/varying/uniform blocks, const globals,
// sampler2D + texture2D()/texture() sampling, vecN(vecM) truncation, and mat3/mat4 — enough for glmark2's
// build/texture-scene shaders (which Metal compiles; see `hl-display selftest-msl`). Arbitrary shaders
// (control flow, all builtins) remain the long tail — SPIRV-Cross is unbuildable on this host (no brew /
// egress / crates.io), so the hand translator is the committed path.
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <errno.h>
#include <dlfcn.h>
#include <time.h>
#include <poll.h>

// ---- EGL/GL minimal typedefs (avoid depending on system headers in the shim build) ----
typedef int32_t EGLint;
typedef unsigned int EGLBoolean;
typedef unsigned int EGLenum;
typedef void *EGLDisplay;
typedef void *EGLConfig;
typedef void *EGLContext;
typedef void *EGLSurface;
typedef void *EGLNativeWindowType;
typedef void *EGLNativeDisplayType;
typedef unsigned int GLenum;
typedef unsigned char GLboolean;
typedef unsigned int GLbitfield;
typedef int GLint;
typedef int GLsizei;
typedef unsigned int GLuint;
typedef float GLfloat;
typedef char GLchar;
typedef intptr_t GLintptr;
typedef intptr_t GLsizeiptr;
#define EGL_TRUE 1
#define EGL_FALSE 0
#define EGL_NONE 0x3038
#define EGL_NO_CONTEXT ((EGLContext)0)
#define EGL_NO_SURFACE ((EGLSurface)0)
#define EGL_NO_DISPLAY ((EGLDisplay)0)
#define EGL_SUCCESS 0x3000
#define EGL_BAD_MATCH 0x3009
#define EGL_WIDTH 0x3057
#define EGL_HEIGHT 0x3056
// EGLConfig attribute enums (the set glmark2 queries via eglGetConfigAttrib).
#define EGL_BUFFER_SIZE 0x3020
#define EGL_ALPHA_SIZE 0x3021
#define EGL_BLUE_SIZE 0x3022
#define EGL_GREEN_SIZE 0x3023
#define EGL_RED_SIZE 0x3024
#define EGL_DEPTH_SIZE 0x3025
#define EGL_STENCIL_SIZE 0x3026
#define EGL_CONFIG_CAVEAT 0x3027
#define EGL_CONFIG_ID 0x3028
#define EGL_LEVEL 0x3029
#define EGL_MAX_PBUFFER_HEIGHT 0x302A
#define EGL_MAX_PBUFFER_PIXELS 0x302B
#define EGL_MAX_PBUFFER_WIDTH 0x302C
#define EGL_NATIVE_RENDERABLE 0x302D
#define EGL_NATIVE_VISUAL_ID 0x302E
#define EGL_NATIVE_VISUAL_TYPE 0x302F
#define EGL_SAMPLES 0x3031
#define EGL_SAMPLE_BUFFERS 0x3032
#define EGL_SURFACE_TYPE 0x3033
#define EGL_TRANSPARENT_TYPE 0x3034
#define EGL_BIND_TO_TEXTURE_RGB 0x3039
#define EGL_BIND_TO_TEXTURE_RGBA 0x303A
#define EGL_MIN_SWAP_INTERVAL 0x303B
#define EGL_MAX_SWAP_INTERVAL 0x303C
#define EGL_LUMINANCE_SIZE 0x303D
#define EGL_ALPHA_MASK_SIZE 0x303E
#define EGL_COLOR_BUFFER_TYPE 0x303F
#define EGL_RENDERABLE_TYPE 0x3040
#define EGL_CONFORMANT 0x3042
#define EGL_RGB_BUFFER 0x308E
// EGL_SURFACE_TYPE / EGL_RENDERABLE_TYPE bit values.
#define EGL_PBUFFER_BIT 0x0001
#define EGL_WINDOW_BIT 0x0004
#define EGL_OPENGL_ES_BIT 0x0001
#define EGL_OPENGL_ES2_BIT 0x0004
#define EGL_OPENGL_ES3_BIT_KHR 0x0040
// eglQueryString targets.
#define EGL_VENDOR 0x3053
#define EGL_VERSION 0x3054
#define EGL_EXTENSIONS 0x3055
#define EGL_CLIENT_APIS 0x308D
#define GL_FALSE 0
#define GL_TRUE 1
#define GL_NO_ERROR 0
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_COMPILE_STATUS 0x8B81
#define GL_LINK_STATUS 0x8B82
#define GL_ARRAY_BUFFER 0x8892
#define GL_ELEMENT_ARRAY_BUFFER 0x8893
#define GL_FLOAT 0x1406
#define GL_HALF_FLOAT 0x140B
#define GL_BYTE 0x1400
#define GL_UNSIGNED_BYTE 0x1401
#define GL_SHORT 0x1402
#define GL_UNSIGNED_SHORT 0x1403
#define GL_INT 0x1404
#define GL_UNSIGNED_INT 0x1405
#define GL_TRIANGLES 0x0004
#define GL_TRIANGLE_STRIP 0x0005
#define GL_COLOR 0x1800
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_DEPTH_BUFFER_BIT 0x0100
#define GL_DEPTH_TEST 0x0B71
#define GL_BLEND 0x0BE2
#define GL_CULL_FACE 0x0B44
#define GL_SCISSOR_TEST 0x0C11
#define GL_SCISSOR_BOX 0x0C10
#define GL_TEXTURE_2D 0x0DE1
#define GL_TEXTURE_3D 0x806F
#define GL_TEXTURE_2D_ARRAY 0x8C1A
#define GL_TEXTURE0 0x84C0
#define GL_RGBA 0x1908
#define GL_RGB 0x1907
#define GL_RED 0x1903
#define GL_ALPHA 0x1906
#define GL_LUMINANCE 0x1909
#define GL_BGRA_EXT 0x80E1
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_TEXTURE_WRAP_S 0x2802
#define GL_TEXTURE_WRAP_T 0x2803
#define GL_TEXTURE_BASE_LEVEL 0x813C
#define GL_TEXTURE_MAX_LEVEL 0x813D
#define GL_TEXTURE_IMMUTABLE_FORMAT 0x912F
#define GL_NEAREST 0x2600
#define GL_LINEAR 0x2601
#define GL_NEAREST_MIPMAP_NEAREST 0x2700
#define GL_LINEAR_MIPMAP_NEAREST 0x2701
#define GL_NEAREST_MIPMAP_LINEAR 0x2702
#define GL_LINEAR_MIPMAP_LINEAR 0x2703
#define GL_CLAMP_TO_EDGE 0x812F
#define GL_REPEAT 0x2901
#define GL_MIRRORED_REPEAT 0x8370
#define GL_ZERO 0
#define GL_ONE 1
#define GL_SRC_COLOR 0x0300
#define GL_ONE_MINUS_SRC_COLOR 0x0301
#define GL_SRC_ALPHA 0x0302
#define GL_ONE_MINUS_SRC_ALPHA 0x0303
#define GL_DST_ALPHA 0x0304
#define GL_ONE_MINUS_DST_ALPHA 0x0305
#define GL_DST_COLOR 0x0306
#define GL_ONE_MINUS_DST_COLOR 0x0307
#define GL_SRC_ALPHA_SATURATE 0x0308
#define GL_FUNC_ADD 0x8006
#define GL_FUNC_SUBTRACT 0x800A
#define GL_FUNC_REVERSE_SUBTRACT 0x800B
#define GL_MIN 0x8007
#define GL_MAX 0x8008
#define GL_FRAMEBUFFER 0x8D40
#define GL_RENDERBUFFER 0x8D41
#define GL_READ_FRAMEBUFFER 0x8CA8
#define GL_DRAW_FRAMEBUFFER 0x8CA9
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_DEPTH_ATTACHMENT 0x8D00
#define GL_STENCIL_ATTACHMENT 0x8D20
#define GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE 0x8CD0
#define GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME 0x8CD1
#define GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL 0x8CD2
#define GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_CUBE_MAP_FACE 0x8CD3
#define GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER 0x8CD4
#define GL_FRAMEBUFFER_ATTACHMENT_RED_SIZE 0x8212
#define GL_FRAMEBUFFER_ATTACHMENT_GREEN_SIZE 0x8213
#define GL_FRAMEBUFFER_ATTACHMENT_BLUE_SIZE 0x8214
#define GL_FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE 0x8215
#define GL_FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE 0x8216
#define GL_FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE 0x8217
#define GL_FRAMEBUFFER_DEFAULT 0x8218
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5
#define GL_RENDERBUFFER_WIDTH 0x8D42
#define GL_RENDERBUFFER_HEIGHT 0x8D43
#define GL_RENDERBUFFER_INTERNAL_FORMAT 0x8D44
#define GL_RENDERBUFFER_RED_SIZE 0x8D50
#define GL_RENDERBUFFER_GREEN_SIZE 0x8D51
#define GL_RENDERBUFFER_BLUE_SIZE 0x8D52
#define GL_RENDERBUFFER_ALPHA_SIZE 0x8D53
#define GL_RENDERBUFFER_DEPTH_SIZE 0x8D54
#define GL_RENDERBUFFER_STENCIL_SIZE 0x8D55
#define GL_RENDERBUFFER_SAMPLES 0x8CAB
#define GL_TEXTURE 0x1702
#define GL_VERSION 0x1F02
#define GL_VENDOR 0x1F00
#define GL_RENDERER 0x1F01
#define GL_SHADING_LANGUAGE_VERSION 0x8B8C
#define GL_EXTENSIONS 0x1F03

// ---- hl ioctl + dmabuf constants (match hl_gpu.h) ----
#define HL_IOCTL_GPU_ALLOC 0xC020DD01u
#define HL_DMABUF_MOD_MAGIC 0x6464u
#define DRM_FMT_XRGB8888 0x34325258u
struct hl_gpu_alloc {
    uint32_t width, height, format, stride, id;
    int32_t fd;
    uint64_t ptr;
};

// ======================= GL state machine =======================
#define MAXSH 64
#define MAXPROG 64
#define MAXBUF 64
#define MAXATTR 16
struct uni { char name[32]; int off, sz; }; // one uniform's byte offset/size in the uniform block
struct sh {
    int used, type;
    char *src;
};
struct prog {
    int used, vs, fs;
    char *msl;
    struct uni unis[16];
    int nuni, ubuf_size;
    uint8_t ubuf[512];
    char samp[4][32]; // sampler2D uniform names (→ texture(0..)/sampler(0..))
    int samp_units[4]; // sampler index -> GL texture unit selected by glUniform1i
    int nsamp;
};
static uint8_t g_ubuf[512]; // current uniform-block bytes (written by glUniform*)
struct buf {
    int used;
    uint8_t *data;
    size_t size;
    GLenum usage;
    uint64_t gen; // L5: bumped on every content mutation (glBufferData/SubData, alloc/free) → dirty key
};
struct attr {
    int enabled, size;
    int normalized;
    int integer;
    GLenum type;
    GLsizei stride;
    size_t offset;
    GLuint buffer;
};
#define MAXTEX 32
struct tex {
    int used, w, h;
    uint8_t *data; // stored as RGBA8 (converted from the app's format)
    size_t size;
    int minf, magf, ws, wt; // GL filter/wrap enums
    uint64_t gen; // L5: bumped on every glTexImage2D (content change) → dirty key for the upload skip
};
#define MAXFBO 64
struct fbo {
    int used;
    GLuint color_tex;
    GLuint color_rbo;
    GLint color_level;
    GLint color_layer;
};
#define MAXRBO 64
struct rbo {
    int used, w, h, samples;
    GLenum ifmt;
    uint64_t gen;
};
static struct sh g_sh[MAXSH];
static struct prog g_prog[MAXPROG];
static struct buf g_buf[MAXBUF];
static struct attr g_attr[MAXATTR];
#define MAXVAO 128
struct vao {
    int used;
    struct attr attrs[MAXATTR];
    GLuint elem_buf;
};
static struct vao g_vao[MAXVAO];
static GLuint g_cur_vao;
static struct tex g_tex[MAXTEX];
static struct fbo g_fbo[MAXFBO];
static struct rbo g_rbo[MAXRBO];
static GLuint g_tex_unit[8]; // texture bound per active unit (GL_TEXTURE_2D)
static int g_active_unit;
static int g_unpack_alignment = 4, g_unpack_row_length, g_unpack_skip_rows, g_unpack_skip_pixels;
static GLuint g_cur_prog, g_arr_buf, g_elem_buf;
static GLuint g_draw_fbo, g_read_fbo;
static GLuint g_rbo_bound;
static int g_depth; // GL_DEPTH_TEST enabled
static int g_blend;
static GLenum g_blend_src_rgb = GL_ONE, g_blend_dst_rgb = GL_ZERO, g_blend_src_alpha = GL_ONE, g_blend_dst_alpha = GL_ZERO;
static GLenum g_blend_eq_rgb = GL_FUNC_ADD, g_blend_eq_alpha = GL_FUNC_ADD;
static float g_clear[4] = {0, 0, 0, 1};
static int g_clear_serial;
static int g_viewport[4] = {0, 0, 0, 0};
static int g_scissor_enabled;
static int g_scissor[4] = {0, 0, 0, 0};
static int g_draw_mode = -1, g_draw_first, g_draw_count; // last glDrawArrays this frame
static int g_draw_indexed;      // this frame's draw was glDrawElements
static int g_index_type;        // GL_UNSIGNED_SHORT / GL_UNSIGNED_INT
static size_t g_index_offset;   // byte offset into the element buffer
#define MAXDRAWS 512
struct draw_call {
    int is_clear;
    int mode, first, count, indexed, index_type;
    size_t index_offset;
    GLuint prog;
    GLuint elem_buf;
    GLuint target_tex; // 0 = default window surface, otherwise GL texture attached to draw FBO color0
    int clear_rect[4];
    struct attr attrs[MAXATTR];
    GLuint tex_units[8];
    int samp_units[4];
    int viewport[4];
    int scissor_enabled;
    int scissor[4];
    int blend;
    GLenum blend_src_rgb, blend_dst_rgb, blend_src_alpha, blend_dst_alpha;
    GLenum blend_eq_rgb, blend_eq_alpha;
    float clear[4];
    int clear_serial;
    uint8_t ubuf[sizeof g_ubuf];
    int snap_vbo_count;
    GLuint snap_vbo_src[MAXATTR];
    uint64_t snap_vbo_gen[MAXATTR];
    uint8_t *snap_vbo_data[MAXATTR];
    size_t snap_vbo_size[MAXATTR];
    GLuint snap_ibo_src;
    uint64_t snap_ibo_gen;
    uint8_t *snap_ibo_data;
    size_t snap_ibo_size;
};
static struct draw_call g_draws[MAXDRAWS];
static int g_ndraws;
// Draw-time snapshot of the vertex-attribute array. glmark2 (Mesh::render_vbo) enables its attribs,
// issues the draw, then DISABLES them again — all before eglSwapBuffers. Since the shim assembles the
// frame's IR lazily at swap, reading live g_attr would see the torn-down (disabled) state → empty vertex
// layout. So we snapshot g_attr at draw-call time and the swap uses that snapshot.
static struct attr g_attr_snap[MAXATTR];
static int g_have_draw_snap;

static void vao_store_current(void) {
    if (g_cur_vao < MAXVAO) {
        g_vao[g_cur_vao].used = 1;
        memcpy(g_vao[g_cur_vao].attrs, g_attr, sizeof g_attr);
        g_vao[g_cur_vao].elem_buf = g_elem_buf;
    }
}

static void vao_load(GLuint vao) {
    if (vao < MAXVAO && g_vao[vao].used) {
        memcpy(g_attr, g_vao[vao].attrs, sizeof g_attr);
        g_elem_buf = g_vao[vao].elem_buf;
    } else {
        memset(g_attr, 0, sizeof g_attr);
        g_elem_buf = 0;
        if (vao < MAXVAO) g_vao[vao].used = 1;
    }
}

// surface
static struct hl_gpu_alloc g_surf;
static int g_have_surf;
static int g_default_surface_valid;
static int g_default_full_clear_since_swap;
static int g_wl = -1, g_wl_ready; // wayland socket to hl-display
static uint32_t g_wl_surface = 6, g_wl_buffer = 10;
static uint32_t g_xdg_surface = 7;
static uint32_t g_wl_frame_cb = 11; // wl_callback id for wl_surface.frame (L1 pacing)
static int g_pending_logical_w, g_pending_logical_h;
static int g_pending_attach_x, g_pending_attach_y;
static int g_surface_logical_w, g_surface_logical_h;
static int g_surface_geom_x, g_surface_geom_y;
static int g_surface_geom_source; // 0=backing, 1=wl_egl_window, 2=env, 3=cmdline
static int g_surface_geom_sent;

// ---- L2/L7.1: persistent executor socket (one connection for the surface's whole lifetime) ----
static int g_exec_fd = -1;
static uint64_t g_exec_connects; // count of connect()s — should be 1 for a whole run (was 1/frame)

// ---- L5: buffer/texture persistence + delta upload ----------------------------------------------
// The host executor holds a persistent MetalBackend (L2), so a buffer/texture uploaded once STAYS
// resident (keyed by its IR id) across frames. glmark2's horse VBO/IBO (~1 MB) and textures are static —
// re-encoding + re-socketing + re-uploading them every frame is pure waste. We track, per IR resource id,
// which guest resource (source GL id + a monotonically-bumped content `gen`) is currently resident on the
// host; a frame emits CreateBuffer+WriteBuffer (or the texture staging upload+copy) ONLY on a miss (new id,
// changed content, or a different source). Static geometry then uploads exactly ONCE → steady-state IR
// bytes/frame collapse from ~1 MB toward a few KB. Correctness rests on the content `gen` (any
// glBufferData/glBufferSubData/glTexImage2D bumps it → forced re-upload) and `g_res_reset` on reconnect
// (a fresh host backend has an empty cache → re-emit everything). A/B: HL_NO_DELTA=1 forces full re-upload.
struct residency { int valid, src; uint64_t gen; };
static struct residency g_res_vbuf[MAXATTR]; // IR ids 200+slot (one per distinct source VBO this frame)
static struct residency g_res_index;         // IR id 12 (the element/index buffer)
static struct residency g_res_tex[4];         // texture slots k → tid 50+k (staging upload + CopyToTexture)
static struct residency g_res_tex_replay[MAXTEX]; // replay path: GL texture id → uploaded generation
static struct residency g_res_replay_vbuf[MAXDRAWS][MAXATTR]; // replay draw/slot ids 2000+...
static struct residency g_res_replay_ibo[MAXDRAWS];           // replay draw ids 10000+...
static struct residency g_res_frame_vbuf[MAXDRAWS];           // replay fallback ids 200+k
static struct residency g_res_frame_ibo[MAXDRAWS];            // replay fallback ids 300+k
static int g_res_reset;                       // set on a host RE-connect (cache went empty) → re-emit all
static int g_no_delta = -1;                   // HL_NO_DELTA A/B gate (−1 unresolved)
static int delta_on(void) {
    if (g_no_delta < 0) g_no_delta = getenv("HL_NO_DELTA") ? 1 : 0;
    return !g_no_delta;
}
static void l5_reset_residency(void) {
    memset(g_res_vbuf, 0, sizeof g_res_vbuf);
    memset(&g_res_index, 0, sizeof g_res_index);
    memset(g_res_tex, 0, sizeof g_res_tex);
    memset(g_res_tex_replay, 0, sizeof g_res_tex_replay);
    memset(g_res_replay_vbuf, 0, sizeof g_res_replay_vbuf);
    memset(g_res_replay_ibo, 0, sizeof g_res_replay_ibo);
    memset(g_res_frame_vbuf, 0, sizeof g_res_frame_vbuf);
    memset(g_res_frame_ibo, 0, sizeof g_res_frame_ibo);
}

// ---- HL_RENDER_PROF: env-gated per-frame frame-time ledger (mirrors HL_SHIM_DEBUG getenv-once) ----
static int g_prof = -1;      // -1 = unresolved, 0 = off, 1 = on
static FILE *g_prof_f;
static uint64_t g_prof_seq;
static uint64_t g_prof_last_gl0; // previous frame's t_gl0 (for the frame period → FPS)
static uint64_t now_us(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000ull + (uint64_t)ts.tv_nsec / 1000ull;
}
static int prof_on(void) {
    if (g_prof < 0) {
        g_prof = getenv("HL_RENDER_PROF") ? 1 : 0;
        if (g_prof) {
            const char *dir = getenv("HL_RENDER_PROF_DIR");
            if (!dir) dir = "/tmp";
            char path[512];
            snprintf(path, sizeof path, "%s/shim-%d.csv", dir, (int)getpid());
            g_prof_f = fopen(path, "w");
            if (g_prof_f) {
                fprintf(g_prof_f, "seq,encode_us,exec_rtt_us,commit_us,frame_us,ir_bytes,connects\n");
                fflush(g_prof_f);
            } else {
                g_prof = 0;
            }
        }
    }
    return g_prof;
}

// ======================= hl-gpu IR wire (matches hl-gpu/src/wire.rs) =======================
// Chrome can upload multi-megabyte GPU-raster textures in a single frame, so the IR stream must grow.
// Never emit a length prefix unless the payload bytes can also be appended; otherwise the host decoder
// sees a well-formed length followed by a truncated stream.
static uint8_t *ir;
static size_t irn, ircap;
static int ireserve(size_t add) {
    if (add > SIZE_MAX - irn) return 0;
    size_t need = irn + add;
    if (need <= ircap) return 1;
    size_t nc = ircap ? ircap : (8u << 20);
    while (nc < need) {
        if (nc > SIZE_MAX / 2) { nc = need; break; }
        nc *= 2;
    }
    uint8_t *p = (uint8_t *)realloc(ir, nc);
    if (!p) {
        fprintf(stderr, "gl_shim: IR realloc failed (%zu -> %zu)\n", ircap, nc);
        return 0;
    }
    ir = p;
    ircap = nc;
    return 1;
}
static void iu8(uint8_t v) { if (!ireserve(1)) return; ir[irn++] = v; }
static void iu32(uint32_t v) { if (!ireserve(4)) return; memcpy(ir + irn, &v, 4); irn += 4; }
static void iu64(uint64_t v) { if (!ireserve(8)) return; memcpy(ir + irn, &v, 8); irn += 8; }
static void ifl(float v) { if (!ireserve(4)) return; memcpy(ir + irn, &v, 4); irn += 4; }
static void istr(const char *s) { uint32_t l = (uint32_t)strlen(s); if (!ireserve(4u + l)) return; iu32(l); memcpy(ir + irn, s, l); irn += l; }
static void ibytes(const uint8_t *b, uint32_t l) { if (!ireserve(4u + l)) return; iu32(l); memcpy(ir + irn, b, l); irn += l; }
static int write_full(int fd, const void *buf, size_t len) {
    const uint8_t *p = (const uint8_t *)buf;
    while (len > 0) {
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
// CreateShader{id, words}: pack the MSL source as bytes (word[0]=len, then 4 bytes/word).
static void ir_shader(uint32_t id, const char *msl) {
    uint32_t len = (uint32_t)strlen(msl);
    uint32_t nwords = 1 + (len + 3) / 4;
    iu8(8); iu32(id); iu32(nwords); iu32(len);
    for (uint32_t i = 0; i < nwords - 1; i++) {
        uint32_t w = 0;
        uint32_t rem = len - i * 4;
        memcpy(&w, msl + i * 4, rem < 4 ? rem : 4);
        iu32(w);
    }
}
static uint32_t blend_factor_wire(GLenum f) {
    switch (f) {
        case GL_ZERO: return 0;
        case GL_ONE: return 1;
        case GL_SRC_COLOR: return 2;
        case GL_ONE_MINUS_SRC_COLOR: return 3;
        case GL_SRC_ALPHA: return 4;
        case GL_ONE_MINUS_SRC_ALPHA: return 5;
        case GL_DST_COLOR: return 6;
        case GL_ONE_MINUS_DST_COLOR: return 7;
        case GL_DST_ALPHA: return 8;
        case GL_ONE_MINUS_DST_ALPHA: return 9;
        case GL_SRC_ALPHA_SATURATE: return 10;
        default: return 1;
    }
}
static uint32_t blend_op_wire(GLenum e) {
    switch (e) {
        case GL_FUNC_SUBTRACT: return 1;
        case GL_FUNC_REVERSE_SUBTRACT: return 2;
        case GL_MIN: return 3;
        case GL_MAX: return 4;
        case GL_FUNC_ADD:
        default: return 0;
    }
}
static uint32_t vertex_format_wire(GLenum type, int comps, int normalized, int integer) {
    if (comps < 1) comps = 1;
    if (comps > 4) comps = 4;
    uint32_t kind;
    switch (type) {
        case GL_UNSIGNED_BYTE: kind = 1; break;
        case GL_BYTE: kind = 2; break;
        case GL_UNSIGNED_SHORT: kind = 3; break;
        case GL_SHORT: kind = 4; break;
        case GL_UNSIGNED_INT: kind = 5; break;
        case GL_INT: kind = 6; break;
        case GL_HALF_FLOAT: kind = 7; break; /* ES3 16-bit float (GskGL); host maps kind 7 → half */
        case GL_FLOAT:
        default: kind = 0; break;
    }
    // glVertexAttribPointer converts fixed-point attributes to floats; integer shader inputs must come
    // from glVertexAttribIPointer. Preserve both cases so the Metal vertex descriptor matches MSL.
    return (uint32_t)comps | (kind << 8) | ((normalized ? 1u : 0u) << 16) | ((integer ? 1u : 0u) << 17);
}
static uint32_t decl_format_wire(const char *t) {
    int comps = strstr(t, "vec2") ? 2 : strstr(t, "vec3") ? 3 : !strncmp(t, "float", 5) ? 1 : 4;
    int integer = !strncmp(t, "ivec", 4) || !strncmp(t, "uvec", 4);
    uint32_t kind = !strncmp(t, "ivec", 4) ? 6u : !strncmp(t, "uvec", 4) ? 5u : 0u;
    return (uint32_t)comps | (kind << 8) | ((integer ? 1u : 0u) << 17);
}
static uint32_t color_target_format(GLuint target) {
    return target ? 1u : 2u; // offscreen GL textures are Rgba8Unorm; IOSurface-backed window is Bgra8Unorm.
}
static void emit_color_target_fmt(uint32_t fmt, int blend, GLenum src_rgb, GLenum dst_rgb, GLenum eq_rgb,
                                  GLenum src_alpha, GLenum dst_alpha, GLenum eq_alpha, uint32_t mask) {
    iu32(fmt);
    if (blend) {
        iu8(1);
        iu32(blend_factor_wire(src_rgb));
        iu32(blend_factor_wire(dst_rgb));
        iu32(blend_op_wire(eq_rgb));
        iu32(blend_factor_wire(src_alpha));
        iu32(blend_factor_wire(dst_alpha));
        iu32(blend_op_wire(eq_alpha));
    } else {
        iu8(0);
    }
    iu32(mask & 0xf);
}
static void emit_color_target(int blend, GLenum src_rgb, GLenum dst_rgb, GLenum eq_rgb,
                              GLenum src_alpha, GLenum dst_alpha, GLenum eq_alpha, uint32_t mask) {
    emit_color_target_fmt(2u, blend, src_rgb, dst_rgb, eq_rgb, src_alpha, dst_alpha, eq_alpha, mask);
}
static void emit_viewport_h(const int vp[4], int target_h) {
    if (target_h <= 0) target_h = (int)g_surf.height;
    float x = 0.0f, y = 0.0f, w = (float)g_surf.width, h = (float)target_h;
    if (vp && vp[2] > 0 && vp[3] > 0) {
        x = (float)vp[0];
        w = (float)vp[2];
        h = (float)vp[3];
        y = (float)(target_h - vp[1] - vp[3]);
    }
    iu8(7); ifl(x); ifl(y); ifl(w); ifl(h); ifl(0.0f); ifl(1.0f);
}
static void emit_viewport(const int vp[4]) { emit_viewport_h(vp, (int)g_surf.height); }
static void emit_scissor_h(int enabled, const int sc[4], int target_w, int target_h) {
    if (target_w <= 0) target_w = (int)g_surf.width;
    if (target_h <= 0) target_h = (int)g_surf.height;
    int x = 0, y = 0, w = target_w, h = target_h;
    if (enabled && sc && sc[2] > 0 && sc[3] > 0) {
        x = sc[0];
        y = target_h - sc[1] - sc[3];
        w = sc[2];
        h = sc[3];
    }
    if (x < 0) { w += x; x = 0; }
    if (y < 0) { h += y; y = 0; }
    if (x > target_w) x = target_w;
    if (y > target_h) y = target_h;
    if (x + w > target_w) w = target_w - x;
    if (y + h > target_h) h = target_h - y;
    if (w < 0) w = 0;
    if (h < 0) h = 0;
    iu8(16); iu32((uint32_t)x); iu32((uint32_t)y); iu32((uint32_t)w); iu32((uint32_t)h);
}
static void emit_scissor(int enabled, const int sc[4]) { emit_scissor_h(enabled, sc, (int)g_surf.width, (int)g_surf.height); }
static uint32_t tex_ir_id(GLuint tex);
static int draw_target_w(GLuint tex);
static int draw_target_h(GLuint tex);
static size_t attr_elem_size(GLenum type);
static int draw_vbo_snapshot_index(const struct draw_call *d, GLuint src);
static void free_draw_snapshots(void);
static int draw_vbo_slots(const struct draw_call *d, int slot_vbo[MAXATTR],
                          int attr_slot[MAXATTR], uint32_t slot_stride[MAXATTR]);
static void emit_clear_rect(const struct draw_call *d) {
    GLuint target = d->target_tex;
    if (target >= MAXTEX || !g_tex[target].used) target = 0;
    int tw = draw_target_w(target), th = draw_target_h(target);
    int x = d->clear_rect[0], y = th - d->clear_rect[1] - d->clear_rect[3];
    int w = d->clear_rect[2], h = d->clear_rect[3];
    if (x < 0) { w += x; x = 0; }
    if (y < 0) { h += y; y = 0; }
    if (x > tw) x = tw;
    if (y > th) y = th;
    if (x + w > tw) w = tw - x;
    if (y + h > th) h = th - y;
    if (w < 0) w = 0;
    if (h < 0) h = 0;
    iu8(17);
    iu32(target ? tex_ir_id(target) : 1);
    iu32((uint32_t)x); iu32((uint32_t)y); iu32((uint32_t)w); iu32((uint32_t)h);
    ifl(d->clear[0]); ifl(d->clear[1]); ifl(d->clear[2]); ifl(d->clear[3]);
}
static uint32_t tex_ir_id(GLuint tex) { return 500u + (uint32_t)tex; }
static uint32_t sampler_ir_id(GLuint tex) { return 600u + (uint32_t)tex; }
static uint32_t stage_ir_id(GLuint tex) { return 700u + (uint32_t)tex; }
static uint32_t replay_vbo_ir_id(int draw_index, int slot) {
    return 2000u + (uint32_t)draw_index * (uint32_t)MAXATTR + (uint32_t)slot;
}
static uint32_t replay_ibo_ir_id(int draw_index) {
    return 10000u + (uint32_t)draw_index;
}
static const char *g_gl_exts[] = {
    // Chromium's SharedImage GL texture factory enables BGRA_8888 raster/display images from these caps.
    "GL_EXT_texture_format_BGRA8888",
    "GL_APPLE_texture_format_BGRA8888",
    "GL_EXT_texture_storage",
    "GL_OES_rgb8_rgba8",
    "GL_OES_texture_npot",
    "GL_EXT_sRGB",
    "GL_EXT_sRGB_write_control",
    "GL_ANGLE_framebuffer_multisample",
    "GL_ANGLE_texture_usage",
};
static const int g_gl_nexts = (int)(sizeof(g_gl_exts) / sizeof(g_gl_exts[0]));
static const char *gl_extensions_string(void) {
    static char buf[512];
    if (!buf[0]) {
        size_t off = 0;
        for (int i = 0; i < g_gl_nexts; i++) {
            int n = snprintf(buf + off, sizeof(buf) - off, "%s%s", i ? " " : "", g_gl_exts[i]);
            if (n < 0) break;
            off += (size_t)n;
            if (off >= sizeof(buf)) { buf[sizeof(buf) - 1] = 0; break; }
        }
    }
    return buf;
}
static int draw_target_w(GLuint tex) {
    return (tex > 0 && tex < MAXTEX && g_tex[tex].used && g_tex[tex].w > 0) ? g_tex[tex].w : (int)g_surf.width;
}
static int draw_target_h(GLuint tex) {
    return (tex > 0 && tex < MAXTEX && g_tex[tex].used && g_tex[tex].h > 0) ? g_tex[tex].h : (int)g_surf.height;
}

// ======================= minimal GLSL-ES → MSL translator =======================
// Handles passthrough shaders: `attribute TYPE name;`, `varying TYPE name;`, main() writing gl_Position /
// a varying (vertex) and gl_FragColor (fragment), with vecN constructors. Enough for a simple app; more
// (uniforms, textures, control flow, builtins) is the ICD long tail (RENDERING_PLAN.md M5).
struct decl {
    char type[16], name[32];
};
static void gl_type_to_msl(const char *t, char *out) {
    if (!strcmp(t, "vec2")) strcpy(out, "float2");
    else if (!strcmp(t, "vec3")) strcpy(out, "float3");
    else if (!strcmp(t, "vec4")) strcpy(out, "float4");
    else if (!strcmp(t, "ivec2")) strcpy(out, "int2");
    else if (!strcmp(t, "ivec3")) strcpy(out, "int3");
    else if (!strcmp(t, "ivec4")) strcpy(out, "int4");
    else if (!strcmp(t, "uvec2")) strcpy(out, "uint2");
    else if (!strcmp(t, "uvec3")) strcpy(out, "uint3");
    else if (!strcmp(t, "uvec4")) strcpy(out, "uint4");
    else if (!strcmp(t, "mat2") || !strcmp(t, "mat2x2")) strcpy(out, "float2x2");
    else if (!strcmp(t, "mat3") || !strcmp(t, "mat3x3")) strcpy(out, "float3x3");
    else if (!strcmp(t, "mat4") || !strcmp(t, "mat4x4")) strcpy(out, "float4x4");
    else if (!strcmp(t, "mat2x3")) strcpy(out, "float2x3");
    else if (!strcmp(t, "mat3x2")) strcpy(out, "float3x2");
    else if (!strcmp(t, "mat2x4")) strcpy(out, "float2x4");
    else if (!strcmp(t, "mat4x2")) strcpy(out, "float4x2");
    else if (!strcmp(t, "mat3x4")) strcpy(out, "float3x4");
    else if (!strcmp(t, "mat4x3")) strcpy(out, "float4x3");
    else strcpy(out, t);
}
static int is_space(char c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r';
}
static int is_word(char c) {
    return c == '_' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9');
}
static int is_precision_or_interp(const char *t) {
    return !strcmp(t, "lowp") || !strcmp(t, "mediump") || !strcmp(t, "highp") ||
           !strcmp(t, "flat") || !strcmp(t, "smooth") || !strcmp(t, "centroid");
}
// collect `kw TYPE name;` decls from src
static int collect(const char *src, const char *kw, struct decl *out, int max) {
    int n = 0;
    const char *p = src;
    size_t kl = strlen(kw);
    while ((p = strstr(p, kw)) && n < max) {
        // must be at a word boundary
        if ((p != src && is_word(p[-1])) || is_word(p[kl])) { p += kl; continue; }
        const char *q = p + kl;
        while (is_space(*q)) q++;
        char ty[16] = {0};
        int i = 0;
        while (*q && !is_space(*q) && *q != ';' && i < 15) ty[i++] = *q++;
        ty[i] = 0;
        while (is_precision_or_interp(ty)) {
            while (is_space(*q)) q++;
            i = 0;
            memset(ty, 0, sizeof ty);
            while (*q && !is_space(*q) && *q != ';' && i < 15) ty[i++] = *q++;
            ty[i] = 0;
        }
        while (is_space(*q)) q++;
        // std140 interface block: `uniform Block { TYPE m; ... } [inst];` — enumerate the MEMBERS as
        // the collected decls (not the block name / not dropped). `ty` held the block name. Members with
        // no instance name are referenced by bare name in the body, so they flow through the existing
        // uniform pipeline (one `struct Uniforms` at [[buffer(1)]], `u.<member>`).
        if (*q == '{') {
            q++;
            while (n < max) {
                while (is_space(*q)) q++;
                if (*q == '}' || !*q) break;
                char mty[16] = {0};
                int mi = 0;
                while (*q && !is_space(*q) && *q != ';' && mi < 15) mty[mi++] = *q++;
                mty[mi] = 0;
                while (is_precision_or_interp(mty)) {
                    while (is_space(*q)) q++;
                    mi = 0;
                    memset(mty, 0, sizeof mty);
                    while (*q && !is_space(*q) && *q != ';' && mi < 15) mty[mi++] = *q++;
                    mty[mi] = 0;
                }
                while (is_space(*q)) q++;
                char mnm[32] = {0};
                mi = 0;
                while (*q && is_word(*q) && mi < 31) mnm[mi++] = *q++;
                while (*q && *q != ';' && *q != '}') q++; // skip any array subscript to the member end
                if (*q == ';') q++;
                if (mty[0] && mnm[0]) {
                    strcpy(out[n].type, mty);
                    strcpy(out[n].name, mnm);
                    n++;
                }
            }
            if (*q == '}') q++;
            while (*q && *q != ';') q++; // skip the optional instance name
            if (*q == ';') q++;
            p = q;
            continue;
        }
        char nm[32] = {0};
        i = 0;
        while (*q && is_word(*q) && i < 31) nm[i++] = *q++;
        if (ty[0] && nm[0]) {
            strcpy(out[n].type, ty);
            strcpy(out[n].name, nm);
            n++;
        }
        p = q;
    }
    return n;
}
// extract the body between `void main(){` and the matching final `}`
static void main_body(const char *src, char *out, size_t cap) {
    const char *p = strstr(src, "main");
    if (p) p = strchr(p, '{');
    const char *e = p ? strrchr(src, '}') : NULL;
    if (p && e && e > p) {
        size_t n = e - (p + 1);
        if (n >= cap) n = cap - 1;
        memcpy(out, p + 1, n);
        out[n] = 0;
    } else {
        out[0] = 0;
    }
}
// word-boundary replace `from` with `to` in buf (in place, bounded)
static void wreplace(char *buf, const char *from, const char *to) {
    char tmp[4096];
    size_t fl = strlen(from), tl = strlen(to), o = 0;
    for (size_t i = 0; buf[i] && o < sizeof(tmp) - tl - 1;) {
        if (!strncmp(buf + i, from, fl)) {
            char b = i ? buf[i - 1] : ' ';
            char a = buf[i + fl];
            int bw = (b == '_' || (b >= 'a' && b <= 'z') || (b >= 'A' && b <= 'Z') || (b >= '0' && b <= '9'));
            int aw = (a == '_' || (a >= 'a' && a <= 'z') || (a >= 'A' && a <= 'Z') || (a >= '0' && a <= '9'));
            if (!bw && !aw) {
                memcpy(tmp + o, to, tl);
                o += tl;
                i += fl;
                continue;
            }
        }
        tmp[o++] = buf[i++];
    }
    tmp[o] = 0;
    strcpy(buf, tmp);
}
static void sreplace(char *buf, const char *from, const char *to); // defined below; used for mat3x2( ctor
static void type_fixups(char *b) {
    wreplace(b, "lowp", "");
    wreplace(b, "mediump", "");
    wreplace(b, "highp", "");
    wreplace(b, "vec2", "float2");
    wreplace(b, "vec3", "float3");
    wreplace(b, "vec4", "float4");
    wreplace(b, "ivec2", "int2");
    wreplace(b, "ivec3", "int3");
    wreplace(b, "ivec4", "int4");
    wreplace(b, "uvec2", "uint2");
    wreplace(b, "uvec3", "uint3");
    wreplace(b, "uvec4", "uint4");
    // Non-square forms first (word-boundary replace already protects mat3 inside mat3x2, but list all).
    // GLSL's `mat3x2(m3)` truncating constructor has NO MSL equivalent — MSL forbids building a matrix
    // from a larger one, so `float3x2(float3x3)` fails to compile and the whole (gradient) shader falls
    // back to the builtin → a black, mispositioned block. Route the CONSTRUCTOR call to a hl_mat3x2()
    // helper (injected at file scope) that extracts the upper-left 3×2 block; bare `mat3x2` type
    // declarations still map to `float3x2` via the wreplace below (hl_mat3x2 is protected — the leading
    // `_` is a word char, so the word-boundary wreplace won't rewrite the `mat3x2` inside it).
    sreplace(b, "mat3x2(", "hl_mat3x2(");
    wreplace(b, "mat2x2", "float2x2");
    wreplace(b, "mat2x3", "float2x3");
    wreplace(b, "mat2x4", "float2x4");
    wreplace(b, "mat3x2", "float3x2");
    wreplace(b, "mat3x3", "float3x3");
    wreplace(b, "mat3x4", "float3x4");
    wreplace(b, "mat4x2", "float4x2");
    wreplace(b, "mat4x3", "float4x3");
    wreplace(b, "mat4x4", "float4x4");
    wreplace(b, "mat2", "float2x2");
    wreplace(b, "mat3", "float3x3");
    wreplace(b, "mat4", "float4x4");
}
// Plain (non-word-boundary) substring replace of `from`→`to` in buf, bounded.
static void sreplace(char *buf, const char *from, const char *to) {
    char tmp[8192];
    size_t fl = strlen(from), tl = strlen(to), o = 0;
    for (size_t i = 0; buf[i] && o < sizeof(tmp) - tl - 1;) {
        if (!strncmp(buf + i, from, fl)) { memcpy(tmp + o, to, tl); o += tl; i += fl; continue; }
        tmp[o++] = buf[i++];
    }
    tmp[o] = 0;
    strcpy(buf, tmp);
}
static void call2_fixup(char *buf, const char *fn, const char *op) {
    char tmp[8192];
    size_t fl = strlen(fn), opl = strlen(op), o = 0;
    for (size_t i = 0; buf[i] && o < sizeof(tmp) - 1;) {
        if (!strncmp(buf + i, fn, fl) && buf[i + fl] == '(') {
            char b = i ? buf[i - 1] : ' ';
            if (!is_word(b)) {
                size_t a0 = i + fl + 1, j = a0, comma = 0;
                int depth = 1;
                while (buf[j] && depth) {
                    if (buf[j] == '(') depth++;
                    else if (buf[j] == ')') { depth--; if (!depth) break; }
                    else if (buf[j] == ',' && depth == 1 && !comma) comma = j;
                    j++;
                }
                if (buf[j] == ')' && comma) {
                    size_t need = 4 + (comma - a0) + opl + (j - comma - 1) + 4;
                    if (o + need < sizeof(tmp) - 1) {
                        tmp[o++] = '('; tmp[o++] = '(';
                        memcpy(tmp + o, buf + a0, comma - a0); o += comma - a0;
                        tmp[o++] = ')'; tmp[o++] = ' ';
                        memcpy(tmp + o, op, opl); o += opl;
                        tmp[o++] = ' '; tmp[o++] = '(';
                        memcpy(tmp + o, buf + comma + 1, j - comma - 1); o += j - comma - 1;
                        tmp[o++] = ')'; tmp[o++] = ')';
                        i = j + 1;
                        continue;
                    }
                }
            }
        }
        tmp[o++] = buf[i++];
    }
    tmp[o] = 0;
    strcpy(buf, tmp);
}
static void relational_fixups(char *b) {
    call2_fixup(b, "greaterThanEqual", ">=");
    call2_fixup(b, "lessThanEqual", "<=");
    call2_fixup(b, "greaterThan", ">");
    call2_fixup(b, "lessThan", "<");
    call2_fixup(b, "notEqual", "!=");
    call2_fixup(b, "equal", "==");
}
// Rename a builtin call `fn(a, b, …)` → `to(a, b, …)` only when it has a top-level comma (2+ args). Used
// for GLSL `atan(y,x)` → MSL `atan2(y,x)` while leaving the 1-arg `atan(x)` (same name in MSL) untouched.
static void rename_call2(char *buf, const char *fn, const char *to) {
    char tmp[8192];
    size_t fl = strlen(fn), tl = strlen(to), o = 0;
    for (size_t i = 0; buf[i] && o < sizeof(tmp) - tl - 2;) {
        if (!strncmp(buf + i, fn, fl) && buf[i + fl] == '(' && (i == 0 || !is_word(buf[i - 1]))) {
            size_t j = i + fl + 1;
            int depth = 1, comma = 0;
            while (buf[j] && depth) {
                if (buf[j] == '(') depth++;
                else if (buf[j] == ')') { depth--; if (!depth) break; }
                else if (buf[j] == ',' && depth == 1) comma = 1;
                j++;
            }
            if (comma) { memcpy(tmp + o, to, tl); o += tl; i += fl; continue; }
        }
        tmp[o++] = buf[i++];
    }
    tmp[o] = 0;
    strcpy(buf, tmp);
}
// GLSL-ES builtins whose MSL spelling differs. Missing these makes the whole shader fail to compile, so the
// program never links and its draws silently VANISH. `mod` has NO MSL builtin (fmod differs on negatives) —
// it's provided by hl_mod overloads injected at file scope (see translate()).
static void builtin_fixups(char *b) {
    wreplace(b, "dFdx", "dfdx");           // MSL derivatives are lowercase
    wreplace(b, "dFdy", "dfdy");
    wreplace(b, "inversesqrt", "rsqrt");   // GLSL inversesqrt → MSL rsqrt
    rename_call2(b, "atan", "atan2");      // GLSL atan(y,x) → MSL atan2 (1-arg atan kept)
    wreplace(b, "mod", "hl_mod");          // GLSL mod(x,y) = x - y*floor(x/y); MSL has no `mod`
}
// float-scalar/vector GLSL mod() replacement, injected at MSL file scope when a shader uses mod().
static const char *HL_MOD_HELPERS =
    "template<typename T> inline T hl_mod(T x, T y) { return x - y * floor(x / y); }\n"
    "inline float2 hl_mod(float2 x, float y) { return x - y * floor(x / y); }\n"
    "inline float3 hl_mod(float3 x, float y) { return x - y * floor(x / y); }\n"
    "inline float4 hl_mod(float4 x, float y) { return x - y * floor(x / y); }\n";
// GLSL `mat3x2(m3)` truncating-matrix constructor → MSL has none. Provide it: extract the upper-left
// 3×2 block (cols 0..2, rows x/y). The float2×3 column form is passed through unchanged. Skia emits
// `mat3x2(gradientMatrix) * vec3(coord, 1.0)` for linear-gradient/coord transforms; without this the
// shader fails to compile and the draw falls back to the builtin (black, mispositioned).
static const char *HL_MAT3X2_HELPER =
    "inline float3x2 hl_mat3x2(float3x3 m) { return float3x2(m[0].xy, m[1].xy, m[2].xy); }\n"
    "inline float3x2 hl_mat3x2(float2 a, float2 b, float2 c) { return float3x2(a, b, c); }\n";
static void local_decl_fixups(char *b) {
    sreplace(b, "float in.", "float ");
    sreplace(b, "float2 in.", "float2 ");
    sreplace(b, "float3 in.", "float3 ");
    sreplace(b, "float4 in.", "float4 ");
    sreplace(b, "int in.", "int ");
    sreplace(b, "int2 in.", "int2 ");
    sreplace(b, "int3 in.", "int3 ");
    sreplace(b, "int4 in.", "int4 ");
    sreplace(b, "uint in.", "uint ");
    sreplace(b, "uint2 in.", "uint2 ");
    sreplace(b, "uint3 in.", "uint3 ");
    sreplace(b, "uint4 in.", "uint4 ");
}
// MSL has no narrowing vector constructor, so rewrite `vecN( EXPR )` truncations to a swizzle. Heuristic:
// a SINGLE top-level argument (no top-level comma) containing a top-level `*` (matrix/vector product) is a
// vector being truncated → `(EXPR).xyz`/`.xy`. Splats like `vec3(1.0)` (no `*`) are left untouched. Runs on
// the GLSL `vecN(` form BEFORE type_fixups. Handles glmark2's `vec3(NormalMatrix * vec4(normal, 1.0))`.
static void fix_trunc(char *buf) {
    char out[8192];
    size_t o = 0;
    for (size_t i = 0; buf[i] && o < sizeof(out) - 8;) {
        int n = 0;
        if (!strncmp(buf + i, "vec2(", 5)) n = 2;
        else if (!strncmp(buf + i, "vec3(", 5)) n = 3;
        if (n) {
            char b = i ? buf[i - 1] : ' ';
            if (b == '_' || (b >= 'a' && b <= 'z') || (b >= 'A' && b <= 'Z') || (b >= '0' && b <= '9')) n = 0;
        }
        if (n) {
            size_t start = i + 5, j = start;
            int depth = 1, topcomma = 0, topstar = 0;
            while (buf[j] && depth) {
                if (buf[j] == '(') depth++;
                else if (buf[j] == ')') { depth--; if (!depth) break; }
                else if (depth == 1 && buf[j] == ',') topcomma = 1;
                else if (depth == 1 && buf[j] == '*') topstar = 1;
                j++;
            }
            if (buf[j] == ')' && !topcomma && topstar) {
                out[o++] = '(';
                memcpy(out + o, buf + start, j - start);
                o += j - start;
                out[o++] = ')';
                out[o++] = '.'; out[o++] = 'x'; out[o++] = 'y';
                if (n == 3) out[o++] = 'z';
                i = j + 1;
                continue;
            }
        }
        out[o++] = buf[i++];
    }
    out[o] = 0;
    strcpy(buf, out);
}
// Collect global `const TYPE name = …;` declarations that appear BEFORE main() — glmark2 prepends light/
// material/PI constants this way (ShaderSource::add_const). Emitted at MSL global scope as `constant …`.
static int collect_consts(const char *src, char out[][256], int max) {
    const char *end = strstr(src, "main");
    int n = 0;
    const char *p = src;
    while (n < max && (p = strstr(p, "const"))) {
        if (end && p >= end) break;
        char b = p == src ? ' ' : p[-1];
        char a = p[5];
        int bw = (b == '_' || (b >= 'a' && b <= 'z') || (b >= 'A' && b <= 'Z') || (b >= '0' && b <= '9'));
        if (bw || (a != ' ' && a != '\t')) { p += 5; continue; }
        const char *semi = strchr(p, ';');
        if (!semi) break;
        size_t len = semi - p + 1;
        if (len < 256) {
            memcpy(out[n], p, len);
            out[n][len] = 0;
            n++;
        }
        p = semi + 1;
    }
    return n;
}
// Translate a vertex + fragment GLSL pair into ONE combined MSL source (shared VOut + a Uniforms block at
// [[buffer(1)]] read by both stages). Returns malloc'd. Handles passthrough + uniform/matrix shaders that
// use explicit swizzles (.xyz); GLSL implicit truncation like vec3(vec4) is NOT handled (see M5).
// Strip GLSL `//` and `/* */` comments in place (so a keyword inside a comment — e.g. the word "varying"
// in glmark2's light-basic.vert — isn't mis-parsed as a declaration, and `}` in a comment doesn't fool
// main_body's brace scan).
static void strip_comments(char *s) {
    char *w = s;
    for (char *r = s; *r;) {
        if (r[0] == '/' && r[1] == '/') {
            while (*r && *r != '\n') r++;
        } else if (r[0] == '/' && r[1] == '*') {
            r += 2;
            while (*r && !(r[0] == '*' && r[1] == '/')) r++;
            if (*r) r += 2;
        } else {
            *w++ = *r++;
        }
    }
    *w = 0;
}
static int is_sampler_type(const char *t) {
    return !strcmp(t, "sampler2D") || !strcmp(t, "samplerCube") || !strcmp(t, "sampler2DShadow");
}
// Collect uniforms from vs+fs (dedup by name), then split into DATA uniforms (go in the Metal Uniforms
// block at [[buffer(1)]]) and SAMPLER uniforms (become texture2d<float>+sampler params). Returns data count
// via *ndata, sampler decls via samps/*nsamp.
static int collect_uniforms(const char *vs, const char *fs, struct decl *data, int *ndata,
                            struct decl *samps, int *nsamp) {
    struct decl all[32];
    int n = collect(vs, "uniform", all, 32);
    struct decl ufs[32];
    int nf = collect(fs, "uniform", ufs, 32);
    for (int i = 0; i < nf && n < 32; i++) {
        int dup = 0;
        for (int j = 0; j < n; j++) if (!strcmp(all[j].name, ufs[i].name)) dup = 1;
        if (!dup) all[n++] = ufs[i];
    }
    int nd = 0, ns = 0;
    for (int i = 0; i < n; i++) {
        if (is_sampler_type(all[i].type)) { if (ns < 4) samps[ns++] = all[i]; }
        else if (nd < 16) data[nd++] = all[i];
    }
    *ndata = nd;
    *nsamp = ns;
    return n;
}
// Emit the per-stage texture/sampler params for the samplers this stage's body references.
// translate()'s MSL output buffer capacity. Every write is bounds-checked against this (see MSLCAT), so a
// large shader can never overflow the allocation (was a fixed 32 KB with unbounded sprintf).
#define TRANSLATE_OUTCAP 65536
// Bounds-checked append into out[.. cap] at offset o; returns the new offset (clamped so it never exceeds
// cap-1 and never writes past the allocation, even on truncation).
static size_t cat_msl(char *out, size_t o, size_t cap, const char *fmt, ...) {
    if (o >= cap) return o;
    va_list ap; va_start(ap, fmt);
    int r = vsnprintf(out + o, cap - o, fmt, ap);
    va_end(ap);
    if (r < 0) return o;
    size_t rem = cap - o;
    return o + ((size_t)r < rem ? (size_t)r : rem - 1);
}
static void dump_text_file(const char *dir, const char *name, const char *text) {
    if (!dir || !dir[0] || !name || !text) return;
    mkdir(dir, 0755);
    char path[512];
    snprintf(path, sizeof path, "%s/%s", dir, name);
    FILE *f = fopen(path, "wb");
    if (!f) return;
    fwrite(text, 1, strlen(text), f);
    fclose(f);
}
static void dump_tex_ppm(GLuint id, const struct tex *t, const char *tag) {
    const char *dir = getenv("HL_TEXTURE_DUMP_DIR");
    static int seq;
    if (!dir || !dir[0] || !t || !t->data || t->w <= 0 || t->h <= 0 || seq >= 96) return;
    char path[512];
    snprintf(path, sizeof path, "%s/tex-%03d-id%u-%s-%dx%d.ppm", dir, seq++, id, tag ? tag : "upload", t->w, t->h);
    FILE *f = fopen(path, "wb");
    if (!f) return;
    fprintf(f, "P6\n%d %d\n255\n", t->w, t->h);
    for (int y = 0; y < t->h; y++) {
        for (int x = 0; x < t->w; x++) {
            const uint8_t *p = t->data + ((size_t)y * (size_t)t->w + (size_t)x) * 4u;
            fwrite(p, 1, 3, f);
        }
    }
    fclose(f);
    snprintf(path, sizeof path, "%s/tex-%03d-id%u-%s-%dx%d-alpha.ppm", dir, seq - 1, id, tag ? tag : "upload", t->w, t->h);
    f = fopen(path, "wb");
    if (!f) return;
    fprintf(f, "P6\n%d %d\n255\n", t->w, t->h);
    for (int y = 0; y < t->h; y++) {
        for (int x = 0; x < t->w; x++) {
            const uint8_t *p = t->data + ((size_t)y * (size_t)t->w + (size_t)x) * 4u;
            uint8_t a[3] = { p[3], p[3], p[3] };
            fwrite(a, 1, 3, f);
        }
    }
    fclose(f);
}
static size_t emit_samp_params(char *out, size_t o, const char *body, struct decl *samps, int ns) {
    for (int i = 0; i < ns; i++) {
        if (!strstr(body, samps[i].name)) continue;
        o = cat_msl(out, o, TRANSLATE_OUTCAP, ", texture2d<float> %s [[texture(%d)]], sampler %sSmplr [[sampler(%d)]]",
                     samps[i].name, i, samps[i].name, i);
    }
    return o;
}
// texture2D(NAME, uv) / texture(NAME, uv) → NAME.sample(NAMESmplr, uv)
static void sampler_fixups(char *b, struct decl *samps, int ns) {
    for (int i = 0; i < ns; i++) {
        char from[80], to[96];
        sprintf(from, "texture2D(%s", samps[i].name);
        sprintf(to, "%s.sample(%sSmplr", samps[i].name, samps[i].name);
        sreplace(b, from, to);
        sprintf(from, "texture(%s", samps[i].name);
        sreplace(b, from, to);
    }
}
static int has_decl_named(struct decl *decls, int n, const char *name) {
    for (int i = 0; i < n; i++) if (!strcmp(decls[i].name, name)) return 1;
    return 0;
}
static void append_decls_unique(struct decl *dst, int *ndst, int max, struct decl *src, int nsrc) {
    for (int i = 0; i < nsrc && *ndst < max; i++) {
        if (has_decl_named(dst, *ndst, src[i].name)) continue;
        dst[(*ndst)++] = src[i];
    }
}
static int collect_vertex_attrs(const char *vs, struct decl *attrs, int max) {
    struct decl tmp[16];
    int na = collect(vs, "attribute", attrs, max);
    int ntmp = collect(vs, "in", tmp, 16);
    append_decls_unique(attrs, &na, max, tmp, ntmp);
    return na;
}
static char *translate(const char *vs_in, const char *fs_in) {
    char vsbuf[16384], fsbuf[16384];
    snprintf(vsbuf, sizeof vsbuf, "%s", vs_in);
    snprintf(fsbuf, sizeof fsbuf, "%s", fs_in);
    strip_comments(vsbuf);
    strip_comments(fsbuf);
    const char *vs = vsbuf, *fs = fsbuf;
    struct decl attrs[16], vary[16], unis[16], samps[4], tmp[16], fragouts[4];
    int na = collect_vertex_attrs(vs, attrs, 16);
    int nv = collect(vs, "varying", vary, 16);
    int ntmp = collect(vs, "out", tmp, 16);
    append_decls_unique(vary, &nv, 16, tmp, ntmp);
    int nfragout = collect(fs, "out", fragouts, 4);
    int nu, nsamp;
    collect_uniforms(vs, fs, unis, &nu, samps, &nsamp);
    char consts[16][256];
    int nc = collect_consts(vs, consts, 16);
    { // add fs-only consts (dedup by raw text)
        char cf[16][256];
        int ncf = collect_consts(fs, cf, 16);
        for (int i = 0; i < ncf && nc < 16; i++) {
            int dup = 0;
            for (int j = 0; j < nc; j++) if (!strcmp(consts[j], cf[i])) dup = 1;
            if (!dup) strcpy(consts[nc++], cf[i]);
        }
    }
    char *out = malloc(TRANSLATE_OUTCAP);
    if (!out) return NULL;
    size_t o = 0;
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "#include <metal_stdlib>\nusing namespace metal;\n");
    // Inject GLSL mod() helper overloads (MSL has no `mod`) only when a shader actually uses mod(…).
    if (strstr(vs, "mod(") || strstr(fs, "mod(")) o = cat_msl(out, o, TRANSLATE_OUTCAP, "%s", HL_MOD_HELPERS);
    if (strstr(vs, "mat3x2(") || strstr(fs, "mat3x2(")) o = cat_msl(out, o, TRANSLATE_OUTCAP, "%s", HL_MAT3X2_HELPER);
    // Global consts (glmark2's prepended light/material/PI): `const …` → `constant …`, types fixed.
    for (int i = 0; i < nc; i++) {
        char line[256];
        strcpy(line, consts[i]);
        type_fixups(line);
        char *kw = strstr(line, "const");
        if (kw == line || (kw && (kw[-1] == ' ' || kw[-1] == '\n'))) {
            o = cat_msl(out, o, TRANSLATE_OUTCAP, "constant %s\n", kw + 5);
        }
    }
    // Uniforms block (declaration order = the offset layout the shim writes)
    int has_u = nu > 0;
    if (has_u) {
        o = cat_msl(out, o, TRANSLATE_OUTCAP, "struct Uniforms {\n");
        for (int i = 0; i < nu; i++) {
            char mt[16];
            gl_type_to_msl(unis[i].type, mt);
            o = cat_msl(out, o, TRANSLATE_OUTCAP, "  %s %s;\n", mt, unis[i].name);
        }
        o = cat_msl(out, o, TRANSLATE_OUTCAP, "};\n");
    }
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "struct VIn {\n");
    for (int i = 0; i < na; i++) {
        char mt[16];
        gl_type_to_msl(attrs[i].type, mt);
        o = cat_msl(out, o, TRANSLATE_OUTCAP, "  %s %s [[attribute(%d)]];\n", mt, attrs[i].name, i);
    }
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "};\n");
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "struct VOut {\n  float4 position [[position]];\n");
    for (int i = 0; i < nv; i++) {
        char mt[16];
        gl_type_to_msl(vary[i].type, mt);
        o = cat_msl(out, o, TRANSLATE_OUTCAP, "  %s %s [[user(v%d)]];\n", mt, vary[i].name, i);
    }
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "};\n");
    const char *uparam = has_u ? ", constant Uniforms& u [[buffer(1)]]" : "";
    // vertex — qualify attribute/varying/uniform identifiers BEFORE the gl_Position rewrite, so an
    // attribute named `position` doesn't corrupt `out.position` (order matters for the word-replace).
    char vb[4096];
    main_body(vs, vb, sizeof vb);
    fix_trunc(vb);
    type_fixups(vb);
    builtin_fixups(vb);
    sampler_fixups(vb, samps, nsamp);
    for (int i = 0; i < na; i++) {
        char in[40]; sprintf(in, "in.%s", attrs[i].name); wreplace(vb, attrs[i].name, in);
    }
    for (int i = 0; i < nv; i++) {
        char ov[40]; sprintf(ov, "out.%s", vary[i].name); wreplace(vb, vary[i].name, ov);
    }
    for (int i = 0; i < nu; i++) {
        char un[40]; sprintf(un, "u.%s", unis[i].name); wreplace(vb, unis[i].name, un);
    }
    wreplace(vb, "gl_Position", "out.position");
    local_decl_fixups(vb);
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "vertex VOut vmain(VIn in [[stage_in]]%s", uparam);
    o = emit_samp_params(out, o, vb, samps, nsamp);
    o = cat_msl(out, o, TRANSLATE_OUTCAP, ") {\n  VOut out;\n%s\n  return out;\n}\n", vb);
    // fragment
    char fb[4096];
    main_body(fs, fb, sizeof fb);
    int frag_uses_coord = strstr(fb, "gl_FragCoord") != NULL;
    fix_trunc(fb);
    type_fixups(fb);
    builtin_fixups(fb);
    sampler_fixups(fb, samps, nsamp);
    for (int i = 0; i < nv; i++) {
        char in[40]; sprintf(in, "in.%s", vary[i].name); wreplace(fb, vary[i].name, in);
    }
    for (int i = 0; i < nu; i++) {
        char un[40]; sprintf(un, "u.%s", unis[i].name); wreplace(fb, unis[i].name, un);
    }
    for (int i = 0; i < nfragout; i++) wreplace(fb, fragouts[i].name, "_frag");
    wreplace(fb, "gl_FragColor", "_frag");
    wreplace(fb, "gl_FragCoord", "_dd_FragCoord");
    relational_fixups(fb);
    local_decl_fixups(fb);
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "fragment float4 fmain(VOut in [[stage_in]]");
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "%s", uparam);
    o = emit_samp_params(out, o, fb, samps, nsamp);
    o = cat_msl(out, o, TRANSLATE_OUTCAP, ") {\n  float4 _frag = float4(0);\n");
    if (frag_uses_coord) {
        o = cat_msl(out, o, TRANSLATE_OUTCAP, "  float4 _dd_FragCoord = in.position;\n");
    }
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "%s\n  return _frag;\n}\n", fb);
    return out;
}

// MSL struct member layout (size + alignment, in bytes) for a GLSL uniform type, matching how Metal lays
// out a `constant Uniforms&` struct. Getting this wrong shifts EVERY uniform after the offending member, so
// transforms/gradients silently render with corrupt coordinates even though the shader compiles.
//
// Key facts (default, non-packed MSL):
//   * float/int/uint/bool = 4B/4B; vecN follows: vec2=8/8, vec3=16/16 (float3 pads to 16!), vec4=16/16.
//   * matCxR is C columns of vecR. The COLUMN type sets stride+align: R=2→float2(8/8), R=3→float3(16/16),
//     R=4→float4(16/16). Matrix size = C*colStride, align = colAlign. So float2x2=16/8, float3x3=48/16
//     (not 36!), float3x2=24/8, float2x3=32/16, float4x2=32/8, float4x3=64/16, float4x4=64/16.
// Returns 1 if recognized (sz/al set), 0 otherwise (caller falls back to scalar 4/4).
static int msl_type_layout(const char *t, int *sz, int *al) {
    struct { const char *g; int sz, al; } tbl[] = {
        {"float", 4, 4},  {"int", 4, 4},   {"uint", 4, 4},   {"bool", 4, 4},
        {"vec2", 8, 8},   {"vec3", 16, 16}, {"vec4", 16, 16},
        {"ivec2", 8, 8},  {"ivec3", 16, 16}, {"ivec4", 16, 16},
        {"uvec2", 8, 8},  {"uvec3", 16, 16}, {"uvec4", 16, 16},
        {"bvec2", 8, 8},  {"bvec3", 16, 16}, {"bvec4", 16, 16},
        // matrices: matCxR (C cols, R rows). col stride: R=2→8, R=3→16, R=4→16.
        {"mat2", 16, 8},   {"mat3", 48, 16}, {"mat4", 64, 16},
        {"mat2x2", 16, 8}, {"mat2x3", 32, 16}, {"mat2x4", 32, 16},
        {"mat3x2", 24, 8}, {"mat3x3", 48, 16}, {"mat3x4", 48, 16},
        {"mat4x2", 32, 8}, {"mat4x3", 64, 16}, {"mat4x4", 64, 16},
    };
    for (unsigned i = 0; i < sizeof(tbl) / sizeof(tbl[0]); i++) {
        if (!strcmp(t, tbl[i].g)) { *sz = tbl[i].sz; *al = tbl[i].al; return 1; }
    }
    return 0;
}
// Compute the uniform-buffer byte layout (name→offset/size) matching Metal's struct alignment, so the
// bytes the app writes via glUniform* land where the MSL Uniforms struct expects them.
static int uni_layout(const char *vs, const char *fs, struct uni *out, int max, int *total) {
    struct decl unis[16], samps[4];
    int nu, nsamp;
    // Strip comments FIRST — translate() emits the MSL Uniforms struct from comment-stripped source, so if
    // we collected uniforms from the raw text a stray "uniform"/"attribute"/"varying" word inside a comment
    // would invent a phantom member here and shift every real uniform's offset off the MSL struct's layout.
    char vsbuf[16384], fsbuf[16384];
    snprintf(vsbuf, sizeof vsbuf, "%s", vs);
    snprintf(fsbuf, sizeof fsbuf, "%s", fs);
    strip_comments(vsbuf);
    strip_comments(fsbuf);
    collect_uniforms(vsbuf, fsbuf, unis, &nu, samps, &nsamp); // DATA uniforms only (samplers excluded)
    int cur = 0, n = 0;
    for (int i = 0; i < nu && n < max; i++) {
        int sz = 4, al = 4;
        msl_type_layout(unis[i].type, &sz, &al); // unknown types default to scalar 4/4
        cur = (cur + al - 1) & ~(al - 1);
        strcpy(out[n].name, unis[i].name);
        out[n].off = cur;
        out[n].sz = sz;
        cur += sz;
        n++;
    }
    *total = (cur + 15) & ~15;
    return n;
}

// ======================= surface bring-up (IOSurface + wayland + executor) =======================
static uint8_t txb[8192];
static size_t txn;
static void wmsg(uint32_t obj, uint16_t op, const uint32_t *w, int nw) {
    uint32_t sz = 8 + nw * 4, hdr[2] = {obj, (sz << 16) | op};
    memcpy(txb + txn, hdr, 8);
    txn += 8;
    if (nw) { memcpy(txb + txn, w, nw * 4); txn += nw * 4; }
}
static void wflush(void) { if (txn) { write(g_wl, txb, txn); txn = 0; } }
static void wflush_fd(int fd) {
    struct iovec io = {txb, txn};
    char cb[CMSG_SPACE(sizeof(int))];
    memset(cb, 0, sizeof cb);
    struct msghdr mh = {0};
    mh.msg_iov = &io; mh.msg_iovlen = 1; mh.msg_control = cb; mh.msg_controllen = sizeof cb;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof(int));
    sendmsg(g_wl, &mh, 0);
    txn = 0;
}
static int parse_size_pair(const char *s, int *w, int *h) {
    if (!s || !*s) return 0;
    char *e = 0;
    long ww = strtol(s, &e, 10);
    if (e == s || (*e != ',' && *e != 'x' && *e != 'X')) return 0;
    char *e2 = 0;
    long hh = strtol(e + 1, &e2, 10);
    if (e2 == e + 1 || ww <= 0 || hh <= 0 || ww > 8192 || hh > 8192) return 0;
    if (w) *w = (int)ww;
    if (h) *h = (int)hh;
    return 1;
}
static int env_logical_size(int *w, int *h) {
    const char *s = getenv("HL_SHIM_LOGICAL_SIZE");
    if (parse_size_pair(s, w, h)) return 1;
    s = getenv("HL_WINDOW_SIZE");
    return parse_size_pair(s, w, h);
}
static int cmdline_window_size(int *w, int *h) {
    int fd = open("/proc/self/cmdline", O_RDONLY);
    if (fd < 0) return 0;
    char buf[8192];
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0) return 0;
    buf[n] = 0;
    for (ssize_t i = 0; i < n;) {
        char *arg = buf + i;
        size_t l = strnlen(arg, (size_t)(n - i));
        if (!strncmp(arg, "--window-size=", 14) && parse_size_pair(arg + 14, w, h)) return 1;
        if (!strcmp(arg, "--window-size") && i + (ssize_t)l + 1 < n) {
            char *next = buf + i + l + 1;
            if (parse_size_pair(next, w, h)) return 1;
        }
        i += (ssize_t)l + 1;
    }
    return 0;
}
static const char *geom_source_name(int source) {
    switch (source) {
        case 1: return "wl_egl_window";
        case 2: return "env";
        case 3: return "cmdline";
        default: return "backing";
    }
}
static void resolve_surface_geometry(uint32_t bw, uint32_t bh) {
    int lw = g_pending_logical_w, lh = g_pending_logical_h, source = 1;
    if (lw <= 0 || lh <= 0 || lw > (int)bw || lh > (int)bh) {
        lw = (int)bw;
        lh = (int)bh;
        source = 0;
    }

    int fw = 0, fh = 0;
    if (lw == (int)bw && lh == (int)bh && env_logical_size(&fw, &fh) &&
        fw > 0 && fh > 0 && fw <= (int)bw && fh <= (int)bh) {
        lw = fw;
        lh = fh;
        source = 2;
    }
    if (lw == (int)bw && lh == (int)bh && cmdline_window_size(&fw, &fh) &&
        fw > 0 && fh > 0 && fw <= (int)bw && fh <= (int)bh) {
        lw = fw;
        lh = fh;
        source = 3;
    }

    g_surface_logical_w = lw;
    g_surface_logical_h = lh;
    g_surface_geom_x = 0;
    g_surface_geom_y = 0;
    if ((source == 2 || source == 3) && lw < (int)bw) g_surface_geom_x = ((int)bw - lw) / 2;
    if ((source == 2 || source == 3) && lh < (int)bh) g_surface_geom_y = ((int)bh - lh) / 2;
    g_surface_geom_source = source;
    g_surface_geom_sent = 0;
}
static int wl_send_window_geometry(void) {
    if (g_surface_logical_w <= 0 || g_surface_logical_h <= 0) return 0;
    if (g_surface_logical_w == (int)g_surf.width && g_surface_logical_h == (int)g_surf.height &&
        g_surface_geom_x == 0 && g_surface_geom_y == 0) {
        return 0;
    }
    uint32_t geom[4] = {
        (uint32_t)g_surface_geom_x,
        (uint32_t)g_surface_geom_y,
        (uint32_t)g_surface_logical_w,
        (uint32_t)g_surface_logical_h,
    };
    wmsg(g_xdg_surface, 3, geom, 4); // xdg_surface.set_window_geometry
    g_surface_geom_sent = 1;
    return 1;
}
static void surface_up(uint32_t w, uint32_t h) {
    fprintf(stderr, "gl_shim: surface_up %ux%u\n", w, h);
    g_default_surface_valid = 0;
    g_default_full_clear_since_swap = 0;
    resolve_surface_geometry(w, h);
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] surface_up backing=%ux%u logical_geom=%d,%d %dx%d source=%s attach=%d,%d\n",
                w, h, g_surface_geom_x, g_surface_geom_y, g_surface_logical_w, g_surface_logical_h,
                geom_source_name(g_surface_geom_source), g_pending_attach_x, g_pending_attach_y);
        fflush(stderr);
    }
    if (g_viewport[2] <= 0 || g_viewport[3] <= 0) {
        g_viewport[0] = 0; g_viewport[1] = 0; g_viewport[2] = (int)w; g_viewport[3] = (int)h;
    }
    if (g_scissor[2] <= 0 || g_scissor[3] <= 0) {
        g_scissor[0] = 0; g_scissor[1] = 0; g_scissor[2] = (int)w; g_scissor[3] = (int)h;
    }
    // HL_IR_DUMP: host-tool mode — no renderD128/wayland; just record the surface so eglSwapBuffers builds
    // the IR and exec_stream writes it to the dump file. Proves the shim's IR byte-stream on any host.
    if (getenv("HL_IR_DUMP")) {
        g_surf.width = w; g_surf.height = h; g_surf.id = 1; g_have_surf = 1;
        return;
    }
    int rnode = open("/dev/dri/renderD128", O_RDWR);
    if (rnode < 0) { fprintf(stderr, "gl_shim: no renderD128 (errno=%d %s)\n", errno, strerror(errno)); return; }
    fprintf(stderr, "gl_shim: renderD128 fd=%d\n", rnode);
    g_surf.width = w; g_surf.height = h; g_surf.format = 0;
    if (ioctl(rnode, HL_IOCTL_GPU_ALLOC, &g_surf) != 0) { fprintf(stderr, "gl_shim: alloc failed\n"); return; }
    g_have_surf = 1;
    // wayland handshake to hl-display
    const char *disp = getenv("WAYLAND_DISPLAY"), *rd = getenv("XDG_RUNTIME_DIR");
    if (!disp) disp = "wayland-0";
    if (!rd) rd = "/run/user/0";
    char path[256];
    if (disp[0] == '/') snprintf(path, sizeof path, "%s", disp);
    else snprintf(path, sizeof path, "%s/%s", rd, disp);
    g_wl = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un un = {0};
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s", path);
    if (connect(g_wl, (struct sockaddr *)&un, sizeof un) != 0) { fprintf(stderr, "gl_shim: wl connect fail\n"); g_wl = -1; return; }
    uint32_t reg = 2, comp = 3, dmabuf = 4, wm = 5, toplevel = 8;
    uint32_t a1[1] = {reg};
    wmsg(1, 1, a1, 1); // get_registry
    uint8_t bb[64];
    int n;
#define BIND(nm, ifc, ver, nid)                                              \
    do {                                                                     \
        const char *s = ifc; uint32_t sl = strlen(s) + 1, pd = (sl + 3) & ~3u; n = 0; \
        memcpy(bb + n, &(uint32_t){nm}, 4); n += 4; memcpy(bb + n, &sl, 4); n += 4;    \
        memcpy(bb + n, s, strlen(s) + 1); memset(bb + n + strlen(s) + 1, 0, pd - sl); n += pd; \
        memcpy(bb + n, &(uint32_t){ver}, 4); n += 4; memcpy(bb + n, &(uint32_t){nid}, 4); n += 4; \
        wmsg(reg, 0, (uint32_t *)bb, n / 4);                                 \
    } while (0)
    BIND(1, "wl_compositor", 4, comp);
    BIND(6, "zwp_linux_dmabuf_v1", 3, dmabuf);
    BIND(3, "xdg_wm_base", 1, wm);
    wmsg(comp, 0, &g_wl_surface, 1);       // create_surface
    uint32_t xw[2] = {g_xdg_surface, g_wl_surface};
    wmsg(wm, 2, xw, 2);                     // get_xdg_surface
    wmsg(g_xdg_surface, 1, &toplevel, 1);   // get_toplevel
    int geom_sent = wl_send_window_geometry();
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] initial_commit backing=%ux%u logical_geom=%d,%d %dx%d attach=%d,%d geometry_sent=%d\n",
                g_surf.width, g_surf.height, g_surface_geom_x, g_surface_geom_y,
                g_surface_logical_w, g_surface_logical_h, g_pending_attach_x, g_pending_attach_y, geom_sent);
        fflush(stderr);
    }
    wmsg(g_wl_surface, 6, 0, 0);          // commit (initial)
    wflush();
    usleep(50000);
    uint32_t ack[1] = {1};
    wmsg(g_xdg_surface, 4, ack, 1); // ack_configure
    wflush();
    g_wl_ready = 1;
    (void)dmabuf;
}
// Stream the current IR to the executor and wait for the render ack.
static void exec_stream(void) {
    const char *dump = getenv("HL_IR_DUMP");
    if (dump) { // host-tool mode: write the raw IR byte-stream to the dump file (proof harness)
        int fd = open(dump, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd >= 0) { if (write(fd, ir, irn) < 0) perror("ir dump"); close(fd); }
        fprintf(stderr, "gl_shim: dumped %zu IR bytes to %s\n", irn, dump);
        return;
    }
    const char *tee = getenv("HL_IR_TEE_DUMP");
    if (tee && *tee) {
        static unsigned tee_seq;
        char path[512];
        snprintf(path, sizeof path, "%s-%03u.ir", tee, tee_seq++);
        int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd >= 0) {
            if (write(fd, ir, irn) < 0) perror("ir tee");
            close(fd);
        }
    }
    const char *ep = getenv("HL_GPU_EXEC");
    if (!ep) ep = "/run/user/0/hl-gpu-0";
    // L2/L7.1: keep ONE executor connection open for the surface's lifetime — a frame is just
    // [hdr][ir]+ack on the same fd (no socket()/connect()/close() per frame). The executor holds a
    // persistent MetalBackend per connection, so its shader/PSO/resource caches survive across frames.
    // Reconnect lazily if the fd is closed or an I/O error tears it down.
    for (int attempt = 0; attempt < 2; attempt++) {
        if (g_exec_fd < 0) {
            int fd = socket(AF_UNIX, SOCK_STREAM, 0);
            struct sockaddr_un un = {0};
            un.sun_family = AF_UNIX;
            snprintf(un.sun_path, sizeof un.sun_path, "%s", ep);
            if (connect(fd, (struct sockaddr *)&un, sizeof un) != 0) {
                fprintf(stderr, "gl_shim: exec connect fail path=%s errno=%d %s\n", ep, errno, strerror(errno));
                close(fd);
                return;
            }
            // L5: a RE-connect (not the first) means a fresh host MetalBackend with an EMPTY resource
            // cache — every buffer/texture we thought was resident is gone. Force the next frame to
            // re-emit everything. (The first connect leaves residency untouched: frame 1 already uploads
            // all, populating that same new backend.)
            if (g_exec_connects >= 1) g_res_reset = 1;
            g_exec_fd = fd;
            g_exec_connects++;
        }
        uint32_t hdr[4] = {g_surf.id, g_surf.width, g_surf.height, (uint32_t)irn};
        if (write_full(g_exec_fd, hdr, sizeof hdr) != 0 ||
            write_full(g_exec_fd, ir, irn) != 0) {
            close(g_exec_fd); g_exec_fd = -1; continue; // executor gone → reconnect once
        }
        uint8_t ack = 0;
        if (read(g_exec_fd, &ack, 1) != 1) { close(g_exec_fd); g_exec_fd = -1; continue; }
        return; // frame acked
    }
}
// Drain wayland events (bounded), returning when wl_callback.done for `cb` is seen, or on timeout.
// This is the L1 replacement for the fixed 20 ms usleep: the compositor already fires wl_callback.done
// on present (server.rs), so waiting on it paces the guest to the real present rate instead of a fixed
// ~50 FPS cap. Bounded by a 100 ms deadline so a busy/stalled compositor can NEVER hang the guest. It
// also drains wl_buffer.release / delete_id (the old code never read g_wl, so those events piled up in
// the shim's recv buffer).
static uint8_t wl_rx[8192];
static int wl_rxn;
static void wl_drain_until_frame(uint32_t cb) {
    uint64_t deadline = now_us() + 100000; // 100 ms bound
    for (;;) {
        int off = 0, hit = 0;
        while (wl_rxn - off >= 8) {
            uint32_t obj, so;
            memcpy(&obj, wl_rx + off, 4);
            memcpy(&so, wl_rx + off + 4, 4);
            uint32_t size = so >> 16, op = so & 0xffff;
            if (size < 8) { off = wl_rxn; break; }     // corrupt framing → drop buffer
            if ((uint32_t)(wl_rxn - off) < size) break; // partial message; read more
            if (obj == cb && op == 0) hit = 1;          // wl_callback.done
            off += size;
        }
        if (off > 0) { memmove(wl_rx, wl_rx + off, wl_rxn - off); wl_rxn -= off; }
        if (hit) return;
        int64_t rem = (int64_t)deadline - (int64_t)now_us();
        if (rem <= 0) return;
        struct pollfd pfd = { g_wl, POLLIN, 0 };
        int pr = poll(&pfd, 1, (int)(rem / 1000) + 1);
        if (pr <= 0) return;
        if (pfd.revents & POLLIN) {
            if (wl_rxn >= (int)sizeof wl_rx) wl_rxn = 0; // overflow guard
            ssize_t n = read(g_wl, wl_rx + wl_rxn, sizeof wl_rx - wl_rxn);
            if (n <= 0) return;
            wl_rxn += (int)n;
        }
    }
}
// Commit the (executor-rendered) IOSurface to hl-display via linux-dmabuf.
static void wl_commit(void) {
    if (getenv("HL_IR_DUMP")) return; // host-tool mode: no wayland commit
    if (g_wl < 0 || !g_wl_ready) return;
    uint32_t dmabuf = 4, params = 9;
    wmsg(dmabuf, 1, &params, 1); // create_params
    wflush();
    uint32_t addw[5] = {0, 0, g_surf.stride, HL_DMABUF_MOD_MAGIC, g_surf.id};
    wmsg(params, 1, addw, 5); // add(fd via SCM_RIGHTS)
    wflush_fd(g_surf.fd);
    uint32_t ci[5] = {g_wl_buffer, g_surf.width, g_surf.height, DRM_FMT_XRGB8888, 0};
    wmsg(params, 3, ci, 5); // create_immed
    uint32_t at[3] = {g_wl_buffer, (uint32_t)g_pending_attach_x, (uint32_t)g_pending_attach_y};
    wmsg(g_wl_surface, 1, at, 3); // attach
    uint32_t dm[4] = {0, 0, g_surf.width, g_surf.height};
    wmsg(g_wl_surface, 2, dm, 4); // damage
    int geom_sent = wl_send_window_geometry();
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] wl_commit backing=%ux%u logical_geom=%d,%d %dx%d attach=%d,%d geometry_sent=%d source=%s\n",
                g_surf.width, g_surf.height, g_surface_geom_x, g_surface_geom_y,
                g_surface_logical_w, g_surface_logical_h, g_pending_attach_x, g_pending_attach_y,
                geom_sent, geom_source_name(g_surface_geom_source));
        fflush(stderr);
    }
    wmsg(g_wl_surface, 3, &g_wl_frame_cb, 1); // frame(callback) — L1 pacing (was: fixed 20ms usleep)
    wmsg(g_wl_surface, 6, 0, 0);  // commit
    wflush();
    // destroy the params + buffer objects so ids are reusable next frame
    wmsg(params, 0, 0, 0);
    wmsg(g_wl_buffer, 0, 0, 0);
    wflush();
    // L1: pace on the compositor's frame callback (bounded) instead of a blind 20ms sleep.
    wl_drain_until_frame(g_wl_frame_cb);
}

// ======================= wl_egl_window (our libwayland-egl.so.1) =======================
// glmark2 (and Chrome's ozone-wayland) link libwayland-egl and call wl_egl_window_create() to wrap a
// wl_surface + size, then pass the pointer to eglCreateWindowSurface(). We provide our OWN libwayland-egl
// (this same .so, staged over Mesa's) so the struct layout is fully under our control and self-consistent
// with the reader below — no dependence on Mesa's exact field order. The first field mirrors Mesa's ABI
// (`intptr_t version`) so a stray Mesa struct is still parseable via the offset fallback in
// eglCreateWindowSurface.
#define HL_WL_EGL_MAGIC ((intptr_t)0x6464776C65676CLL) // "hlwlegl" magic
struct hl_wl_egl_window {
    intptr_t version;   // = HL_WL_EGL_MAGIC (Mesa stores WL_EGL_WINDOW_VERSION here)
    int width, height;  // offsets 8/12 — same as Mesa's struct
    int dx, dy;
    int attached_width, attached_height;
    void *driver_private;
    void (*resize_cb)(struct hl_wl_egl_window *, void *);
    void (*destroy_cb)(struct hl_wl_egl_window *);
    void *surface;      // the wl_surface
};
struct hl_wl_egl_window *wl_egl_window_create(void *surface, int width, int height) {
    if (width <= 0 || height <= 0) return 0;
    struct hl_wl_egl_window *w = calloc(1, sizeof *w);
    if (!w) return 0;
    w->version = HL_WL_EGL_MAGIC;
    w->width = width;
    w->height = height;
    w->surface = surface;
    g_pending_logical_w = width;
    g_pending_logical_h = height;
    g_pending_attach_x = 0;
    g_pending_attach_y = 0;
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] wl_egl_window_create surface=%p size=%dx%d\n", surface, width, height);
        fflush(stderr);
    }
    return w;
}
void wl_egl_window_resize(struct hl_wl_egl_window *w, int width, int height, int dx, int dy) {
    if (!w) return;
    w->width = width;
    w->height = height;
    w->dx = dx;
    w->dy = dy;
    g_pending_logical_w = width;
    g_pending_logical_h = height;
    g_pending_attach_x = dx;
    g_pending_attach_y = dy;
    if (g_have_surf && width > 0 && height > 0 && width <= (int)g_surf.width && height <= (int)g_surf.height) {
        g_surface_logical_w = width;
        g_surface_logical_h = height;
        g_surface_geom_x = 0;
        g_surface_geom_y = 0;
        g_surface_geom_source = 1;
        g_surface_geom_sent = 0;
    }
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] wl_egl_window_resize window=%p size=%dx%d attach=%d,%d\n", (void *)w, width, height, dx, dy);
        fflush(stderr);
    }
}
void wl_egl_window_get_attached_size(struct hl_wl_egl_window *w, int *width, int *height) {
    if (!w) return;
    if (width) *width = w->attached_width ? w->attached_width : w->width;
    if (height) *height = w->attached_height ? w->attached_height : w->height;
}
void wl_egl_window_destroy(struct hl_wl_egl_window *w) { free(w); }

// ======================= EGL entry points =======================
#define EGLDBG(...) do { if (getenv("HL_SHIM_DEBUG")) { fprintf(stderr, "[shim] " __VA_ARGS__); fflush(stderr); } } while (0)
static int shim_es3(void) {
    const char *e = getenv("HL_SHIM_ES3");
    return e && e[0] && e[0] != '0';
}
static EGLint g_egl_error = EGL_SUCCESS;
static int g_ctx_major = 2;
static int g_ctx_minor = 0;
EGLDisplay eglGetDisplay(EGLNativeDisplayType d) { (void)d; EGLDBG("eglGetDisplay -> 1\n"); return (EGLDisplay)1; }
// EGL 1.5 / EXT platform-display entry: glmark2 prefers eglGetPlatformDisplay*() when the client
// extension string advertises it; we return "" from eglQueryString so it falls back to eglGetDisplay,
// but provide these too for callers (e.g. ANGLE/Chrome) that always use the platform path.
EGLDisplay eglGetPlatformDisplay(EGLenum plat, void *native, const void *attr) { (void)plat; (void)native; (void)attr; EGLDBG("eglGetPlatformDisplay plat=0x%x -> 1\n", plat); return (EGLDisplay)1; }
EGLDisplay eglGetPlatformDisplayEXT(EGLenum plat, void *native, const EGLint *attr) { (void)plat; (void)native; (void)attr; EGLDBG("eglGetPlatformDisplayEXT plat=0x%x -> 1\n", plat); return (EGLDisplay)1; }
EGLBoolean eglInitialize(EGLDisplay dpy, EGLint *maj, EGLint *min) { (void)dpy; if (maj) *maj = 1; if (min) *min = 4; EGLDBG("eglInitialize -> 1.4\n"); return EGL_TRUE; }
const char *eglQueryString(EGLDisplay dpy, EGLint name) {
    const char *r;
    switch (name) {
        case EGL_VENDOR: r = "dd"; break;
        case EGL_VERSION: r = "1.4 hl-shim"; break;
        case EGL_CLIENT_APIS: r = "OpenGL_ES"; break;
        // Advertise ONLY the client extensions ANGLE's gl-egl backend actually needs and that we back:
        // EGL_KHR_create_context (context client version / ES3 profile bits ANGLE passes to eglCreateContext)
        // and the surfaceless-context path (no window surface needed to make the bootstrap context current).
        // When dpy == EGL_NO_DISPLAY (client-extension query) return the platform-client set; otherwise the
        // display-extension set. Advertising nothing else avoids ANGLE resolving+calling an ext entry we
        // return NULL for.
        case EGL_EXTENSIONS:
            r = (dpy == EGL_NO_DISPLAY)
                    ? "EGL_EXT_client_extensions EGL_KHR_platform_gbm EGL_KHR_platform_wayland EGL_EXT_platform_base"
                    : "EGL_KHR_create_context EGL_KHR_surfaceless_context EGL_KHR_no_config_context";
            break;
        default: r = ""; break;
    }
    EGLDBG("eglQueryString dpy=%p name=0x%x -> \"%s\"\n", dpy, name, r);
    return r;
}
// One RGBA8888 + depth24 + stencil8 window config. We expose exactly one config (id 1); eglChooseConfig
// and eglGetConfigs both return it, so glmark2's "best config" selection always lands on it.
EGLBoolean eglChooseConfig(EGLDisplay dpy, const EGLint *a, EGLConfig *c, EGLint n, EGLint *num) {
    (void)dpy;
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] eglChooseConfig n=%d attribs:", n);
        if (a) for (const EGLint *p = a; *p != EGL_NONE; p += 2) fprintf(stderr, " 0x%x=%d", p[0], p[1]);
        fprintf(stderr, "\n");
    }
    if (c && n >= 1) c[0] = (EGLConfig)1;
    if (num) *num = 1;
    return EGL_TRUE;
}
EGLBoolean eglGetConfigs(EGLDisplay dpy, EGLConfig *c, EGLint n, EGLint *num) { (void)dpy; if (c && n >= 1) c[0] = (EGLConfig)1; if (num) *num = 1; return EGL_TRUE; }
// Return real, self-consistent attributes so glmark2 can build a GLVisualConfig and pick this config.
EGLBoolean eglGetConfigAttrib(EGLDisplay dpy, EGLConfig c, EGLint a, EGLint *v) {
    (void)dpy; (void)c;
    if (!v) return EGL_FALSE;
    EGLint r;
    switch (a) {
        case EGL_CONFIG_ID: r = 1; break;
        case EGL_RED_SIZE: r = 8; break;
        case EGL_GREEN_SIZE: r = 8; break;
        case EGL_BLUE_SIZE: r = 8; break;
        case EGL_ALPHA_SIZE: r = 8; break;
        case EGL_BUFFER_SIZE: r = 32; break;
        case EGL_DEPTH_SIZE: r = 24; break;
        // glmark2's config scorer (gl-visual-config.cpp score_component) returns UNACCEPTABLE (-1000)
        // for any component present (>0) that the app did NOT request (target==0). Its default request
        // is depth=1 (want depth), stencil=0/-1 (do NOT want stencil), samples=0. Advertising stencil=8
        // therefore poisoned the ONLY config → "Failed to find suitable EGL config". Report stencil=0 so
        // the config scores positive (real drivers pass because they also expose a 0-stencil config).
        // The Metal executor still allocates depth/stencil as needed; this only affects EGL matching.
        case EGL_STENCIL_SIZE: r = 0; break;
        case EGL_LUMINANCE_SIZE: r = 0; break;
        case EGL_ALPHA_MASK_SIZE: r = 0; break;
        case EGL_SURFACE_TYPE: r = EGL_WINDOW_BIT | EGL_PBUFFER_BIT; break;
        case EGL_RENDERABLE_TYPE:
            r = EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES_BIT | (shim_es3() ? EGL_OPENGL_ES3_BIT_KHR : 0);
            break;
        case EGL_CONFORMANT:
            r = EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES_BIT | (shim_es3() ? EGL_OPENGL_ES3_BIT_KHR : 0);
            break;
        case EGL_COLOR_BUFFER_TYPE: r = EGL_RGB_BUFFER; break;
        case EGL_CONFIG_CAVEAT: r = EGL_NONE; break;
        case EGL_NATIVE_RENDERABLE: r = EGL_TRUE; break;
        case EGL_NATIVE_VISUAL_ID: r = (EGLint)DRM_FMT_XRGB8888; break;
        case EGL_NATIVE_VISUAL_TYPE: r = 0; break;
        case EGL_MAX_PBUFFER_WIDTH: r = 4096; break;
        case EGL_MAX_PBUFFER_HEIGHT: r = 4096; break;
        case EGL_MAX_PBUFFER_PIXELS: r = 4096 * 4096; break;
        case EGL_MIN_SWAP_INTERVAL: r = 0; break;
        case EGL_MAX_SWAP_INTERVAL: r = 1; break;
        case EGL_SAMPLES: r = 0; break;
        case EGL_SAMPLE_BUFFERS: r = 0; break;
        case EGL_LEVEL: r = 0; break;
        case EGL_TRANSPARENT_TYPE: r = EGL_NONE; break;
        case EGL_BIND_TO_TEXTURE_RGB: r = EGL_FALSE; break;
        case EGL_BIND_TO_TEXTURE_RGBA: r = EGL_FALSE; break;
        default: r = 0; break;
    }
    *v = r;
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] eglGetConfigAttrib cfg=%p attr=0x%x -> %d\n", c, a, r);
    return EGL_TRUE;
}
EGLContext eglCreateContext(EGLDisplay dpy, EGLConfig c, EGLContext s, const EGLint *a) {
    (void)dpy; (void)s;
    int req_major = 1, req_minor = 0;
    if (a) {
        for (const EGLint *p = a; *p != EGL_NONE; p += 2) {
            if (p[0] == 0x3098) req_major = p[1];      // EGL_CONTEXT_CLIENT_VERSION / MAJOR_VERSION_KHR
            else if (p[0] == 0x30FB) req_minor = p[1]; // EGL_CONTEXT_MINOR_VERSION_KHR
        }
    }
    int max_major = shim_es3() ? 3 : 2;
    int max_minor = 0;
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] eglCreateContext cfg=%p share=%p attribs:", c, s);
        if (a) for (const EGLint *p = a; *p != EGL_NONE; p += 2) fprintf(stderr, " 0x%x=%d", p[0], p[1]);
        fprintf(stderr, " requested=%d.%d max=%d.%d", req_major, req_minor, max_major, max_minor);
    }
    if (req_major > max_major || (req_major == max_major && req_minor > max_minor)) {
        g_egl_error = EGL_BAD_MATCH;
        if (getenv("HL_SHIM_DEBUG")) { fprintf(stderr, " -> EGL_NO_CONTEXT (BAD_MATCH)\n"); fflush(stderr); }
        return EGL_NO_CONTEXT;
    }
    g_ctx_major = req_major;
    g_ctx_minor = req_minor;
    if (getenv("HL_SHIM_DEBUG")) { fprintf(stderr, " -> 1\n"); fflush(stderr); }
    return (EGLContext)1;
}
EGLSurface eglCreateWindowSurface(EGLDisplay dpy, EGLConfig c, EGLNativeWindowType w, const EGLint *a) {
    (void)dpy; (void)c; (void)a;
    uint32_t W = 256, H = 256;
    int logical_w = 0, logical_h = 0, attach_x = 0, attach_y = 0;
    if (w) {
        struct hl_wl_egl_window *win = (struct hl_wl_egl_window *)w;
        int ww, hh;
        if (win->version == HL_WL_EGL_MAGIC) {
            // Our libwayland-egl.so.1 struct (glmark2, Chrome/ozone): width/height at offsets 8/12.
            ww = win->width;
            hh = win->height;
            logical_w = win->width;
            logical_h = win->height;
            attach_x = win->dx;
            attach_y = win->dy;
            if (win->attached_width > 0 && win->attached_height > 0 &&
                win->attached_width <= 8192 && win->attached_height <= 8192 &&
                (win->attached_width != win->width || win->attached_height != win->height)) {
                if (win->attached_width > ww) ww = win->attached_width;
                if (win->attached_height > hh) hh = win->attached_height;
            }
            if (getenv("HL_SHIM_DEBUG"))
                fprintf(stderr, "[shim] native_window=%p hlwlegl width=%d height=%d attached=%d,%d attach=%d,%d\n",
                        w, win->width, win->height, win->attached_width, win->attached_height, attach_x, attach_y);
        } else {
            // Stock-app convention (es2tri/es2tex): two ints {width, height} at offset 0. Chrome/ANGLE
            // may instead hand us Mesa's wl_egl_window ({version,width,height,...}); detect that narrow
            // version-looking first word before accepting the two-int shape. Only that case reads p[2].
            int *p = (int *)w;
            if (getenv("HL_SHIM_DEBUG"))
                fprintf(stderr, "[shim] native_window=%p words=%d,%d,%d,%d\n", w, p[0], p[1], p[2], p[3]);
            if (p[0] > 0 && p[0] <= 16 && p[2] > 16 && p[2] <= 8192 && p[3] > 16 && p[3] <= 8192) {
                if (getenv("HL_SHIM_DEBUG"))
                    fprintf(stderr, "[shim] native_window=%p mesa_words=%d,%d,%d,%d,%d,%d,%d,%d\n",
                            w, p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]);
                ww = p[2];
                hh = p[3];
                logical_w = ww;
                logical_h = hh;
                attach_x = p[4];
                attach_y = p[5];
                if (p[6] > 16 && p[6] <= 8192 && p[7] > 16 && p[7] <= 8192 &&
                    (p[6] != ww || p[7] != hh)) {
                    if (p[6] > ww) ww = p[6];
                    else if (p[6] < ww && p[7] == hh) logical_w = p[6];
                    if (p[7] > hh) hh = p[7];
                    else if (p[7] < hh && p[6] == ww) logical_h = p[7];
                }
            } else if (p[0] > 0 && p[0] <= 16 && p[1] > 16 && p[1] <= 8192) {
                ww = p[1];
                hh = p[2];
                logical_w = ww;
                logical_h = hh;
            } else {
                ww = p[0];
                hh = p[1];
                logical_w = ww;
                logical_h = hh;
            }
        }
        if (ww > 0 && ww <= 8192) W = (uint32_t)ww;
        if (hh > 0 && hh <= 8192) H = (uint32_t)hh;
    }
    if (logical_w <= 0 || logical_w > (int)W) logical_w = (int)W;
    if (logical_h <= 0 || logical_h > (int)H) logical_h = (int)H;
    g_pending_logical_w = logical_w;
    g_pending_logical_h = logical_h;
    g_pending_attach_x = attach_x;
    g_pending_attach_y = attach_y;
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] eglCreateWindowSurface backing=%ux%u pending_logical=%dx%d attach=%d,%d\n",
                W, H, g_pending_logical_w, g_pending_logical_h, g_pending_attach_x, g_pending_attach_y);
        fflush(stderr);
    }
    surface_up(W, H);
    return (EGLSurface)1;
}
EGLBoolean eglMakeCurrent(EGLDisplay dpy, EGLSurface d, EGLSurface r, EGLContext c) { (void)dpy; EGLDBG("eglMakeCurrent draw=%p read=%p ctx=%p\n", d, r, c); return EGL_TRUE; }
EGLBoolean eglSwapInterval(EGLDisplay dpy, EGLint i) { (void)dpy; (void)i; return EGL_TRUE; }
EGLBoolean eglBindAPI(EGLenum a) { EGLDBG("eglBindAPI 0x%x\n", a); return EGL_TRUE; }
EGLint eglGetError(void) {
    EGLint e = g_egl_error;
    g_egl_error = EGL_SUCCESS;
    EGLDBG("eglGetError -> 0x%x\n", e);
    return e;
}
EGLBoolean eglQuerySurface(EGLDisplay dpy, EGLSurface s, EGLint a, EGLint *v) {
    (void)dpy; (void)s;
    if (v) {
        if (a == EGL_WIDTH) *v = g_surface_logical_w > 0 ? g_surface_logical_w : (int)g_surf.width;
        else if (a == EGL_HEIGHT) *v = g_surface_logical_h > 0 ? g_surface_logical_h : (int)g_surf.height;
        else *v = 0;
    }
    return EGL_TRUE;
}
EGLBoolean eglTerminate(EGLDisplay dpy) { (void)dpy; return EGL_TRUE; }
EGLBoolean eglReleaseThread(void) { return EGL_TRUE; }
EGLBoolean eglDestroySurface(EGLDisplay dpy, EGLSurface s) { (void)dpy; (void)s; return EGL_TRUE; }
EGLBoolean eglDestroyContext(EGLDisplay dpy, EGLContext c) { (void)dpy; (void)c; return EGL_TRUE; }
EGLBoolean eglWaitClient(void) { return EGL_TRUE; }
EGLBoolean eglWaitGL(void) { return EGL_TRUE; }
EGLBoolean eglWaitNative(EGLint e) { (void)e; return EGL_TRUE; }
EGLSurface eglGetCurrentSurface(EGLint r) { (void)r; return (EGLSurface)1; }
EGLDisplay eglGetCurrentDisplay(void) { return (EGLDisplay)1; }
EGLContext eglGetCurrentContext(void) { return (EGLContext)1; }
EGLenum eglQueryAPI(void) { return 0x30A0; } // EGL_OPENGL_ES_API
// ---- ANGLE gl-egl entry-point breadth (Wall 5) --------------------------------------------------------
// chromium bundles ANGLE; its gl-egl backend (FunctionsEGL::initialize) resolves a FIXED set of EGL 1.0-1.2
// CORE entry points via eglGetProcAddress and ABORTS Display::initialize if ANY is absent ("Could not load
// EGL entry point eglCreatePbufferSurface" → "eglInitialize OpenGLEGL failed EGL_NOT_INITIALIZED"). We must
// export the whole set even where the op is a no-op for our offscreen/IOSurface model. All are resolvable
// via eglGetProcAddress (dlsym RTLD_DEFAULT over these exported globals).
EGLBoolean eglQueryContext(EGLDisplay dpy, EGLContext c, EGLint a, EGLint *v) {
    (void)dpy;
    (void)c;
    if (!v) return EGL_FALSE;
    switch (a) {
        case 0x3097: *v = 0x30A0; break;             // EGL_CONTEXT_CLIENT_TYPE = EGL_OPENGL_ES_API
        case 0x3098: *v = g_ctx_major; break; // EGL_CONTEXT_CLIENT_VERSION / MAJOR_VERSION_KHR
        case 0x30FB: *v = g_ctx_minor; break; // EGL_CONTEXT_MINOR_VERSION_KHR
        default: *v = 0; break;
    }
    EGLDBG("eglQueryContext ctx=%p attr=0x%x -> %d\n", c, a, *v);
    return EGL_TRUE;
}
EGLBoolean eglCopyBuffers(EGLDisplay dpy, EGLSurface s, void *tgt) { (void)dpy; (void)s; (void)tgt; return EGL_TRUE; }
// Pbuffer surface: ANGLE creates a tiny (typically 1x1) offscreen surface to make its BOOTSTRAP GL context
// current during Display::initialize (GL capability probing), BEFORE the real window surface exists. Return
// a DISTINCT non-null handle and do NOT run the IOSurface/Wayland bring-up — that belongs to the WINDOW
// surface, whose pixels reach hl-display; clobbering the single global g_surf here would redirect the
// browser's frames to a 1x1 offscreen buffer. eglSwapBuffers is a no-op on a pbuffer in real EGL, and our
// eglSwapBuffers only acts when g_have_surf (set by the window path), so this handle stays inert on swap.
EGLSurface eglCreatePbufferSurface(EGLDisplay dpy, EGLConfig c, const EGLint *a) { (void)dpy; (void)c; (void)a; EGLDBG("eglCreatePbufferSurface cfg=%p -> 2\n", c); return (EGLSurface)2; }
EGLSurface eglCreatePixmapSurface(EGLDisplay dpy, EGLConfig c, void *pix, const EGLint *a) { (void)dpy; (void)c; (void)pix; (void)a; return (EGLSurface)3; }
EGLSurface eglCreatePbufferFromClientBuffer(EGLDisplay dpy, EGLenum bt, void *buf, EGLConfig c, const EGLint *a) { (void)dpy; (void)bt; (void)buf; (void)c; (void)a; return (EGLSurface)2; }
EGLBoolean eglBindTexImage(EGLDisplay dpy, EGLSurface s, EGLint b) { (void)dpy; (void)s; (void)b; return EGL_TRUE; }
EGLBoolean eglReleaseTexImage(EGLDisplay dpy, EGLSurface s, EGLint b) { (void)dpy; (void)s; (void)b; return EGL_TRUE; }
EGLBoolean eglSurfaceAttrib(EGLDisplay dpy, EGLSurface s, EGLint a, EGLint v) { (void)dpy; (void)s; (void)a; (void)v; return EGL_TRUE; }
// Resolve any GL/EGL entry point by name from our own libs (glmark2 + ANGLE load core+ext this way).
// A name we don't export returns NULL, which callers treat as "extension unavailable"; the important
// thing is the CORE functions resolve here (previously this returned NULL for everything → a NULL call).
void (*eglGetProcAddress(const char *n))(void) {
    if (!n) return 0;
    // CRITICAL: this shim is deployed under THREE sonames (libEGL/libGLESv2/libwayland-egl), so three
    // independent copies with SEPARATE static state (g_cur_prog/g_attr/g_draw_mode/...) are loaded. A
    // native-GL app like glmark2 obtains its GL entry points here (via eglGetProcAddress) but calls
    // eglSwapBuffers/eglCreateWindowSurface directly — those live in THIS instance. If we resolved GL
    // names with dlsym(RTLD_DEFAULT), they'd bind to whichever copy sorts first in the global scope
    // (libwayland-egl), so glUseProgram/glDrawElements/glVertexAttribPointer would mutate a DIFFERENT
    // copy's state than the one eglSwapBuffers reads → every frame renders clear-only (black). Resolve
    // against OUR OWN object (the one containing this function) so GL state and the swap share globals.
    // libGLESv2.so.2 + libwayland-egl.so.1 are thin DT_NEEDED->libEGL.so.1 stubs (see build), so exactly
    // ONE copy of this code (and its static GL state) is loaded → RTLD_DEFAULT resolves GL names to it.
    // RTLD_DEFAULT only searches the GLOBAL symbol scope. glmark2 pulls us in via DT_NEEDED (executable
    // dependency → global), so RTLD_DEFAULT finds our GL entry points. But chromium's ANGLE gl-egl backend
    // dlopen()s libEGL.so.1 with RTLD_LOCAL — our symbols are then NOT global, so dlsym(RTLD_DEFAULT, "glX")
    // returned NULL for EVERY GLES function ANGLE resolved → ANGLE stored 130 null FunctionsGL pointers →
    // NULL-deref. Fix: also resolve against OUR OWN loaded object via a RTLD_NOLOAD handle to our soname
    // (dlsym on a handle finds a lib's symbols regardless of RTLD_LOCAL). This keeps ONE code+state copy
    // (libGLESv2/libwayland-egl are DT_NEEDED->libEGL stubs) so GL state and eglSwapBuffers share globals.
    static void *self = (void *)-1;
    if (self == (void *)-1) self = dlopen("libEGL.so.1", RTLD_NOLOAD | RTLD_NOW);
    void *p = self ? dlsym(self, n) : NULL;
    if (!p) p = dlsym(RTLD_DEFAULT, n);
    if (!p && getenv("HL_SHIM_DEBUG")) { fprintf(stderr, "[shim] eglGetProcAddress(\"%s\") -> NULL\n", n); fflush(stderr); }
    return (void (*)(void))p;
}

// Build + stream the IR for the current frame, then composite.
EGLBoolean eglSwapBuffers(EGLDisplay dpy, EGLSurface s) {
    (void)dpy; (void)s;
    if (!g_have_surf) return EGL_FALSE;
    uint64_t t_gl0 = prof_on() ? now_us() : 0;
    // L5: a prior host reconnect emptied the backend's caches → drop all residency so this frame re-uploads.
    if (g_res_reset) { l5_reset_residency(); g_res_reset = 0; }
    // Use the draw-time attribute snapshot (glmark2 disables its attribs before swapping — see g_attr_snap).
    if (g_have_draw_snap) memcpy(g_attr, g_attr_snap, sizeof g_attr);
    int replay_draws = g_ndraws > 1 || (g_ndraws == 1 && g_draws[0].is_clear);
    int frame_default_touched = 0;
    irn = 0;
    struct prog *pr = (g_cur_prog < MAXPROG) ? &g_prog[g_cur_prog] : NULL;
    // Which textures this frame binds: sampler i (declaration order) samples the GL texture unit selected by
    // glUniform1i for that sampler. ANGLE/Chrome does not have to map sampler N to texture unit N.
    int texlist[MAXDRAWS], ntex = 0;
    int tex_upload[MAXDRAWS];
    memset(tex_upload, 0, sizeof tex_upload); // L5: per-bound-texture, did we (re)upload pixels this frame? → copy op
    if (pr && replay_draws) {
        for (int d = 0; d < g_ndraws; d++) {
            if (g_draws[d].is_clear) continue;
            struct prog *dpr = (g_draws[d].prog < MAXPROG) ? &g_prog[g_draws[d].prog] : pr;
            for (int i = 0; i < dpr->nsamp && i < 4; i++) {
                int unit = (g_draws[d].samp_units[i] >= 0 && g_draws[d].samp_units[i] < 8) ? g_draws[d].samp_units[i] : i;
                GLuint tu = g_draws[d].tex_units[unit];
                if (tu >= MAXTEX || !g_tex[tu].used || !g_tex[tu].data) continue;
                int seen = 0;
                for (int k = 0; k < ntex; k++) if (texlist[k] == (int)tu) seen = 1;
                if (!seen && ntex < MAXDRAWS) texlist[ntex++] = (int)tu;
            }
        }
    } else if (pr)
        for (int i = 0; i < pr->nsamp && i < 4; i++) {
            int unit = (pr->samp_units[i] >= 0 && pr->samp_units[i] < 8) ? pr->samp_units[i] : i;
            GLuint tu = g_tex_unit[unit];
            if (tu < MAXTEX && g_tex[tu].used && g_tex[tu].data) texlist[ntex++] = (int)tu;
        }
    if (replay_draws) {
        for (int d = 0; d < g_ndraws; d++) {
            GLuint tu = g_draws[d].target_tex;
            if (tu == 0 || tu >= MAXTEX || !g_tex[tu].used || !g_tex[tu].data) continue;
            int seen = 0;
            for (int k = 0; k < ntex; k++) if (texlist[k] == (int)tu) seen = 1;
            if (!seen && ntex < MAXDRAWS) texlist[ntex++] = (int)tu;
        }
    }
    // ---- Multi-vertex-buffer analysis (RENDERING_PLAN M6) --------------------------------------------
    // glmark2/ANGLE bind a SEPARATE tightly-packed VBO per attribute (position in one buffer, normal in
    // another). Group the enabled attributes by their source GL buffer → one IR vertex-buffer (== one
    // Metal binding slot + MTLVertexBufferLayout) per distinct VBO, each attribute referencing its slot.
    // (Previously all attributes aliased slot 0 → secondary streams like normals were wrong → grayscale.)
    // Slot ids: layout index = slot (0,1,…); IR buffer id = 200 + slot (avoids the 10/11/12/50/60/70 ids).
    int slot_vbo[MAXATTR];   // slot → source GL array buffer id
    int nslot = 0;
    int attr_slot[MAXATTR];  // attribute location → slot (−1 if not enabled / no valid buffer)
    int frame_vbo[MAXDRAWS];
    int nframe_vbo = 0;
    int frame_ibo[MAXDRAWS];
    int nframe_ibo = 0;
    for (int i = 0; i < MAXATTR; i++) attr_slot[i] = -1;
    if (replay_draws) {
        nslot = 1; // Chrome compositor draws use one interleaved VBO; SetVertexBuffer swaps it per draw.
        for (int d = 0; d < g_ndraws; d++) {
            if (g_draws[d].is_clear) continue;
            for (int i = 0; i < MAXATTR; i++) {
                if (!g_draws[d].attrs[i].enabled) continue;
                int b = g_draws[d].attrs[i].buffer;
                if (b <= 0 || b >= MAXBUF || !g_buf[b].used || !g_buf[b].data) continue;
                int seen = 0;
                for (int k = 0; k < nframe_vbo; k++) if (frame_vbo[k] == b) seen = 1;
                if (!seen && nframe_vbo < MAXDRAWS) frame_vbo[nframe_vbo++] = b;
            }
            if (g_draws[d].indexed) {
                int b = (int)g_draws[d].elem_buf;
                if (b > 0 && b < MAXBUF && g_buf[b].used && g_buf[b].data) {
                    int seen = 0;
                    for (int k = 0; k < nframe_ibo; k++) if (frame_ibo[k] == b) seen = 1;
                    if (!seen && nframe_ibo < MAXDRAWS) frame_ibo[nframe_ibo++] = b;
                }
            }
        }
    } else {
        for (int i = 0; i < MAXATTR; i++) {
            if (!g_attr[i].enabled) continue;
            int b = g_attr[i].buffer;
            if (b < 0 || b >= MAXBUF || !g_buf[b].used || !g_buf[b].data) continue;
            int sl = -1;
            for (int k = 0; k < nslot; k++) if (slot_vbo[k] == b) { sl = k; break; }
            if (sl < 0) { sl = nslot; slot_vbo[nslot++] = b; }
            attr_slot[i] = sl;
        }
    }
    // Per-slot vertex stride: an interleaved VBO reports the same stride on all its attrs; a tightly-packed
    // per-attribute VBO reports stride 0 → its own component size (size*4 bytes).
    uint32_t slot_stride[MAXATTR];
    for (int sl = 0; sl < nslot; sl++) slot_stride[sl] = 0;
    for (int i = 0; i < MAXATTR; i++) {
        int sl = attr_slot[i];
        if (sl < 0) continue;
        uint32_t st = (uint32_t)g_attr[i].stride;
        if (st == 0) st = (uint32_t)g_attr[i].size * 4u;
        if (st > slot_stride[sl]) slot_stride[sl] = st;
    }
    for (int sl = 0; sl < nslot; sl++) if (slot_stride[sl] == 0) slot_stride[sl] = 24;

    // 1. one CreateBuffer(VERTEX) + WriteBuffer per distinct source VBO — but ONLY when its content changed
    //    or it isn't yet resident on the host (L5 delta upload). Static VBOs (glmark2's horse) upload once.
    if (replay_draws) {
        for (int d = 0; d < g_ndraws; d++) {
            if (g_draws[d].is_clear) continue;
            int dslot_vbo[MAXATTR], dattr_slot[MAXATTR];
            uint32_t dslot_stride[MAXATTR];
            int dnslot = draw_vbo_slots(&g_draws[d], dslot_vbo, dattr_slot, dslot_stride);
            (void)dattr_slot;
            (void)dslot_stride;
            for (int sl = 0; sl < dnslot; sl++) {
                int si = draw_vbo_snapshot_index(&g_draws[d], (GLuint)dslot_vbo[sl]);
                if (si < 0 || !g_draws[d].snap_vbo_data[si] || g_draws[d].snap_vbo_size[si] == 0) continue;
                uint32_t id = replay_vbo_ir_id(d, sl);
                struct residency *r = &g_res_replay_vbuf[d][sl];
                int resident = delta_on() && r->valid && r->src == dslot_vbo[sl] && r->gen == g_draws[d].snap_vbo_gen[si];
                if (!resident) {
                    iu8(1); iu32(id); iu64(g_draws[d].snap_vbo_size[si]); iu32(1); istr("");
                    iu8(3); iu32(id); iu64(0); ibytes(g_draws[d].snap_vbo_data[si], (uint32_t)g_draws[d].snap_vbo_size[si]);
                    r->valid = 1; r->src = dslot_vbo[sl]; r->gen = g_draws[d].snap_vbo_gen[si];
                }
            }
        }
        for (int k = 0; k < nframe_vbo; k++) {
            struct buf *b = &g_buf[frame_vbo[k]];
            uint32_t id = 200 + (uint32_t)k;
            struct residency *r = &g_res_frame_vbuf[k];
            int resident = delta_on() && r->valid && r->src == frame_vbo[k] && r->gen == b->gen;
            if (!resident) {
                iu8(1); iu32(id); iu64(b->size); iu32(1); istr("");
                iu8(3); iu32(id); iu64(0); ibytes(b->data, (uint32_t)b->size);
                r->valid = 1; r->src = frame_vbo[k]; r->gen = b->gen;
            }
        }
    } else {
        for (int sl = 0; sl < nslot; sl++) {
            struct buf *b = &g_buf[slot_vbo[sl]];
            uint32_t id = 200 + (uint32_t)sl;
            struct residency *r = &g_res_vbuf[sl];
            int resident = delta_on() && r->valid && r->src == slot_vbo[sl] && r->gen == b->gen;
            if (!resident) {
                iu8(1); iu32(id); iu64(b->size); iu32(1); istr("");            // CreateBuffer(id,{size,VERTEX,""})
                iu8(3); iu32(id); iu64(0); ibytes(b->data, (uint32_t)b->size); // WriteBuffer(id)
                r->valid = 1; r->src = slot_vbo[sl]; r->gen = b->gen;
            }
        }
    }
    // 1b. index buffer for glDrawElements → CreateBuffer(12, INDEX) + WriteBuffer (whole element buffer),
    //     gated the same way — the horse's 21.5k-index buffer is static, so it too uploads exactly once.
    if (replay_draws) {
        for (int d = 0; d < g_ndraws; d++) {
            if (g_draws[d].is_clear || !g_draws[d].indexed ||
                !g_draws[d].snap_ibo_data || g_draws[d].snap_ibo_size == 0) continue;
            uint32_t id = replay_ibo_ir_id(d);
            struct residency *r = &g_res_replay_ibo[d];
            int resident = delta_on() && r->valid && r->src == (int)g_draws[d].snap_ibo_src && r->gen == g_draws[d].snap_ibo_gen;
            if (!resident) {
                iu8(1); iu32(id); iu64(g_draws[d].snap_ibo_size); iu32(2 /*INDEX*/); istr("");
                iu8(3); iu32(id); iu64(0); ibytes(g_draws[d].snap_ibo_data, (uint32_t)g_draws[d].snap_ibo_size);
                r->valid = 1; r->src = (int)g_draws[d].snap_ibo_src; r->gen = g_draws[d].snap_ibo_gen;
            }
        }
        for (int k = 0; k < nframe_ibo; k++) {
            struct buf *eb = &g_buf[frame_ibo[k]];
            uint32_t id = 300 + (uint32_t)k;
            struct residency *r = &g_res_frame_ibo[k];
            int resident = delta_on() && r->valid && r->src == frame_ibo[k] && r->gen == eb->gen;
            if (!resident) {
                iu8(1); iu32(id); iu64(eb->size); iu32(2 /*INDEX*/); istr("");
                iu8(3); iu32(id); iu64(0); ibytes(eb->data, (uint32_t)eb->size);
                r->valid = 1; r->src = frame_ibo[k]; r->gen = eb->gen;
            }
        }
    } else if (g_draw_indexed && g_elem_buf < MAXBUF && g_buf[g_elem_buf].used && g_buf[g_elem_buf].data) {
        struct buf *eb = &g_buf[g_elem_buf];
        int resident = delta_on() && g_res_index.valid && g_res_index.src == (int)g_elem_buf && g_res_index.gen == eb->gen;
        if (!resident) {
            iu8(1); iu32(12); iu64(eb->size); iu32(2 /*INDEX*/); istr("");
            iu8(3); iu32(12); iu64(0); ibytes(eb->data, (uint32_t)eb->size);
            g_res_index.valid = 1; g_res_index.src = (int)g_elem_buf; g_res_index.gen = eb->gen;
        }
    }
    // 1c. textures → CreateTexture + CreateSampler + staging CreateBuffer + WriteBuffer (uploaded in-pass)
    for (int k = 0; k < ntex; k++) {
        struct tex *t = &g_tex[texlist[k]];
        uint32_t tid = tex_ir_id((GLuint)texlist[k]), sid = sampler_ir_id((GLuint)texlist[k]), stg = stage_ir_id((GLuint)texlist[k]);
        // CreateTexture(tid): {w,h,depth1,mips1,samples1,dim=D2,fmt=Rgba8Unorm,usage=SAMPLED|RENDER_TARGET|COPY_DST}
        iu8(4); iu32(tid); iu32(t->w); iu32(t->h); iu32(1); iu32(1); iu32(1); iu32(2); iu32(1); iu32(1 | 4 | 16); istr("");
        // CreateSampler(sid): min,mag,mip,addrU,addrV,addrW
        uint32_t minf = (t->minf == GL_LINEAR || t->minf == GL_LINEAR_MIPMAP_NEAREST || t->minf == GL_LINEAR_MIPMAP_LINEAR) ? 1 : 0;
        uint32_t magf = (t->magf == GL_LINEAR) ? 1 : 0;
        uint32_t au = (t->ws == GL_CLAMP_TO_EDGE) ? 0 : (t->ws == GL_MIRRORED_REPEAT ? 2 : 1);
        uint32_t av = (t->wt == GL_CLAMP_TO_EDGE) ? 0 : (t->wt == GL_MIRRORED_REPEAT ? 2 : 1);
        iu8(6); iu32(sid); iu32(minf); iu32(magf); iu32(0); iu32(au); iu32(av); iu32(0);
        // L5: the texture pixels live on the host once uploaded (persistent backend). Re-stage + re-copy
        // ONLY when the texture changed or isn't resident — a static texture (glmark2 texture cube) uploads
        // once. CreateTexture/CreateSampler stay per-frame (cheap; the host CreateTexture is a no-op when the
        // id already exists, and the sampler carries this frame's filter/wrap).
        struct residency *r = replay_draws ? &g_res_tex_replay[texlist[k]] : &g_res_tex[k];
        int resident = delta_on() && r->valid && r->src == texlist[k] && r->gen == t->gen;
        if (!resident) {
            // staging buffer with the RGBA8 pixels (COPY_SRC=1<<4)
            iu8(1); iu32(stg); iu64(t->size); iu32(16); istr("");
            iu8(3); iu32(stg); iu64(0); ibytes(t->data, (uint32_t)t->size);
            tex_upload[k] = 1;
            r->valid = 1; r->src = texlist[k]; r->gen = t->gen;
        }
    }
    // Declared vertex attributes (declaration order == [[attribute(L)]] == glGetAttribLocation). The Metal
    // pipeline requires EVERY declared attribute to appear in the vertex descriptor; glmark2's build shader
    // declares position+normal+texcoord but a scene may enable only position+normal → an unbound declared
    // attribute becomes a placeholder in slot 0 (offset 0, its GLSL component count).
    struct decl vdecl[16];
    int ndecl = (pr && pr->vs && g_sh[pr->vs].src) ? collect_vertex_attrs(g_sh[pr->vs].src, vdecl, 16) : 0;
    int nvd = ndecl;
    if (replay_draws) {
        for (int d = 0; d < g_ndraws; d++)
            if (!g_draws[d].is_clear)
            for (int i = 0; i < MAXATTR; i++)
                if (g_draws[d].attrs[i].enabled && i + 1 > nvd) nvd = i + 1;
    } else {
        for (int i = 0; i < MAXATTR; i++) if (g_attr[i].enabled && i + 1 > nvd) nvd = i + 1;
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] eglSwapBuffers draw_mode=%d ndraws=%d replay=%d prog=%d msl=%s nslot=%d nvd=%d ntex=%d vbos=%d ibos=%d\n",
        g_draw_mode, g_ndraws, replay_draws, g_cur_prog, (pr&&pr->msl)?"OK":"none", nslot, nvd, ntex, nframe_vbo, nframe_ibo);

    // 2. program → shader (combined MSL) + pipeline
    int has_u = pr && pr->nuni > 0;
    if (replay_draws) {
        has_u = 0;
        for (int d = 0; d < g_ndraws; d++) {
            if (g_draws[d].is_clear) continue;
            struct prog *dpr = (g_draws[d].prog < MAXPROG && g_prog[g_draws[d].prog].used) ? &g_prog[g_draws[d].prog] : pr;
            if (dpr && dpr->nuni > 0) has_u = 1;
        }
    }
    if (pr && pr->msl) {
        if (replay_draws) {
            for (int d = 0; d < g_ndraws; d++) {
                if (g_draws[d].is_clear) continue;
                struct prog *dpr = (g_draws[d].prog < MAXPROG && g_prog[g_draws[d].prog].used) ? &g_prog[g_draws[d].prog] : pr;
                if (!dpr || !dpr->msl) continue;
                ir_shader(20 + (uint32_t)d, dpr->msl);
                iu8(10); iu32(30 + (uint32_t)d);
                iu32(20 + (uint32_t)d); istr("vmain");
                iu8(1); iu32(20 + (uint32_t)d); istr("fmain");
                struct decl hlecl[16];
                int dnd = (dpr->vs && g_sh[dpr->vs].src) ? collect_vertex_attrs(g_sh[dpr->vs].src, hlecl, 16) : 0;
                int dvcount = dnd;
                for (int i = 0; i < MAXATTR; i++) {
                    if (g_draws[d].attrs[i].enabled && i + 1 > dvcount) dvcount = i + 1;
                }
                if (dvcount <= 0) dvcount = nvd;
                int dslot_vbo[MAXATTR], dattr_slot[MAXATTR];
                uint32_t dslot_stride[MAXATTR];
                int dnslot = draw_vbo_slots(&g_draws[d], dslot_vbo, dattr_slot, dslot_stride);
                uint32_t nvb = dnslot > 0 ? (uint32_t)dnslot : 1u;
                iu32(nvb);
                for (uint32_t sl = 0; sl < nvb; sl++) {
                    int locs[MAXATTR], nl = 0;
                    for (int L = 0; L < dvcount && L < MAXATTR; L++) {
                        int ls = (dattr_slot[L] >= 0) ? dattr_slot[L] : 0;
                        if (ls == (int)sl) locs[nl++] = L;
                    }
                    uint32_t stride = (sl < (uint32_t)dnslot) ? dslot_stride[sl] : 16u;
                    iu32(stride); iu32(0); iu32((uint32_t)nl);
                    for (int j = 0; j < nl; j++) {
                        int L = locs[j];
                        uint32_t fmt, off;
                        if (L < MAXATTR && g_draws[d].attrs[L].enabled && dattr_slot[L] >= 0) {
                            fmt = vertex_format_wire(g_draws[d].attrs[L].type, g_draws[d].attrs[L].size,
                                                     g_draws[d].attrs[L].normalized, g_draws[d].attrs[L].integer);
                            off = (uint32_t)g_draws[d].attrs[L].offset;
                        } else {
                            const char *t = (L < dnd) ? hlecl[L].type : "vec4";
                            fmt = decl_format_wire(t);
                            off = 0;
                        }
                        iu32((uint32_t)L); iu32(fmt); iu32(off);
                    }
                }
                iu32(1);
                emit_color_target_fmt(color_target_format(g_draws[d].target_tex), g_draws[d].blend,
                                      g_draws[d].blend_src_rgb, g_draws[d].blend_dst_rgb, g_draws[d].blend_eq_rgb,
                                      g_draws[d].blend_src_alpha, g_draws[d].blend_dst_alpha, g_draws[d].blend_eq_alpha, 0xf);
                if (g_depth) { iu8(1); iu32(10); iu8(1); iu32(0); }
                else iu8(0);
                uint32_t dtopo = (g_draws[d].mode == GL_TRIANGLE_STRIP) ? 4 : 3;
                iu32(dtopo); iu32(0); iu32(0);
                istr("");
            }
        } else {
        ir_shader(20, pr->msl); // CreateShader(20, MSL)
        // CreateRenderPipeline(30): vertex module 20 entry vmain, fragment module 20 entry fmain
        iu8(10); iu32(30);
        iu32(20); istr("vmain");         // vertex ShaderRef
        iu8(1); iu32(20); istr("fmain"); // fragment Some
        // One VertexLayout per slot (== distinct source VBO); each carries the attributes bound to that
        // slot, so the Metal descriptor gives every buffer its own MTLVertexBufferLayout + bufferIndex.
        if (replay_draws) {
            uint32_t stride = 16u;
            int ref = g_ndraws > 0 ? 0 : -1;
            if (ref >= 0) {
                for (int L = 0; L < nvd && L < MAXATTR; L++) {
                    if (g_draws[ref].attrs[L].enabled && g_draws[ref].attrs[L].stride > 0) {
                        stride = (uint32_t)g_draws[ref].attrs[L].stride;
                        break;
                    }
                }
            }
            iu32(1);                        // vertex_buffers len
            iu32(stride); iu32(0); iu32((uint32_t)nvd); // stride, step_mode, attr count
            for (int L = 0; L < nvd; L++) {
                uint32_t fmt, off;
                if (ref >= 0 && L < MAXATTR && g_draws[ref].attrs[L].enabled) {
                    fmt = vertex_format_wire(g_draws[ref].attrs[L].type, g_draws[ref].attrs[L].size,
                                             g_draws[ref].attrs[L].normalized, g_draws[ref].attrs[L].integer);
                    off = (uint32_t)g_draws[ref].attrs[L].offset;
                } else {
                    const char *t = (L < ndecl) ? vdecl[L].type : "vec4";
                    fmt = decl_format_wire(t);
                    off = 0;
                }
                iu32((uint32_t)L); iu32(fmt); iu32(off);
            }
        } else {
            uint32_t nvb = nslot > 0 ? (uint32_t)nslot : 1u;
            iu32(nvb);                        // vertex_buffers len
            for (uint32_t sl = 0; sl < nvb; sl++) {
                int locs[MAXATTR], nl = 0;
                for (int L = 0; L < nvd; L++) {
                    int ls = (L < MAXATTR && attr_slot[L] >= 0) ? attr_slot[L] : 0; // unbound → placeholder in slot 0
                    if (ls == (int)sl) locs[nl++] = L;
                }
                uint32_t stride = (sl < (uint32_t)nslot) ? slot_stride[sl] : 24u;
                iu32(stride); iu32(0); iu32((uint32_t)nl); // stride, step_mode, attr count
                for (int j = 0; j < nl; j++) {
                    int L = locs[j];
                    uint32_t fmt, off;
                    if (L < MAXATTR && g_attr[L].enabled && attr_slot[L] >= 0) {
                        fmt = vertex_format_wire(g_attr[L].type, g_attr[L].size, g_attr[L].normalized, g_attr[L].integer);
                        off = (uint32_t)g_attr[L].offset;
                    }
                    else {
                        const char *t = (L < ndecl) ? vdecl[L].type : "vec4";
                        fmt = decl_format_wire(t);
                        off = 0;
                    }
                    iu32((uint32_t)L); iu32(fmt); iu32(off); // location, packed format, offset
                }
            }
        }
        iu32(1);                          // color_targets len
        emit_color_target(g_blend, g_blend_src_rgb, g_blend_dst_rgb, g_blend_eq_rgb,
                          g_blend_src_alpha, g_blend_dst_alpha, g_blend_eq_alpha, 0xf);
        if (g_depth) { iu8(1); iu32(10); iu8(1); iu32(0); } // depth Some{Depth32Float, write, compare}
        else iu8(0);                      // depth None
        uint32_t topo = (g_draw_mode == GL_TRIANGLE_STRIP) ? 4 : 3;
        iu32(topo); iu32(0); iu32(0);     // topology, cull, front_face
        istr("");                         // label
        }
    }
    // 2b. uniforms + textures + samplers → uniform buffer + a combined bind group (40).
    if (has_u && replay_draws) {
        for (int d = 0; d < g_ndraws; d++) {
            if (g_draws[d].is_clear) continue;
            struct prog *dpr = (g_draws[d].prog < MAXPROG && g_prog[g_draws[d].prog].used) ? &g_prog[g_draws[d].prog] : pr;
            if (!dpr || dpr->nuni <= 0) continue;
            iu8(1); iu32(1000 + (uint32_t)d); iu64(dpr->ubuf_size); iu32(4 /*UNIFORM*/); istr("");
            iu8(3); iu32(1000 + (uint32_t)d); iu64(0); ibytes(g_draws[d].ubuf, (uint32_t)dpr->ubuf_size);
        }
    } else if (has_u) {
        iu8(1); iu32(11); iu64(pr->ubuf_size); iu32(4 /*UNIFORM*/); istr(""); // CreateBuffer(11)
        iu8(3); iu32(11); iu64(0); ibytes(pr->ubuf, (uint32_t)pr->ubuf_size); // WriteBuffer(11)
    }
    int has_bg = has_u || ntex > 0;
    if (has_bg) {
        if (replay_draws) {
            for (int d = 0; d < g_ndraws; d++) {
                if (g_draws[d].is_clear) continue;
                int dtex[4], ndt = 0;
                struct prog *dpr = (g_draws[d].prog < MAXPROG && g_prog[g_draws[d].prog].used) ? &g_prog[g_draws[d].prog] : pr;
                if (dpr) {
                    for (int i = 0; i < dpr->nsamp && i < 4; i++) {
                        int unit = (g_draws[d].samp_units[i] >= 0 && g_draws[d].samp_units[i] < 8) ? g_draws[d].samp_units[i] : i;
                        GLuint bound = g_draws[d].tex_units[unit];
                        int ti = -1;
                        for (int k = 0; k < ntex; k++) if (texlist[k] == (int)bound) ti = k;
                        if (ti >= 0) dtex[ndt++] = ti;
                    }
                }
                int draw_has_u = dpr && dpr->nuni > 0;
                uint32_t nent = (draw_has_u ? 1u : 0u) + (uint32_t)ndt * 2u;
                iu8(13); iu32(40 + (uint32_t)d); iu32(0); iu32(nent);
                if (draw_has_u) { iu32(1); iu8(0); iu32(1000 + (uint32_t)d); iu64(0); iu64(dpr->ubuf_size); }
                for (int k = 0; k < ndt; k++) {
                    GLuint gltex = (GLuint)texlist[dtex[k]];
                    iu32((uint32_t)k); iu8(1); iu32(tex_ir_id(gltex));
                    iu32((uint32_t)k); iu8(2); iu32(sampler_ir_id(gltex));
                }
            }
        } else {
            uint32_t nent = (has_u ? 1u : 0u) + (uint32_t)ntex * 2u;
            // CreateBindGroup(40, {set:0, entries:[...]})
            iu8(13); iu32(40); iu32(0); iu32(nent);
            if (has_u) { iu32(1); iu8(0); iu32(11); iu64(0); iu64(pr->ubuf_size); }  // binding1 = Uniforms buffer
            for (int k = 0; k < ntex; k++) {
                GLuint gltex = (GLuint)texlist[k];
                iu32((uint32_t)k); iu8(1); iu32(tex_ir_id(gltex));     // binding k = Texture(gl texture)
                iu32((uint32_t)k); iu8(2); iu32(sampler_ir_id(gltex)); // binding k = Sampler(gl texture)
            }
        }
    }
    // 3. Submit: [CopyBufferToTexture]* + BeginRenderPass + [SetPipeline,(SetBindGroup),SetVertexBuffer,
    //    (SetIndexBuffer,DrawIndexed | Draw)] + EndRenderPass.
    int ncopy = 0; // L5: only textures re-uploaded this frame need a CopyBufferToTexture op
    for (int k = 0; k < ntex; k++) ncopy += tex_upload[k];
    int nops = ncopy;
    if (replay_draws) {
        int d = 0;
        while (d < g_ndraws) {
            if (g_draws[d].is_clear) {
                nops += 1;
                d++;
                continue;
            }
            GLuint target = g_draws[d].target_tex;
            if (target >= MAXTEX || !g_tex[target].used) target = 0;
            int seg_clear = g_draws[d].clear_serial;
            nops += 2; // Begin + End for this target segment
            while (d < g_ndraws) {
                if (g_draws[d].is_clear) break;
                GLuint t2 = g_draws[d].target_tex;
                if (t2 >= MAXTEX || !g_tex[t2].used) t2 = 0;
                if (t2 != target) break;
                if (g_draws[d].clear_serial != seg_clear) break;
                int dslot_vbo[MAXATTR], dattr_slot[MAXATTR];
                uint32_t dslot_stride[MAXATTR];
                int dnslot = draw_vbo_slots(&g_draws[d], dslot_vbo, dattr_slot, dslot_stride);
                nops += 3 + (has_bg ? 1 : 0) + dnslot + (g_draws[d].indexed ? 2 : 1); // SetPipeline + Viewport + Scissor + Bind + SetVBs + optional SetIB + Draw
                d++;
            }
        }
    } else if (g_draw_mode >= 0) {
        nops += 2 + 3 + nslot; // Begin + End + SetPipeline + Viewport + Scissor + one SetVertexBuffer per slot
        if (has_bg) nops += 1;
        nops += g_draw_indexed ? 2 : 1;
    }
    iu8(19);
    iu32(nops);
    // texture uploads BEFORE the render pass (standalone blit) — only for textures (re)staged this frame
    for (int k = 0; k < ntex; k++) {
        if (!tex_upload[k]) continue; // L5: resident texture already on the host → no re-copy
        struct tex *t = &g_tex[texlist[k]];
        // CopyBufferToTexture{src=stg, src_offset0, bytes_per_row=w*4, dst=tid, mip0, w, h}
        iu8(14); iu32(stage_ir_id((GLuint)texlist[k])); iu64(0); iu32((uint32_t)t->w * 4); iu32(tex_ir_id((GLuint)texlist[k])); iu32(0); iu32(t->w); iu32(t->h);
    }
    if (replay_draws) {
        int seen_default = 0;
        int seen_tex[MAXTEX];
        int clear_default = -1;
        int clear_tex[MAXTEX];
        memset(seen_tex, 0, sizeof seen_tex);
        for (int i = 0; i < MAXTEX; i++) clear_tex[i] = -1;
        int d = 0;
        while (d < g_ndraws) {
            if (g_draws[d].is_clear) {
                GLuint clear_target = g_draws[d].target_tex;
                if (clear_target >= MAXTEX || !g_tex[clear_target].used) clear_target = 0;
                if (!clear_target) frame_default_touched = 1;
                emit_clear_rect(&g_draws[d]);
                d++;
                continue;
            }
            GLuint target = g_draws[d].target_tex;
            if (target >= MAXTEX || !g_tex[target].used) target = 0;
            int seg_clear = g_draws[d].clear_serial;
            int seen = target ? seen_tex[target] : seen_default;
            int last_clear = target ? clear_tex[target] : clear_default;
            int load = (seen && seg_clear == last_clear);
            if (!target && !seen && g_default_surface_valid && !g_default_full_clear_since_swap)
                load = 1;
            if (!target) frame_default_touched = 1;
            if (getenv("HL_SHIM_DEBUG"))
                fprintf(stderr, "[shim] replay_pass target=%u load=%d seen=%d seg_clear=%d last_clear=%d default_valid=%d default_full_clear=%d sc=%d %d,%d %dx%d\n",
                        target, load, seen, seg_clear, last_clear, g_default_surface_valid,
                        g_default_full_clear_since_swap, g_draws[d].scissor_enabled,
                        g_draws[d].scissor[0], g_draws[d].scissor[1], g_draws[d].scissor[2], g_draws[d].scissor[3]);
            if (target) seen_tex[target] = 1; else seen_default = 1;
            if (target) clear_tex[target] = seg_clear; else clear_default = seg_clear;
            iu8(1); iu32(1); // BeginRenderPass, 1 color
            iu32(target ? tex_ir_id(target) : 1); iu32(load ? 0 : 1);
            ifl(g_draws[d].clear[0]); ifl(g_draws[d].clear[1]); ifl(g_draws[d].clear[2]); ifl(g_draws[d].clear[3]); iu8(1);
            if (g_depth) { iu8(1); iu32(2); iu32(1); ifl(1.0f); }
            else iu8(0);
            int tw = draw_target_w(target), th = draw_target_h(target);
            while (d < g_ndraws) {
                if (g_draws[d].is_clear) break;
                GLuint t2 = g_draws[d].target_tex;
                if (t2 >= MAXTEX || !g_tex[t2].used) t2 = 0;
                if (t2 != target) break;
                if (g_draws[d].clear_serial != seg_clear) break;
                iu8(3); iu32(30 + (uint32_t)d);
                emit_viewport_h(g_draws[d].viewport, th);
                emit_scissor_h(g_draws[d].scissor_enabled, g_draws[d].scissor, tw, th);
                if (has_bg) { iu8(4); iu32(0); iu32(40 + (uint32_t)d); }
                int dslot_vbo[MAXATTR], dattr_slot[MAXATTR];
                uint32_t dslot_stride[MAXATTR];
                int dnslot = draw_vbo_slots(&g_draws[d], dslot_vbo, dattr_slot, dslot_stride);
                for (int sl = 0; sl < dnslot; sl++) {
                    int si = draw_vbo_snapshot_index(&g_draws[d], (GLuint)dslot_vbo[sl]);
                    if (si >= 0) {
                        iu8(5); iu32((uint32_t)sl); iu32(replay_vbo_ir_id(d, sl)); iu64(0);
                    } else {
                        int vk = 0;
                        for (int k = 0; k < nframe_vbo; k++) if (frame_vbo[k] == dslot_vbo[sl]) vk = k;
                        iu8(5); iu32((uint32_t)sl); iu32(200 + (uint32_t)vk); iu64(0);
                    }
                }
                if (g_draws[d].indexed) {
                    int ib = (int)g_draws[d].elem_buf;
                    uint32_t ifmt = (g_draws[d].index_type == GL_UNSIGNED_INT) ? 2 : 1;
                    if (g_draws[d].snap_ibo_data) {
                        iu8(6); iu32(replay_ibo_ir_id(d)); iu64(g_draws[d].index_offset); iu32(ifmt);
                    } else {
                        int ik = 0;
                        for (int k = 0; k < nframe_ibo; k++) if (frame_ibo[k] == ib) ik = k;
                        iu8(6); iu32(300 + (uint32_t)ik); iu64(g_draws[d].index_offset); iu32(ifmt);
                    }
                    iu8(9); iu32(g_draws[d].count); iu32(1); iu32(0); iu32(0); iu32(0);
                } else {
                    iu8(8); iu32(g_draws[d].count); iu32(1); iu32(g_draws[d].first); iu32(0);
                }
                d++;
            }
            iu8(2); // EndRenderPass
        }
    } else if (g_draw_mode >= 0) {
        GLuint target = (g_ndraws == 1) ? g_draws[0].target_tex : 0;
        if (target >= MAXTEX || !g_tex[target].used) target = 0;
        int load = (!target && g_default_surface_valid && !g_default_full_clear_since_swap);
        if (!target) frame_default_touched = 1;
        if (getenv("HL_SHIM_DEBUG"))
            fprintf(stderr, "[shim] single_pass target=%u load=%d default_valid=%d default_full_clear=%d sc=%d %d,%d %dx%d\n",
                    target, load, g_default_surface_valid, g_default_full_clear_since_swap,
                    g_scissor_enabled, g_scissor[0], g_scissor[1], g_scissor[2], g_scissor[3]);
        iu8(1); iu32(1);                 // BeginRenderPass, 1 color
        iu32(target ? tex_ir_id(target) : 1); iu32(load ? 0 : 1); ifl(g_clear[0]); ifl(g_clear[1]); ifl(g_clear[2]); ifl(g_clear[3]); iu8(1);
        if (g_depth) { iu8(1); iu32(2); iu32(1); ifl(1.0f); }
        else iu8(0);
        iu8(3); iu32(30);                                // SetPipeline
        emit_viewport_h(g_viewport, draw_target_h(target));
        emit_scissor_h(g_scissor_enabled, g_scissor, draw_target_w(target), draw_target_h(target));
        if (has_bg) { iu8(4); iu32(0); iu32(40); }       // SetBindGroup{index0, group40}
        for (int sl = 0; sl < nslot; sl++) {             // one SetVertexBuffer per distinct source VBO
            iu8(5); iu32((uint32_t)sl); iu32(200 + (uint32_t)sl); iu64(0); // slot sl → IR buffer 200+sl
        }
        if (g_draw_indexed) {
            uint32_t ifmt = (g_index_type == GL_UNSIGNED_INT) ? 2 : 1; // U32 : U16
            iu8(6); iu32(12); iu64(g_index_offset); iu32(ifmt);        // SetIndexBuffer
            iu8(9); iu32(g_draw_count); iu32(1); iu32(0); iu32(0); iu32(0); // DrawIndexed
        } else {
            iu8(8); iu32(g_draw_count); iu32(1); iu32(g_draw_first); iu32(0); // Draw
        }
        iu8(2);  // EndRenderPass
    }
    iu8(0);  // signal None
    uint64_t t_enc = g_prof ? now_us() : 0;
    exec_stream();
    uint64_t t_exec = g_prof ? now_us() : 0;
    wl_commit();
    if (frame_default_touched) g_default_surface_valid = 1;
    g_default_full_clear_since_swap = 0;
    if (g_prof && g_prof_f) {
        uint64_t t_commit = now_us();
        uint64_t frame_us = g_prof_last_gl0 ? (t_gl0 - g_prof_last_gl0) : 0;
        fprintf(g_prof_f, "%llu,%llu,%llu,%llu,%llu,%zu,%llu\n",
                (unsigned long long)g_prof_seq, (unsigned long long)(t_enc - t_gl0),
                (unsigned long long)(t_exec - t_enc), (unsigned long long)(t_commit - t_exec),
                (unsigned long long)frame_us, irn, (unsigned long long)g_exec_connects);
        fflush(g_prof_f);
        g_prof_seq++;
        g_prof_last_gl0 = t_gl0;
    }
    g_draw_mode = -1; // reset per-frame draw
    g_have_draw_snap = 0;
    g_draw_indexed = 0;
    free_draw_snapshots();
    g_ndraws = 0;
    return EGL_TRUE;
}

// ======================= GLES2 entry points =======================
void glClearColor(GLfloat r, GLfloat g, GLfloat b, GLfloat a) { g_clear[0] = r; g_clear[1] = g; g_clear[2] = b; g_clear[3] = a; }
static void record_clear_call(int x, int y, int w, int h);
static int clear_scissor_rect(int *rx, int *ry, int *rw, int *rh) {
    int target_w = draw_target_w((g_draw_fbo > 0 && g_draw_fbo < MAXFBO && g_fbo[g_draw_fbo].used) ? g_fbo[g_draw_fbo].color_tex : 0);
    int target_h = draw_target_h((g_draw_fbo > 0 && g_draw_fbo < MAXFBO && g_fbo[g_draw_fbo].used) ? g_fbo[g_draw_fbo].color_tex : 0);
    int x = 0, y = 0, w = target_w, h = target_h;
    if (g_scissor_enabled && g_scissor[2] > 0 && g_scissor[3] > 0) {
        x = g_scissor[0];
        y = g_scissor[1];
        w = g_scissor[2];
        h = g_scissor[3];
    }
    if (x < 0) { w += x; x = 0; }
    if (y < 0) { h += y; y = 0; }
    if (x > target_w) x = target_w;
    if (y > target_h) y = target_h;
    if (x + w > target_w) w = target_w - x;
    if (y + h > target_h) h = target_h - y;
    if (w < 0) w = 0;
    if (h < 0) h = 0;
    if (rx) *rx = x;
    if (ry) *ry = y;
    if (rw) *rw = w;
    if (rh) *rh = h;
    return g_scissor_enabled && (x != 0 || y != 0 || w != target_w || h != target_h);
}

static void clear_bound_color_texture(const float color[4]) {
    if (!(g_draw_fbo > 0 && g_draw_fbo < MAXFBO && g_fbo[g_draw_fbo].used)) return;
    GLuint tex = g_fbo[g_draw_fbo].color_tex;
    if (tex >= MAXTEX || !g_tex[tex].used || !g_tex[tex].data) return;
    uint8_t r = (uint8_t)((color[0] < 0.0f ? 0.0f : (color[0] > 1.0f ? 1.0f : color[0])) * 255.0f + 0.5f);
    uint8_t g = (uint8_t)((color[1] < 0.0f ? 0.0f : (color[1] > 1.0f ? 1.0f : color[1])) * 255.0f + 0.5f);
    uint8_t b = (uint8_t)((color[2] < 0.0f ? 0.0f : (color[2] > 1.0f ? 1.0f : color[2])) * 255.0f + 0.5f);
    uint8_t a = (uint8_t)((color[3] < 0.0f ? 0.0f : (color[3] > 1.0f ? 1.0f : color[3])) * 255.0f + 0.5f);
    int x, y, w, h;
    clear_scissor_rect(&x, &y, &w, &h);
    for (int yy = y; yy < y + h; yy++) {
        for (int xx = x; xx < x + w; xx++) {
            uint8_t *p = g_tex[tex].data + ((size_t)yy * (size_t)g_tex[tex].w + (size_t)xx) * 4u;
            p[0] = r; p[1] = g; p[2] = b; p[3] = a;
        }
    }
    g_tex[tex].gen++;
}
void glClear(GLbitfield m) {
    int sx, sy, sw, sh;
    int scissored_clear = clear_scissor_rect(&sx, &sy, &sw, &sh);
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glClear mask=0x%x fbo=%u color=%g,%g,%g,%g sc=%d %d,%d %dx%d\n",
        m, g_draw_fbo, g_clear[0], g_clear[1], g_clear[2], g_clear[3], g_scissor_enabled, sx, sy, sw, sh);
    if (m & GL_COLOR_BUFFER_BIT) {
        // Metal render-pass load clears are full-target, while GLES glClear obeys GL_SCISSOR_TEST.
        if (scissored_clear) {
            if (!(g_draw_fbo == 0 && getenv("HL_SKIP_DEFAULT_SCISSOR_CLEAR"))) {
                record_clear_call(sx, sy, sw, sh);
            }
        }
        else {
            g_clear_serial++;
            if (g_draw_fbo == 0) {
                g_default_full_clear_since_swap = 1;
                record_clear_call(sx, sy, sw, sh);
                if (getenv("HL_SHIM_DEBUG"))
                    fprintf(stderr, "[shim] default_full_clear_recorded serial=%d rect=%d,%d %dx%d\n",
                            g_clear_serial, sx, sy, sw, sh);
            }
        }
        clear_bound_color_texture(g_clear);
    }
}
void glViewport(GLint x, GLint y, GLsizei w, GLsizei h) {
    g_viewport[0] = x; g_viewport[1] = y; g_viewport[2] = w; g_viewport[3] = h;
}
void glEnable(GLenum c) {
    if (c == GL_DEPTH_TEST) g_depth = 1;
    else if (c == GL_BLEND) g_blend = 1;
    else if (c == GL_SCISSOR_TEST) g_scissor_enabled = 1;
}
void glDisable(GLenum c) {
    if (c == GL_DEPTH_TEST) g_depth = 0;
    else if (c == GL_BLEND) g_blend = 0;
    else if (c == GL_SCISSOR_TEST) g_scissor_enabled = 0;
}
GLenum glGetError(void) { return GL_NO_ERROR; }
void glFinish(void) {}
void glFlush(void) {}
const unsigned char *glGetString(GLenum n) {
    const unsigned char *r;
    switch (n) {
        // ES2 by default so glmark2 (GLSL ES 1.00 shaders) stays on the known-good path. Chromium's ANGLE
        // asks for an ES3 context; HL_SHIM_ES3 opts that run into the ES3 caps and exported stubs below.
        case GL_VERSION:
            r = (const unsigned char *)(g_ctx_major >= 3 ? "OpenGL ES 3.0 hl-shim" : "OpenGL ES 2.0 hl-shim");
            break;
        case GL_VENDOR: r = (const unsigned char *)"dd"; break;
        case GL_RENDERER: r = (const unsigned char *)"hl-metal"; break;
        case GL_SHADING_LANGUAGE_VERSION:
            r = (const unsigned char *)(g_ctx_major >= 3 ? "OpenGL ES GLSL ES 3.00" : "OpenGL ES GLSL ES 1.00");
            break;
        case GL_EXTENSIONS:
            r = (const unsigned char *)gl_extensions_string();
            break;
        default: r = (const unsigned char *)""; break;
    }
    EGLDBG("glGetString(0x%x) -> \"%s\"\n", n, r);
    return r;
}
GLuint glCreateShader(GLenum type) {
    for (int i = 1; i < MAXSH; i++)
        if (!g_sh[i].used) { g_sh[i].used = 1; g_sh[i].type = type; g_sh[i].src = NULL;
            if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glCreateShader(0x%x) -> %d\n", type, i); return i; }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glCreateShader(0x%x) EXHAUSTED -> 0\n", type);
    return 0;
}
void glShaderSource(GLuint sh, GLsizei count, const GLchar *const *str, const GLint *len) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glShaderSource ENTRY sh=%u count=%d used=%d\n", sh, count, (sh<MAXSH)?g_sh[sh].used:-1);
    if (sh >= MAXSH || !g_sh[sh].used) return;
    size_t tot = 0;
    for (int i = 0; i < count; i++) tot += (len && len[i] >= 0) ? (size_t)len[i] : strlen(str[i]);
    char *s = malloc(tot + 1);
    if (!s) return;
    size_t o = 0;
    for (int i = 0; i < count; i++) {
        size_t l = (len && len[i] >= 0) ? (size_t)len[i] : strlen(str[i]);
        memcpy(s + o, str[i], l);
        o += l;
    }
    s[o] = 0;
    free(g_sh[sh].src);
    g_sh[sh].src = s;
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glShaderSource sh=%u count=%d stored len=%zu\n", sh, count, o);
}
void glCompileShader(GLuint sh) { (void)sh; }
void glGetShaderiv(GLuint sh, GLenum p, GLint *v) {
    if (!v) return;
    // GL_SHADER_SOURCE_LENGTH (0x8B88): glmark2 sets the source then verifies this round-trips to
    // strlen(source)+1 (incl. NUL) before it will compile — returning 0 aborted "Failed to add shader".
    if (p == 0x8B88) { *v = (sh < MAXSH && g_sh[sh].used && g_sh[sh].src) ? (GLint)(strlen(g_sh[sh].src) + 1) : 0;
        if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glGetShaderiv sh=%u SOURCE_LENGTH -> %d (src=%p)\n", sh, *v, (sh<MAXSH)?(void*)g_sh[sh].src:0); return; }
    if (p == GL_COMPILE_STATUS) { *v = GL_TRUE; return; }
    *v = 0;
}
void glGetShaderInfoLog(GLuint sh, GLsizei bufSize, GLsizei *length, GLchar *infoLog) { (void)sh; (void)bufSize; if (length) *length = 0; if (infoLog && bufSize) infoLog[0] = 0; }
GLuint glCreateProgram(void) {
    for (int i = 1; i < MAXPROG; i++)
        if (!g_prog[i].used) {
            g_prog[i].used = 1;
            g_prog[i].vs = g_prog[i].fs = 0;
            g_prog[i].msl = NULL;
            g_prog[i].nuni = g_prog[i].ubuf_size = g_prog[i].nsamp = 0;
            for (int s = 0; s < 4; s++) g_prog[i].samp_units[s] = 0;
            return i;
        }
    return 0;
}
void glAttachShader(GLuint p, GLuint sh) {
    if (p >= MAXPROG || !g_prog[p].used || sh >= MAXSH) return;
    if (g_sh[sh].type == GL_VERTEX_SHADER) g_prog[p].vs = sh;
    else g_prog[p].fs = sh;
}
void glLinkProgram(GLuint p) {
    if (p >= MAXPROG || !g_prog[p].used) return;
    struct prog *pr = &g_prog[p];
    if (pr->vs && pr->fs && g_sh[pr->vs].src && g_sh[pr->fs].src) {
        free(pr->msl);
        pr->msl = translate(g_sh[pr->vs].src, g_sh[pr->fs].src);
        if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glLinkProgram p=%u msl=%s (len=%zu)\n", p, pr->msl?"OK":"NULL", pr->msl?strlen(pr->msl):0);
        if (getenv("HL_SHADER_LOG") && pr->msl) {
            fprintf(stderr, "[shader-msl-begin program=%u]\n%s\n[shader-msl-end program=%u]\n", p, pr->msl, p);
        }
        if (getenv("HL_SHADER_DUMP_DIR")) {
            const char *dir = getenv("HL_SHADER_DUMP_DIR");
            char name[64];
            snprintf(name, sizeof name, "program-%u.vert.glsl", p);
            dump_text_file(dir, name, g_sh[pr->vs].src);
            snprintf(name, sizeof name, "program-%u.frag.glsl", p);
            dump_text_file(dir, name, g_sh[pr->fs].src);
            snprintf(name, sizeof name, "program-%u.metal", p);
            dump_text_file(dir, name, pr->msl);
        }
        pr->nuni = uni_layout(g_sh[pr->vs].src, g_sh[pr->fs].src, pr->unis, 16, &pr->ubuf_size);
        memset(pr->ubuf, 0, sizeof pr->ubuf);
        // Record sampler-uniform names → texture/sampler bind slots (index = declaration order).
        struct decl du[16], su[4];
        int ndu, nsu;
        collect_uniforms(g_sh[pr->vs].src, g_sh[pr->fs].src, du, &ndu, su, &nsu);
        pr->nsamp = nsu;
        for (int i = 0; i < 4; i++) {
            pr->samp_units[i] = 0; // GL sampler uniforms default to texture unit 0.
            pr->samp[i][0] = 0;
        }
        for (int i = 0; i < nsu; i++) strcpy(pr->samp[i], su[i].name);
    }
}
GLint glGetUniformLocation(GLuint p, const GLchar *name) {
    if (p < MAXPROG && g_prog[p].used) {
        for (int i = 0; i < g_prog[p].nuni; i++)
            if (!strcmp(g_prog[p].unis[i].name, name)) return g_prog[p].unis[i].off; // location = byte offset
        // sampler2D uniform: a sentinel location (>uniform-block) so glUniform1i records texture-unit mapping.
        for (int i = 0; i < g_prog[p].nsamp; i++)
            if (!strcmp(g_prog[p].samp[i], name)) return 100000 + i;
    }
    return -1;
}
static void uni_write(GLint loc, const void *data, int n) {
    if (loc >= 0 && loc + n <= (int)sizeof(g_ubuf)) {
        memcpy(g_ubuf + loc, data, n);
        if (g_cur_prog < MAXPROG && g_prog[g_cur_prog].used) {
            memcpy(g_prog[g_cur_prog].ubuf + loc, data, n);
        }
        if (getenv("HL_DRAW_DEBUG")) {
            const float *f = (const float *)data;
            fprintf(stderr, "[uniform] prog=%u loc=%d bytes=%d f=%g,%g,%g,%g\n",
                    g_cur_prog, loc, n, n >= 4 ? f[0] : 0.0f, n >= 8 ? f[1] : 0.0f,
                    n >= 12 ? f[2] : 0.0f, n >= 16 ? f[3] : 0.0f);
        }
    }
}
// Write a GLSL column-major matrix (cols x rows, tightly packed by the client — each column is `rows`
// contiguous floats) into the uniform block at `loc`, expanding each column to its MSL column stride.
// MSL packs a 3-row column (float3) into 16 bytes, so mat3/mat2x3/mat4x3 need per-column re-striding;
// 2-row and 4-row columns are already tight (float2=8, float4=16) and copy in one shot.
static void uni_write_matrix(GLint loc, const GLfloat *v, int cols, int rows) {
    int col_stride = (rows == 3) ? 16 : rows * 4; // MSL bytes per column
    if (col_stride == rows * 4) { uni_write(loc, v, cols * rows * 4); return; } // tight → single copy
    for (int c = 0; c < cols; c++) uni_write(loc + c * col_stride, v + c * rows, rows * 4);
}
void glUniformMatrix4fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 4, 4); }
void glUniform4fv(GLint l, GLsizei n, const GLfloat *v) { (void)n; uni_write(l, v, 16); }
void glUniform4f(GLint l, GLfloat a, GLfloat b, GLfloat c, GLfloat d) { float v[4] = {a, b, c, d}; uni_write(l, v, 16); }
void glUniform3fv(GLint l, GLsizei n, const GLfloat *v) { (void)n; uni_write(l, v, 12); }
void glUniform3f(GLint l, GLfloat a, GLfloat b, GLfloat c) { float v[3] = {a, b, c}; uni_write(l, v, 12); }
void glUniform1f(GLint l, GLfloat a) { uni_write(l, &a, 4); }
void glUniform1i(GLint l, GLint a) {
    if (l >= 100000 && l < 100004) {
        int si = l - 100000;
        if (g_cur_prog < MAXPROG && g_prog[g_cur_prog].used && si < g_prog[g_cur_prog].nsamp) {
            g_prog[g_cur_prog].samp_units[si] = a;
            if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glUniform1i sampler prog=%u idx=%d unit=%d\n", g_cur_prog, si, a);
        }
        return;
    }
    uni_write(l, &a, 4);
}
void glGetProgramiv(GLuint p, GLenum pn, GLint *v) { (void)p; if (pn == GL_LINK_STATUS && v) *v = GL_TRUE; else if (v) *v = 0; }
void glGetProgramInfoLog(GLuint p, GLsizei bufSize, GLsizei *length, GLchar *infoLog) { (void)p; (void)bufSize; if (length) *length = 0; if (infoLog && bufSize) infoLog[0] = 0; }
void glUseProgram(GLuint p) { if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glUseProgram(%u)\n", p); g_cur_prog = p; }
GLint glGetAttribLocation(GLuint p, const GLchar *name) {
    // declaration-order index in the vertex shader (matches our VIn attribute() numbering)
    if (p < MAXPROG && g_prog[p].used && g_prog[p].vs && g_sh[g_prog[p].vs].src) {
        struct decl at[16];
        int na = collect_vertex_attrs(g_sh[g_prog[p].vs].src, at, 16);
        for (int i = 0; i < na; i++)
            if (!strcmp(at[i].name, name)) { if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glGetAttribLocation(%s) -> %d\n", name, i); return i; }
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glGetAttribLocation(%s) -> -1\n", name);
    return -1;
}
void glGenBuffers(GLsizei n, GLuint *b) {
    for (int k = 0; k < n; k++) {
        b[k] = 0;
        for (int i = 1; i < MAXBUF; i++)
            if (!g_buf[i].used) { g_buf[i].used = 1; g_buf[i].data = NULL; g_buf[i].size = 0; g_buf[i].gen++; b[k] = i; break; }
    }
}
void glBindBuffer(GLenum t, GLuint b) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBindBuffer t=0x%x b=%u\n", t, b);
    if (t == GL_ARRAY_BUFFER) g_arr_buf = b;
    else if (t == GL_ELEMENT_ARRAY_BUFFER) {
        g_elem_buf = b;
        vao_store_current();
    }
}
void glBufferData(GLenum t, GLsizeiptr size, const void *data, GLenum usage) {
    GLuint b = (t == GL_ELEMENT_ARRAY_BUFFER) ? g_elem_buf : g_arr_buf;
    if ((t != GL_ARRAY_BUFFER && t != GL_ELEMENT_ARRAY_BUFFER) || b >= MAXBUF || !g_buf[b].used) return;
    free(g_buf[b].data);
    g_buf[b].data = malloc(size);
    g_buf[b].size = size;
    g_buf[b].usage = usage;
    if (!g_buf[b].data) { g_buf[b].size = 0; return; }
    if (data) memcpy(g_buf[b].data, data, size);
    g_buf[b].gen++; // L5: content changed → next swap re-uploads this buffer
}
void glBufferSubData(GLenum t, GLintptr off, GLsizeiptr size, const void *data) {
    GLuint b = (t == GL_ELEMENT_ARRAY_BUFFER) ? g_elem_buf : g_arr_buf;
    if (b >= MAXBUF || !g_buf[b].used || !g_buf[b].data) return;
    if ((size_t)off + (size_t)size <= g_buf[b].size && data) memcpy(g_buf[b].data + off, data, size);
    g_buf[b].gen++; // L5: content changed → next swap re-uploads this buffer
}

// ---- textures (sampler2D path) ----
void glGenTextures(GLsizei n, GLuint *t) {
    for (int k = 0; k < n; k++) {
        t[k] = 0;
        for (int i = 1; i < MAXTEX; i++)
            if (!g_tex[i].used) {
                g_tex[i].used = 1; g_tex[i].data = NULL; g_tex[i].size = 0;
                g_tex[i].minf = GL_LINEAR; g_tex[i].magf = GL_LINEAR;
                g_tex[i].ws = GL_REPEAT; g_tex[i].wt = GL_REPEAT;
                g_tex[i].gen++; // L5: id (re)allocated → invalidate any stale residency for its slot
                t[k] = i; break;
            }
    }
}
void glDeleteTextures(GLsizei n, const GLuint *t) {
    for (int k = 0; k < n; k++) {
        GLuint i = t[k];
        if (i && i < MAXTEX && g_tex[i].used) { free(g_tex[i].data); g_tex[i].used = 0; g_tex[i].data = NULL; g_tex[i].gen++; }
    }
}
void glActiveTexture(GLenum unit) { int u = (int)unit - GL_TEXTURE0; if (u >= 0 && u < 8) g_active_unit = u; }
void glBindTexture(GLenum target, GLuint t) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBindTexture target=0x%x unit=%d tex=%u\n", target, g_active_unit, t);
    if (g_active_unit < 8) g_tex_unit[g_active_unit] = t;
}
static int tex_bpp(GLenum fmt) {
    if (fmt == GL_RGBA || fmt == GL_BGRA_EXT) return 4;
    if (fmt == GL_RGB) return 3;
    return 1;
}
static void tex_store_pixels(struct tex *t, int xo, int yo, int w, int h, GLenum fmt, const void *pixels) {
    if (!t || !t->data || w <= 0 || h <= 0) return;
    if (xo < 0 || yo < 0 || xo + w > t->w || yo + h > t->h) return;
    const uint8_t *p = pixels;
    int bpp = tex_bpp(fmt);
    int row_pixels = g_unpack_row_length > 0 ? g_unpack_row_length : w;
    size_t row_bytes = (size_t)row_pixels * (size_t)bpp;
    if (g_unpack_alignment > 1) {
        size_t a = (size_t)g_unpack_alignment;
        row_bytes = (row_bytes + a - 1) & ~(a - 1);
    }
    for (int y = 0; y < h; y++) {
        const uint8_t *row = p ? p + ((size_t)(y + g_unpack_skip_rows) * row_bytes) + (size_t)g_unpack_skip_pixels * (size_t)bpp : NULL;
        for (int x = 0; x < w; x++) {
            uint8_t r = 0, gg = 0, b = 0, a = 255;
            if (row) {
                const uint8_t *s = row + (size_t)x * (size_t)bpp;
                if (fmt == GL_RGBA) { r = s[0]; gg = s[1]; b = s[2]; a = s[3]; }
                else if (fmt == GL_BGRA_EXT) { b = s[0]; gg = s[1]; r = s[2]; a = s[3]; }
                else if (fmt == GL_RGB) { r = s[0]; gg = s[1]; b = s[2]; }
                else if (fmt == GL_RED) { r = s[0]; }
                else if (fmt == GL_ALPHA) { a = s[0]; }
                else if (fmt == GL_LUMINANCE) { r = gg = b = s[0]; }
                else { r = gg = b = s[0]; }
            }
            if (getenv("HL_PREMULTIPLY_UPLOAD") && a < 255) {
                r = (uint8_t)(((unsigned)r * (unsigned)a + 127u) / 255u);
                gg = (uint8_t)(((unsigned)gg * (unsigned)a + 127u) / 255u);
                b = (uint8_t)(((unsigned)b * (unsigned)a + 127u) / 255u);
            }
            size_t di = ((size_t)(yo + y) * (size_t)t->w + (size_t)(xo + x)) * 4;
            t->data[di] = r; t->data[di + 1] = gg; t->data[di + 2] = b; t->data[di + 3] = a;
        }
    }
}
static int tex_alloc_rgba(GLuint id, int w, int h) {
    if (id >= MAXTEX || !g_tex[id].used || w <= 0 || h <= 0) return 0;
    size_t sz = (size_t)w * (size_t)h * 4u;
    uint8_t *data = calloc(1, sz);
    if (!data) return 0;
    free(g_tex[id].data);
    g_tex[id].data = data;
    g_tex[id].w = w;
    g_tex[id].h = h;
    g_tex[id].size = sz;
    g_tex[id].gen++;
    return 1;
}
static GLuint fbo_color_texture(GLuint fbo) {
    if (fbo == 0 || fbo >= MAXFBO || !g_fbo[fbo].used) return 0;
    GLuint tex = g_fbo[fbo].color_tex;
    if (tex >= MAXTEX || !g_tex[tex].used || !g_tex[tex].data) return 0;
    return tex;
}
static int clampi(int v, int lo, int hi) {
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}
static void copy_texture_rect(GLuint src_id, GLuint dst_id,
                              int sx0, int sy0, int sx1, int sy1,
                              int dx0, int dy0, int dx1, int dy1) {
    if (src_id >= MAXTEX || dst_id >= MAXTEX) return;
    struct tex *src = &g_tex[src_id], *dst = &g_tex[dst_id];
    if (!src->used || !dst->used || !src->data || !dst->data) return;
    int dw = dx1 - dx0, dh = dy1 - dy0;
    if (dw == 0 || dh == 0 || sx0 == sx1 || sy0 == sy1) return;
    int adw = dw < 0 ? -dw : dw;
    int adh = dh < 0 ? -dh : dh;
    for (int j = 0; j < adh; j++) {
        int dy = dy0 + (dh < 0 ? -j : j);
        if (dy < 0 || dy >= dst->h) continue;
        int sy = sy0 + (int)(((int64_t)j * (int64_t)(sy1 - sy0)) / adh);
        sy = clampi(sy, 0, src->h - 1);
        for (int i = 0; i < adw; i++) {
            int dx = dx0 + (dw < 0 ? -i : i);
            if (dx < 0 || dx >= dst->w) continue;
            int sx = sx0 + (int)(((int64_t)i * (int64_t)(sx1 - sx0)) / adw);
            sx = clampi(sx, 0, src->w - 1);
            const uint8_t *sp = src->data + ((size_t)sy * (size_t)src->w + (size_t)sx) * 4u;
            uint8_t *dp = dst->data + ((size_t)dy * (size_t)dst->w + (size_t)dx) * 4u;
            dp[0] = sp[0]; dp[1] = sp[1]; dp[2] = sp[2]; dp[3] = sp[3];
        }
    }
    dst->gen++;
}
static int read_draw_index(const struct draw_call *d, int n, uint32_t *out) {
    if (!d || !out || !d->indexed) return 0;
    const uint8_t *data = d->snap_ibo_data;
    size_t size = d->snap_ibo_size;
    if (!data) {
        GLuint b = d->elem_buf;
        if (b >= MAXBUF || !g_buf[b].used || !g_buf[b].data) return 0;
        data = g_buf[b].data;
        size = g_buf[b].size;
    }
    size_t off = d->index_offset;
    if (d->index_type == GL_UNSIGNED_INT) {
        if (off + (size_t)(n + 1) * 4u > size) return 0;
        const uint32_t *p = (const uint32_t *)(const void *)(data + off);
        *out = p[n];
        return 1;
    }
    if (off + (size_t)(n + 1) * 2u > size) return 0;
    const uint16_t *p = (const uint16_t *)(const void *)(data + off);
    *out = p[n];
    return 1;
}
static size_t attr_elem_size(GLenum type) {
    return (type == GL_FLOAT || type == GL_UNSIGNED_INT || type == GL_INT) ? 4u :
           (type == GL_UNSIGNED_SHORT || type == GL_SHORT || type == GL_HALF_FLOAT) ? 2u : 1u;
}
/* IEEE-754 binary16 → binary32 (for GL_HALF_FLOAT vertex attribute readback). */
static float half_to_float(uint16_t h) {
    uint32_t sign = (uint32_t)(h & 0x8000u) << 16;
    uint32_t exp = (h >> 10) & 0x1F, man = h & 0x3FF, bits;
    if (exp == 0) {
        if (man == 0) bits = sign; /* ±0 */
        else { /* subnormal → normalized float */
            exp = 127 - 15 + 1;
            while (!(man & 0x400)) { man <<= 1; exp--; }
            man &= 0x3FF;
            bits = sign | (exp << 23) | (man << 13);
        }
    } else if (exp == 0x1F) {
        bits = sign | 0x7F800000u | (man << 13); /* inf/nan */
    } else {
        bits = sign | ((exp - 15 + 127) << 23) | (man << 13);
    }
    float f;
    memcpy(&f, &bits, 4);
    return f;
}
static int draw_vbo_snapshot_index(const struct draw_call *d, GLuint src) {
    if (!d || src == 0) return -1;
    for (int i = 0; i < d->snap_vbo_count && i < MAXATTR; i++) {
        if (d->snap_vbo_src[i] == src && d->snap_vbo_data[i]) return i;
    }
    return -1;
}
static void free_draw_snapshots(void) {
    for (int d = 0; d < g_ndraws; d++) {
        for (int i = 0; i < g_draws[d].snap_vbo_count && i < MAXATTR; i++) {
            free(g_draws[d].snap_vbo_data[i]);
            g_draws[d].snap_vbo_data[i] = NULL;
        }
        g_draws[d].snap_vbo_count = 0;
        free(g_draws[d].snap_ibo_data);
        g_draws[d].snap_ibo_data = NULL;
        g_draws[d].snap_ibo_size = 0;
    }
}
static void snapshot_draw_buffer(struct draw_call *d, GLuint src) {
    if (!d || src == 0 || src >= MAXBUF || !g_buf[src].used || !g_buf[src].data || g_buf[src].size == 0) return;
    if (draw_vbo_snapshot_index(d, src) >= 0) return;
    if (d->snap_vbo_count >= MAXATTR) return;
    uint8_t *copy = (uint8_t *)malloc(g_buf[src].size);
    if (!copy) return;
    memcpy(copy, g_buf[src].data, g_buf[src].size);
    int si = d->snap_vbo_count++;
    d->snap_vbo_src[si] = src;
    d->snap_vbo_gen[si] = g_buf[src].gen;
    d->snap_vbo_data[si] = copy;
    d->snap_vbo_size[si] = g_buf[src].size;
}
static void snapshot_draw_buffers(struct draw_call *d) {
    if (!d) return;
    for (int i = 0; i < MAXATTR; i++) {
        if (!d->attrs[i].enabled) continue;
        snapshot_draw_buffer(d, d->attrs[i].buffer);
    }
    if (d->indexed && d->elem_buf > 0 && d->elem_buf < MAXBUF &&
        g_buf[d->elem_buf].used && g_buf[d->elem_buf].data && g_buf[d->elem_buf].size > 0) {
        uint8_t *copy = (uint8_t *)malloc(g_buf[d->elem_buf].size);
        if (copy) {
            memcpy(copy, g_buf[d->elem_buf].data, g_buf[d->elem_buf].size);
            d->snap_ibo_src = d->elem_buf;
            d->snap_ibo_gen = g_buf[d->elem_buf].gen;
            d->snap_ibo_data = copy;
            d->snap_ibo_size = g_buf[d->elem_buf].size;
        }
    }
}
static int draw_vbo_slots(const struct draw_call *d, int slot_vbo[MAXATTR],
                          int attr_slot[MAXATTR], uint32_t slot_stride[MAXATTR]) {
    int nslot = 0;
    for (int i = 0; i < MAXATTR; i++) {
        attr_slot[i] = -1;
        slot_stride[i] = 0;
        slot_vbo[i] = 0;
    }
    if (!d) return 0;
    for (int i = 0; i < MAXATTR; i++) {
        if (!d->attrs[i].enabled) continue;
        int b = (int)d->attrs[i].buffer;
        if (b <= 0) continue;
        if (draw_vbo_snapshot_index(d, (GLuint)b) < 0 &&
            (b >= MAXBUF || !g_buf[b].used || !g_buf[b].data)) continue;
        int sl = -1;
        for (int k = 0; k < nslot; k++) {
            if (slot_vbo[k] == b) { sl = k; break; }
        }
        if (sl < 0) {
            if (nslot >= MAXATTR) continue;
            sl = nslot;
            slot_vbo[nslot++] = b;
        }
        attr_slot[i] = sl;
        uint32_t st = (uint32_t)d->attrs[i].stride;
        if (st == 0) st = (uint32_t)((size_t)d->attrs[i].size * attr_elem_size(d->attrs[i].type));
        if (st > slot_stride[sl]) slot_stride[sl] = st;
    }
    for (int sl = 0; sl < nslot; sl++) {
        if (slot_stride[sl] == 0) slot_stride[sl] = 16;
    }
    return nslot;
}
static int read_attr_float(const struct attr *a, uint32_t vertex, int comp, float *out) {
    if (!a || !out || !a->enabled || comp < 0 || comp >= a->size) return 0;
    const uint8_t *data = NULL;
    size_t size = 0;
    GLuint b = a->buffer;
    if (b >= MAXBUF || !g_buf[b].used || !g_buf[b].data) return 0;
    data = g_buf[b].data;
    size = g_buf[b].size;
    size_t elem = attr_elem_size(a->type);
    size_t stride = a->stride ? (size_t)a->stride : (size_t)a->size * elem;
    size_t off = (size_t)vertex * stride + a->offset + (size_t)comp * elem;
    if (off + elem > size) return 0;
    const uint8_t *p = data + off;
    if (a->type == GL_FLOAT) memcpy(out, p, 4);
    else if (a->type == GL_UNSIGNED_BYTE) *out = a->normalized ? (float)p[0] / 255.0f : (float)p[0];
    else if (a->type == GL_BYTE) { int8_t v; memcpy(&v, p, 1); *out = a->normalized ? (float)v / 127.0f : (float)v; }
    else if (a->type == GL_UNSIGNED_SHORT) { uint16_t v; memcpy(&v, p, 2); *out = a->normalized ? (float)v / 65535.0f : (float)v; }
    else if (a->type == GL_SHORT) { int16_t v; memcpy(&v, p, 2); *out = a->normalized ? (float)v / 32767.0f : (float)v; }
    else if (a->type == GL_UNSIGNED_INT) { uint32_t v; memcpy(&v, p, 4); *out = (float)v; }
    else if (a->type == GL_INT) { int32_t v; memcpy(&v, p, 4); *out = (float)v; }
    else if (a->type == GL_HALF_FLOAT) { uint16_t h; memcpy(&h, p, 2); *out = half_to_float(h); }
    else return 0;
    return 1;
}
static void texture_stats(GLuint tex, int *w, int *h, int *mean_rgb, int *mean_a, int *min_rgb, int *max_rgb) {
    *w = *h = *mean_rgb = *mean_a = *min_rgb = *max_rgb = 0;
    if (tex >= MAXTEX || !g_tex[tex].used || !g_tex[tex].data || g_tex[tex].w <= 0 || g_tex[tex].h <= 0) return;
    struct tex *t = &g_tex[tex];
    *w = t->w; *h = t->h; *min_rgb = 255;
    uint64_t srgb = 0, sa = 0, n = 0;
    int step = (t->w * t->h > 65536) ? 4 : 1;
    for (int y = 0; y < t->h; y += step) {
        for (int x = 0; x < t->w; x += step) {
            const uint8_t *p = t->data + ((size_t)y * (size_t)t->w + (size_t)x) * 4u;
            int l = ((int)p[0] + (int)p[1] + (int)p[2]) / 3;
            if (l < *min_rgb) *min_rgb = l;
            if (l > *max_rgb) *max_rgb = l;
            srgb += (uint64_t)l;
            sa += (uint64_t)p[3];
            n++;
        }
    }
    if (n) {
        *mean_rgb = (int)(srgb / n);
        *mean_a = (int)(sa / n);
    }
}
static void debug_draw_call(const struct draw_call *d, int draw_index) {
    if (!getenv("HL_DRAW_DEBUG") || !d) return;
    uint32_t sample_tex = 0;
    int unit = (d->samp_units[0] >= 0 && d->samp_units[0] < 8) ? d->samp_units[0] : 0;
    sample_tex = d->tex_units[unit];
    int tw, th, mean, alpha, minv, maxv;
    texture_stats(sample_tex, &tw, &th, &mean, &alpha, &minv, &maxv);
    fprintf(stderr,
            "[drawdbg] #%d prog=%u target=%u mode=0x%x count=%d indexed=%d ib=%u ioff=%zu samp0_unit=%d tex=%u texsz=%dx%d mean=%d alpha=%d range=%d..%d vp=%d,%d %dx%d sc=%d %d,%d %dx%d blend=%d bf=0x%x,0x%x,0x%x,0x%x\n",
            draw_index, d->prog, d->target_tex, d->mode, d->count, d->indexed, d->elem_buf,
            d->index_offset, unit, sample_tex, tw, th, mean, alpha, minv, maxv,
            d->viewport[0], d->viewport[1], d->viewport[2], d->viewport[3],
            d->scissor_enabled, d->scissor[0], d->scissor[1], d->scissor[2], d->scissor[3], d->blend,
            d->blend_src_rgb, d->blend_dst_rgb, d->blend_src_alpha, d->blend_dst_alpha);
    for (int i = 0; i < MAXATTR; i++) {
        if (!d->attrs[i].enabled) continue;
        fprintf(stderr, "[drawdbg] #%d attr%d buf=%u size=%d type=0x%x norm=%d int=%d stride=%d off=%zu\n",
                draw_index, i, d->attrs[i].buffer, d->attrs[i].size, d->attrs[i].type,
                d->attrs[i].normalized, d->attrs[i].integer, d->attrs[i].stride, d->attrs[i].offset);
    }
    int n = d->count < 6 ? d->count : 6;
    for (int k = 0; k < n; k++) {
        uint32_t v = d->first + (uint32_t)k;
        if (d->indexed) {
            if (!read_draw_index(d, k, &v)) continue;
        }
        float p0 = 0, p1 = 0, u0 = 0, u1 = 0, c0 = 0, c1 = 0, c2 = 0, c3 = 0;
        int hp = read_attr_float(&d->attrs[0], v, 0, &p0) && read_attr_float(&d->attrs[0], v, 1, &p1);
        int hu = read_attr_float(&d->attrs[1], v, 0, &u0) && read_attr_float(&d->attrs[1], v, 1, &u1);
        int hc = read_attr_float(&d->attrs[1], v, 0, &c0) && read_attr_float(&d->attrs[1], v, 1, &c1) &&
                 read_attr_float(&d->attrs[1], v, 2, &c2) && read_attr_float(&d->attrs[1], v, 3, &c3);
        fprintf(stderr, "[drawdbg] #%d v%d idx=%u pos=%s%g,%g uv=%s%g,%g color=%s%g,%g,%g,%g\n",
                draw_index, k, v, hp ? "" : "?", p0, p1, hu ? "" : "?", u0, u1,
                hc ? "" : "?", c0, c1, c2, c3);
    }
    fflush(stderr);
}
void glTexImage2D(GLenum target, GLint level, GLint ifmt, GLsizei w, GLsizei h, GLint border, GLenum fmt, GLenum type, const void *pixels) {
    (void)target; (void)ifmt; (void)border; (void)type;
    GLuint t = g_tex_unit[g_active_unit];
    if (level != 0 || t >= MAXTEX || !g_tex[t].used) return;
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glTexImage2D unit=%d tex=%u ifmt=0x%x %dx%d fmt=0x%x type=0x%x pixels=%p\n", g_active_unit, t, ifmt, w, h, fmt, type, pixels);
    free(g_tex[t].data);
    g_tex[t].w = w; g_tex[t].h = h;
    g_tex[t].size = (size_t)w * h * 4;
    g_tex[t].data = calloc(1, g_tex[t].size);
    g_tex[t].gen++; // L5: pixels changed → next swap re-uploads (staging buffer + CopyBufferToTexture)
    tex_store_pixels(&g_tex[t], 0, 0, w, h, fmt, pixels);
    dump_tex_ppm(t, &g_tex[t], "image");
}
void glTexParameteri(GLenum target, GLenum p, GLint v) {
    (void)target;
    GLuint t = g_tex_unit[g_active_unit];
    if (t >= MAXTEX || !g_tex[t].used) return;
    if (p == GL_TEXTURE_MIN_FILTER) g_tex[t].minf = v;
    else if (p == GL_TEXTURE_MAG_FILTER) g_tex[t].magf = v;
    else if (p == GL_TEXTURE_WRAP_S) g_tex[t].ws = v;
    else if (p == GL_TEXTURE_WRAP_T) g_tex[t].wt = v;
}
void glTexParameterf(GLenum target, GLenum p, GLfloat v) { glTexParameteri(target, p, (GLint)v); }
void glPixelStorei(GLenum p, GLint v) {
    if (p == 0x0CF5) g_unpack_alignment = v > 0 ? v : 1; // GL_UNPACK_ALIGNMENT
    else if (p == 0x0CF2) g_unpack_row_length = v;        // GL_UNPACK_ROW_LENGTH
    else if (p == 0x0CF3) g_unpack_skip_rows = v;         // GL_UNPACK_SKIP_ROWS
    else if (p == 0x0CF4) g_unpack_skip_pixels = v;       // GL_UNPACK_SKIP_PIXELS
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glPixelStorei 0x%x=%d row=%d skip=%d,%d align=%d\n", p, v, g_unpack_row_length, g_unpack_skip_pixels, g_unpack_skip_rows, g_unpack_alignment);
}
void glGenerateMipmap(GLenum target) { (void)target; }
static void record_draw_call(int mode, int first, int count, int indexed, GLenum type, const void *indices) {
    if (g_ndraws >= MAXDRAWS) return;
    struct draw_call *d = &g_draws[g_ndraws++];
    memset(d, 0, sizeof *d);
    d->is_clear = 0;
    d->mode = mode;
    d->first = first;
    d->count = count;
    d->indexed = indexed;
    d->index_type = (int)type;
    d->index_offset = (size_t)indices;
    d->prog = g_cur_prog;
    d->elem_buf = g_elem_buf;
    d->target_tex = (g_draw_fbo > 0 && g_draw_fbo < MAXFBO && g_fbo[g_draw_fbo].used) ? g_fbo[g_draw_fbo].color_tex : 0;
    memcpy(d->attrs, g_attr, sizeof d->attrs);
    memcpy(d->tex_units, g_tex_unit, sizeof d->tex_units);
    for (int i = 0; i < 4; i++) d->samp_units[i] = (g_cur_prog < MAXPROG && g_prog[g_cur_prog].used) ? g_prog[g_cur_prog].samp_units[i] : 0;
    memcpy(d->viewport, g_viewport, sizeof d->viewport);
    d->scissor_enabled = g_scissor_enabled;
    memcpy(d->scissor, g_scissor, sizeof d->scissor);
    d->blend = g_blend;
    d->blend_src_rgb = g_blend_src_rgb;
    d->blend_dst_rgb = g_blend_dst_rgb;
    d->blend_src_alpha = g_blend_src_alpha;
    d->blend_dst_alpha = g_blend_dst_alpha;
    d->blend_eq_rgb = g_blend_eq_rgb;
    d->blend_eq_alpha = g_blend_eq_alpha;
    memcpy(d->clear, g_clear, sizeof d->clear);
    d->clear_serial = g_clear_serial;
    if (g_cur_prog < MAXPROG && g_prog[g_cur_prog].used) memcpy(d->ubuf, g_prog[g_cur_prog].ubuf, sizeof d->ubuf);
    else memcpy(d->ubuf, g_ubuf, sizeof d->ubuf);
    snapshot_draw_buffers(d);
    if (getenv("HL_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] record_draw #%d prog=%u target_tex=%u mode=0x%x count=%d samp_units=%d,%d,%d,%d tex_units=%u,%u,%u,%u\n",
                g_ndraws - 1, d->prog, d->target_tex, mode, count, d->samp_units[0], d->samp_units[1], d->samp_units[2], d->samp_units[3],
                d->tex_units[0], d->tex_units[1], d->tex_units[2], d->tex_units[3]);
    }
    debug_draw_call(d, g_ndraws - 1);
}
static void record_clear_call(int x, int y, int w, int h) {
    if (g_ndraws >= MAXDRAWS) return;
    struct draw_call *d = &g_draws[g_ndraws++];
    memset(d, 0, sizeof *d);
    d->is_clear = 1;
    d->target_tex = (g_draw_fbo > 0 && g_draw_fbo < MAXFBO && g_fbo[g_draw_fbo].used) ? g_fbo[g_draw_fbo].color_tex : 0;
    d->clear_rect[0] = x;
    d->clear_rect[1] = y;
    d->clear_rect[2] = w;
    d->clear_rect[3] = h;
    memcpy(d->clear, g_clear, sizeof d->clear);
    d->clear_serial = g_clear_serial;
    if (getenv("HL_SHIM_DEBUG"))
        fprintf(stderr, "[shim] record_clear #%d target_tex=%u rect=%d,%d %dx%d color=%g,%g,%g,%g\n",
                g_ndraws - 1, d->target_tex, x, y, w, h, d->clear[0], d->clear[1], d->clear[2], d->clear[3]);
}
void glDrawElements(GLenum mode, GLsizei count, GLenum type, const void *indices) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glDrawElements mode=0x%x count=%d type=0x%x\n", mode, count, type);
    g_draw_mode = mode;
    g_draw_count = count;
    g_draw_indexed = 1;
    g_index_type = type;
    g_index_offset = (size_t)indices;
    memcpy(g_attr_snap, g_attr, sizeof g_attr); g_have_draw_snap = 1;
    record_draw_call(mode, 0, count, 1, type, indices);
}
void glVertexAttribPointer(GLuint i, GLint size, GLenum type, GLboolean norm, GLsizei stride, const void *ptr) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glVertexAttribPointer i=%u size=%d type=0x%x norm=%u stride=%d off=%zu arrbuf=%d\n", i, size, type, (unsigned)norm, stride, (size_t)ptr, g_arr_buf);
    if (i >= MAXATTR) return;
    g_attr[i].size = size;
    g_attr[i].type = type;
    g_attr[i].normalized = norm ? 1 : 0;
    g_attr[i].integer = 0;
    g_attr[i].stride = stride;
    g_attr[i].offset = (size_t)ptr;
    g_attr[i].buffer = g_arr_buf;
    vao_store_current();
}
void glEnableVertexAttribArray(GLuint i) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glEnableVertexAttribArray(%u) [MAXATTR=%d]\n", i, MAXATTR);
    if (i < MAXATTR) {
        g_attr[i].enabled = 1;
        vao_store_current();
    }
}
void glDisableVertexAttribArray(GLuint i) {
    if (i < MAXATTR) {
        g_attr[i].enabled = 0;
        vao_store_current();
    }
}
void glDrawArrays(GLenum mode, GLint first, GLsizei count) { if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glDrawArrays(0x%x,%d,%d)\n", mode, first, count); g_draw_mode = mode; g_draw_first = first; g_draw_count = count; g_draw_indexed = 0; memcpy(g_attr_snap, g_attr, sizeof g_attr); g_have_draw_snap = 1; record_draw_call(mode, first, count, 0, 0, NULL); }

// ---- state setters (no-ops for our forward-renderer) + getters glmark2 queries ----
#define GL_MAX_TEXTURE_SIZE 0x0D33
#define GL_MAX_CUBE_MAP_TEXTURE_SIZE 0x851C
#define GL_MAX_RENDERBUFFER_SIZE 0x84E8
#define GL_MAX_VERTEX_ATTRIBS 0x8869
#define GL_MAX_TEXTURE_IMAGE_UNITS 0x8872
#define GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS 0x8B4D
#define GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS 0x8B4C
#define GL_MAX_FRAGMENT_UNIFORM_VECTORS 0x8DFD
#define GL_MAX_VERTEX_UNIFORM_VECTORS 0x8DFB
#define GL_MAX_VARYING_VECTORS 0x8DFC
#define GL_NUM_COMPRESSED_TEXTURE_FORMATS 0x86A2
#define GL_MAX_VIEWPORT_DIMS 0x0D3A
#define GL_VIEWPORT 0x0BA2
#define GL_CURRENT_PROGRAM 0x8B8D
#define GL_ACTIVE_TEXTURE 0x84E0
#define GL_ARRAY_BUFFER_BINDING 0x8894
#define GL_ELEMENT_ARRAY_BUFFER_BINDING 0x8895
#define GL_TEXTURE_BINDING_2D 0x8069
#define GL_RENDERBUFFER_BINDING 0x8CA7
#define GL_DRAW_FRAMEBUFFER_BINDING 0x8CA6
#define GL_READ_FRAMEBUFFER_BINDING 0x8CAA
#define GL_DEPTH_BITS 0x0D56
#define GL_STENCIL_BITS 0x0D57
#define GL_RED_BITS 0x0D52
#define GL_SAMPLES 0x80A9
#define GL_MAX_SAMPLES_ES3 0x8D57
#define GL_NUM_SAMPLE_COUNTS 0x9380
void glGetIntegerv(GLenum p, GLint *v) {
    if (!v) return;
    switch (p) {
        case GL_MAX_TEXTURE_SIZE:
        case GL_MAX_CUBE_MAP_TEXTURE_SIZE:
        case GL_MAX_RENDERBUFFER_SIZE: *v = 4096; break;
        case GL_MAX_VERTEX_ATTRIBS: *v = 16; break;
        case GL_MAX_TEXTURE_IMAGE_UNITS:
        case GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS: *v = 8; break;
        case GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS: *v = 4; break;
        case GL_MAX_FRAGMENT_UNIFORM_VECTORS:
        case GL_MAX_VERTEX_UNIFORM_VECTORS: *v = 256; break;
        case GL_MAX_VARYING_VECTORS: *v = 15; break;
        case GL_NUM_COMPRESSED_TEXTURE_FORMATS:
        case GL_SAMPLES: *v = 0; break;
        case GL_CURRENT_PROGRAM: *v = (GLint)g_cur_prog; break;
        case GL_ACTIVE_TEXTURE: *v = GL_TEXTURE0 + g_active_unit; break;
        case GL_ARRAY_BUFFER_BINDING: *v = (GLint)g_arr_buf; break;
        case GL_ELEMENT_ARRAY_BUFFER_BINDING: *v = (GLint)g_elem_buf; break;
        case GL_TEXTURE_BINDING_2D: *v = (GLint)g_tex_unit[g_active_unit]; break;
        case GL_RENDERBUFFER_BINDING: *v = (GLint)g_rbo_bound; break;
        case GL_DRAW_FRAMEBUFFER_BINDING: *v = (GLint)g_draw_fbo; break;
        case GL_READ_FRAMEBUFFER_BINDING: *v = (GLint)g_read_fbo; break;
        case GL_MAX_SAMPLES_ES3: *v = 4; break;
        case 0x80E8: *v = 4096; break; // GL_MAX_ELEMENTS_VERTICES
        case 0x80E9: *v = 4096; break; // GL_MAX_ELEMENTS_INDICES
        case 0x8B49: *v = 1024; break; // GL_MAX_FRAGMENT_UNIFORM_COMPONENTS
        case 0x8B4A: *v = 1024; break; // GL_MAX_VERTEX_UNIFORM_COMPONENTS
        case 0x8B4B: *v = 60; break; // GL_MAX_VARYING_COMPONENTS
        case 0x8D6B: *v = 0x00ffffff; break; // GL_MAX_ELEMENT_INDEX
        case 0x910E: *v = 4; break; // GL_MAX_COLOR_TEXTURE_SAMPLES
        case 0x910F: *v = 4; break; // GL_MAX_DEPTH_TEXTURE_SAMPLES
        case 0x9110: *v = 4; break; // GL_MAX_INTEGER_SAMPLES
        case 0x821B: *v = g_ctx_major; break; // GL_MAJOR_VERSION
        case 0x821C: *v = g_ctx_minor; break; // GL_MINOR_VERSION
        case 0x821D: *v = g_gl_nexts; break; // GL_NUM_EXTENSIONS
        case 0x8824: *v = 4; break; // GL_MAX_DRAW_BUFFERS
        case 0x8CDF: *v = 4; break; // GL_MAX_COLOR_ATTACHMENTS
        case 0x8073: *v = 2048; break; // GL_MAX_3D_TEXTURE_SIZE
        case 0x88FF: *v = 256; break; // GL_MAX_ARRAY_TEXTURE_LAYERS
        case 0x8A2B: *v = 12; break; // GL_MAX_VERTEX_UNIFORM_BLOCKS
        case 0x8A2D: *v = 12; break; // GL_MAX_FRAGMENT_UNIFORM_BLOCKS
        case 0x8A2E: *v = 24; break; // GL_MAX_COMBINED_UNIFORM_BLOCKS
        case 0x8A2F: *v = 24; break; // GL_MAX_UNIFORM_BUFFER_BINDINGS
        case 0x8A30: *v = 16384; break; // GL_MAX_UNIFORM_BLOCK_SIZE
        case 0x8C8A: *v = 4; break; // GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS
        case 0x8C8B: *v = 4; break; // GL_MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS
        case 0x8C80: *v = 4; break; // GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS
        case 0x9122: *v = 16; break; // GL_MAX_VERTEX_OUTPUT_COMPONENTS
        case 0x9125: *v = 60; break; // GL_MAX_FRAGMENT_INPUT_COMPONENTS
        case 0x8905: *v = 8; break; // GL_MAX_SAMPLES_EXT
        case GL_DEPTH_BITS: *v = 24; break;
        case GL_STENCIL_BITS: *v = 8; break;
        case GL_RED_BITS: *v = 8; break;
        case GL_MAX_VIEWPORT_DIMS: v[0] = 4096; v[1] = 4096; break;
        case GL_VIEWPORT:
            v[0] = g_viewport[0];
            v[1] = g_viewport[1];
            v[2] = g_viewport[2] ? g_viewport[2] : (g_surf.width ? (GLint)g_surf.width : 256);
            v[3] = g_viewport[3] ? g_viewport[3] : (g_surf.height ? (GLint)g_surf.height : 256);
            break;
        case GL_SCISSOR_BOX:
            v[0] = g_scissor[0]; v[1] = g_scissor[1]; v[2] = g_scissor[2]; v[3] = g_scissor[3];
            break;
        default: *v = 0; break;
    }
    if (p == 0x821B || p == 0x821C || p == 0x821D) EGLDBG("glGetIntegerv(0x%x) -> %d\n", p, *v);
}
void glGetFloatv(GLenum p, GLfloat *v) { (void)p; if (v) *v = 0; }
void glGetBooleanv(GLenum p, GLboolean *v) { (void)p; if (v) *v = 0; }
void glDepthFunc(GLenum f) { (void)f; }
void glDepthMask(GLboolean f) { (void)f; }
void glDepthRangef(GLfloat n, GLfloat f) { (void)n; (void)f; }
void glClearDepthf(GLfloat d) { (void)d; }
void glClearStencil(GLint s) { (void)s; }
void glStencilFunc(GLenum f, GLint r, GLuint m) { (void)f; (void)r; (void)m; }
void glStencilOp(GLenum a, GLenum b, GLenum c) { (void)a; (void)b; (void)c; }
void glStencilMask(GLuint m) { (void)m; }
void glBlendFunc(GLenum s, GLenum d) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBlendFunc s=0x%x d=0x%x\n", s, d);
    g_blend_src_rgb = s; g_blend_dst_rgb = d;
    g_blend_src_alpha = s; g_blend_dst_alpha = d;
}
void glBlendFuncSeparate(GLenum a, GLenum b, GLenum c, GLenum d) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBlendFuncSeparate rgb=0x%x,0x%x a=0x%x,0x%x\n", a, b, c, d);
    g_blend_src_rgb = a; g_blend_dst_rgb = b;
    g_blend_src_alpha = c; g_blend_dst_alpha = d;
}
void glBlendFunci(GLuint buf, GLenum s, GLenum d) { if (buf == 0) glBlendFunc(s, d); }
void glBlendFuncSeparatei(GLuint buf, GLenum a, GLenum b, GLenum c, GLenum d) { if (buf == 0) glBlendFuncSeparate(a, b, c, d); }
void glBlendFunciEXT(GLuint buf, GLenum s, GLenum d) { glBlendFunci(buf, s, d); }
void glBlendFuncSeparateiEXT(GLuint buf, GLenum a, GLenum b, GLenum c, GLenum d) { glBlendFuncSeparatei(buf, a, b, c, d); }
void glBlendEquation(GLenum m) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBlendEquation m=0x%x\n", m);
    g_blend_eq_rgb = m; g_blend_eq_alpha = m;
}
void glBlendEquationSeparate(GLenum a, GLenum b) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBlendEquationSeparate rgb=0x%x a=0x%x\n", a, b);
    g_blend_eq_rgb = a; g_blend_eq_alpha = b;
}
void glBlendEquationi(GLuint buf, GLenum m) { if (buf == 0) glBlendEquation(m); }
void glBlendEquationSeparatei(GLuint buf, GLenum a, GLenum b) { if (buf == 0) glBlendEquationSeparate(a, b); }
void glBlendEquationiEXT(GLuint buf, GLenum m) { glBlendEquationi(buf, m); }
void glBlendEquationSeparateiEXT(GLuint buf, GLenum a, GLenum b) { glBlendEquationSeparatei(buf, a, b); }
void glBlendColor(GLfloat r, GLfloat g, GLfloat b, GLfloat a) { (void)r; (void)g; (void)b; (void)a; }
void glFrontFace(GLenum m) { (void)m; }
void glCullFace(GLenum m) { (void)m; }
void glColorMask(GLboolean r, GLboolean g, GLboolean b, GLboolean a) { (void)r; (void)g; (void)b; (void)a; }
void glScissor(GLint x, GLint y, GLsizei w, GLsizei h) {
    g_scissor[0] = x; g_scissor[1] = y; g_scissor[2] = w; g_scissor[3] = h;
}
void glLineWidth(GLfloat w) { (void)w; }
void glHint(GLenum t, GLenum m) { (void)t; (void)m; }
void glPolygonOffset(GLfloat a, GLfloat b) { (void)a; (void)b; }
void glSampleCoverage(GLfloat v, GLboolean i) { (void)v; (void)i; }
GLboolean glIsEnabled(GLenum c) {
    if (c == GL_DEPTH_TEST) return (GLboolean)g_depth;
    if (c == GL_BLEND) return (GLboolean)g_blend;
    if (c == GL_SCISSOR_TEST) return (GLboolean)g_scissor_enabled;
    return 0;
}
GLboolean glIsTexture(GLuint t) { return (t && t < MAXTEX && g_tex[t].used) ? 1 : 0; }
GLboolean glIsBuffer(GLuint b) { return (b && b < MAXBUF && g_buf[b].used) ? 1 : 0; }
GLboolean glIsProgram(GLuint p) { return (p && p < MAXPROG && g_prog[p].used) ? 1 : 0; }
GLboolean glIsShader(GLuint s) { return (s && s < MAXSH && g_sh[s].used) ? 1 : 0; }
void glUniformMatrix3fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 3, 3); }
void glUniformMatrix2fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 2, 2); }
void glUniform2fv(GLint l, GLsizei n, const GLfloat *v) { (void)n; uni_write(l, v, 8); }
void glUniform2f(GLint l, GLfloat a, GLfloat b) { GLfloat v[2] = {a, b}; uni_write(l, v, 8); }
void glUniform1fv(GLint l, GLsizei n, const GLfloat *v) { (void)n; uni_write(l, v, 4); }
void glUniform2i(GLint l, GLint a, GLint b) { GLint v[2] = {a, b}; uni_write(l, v, 8); }
void glUniform3i(GLint l, GLint a, GLint b, GLint cc) { GLint v[3] = {a, b, cc}; uni_write(l, v, 12); }
void glUniform1iv(GLint l, GLsizei n, const GLint *v) {
    if (l >= 100000 && l < 100004 && v) {
        for (GLsizei k = 0; k < n && l + k < 100004; k++) glUniform1i(l + k, v[k]);
        return;
    }
    (void)n;
    uni_write(l, v, 4);
}
void glVertexAttrib1f(GLuint i, GLfloat x) { (void)i; (void)x; }
void glVertexAttrib2f(GLuint i, GLfloat x, GLfloat y) { (void)i; (void)x; (void)y; }
void glVertexAttrib3f(GLuint i, GLfloat x, GLfloat y, GLfloat z) { (void)i; (void)x; (void)y; (void)z; }
void glVertexAttrib4f(GLuint i, GLfloat x, GLfloat y, GLfloat z, GLfloat w) { (void)i; (void)x; (void)y; (void)z; (void)w; }
void glVertexAttrib4fv(GLuint i, const GLfloat *v) { (void)i; (void)v; }
void glDeleteShader(GLuint s) { if (s && s < MAXSH) { free(g_sh[s].src); g_sh[s].src = 0; g_sh[s].used = 0; } }
void glDeleteProgram(GLuint p) { if (p && p < MAXPROG) { free(g_prog[p].msl); g_prog[p].msl = 0; g_prog[p].used = 0; } }
void glDeleteBuffers(GLsizei n, const GLuint *b) { for (int i = 0; i < n; i++) { GLuint x = b[i]; if (x && x < MAXBUF) { free(g_buf[x].data); g_buf[x].data = 0; g_buf[x].used = 0; g_buf[x].gen++; } } }
void glDetachShader(GLuint p, GLuint s) { (void)p; (void)s; }
void glBindAttribLocation(GLuint p, GLuint idx, const GLchar *nm) { (void)p; (void)idx; (void)nm; }
void glGetActiveAttrib(GLuint p, GLuint i, GLsizei bs, GLsizei *len, GLint *sz, GLenum *ty, GLchar *nm) { (void)p; (void)i; (void)bs; if (len) *len = 0; if (sz) *sz = 1; if (ty) *ty = GL_FLOAT; if (nm && bs) nm[0] = 0; }
void glGetActiveUniform(GLuint p, GLuint i, GLsizei bs, GLsizei *len, GLint *sz, GLenum *ty, GLchar *nm) { (void)p; (void)i; (void)bs; if (len) *len = 0; if (sz) *sz = 1; if (ty) *ty = GL_FLOAT; if (nm && bs) nm[0] = 0; }
// ---- framebuffer/renderbuffer objects --------------------------------------------------------------
void glGenFramebuffers(GLsizei n, GLuint *f) {
    for (int i = 0; i < n; i++) {
        f[i] = 0;
        for (int id = 1; id < MAXFBO; id++)
            if (!g_fbo[id].used) {
                memset(&g_fbo[id], 0, sizeof g_fbo[id]);
                g_fbo[id].used = 1;
                f[i] = (GLuint)id;
                break;
            }
    }
}
void glBindFramebuffer(GLenum t, GLuint f) {
    if (f < MAXFBO && f != 0) g_fbo[f].used = 1;
    if (t == GL_FRAMEBUFFER) {
        g_draw_fbo = f;
        g_read_fbo = f;
    } else if (t == GL_DRAW_FRAMEBUFFER) {
        g_draw_fbo = f;
    } else if (t == GL_READ_FRAMEBUFFER) {
        g_read_fbo = f;
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBindFramebuffer target=0x%x fbo=%u\n", t, f);
}
void glDeleteFramebuffers(GLsizei n, const GLuint *f) {
    for (int i = 0; i < n; i++) {
        GLuint id = f ? f[i] : 0;
        if (id < MAXFBO) {
            memset(&g_fbo[id], 0, sizeof g_fbo[id]);
            if (g_draw_fbo == id) g_draw_fbo = 0;
            if (g_read_fbo == id) g_read_fbo = 0;
        }
    }
}
GLenum glCheckFramebufferStatus(GLenum t) { (void)t; return 0x8CD5; /* GL_FRAMEBUFFER_COMPLETE */ }
void glFramebufferTexture2D(GLenum a, GLenum b, GLenum c, GLuint d, GLint e) {
    GLuint f = (a == GL_READ_FRAMEBUFFER) ? g_read_fbo : g_draw_fbo;
    if ((a == GL_FRAMEBUFFER || a == GL_DRAW_FRAMEBUFFER || a == GL_READ_FRAMEBUFFER) &&
        b == GL_COLOR_ATTACHMENT0 && c == GL_TEXTURE_2D && e == 0 && f > 0 && f < MAXFBO) {
        g_fbo[f].used = 1;
        g_fbo[f].color_tex = d;
        g_fbo[f].color_rbo = 0;
        g_fbo[f].color_level = e;
        g_fbo[f].color_layer = 0;
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glFramebufferTexture2D target=0x%x attachment=0x%x textarget=0x%x tex=%u level=%d\n", a, b, c, d, e);
}
void glFramebufferRenderbuffer(GLenum a, GLenum b, GLenum c, GLuint d) {
    GLuint f = (a == GL_READ_FRAMEBUFFER) ? g_read_fbo : g_draw_fbo;
    if ((a == GL_FRAMEBUFFER || a == GL_DRAW_FRAMEBUFFER || a == GL_READ_FRAMEBUFFER) &&
        b == GL_COLOR_ATTACHMENT0 && c == GL_RENDERBUFFER && f > 0 && f < MAXFBO) {
        g_fbo[f].used = 1;
        g_fbo[f].color_tex = 0;
        g_fbo[f].color_rbo = d;
        g_fbo[f].color_level = 0;
        g_fbo[f].color_layer = 0;
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glFramebufferRenderbuffer target=0x%x attachment=0x%x rbtarget=0x%x rbo=%u\n", a, b, c, d);
}
void glGenRenderbuffers(GLsizei n, GLuint *r) {
    for (int k = 0; k < n; k++) {
        r[k] = 0;
        for (int i = 1; i < MAXRBO; i++)
            if (!g_rbo[i].used) {
                memset(&g_rbo[i], 0, sizeof g_rbo[i]);
                g_rbo[i].used = 1;
                g_rbo[i].gen++;
                r[k] = (GLuint)i;
                break;
            }
    }
}
void glBindRenderbuffer(GLenum t, GLuint r) {
    if (t == GL_RENDERBUFFER) {
        g_rbo_bound = r;
        if (r > 0 && r < MAXRBO) g_rbo[r].used = 1;
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBindRenderbuffer target=0x%x rbo=%u\n", t, r);
}
void glDeleteRenderbuffers(GLsizei n, const GLuint *r) {
    for (int i = 0; i < n; i++) {
        GLuint id = r ? r[i] : 0;
        if (id < MAXRBO) {
            memset(&g_rbo[id], 0, sizeof g_rbo[id]);
            if (g_rbo_bound == id) g_rbo_bound = 0;
            for (int f = 1; f < MAXFBO; f++) {
                if (g_fbo[f].color_rbo == id) g_fbo[f].color_rbo = 0;
            }
        }
    }
}
void glRenderbufferStorage(GLenum a, GLenum b, GLsizei w, GLsizei h) {
    if (a == GL_RENDERBUFFER && g_rbo_bound > 0 && g_rbo_bound < MAXRBO) {
        g_rbo[g_rbo_bound].used = 1;
        g_rbo[g_rbo_bound].w = w;
        g_rbo[g_rbo_bound].h = h;
        g_rbo[g_rbo_bound].ifmt = b;
        g_rbo[g_rbo_bound].samples = 0;
        g_rbo[g_rbo_bound].gen++;
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glRenderbufferStorage target=0x%x ifmt=0x%x %dx%d\n", a, b, w, h);
}
void glReadPixels(GLint x, GLint y, GLsizei w, GLsizei h, GLenum f, GLenum t, void *d) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glReadPixels xy=%d,%d %dx%d fmt=0x%x type=0x%x dst=%p\n", x, y, w, h, f, t, d);
    if (!d || w <= 0 || h <= 0) return;
    int bpp = tex_bpp(f);
    memset(d, 0, (size_t)w * (size_t)h * (size_t)bpp);
    if (t != GL_UNSIGNED_BYTE) return;
    GLuint src_id = fbo_color_texture(g_read_fbo);
    if (!src_id) return;
    struct tex *src = &g_tex[src_id];
    uint8_t *out = d;
    for (int yy = 0; yy < h; yy++) {
        int sy = y + yy;
        for (int xx = 0; xx < w; xx++) {
            int sx = x + xx;
            if (sx < 0 || sx >= src->w || sy < 0 || sy >= src->h) continue;
            const uint8_t *sp = src->data + ((size_t)sy * (size_t)src->w + (size_t)sx) * 4u;
            uint8_t *dp = out + ((size_t)yy * (size_t)w + (size_t)xx) * (size_t)bpp;
            if (f == GL_RGBA) { dp[0] = sp[0]; dp[1] = sp[1]; dp[2] = sp[2]; dp[3] = sp[3]; }
            else if (f == GL_BGRA_EXT) { dp[0] = sp[2]; dp[1] = sp[1]; dp[2] = sp[0]; dp[3] = sp[3]; }
            else if (f == GL_RGB) { dp[0] = sp[0]; dp[1] = sp[1]; dp[2] = sp[2]; }
            else { dp[0] = sp[0]; }
        }
    }
}
const unsigned char *glGetStringi(GLenum n, GLuint i) {
    if (n == GL_EXTENSIONS && i < (GLuint)g_gl_nexts) return (const unsigned char *)g_gl_exts[i];
    return (const unsigned char *)"";
}

// ---- GLES2 core completeness for ANGLE (gl-egl) ------------------------------------------------------
// chromium's ANGLE resolves the ENTIRE GLES2 core entry-point set via eglGetProcAddress and stores it in
// FunctionsGL; any NULL becomes a null call during renderer caps init (glGetShaderPrecisionFormat is the
// first) → SIGSEGV. glmark2 only touched a subset, so these were absent. Implement the queries with sane
// values and route the setters/no-ops so ANGLE's ES2 bring-up completes. (ANGLE sees GL_VERSION="OpenGL
// ES 2.0" so it uses the ES2 path only.)
void glStencilFuncSeparate(GLenum face, GLenum func, GLint ref, GLuint mask) { (void)face; (void)func; (void)ref; (void)mask; }
void glStencilMaskSeparate(GLenum face, GLuint mask) { (void)face; (void)mask; }
void glStencilOpSeparate(GLenum face, GLenum s, GLenum dpf, GLenum dpp) { (void)face; (void)s; (void)dpf; (void)dpp; }
void glReleaseShaderCompiler(void) { }
void glShaderBinary(GLsizei n, const GLuint *sh, GLenum fmt, const void *bin, GLsizei len) { (void)n; (void)sh; (void)fmt; (void)bin; (void)len; }
void glValidateProgram(GLuint p) { (void)p; }
void glCompressedTexImage2D(GLenum t, GLint l, GLenum ifmt, GLsizei w, GLsizei h, GLint b, GLsizei sz, const void *d) { (void)t; (void)l; (void)ifmt; (void)w; (void)h; (void)b; (void)sz; (void)d; }
void glCompressedTexSubImage2D(GLenum t, GLint l, GLint xo, GLint yo, GLsizei w, GLsizei h, GLenum fmt, GLsizei sz, const void *d) { (void)t; (void)l; (void)xo; (void)yo; (void)w; (void)h; (void)fmt; (void)sz; (void)d; }
void glCopyTexImage2D(GLenum t, GLint l, GLenum ifmt, GLint x, GLint y, GLsizei w, GLsizei h, GLint b) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glCopyTexImage2D target=0x%x level=%d ifmt=0x%x src=%d,%d %dx%d border=%d dsttex=%u\n", t, l, ifmt, x, y, w, h, b, g_tex_unit[g_active_unit]);
    (void)ifmt;
    if (t != GL_TEXTURE_2D || l != 0 || b != 0) return;
    GLuint dst = g_tex_unit[g_active_unit];
    GLuint src = fbo_color_texture(g_read_fbo);
    if (!src || !tex_alloc_rgba(dst, w, h)) return;
    copy_texture_rect(src, dst, x, y, x + w, y + h, 0, 0, w, h);
    dump_tex_ppm(dst, &g_tex[dst], "copy-image");
}
void glCopyTexSubImage2D(GLenum t, GLint l, GLint xo, GLint yo, GLint x, GLint y, GLsizei w, GLsizei h) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glCopyTexSubImage2D target=0x%x level=%d dst=%d,%d src=%d,%d %dx%d dsttex=%u\n", t, l, xo, yo, x, y, w, h, g_tex_unit[g_active_unit]);
    if (t != GL_TEXTURE_2D || l != 0 || w <= 0 || h <= 0) return;
    GLuint dst = g_tex_unit[g_active_unit];
    GLuint src = fbo_color_texture(g_read_fbo);
    if (!src || dst >= MAXTEX || !g_tex[dst].used || !g_tex[dst].data) return;
    copy_texture_rect(src, dst, x, y, x + w, y + h, xo, yo, xo + w, yo + h);
    dump_tex_ppm(dst, &g_tex[dst], "copy-sub");
}
void glTexParameterfv(GLenum target, GLenum p, const GLfloat *v) { if (v) glTexParameteri(target, p, (GLint)v[0]); }
void glTexParameteriv(GLenum target, GLenum p, const GLint *v) { if (v) glTexParameteri(target, p, v[0]); }
void glTexSubImage2D(GLenum target, GLint level, GLint xo, GLint yo, GLsizei w, GLsizei h, GLenum fmt, GLenum type, const void *pixels) {
    (void)target; (void)type;
    GLuint t = g_tex_unit[g_active_unit];
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glTexSubImage2D unit=%d tex=%u level=%d xy=%d,%d %dx%d fmt=0x%x type=0x%x pixels=%p\n", g_active_unit, t, level, xo, yo, w, h, fmt, type, pixels);
    if (level != 0 || t >= MAXTEX || !g_tex[t].used || !g_tex[t].data) return;
    tex_store_pixels(&g_tex[t], xo, yo, w, h, fmt, pixels);
    g_tex[t].gen++;
    dump_tex_ppm(t, &g_tex[t], "sub");
}
void glUniform2iv(GLint l, GLsizei n, const GLint *v) { (void)n; uni_write(l, v, 8); }
void glUniform3iv(GLint l, GLsizei n, const GLint *v) { (void)n; uni_write(l, v, 12); }
void glUniform4i(GLint l, GLint a, GLint b, GLint c, GLint d) { GLint v[4] = {a, b, c, d}; uni_write(l, v, 16); }
void glUniform4iv(GLint l, GLsizei n, const GLint *v) { (void)n; uni_write(l, v, 16); }
void glVertexAttrib1fv(GLuint i, const GLfloat *v) { (void)i; (void)v; }
void glVertexAttrib2fv(GLuint i, const GLfloat *v) { (void)i; (void)v; }
void glVertexAttrib3fv(GLuint i, const GLfloat *v) { (void)i; (void)v; }
void glGetShaderPrecisionFormat(GLenum shadertype, GLenum ptype, GLint *range, GLint *precision) {
    (void)shadertype; (void)ptype;
    if (range) { range[0] = 127; range[1] = 127; }
    if (precision) precision[0] = 23; // IEEE float mantissa; enough for ANGLE highp caps
}
#define GL_BUFFER_SIZE 0x8764
#define GL_BUFFER_USAGE 0x8765
#define GL_BUFFER_MAPPED 0x88BC
#define GL_BUFFER_ACCESS_FLAGS 0x911F
static GLuint bound_buffer_for_target(GLenum target) {
    if (target == GL_ELEMENT_ARRAY_BUFFER) return g_elem_buf;
    return g_arr_buf;
}
static GLuint bound_fbo_for_target(GLenum target) {
    if (target == GL_READ_FRAMEBUFFER) return g_read_fbo;
    return g_draw_fbo;
}
static void rbo_component_bits(const struct rbo *r, GLint *red, GLint *green, GLint *blue, GLint *alpha, GLint *depth, GLint *stencil) {
    GLint cr = 8, cg = 8, cb = 8, ca = 8, cd = 0, cs = 0;
    if (r) {
        if (r->ifmt == 0x81A5 || r->ifmt == 0x81A6 || r->ifmt == 0x81A7) { cr = cg = cb = ca = 0; cd = 24; }
        else if (r->ifmt == 0x8D48) { cr = cg = cb = ca = 0; cs = 8; }
        else if (r->ifmt == 0x88F0) { cr = cg = cb = ca = 0; cd = 24; cs = 8; }
    }
    if (red) *red = cr;
    if (green) *green = cg;
    if (blue) *blue = cb;
    if (alpha) *alpha = ca;
    if (depth) *depth = cd;
    if (stencil) *stencil = cs;
}
void glGetBufferParameteriv(GLenum target, GLenum p, GLint *v) {
    if (!v) return;
    *v = 0;
    GLuint b = bound_buffer_for_target(target);
    if (b >= MAXBUF || !g_buf[b].used) return;
    if (p == GL_BUFFER_SIZE) *v = (GLint)g_buf[b].size;
    else if (p == GL_BUFFER_USAGE) *v = (GLint)g_buf[b].usage;
    else if (p == GL_BUFFER_MAPPED || p == GL_BUFFER_ACCESS_FLAGS) *v = 0;
}
void glGetFramebufferAttachmentParameteriv(GLenum target, GLenum att, GLenum p, GLint *v) {
    if (!v) return;
    *v = 0;
    GLuint f = bound_fbo_for_target(target);
    if (f == 0) {
        if (p == GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE) *v = GL_FRAMEBUFFER_DEFAULT;
        else if (p == GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME) *v = 0;
        else if (p >= GL_FRAMEBUFFER_ATTACHMENT_RED_SIZE && p <= GL_FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE) *v = (att == GL_COLOR_ATTACHMENT0) ? 8 : 0;
        else if (p == GL_FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE) *v = (att == GL_DEPTH_ATTACHMENT) ? 24 : 0;
        else if (p == GL_FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE) *v = (att == GL_STENCIL_ATTACHMENT) ? 8 : 0;
        return;
    }
    if (f >= MAXFBO || !g_fbo[f].used) return;
    if (att != GL_COLOR_ATTACHMENT0) return;
    struct fbo *fb = &g_fbo[f];
    if (p == GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE) {
        if (fb->color_tex) *v = GL_TEXTURE;
        else if (fb->color_rbo) *v = GL_RENDERBUFFER;
        else *v = 0;
    } else if (p == GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME) {
        *v = (GLint)(fb->color_tex ? fb->color_tex : fb->color_rbo);
    } else if (p == GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL) {
        *v = fb->color_tex ? fb->color_level : 0;
    } else if (p == GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_CUBE_MAP_FACE) {
        *v = 0;
    } else if (p == GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER) {
        *v = fb->color_tex ? fb->color_layer : 0;
    } else if (p >= GL_FRAMEBUFFER_ATTACHMENT_RED_SIZE && p <= GL_FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE) {
        GLint red = 8, green = 8, blue = 8, alpha = 8, depth = 0, stencil = 0;
        if (fb->color_rbo && fb->color_rbo < MAXRBO && g_rbo[fb->color_rbo].used)
            rbo_component_bits(&g_rbo[fb->color_rbo], &red, &green, &blue, &alpha, &depth, &stencil);
        if (p == GL_FRAMEBUFFER_ATTACHMENT_RED_SIZE) *v = red;
        else if (p == GL_FRAMEBUFFER_ATTACHMENT_GREEN_SIZE) *v = green;
        else if (p == GL_FRAMEBUFFER_ATTACHMENT_BLUE_SIZE) *v = blue;
        else if (p == GL_FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE) *v = alpha;
        else if (p == GL_FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE) *v = depth;
        else if (p == GL_FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE) *v = stencil;
    }
}
void glGetRenderbufferParameteriv(GLenum target, GLenum p, GLint *v) {
    if (!v) return;
    *v = 0;
    if (target != GL_RENDERBUFFER || g_rbo_bound >= MAXRBO || !g_rbo[g_rbo_bound].used) return;
    struct rbo *r = &g_rbo[g_rbo_bound];
    GLint red = 8, green = 8, blue = 8, alpha = 8, depth = 0, stencil = 0;
    rbo_component_bits(r, &red, &green, &blue, &alpha, &depth, &stencil);
    if (p == GL_RENDERBUFFER_WIDTH) *v = r->w;
    else if (p == GL_RENDERBUFFER_HEIGHT) *v = r->h;
    else if (p == GL_RENDERBUFFER_INTERNAL_FORMAT) *v = (GLint)r->ifmt;
    else if (p == GL_RENDERBUFFER_RED_SIZE) *v = red;
    else if (p == GL_RENDERBUFFER_GREEN_SIZE) *v = green;
    else if (p == GL_RENDERBUFFER_BLUE_SIZE) *v = blue;
    else if (p == GL_RENDERBUFFER_ALPHA_SIZE) *v = alpha;
    else if (p == GL_RENDERBUFFER_DEPTH_SIZE) *v = depth;
    else if (p == GL_RENDERBUFFER_STENCIL_SIZE) *v = stencil;
    else if (p == GL_RENDERBUFFER_SAMPLES) *v = r->samples;
}
void glGetTexParameteriv(GLenum target, GLenum p, GLint *v) {
    if (!v) return;
    *v = 0;
    if (target != GL_TEXTURE_2D) return;
    GLuint t = g_tex_unit[g_active_unit];
    if (t >= MAXTEX || !g_tex[t].used) return;
    if (p == GL_TEXTURE_MIN_FILTER) *v = g_tex[t].minf;
    else if (p == GL_TEXTURE_MAG_FILTER) *v = g_tex[t].magf;
    else if (p == GL_TEXTURE_WRAP_S) *v = g_tex[t].ws;
    else if (p == GL_TEXTURE_WRAP_T) *v = g_tex[t].wt;
    else if (p == GL_TEXTURE_BASE_LEVEL || p == GL_TEXTURE_MAX_LEVEL || p == GL_TEXTURE_IMMUTABLE_FORMAT) *v = 0;
}
void glGetTexParameterfv(GLenum target, GLenum p, GLfloat *v) {
    if (!v) return;
    GLint iv = 0;
    glGetTexParameteriv(target, p, &iv);
    *v = (GLfloat)iv;
}
void glGetUniformfv(GLuint prog, GLint l, GLfloat *v) { (void)prog; (void)l; if (v) *v = 0; }
void glGetUniformiv(GLuint prog, GLint l, GLint *v) { (void)prog; (void)l; if (v) *v = 0; }
void glGetVertexAttribfv(GLuint idx, GLenum p, GLfloat *v) { (void)idx; (void)p; if (v) *v = 0; }
void glGetVertexAttribiv(GLuint idx, GLenum p, GLint *v) { (void)idx; (void)p; if (v) *v = 0; }
void glGetVertexAttribPointerv(GLuint idx, GLenum p, void **ptr) { (void)idx; (void)p; if (ptr) *ptr = 0; }
void glGetAttachedShaders(GLuint prog, GLsizei maxc, GLsizei *count, GLuint *shaders) { (void)prog; (void)maxc; (void)shaders; if (count) *count = 0; }
void glGetShaderSource(GLuint s, GLsizei bufSize, GLsizei *length, GLchar *source) { (void)s; if (length) *length = 0; if (source && bufSize) source[0] = 0; }
GLboolean glIsFramebuffer(GLuint f) { return (f > 0 && f < MAXFBO && g_fbo[f].used) ? 1 : 0; }
GLboolean glIsRenderbuffer(GLuint r) { return (r > 0 && r < MAXRBO && g_rbo[r].used) ? 1 : 0; }

// ---- GLES3 core completeness for ANGLE (gl-egl, ES3.0 context) ---------------------------------------
// With GL_VERSION="OpenGL ES 3.0", ANGLE resolves the ENTIRE ES3 core set and NULL-derefs on any missing
// pointer during ES3 caps init. Provide them all: queries return benign values; VAOs/samplers/queries/
// sync/transform-feedback are id-vending stubs; instanced/range draws route to the base draw; ES3 uniform
// variants route through uni_write; and glMapBufferRange/glUnmapBuffer are FUNCTIONAL (return a pointer into
// the bound buffer's storage) so Chrome's vertex/index uploads land in g_buf and reach the IR/Metal path.
typedef int64_t GLint64; typedef uint64_t GLuint64; typedef void *GLsync;
static GLuint g_vao_seq = 1, g_samp_seq = 1, g_query_seq = 1, g_xfb_seq = 1;
void glGenVertexArrays(GLsizei n, GLuint *a) {
    for (int i = 0; i < n; i++) {
        a[i] = 0;
        for (GLuint id = g_vao_seq; id < MAXVAO; id++) {
            if (!g_vao[id].used) {
                g_vao[id].used = 1;
                memset(g_vao[id].attrs, 0, sizeof g_vao[id].attrs);
                g_vao[id].elem_buf = 0;
                a[i] = id;
                g_vao_seq = id + 1;
                break;
            }
        }
    }
}
void glBindVertexArray(GLuint a) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBindVertexArray(%u)\n", a);
    vao_store_current();
    if (a >= MAXVAO) return;
    g_cur_vao = a;
    vao_load(a);
}
void glDeleteVertexArrays(GLsizei n, const GLuint *a) {
    for (int i = 0; i < n; i++) {
        GLuint id = a ? a[i] : 0;
        if (id > 0 && id < MAXVAO) {
            memset(&g_vao[id], 0, sizeof g_vao[id]);
            if (g_cur_vao == id) glBindVertexArray(0);
        }
    }
}
GLboolean glIsVertexArray(GLuint a) { return (a < MAXVAO && g_vao[a].used) ? 1 : 0; }
// Samplers (stub).
void glGenSamplers(GLsizei n, GLuint *s) { for (int i = 0; i < n; i++) s[i] = g_samp_seq++; }
void glBindSampler(GLuint unit, GLuint s) { (void)unit; (void)s; }
void glDeleteSamplers(GLsizei n, const GLuint *s) { (void)n; (void)s; }
GLboolean glIsSampler(GLuint s) { return s ? 1 : 0; }
void glSamplerParameteri(GLuint s, GLenum p, GLint v) { (void)s; (void)p; (void)v; }
void glSamplerParameterf(GLuint s, GLenum p, GLfloat v) { (void)s; (void)p; (void)v; }
void glSamplerParameteriv(GLuint s, GLenum p, const GLint *v) { (void)s; (void)p; (void)v; }
void glSamplerParameterfv(GLuint s, GLenum p, const GLfloat *v) { (void)s; (void)p; (void)v; }
void glGetSamplerParameteriv(GLuint s, GLenum p, GLint *v) { (void)s; (void)p; if (v) *v = 0; }
void glGetSamplerParameterfv(GLuint s, GLenum p, GLfloat *v) { (void)s; (void)p; if (v) *v = 0; }
// Query objects (stub: report "not available"/0 results so ANGLE's timer/occlusion probes are inert).
void glGenQueries(GLsizei n, GLuint *q) { for (int i = 0; i < n; i++) q[i] = g_query_seq++; }
void glDeleteQueries(GLsizei n, const GLuint *q) { (void)n; (void)q; }
GLboolean glIsQuery(GLuint q) { return q ? 1 : 0; }
void glBeginQuery(GLenum t, GLuint q) { (void)t; (void)q; }
void glEndQuery(GLenum t) { (void)t; }
void glGetQueryiv(GLenum t, GLenum p, GLint *v) { (void)t; (void)p; if (v) *v = 0; }
void glGetQueryObjectuiv(GLuint q, GLenum p, GLuint *v) { (void)q; (void)p; if (v) *v = 0; }
// Transform feedback (stub).
void glGenTransformFeedbacks(GLsizei n, GLuint *t) { for (int i = 0; i < n; i++) t[i] = g_xfb_seq++; }
void glDeleteTransformFeedbacks(GLsizei n, const GLuint *t) { (void)n; (void)t; }
void glBindTransformFeedback(GLenum tgt, GLuint t) { (void)tgt; (void)t; }
GLboolean glIsTransformFeedback(GLuint t) { return t ? 1 : 0; }
void glBeginTransformFeedback(GLenum m) { (void)m; }
void glEndTransformFeedback(void) { }
void glPauseTransformFeedback(void) { }
void glResumeTransformFeedback(void) { }
void glTransformFeedbackVaryings(GLuint p, GLsizei n, const GLchar *const *v, GLenum mode) { (void)p; (void)n; (void)v; (void)mode; }
void glGetTransformFeedbackVarying(GLuint p, GLuint i, GLsizei bs, GLsizei *len, GLsizei *sz, GLenum *ty, GLchar *nm) { (void)p; (void)i; (void)bs; if (len) *len = 0; if (sz) *sz = 0; if (ty) *ty = 0; if (nm && bs) nm[0] = 0; }
// Sync objects (stub: report immediately signaled so Chrome's fences never block).
GLsync glFenceSync(GLenum cond, GLbitfield f) { (void)cond; (void)f; return (GLsync)1; }
void glDeleteSync(GLsync s) { (void)s; }
GLboolean glIsSync(GLsync s) { return s ? 1 : 0; }
GLenum glClientWaitSync(GLsync s, GLbitfield f, GLuint64 to) { (void)s; (void)f; (void)to; return 0x911A; /* GL_ALREADY_SIGNALED */ }
void glWaitSync(GLsync s, GLbitfield f, GLuint64 to) { (void)s; (void)f; (void)to; }
void glGetSynciv(GLsync s, GLenum p, GLsizei bs, GLsizei *len, GLint *v) { (void)s; (void)bs; if (len) *len = 1; if (v) { *v = (p == 0x9114 /*GL_SYNC_STATUS*/) ? 0x9119 /*GL_SIGNALED*/ : 0; } }
// Buffer mapping — FUNCTIONAL: hand back a pointer into the bound buffer's storage.
void *glMapBufferRange(GLenum t, GLintptr off, GLsizeiptr len, GLbitfield access) {
    (void)access;
    if (off < 0 || len < 0) return NULL; // signed OOB → negative pointer/size
    GLuint b = (t == GL_ELEMENT_ARRAY_BUFFER) ? g_elem_buf : g_arr_buf;
    if (b >= MAXBUF || !g_buf[b].used) return NULL;
    if (!g_buf[b].data || g_buf[b].size < (size_t)(off + len)) {
        uint8_t *nd = realloc(g_buf[b].data, (size_t)(off + len));
        if (!nd) return NULL;
        g_buf[b].data = nd;
        if (g_buf[b].size < (size_t)(off + len)) g_buf[b].size = (size_t)(off + len);
    }
    return g_buf[b].data + off;
}
GLboolean glUnmapBuffer(GLenum t) { GLuint b = (t == GL_ELEMENT_ARRAY_BUFFER) ? g_elem_buf : g_arr_buf; if (b < MAXBUF && g_buf[b].used) g_buf[b].gen++; return 1; }
void glFlushMappedBufferRange(GLenum t, GLintptr off, GLsizeiptr len) { (void)t; (void)off; (void)len; }
void glGetBufferPointerv(GLenum t, GLenum p, void **v) { (void)t; (void)p; if (v) *v = 0; }
void glCopyBufferSubData(GLenum r, GLenum w, GLintptr ro, GLintptr wo, GLsizeiptr sz) {
    GLuint rb = (r == GL_ELEMENT_ARRAY_BUFFER) ? g_elem_buf : g_arr_buf;
    GLuint wb = (w == GL_ELEMENT_ARRAY_BUFFER) ? g_elem_buf : g_arr_buf;
    if (rb < MAXBUF && wb < MAXBUF && g_buf[rb].data && g_buf[wb].data &&
        (size_t)(ro + sz) <= g_buf[rb].size && (size_t)(wo + sz) <= g_buf[wb].size) {
        memmove(g_buf[wb].data + wo, g_buf[rb].data + ro, (size_t)sz); g_buf[wb].gen++;
    }
}
void glBindBufferBase(GLenum t, GLuint idx, GLuint b) { (void)t; (void)idx; (void)b; }
void glBindBufferRange(GLenum t, GLuint idx, GLuint b, GLintptr off, GLsizeiptr sz) { (void)t; (void)idx; (void)b; (void)off; (void)sz; }
// Instanced / range draws → base draw (single instance is enough for a first frame).
void glDrawArraysInstanced(GLenum m, GLint first, GLsizei count, GLsizei inst) { (void)inst; glDrawArrays(m, first, count); }
void glDrawElementsInstanced(GLenum m, GLsizei count, GLenum type, const void *idx, GLsizei inst) { (void)inst; glDrawElements(m, count, type, idx); }
void glDrawRangeElements(GLenum m, GLuint s, GLuint e, GLsizei count, GLenum type, const void *idx) { (void)s; (void)e; glDrawElements(m, count, type, idx); }
void glDrawBuffers(GLsizei n, const GLenum *b) { (void)n; (void)b; }
void glReadBuffer(GLenum m) { (void)m; }
void glVertexAttribDivisor(GLuint i, GLuint d) { (void)i; (void)d; }
void glVertexAttribIPointer(GLuint i, GLint size, GLenum type, GLsizei stride, const void *ptr) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glVertexAttribIPointer i=%u size=%d type=0x%x stride=%d off=%zu arrbuf=%d\n", i, size, type, stride, (size_t)ptr, g_arr_buf);
    if (i >= MAXATTR) return;
    g_attr[i].size = size;
    g_attr[i].type = type;
    g_attr[i].normalized = 0;
    g_attr[i].integer = 1;
    g_attr[i].stride = stride;
    g_attr[i].offset = (size_t)ptr;
    g_attr[i].buffer = g_arr_buf;
    vao_store_current();
}
void glVertexAttribI4i(GLuint i, GLint x, GLint y, GLint z, GLint w) { (void)i; (void)x; (void)y; (void)z; (void)w; }
void glVertexAttribI4ui(GLuint i, GLuint x, GLuint y, GLuint z, GLuint w) { (void)i; (void)x; (void)y; (void)z; (void)w; }
void glVertexAttribI4iv(GLuint i, const GLint *v) { (void)i; (void)v; }
void glVertexAttribI4uiv(GLuint i, const GLuint *v) { (void)i; (void)v; }
void glGetVertexAttribIiv(GLuint i, GLenum p, GLint *v) { (void)i; (void)p; if (v) *v = 0; }
void glGetVertexAttribIuiv(GLuint i, GLenum p, GLuint *v) { (void)i; (void)p; if (v) *v = 0; }
// ES3 unsigned-int + non-square-matrix uniforms → same uniform store.
void glUniform1ui(GLint l, GLuint a) { uni_write(l, &a, 4); }
void glUniform2ui(GLint l, GLuint a, GLuint b) { GLuint v[2] = {a, b}; uni_write(l, v, 8); }
void glUniform3ui(GLint l, GLuint a, GLuint b, GLuint c) { GLuint v[3] = {a, b, c}; uni_write(l, v, 12); }
void glUniform4ui(GLint l, GLuint a, GLuint b, GLuint c, GLuint d) { GLuint v[4] = {a, b, c, d}; uni_write(l, v, 16); }
void glUniform1uiv(GLint l, GLsizei n, const GLuint *v) { (void)n; uni_write(l, v, 4); }
void glUniform2uiv(GLint l, GLsizei n, const GLuint *v) { (void)n; uni_write(l, v, 8); }
void glUniform3uiv(GLint l, GLsizei n, const GLuint *v) { (void)n; uni_write(l, v, 12); }
void glUniform4uiv(GLint l, GLsizei n, const GLuint *v) { (void)n; uni_write(l, v, 16); }
// GL glUniformMatrixCxRfv names are C columns × R rows (C first). Route through uni_write_matrix so 3-row
// variants (2x3, 4x3) get their columns re-strided to MSL's 16-byte float3 columns.
void glUniformMatrix2x3fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 2, 3); }
void glUniformMatrix3x2fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 3, 2); }
void glUniformMatrix2x4fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 2, 4); }
void glUniformMatrix4x2fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 4, 2); }
void glUniformMatrix3x4fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 3, 4); }
void glUniformMatrix4x3fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write_matrix(l, v, 4, 3); }
void glGetUniformuiv(GLuint p, GLint l, GLuint *v) { (void)p; (void)l; if (v) *v = 0; }
GLint glGetFragDataLocation(GLuint p, const GLchar *nm) { (void)p; (void)nm; return 0; }
// Uniform blocks (stub: caps report blocks but a first frame uses default-block uniforms via uni_write).
GLuint glGetUniformBlockIndex(GLuint p, const GLchar *nm) { (void)p; (void)nm; return 0xFFFFFFFFu; /* GL_INVALID_INDEX */ }
void glGetActiveUniformBlockiv(GLuint p, GLuint idx, GLenum pn, GLint *v) { (void)p; (void)idx; (void)pn; if (v) *v = 0; }
void glGetActiveUniformBlockName(GLuint p, GLuint idx, GLsizei bs, GLsizei *len, GLchar *nm) { (void)p; (void)idx; if (len) *len = 0; if (nm && bs) nm[0] = 0; }
void glGetActiveUniformsiv(GLuint p, GLsizei n, const GLuint *idx, GLenum pn, GLint *v) { (void)p; (void)idx; (void)pn; if (v) for (int i = 0; i < n; i++) v[i] = 0; }
void glGetUniformIndices(GLuint p, GLsizei n, const GLchar *const *nm, GLuint *idx) { (void)p; (void)nm; if (idx) for (int i = 0; i < n; i++) idx[i] = 0xFFFFFFFFu; }
void glUniformBlockBinding(GLuint p, GLuint bi, GLuint bb) { (void)p; (void)bi; (void)bb; }
// 64-bit / indexed integer queries → reuse glGetIntegerv for the scalar values.
void glGetInteger64v(GLenum p, GLint64 *v) { if (!v) return; GLint t = 0; glGetIntegerv(p, &t); *v = t; }
void glGetIntegeri_v(GLenum p, GLuint idx, GLint *v) { (void)idx; if (v) glGetIntegerv(p, v); }
void glGetInteger64i_v(GLenum p, GLuint idx, GLint64 *v) { (void)idx; if (!v) return; GLint t = 0; glGetIntegerv(p, &t); *v = t; }
void glGetBufferParameteri64v(GLenum t, GLenum p, GLint64 *v) { (void)t; (void)p; if (v) *v = 0; }
void glGetInternalformativ(GLenum tgt, GLenum ifmt, GLenum pn, GLsizei bs, GLint *v) {
    (void)tgt;
    (void)ifmt;
    if (!v || bs <= 0) return;
    if (pn == GL_NUM_SAMPLE_COUNTS) {
        v[0] = 1;
    } else if (pn == GL_SAMPLES) {
        v[0] = 4;
    } else {
        v[0] = 0;
    }
    EGLDBG("glGetInternalformativ target=0x%x ifmt=0x%x pname=0x%x -> %d\n", tgt, ifmt, pn, v[0]);
}
// Program binary (stub: force source compile path).
void glGetProgramBinary(GLuint p, GLsizei bs, GLsizei *len, GLenum *fmt, void *bin) { (void)p; (void)bs; (void)bin; if (len) *len = 0; if (fmt) *fmt = 0; }
void glProgramBinary(GLuint p, GLenum fmt, const void *bin, GLsizei len) { (void)p; (void)fmt; (void)bin; (void)len; }
void glProgramParameteri(GLuint p, GLenum pn, GLint v) { (void)p; (void)pn; (void)v; }
// 3D / texture-array entry points. Chrome's GPU raster path may use the ES3 layer API even when it renders
// and samples a single 2D layer. Store layer 0 in the existing 2D texture representation so FBO-layer render
// passes can flow through the current IR/Metal render-to-texture path.
void glTexImage3D(GLenum t, GLint l, GLint ifmt, GLsizei w, GLsizei h, GLsizei d, GLint b, GLenum fmt, GLenum ty, const void *px) {
    (void)ifmt; (void)b; (void)ty;
    GLuint tex = g_tex_unit[g_active_unit];
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glTexImage3D target=0x%x unit=%d tex=%u level=%d %dx%dx%d fmt=0x%x type=0x%x pixels=%p\n", t, g_active_unit, tex, l, w, h, d, fmt, ty, px);
    if (l != 0 || (t != GL_TEXTURE_2D_ARRAY && t != GL_TEXTURE_3D) || tex >= MAXTEX || !g_tex[tex].used) return;
    if (!tex_alloc_rgba(tex, w, h)) return;
    if (px && d > 0) tex_store_pixels(&g_tex[tex], 0, 0, w, h, fmt, px);
    dump_tex_ppm(tex, &g_tex[tex], "image3d-layer0");
}
void glTexSubImage3D(GLenum t, GLint l, GLint xo, GLint yo, GLint zo, GLsizei w, GLsizei h, GLsizei d, GLenum fmt, GLenum ty, const void *px) {
    (void)ty;
    GLuint tex = g_tex_unit[g_active_unit];
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glTexSubImage3D target=0x%x unit=%d tex=%u level=%d xyz=%d,%d,%d %dx%dx%d fmt=0x%x type=0x%x pixels=%p\n", t, g_active_unit, tex, l, xo, yo, zo, w, h, d, fmt, ty, px);
    if (l != 0 || zo != 0 || d <= 0 || (t != GL_TEXTURE_2D_ARRAY && t != GL_TEXTURE_3D) ||
        tex >= MAXTEX || !g_tex[tex].used || !g_tex[tex].data) return;
    tex_store_pixels(&g_tex[tex], xo, yo, w, h, fmt, px);
    g_tex[tex].gen++;
    dump_tex_ppm(tex, &g_tex[tex], "sub3d-layer0");
}
void glCompressedTexImage3D(GLenum t, GLint l, GLenum ifmt, GLsizei w, GLsizei h, GLsizei d, GLint b, GLsizei sz, const void *px) { (void)t; (void)l; (void)ifmt; (void)w; (void)h; (void)d; (void)b; (void)sz; (void)px; }
void glCompressedTexSubImage3D(GLenum t, GLint l, GLint xo, GLint yo, GLint zo, GLsizei w, GLsizei h, GLsizei d, GLenum fmt, GLsizei sz, const void *px) { (void)t; (void)l; (void)xo; (void)yo; (void)zo; (void)w; (void)h; (void)d; (void)fmt; (void)sz; (void)px; }
void glCopyTexSubImage3D(GLenum t, GLint l, GLint xo, GLint yo, GLint zo, GLint x, GLint y, GLsizei w, GLsizei h) { (void)t; (void)l; (void)xo; (void)yo; (void)zo; (void)x; (void)y; (void)w; (void)h; }
void glTexStorage2D(GLenum t, GLsizei levels, GLenum ifmt, GLsizei w, GLsizei h) { (void)levels; (void)ifmt;
    // Immutable-storage allocation: mirror glTexImage2D's alloc so a later glTexSubImage2D has a target.
    GLuint tex = g_tex_unit[g_active_unit];
    if (t == GL_TEXTURE_2D && tex < MAXTEX && g_tex[tex].used) {
        free(g_tex[tex].data); g_tex[tex].w = w; g_tex[tex].h = h; g_tex[tex].size = (size_t)w * h * 4;
        g_tex[tex].data = calloc(1, g_tex[tex].size); g_tex[tex].gen++;
    }
}
void glTexStorage3D(GLenum t, GLsizei levels, GLenum ifmt, GLsizei w, GLsizei h, GLsizei d) {
    (void)levels; (void)ifmt;
    GLuint tex = g_tex_unit[g_active_unit];
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glTexStorage3D target=0x%x unit=%d tex=%u ifmt=0x%x %dx%dx%d\n", t, g_active_unit, tex, ifmt, w, h, d);
    if ((t == GL_TEXTURE_2D_ARRAY || t == GL_TEXTURE_3D) && tex < MAXTEX && g_tex[tex].used) {
        tex_alloc_rgba(tex, w, h);
    }
}
void glTexStorage2DEXT(GLenum t, GLsizei levels, GLenum ifmt, GLsizei w, GLsizei h) { glTexStorage2D(t, levels, ifmt, w, h); }
void glTexStorage3DEXT(GLenum t, GLsizei levels, GLenum ifmt, GLsizei w, GLsizei h, GLsizei d) { glTexStorage3D(t, levels, ifmt, w, h, d); }
void glBlitFramebuffer(GLint sx0, GLint sy0, GLint sx1, GLint sy1, GLint dx0, GLint dy0, GLint dx1, GLint dy1, GLbitfield mask, GLenum filter) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glBlitFramebuffer src=%d,%d-%d,%d dst=%d,%d-%d,%d mask=0x%x filter=0x%x\n", sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1, mask, filter);
    (void)filter;
    if (!(mask & GL_COLOR_BUFFER_BIT)) return;
    GLuint src = fbo_color_texture(g_read_fbo);
    GLuint dst = fbo_color_texture(g_draw_fbo);
    if (!src || !dst || src == dst) return;
    copy_texture_rect(src, dst, sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1);
    dump_tex_ppm(dst, &g_tex[dst], "blit");
}
void glBlitFramebufferANGLE(GLint sx0, GLint sy0, GLint sx1, GLint sy1, GLint dx0, GLint dy0, GLint dx1, GLint dy1, GLbitfield mask, GLenum filter) {
    glBlitFramebuffer(sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1, mask, filter);
}
void glFramebufferTextureLayer(GLenum t, GLenum att, GLuint tex, GLint l, GLint layer) {
    GLuint f = (t == GL_READ_FRAMEBUFFER) ? g_read_fbo : g_draw_fbo;
    if ((t == GL_FRAMEBUFFER || t == GL_DRAW_FRAMEBUFFER || t == GL_READ_FRAMEBUFFER) &&
        att == GL_COLOR_ATTACHMENT0 && l == 0 && f > 0 && f < MAXFBO) {
        g_fbo[f].used = 1;
        g_fbo[f].color_tex = tex;
        g_fbo[f].color_rbo = 0;
        g_fbo[f].color_level = l;
        g_fbo[f].color_layer = layer;
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glFramebufferTextureLayer target=0x%x attachment=0x%x tex=%u level=%d layer=%d fbo=%u\n", t, att, tex, l, layer, f);
}
void glRenderbufferStorageMultisample(GLenum t, GLsizei s, GLenum ifmt, GLsizei w, GLsizei h) {
    if (t == GL_RENDERBUFFER && g_rbo_bound > 0 && g_rbo_bound < MAXRBO) {
        g_rbo[g_rbo_bound].used = 1;
        g_rbo[g_rbo_bound].w = w;
        g_rbo[g_rbo_bound].h = h;
        g_rbo[g_rbo_bound].ifmt = ifmt;
        g_rbo[g_rbo_bound].samples = s;
        g_rbo[g_rbo_bound].gen++;
    }
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glRenderbufferStorageMultisample target=0x%x samples=%d ifmt=0x%x %dx%d\n", t, s, ifmt, w, h);
}
void glRenderbufferStorageMultisampleANGLE(GLenum t, GLsizei s, GLenum ifmt, GLsizei w, GLsizei h) { glRenderbufferStorageMultisample(t, s, ifmt, w, h); }
void glRenderbufferStorageMultisampleEXT(GLenum t, GLsizei s, GLenum ifmt, GLsizei w, GLsizei h) { glRenderbufferStorageMultisample(t, s, ifmt, w, h); }
void glInvalidateFramebuffer(GLenum t, GLsizei n, const GLenum *att) { (void)t; (void)n; (void)att; }
void glInvalidateSubFramebuffer(GLenum t, GLsizei n, const GLenum *att, GLint x, GLint y, GLsizei w, GLsizei h) { (void)t; (void)n; (void)att; (void)x; (void)y; (void)w; (void)h; }
void glClearBufferiv(GLenum b, GLint d, const GLint *v) { (void)b; (void)d; (void)v; }
void glClearBufferuiv(GLenum b, GLint d, const GLuint *v) { (void)b; (void)d; (void)v; }
void glClearBufferfv(GLenum b, GLint d, const GLfloat *v) {
    if (getenv("HL_SHIM_DEBUG")) fprintf(stderr, "[shim] glClearBufferfv buffer=0x%x drawbuffer=%d fbo=%u\n", b, d, g_draw_fbo);
    if (b == GL_COLOR && d == 0 && v) clear_bound_color_texture(v);
}
void glClearBufferfi(GLenum b, GLint d, GLfloat depth, GLint stencil) { (void)b; (void)d; (void)depth; (void)stencil; }

// ======================= translator test tool (host build) =======================
// Build: cc -HLD_TR_TOOL gl_shim.c -o gl_tr ; run: gl_tr vertex.glsl fragment.glsl > out.metal
// Feeds real GLSL-ES through the SAME translate() the shim uses at glLinkProgram time, so the emitted MSL
// can be compiled (hl-display selftest-msl) to prove arbitrary app shaders (e.g. glmark2's) translate.
#ifdef HL_TR_TOOL
static char *slurp(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) { perror(path); exit(1); }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *b = malloc(n + 1);
    if (fread(b, 1, n, f) != (size_t)n) { perror("read"); exit(1); }
    b[n] = 0;
    fclose(f);
    return b;
}
int main(int argc, char **argv) {
    if (argc < 3) { fprintf(stderr, "usage: %s vert.glsl frag.glsl [--print-layout]\n", argv[0]); return 2; }
    int print_layout = 0;
    for (int i = 3; i < argc; i++) if (!strcmp(argv[i], "--print-layout")) print_layout = 1;
    char *vs = slurp(argv[1]), *fs = slurp(argv[2]);
    // --print-layout: dump the uniform-block byte layout uni_layout() computes as `LAYOUT name off sz` lines
    // (+ `TOTAL n`). A host proof (run_uniform_layout_proof.sh) diffs this against offsets the C compiler
    // computes for an MSL-faithful struct, proving the shim's offsets match Metal's real matrix layout.
    if (print_layout) {
        struct uni u[16];
        int total = 0;
        int nu = uni_layout(vs, fs, u, 16, &total);
        for (int i = 0; i < nu; i++) printf("LAYOUT %s %d %d\n", u[i].name, u[i].off, u[i].sz);
        printf("TOTAL %d\n", total);
        return 0;
    }
    char *msl = translate(vs, fs);
    fputs(msl, stdout);
    return 0;
}
#endif
