// dd guest GLES2 + EGL shim (GPU rung 3 ICD, first slice). A real GLES2 app links -lEGL -lGLESv2 and
// runs UNMODIFIED against these symbols (mount-injected as libEGL.so.1 + libGLESv2.so.2, like libwayland
// — NOT a specialized image). Each GL/EGL call drives a small state machine; on eglSwapBuffers the shim
// translates the accumulated GL state into a dd-gpu IR command stream, ships it to the host Metal executor
// ($DD_GPU_EXEC) which renders it into a rung-2 IOSurface, and commits that IOSurface to dd-display
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
// build/texture-scene shaders (which Metal compiles; see `dd-display selftest-msl`). Arbitrary shaders
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
#define GL_UNSIGNED_BYTE 0x1401
#define GL_UNSIGNED_SHORT 0x1403
#define GL_UNSIGNED_INT 0x1405
#define GL_TRIANGLES 0x0004
#define GL_TRIANGLE_STRIP 0x0005
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_DEPTH_BUFFER_BIT 0x0100
#define GL_DEPTH_TEST 0x0B71
#define GL_CULL_FACE 0x0B44
#define GL_TEXTURE_2D 0x0DE1
#define GL_TEXTURE0 0x84C0
#define GL_RGBA 0x1908
#define GL_RGB 0x1907
#define GL_LUMINANCE 0x1909
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_TEXTURE_WRAP_S 0x2802
#define GL_TEXTURE_WRAP_T 0x2803
#define GL_NEAREST 0x2600
#define GL_LINEAR 0x2601
#define GL_NEAREST_MIPMAP_NEAREST 0x2700
#define GL_LINEAR_MIPMAP_NEAREST 0x2701
#define GL_NEAREST_MIPMAP_LINEAR 0x2702
#define GL_LINEAR_MIPMAP_LINEAR 0x2703
#define GL_CLAMP_TO_EDGE 0x812F
#define GL_REPEAT 0x2901
#define GL_MIRRORED_REPEAT 0x8370
#define GL_VERSION 0x1F02
#define GL_VENDOR 0x1F00
#define GL_RENDERER 0x1F01
#define GL_SHADING_LANGUAGE_VERSION 0x8B8C

// ---- dd ioctl + dmabuf constants (match dd_gpu.h) ----
#define DD_IOCTL_GPU_ALLOC 0xC020DD01u
#define DD_DMABUF_MOD_MAGIC 0x6464u
#define DRM_FMT_XRGB8888 0x34325258u
struct dd_gpu_alloc {
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
    char samp[4][32]; // sampler2D uniform names (→ texture(0..)/sampler(0..))
    int nsamp;
};
static uint8_t g_ubuf[512]; // current uniform-block bytes (written by glUniform*)
struct buf {
    int used;
    uint8_t *data;
    size_t size;
    uint64_t gen; // L5: bumped on every content mutation (glBufferData/SubData, alloc/free) → dirty key
};
struct attr {
    int enabled, size;
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
static struct sh g_sh[MAXSH];
static struct prog g_prog[MAXPROG];
static struct buf g_buf[MAXBUF];
static struct attr g_attr[MAXATTR];
static struct tex g_tex[MAXTEX];
static GLuint g_tex_unit[8]; // texture bound per active unit (GL_TEXTURE_2D)
static int g_active_unit;
static GLuint g_cur_prog, g_arr_buf, g_elem_buf;
static int g_depth; // GL_DEPTH_TEST enabled
static float g_clear[4] = {0, 0, 0, 1};
static int g_draw_mode = -1, g_draw_first, g_draw_count; // last glDrawArrays this frame
static int g_draw_indexed;      // this frame's draw was glDrawElements
static int g_index_type;        // GL_UNSIGNED_SHORT / GL_UNSIGNED_INT
static size_t g_index_offset;   // byte offset into the element buffer
// Draw-time snapshot of the vertex-attribute array. glmark2 (Mesh::render_vbo) enables its attribs,
// issues the draw, then DISABLES them again — all before eglSwapBuffers. Since the shim assembles the
// frame's IR lazily at swap, reading live g_attr would see the torn-down (disabled) state → empty vertex
// layout. So we snapshot g_attr at draw-call time and the swap uses that snapshot.
static struct attr g_attr_snap[MAXATTR];
static int g_have_draw_snap;

// surface
static struct dd_gpu_alloc g_surf;
static int g_have_surf;
static int g_wl = -1, g_wl_ready; // wayland socket to dd-display
static uint32_t g_wl_surface = 6, g_wl_buffer = 10;
static uint32_t g_wl_frame_cb = 11; // wl_callback id for wl_surface.frame (L1 pacing)

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
// (a fresh host backend has an empty cache → re-emit everything). A/B: DD_NO_DELTA=1 forces full re-upload.
struct residency { int valid, src; uint64_t gen; };
static struct residency g_res_vbuf[MAXATTR]; // IR ids 200+slot (one per distinct source VBO this frame)
static struct residency g_res_index;         // IR id 12 (the element/index buffer)
static struct residency g_res_tex[4];         // texture slots k → tid 50+k (staging upload + CopyToTexture)
static int g_res_reset;                       // set on a host RE-connect (cache went empty) → re-emit all
static int g_no_delta = -1;                   // DD_NO_DELTA A/B gate (−1 unresolved)
static int delta_on(void) {
    if (g_no_delta < 0) g_no_delta = getenv("DD_NO_DELTA") ? 1 : 0;
    return !g_no_delta;
}
static void l5_reset_residency(void) {
    memset(g_res_vbuf, 0, sizeof g_res_vbuf);
    memset(&g_res_index, 0, sizeof g_res_index);
    memset(g_res_tex, 0, sizeof g_res_tex);
}

// ---- DD_RENDER_PROF: env-gated per-frame frame-time ledger (mirrors DD_SHIM_DEBUG getenv-once) ----
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
        g_prof = getenv("DD_RENDER_PROF") ? 1 : 0;
        if (g_prof) {
            const char *dir = getenv("DD_RENDER_PROF_DIR");
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

// ======================= dd-gpu IR wire (matches dd-gpu/src/wire.rs) =======================
// Large enough for a real model's vertex/index/texture uploads (glmark2's horse is ~21.5k verts ≈ 258 KB
// per attribute buffer); es2tri's triangle needed only bytes. 8 MB headroom keeps a whole frame's IR.
static uint8_t ir[8 << 20];
static size_t irn;
static void iu8(uint8_t v) { ir[irn++] = v; }
static void iu32(uint32_t v) { memcpy(ir + irn, &v, 4); irn += 4; }
static void iu64(uint64_t v) { memcpy(ir + irn, &v, 8); irn += 8; }
static void ifl(float v) { memcpy(ir + irn, &v, 4); irn += 4; }
static void istr(const char *s) { uint32_t l = (uint32_t)strlen(s); iu32(l); memcpy(ir + irn, s, l); irn += l; }
static void ibytes(const uint8_t *b, uint32_t l) { iu32(l); if (irn + l > sizeof ir) { fprintf(stderr, "gl_shim: IR overflow (%zu+%u > %zu), dropping\n", irn, l, sizeof ir); return; } memcpy(ir + irn, b, l); irn += l; }
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
    else if (!strcmp(t, "mat4")) strcpy(out, "float4x4");
    else strcpy(out, t);
}
// collect `kw TYPE name;` decls from src
static int collect(const char *src, const char *kw, struct decl *out, int max) {
    int n = 0;
    const char *p = src;
    size_t kl = strlen(kw);
    while ((p = strstr(p, kw)) && n < max) {
        // must be at a word boundary
        if (p != src && (p[-1] == '_' || (p[-1] >= 'a' && p[-1] <= 'z') || (p[-1] >= 'A' && p[-1] <= 'Z'))) { p += kl; continue; }
        const char *q = p + kl;
        while (*q == ' ' || *q == '\t') q++;
        char ty[16] = {0};
        int i = 0;
        while (*q && *q != ' ' && *q != '\t' && i < 15) ty[i++] = *q++;
        while (*q == ' ' || *q == '\t') q++;
        char nm[32] = {0};
        i = 0;
        while (*q && (*q == '_' || (*q >= 'a' && *q <= 'z') || (*q >= 'A' && *q <= 'Z') || (*q >= '0' && *q <= '9')) && i < 31) nm[i++] = *q++;
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
static void type_fixups(char *b) {
    wreplace(b, "vec2", "float2");
    wreplace(b, "vec3", "float3");
    wreplace(b, "vec4", "float4");
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
static char *translate(const char *vs_in, const char *fs_in) {
    char vsbuf[16384], fsbuf[16384];
    snprintf(vsbuf, sizeof vsbuf, "%s", vs_in);
    snprintf(fsbuf, sizeof fsbuf, "%s", fs_in);
    strip_comments(vsbuf);
    strip_comments(fsbuf);
    const char *vs = vsbuf, *fs = fsbuf;
    struct decl attrs[16], vary[16], unis[16], samps[4];
    int na = collect(vs, "attribute", attrs, 16);
    int nv = collect(vs, "varying", vary, 16);
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
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "vertex VOut vmain(VIn in [[stage_in]]%s", uparam);
    o = emit_samp_params(out, o, vb, samps, nsamp);
    o = cat_msl(out, o, TRANSLATE_OUTCAP, ") {\n  VOut out;\n%s\n  return out;\n}\n", vb);
    // fragment
    char fb[4096];
    main_body(fs, fb, sizeof fb);
    fix_trunc(fb);
    type_fixups(fb);
    sampler_fixups(fb, samps, nsamp);
    for (int i = 0; i < nv; i++) {
        char in[40]; sprintf(in, "in.%s", vary[i].name); wreplace(fb, vary[i].name, in);
    }
    for (int i = 0; i < nu; i++) {
        char un[40]; sprintf(un, "u.%s", unis[i].name); wreplace(fb, unis[i].name, un);
    }
    wreplace(fb, "gl_FragColor", "_frag");
    o = cat_msl(out, o, TRANSLATE_OUTCAP, "fragment float4 fmain(VOut in [[stage_in]]%s", uparam);
    o = emit_samp_params(out, o, fb, samps, nsamp);
    o = cat_msl(out, o, TRANSLATE_OUTCAP, ") {\n  float4 _frag = float4(0);\n%s\n  return _frag;\n}\n", fb);
    return out;
}

// Compute the uniform-buffer byte layout (name→offset/size) matching Metal's struct alignment, so the
// bytes the app writes via glUniform* land where the MSL Uniforms struct expects them.
static int uni_layout(const char *vs, const char *fs, struct uni *out, int max, int *total) {
    struct decl unis[16], samps[4];
    int nu, nsamp;
    collect_uniforms(vs, fs, unis, &nu, samps, &nsamp); // DATA uniforms only (samplers excluded)
    int cur = 0, n = 0;
    for (int i = 0; i < nu && n < max; i++) {
        int sz, al;
        if (!strcmp(unis[i].type, "mat4")) { sz = 64; al = 16; }
        else if (!strcmp(unis[i].type, "vec4")) { sz = 16; al = 16; }
        else if (!strcmp(unis[i].type, "vec3")) { sz = 16; al = 16; }
        else if (!strcmp(unis[i].type, "vec2")) { sz = 8; al = 8; }
        else { sz = 4; al = 4; }
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
static void surface_up(uint32_t w, uint32_t h) {
    fprintf(stderr, "gl_shim: surface_up %ux%u\n", w, h);
    // DD_IR_DUMP: host-tool mode — no renderD128/wayland; just record the surface so eglSwapBuffers builds
    // the IR and exec_stream writes it to the dump file. Proves the shim's IR byte-stream on any host.
    if (getenv("DD_IR_DUMP")) {
        g_surf.width = w; g_surf.height = h; g_surf.id = 1; g_have_surf = 1;
        return;
    }
    int rnode = open("/dev/dri/renderD128", O_RDWR);
    if (rnode < 0) { fprintf(stderr, "gl_shim: no renderD128 (errno=%d %s)\n", errno, strerror(errno)); return; }
    fprintf(stderr, "gl_shim: renderD128 fd=%d\n", rnode);
    g_surf.width = w; g_surf.height = h; g_surf.format = 0;
    if (ioctl(rnode, DD_IOCTL_GPU_ALLOC, &g_surf) != 0) { fprintf(stderr, "gl_shim: alloc failed\n"); return; }
    g_have_surf = 1;
    // wayland handshake to dd-display
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
    uint32_t reg = 2, comp = 3, dmabuf = 4, wm = 5, xdg = 7, toplevel = 8;
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
    uint32_t xw[2] = {xdg, g_wl_surface};
    wmsg(wm, 2, xw, 2);                     // get_xdg_surface
    wmsg(xdg, 1, &toplevel, 1);            // get_toplevel
    wmsg(g_wl_surface, 6, 0, 0);          // commit (initial)
    wflush();
    usleep(50000);
    uint32_t ack[1] = {1};
    wmsg(xdg, 4, ack, 1); // ack_configure
    wflush();
    g_wl_ready = 1;
    (void)dmabuf;
}
// Stream the current IR to the executor and wait for the render ack.
static void exec_stream(void) {
    const char *dump = getenv("DD_IR_DUMP");
    if (dump) { // host-tool mode: write the raw IR byte-stream to the dump file (proof harness)
        int fd = open(dump, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd >= 0) { if (write(fd, ir, irn) < 0) perror("ir dump"); close(fd); }
        fprintf(stderr, "gl_shim: dumped %zu IR bytes to %s\n", irn, dump);
        return;
    }
    const char *ep = getenv("DD_GPU_EXEC");
    if (!ep) ep = "/run/user/0/dd-gpu-0";
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
        if (write(g_exec_fd, hdr, sizeof hdr) != (ssize_t)sizeof hdr ||
            write(g_exec_fd, ir, irn) != (ssize_t)irn) {
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
// Commit the (executor-rendered) IOSurface to dd-display via linux-dmabuf.
static void wl_commit(void) {
    if (getenv("DD_IR_DUMP")) return; // host-tool mode: no wayland commit
    if (g_wl < 0 || !g_wl_ready) return;
    uint32_t dmabuf = 4, params = 9;
    wmsg(dmabuf, 1, &params, 1); // create_params
    wflush();
    uint32_t addw[5] = {0, 0, g_surf.stride, DD_DMABUF_MOD_MAGIC, g_surf.id};
    wmsg(params, 1, addw, 5); // add(fd via SCM_RIGHTS)
    wflush_fd(g_surf.fd);
    uint32_t ci[5] = {g_wl_buffer, g_surf.width, g_surf.height, DRM_FMT_XRGB8888, 0};
    wmsg(params, 3, ci, 5); // create_immed
    uint32_t at[3] = {g_wl_buffer, 0, 0};
    wmsg(g_wl_surface, 1, at, 3); // attach
    uint32_t dm[4] = {0, 0, g_surf.width, g_surf.height};
    wmsg(g_wl_surface, 2, dm, 4); // damage
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
#define DD_WL_EGL_MAGIC ((intptr_t)0x6464776C65676CLL) // "ddwlegl" magic
struct dd_wl_egl_window {
    intptr_t version;   // = DD_WL_EGL_MAGIC (Mesa stores WL_EGL_WINDOW_VERSION here)
    int width, height;  // offsets 8/12 — same as Mesa's struct
    int dx, dy;
    int attached_width, attached_height;
    void *driver_private;
    void (*resize_cb)(struct dd_wl_egl_window *, void *);
    void (*destroy_cb)(struct dd_wl_egl_window *);
    void *surface;      // the wl_surface
};
struct dd_wl_egl_window *wl_egl_window_create(void *surface, int width, int height) {
    if (width <= 0 || height <= 0) return 0;
    struct dd_wl_egl_window *w = calloc(1, sizeof *w);
    if (!w) return 0;
    w->version = DD_WL_EGL_MAGIC;
    w->width = width;
    w->height = height;
    w->surface = surface;
    return w;
}
void wl_egl_window_resize(struct dd_wl_egl_window *w, int width, int height, int dx, int dy) {
    if (!w) return;
    w->width = width;
    w->height = height;
    w->dx = dx;
    w->dy = dy;
}
void wl_egl_window_get_attached_size(struct dd_wl_egl_window *w, int *width, int *height) {
    if (!w) return;
    if (width) *width = w->attached_width ? w->attached_width : w->width;
    if (height) *height = w->attached_height ? w->attached_height : w->height;
}
void wl_egl_window_destroy(struct dd_wl_egl_window *w) { free(w); }

// ======================= EGL entry points =======================
#define EGLDBG(...) do { if (getenv("DD_SHIM_DEBUG")) { fprintf(stderr, "[shim] " __VA_ARGS__); fflush(stderr); } } while (0)
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
        case EGL_VERSION: r = "1.4 dd-shim"; break;
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
    if (getenv("DD_SHIM_DEBUG")) {
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
        case EGL_RENDERABLE_TYPE: r = EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES_BIT; break;
        case EGL_CONFORMANT: r = EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES_BIT; break;
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
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] eglGetConfigAttrib cfg=%p attr=0x%x -> %d\n", c, a, r);
    return EGL_TRUE;
}
EGLContext eglCreateContext(EGLDisplay dpy, EGLConfig c, EGLContext s, const EGLint *a) {
    (void)dpy; (void)s;
    if (getenv("DD_SHIM_DEBUG")) {
        fprintf(stderr, "[shim] eglCreateContext cfg=%p share=%p attribs:", c, s);
        if (a) for (const EGLint *p = a; *p != EGL_NONE; p += 2) fprintf(stderr, " 0x%x=%d", p[0], p[1]);
        fprintf(stderr, " -> 1\n"); fflush(stderr);
    }
    return (EGLContext)1;
}
EGLSurface eglCreateWindowSurface(EGLDisplay dpy, EGLConfig c, EGLNativeWindowType w, const EGLint *a) {
    (void)dpy; (void)c; (void)a;
    uint32_t W = 256, H = 256;
    if (w) {
        struct dd_wl_egl_window *win = (struct dd_wl_egl_window *)w;
        int ww, hh;
        if (win->version == DD_WL_EGL_MAGIC) {
            // Our libwayland-egl.so.1 struct (glmark2, Chrome/ozone): width/height at offsets 8/12.
            ww = win->width;
            hh = win->height;
        } else {
            // Stock-app convention (es2tri/es2tex): two ints {width, height} at offset 0. NB: read only
            // the 8 bytes those apps actually allocate — never touch offsets 8/12 here (that array is
            // exactly 8 bytes; the OOB read segfaulted the in-process launcher).
            int *p = (int *)w;
            ww = p[0];
            hh = p[1];
        }
        if (ww > 0 && ww <= 8192) W = (uint32_t)ww;
        if (hh > 0 && hh <= 8192) H = (uint32_t)hh;
    }
    surface_up(W, H);
    return (EGLSurface)1;
}
EGLBoolean eglMakeCurrent(EGLDisplay dpy, EGLSurface d, EGLSurface r, EGLContext c) { (void)dpy; EGLDBG("eglMakeCurrent draw=%p read=%p ctx=%p\n", d, r, c); return EGL_TRUE; }
EGLBoolean eglSwapInterval(EGLDisplay dpy, EGLint i) { (void)dpy; (void)i; return EGL_TRUE; }
EGLBoolean eglBindAPI(EGLenum a) { EGLDBG("eglBindAPI 0x%x\n", a); return EGL_TRUE; }
EGLint eglGetError(void) { return EGL_SUCCESS; }
EGLBoolean eglQuerySurface(EGLDisplay dpy, EGLSurface s, EGLint a, EGLint *v) {
    (void)dpy; (void)s;
    if (v) { if (a == EGL_WIDTH) *v = g_surf.width; else if (a == EGL_HEIGHT) *v = g_surf.height; else *v = 0; }
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
EGLBoolean eglQueryContext(EGLDisplay dpy, EGLContext c, EGLint a, EGLint *v) { (void)dpy; (void)c; (void)a; if (v) *v = 0; return EGL_TRUE; }
EGLBoolean eglCopyBuffers(EGLDisplay dpy, EGLSurface s, void *tgt) { (void)dpy; (void)s; (void)tgt; return EGL_TRUE; }
// Pbuffer surface: ANGLE creates a tiny (typically 1x1) offscreen surface to make its BOOTSTRAP GL context
// current during Display::initialize (GL capability probing), BEFORE the real window surface exists. Return
// a DISTINCT non-null handle and do NOT run the IOSurface/Wayland bring-up — that belongs to the WINDOW
// surface, whose pixels reach dd-display; clobbering the single global g_surf here would redirect the
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
    if (!p && getenv("DD_SHIM_DEBUG")) { fprintf(stderr, "[shim] eglGetProcAddress(\"%s\") -> NULL\n", n); fflush(stderr); }
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
    irn = 0;
    struct prog *pr = (g_cur_prog < MAXPROG) ? &g_prog[g_cur_prog] : NULL;
    // Which textures this frame binds: sampler i (declaration order) → texture unit i's bound texture.
    int texlist[4], ntex = 0;
    int tex_upload[4] = {0, 0, 0, 0}; // L5: per-bound-texture, did we (re)upload pixels this frame? → copy op
    if (pr)
        for (int i = 0; i < pr->nsamp && i < 4; i++) {
            GLuint tu = g_tex_unit[i];
            if (tu < MAXTEX && g_tex[tu].used && g_tex[tu].data) texlist[ntex++] = (int)tu;
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
    for (int i = 0; i < MAXATTR; i++) attr_slot[i] = -1;
    for (int i = 0; i < MAXATTR; i++) {
        if (!g_attr[i].enabled) continue;
        int b = g_attr[i].buffer;
        if (b < 0 || b >= MAXBUF || !g_buf[b].used || !g_buf[b].data) continue;
        int sl = -1;
        for (int k = 0; k < nslot; k++) if (slot_vbo[k] == b) { sl = k; break; }
        if (sl < 0) { sl = nslot; slot_vbo[nslot++] = b; }
        attr_slot[i] = sl;
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
    // 1b. index buffer for glDrawElements → CreateBuffer(12, INDEX) + WriteBuffer (whole element buffer),
    //     gated the same way — the horse's 21.5k-index buffer is static, so it too uploads exactly once.
    if (g_draw_indexed && g_elem_buf < MAXBUF && g_buf[g_elem_buf].used && g_buf[g_elem_buf].data) {
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
        uint32_t tid = 50 + k, sid = 60 + k, stg = 70 + k;
        // CreateTexture(tid): {w,h,depth1,mips1,samples1,dim=D2,fmt=Rgba8Unorm,usage=SAMPLED|COPY_DST}
        iu8(4); iu32(tid); iu32(t->w); iu32(t->h); iu32(1); iu32(1); iu32(1); iu32(2); iu32(1); iu32(1 | 16); istr("");
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
        struct residency *r = &g_res_tex[k];
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
    int ndecl = (pr && pr->vs && g_sh[pr->vs].src) ? collect(g_sh[pr->vs].src, "attribute", vdecl, 16) : 0;
    int nvd = ndecl;
    for (int i = 0; i < MAXATTR; i++) if (g_attr[i].enabled && i + 1 > nvd) nvd = i + 1;
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] eglSwapBuffers draw_mode=%d prog=%d msl=%s nslot=%d nvd=%d ntex=%d\n",
        g_draw_mode, g_cur_prog, (pr&&pr->msl)?"OK":"none", nslot, nvd, ntex);

    // 2. program → shader (combined MSL) + pipeline
    int has_u = pr && pr->nuni > 0;
    if (pr && pr->msl) {
        ir_shader(20, pr->msl); // CreateShader(20, MSL)
        // CreateRenderPipeline(30): vertex module 20 entry vmain, fragment module 20 entry fmain
        iu8(10); iu32(30);
        iu32(20); istr("vmain");         // vertex ShaderRef
        iu8(1); iu32(20); istr("fmain"); // fragment Some
        // One VertexLayout per slot (== distinct source VBO); each carries the attributes bound to that
        // slot, so the Metal descriptor gives every buffer its own MTLVertexBufferLayout + bufferIndex.
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
                uint32_t comps, off;
                if (L < MAXATTR && g_attr[L].enabled && attr_slot[L] >= 0) { comps = (uint32_t)g_attr[L].size; off = (uint32_t)g_attr[L].offset; }
                else {
                    const char *t = (L < ndecl) ? vdecl[L].type : "vec4";
                    comps = !strncmp(t, "vec2", 4) ? 2 : !strncmp(t, "vec3", 4) ? 3 : !strncmp(t, "float", 5) ? 1 : 4;
                    off = 0;
                }
                iu32((uint32_t)L); iu32(comps); iu32(off); // location, format=components, offset
            }
        }
        iu32(1);                          // color_targets len
        iu32(2); iu8(0); iu32(0xf);       // Bgra8Unorm, no blend, mask
        if (g_depth) { iu8(1); iu32(10); iu8(1); iu32(0); } // depth Some{Depth32Float, write, compare}
        else iu8(0);                      // depth None
        uint32_t topo = (g_draw_mode == GL_TRIANGLE_STRIP) ? 4 : 3;
        iu32(topo); iu32(0); iu32(0);     // topology, cull, front_face
        istr("");                         // label
    }
    // 2b. uniforms + textures + samplers → uniform buffer + a combined bind group (40).
    if (has_u) {
        iu8(1); iu32(11); iu64(pr->ubuf_size); iu32(4 /*UNIFORM*/); istr(""); // CreateBuffer(11)
        iu8(3); iu32(11); iu64(0); ibytes(g_ubuf, (uint32_t)pr->ubuf_size);   // WriteBuffer(11)
    }
    int has_bg = has_u || ntex > 0;
    if (has_bg) {
        uint32_t nent = (has_u ? 1u : 0u) + (uint32_t)ntex * 2u;
        // CreateBindGroup(40, {set:0, entries:[...]})
        iu8(13); iu32(40); iu32(0); iu32(nent);
        if (has_u) { iu32(1); iu8(0); iu32(11); iu64(0); iu64(pr->ubuf_size); }  // binding1 = Uniforms buffer
        for (int k = 0; k < ntex; k++) {
            iu32((uint32_t)k); iu8(1); iu32(50 + k); // binding k = Texture(50+k)
            iu32((uint32_t)k); iu8(2); iu32(60 + k); // binding k = Sampler(60+k)
        }
    }
    // 3. Submit: [CopyBufferToTexture]* + BeginRenderPass + [SetPipeline,(SetBindGroup),SetVertexBuffer,
    //    (SetIndexBuffer,DrawIndexed | Draw)] + EndRenderPass.
    int ncopy = 0; // L5: only textures re-uploaded this frame need a CopyBufferToTexture op
    for (int k = 0; k < ntex; k++) ncopy += tex_upload[k];
    int nops = 2 + ncopy; // Begin + End + one copy per (re)uploaded texture
    if (g_draw_mode >= 0) {
        nops += 1 + nslot; // SetPipeline + one SetVertexBuffer per slot
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
        iu8(14); iu32(70 + k); iu64(0); iu32((uint32_t)t->w * 4); iu32(50 + k); iu32(0); iu32(t->w); iu32(t->h);
    }
    iu8(1); iu32(1);                 // BeginRenderPass, 1 color
    iu32(1); iu32(1); ifl(g_clear[0]); ifl(g_clear[1]); ifl(g_clear[2]); ifl(g_clear[3]); iu8(1); // tex1, Clear
    if (g_depth) { iu8(1); iu32(2); iu32(1); ifl(1.0f); } // depth Some{tex2, Clear, 1.0}
    else iu8(0);                     // depth None
    if (g_draw_mode >= 0) {
        iu8(3); iu32(30);                                // SetPipeline
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
    }
    iu8(2);  // EndRenderPass
    iu8(0);  // signal None
    uint64_t t_enc = g_prof ? now_us() : 0;
    exec_stream();
    uint64_t t_exec = g_prof ? now_us() : 0;
    wl_commit();
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
    return EGL_TRUE;
}

// ======================= GLES2 entry points =======================
void glClearColor(GLfloat r, GLfloat g, GLfloat b, GLfloat a) { g_clear[0] = r; g_clear[1] = g; g_clear[2] = b; g_clear[3] = a; }
void glClear(GLbitfield m) { (void)m; }
void glViewport(GLint x, GLint y, GLsizei w, GLsizei h) { (void)x; (void)y; (void)w; (void)h; }
void glEnable(GLenum c) { if (c == GL_DEPTH_TEST) g_depth = 1; }
void glDisable(GLenum c) { if (c == GL_DEPTH_TEST) g_depth = 0; }
GLenum glGetError(void) { return GL_NO_ERROR; }
void glFinish(void) {}
void glFlush(void) {}
const unsigned char *glGetString(GLenum n) {
    switch (n) {
        // ES2 by default so glmark2 (GLSL ES 1.00 shaders) and ANGLE's ES2 path both work. Chromium's
        // ANGLE gl-egl caps at ES2 against us anyway ("max supported 2.0"); reporting ES3 here bought no
        // extra Chrome progress (same pre-existing window-bringup stall) but risked glmark2's shaders, so
        // stay ES2. The ES3 entry points below stay exported and dormant (ANGLE won't resolve them on ES2).
        case GL_VERSION: return (const unsigned char *)"OpenGL ES 2.0 dd-shim";
        case GL_VENDOR: return (const unsigned char *)"dd";
        case GL_RENDERER: return (const unsigned char *)"dd-metal";
        case GL_SHADING_LANGUAGE_VERSION: return (const unsigned char *)"OpenGL ES GLSL ES 1.00";
        default: return (const unsigned char *)"";
    }
}
GLuint glCreateShader(GLenum type) {
    for (int i = 1; i < MAXSH; i++)
        if (!g_sh[i].used) { g_sh[i].used = 1; g_sh[i].type = type; g_sh[i].src = NULL;
            if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glCreateShader(0x%x) -> %d\n", type, i); return i; }
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glCreateShader(0x%x) EXHAUSTED -> 0\n", type);
    return 0;
}
void glShaderSource(GLuint sh, GLsizei count, const GLchar *const *str, const GLint *len) {
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glShaderSource ENTRY sh=%u count=%d used=%d\n", sh, count, (sh<MAXSH)?g_sh[sh].used:-1);
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
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glShaderSource sh=%u count=%d stored len=%zu\n", sh, count, o);
}
void glCompileShader(GLuint sh) { (void)sh; }
void glGetShaderiv(GLuint sh, GLenum p, GLint *v) {
    if (!v) return;
    // GL_SHADER_SOURCE_LENGTH (0x8B88): glmark2 sets the source then verifies this round-trips to
    // strlen(source)+1 (incl. NUL) before it will compile — returning 0 aborted "Failed to add shader".
    if (p == 0x8B88) { *v = (sh < MAXSH && g_sh[sh].used && g_sh[sh].src) ? (GLint)(strlen(g_sh[sh].src) + 1) : 0;
        if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glGetShaderiv sh=%u SOURCE_LENGTH -> %d (src=%p)\n", sh, *v, (sh<MAXSH)?(void*)g_sh[sh].src:0); return; }
    if (p == GL_COMPILE_STATUS) { *v = GL_TRUE; return; }
    *v = 0;
}
void glGetShaderInfoLog(GLuint sh, GLsizei bufSize, GLsizei *length, GLchar *infoLog) { (void)sh; (void)bufSize; if (length) *length = 0; if (infoLog && bufSize) infoLog[0] = 0; }
GLuint glCreateProgram(void) {
    for (int i = 1; i < MAXPROG; i++)
        if (!g_prog[i].used) { g_prog[i].used = 1; g_prog[i].vs = g_prog[i].fs = 0; g_prog[i].msl = NULL; return i; }
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
        if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glLinkProgram p=%u msl=%s (len=%zu)\n", p, pr->msl?"OK":"NULL", pr->msl?strlen(pr->msl):0);
        pr->nuni = uni_layout(g_sh[pr->vs].src, g_sh[pr->fs].src, pr->unis, 16, &pr->ubuf_size);
        // Record sampler-uniform names → texture/sampler bind slots (index = declaration order).
        struct decl du[16], su[4];
        int ndu, nsu;
        collect_uniforms(g_sh[pr->vs].src, g_sh[pr->fs].src, du, &ndu, su, &nsu);
        pr->nsamp = nsu;
        for (int i = 0; i < nsu; i++) strcpy(pr->samp[i], su[i].name);
    }
}
GLint glGetUniformLocation(GLuint p, const GLchar *name) {
    if (p < MAXPROG && g_prog[p].used) {
        for (int i = 0; i < g_prog[p].nuni; i++)
            if (!strcmp(g_prog[p].unis[i].name, name)) return g_prog[p].unis[i].off; // location = byte offset
        // sampler2D uniform: a sentinel location (>uniform-block) so glUniform1i(loc, unit) is ignored — the
        // sampler is bound by our bind group, not the uniform block.
        for (int i = 0; i < g_prog[p].nsamp; i++)
            if (!strcmp(g_prog[p].samp[i], name)) return 100000 + i;
    }
    return -1;
}
static void uni_write(GLint loc, const void *data, int n) {
    if (loc >= 0 && loc + n <= (int)sizeof(g_ubuf)) memcpy(g_ubuf + loc, data, n);
}
void glUniformMatrix4fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 64); }
void glUniform4fv(GLint l, GLsizei n, const GLfloat *v) { (void)n; uni_write(l, v, 16); }
void glUniform4f(GLint l, GLfloat a, GLfloat b, GLfloat c, GLfloat d) { float v[4] = {a, b, c, d}; uni_write(l, v, 16); }
void glUniform3fv(GLint l, GLsizei n, const GLfloat *v) { (void)n; uni_write(l, v, 12); }
void glUniform3f(GLint l, GLfloat a, GLfloat b, GLfloat c) { float v[3] = {a, b, c}; uni_write(l, v, 12); }
void glUniform1f(GLint l, GLfloat a) { uni_write(l, &a, 4); }
void glUniform1i(GLint l, GLint a) { uni_write(l, &a, 4); }
void glGetProgramiv(GLuint p, GLenum pn, GLint *v) { (void)p; if (pn == GL_LINK_STATUS && v) *v = GL_TRUE; else if (v) *v = 0; }
void glGetProgramInfoLog(GLuint p, GLsizei bufSize, GLsizei *length, GLchar *infoLog) { (void)p; (void)bufSize; if (length) *length = 0; if (infoLog && bufSize) infoLog[0] = 0; }
void glUseProgram(GLuint p) { if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glUseProgram(%u)\n", p); g_cur_prog = p; }
GLint glGetAttribLocation(GLuint p, const GLchar *name) {
    // declaration-order index in the vertex shader (matches our VIn attribute() numbering)
    if (p < MAXPROG && g_prog[p].used && g_prog[p].vs && g_sh[g_prog[p].vs].src) {
        struct decl at[16];
        int na = collect(g_sh[g_prog[p].vs].src, "attribute", at, 16);
        for (int i = 0; i < na; i++)
            if (!strcmp(at[i].name, name)) { if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glGetAttribLocation(%s) -> %d\n", name, i); return i; }
    }
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glGetAttribLocation(%s) -> -1\n", name);
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
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glBindBuffer t=0x%x b=%u\n", t, b);
    if (t == GL_ARRAY_BUFFER) g_arr_buf = b;
    else if (t == GL_ELEMENT_ARRAY_BUFFER) g_elem_buf = b;
}
void glBufferData(GLenum t, GLsizeiptr size, const void *data, GLenum usage) {
    (void)usage;
    GLuint b = (t == GL_ELEMENT_ARRAY_BUFFER) ? g_elem_buf : g_arr_buf;
    if ((t != GL_ARRAY_BUFFER && t != GL_ELEMENT_ARRAY_BUFFER) || b >= MAXBUF || !g_buf[b].used) return;
    free(g_buf[b].data);
    g_buf[b].data = malloc(size);
    g_buf[b].size = size;
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
void glBindTexture(GLenum target, GLuint t) { (void)target; if (g_active_unit < 8) g_tex_unit[g_active_unit] = t; }
void glTexImage2D(GLenum target, GLint level, GLint ifmt, GLsizei w, GLsizei h, GLint border, GLenum fmt, GLenum type, const void *pixels) {
    (void)target; (void)ifmt; (void)border; (void)type;
    GLuint t = g_tex_unit[g_active_unit];
    if (level != 0 || t >= MAXTEX || !g_tex[t].used) return;
    free(g_tex[t].data);
    g_tex[t].w = w; g_tex[t].h = h;
    g_tex[t].size = (size_t)w * h * 4;
    g_tex[t].data = malloc(g_tex[t].size);
    g_tex[t].gen++; // L5: pixels changed → next swap re-uploads (staging buffer + CopyBufferToTexture)
    // Expand the app's pixels to RGBA8 (Metal RGBA8Unorm). Handles GL_RGBA / GL_RGB / GL_LUMINANCE.
    if (g_tex[t].data) {
        const uint8_t *p = pixels;
        for (int i = 0; i < w * h; i++) {
            uint8_t r = 0, gg = 0, b = 0, a = 255;
            if (p) {
                if (fmt == GL_RGBA) { r = p[i * 4]; gg = p[i * 4 + 1]; b = p[i * 4 + 2]; a = p[i * 4 + 3]; }
                else if (fmt == GL_RGB) { r = p[i * 3]; gg = p[i * 3 + 1]; b = p[i * 3 + 2]; }
                else { r = gg = b = p[i]; } // LUMINANCE/alpha
            }
            g_tex[t].data[i * 4] = r; g_tex[t].data[i * 4 + 1] = gg; g_tex[t].data[i * 4 + 2] = b; g_tex[t].data[i * 4 + 3] = a;
        }
    }
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
void glPixelStorei(GLenum p, GLint v) { (void)p; (void)v; }
void glGenerateMipmap(GLenum target) { (void)target; }
void glDrawElements(GLenum mode, GLsizei count, GLenum type, const void *indices) {
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glDrawElements mode=0x%x count=%d type=0x%x\n", mode, count, type);
    g_draw_mode = mode;
    g_draw_count = count;
    g_draw_indexed = 1;
    g_index_type = type;
    g_index_offset = (size_t)indices;
    memcpy(g_attr_snap, g_attr, sizeof g_attr); g_have_draw_snap = 1;
}
void glVertexAttribPointer(GLuint i, GLint size, GLenum type, GLboolean norm, GLsizei stride, const void *ptr) {
    (void)norm;
    if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glVertexAttribPointer i=%u size=%d stride=%d off=%zu arrbuf=%d\n", i, size, stride, (size_t)ptr, g_arr_buf);
    if (i >= MAXATTR) return;
    g_attr[i].size = size;
    g_attr[i].type = type;
    g_attr[i].stride = stride;
    g_attr[i].offset = (size_t)ptr;
    g_attr[i].buffer = g_arr_buf;
}
void glEnableVertexAttribArray(GLuint i) { if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glEnableVertexAttribArray(%u) [MAXATTR=%d]\n", i, MAXATTR); if (i < MAXATTR) g_attr[i].enabled = 1; }
void glDisableVertexAttribArray(GLuint i) { if (i < MAXATTR) g_attr[i].enabled = 0; }
void glDrawArrays(GLenum mode, GLint first, GLsizei count) { if (getenv("DD_SHIM_DEBUG")) fprintf(stderr, "[shim] glDrawArrays(0x%x,%d,%d)\n", mode, first, count); g_draw_mode = mode; g_draw_first = first; g_draw_count = count; memcpy(g_attr_snap, g_attr, sizeof g_attr); g_have_draw_snap = 1; }

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
#define GL_DEPTH_BITS 0x0D56
#define GL_STENCIL_BITS 0x0D57
#define GL_RED_BITS 0x0D52
#define GL_SAMPLES 0x80A9
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
        case 0x821B: *v = 3; break; // GL_MAJOR_VERSION
        case 0x821C: *v = 0; break; // GL_MINOR_VERSION
        case 0x821D: *v = 0; break; // GL_NUM_EXTENSIONS (empty extension list)
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
        case 0x8905: *v = 8; break; // GL_MAX_SAMPLES
        case GL_DEPTH_BITS: *v = 24; break;
        case GL_STENCIL_BITS: *v = 8; break;
        case GL_RED_BITS: *v = 8; break;
        case GL_MAX_VIEWPORT_DIMS: v[0] = 4096; v[1] = 4096; break;
        case GL_VIEWPORT:
            v[0] = 0; v[1] = 0;
            v[2] = g_surf.width ? (GLint)g_surf.width : 256;
            v[3] = g_surf.height ? (GLint)g_surf.height : 256;
            break;
        default: *v = 0; break;
    }
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
void glBlendFunc(GLenum s, GLenum d) { (void)s; (void)d; }
void glBlendFuncSeparate(GLenum a, GLenum b, GLenum c, GLenum d) { (void)a; (void)b; (void)c; (void)d; }
void glBlendEquation(GLenum m) { (void)m; }
void glBlendEquationSeparate(GLenum a, GLenum b) { (void)a; (void)b; }
void glBlendColor(GLfloat r, GLfloat g, GLfloat b, GLfloat a) { (void)r; (void)g; (void)b; (void)a; }
void glFrontFace(GLenum m) { (void)m; }
void glCullFace(GLenum m) { (void)m; }
void glColorMask(GLboolean r, GLboolean g, GLboolean b, GLboolean a) { (void)r; (void)g; (void)b; (void)a; }
void glScissor(GLint x, GLint y, GLsizei w, GLsizei h) { (void)x; (void)y; (void)w; (void)h; }
void glLineWidth(GLfloat w) { (void)w; }
void glHint(GLenum t, GLenum m) { (void)t; (void)m; }
void glPolygonOffset(GLfloat a, GLfloat b) { (void)a; (void)b; }
void glSampleCoverage(GLfloat v, GLboolean i) { (void)v; (void)i; }
GLboolean glIsEnabled(GLenum c) { return (c == GL_DEPTH_TEST) ? (GLboolean)g_depth : 0; }
GLboolean glIsTexture(GLuint t) { return (t && t < MAXTEX && g_tex[t].used) ? 1 : 0; }
GLboolean glIsBuffer(GLuint b) { return (b && b < MAXBUF && g_buf[b].used) ? 1 : 0; }
GLboolean glIsProgram(GLuint p) { return (p && p < MAXPROG && g_prog[p].used) ? 1 : 0; }
GLboolean glIsShader(GLuint s) { return (s && s < MAXSH && g_sh[s].used) ? 1 : 0; }
void glUniformMatrix3fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 36); }
void glUniformMatrix2fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 16); }
void glUniform2fv(GLint l, GLsizei n, const GLfloat *v) { (void)n; uni_write(l, v, 8); }
void glUniform2f(GLint l, GLfloat a, GLfloat b) { GLfloat v[2] = {a, b}; uni_write(l, v, 8); }
void glUniform1fv(GLint l, GLsizei n, const GLfloat *v) { (void)n; uni_write(l, v, 4); }
void glUniform2i(GLint l, GLint a, GLint b) { GLint v[2] = {a, b}; uni_write(l, v, 8); }
void glUniform3i(GLint l, GLint a, GLint b, GLint cc) { GLint v[3] = {a, b, cc}; uni_write(l, v, 12); }
void glUniform1iv(GLint l, GLsizei n, const GLint *v) { (void)n; uni_write(l, v, 4); }
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
// ---- framebuffer/renderbuffer objects (stubs: our scenes render to the default framebuffer) ----
static GLuint g_fbo_seq = 1, g_rbo_seq = 1;
void glGenFramebuffers(GLsizei n, GLuint *f) { for (int i = 0; i < n; i++) f[i] = g_fbo_seq++; }
void glBindFramebuffer(GLenum t, GLuint f) { (void)t; (void)f; }
void glDeleteFramebuffers(GLsizei n, const GLuint *f) { (void)n; (void)f; }
GLenum glCheckFramebufferStatus(GLenum t) { (void)t; return 0x8CD5; /* GL_FRAMEBUFFER_COMPLETE */ }
void glFramebufferTexture2D(GLenum a, GLenum b, GLenum c, GLuint d, GLint e) { (void)a; (void)b; (void)c; (void)d; (void)e; }
void glFramebufferRenderbuffer(GLenum a, GLenum b, GLenum c, GLuint d) { (void)a; (void)b; (void)c; (void)d; }
void glGenRenderbuffers(GLsizei n, GLuint *r) { for (int i = 0; i < n; i++) r[i] = g_rbo_seq++; }
void glBindRenderbuffer(GLenum t, GLuint r) { (void)t; (void)r; }
void glDeleteRenderbuffers(GLsizei n, const GLuint *r) { (void)n; (void)r; }
void glRenderbufferStorage(GLenum a, GLenum b, GLsizei w, GLsizei h) { (void)a; (void)b; (void)w; (void)h; }
void glReadPixels(GLint x, GLint y, GLsizei w, GLsizei h, GLenum f, GLenum t, void *d) { (void)x; (void)y; (void)f; (void)t; if (d) memset(d, 0, (size_t)w * h * 4); }
const unsigned char *glGetStringi(GLenum n, GLuint i) { (void)n; (void)i; return (const unsigned char *)""; }

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
void glCopyTexImage2D(GLenum t, GLint l, GLenum ifmt, GLint x, GLint y, GLsizei w, GLsizei h, GLint b) { (void)t; (void)l; (void)ifmt; (void)x; (void)y; (void)w; (void)h; (void)b; }
void glCopyTexSubImage2D(GLenum t, GLint l, GLint xo, GLint yo, GLint x, GLint y, GLsizei w, GLsizei h) { (void)t; (void)l; (void)xo; (void)yo; (void)x; (void)y; (void)w; (void)h; }
void glTexParameterfv(GLenum target, GLenum p, const GLfloat *v) { if (v) glTexParameteri(target, p, (GLint)v[0]); }
void glTexParameteriv(GLenum target, GLenum p, const GLint *v) { if (v) glTexParameteri(target, p, v[0]); }
// Sub-image update: our glTexImage2D already re-uploads whole textures; a full-extent subimage at (0,0)
// covering the level is equivalent, so route those through it. Partial updates are a no-op for now (Chrome's
// first frame uploads full tile textures).
void glTexSubImage2D(GLenum target, GLint level, GLint xo, GLint yo, GLsizei w, GLsizei h, GLenum fmt, GLenum type, const void *pixels) {
    if (level == 0 && xo == 0 && yo == 0) {
        GLuint t = g_tex_unit[g_active_unit];
        if (t < MAXTEX && g_tex[t].used && (GLsizei)g_tex[t].w == w && (GLsizei)g_tex[t].h == h)
            glTexImage2D(target, 0, GL_RGBA, w, h, 0, fmt, type, pixels);
    }
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
void glGetBufferParameteriv(GLenum target, GLenum p, GLint *v) { (void)target; (void)p; if (v) *v = 0; }
void glGetFramebufferAttachmentParameteriv(GLenum target, GLenum att, GLenum p, GLint *v) { (void)target; (void)att; (void)p; if (v) *v = 0; }
void glGetRenderbufferParameteriv(GLenum target, GLenum p, GLint *v) { (void)target; (void)p; if (v) *v = 0; }
void glGetTexParameterfv(GLenum target, GLenum p, GLfloat *v) { (void)target; (void)p; if (v) *v = 0; }
void glGetTexParameteriv(GLenum target, GLenum p, GLint *v) { (void)target; (void)p; if (v) *v = 0; }
void glGetUniformfv(GLuint prog, GLint l, GLfloat *v) { (void)prog; (void)l; if (v) *v = 0; }
void glGetUniformiv(GLuint prog, GLint l, GLint *v) { (void)prog; (void)l; if (v) *v = 0; }
void glGetVertexAttribfv(GLuint idx, GLenum p, GLfloat *v) { (void)idx; (void)p; if (v) *v = 0; }
void glGetVertexAttribiv(GLuint idx, GLenum p, GLint *v) { (void)idx; (void)p; if (v) *v = 0; }
void glGetVertexAttribPointerv(GLuint idx, GLenum p, void **ptr) { (void)idx; (void)p; if (ptr) *ptr = 0; }
void glGetAttachedShaders(GLuint prog, GLsizei maxc, GLsizei *count, GLuint *shaders) { (void)prog; (void)maxc; (void)shaders; if (count) *count = 0; }
void glGetShaderSource(GLuint s, GLsizei bufSize, GLsizei *length, GLchar *source) { (void)s; if (length) *length = 0; if (source && bufSize) source[0] = 0; }
GLboolean glIsFramebuffer(GLuint f) { (void)f; return 0; }
GLboolean glIsRenderbuffer(GLuint r) { (void)r; return 0; }

// ---- GLES3 core completeness for ANGLE (gl-egl, ES3.0 context) ---------------------------------------
// With GL_VERSION="OpenGL ES 3.0", ANGLE resolves the ENTIRE ES3 core set and NULL-derefs on any missing
// pointer during ES3 caps init. Provide them all: queries return benign values; VAOs/samplers/queries/
// sync/transform-feedback are id-vending stubs; instanced/range draws route to the base draw; ES3 uniform
// variants route through uni_write; and glMapBufferRange/glUnmapBuffer are FUNCTIONAL (return a pointer into
// the bound buffer's storage) so Chrome's vertex/index uploads land in g_buf and reach the IR/Metal path.
typedef int64_t GLint64; typedef uint64_t GLuint64; typedef void *GLsync;
static GLuint g_vao_seq = 1, g_samp_seq = 1, g_query_seq = 1, g_xfb_seq = 1;
// VAOs (stub: our attribute state is global; a first frame binds one VAO and configures attribs on it).
void glGenVertexArrays(GLsizei n, GLuint *a) { for (int i = 0; i < n; i++) a[i] = g_vao_seq++; }
void glBindVertexArray(GLuint a) { (void)a; }
void glDeleteVertexArrays(GLsizei n, const GLuint *a) { (void)n; (void)a; }
GLboolean glIsVertexArray(GLuint a) { return a ? 1 : 0; }
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
void glVertexAttribIPointer(GLuint i, GLint size, GLenum type, GLsizei stride, const void *ptr) { glVertexAttribPointer(i, size, type, 0, stride, ptr); }
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
void glUniformMatrix2x3fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 24); }
void glUniformMatrix3x2fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 24); }
void glUniformMatrix2x4fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 32); }
void glUniformMatrix4x2fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 32); }
void glUniformMatrix3x4fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 48); }
void glUniformMatrix4x3fv(GLint l, GLsizei n, GLboolean t, const GLfloat *v) { (void)n; (void)t; uni_write(l, v, 48); }
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
void glGetInternalformativ(GLenum tgt, GLenum ifmt, GLenum pn, GLsizei bs, GLint *v) { (void)tgt; (void)ifmt; (void)pn; if (v && bs > 0) v[0] = 0; /* 0 sample counts */ }
// Program binary (stub: force source compile path).
void glGetProgramBinary(GLuint p, GLsizei bs, GLsizei *len, GLenum *fmt, void *bin) { (void)p; (void)bs; (void)bin; if (len) *len = 0; if (fmt) *fmt = 0; }
void glProgramBinary(GLuint p, GLenum fmt, const void *bin, GLsizei len) { (void)p; (void)fmt; (void)bin; (void)len; }
void glProgramParameteri(GLuint p, GLenum pn, GLint v) { (void)p; (void)pn; (void)v; }
// 3D / immutable / multisample texture + framebuffer ops (stub for a first 2D frame).
void glTexImage3D(GLenum t, GLint l, GLint ifmt, GLsizei w, GLsizei h, GLsizei d, GLint b, GLenum fmt, GLenum ty, const void *px) { (void)t; (void)l; (void)ifmt; (void)w; (void)h; (void)d; (void)b; (void)fmt; (void)ty; (void)px; }
void glTexSubImage3D(GLenum t, GLint l, GLint xo, GLint yo, GLint zo, GLsizei w, GLsizei h, GLsizei d, GLenum fmt, GLenum ty, const void *px) { (void)t; (void)l; (void)xo; (void)yo; (void)zo; (void)w; (void)h; (void)d; (void)fmt; (void)ty; (void)px; }
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
void glTexStorage3D(GLenum t, GLsizei levels, GLenum ifmt, GLsizei w, GLsizei h, GLsizei d) { (void)t; (void)levels; (void)ifmt; (void)w; (void)h; (void)d; }
void glBlitFramebuffer(GLint sx0, GLint sy0, GLint sx1, GLint sy1, GLint dx0, GLint dy0, GLint dx1, GLint dy1, GLbitfield mask, GLenum filter) { (void)sx0; (void)sy0; (void)sx1; (void)sy1; (void)dx0; (void)dy0; (void)dx1; (void)dy1; (void)mask; (void)filter; }
void glFramebufferTextureLayer(GLenum t, GLenum att, GLuint tex, GLint l, GLint layer) { (void)t; (void)att; (void)tex; (void)l; (void)layer; }
void glRenderbufferStorageMultisample(GLenum t, GLsizei s, GLenum ifmt, GLsizei w, GLsizei h) { (void)t; (void)s; (void)ifmt; (void)w; (void)h; }
void glInvalidateFramebuffer(GLenum t, GLsizei n, const GLenum *att) { (void)t; (void)n; (void)att; }
void glInvalidateSubFramebuffer(GLenum t, GLsizei n, const GLenum *att, GLint x, GLint y, GLsizei w, GLsizei h) { (void)t; (void)n; (void)att; (void)x; (void)y; (void)w; (void)h; }
void glClearBufferiv(GLenum b, GLint d, const GLint *v) { (void)b; (void)d; (void)v; }
void glClearBufferuiv(GLenum b, GLint d, const GLuint *v) { (void)b; (void)d; (void)v; }
void glClearBufferfv(GLenum b, GLint d, const GLfloat *v) { (void)b; (void)d; (void)v; }
void glClearBufferfi(GLenum b, GLint d, GLfloat depth, GLint stencil) { (void)b; (void)d; (void)depth; (void)stencil; }

// ======================= translator test tool (host build) =======================
// Build: cc -DDD_TR_TOOL gl_shim.c -o gl_tr ; run: gl_tr vertex.glsl fragment.glsl > out.metal
// Feeds real GLSL-ES through the SAME translate() the shim uses at glLinkProgram time, so the emitted MSL
// can be compiled (dd-display selftest-msl) to prove arbitrary app shaders (e.g. glmark2's) translate.
#ifdef DD_TR_TOOL
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
    if (argc < 3) { fprintf(stderr, "usage: %s vert.glsl frag.glsl\n", argv[0]); return 2; }
    char *vs = slurp(argv[1]), *fs = slurp(argv[2]);
    char *msl = translate(vs, fs);
    fputs(msl, stdout);
    return 0;
}
#endif
