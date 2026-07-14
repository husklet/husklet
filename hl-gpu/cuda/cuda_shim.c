/* dd's libcuda.so.1 — a real CUDA Driver API implementation that runs an app's PTX kernels on dd's
 * software backend, with NO NVIDIA GPU. The "show the Linux container a CUDA device, run the compute
 * on the host" seam (docs/ideas/CUDA_ON_METAL.md): dd substitutes libcuda (the ZLUDA seam) instead of
 * emulating the closed /dev/nvidia* ioctl ABI.
 *
 * ## What is real here
 * The full guest-facing Driver API path executes end-to-end:
 *   cuInit -> cuDeviceGet* -> cuCtxCreate -> cuMemAlloc -> cuMemcpyHtoD
 *          -> cuModuleLoadData(PTX) -> cuModuleGetFunction -> cuLaunchKernel -> cuMemcpyDtoH
 * and produces numerically correct results for the modeled PTX subset (vector-add / saxpy /
 * elementwise). Device pointers are real host memory (Apple-silicon unified memory / this oracle's
 * host RAM), so H2D/D2H are memcpy and a kernel dereferences device pointers directly.
 *
 * ## The embedded executor mirrors the Rust `dd-gpu` crate
 * The PTX interpreter below is a compact C mirror of the authoritative Rust executor in
 * `hl-gpu/src/ptx.rs` + `hl-gpu/src/software.rs` (same modeled SIMT subset). It exists so this guest
 * `.so` is *self-contained* for the in-process oracle milestone. On a real host, libcuda instead
 * marshals dd-GPU IR over the shared-memory ring to the host Metal executor (RENDERING_PLAN.md) — the
 * ring transport replaces this embedded interpreter, so the compute logic is not duplicated in
 * production. Unimplemented Driver-API tail returns CUDA_ERROR_NOT_SUPPORTED (never crashes).
 *
 * Device presence values are seeded from environment (dd's launcher sets these), matching dd-nvml:
 *   DD_CUDA_NAME  device name        (default "dd Metal (CUDA-sim) Device")
 *   DD_CUDA_CC    compute capability (default "8.6")
 *   DD_CUDA_VRAM  reported VRAM (MB)  (default 4096)
 */
#define _GNU_SOURCE
#include "cuda_min.h"
#include "fatbin.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <time.h>
#include <math.h>

/* =================================================================================================
 * global device / driver state
 * ================================================================================================= */
static int          g_inited = 0;
static char         g_name[128] = "dd Metal (CUDA-sim) Device";
static int          g_cc_major = 8, g_cc_minor = 6;
static unsigned long long g_vram = 4096ULL * 1024 * 1024;
static const int    g_driver_version = 12020; /* 12.2 -> maj*1000 + min*10 */

static void seed_from_env(void) {
    const char* n = getenv("DD_CUDA_NAME");
    if (n && *n) { strncpy(g_name, n, sizeof(g_name) - 1); g_name[sizeof(g_name) - 1] = 0; }
    const char* cc = getenv("DD_CUDA_CC");
    if (cc && *cc) { int a = 8, b = 6; if (sscanf(cc, "%d.%d", &a, &b) >= 1) { g_cc_major = a; g_cc_minor = b; } }
    const char* v = getenv("DD_CUDA_VRAM");
    if (v && *v) { unsigned long long mb = strtoull(v, NULL, 10); if (mb) g_vram = mb * 1024ULL * 1024ULL; }
}

/* opaque handles are backed by heap structs */
struct CUctx_st  { int alive; unsigned int flags; int is_primary; };
struct CUmod_st  { char* ptx; };
struct CUfunc_st { struct CUmod_st* mod; char entry[128]; int max_dyn_shared; };
struct CUstream_st { unsigned int flags; int priority; };
struct CUevent_st  { unsigned int flags; struct timespec ts; int recorded; };

/* ---- context stack (cuCtxPushCurrent / cuCtxPopCurrent) + primary context ---- */
#define CTX_STACK_MAX 64
static struct CUctx_st* g_ctx_stack[CTX_STACK_MAX];
static int              g_ctx_sp = 0;                 /* top-of-stack index (0 = empty) */
static struct CUctx_st* g_current_ctx = NULL;
static struct CUctx_st* g_primary_ctx = NULL;         /* device 0 primary context */
static int              g_primary_refcount = 0;

/* ---- context-scoped limits / cache config (semantically-correct minimal state) ---- */
static size_t       g_limits[8] = { 1024, 1024*1024, 8*1024*1024, 2, 2048, 128, 0, 0 };
static int          g_cache_config = 0;   /* CU_FUNC_CACHE_PREFER_NONE */
static int          g_shared_config = 0;

/* ---- allocation registry: tracks device/host/managed ranges so cuMemGetAddressRange,
 *      cuPointerGetAttribute(s), cuMemGetInfo and cuMemHostGetDevicePointer answer truthfully.
 *      On unified memory a CUdeviceptr IS the host pointer, so one table covers all kinds. ---- */
enum { ALLOC_DEVICE, ALLOC_HOST, ALLOC_MANAGED, ALLOC_REGISTERED };
typedef struct { void* base; size_t size; int kind; int live; } AllocRec;
#define MAX_ALLOCS 4096
static AllocRec g_allocs[MAX_ALLOCS];
static unsigned long long g_bytes_outstanding = 0;

static AllocRec* alloc_register(void* base, size_t size, int kind) {
    for (int i = 0; i < MAX_ALLOCS; i++) {
        if (!g_allocs[i].live) {
            g_allocs[i].base = base; g_allocs[i].size = size; g_allocs[i].kind = kind; g_allocs[i].live = 1;
            if (kind == ALLOC_DEVICE || kind == ALLOC_MANAGED) g_bytes_outstanding += size;
            return &g_allocs[i];
        }
    }
    return NULL; /* registry full: allocation still succeeds, just untracked */
}
static AllocRec* alloc_find(const void* p) {
    unsigned long long a = (unsigned long long)(size_t)p;
    for (int i = 0; i < MAX_ALLOCS; i++) {
        if (!g_allocs[i].live) continue;
        unsigned long long b = (unsigned long long)(size_t)g_allocs[i].base;
        if (a >= b && a < b + (g_allocs[i].size ? g_allocs[i].size : 1)) return &g_allocs[i];
    }
    return NULL;
}
static void alloc_unregister(void* base) {
    for (int i = 0; i < MAX_ALLOCS; i++) {
        if (g_allocs[i].live && g_allocs[i].base == base) {
            if (g_allocs[i].kind == ALLOC_DEVICE || g_allocs[i].kind == ALLOC_MANAGED)
                g_bytes_outstanding -= g_allocs[i].size;
            g_allocs[i].live = 0;
            return;
        }
    }
}
/* a stream handle is "real" (heap) vs a default-stream sentinel (NULL / LEGACY / PER_THREAD) */
static int stream_is_default(CUstream s) { return (size_t)s <= 2; }

/* =================================================================================================
 * PTX interpreter (mirror of hl-gpu/src/ptx.rs — modeled subset; flat unified addressing)
 * ================================================================================================= */
#define MAX_REG   512
#define MAX_INST  2048
#define MAX_PARAM 32
#define MAX_LABEL 256

enum {
    OP_RET, OP_NOP, OP_MOV_IMM, OP_MOV_REG, OP_MOV_SREG, OP_LDPARAM, OP_CVTA,
    OP_IADD, OP_ISUB, OP_IMUL, OP_IMAD, OP_SETP, OP_BRA, OP_LDG, OP_STG,
    OP_FADD, OP_FSUB, OP_FMUL, OP_FFMA, OP_CVT
};
/* special registers */
enum { SR_TID_X, SR_TID_Y, SR_TID_Z, SR_NTID_X, SR_NTID_Y, SR_NTID_Z,
       SR_CTAID_X, SR_CTAID_Y, SR_CTAID_Z, SR_NCTAID_X, SR_NCTAID_Y, SR_NCTAID_Z };
/* comparisons */
enum { CMP_EQ, CMP_NE, CMP_LT, CMP_LE, CMP_GT, CMP_GE };
/* global access type */
enum { GTY_F32, GTY_U32, GTY_U64 };
/* cvt kinds */
enum { CVT_F32_FROM_S32, CVT_S64_FROM_S32, CVT_S32_FROM_F32, CVT_IDENTITY };

/* operand: kind 0=reg,1=int imm,2=float bits */
typedef struct { int kind; long long i; } Operand;

typedef struct {
    int op, d, sreg, param, ty, cmp, wide, target, pred_reg, pred_neg, cvt_kind;
    int is_unsigned; /* setp.u* / mul.wide.u* : select unsigned semantics */
    long long off;
    Operand a, b, c;
} Instr;

typedef struct {
    int width, offset; /* param layout (natural aligned), in bytes */
} Param;

typedef struct {
    char entry[128];
    unsigned block[3];
    Param params[MAX_PARAM];
    int nparam, param_bytes;
    int reg_count;
    Instr insts[MAX_INST];
    int ninst;
    /* register name interning */
    char reg_names[MAX_REG][24];
    /* labels */
    char label_names[MAX_LABEL][48];
    int  label_idx[MAX_LABEL];
    int  nlabel;
    char param_names[MAX_PARAM][48];
} Program;

static int ptx_error = 0;

static int intern_reg(Program* p, const char* name) {
    for (int i = 0; i < p->reg_count; i++)
        if (strcmp(p->reg_names[i], name) == 0) return i;
    if (p->reg_count >= MAX_REG) { ptx_error = 1; return 0; }
    strncpy(p->reg_names[p->reg_count], name, 23);
    return p->reg_count++;
}

static int type_width(const char* ty) {
    if (!strcmp(ty, ".u8") || !strcmp(ty, ".s8") || !strcmp(ty, ".b8")) return 1;
    if (!strcmp(ty, ".u16") || !strcmp(ty, ".s16") || !strcmp(ty, ".b16") || !strcmp(ty, ".f16")) return 2;
    if (!strcmp(ty, ".u32") || !strcmp(ty, ".s32") || !strcmp(ty, ".b32") || !strcmp(ty, ".f32")) return 4;
    if (!strcmp(ty, ".u64") || !strcmp(ty, ".s64") || !strcmp(ty, ".b64") || !strcmp(ty, ".f64")) return 8;
    return 0;
}

/* strip leading '%' */
static const char* strip_reg(const char* t) { return (*t == '%') ? t + 1 : t; }

static int parse_sreg(const char* t) {
    t = strip_reg(t);
    struct { const char* n; int v; } tab[] = {
        {"tid.x", SR_TID_X}, {"tid.y", SR_TID_Y}, {"tid.z", SR_TID_Z},
        {"ntid.x", SR_NTID_X}, {"ntid.y", SR_NTID_Y}, {"ntid.z", SR_NTID_Z},
        {"ctaid.x", SR_CTAID_X}, {"ctaid.y", SR_CTAID_Y}, {"ctaid.z", SR_CTAID_Z},
        {"nctaid.x", SR_NCTAID_X}, {"nctaid.y", SR_NCTAID_Y}, {"nctaid.z", SR_NCTAID_Z},
    };
    for (size_t i = 0; i < sizeof(tab) / sizeof(tab[0]); i++)
        if (!strcmp(t, tab[i].n)) return tab[i].v;
    return -1;
}

static long long parse_int(const char* t) {
    while (isspace((unsigned char)*t)) t++;
    if (t[0] == '0' && (t[1] == 'x' || t[1] == 'X')) return (long long)strtoull(t + 2, NULL, 16);
    return strtoll(t, NULL, 10);
}
static unsigned parse_float_bits(const char* t) {
    while (isspace((unsigned char)*t)) t++;
    if (t[0] == '0' && (t[1] == 'f' || t[1] == 'F')) return (unsigned)strtoul(t + 2, NULL, 16);
    float f = strtof(t, NULL);
    unsigned u; memcpy(&u, &f, 4); return u;
}
static int is_reg(const char* t) { return t[0] == '%'; }

static Operand make_op(Program* p, const char* t, int want_float) {
    Operand o;
    while (isspace((unsigned char)*t)) t++;
    if (is_reg(t)) { o.kind = 0; o.i = intern_reg(p, strip_reg(t)); }
    else if (want_float) { o.kind = 2; o.i = (long long)parse_float_bits(t); }
    else { o.kind = 1; o.i = parse_int(t); }
    return o;
}

/* parse "[%reg]" / "[%reg+off]" / "[name]" -> writes reg index (or -1 for a name) + offset/name */
static void parse_mem(Program* p, const char* t, int* reg, long long* off, char* name_out) {
    char buf[96]; int j = 0;
    for (const char* s = t; *s && j < 95; s++) if (*s != '[' && *s != ']') buf[j++] = *s;
    buf[j] = 0;
    char* trim = buf; while (isspace((unsigned char)*trim)) trim++;
    if (*trim == '%') {
        char* plus = strpbrk(trim, "+-");
        if (plus) { *off = parse_int(plus); char save = *plus; *plus = 0; *reg = intern_reg(p, strip_reg(trim)); *plus = save; }
        else { *off = 0; char* e = trim; while (*e && !isspace((unsigned char)*e)) e++; *e = 0; *reg = intern_reg(p, strip_reg(trim)); }
        if (name_out) name_out[0] = 0;
    } else {
        *reg = -1; *off = 0;
        char* e = trim; while (*e && (isalnum((unsigned char)*e) || *e == '_' || *e == '$')) e++; *e = 0;
        if (name_out) { int k = 0; while (trim[k] && k < 47) { name_out[k] = trim[k]; k++; } name_out[k] = 0; }
    }
}

/* split an operand list on commas into up to 4 trimmed tokens */
static int split_ops(char* s, char* out[4]) {
    int n = 0; char* tok = strtok(s, ",");
    while (tok && n < 4) { while (isspace((unsigned char)*tok)) tok++;
        char* e = tok + strlen(tok); while (e > tok && isspace((unsigned char)e[-1])) *--e = 0;
        out[n++] = tok; tok = strtok(NULL, ","); }
    return n;
}

static int opcode_has(const char* op, const char* m) {
    char tmp[64]; strncpy(tmp, op, 63); tmp[63] = 0;
    for (char* t = strtok(tmp, "."); t; t = strtok(NULL, ".")) if (!strcmp(t, m)) return 1;
    return 0;
}
static int gtype_of(const char* op) {
    if (strstr(op, "f32")) return GTY_F32;
    if (strstr(op, "64")) return GTY_U64;
    return GTY_U32;
}

/* Find the entry body and param list; returns malloc'd body string (caller frees) or NULL. */
static char* extract_entry(const char* src, const char* entry, char* params_out, size_t plen) {
    const char* s = src;
    while ((s = strstr(s, ".entry")) != NULL) {
        const char* a = s + 6;
        while (isspace((unsigned char)*a)) a++;
        char name[128]; int j = 0;
        while (a[j] && (isalnum((unsigned char)a[j]) || a[j] == '_' || a[j] == '$') && j < 127) { name[j] = a[j]; j++; }
        name[j] = 0;
        if (strcmp(name, entry) == 0) {
            const char* lp = strchr(s, '(');
            const char* lb = strchr(s, '{');
            if (!lb) return NULL;
            params_out[0] = 0;
            if (lp && lp < lb) {
                const char* rp = strchr(lp, ')');
                if (rp) { size_t n = (size_t)(rp - lp - 1); if (n >= plen) n = plen - 1; memcpy(params_out, lp + 1, n); params_out[n] = 0; }
            }
            /* matched braces from lb */
            int depth = 0; const char* body_start = NULL; const char* p = lb;
            for (; *p; p++) {
                if (*p == '{') { if (depth == 0) body_start = p + 1; depth++; }
                else if (*p == '}') { depth--; if (depth == 0) { size_t n = (size_t)(p - body_start); char* out = malloc(n + 1); memcpy(out, body_start, n); out[n] = 0; return out; } }
            }
            return NULL;
        }
        s = a;
    }
    return NULL;
}

static int find_label(Program* p, const char* name) {
    for (int i = 0; i < p->nlabel; i++) if (!strcmp(p->label_names[i], name)) return p->label_idx[i];
    return -1;
}

/* Compile PTX text for `entry` into `prog`. Returns 0 on success, nonzero on malformed/unsupported. */
static int ptx_compile(const char* src, const char* entry, unsigned block[3], Program* prog) {
    memset(prog, 0, sizeof(*prog));
    ptx_error = 0;
    snprintf(prog->entry, sizeof(prog->entry), "%s", entry);
    prog->block[0] = block[0]; prog->block[1] = block[1]; prog->block[2] = block[2];

    char params_src[1024];
    char* body = extract_entry(src, entry, params_src, sizeof(params_src));
    if (!body) return 1;

    /* params: ".param .TYPE name" comma-separated */
    { char tmp[1024]; strncpy(tmp, params_src, sizeof(tmp) - 1); tmp[sizeof(tmp) - 1] = 0;
      char* save; char* item = strtok_r(tmp, ",", &save);
      int off = 0;
      while (item) {
          while (isspace((unsigned char)*item)) item++;
          if (*item) {
              char decl[256]; strncpy(decl, item, 255); decl[255] = 0;
              char* w; char* t = strtok_r(decl, " \t\n\r", &w);
              const char* ty = NULL; char nm[64] = {0};
              int seen_param = 0;
              while (t) {
                  if (!strcmp(t, ".param")) seen_param = 1;
                  else if (t[0] == '.') ty = t;
                  else { strncpy(nm, t, 63); }
                  t = strtok_r(NULL, " \t\n\r", &w);
              }
              if (seen_param && ty && nm[0] && prog->nparam < MAX_PARAM) {
                  int width = type_width(ty);
                  if (!width || strchr(nm, '[')) { free(body); return 1; }
                  if (off % width) off += width - (off % width);
                  prog->params[prog->nparam].width = width;
                  prog->params[prog->nparam].offset = off;
                  snprintf(prog->param_names[prog->nparam], sizeof(prog->param_names[0]), "%s", nm);
                  prog->nparam++;
                  off += width;
              }
          }
          item = strtok_r(NULL, ",", &save);
      }
      prog->param_bytes = off;
    }

    /* Split body into statements (on ';'), peel labels, drop directives. Two passes so branch labels
       resolve: pass 1 records label->index and stashes statement strings; pass 2 parses. */
    static char stmts[MAX_INST][256];
    int nstmt = 0;
    { char* body2 = strdup(body);
      for (char* c = body2; *c; c++) if (*c == '\n' || *c == '\r' || *c == '\t') *c = ' ';
      char* save; char* st = strtok_r(body2, ";", &save);
      while (st) {
          while (isspace((unsigned char)*st)) st++;
          /* peel leading "label:" tokens */
          for (;;) {
              char* sp = st; while (*sp && !isspace((unsigned char)*sp) && *sp != ':') sp++;
              if (*sp == ':') {
                  size_t ln = (size_t)(sp - st);
                  if (prog->nlabel < MAX_LABEL) { memcpy(prog->label_names[prog->nlabel], st, ln); prog->label_names[prog->nlabel][ln] = 0; prog->label_idx[prog->nlabel] = nstmt; prog->nlabel++; }
                  st = sp + 1; while (isspace((unsigned char)*st)) st++;
              } else break;
          }
          if (*st && *st != '.' && nstmt < MAX_INST) { strncpy(stmts[nstmt], st, 255); stmts[nstmt][255] = 0; nstmt++; }
          st = strtok_r(NULL, ";", &save);
      }
      free(body2);
    }
    free(body);

    /* pass 2: parse each statement into an Instr */
    for (int si = 0; si < nstmt; si++) {
        Instr in; memset(&in, 0, sizeof(in)); in.pred_reg = -1;
        char line[256]; strncpy(line, stmts[si], 255); line[255] = 0;
        char* rest = line; while (isspace((unsigned char)*rest)) rest++;

        /* predicate guard @%p / @!%p */
        if (*rest == '@') {
            rest++; int neg = 0; if (*rest == '!') { neg = 1; rest++; }
            char* sp = rest; while (*sp && !isspace((unsigned char)*sp)) sp++;
            char save = *sp; *sp = 0; in.pred_reg = intern_reg(prog, strip_reg(rest)); in.pred_neg = neg; *sp = save;
            rest = sp; while (isspace((unsigned char)*rest)) rest++;
        }

        /* opcode = first token */
        char* sp = rest; while (*sp && !isspace((unsigned char)*sp)) sp++;
        char opcode[64]; size_t ol = (size_t)(sp - rest); if (ol > 63) ol = 63; memcpy(opcode, rest, ol); opcode[ol] = 0;
        char* argstr = sp; while (isspace((unsigned char)*argstr)) argstr++;
        char argbuf[256]; strncpy(argbuf, argstr, 255); argbuf[255] = 0;
        char* ops[4]; int nops = split_ops(argbuf, ops);

        char base[32]; { size_t i = 0; while (opcode[i] && opcode[i] != '.' && i < 31) { base[i] = opcode[i]; i++; } base[i] = 0; }

        if (!strcmp(base, "ret")) in.op = OP_RET;
        else if (!strcmp(base, "bar")) in.op = OP_NOP;
        else if (!strcmp(base, "bra")) {
            in.op = OP_BRA;
            if (nops < 1) { return 1; }
            int idx = find_label(prog, ops[0]);
            if (idx < 0) return 1;
            in.target = idx;
        }
        else if (!strcmp(base, "mov")) {
            if (nops < 2) return 1;
            in.d = intern_reg(prog, strip_reg(ops[0]));
            int sr = parse_sreg(ops[1]);
            if (sr >= 0) { in.op = OP_MOV_SREG; in.sreg = sr; }
            else if (is_reg(ops[1])) { in.op = OP_MOV_REG; in.a = make_op(prog, ops[1], 0); }
            else if (opcode_has(opcode, "f32")) { in.op = OP_MOV_IMM; in.a.kind = 2; in.a.i = parse_float_bits(ops[1]); }
            else { in.op = OP_MOV_IMM; in.a.kind = 1; in.a.i = parse_int(ops[1]); }
        }
        else if (!strcmp(base, "ld")) {
            if (nops < 2) return 1;
            in.d = intern_reg(prog, strip_reg(ops[0]));
            if (opcode_has(opcode, "param")) {
                char nm[64]; int r; long long o; parse_mem(prog, ops[1], &r, &o, nm);
                int pi = -1; for (int k = 0; k < prog->nparam; k++) if (!strcmp(prog->param_names[k], nm)) pi = k;
                if (pi < 0) return 1;
                in.op = OP_LDPARAM; in.param = pi;
            } else if (opcode_has(opcode, "global")) {
                int r; long long o; parse_mem(prog, ops[1], &r, &o, NULL);
                in.op = OP_LDG; in.a.kind = 0; in.a.i = r; in.off = o; in.ty = gtype_of(opcode);
            } else return 1;
        }
        else if (!strcmp(base, "st")) {
            if (nops < 2) return 1;
            if (!opcode_has(opcode, "global")) return 1;
            int r; long long o; parse_mem(prog, ops[0], &r, &o, NULL);
            in.op = OP_STG; in.a.kind = 0; in.a.i = r; in.off = o; in.ty = gtype_of(opcode);
            in.b = make_op(prog, ops[1], opcode_has(opcode, "f32"));
        }
        else if (!strcmp(base, "cvta")) {
            if (nops < 2) return 1;
            in.op = OP_CVTA; in.d = intern_reg(prog, strip_reg(ops[0])); in.a = make_op(prog, ops[1], 0);
        }
        else if (!strcmp(base, "add") || !strcmp(base, "sub")) {
            if (nops < 3) return 1;
            in.d = intern_reg(prog, strip_reg(ops[0]));
            int isf = opcode_has(opcode, "f32");
            in.a = make_op(prog, ops[1], isf); in.b = make_op(prog, ops[2], isf);
            in.wide = opcode_has(opcode, "s64") || opcode_has(opcode, "u64") || opcode_has(opcode, "b64");
            in.op = !strcmp(base, "add") ? (isf ? OP_FADD : OP_IADD) : (isf ? OP_FSUB : OP_ISUB);
        }
        else if (!strcmp(base, "mul")) {
            if (nops < 3) return 1;
            in.d = intern_reg(prog, strip_reg(ops[0]));
            int isf = opcode_has(opcode, "f32");
            in.a = make_op(prog, ops[1], isf); in.b = make_op(prog, ops[2], isf);
            in.wide = opcode_has(opcode, "wide");
            in.is_unsigned = opcode_has(opcode, "u32") || opcode_has(opcode, "u16") || opcode_has(opcode, "u8");
            in.op = isf ? OP_FMUL : OP_IMUL;
        }
        else if (!strcmp(base, "mad")) {
            if (nops < 4) return 1;
            in.op = OP_IMAD; in.d = intern_reg(prog, strip_reg(ops[0]));
            in.a = make_op(prog, ops[1], 0); in.b = make_op(prog, ops[2], 0); in.c = make_op(prog, ops[3], 0);
        }
        else if (!strcmp(base, "fma")) {
            if (nops < 4) return 1;
            in.op = OP_FFMA; in.d = intern_reg(prog, strip_reg(ops[0]));
            in.a = make_op(prog, ops[1], 1); in.b = make_op(prog, ops[2], 1); in.c = make_op(prog, ops[3], 1);
        }
        else if (!strcmp(base, "setp")) {
            if (nops < 3) return 1;
            in.op = OP_SETP; in.d = intern_reg(prog, strip_reg(ops[0]));
            in.cmp = opcode_has(opcode, "eq") ? CMP_EQ : opcode_has(opcode, "ne") ? CMP_NE :
                     opcode_has(opcode, "lt") ? CMP_LT : opcode_has(opcode, "le") ? CMP_LE :
                     opcode_has(opcode, "gt") ? CMP_GT : opcode_has(opcode, "ge") ? CMP_GE : -1;
            if (in.cmp < 0) return 1;
            in.is_unsigned = opcode_has(opcode, "u32") || opcode_has(opcode, "u64") || opcode_has(opcode, "u16") || opcode_has(opcode, "u8");
            in.a = make_op(prog, ops[1], 0); in.b = make_op(prog, ops[2], 0);
        }
        else if (!strcmp(base, "cvt")) {
            if (nops < 2) return 1;
            in.op = OP_CVT; in.d = intern_reg(prog, strip_reg(ops[0]));
            in.a = make_op(prog, ops[1], opcode_has(opcode, "f32"));
            in.cvt_kind = CVT_IDENTITY;
            /* Classify by the ORDER of type tokens: cvt.<rnd>?.<dst>.<src>. Mirrors ptx.rs. Using the
               token order (not strstr("rn")) avoids misreading a rounding mode like `rni` in
               `cvt.rni.s32.f32` as the "float dest" signal. */
            { char tt[64]; strncpy(tt, opcode, 63); tt[63] = 0;
              char dt[8] = {0}, st[8] = {0}; int tn = 0;
              for (char* t = strtok(tt, "."); t; t = strtok(NULL, ".")) {
                  if (!strcmp(t, "cvt")) continue;
                  if (t[0] == 'f' || t[0] == 's' || t[0] == 'u') {
                      if (tn == 0) { strncpy(dt, t, 7); tn = 1; }
                      else { strncpy(st, t, 7); tn = 2; break; }
                  }
              }
              if (dt[0] == 'f' && st[0] == 's') in.cvt_kind = CVT_F32_FROM_S32;
              else if (!strcmp(dt, "s64") && st[0] == 's') in.cvt_kind = CVT_S64_FROM_S32;
              else if (dt[0] == 's' && st[0] == 'f') in.cvt_kind = CVT_S32_FROM_F32;
            }
        }
        else return 1; /* unsupported opcode -> malformed for our subset */

        if (ptx_error) return 1;
        prog->insts[prog->ninst++] = in;
    }
    prog->reg_count = prog->reg_count; /* interner already sized it */
    return 0;
}

/* ---- interpreter ---- */
static unsigned long long reg_u64(unsigned long long* r, Operand* o) {
    if (o->kind == 0) return r[o->i];
    return (unsigned long long)o->i;
}
static float reg_f32(unsigned long long* r, Operand* o) {
    unsigned bits = (o->kind == 2) ? (unsigned)o->i : (unsigned)(reg_u64(r, o) & 0xffffffffu);
    float f; memcpy(&f, &bits, 4); return f;
}
static unsigned long long f32_bits(float f) { unsigned u; memcpy(&u, &f, 4); return (unsigned long long)u; }

static void run_thread(Program* p, const unsigned char* blob, const unsigned sr[12]) {
    unsigned long long r[MAX_REG];
    for (int i = 0; i < p->reg_count; i++) r[i] = 0;
    int pc = 0; long long steps = 0;
    while (pc < p->ninst) {
        if (++steps > 5000000) return;
        Instr* in = &p->insts[pc];
        switch (in->op) {
            case OP_RET: return;
            case OP_NOP: break;
            case OP_MOV_IMM: r[in->d] = (unsigned long long)in->a.i; break;
            case OP_MOV_REG: r[in->d] = reg_u64(r, &in->a); break;
            case OP_MOV_SREG: r[in->d] = sr[in->sreg]; break;
            case OP_CVTA: r[in->d] = reg_u64(r, &in->a); break;
            case OP_LDPARAM: {
                Param* pm = &p->params[in->param];
                unsigned long long v = 0; memcpy(&v, blob + pm->offset, pm->width); r[in->d] = v;
            } break;
            case OP_IADD: case OP_ISUB: {
                /* unsigned arithmetic → defined two's-complement wraparound (matches ptx.rs wrapping_add). */
                unsigned long long a = reg_u64(r, &in->a), b = reg_u64(r, &in->b);
                unsigned long long v = (in->op == OP_IADD) ? a + b : a - b;
                r[in->d] = in->wide ? v : (unsigned long long)(unsigned)v;
            } break;
            case OP_IMUL: {
                if (in->wide) {
                    if (in->is_unsigned) { /* mul.wide.u32: zero-extend */
                        unsigned a = (unsigned)reg_u64(r, &in->a), b = (unsigned)reg_u64(r, &in->b);
                        r[in->d] = (unsigned long long)a * (unsigned long long)b;
                    } else {               /* mul.wide.s32: sign-extend */
                        int a = (int)reg_u64(r, &in->a), b = (int)reg_u64(r, &in->b);
                        r[in->d] = (unsigned long long)((long long)a * (long long)b);
                    }
                } else {                   /* mul.lo: low 32 bits (sign-agnostic); unsigned avoids UB. */
                    unsigned a = (unsigned)reg_u64(r, &in->a), b = (unsigned)reg_u64(r, &in->b);
                    r[in->d] = (unsigned long long)(unsigned)(a * b);
                }
            } break;
            case OP_IMAD: {
                /* mad.lo: low 32 bits. unsigned arithmetic → defined wraparound (matches ptx.rs). */
                unsigned a = (unsigned)reg_u64(r, &in->a), b = (unsigned)reg_u64(r, &in->b), c = (unsigned)reg_u64(r, &in->c);
                r[in->d] = (unsigned long long)(unsigned)(a * b + c);
            } break;
            case OP_SETP: {
                int t = 0;
                if (in->is_unsigned) {
                    unsigned a = (unsigned)reg_u64(r, &in->a), b = (unsigned)reg_u64(r, &in->b);
                    switch (in->cmp) { case CMP_EQ: t = a == b; break; case CMP_NE: t = a != b; break;
                        case CMP_LT: t = a < b; break; case CMP_LE: t = a <= b; break;
                        case CMP_GT: t = a > b; break; default: t = a >= b; }
                } else {
                    int a = (int)reg_u64(r, &in->a), b = (int)reg_u64(r, &in->b);
                    switch (in->cmp) { case CMP_EQ: t = a == b; break; case CMP_NE: t = a != b; break;
                        case CMP_LT: t = a < b; break; case CMP_LE: t = a <= b; break;
                        case CMP_GT: t = a > b; break; default: t = a >= b; }
                }
                r[in->d] = t ? 1 : 0;
            } break;
            case OP_BRA: {
                int take = 1;
                if (in->pred_reg >= 0) { int v = r[in->pred_reg] != 0; take = in->pred_neg ? !v : v; }
                if (take) { pc = in->target; continue; }
            } break;
            case OP_LDG: {
                unsigned long long addr = r[in->a.i] + (unsigned long long)in->off;
                if (in->ty == GTY_F32) { float f; memcpy(&f, (void*)addr, 4); r[in->d] = f32_bits(f); }
                else if (in->ty == GTY_U32) { unsigned u; memcpy(&u, (void*)addr, 4); r[in->d] = u; }
                else { unsigned long long u; memcpy(&u, (void*)addr, 8); r[in->d] = u; }
            } break;
            case OP_STG: {
                unsigned long long addr = r[in->a.i] + (unsigned long long)in->off;
                if (in->ty == GTY_F32) { float f = reg_f32(r, &in->b); memcpy((void*)addr, &f, 4); }
                else if (in->ty == GTY_U32) { unsigned u = (unsigned)reg_u64(r, &in->b); memcpy((void*)addr, &u, 4); }
                else { unsigned long long u = reg_u64(r, &in->b); memcpy((void*)addr, &u, 8); }
            } break;
            case OP_FADD: r[in->d] = f32_bits(reg_f32(r, &in->a) + reg_f32(r, &in->b)); break;
            case OP_FSUB: r[in->d] = f32_bits(reg_f32(r, &in->a) - reg_f32(r, &in->b)); break;
            case OP_FMUL: r[in->d] = f32_bits(reg_f32(r, &in->a) * reg_f32(r, &in->b)); break;
            /* fma.rn.f32 is a SINGLE-rounding fused multiply-add (matches ptx.rs f32::mul_add). An
               unfused `a*b + c` double-rounds and diverges (e.g. 0.1f*10-1 → 0 unfused vs ~1.49e-8 fused). */
            case OP_FFMA: r[in->d] = f32_bits(fmaf(reg_f32(r, &in->a), reg_f32(r, &in->b), reg_f32(r, &in->c))); break;
            case OP_CVT: {
                if (in->cvt_kind == CVT_F32_FROM_S32) r[in->d] = f32_bits((float)(int)reg_u64(r, &in->a));
                else if (in->cvt_kind == CVT_S64_FROM_S32) r[in->d] = (unsigned long long)(long long)(int)reg_u64(r, &in->a);
                else if (in->cvt_kind == CVT_S32_FROM_F32) r[in->d] = (unsigned long long)(unsigned)(int)reg_f32(r, &in->a);
                else r[in->d] = reg_u64(r, &in->a);
            } break;
        }
        pc++;
    }
}

/* Execute a compiled program over the whole launch grid on the CPU (blocks x threads). */
static void ptx_execute(Program* p, const unsigned char* blob, unsigned grid[3]) {
    unsigned bx = p->block[0] ? p->block[0] : 1, by = p->block[1] ? p->block[1] : 1, bz = p->block[2] ? p->block[2] : 1;
    unsigned gx = grid[0] ? grid[0] : 1, gy = grid[1] ? grid[1] : 1, gz = grid[2] ? grid[2] : 1;
    for (unsigned cz = 0; cz < gz; cz++)
    for (unsigned cy = 0; cy < gy; cy++)
    for (unsigned cx = 0; cx < gx; cx++)
    for (unsigned tz = 0; tz < bz; tz++)
    for (unsigned ty = 0; ty < by; ty++)
    for (unsigned tx = 0; tx < bx; tx++) {
        unsigned sr[12] = { tx, ty, tz, bx, by, bz, cx, cy, cz, gx, gy, gz };
        run_thread(p, blob, sr);
    }
}

/* =================================================================================================
 * CUDA Driver API surface
 *
 * Tiering (see the header comment + the task brief):
 *   TIER 1 — fully real, backed by the embedded software backend (init/device/context/module/memory/
 *            launch/stream/event). These execute PTX end-to-end and mutate real host memory.
 *   TIER 2 — semantically-correct minimal impls for dd's model (limits, cache config, occupancy,
 *            pointer attributes, address range, error strings, memGetInfo, peer-can-access=false).
 *   TIER 3 — present-but-honest stubs: the symbol is EXPORTED (so dlsym / cuGetProcAddress resolve and
 *            a version script matches real libcuda) and returns the spec-correct error for an op the
 *            dd-gpu model cannot serve — CUDA_ERROR_NOT_SUPPORTED (graphs, IPC, tex/surf, peer,
 *            external interop, VMM, mempools, GL/VK/EGL/VDPAU), or CUDA_ERROR_INVALID_IMAGE for a
 *            cubin/fatbin (dd executes PTX only). Never a crash, never a fake success.
 * ================================================================================================= */
#define REQUIRE_INIT() do { if (!g_inited) return CUDA_ERROR_NOT_INITIALIZED; } while (0)

/* fixed device identity (matches hl-gpu/src/cuda.rs CudaDeviceDesc::apple_default) */
static const unsigned char g_uuid[16] = { 'd','d','-','m','e','t','a','l','-','c','u','d','a',0,0,0 };

/* -------------------------------------------------------------------------------------------------
 * init / version / error strings
 * ------------------------------------------------------------------------------------------------- */
CUresult cuInit(unsigned int Flags) { (void)Flags; if (!g_inited) { seed_from_env(); g_inited = 1; } return CUDA_SUCCESS; }
CUresult cuDriverGetVersion(int* v) { if (!v) return CUDA_ERROR_INVALID_VALUE; *v = g_driver_version; return CUDA_SUCCESS; }

CUresult cuGetErrorString(CUresult err, const char** str) {
    if (!str) return CUDA_ERROR_INVALID_VALUE;
    const char* s;
    switch (err) {
        case CUDA_SUCCESS: s = "no error"; break;
        case CUDA_ERROR_INVALID_VALUE: s = "invalid argument"; break;
        case CUDA_ERROR_OUT_OF_MEMORY: s = "out of memory"; break;
        case CUDA_ERROR_NOT_INITIALIZED: s = "initialization error"; break;
        case CUDA_ERROR_DEINITIALIZED: s = "driver shutting down"; break;
        case CUDA_ERROR_PROFILER_DISABLED: s = "profiler disabled while using external profiling tool"; break;
        case CUDA_ERROR_NO_DEVICE: s = "no CUDA-capable device is detected"; break;
        case CUDA_ERROR_INVALID_DEVICE: s = "invalid device ordinal"; break;
        case CUDA_ERROR_DEVICE_UNAVAILABLE: s = "device unavailable"; break;
        case CUDA_ERROR_INVALID_IMAGE: s = "device kernel image is invalid"; break;
        case CUDA_ERROR_INVALID_CONTEXT: s = "invalid device context"; break;
        case CUDA_ERROR_CONTEXT_ALREADY_CURRENT: s = "context is already current"; break;
        case CUDA_ERROR_MAP_FAILED: s = "mapping of buffer object failed"; break;
        case CUDA_ERROR_UNMAP_FAILED: s = "unmapping of buffer object failed"; break;
        case CUDA_ERROR_NO_BINARY_FOR_GPU: s = "no kernel image is available for execution on the device"; break;
        case CUDA_ERROR_INVALID_PTX: s = "a PTX JIT compilation failed"; break;
        case CUDA_ERROR_UNSUPPORTED_PTX_VERSION: s = "the provided PTX was compiled with an unsupported toolchain"; break;
        case CUDA_ERROR_INVALID_SOURCE: s = "device kernel source is invalid"; break;
        case CUDA_ERROR_FILE_NOT_FOUND: s = "file not found"; break;
        case CUDA_ERROR_OPERATING_SYSTEM: s = "OS call failed or operation not supported on this OS"; break;
        case CUDA_ERROR_INVALID_HANDLE: s = "invalid resource handle"; break;
        case CUDA_ERROR_ILLEGAL_STATE: s = "the operation cannot be performed in the present state"; break;
        case CUDA_ERROR_NOT_FOUND: s = "named symbol not found"; break;
        case CUDA_ERROR_NOT_READY: s = "device not ready"; break;
        case CUDA_ERROR_ILLEGAL_ADDRESS: s = "an illegal memory access was encountered"; break;
        case CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES: s = "too many resources requested for launch"; break;
        case CUDA_ERROR_LAUNCH_TIMEOUT: s = "the launch timed out and was terminated"; break;
        case CUDA_ERROR_LAUNCH_FAILED: s = "unspecified launch failure"; break;
        case CUDA_ERROR_PEER_ACCESS_UNSUPPORTED: s = "peer access is not supported between these two devices"; break;
        case CUDA_ERROR_PEER_ACCESS_ALREADY_ENABLED: s = "peer access is already enabled"; break;
        case CUDA_ERROR_PEER_ACCESS_NOT_ENABLED: s = "peer access has not been enabled"; break;
        case CUDA_ERROR_PRIMARY_CONTEXT_ACTIVE: s = "the primary context for the device is already active"; break;
        case CUDA_ERROR_CONTEXT_IS_DESTROYED: s = "the context is no longer usable"; break;
        case CUDA_ERROR_ASSERT: s = "device-side assert triggered"; break;
        case CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED: s = "part or all of the requested memory range is already mapped"; break;
        case CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED: s = "pointer does not correspond to a registered memory region"; break;
        case CUDA_ERROR_NOT_PERMITTED: s = "operation not permitted"; break;
        case CUDA_ERROR_NOT_SUPPORTED: s = "operation not supported"; break;
        case CUDA_ERROR_SYSTEM_NOT_READY: s = "system not yet initialized"; break;
        case CUDA_ERROR_STREAM_CAPTURE_UNSUPPORTED: s = "operation not permitted when stream is capturing"; break;
        case CUDA_ERROR_TIMEOUT: s = "the wait operation has timed out"; break;
        case CUDA_ERROR_UNKNOWN: s = "unknown error"; break;
        default: *str = NULL; return CUDA_ERROR_INVALID_VALUE; /* not a valid CUresult */
    }
    *str = s; return CUDA_SUCCESS;
}
CUresult cuGetErrorName(CUresult err, const char** str) {
    if (!str) return CUDA_ERROR_INVALID_VALUE;
    const char* s;
    switch (err) {
        case CUDA_SUCCESS: s = "CUDA_SUCCESS"; break;
        case CUDA_ERROR_INVALID_VALUE: s = "CUDA_ERROR_INVALID_VALUE"; break;
        case CUDA_ERROR_OUT_OF_MEMORY: s = "CUDA_ERROR_OUT_OF_MEMORY"; break;
        case CUDA_ERROR_NOT_INITIALIZED: s = "CUDA_ERROR_NOT_INITIALIZED"; break;
        case CUDA_ERROR_DEINITIALIZED: s = "CUDA_ERROR_DEINITIALIZED"; break;
        case CUDA_ERROR_PROFILER_DISABLED: s = "CUDA_ERROR_PROFILER_DISABLED"; break;
        case CUDA_ERROR_NO_DEVICE: s = "CUDA_ERROR_NO_DEVICE"; break;
        case CUDA_ERROR_INVALID_DEVICE: s = "CUDA_ERROR_INVALID_DEVICE"; break;
        case CUDA_ERROR_DEVICE_UNAVAILABLE: s = "CUDA_ERROR_DEVICE_UNAVAILABLE"; break;
        case CUDA_ERROR_INVALID_IMAGE: s = "CUDA_ERROR_INVALID_IMAGE"; break;
        case CUDA_ERROR_INVALID_CONTEXT: s = "CUDA_ERROR_INVALID_CONTEXT"; break;
        case CUDA_ERROR_MAP_FAILED: s = "CUDA_ERROR_MAP_FAILED"; break;
        case CUDA_ERROR_NO_BINARY_FOR_GPU: s = "CUDA_ERROR_NO_BINARY_FOR_GPU"; break;
        case CUDA_ERROR_INVALID_PTX: s = "CUDA_ERROR_INVALID_PTX"; break;
        case CUDA_ERROR_UNSUPPORTED_PTX_VERSION: s = "CUDA_ERROR_UNSUPPORTED_PTX_VERSION"; break;
        case CUDA_ERROR_INVALID_SOURCE: s = "CUDA_ERROR_INVALID_SOURCE"; break;
        case CUDA_ERROR_FILE_NOT_FOUND: s = "CUDA_ERROR_FILE_NOT_FOUND"; break;
        case CUDA_ERROR_OPERATING_SYSTEM: s = "CUDA_ERROR_OPERATING_SYSTEM"; break;
        case CUDA_ERROR_INVALID_HANDLE: s = "CUDA_ERROR_INVALID_HANDLE"; break;
        case CUDA_ERROR_ILLEGAL_STATE: s = "CUDA_ERROR_ILLEGAL_STATE"; break;
        case CUDA_ERROR_NOT_FOUND: s = "CUDA_ERROR_NOT_FOUND"; break;
        case CUDA_ERROR_NOT_READY: s = "CUDA_ERROR_NOT_READY"; break;
        case CUDA_ERROR_ILLEGAL_ADDRESS: s = "CUDA_ERROR_ILLEGAL_ADDRESS"; break;
        case CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES: s = "CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES"; break;
        case CUDA_ERROR_LAUNCH_TIMEOUT: s = "CUDA_ERROR_LAUNCH_TIMEOUT"; break;
        case CUDA_ERROR_LAUNCH_FAILED: s = "CUDA_ERROR_LAUNCH_FAILED"; break;
        case CUDA_ERROR_PEER_ACCESS_UNSUPPORTED: s = "CUDA_ERROR_PEER_ACCESS_UNSUPPORTED"; break;
        case CUDA_ERROR_PRIMARY_CONTEXT_ACTIVE: s = "CUDA_ERROR_PRIMARY_CONTEXT_ACTIVE"; break;
        case CUDA_ERROR_CONTEXT_IS_DESTROYED: s = "CUDA_ERROR_CONTEXT_IS_DESTROYED"; break;
        case CUDA_ERROR_ASSERT: s = "CUDA_ERROR_ASSERT"; break;
        case CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED: s = "CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED"; break;
        case CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED: s = "CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED"; break;
        case CUDA_ERROR_NOT_PERMITTED: s = "CUDA_ERROR_NOT_PERMITTED"; break;
        case CUDA_ERROR_NOT_SUPPORTED: s = "CUDA_ERROR_NOT_SUPPORTED"; break;
        case CUDA_ERROR_STREAM_CAPTURE_UNSUPPORTED: s = "CUDA_ERROR_STREAM_CAPTURE_UNSUPPORTED"; break;
        case CUDA_ERROR_TIMEOUT: s = "CUDA_ERROR_TIMEOUT"; break;
        case CUDA_ERROR_UNKNOWN: s = "CUDA_ERROR_UNKNOWN"; break;
        default: *str = NULL; return CUDA_ERROR_INVALID_VALUE;
    }
    *str = s; return CUDA_SUCCESS;
}

/* -------------------------------------------------------------------------------------------------
 * device
 * ------------------------------------------------------------------------------------------------- */
CUresult cuDeviceGetCount(int* c) { REQUIRE_INIT(); if (!c) return CUDA_ERROR_INVALID_VALUE; *c = 1; return CUDA_SUCCESS; }
CUresult cuDeviceGet(CUdevice* d, int ordinal) { REQUIRE_INIT(); if (!d) return CUDA_ERROR_INVALID_VALUE; if (ordinal != 0) return CUDA_ERROR_INVALID_DEVICE; *d = 0; return CUDA_SUCCESS; }
CUresult cuDeviceGetName(char* name, int len, CUdevice dev) {
    REQUIRE_INIT(); if (!name || len <= 0 || dev != 0) return CUDA_ERROR_INVALID_VALUE;
    strncpy(name, g_name, len - 1); name[len - 1] = 0; return CUDA_SUCCESS;
}
CUresult cuDeviceGetUuid(CUuuid* uuid, CUdevice dev) {
    REQUIRE_INIT(); if (!uuid || dev != 0) return CUDA_ERROR_INVALID_VALUE;
    memcpy(uuid->bytes, g_uuid, 16); return CUDA_SUCCESS;
}
CUresult cuDeviceGetUuid_v2(CUuuid* uuid, CUdevice dev) { return cuDeviceGetUuid(uuid, dev); }
CUresult cuDeviceGetLuid(char* luid, unsigned int* mask, CUdevice dev) {
    REQUIRE_INIT(); (void)luid; (void)mask; if (dev != 0) return CUDA_ERROR_INVALID_DEVICE;
    return CUDA_ERROR_NOT_SUPPORTED; /* LUID is Windows/TCC-only; not modeled */
}
CUresult cuDeviceTotalMem_v2(size_t* bytes, CUdevice dev) { REQUIRE_INIT(); if (!bytes || dev != 0) return CUDA_ERROR_INVALID_VALUE; *bytes = (size_t)g_vram; return CUDA_SUCCESS; }
CUresult cuDeviceComputeCapability(int* major, int* minor, CUdevice dev) {
    REQUIRE_INIT(); if (!major || !minor || dev != 0) return CUDA_ERROR_INVALID_VALUE;
    *major = g_cc_major; *minor = g_cc_minor; return CUDA_SUCCESS;
}
CUresult cuDeviceGetPCIBusId(char* s, int len, CUdevice dev) {
    REQUIRE_INIT(); if (!s || len <= 0 || dev != 0) return CUDA_ERROR_INVALID_VALUE;
    snprintf(s, len, "0000:00:00.0"); return CUDA_SUCCESS;
}
CUresult cuDeviceGetByPCIBusId(CUdevice* dev, const char* s) {
    REQUIRE_INIT(); if (!dev || !s) return CUDA_ERROR_INVALID_VALUE; *dev = 0; return CUDA_SUCCESS;
}
CUresult cuDeviceCanAccessPeer(int* canAccessPeer, CUdevice a, CUdevice b) {
    REQUIRE_INIT(); if (!canAccessPeer) return CUDA_ERROR_INVALID_VALUE; (void)a; (void)b;
    *canAccessPeer = 0; return CUDA_SUCCESS; /* single simulated device: no peers */
}
CUresult cuDeviceGetProperties(CUdevprop* prop, CUdevice dev) {
    REQUIRE_INIT(); if (!prop || dev != 0) return CUDA_ERROR_INVALID_VALUE;
    prop->maxThreadsPerBlock = 1024;
    prop->maxThreadsDim[0] = 1024; prop->maxThreadsDim[1] = 1024; prop->maxThreadsDim[2] = 64;
    prop->maxGridSize[0] = 2147483647; prop->maxGridSize[1] = 65535; prop->maxGridSize[2] = 65535;
    prop->sharedMemPerBlock = 49152; prop->totalConstantMemory = 65536; prop->SIMDWidth = 32;
    prop->memPitch = 2147483647; prop->regsPerBlock = 65536; prop->clockRate = 1500000; prop->textureAlign = 512;
    return CUDA_SUCCESS;
}
CUresult cuDeviceGetAttribute(int* pi, CUdevice_attribute attrib, CUdevice dev) {
    REQUIRE_INIT(); if (!pi || dev != 0) return CUDA_ERROR_INVALID_VALUE;
    switch (attrib) {
        case CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK: *pi = 1024; break;
        case CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X: *pi = 1024; break;
        case CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Y: *pi = 1024; break;
        case CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Z: *pi = 64; break;
        case CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X: *pi = 2147483647; break;
        case CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Y: *pi = 65535; break;
        case CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Z: *pi = 65535; break;
        case CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK: *pi = 49152; break;
        case CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK_OPTIN: *pi = 101376; break;
        case CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_MULTIPROCESSOR: *pi = 102400; break;
        case CU_DEVICE_ATTRIBUTE_TOTAL_CONSTANT_MEMORY: *pi = 65536; break;
        case CU_DEVICE_ATTRIBUTE_WARP_SIZE: *pi = 32; break;
        case CU_DEVICE_ATTRIBUTE_MAX_PITCH: *pi = 2147483647; break;
        case CU_DEVICE_ATTRIBUTE_MAX_REGISTERS_PER_BLOCK: *pi = 65536; break;
        case CU_DEVICE_ATTRIBUTE_CLOCK_RATE: *pi = 1500000; break;
        case CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT: *pi = 512; break;
        case CU_DEVICE_ATTRIBUTE_GPU_OVERLAP: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT: *pi = 32; break;
        case CU_DEVICE_ATTRIBUTE_KERNEL_EXEC_TIMEOUT: *pi = 0; break;
        case CU_DEVICE_ATTRIBUTE_INTEGRATED: *pi = 1; break;          /* unified memory */
        case CU_DEVICE_ATTRIBUTE_CAN_MAP_HOST_MEMORY: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_COMPUTE_MODE: *pi = 0; break;        /* DEFAULT */
        case CU_DEVICE_ATTRIBUTE_MAXIMUM_TEXTURE1D_WIDTH: *pi = 131072; break;
        case CU_DEVICE_ATTRIBUTE_CONCURRENT_KERNELS: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_ECC_ENABLED: *pi = 0; break;
        case CU_DEVICE_ATTRIBUTE_PCI_BUS_ID: *pi = 0; break;
        case CU_DEVICE_ATTRIBUTE_PCI_DEVICE_ID: *pi = 0; break;
        case CU_DEVICE_ATTRIBUTE_PCI_DOMAIN_ID: *pi = 0; break;
        case CU_DEVICE_ATTRIBUTE_TCC_DRIVER: *pi = 0; break;
        case CU_DEVICE_ATTRIBUTE_MEMORY_CLOCK_RATE: *pi = 6251000; break;
        case CU_DEVICE_ATTRIBUTE_GLOBAL_MEMORY_BUS_WIDTH: *pi = 256; break;
        case CU_DEVICE_ATTRIBUTE_L2_CACHE_SIZE: *pi = 4194304; break;
        case CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_MULTIPROCESSOR: *pi = 2048; break;
        case CU_DEVICE_ATTRIBUTE_ASYNC_ENGINE_COUNT: *pi = 2; break;
        case CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR: *pi = g_cc_major; break;
        case CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR: *pi = g_cc_minor; break;
        case CU_DEVICE_ATTRIBUTE_MANAGED_MEMORY: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_MULTI_GPU_BOARD: *pi = 0; break;
        case CU_DEVICE_ATTRIBUTE_CONCURRENT_MANAGED_ACCESS: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_COMPUTE_PREEMPTION_SUPPORTED: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_PAGEABLE_MEMORY_ACCESS: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_DIRECT_MANAGED_MEM_ACCESS_FROM_HOST: *pi = 1; break;
        case CU_DEVICE_ATTRIBUTE_MEMORY_POOLS_SUPPORTED: *pi = 0; break; /* pools are TIER-3 stubs */
        default: *pi = 0; break; /* benign default for the unmodeled attribute tail */
    }
    return CUDA_SUCCESS;
}

/* -------------------------------------------------------------------------------------------------
 * primary context
 * ------------------------------------------------------------------------------------------------- */
CUresult cuDevicePrimaryCtxRetain(CUcontext* pctx, CUdevice dev) {
    REQUIRE_INIT(); if (!pctx || dev != 0) return CUDA_ERROR_INVALID_VALUE;
    if (!g_primary_ctx) {
        g_primary_ctx = calloc(1, sizeof(*g_primary_ctx));
        if (!g_primary_ctx) return CUDA_ERROR_OUT_OF_MEMORY;
        g_primary_ctx->alive = 1; g_primary_ctx->is_primary = 1;
    }
    g_primary_refcount++;
    *pctx = g_primary_ctx;
    return CUDA_SUCCESS;
}
CUresult cuDevicePrimaryCtxRelease_v2(CUdevice dev) {
    REQUIRE_INIT(); if (dev != 0) return CUDA_ERROR_INVALID_DEVICE;
    if (g_primary_refcount > 0) g_primary_refcount--;
    if (g_primary_refcount == 0 && g_primary_ctx) {
        if (g_current_ctx == g_primary_ctx) g_current_ctx = NULL;
        free(g_primary_ctx); g_primary_ctx = NULL;
    }
    return CUDA_SUCCESS;
}
CUresult cuDevicePrimaryCtxReset_v2(CUdevice dev) {
    REQUIRE_INIT(); if (dev != 0) return CUDA_ERROR_INVALID_DEVICE;
    g_primary_refcount = 0;
    if (g_primary_ctx) { if (g_current_ctx == g_primary_ctx) g_current_ctx = NULL; free(g_primary_ctx); g_primary_ctx = NULL; }
    return CUDA_SUCCESS;
}
CUresult cuDevicePrimaryCtxGetState(CUdevice dev, unsigned int* flags, int* active) {
    REQUIRE_INIT(); if (dev != 0) return CUDA_ERROR_INVALID_DEVICE;
    if (flags) *flags = g_primary_ctx ? g_primary_ctx->flags : 0;
    if (active) *active = g_primary_refcount > 0 ? 1 : 0;
    return CUDA_SUCCESS;
}
CUresult cuDevicePrimaryCtxSetFlags_v2(CUdevice dev, unsigned int flags) {
    REQUIRE_INIT(); if (dev != 0) return CUDA_ERROR_INVALID_DEVICE;
    if (g_primary_ctx) g_primary_ctx->flags = flags;
    return CUDA_SUCCESS;
}

/* -------------------------------------------------------------------------------------------------
 * context management (real: current-context + push/pop stack + limits/cache config)
 * ------------------------------------------------------------------------------------------------- */
CUresult cuCtxCreate_v2(CUcontext* pctx, unsigned int flags, CUdevice dev) {
    REQUIRE_INIT(); if (!pctx || dev != 0) return CUDA_ERROR_INVALID_VALUE;
    struct CUctx_st* c = calloc(1, sizeof(*c)); if (!c) return CUDA_ERROR_OUT_OF_MEMORY;
    c->alive = 1; c->flags = flags; g_current_ctx = c; *pctx = c; return CUDA_SUCCESS;
}
CUresult cuCtxCreate_v3(CUcontext* pctx, void* paramsArray, int numParams, unsigned int flags, CUdevice dev) {
    (void)paramsArray; (void)numParams; return cuCtxCreate_v2(pctx, flags, dev);
}
CUresult cuCtxDestroy_v2(CUcontext ctx) {
    if (!ctx) return CUDA_ERROR_INVALID_HANDLE;
    if (g_current_ctx == ctx) g_current_ctx = NULL;
    for (int i = 0; i < g_ctx_sp; i++) if (g_ctx_stack[i] == ctx) g_ctx_stack[i] = NULL;
    if (ctx != g_primary_ctx) free(ctx);
    return CUDA_SUCCESS;
}
CUresult cuCtxPushCurrent_v2(CUcontext ctx) {
    REQUIRE_INIT(); if (!ctx) return CUDA_ERROR_INVALID_HANDLE;
    if (g_ctx_sp >= CTX_STACK_MAX) return CUDA_ERROR_OUT_OF_MEMORY;
    g_ctx_stack[g_ctx_sp++] = g_current_ctx; g_current_ctx = ctx; return CUDA_SUCCESS;
}
CUresult cuCtxPopCurrent_v2(CUcontext* pctx) {
    REQUIRE_INIT();
    if (pctx) *pctx = g_current_ctx;
    g_current_ctx = (g_ctx_sp > 0) ? g_ctx_stack[--g_ctx_sp] : NULL;
    return CUDA_SUCCESS;
}
CUresult cuCtxSetCurrent(CUcontext ctx) { REQUIRE_INIT(); g_current_ctx = ctx; return CUDA_SUCCESS; }
CUresult cuCtxGetCurrent(CUcontext* pctx) { if (!pctx) return CUDA_ERROR_INVALID_VALUE; *pctx = g_current_ctx; return CUDA_SUCCESS; }
CUresult cuCtxGetDevice(CUdevice* dev) { REQUIRE_INIT(); if (!dev) return CUDA_ERROR_INVALID_VALUE; if (!g_current_ctx) return CUDA_ERROR_INVALID_CONTEXT; *dev = 0; return CUDA_SUCCESS; }
CUresult cuCtxSynchronize(void) { REQUIRE_INIT(); if (!g_current_ctx) return CUDA_ERROR_INVALID_CONTEXT; return CUDA_SUCCESS; } /* synchronous executor */
CUresult cuCtxGetApiVersion(CUcontext ctx, unsigned int* version) { (void)ctx; if (!version) return CUDA_ERROR_INVALID_VALUE; *version = 3020; return CUDA_SUCCESS; }
CUresult cuCtxGetFlags(unsigned int* flags) { if (!flags) return CUDA_ERROR_INVALID_VALUE; *flags = g_current_ctx ? g_current_ctx->flags : 0; return CUDA_SUCCESS; }
CUresult cuCtxSetFlags(unsigned int flags) { if (g_current_ctx) g_current_ctx->flags = flags; return CUDA_SUCCESS; }
CUresult cuCtxGetId(CUcontext ctx, unsigned long long* id) { if (!id) return CUDA_ERROR_INVALID_VALUE; *id = (unsigned long long)(size_t)(ctx ? ctx : g_current_ctx); return CUDA_SUCCESS; }
CUresult cuCtxGetLimit(size_t* v, CUlimit limit) {
    if (!v) return CUDA_ERROR_INVALID_VALUE;
    if ((int)limit < 0 || (int)limit >= (int)CU_LIMIT_MAX) return CUDA_ERROR_UNSUPPORTED_LIMIT;
    *v = g_limits[(int)limit]; return CUDA_SUCCESS;
}
CUresult cuCtxSetLimit(CUlimit limit, size_t value) {
    if ((int)limit < 0 || (int)limit >= (int)CU_LIMIT_MAX) return CUDA_ERROR_UNSUPPORTED_LIMIT;
    g_limits[(int)limit] = value; return CUDA_SUCCESS;
}
CUresult cuCtxGetCacheConfig(CUfunc_cache* c) { if (!c) return CUDA_ERROR_INVALID_VALUE; *c = (CUfunc_cache)g_cache_config; return CUDA_SUCCESS; }
CUresult cuCtxSetCacheConfig(CUfunc_cache c) { g_cache_config = (int)c; return CUDA_SUCCESS; }
CUresult cuCtxGetSharedMemConfig(CUsharedconfig* c) { if (!c) return CUDA_ERROR_INVALID_VALUE; *c = (CUsharedconfig)g_shared_config; return CUDA_SUCCESS; }
CUresult cuCtxSetSharedMemConfig(CUsharedconfig c) { g_shared_config = (int)c; return CUDA_SUCCESS; }
CUresult cuCtxGetStreamPriorityRange(int* least, int* greatest) { if (least) *least = 0; if (greatest) *greatest = 0; return CUDA_SUCCESS; }
CUresult cuCtxResetPersistingL2Cache(void) { REQUIRE_INIT(); return CUDA_SUCCESS; }
/* single-device model: peer access can never be enabled (matches cuDeviceCanAccessPeer==0) */
CUresult cuCtxEnablePeerAccess(CUcontext peer, unsigned int flags) { (void)peer; (void)flags; REQUIRE_INIT(); return CUDA_ERROR_PEER_ACCESS_UNSUPPORTED; }
CUresult cuCtxDisablePeerAccess(CUcontext peer) { (void)peer; REQUIRE_INIT(); return CUDA_ERROR_PEER_ACCESS_NOT_ENABLED; }

/* -------------------------------------------------------------------------------------------------
 * module / PTX
 * ------------------------------------------------------------------------------------------------- */
/* Accept an app's kernel image. dd executes PTX text only, so:
 *   - ELF/cubin (0x7f 'E' 'L' 'F')      -> INVALID_IMAGE (SASS is out of scope, see brief tier 3).
 *   - fatbin wrapper/container          -> walk it and extract the embedded UNCOMPRESSED PTX entry
 *     (fatbin.h). A SASS-only or compressed fatbin has no usable PTX -> INVALID_IMAGE.
 *   - anything else                     -> treated as PTX text.
 * This replaces the former blanket fatbin rejection: fatbin -> PTX extraction is the Tier-1 path
 * (docs/ideas/CUDART_PLAN.md §2), and it lets a driver-API app pass a fatbin to cuModuleLoadData too. */
CUresult cuModuleLoadData(CUmodule* module, const void* image) {
    REQUIRE_INIT(); if (!module || !image) return CUDA_ERROR_INVALID_VALUE;
    if (hl_image_is_elf(image)) return CUDA_ERROR_INVALID_IMAGE; /* cubin/SASS: PTX only */
    char* extracted = NULL;
    const char* ptx;
    if (hl_image_is_fatbin(image)) {
        extracted = hl_fatbin_extract_ptx(image, NULL);
        if (!extracted) return CUDA_ERROR_INVALID_IMAGE; /* SASS-only or compressed: no usable PTX */
        ptx = extracted;
    } else {
        ptx = (const char*)image; /* PTX text */
    }
    struct CUmod_st* m = calloc(1, sizeof(*m));
    if (!m) { free(extracted); return CUDA_ERROR_OUT_OF_MEMORY; }
    m->ptx = extracted ? extracted : strdup(ptx);
    *module = m; return CUDA_SUCCESS;
}
CUresult cuModuleLoadDataEx(CUmodule* module, const void* image, unsigned int n, CUjit_option* o, void** ov) {
    (void)n; (void)o; (void)ov; return cuModuleLoadData(module, image);
}
CUresult cuModuleLoadFatBinary(CUmodule* module, const void* image) {
    /* extract embedded PTX from the fatbin (SASS-only/compressed -> INVALID_IMAGE) */
    return cuModuleLoadData(module, image);
}
CUresult cuModuleLoad(CUmodule* module, const char* fname) {
    REQUIRE_INIT(); if (!module || !fname) return CUDA_ERROR_INVALID_VALUE;
    FILE* fp = fopen(fname, "rb"); if (!fp) return CUDA_ERROR_FILE_NOT_FOUND;
    fseek(fp, 0, SEEK_END); long sz = ftell(fp); fseek(fp, 0, SEEK_SET);
    if (sz < 0) { fclose(fp); return CUDA_ERROR_FILE_NOT_FOUND; }
    char* buf = malloc((size_t)sz + 1); if (!buf) { fclose(fp); return CUDA_ERROR_OUT_OF_MEMORY; }
    size_t got = fread(buf, 1, (size_t)sz, fp); fclose(fp); buf[got] = 0;
    CUresult r = cuModuleLoadData(module, buf); free(buf); return r;
}
CUresult cuModuleGetFunction(CUfunction* f, CUmodule m, const char* name) {
    REQUIRE_INIT(); if (!f || !m || !name) return CUDA_ERROR_INVALID_VALUE;
    char params[1024]; char* body = extract_entry(m->ptx, name, params, sizeof(params));
    if (!body) return CUDA_ERROR_NOT_FOUND;
    free(body);
    struct CUfunc_st* fn = calloc(1, sizeof(*fn)); if (!fn) return CUDA_ERROR_OUT_OF_MEMORY;
    fn->mod = m; strncpy(fn->entry, name, sizeof(fn->entry) - 1); *f = fn; return CUDA_SUCCESS;
}
CUresult cuModuleGetGlobal_v2(CUdeviceptr* dptr, size_t* bytes, CUmodule m, const char* name) {
    REQUIRE_INIT(); if (!m || !name) return CUDA_ERROR_INVALID_VALUE;
    (void)dptr; (void)bytes;
    /* dd parses only kernel entries out of PTX, not .global variables -> the symbol is absent. */
    return CUDA_ERROR_NOT_FOUND;
}
CUresult cuModuleGetLoadingMode(CUmoduleLoadingMode* mode) { if (!mode) return CUDA_ERROR_INVALID_VALUE; *mode = CU_MODULE_EAGER_LOADING; return CUDA_SUCCESS; }
CUresult cuModuleUnload(CUmodule m) { if (m) { free(m->ptx); free(m); } return CUDA_SUCCESS; }
/* a module carries no texture/surface references in dd's PTX model */
CUresult cuModuleGetTexRef(CUtexref* t, CUmodule m, const char* name) { REQUIRE_INIT(); (void)t; (void)m; (void)name; return CUDA_ERROR_NOT_FOUND; }
CUresult cuModuleGetSurfRef(CUsurfref* s, CUmodule m, const char* name) { REQUIRE_INIT(); (void)s; (void)m; (void)name; return CUDA_ERROR_NOT_FOUND; }

/* -------------------------------------------------------------------------------------------------
 * memory (real: on unified memory a CUdeviceptr IS a host pointer, so H2D/D2H/D2D are memcpy)
 * ------------------------------------------------------------------------------------------------- */
CUresult cuMemGetInfo_v2(size_t* freeB, size_t* totalB) {
    REQUIRE_INIT();
    unsigned long long used = g_bytes_outstanding <= g_vram ? g_bytes_outstanding : g_vram;
    if (freeB) *freeB = (size_t)(g_vram - used);
    if (totalB) *totalB = (size_t)g_vram;
    return CUDA_SUCCESS;
}
CUresult cuMemAlloc_v2(CUdeviceptr* dptr, size_t bytesize) {
    REQUIRE_INIT(); if (!dptr || bytesize == 0) return CUDA_ERROR_INVALID_VALUE;
    void* p = malloc(bytesize); if (!p) return CUDA_ERROR_OUT_OF_MEMORY;
    alloc_register(p, bytesize, ALLOC_DEVICE);
    *dptr = (CUdeviceptr)(size_t)p; /* unified memory: the device pointer IS a host pointer */
    return CUDA_SUCCESS;
}
CUresult cuMemAllocPitch_v2(CUdeviceptr* dptr, size_t* pPitch, size_t widthBytes, size_t height, unsigned int elemSz) {
    REQUIRE_INIT(); (void)elemSz; if (!dptr || !pPitch || widthBytes == 0 || height == 0) return CUDA_ERROR_INVALID_VALUE;
    size_t pitch = (widthBytes + 511) & ~((size_t)511); /* 512-byte aligned rows, like a real allocator */
    void* p = malloc(pitch * height); if (!p) return CUDA_ERROR_OUT_OF_MEMORY;
    alloc_register(p, pitch * height, ALLOC_DEVICE);
    *dptr = (CUdeviceptr)(size_t)p; *pPitch = pitch; return CUDA_SUCCESS;
}
CUresult cuMemFree_v2(CUdeviceptr dptr) { REQUIRE_INIT(); if (dptr) { alloc_unregister((void*)(size_t)dptr); free((void*)(size_t)dptr); } return CUDA_SUCCESS; }
CUresult cuMemGetAddressRange_v2(CUdeviceptr* pbase, size_t* psize, CUdeviceptr dptr) {
    REQUIRE_INIT(); AllocRec* a = alloc_find((void*)(size_t)dptr);
    if (!a) return CUDA_ERROR_INVALID_VALUE;
    if (pbase) *pbase = (CUdeviceptr)(size_t)a->base;
    if (psize) *psize = a->size;
    return CUDA_SUCCESS;
}
CUresult cuMemAllocHost_v2(void** pp, size_t size) {
    REQUIRE_INIT(); if (!pp) return CUDA_ERROR_INVALID_VALUE;
    void* p = malloc(size ? size : 1); if (!p) return CUDA_ERROR_OUT_OF_MEMORY;
    alloc_register(p, size, ALLOC_HOST); *pp = p; return CUDA_SUCCESS;
}
CUresult cuMemHostAlloc(void** pp, size_t size, unsigned int flags) {
    REQUIRE_INIT(); (void)flags; if (!pp) return CUDA_ERROR_INVALID_VALUE;
    void* p = malloc(size ? size : 1); if (!p) return CUDA_ERROR_OUT_OF_MEMORY;
    alloc_register(p, size, ALLOC_HOST); *pp = p; return CUDA_SUCCESS;
}
CUresult cuMemFreeHost(void* p) { REQUIRE_INIT(); if (p) { alloc_unregister(p); free(p); } return CUDA_SUCCESS; }
CUresult cuMemHostGetDevicePointer_v2(CUdeviceptr* pdptr, void* p, unsigned int flags) {
    REQUIRE_INIT(); (void)flags; if (!pdptr || !p) return CUDA_ERROR_INVALID_VALUE;
    *pdptr = (CUdeviceptr)(size_t)p; /* unified: host and device addresses coincide */
    return CUDA_SUCCESS;
}
CUresult cuMemHostGetFlags(unsigned int* pflags, void* p) { REQUIRE_INIT(); if (!pflags || !p) return CUDA_ERROR_INVALID_VALUE; *pflags = 0; return CUDA_SUCCESS; }
CUresult cuMemHostRegister_v2(void* p, size_t size, unsigned int flags) {
    REQUIRE_INIT(); (void)flags; if (!p) return CUDA_ERROR_INVALID_VALUE;
    if (alloc_find(p)) return CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED;
    alloc_register(p, size, ALLOC_REGISTERED); return CUDA_SUCCESS;
}
CUresult cuMemHostUnregister(void* p) {
    REQUIRE_INIT(); if (!p) return CUDA_ERROR_INVALID_VALUE;
    if (!alloc_find(p)) return CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED;
    alloc_unregister(p); return CUDA_SUCCESS;
}
CUresult cuMemAllocManaged(CUdeviceptr* dptr, size_t bytesize, unsigned int flags) {
    REQUIRE_INIT(); (void)flags; if (!dptr || bytesize == 0) return CUDA_ERROR_INVALID_VALUE;
    void* p = malloc(bytesize); if (!p) return CUDA_ERROR_OUT_OF_MEMORY;
    alloc_register(p, bytesize, ALLOC_MANAGED);
    *dptr = (CUdeviceptr)(size_t)p; /* managed == plain unified-memory malloc here */
    return CUDA_SUCCESS;
}
/* memcpy family — all directions collapse to memcpy on unified memory */
CUresult cuMemcpy(CUdeviceptr dst, CUdeviceptr src, size_t n) { REQUIRE_INIT(); if (n && (!dst || !src)) return CUDA_ERROR_INVALID_VALUE; if (n) memcpy((void*)(size_t)dst, (void*)(size_t)src, n); return CUDA_SUCCESS; }
CUresult cuMemcpyHtoD_v2(CUdeviceptr dst, const void* src, size_t n) { REQUIRE_INIT(); if (n && (!dst || !src)) return CUDA_ERROR_INVALID_VALUE; if (n) memcpy((void*)(size_t)dst, src, n); return CUDA_SUCCESS; }
CUresult cuMemcpyDtoH_v2(void* dst, CUdeviceptr src, size_t n) { REQUIRE_INIT(); if (n && (!dst || !src)) return CUDA_ERROR_INVALID_VALUE; if (n) memcpy(dst, (void*)(size_t)src, n); return CUDA_SUCCESS; }
CUresult cuMemcpyDtoD_v2(CUdeviceptr dst, CUdeviceptr src, size_t n) { REQUIRE_INIT(); if (n && (!dst || !src)) return CUDA_ERROR_INVALID_VALUE; if (n) memcpy((void*)(size_t)dst, (void*)(size_t)src, n); return CUDA_SUCCESS; }
CUresult cuMemcpyPeer(CUdeviceptr dst, CUcontext dctx, CUdeviceptr src, CUcontext sctx, size_t n) { (void)dctx; (void)sctx; return cuMemcpyDtoD_v2(dst, src, n); }
CUresult cuMemcpyAsync(CUdeviceptr dst, CUdeviceptr src, size_t n, CUstream s) { (void)s; return cuMemcpy(dst, src, n); }
CUresult cuMemcpyHtoDAsync_v2(CUdeviceptr dst, const void* src, size_t n, CUstream s) { (void)s; return cuMemcpyHtoD_v2(dst, src, n); }
CUresult cuMemcpyDtoHAsync_v2(void* dst, CUdeviceptr src, size_t n, CUstream s) { (void)s; return cuMemcpyDtoH_v2(dst, src, n); }
CUresult cuMemcpyDtoDAsync_v2(CUdeviceptr dst, CUdeviceptr src, size_t n, CUstream s) { (void)s; return cuMemcpyDtoD_v2(dst, src, n); }
CUresult cuMemcpyPeerAsync(CUdeviceptr dst, CUcontext dctx, CUdeviceptr src, CUcontext sctx, size_t n, CUstream s) { (void)dctx; (void)sctx; (void)s; return cuMemcpyDtoD_v2(dst, src, n); }
/* memset family */
CUresult cuMemsetD8_v2(CUdeviceptr dst, unsigned char uc, size_t N) { REQUIRE_INIT(); if (N && !dst) return CUDA_ERROR_INVALID_VALUE; if (N) memset((void*)(size_t)dst, uc, N); return CUDA_SUCCESS; }
CUresult cuMemsetD16_v2(CUdeviceptr dst, unsigned short us, size_t N) { REQUIRE_INIT(); if (N && !dst) return CUDA_ERROR_INVALID_VALUE; unsigned short* p = (unsigned short*)(size_t)dst; for (size_t i = 0; i < N; i++) p[i] = us; return CUDA_SUCCESS; }
CUresult cuMemsetD32_v2(CUdeviceptr dst, unsigned int ui, size_t N) { REQUIRE_INIT(); if (N && !dst) return CUDA_ERROR_INVALID_VALUE; unsigned int* p = (unsigned int*)(size_t)dst; for (size_t i = 0; i < N; i++) p[i] = ui; return CUDA_SUCCESS; }
CUresult cuMemsetD8Async(CUdeviceptr dst, unsigned char uc, size_t N, CUstream s) { (void)s; return cuMemsetD8_v2(dst, uc, N); }
CUresult cuMemsetD16Async(CUdeviceptr dst, unsigned short us, size_t N, CUstream s) { (void)s; return cuMemsetD16_v2(dst, us, N); }
CUresult cuMemsetD32Async(CUdeviceptr dst, unsigned int ui, size_t N, CUstream s) { (void)s; return cuMemsetD32_v2(dst, ui, N); }
/* unified memory: prefetch / advise are valid no-ops */
CUresult cuMemPrefetchAsync(CUdeviceptr p, size_t n, CUdevice dst, CUstream s) { (void)p; (void)n; (void)dst; (void)s; REQUIRE_INIT(); return CUDA_SUCCESS; }
CUresult cuMemAdvise(CUdeviceptr p, size_t n, int advice, CUdevice dev) { (void)p; (void)n; (void)advice; (void)dev; REQUIRE_INIT(); return CUDA_SUCCESS; }
/* async pool alloc/free reduce to malloc/free on the synchronous unified-memory model */
CUresult cuMemAllocAsync(CUdeviceptr* dptr, size_t bytesize, CUstream s) { (void)s; return cuMemAlloc_v2(dptr, bytesize); }
CUresult cuMemFreeAsync(CUdeviceptr dptr, CUstream s) { (void)s; return cuMemFree_v2(dptr); }

/* -------------------------------------------------------------------------------------------------
 * pointer attributes (TIER 2: report what dd's model actually knows)
 * ------------------------------------------------------------------------------------------------- */
static CUresult pointer_attr(CUpointer_attribute attr, void* data, CUdeviceptr ptr) {
    AllocRec* a = alloc_find((void*)(size_t)ptr);
    switch (attr) {
        case CU_POINTER_ATTRIBUTE_CONTEXT: *(CUcontext*)data = g_current_ctx; return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_MEMORY_TYPE:
            *(unsigned int*)data = (a && a->kind == ALLOC_HOST) ? CU_MEMORYTYPE_HOST : CU_MEMORYTYPE_DEVICE;
            return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_DEVICE_POINTER: *(CUdeviceptr*)data = ptr; return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_HOST_POINTER: *(void**)data = (void*)(size_t)ptr; return CUDA_SUCCESS; /* unified */
        case CU_POINTER_ATTRIBUTE_IS_MANAGED: *(unsigned int*)data = (a && a->kind == ALLOC_MANAGED) ? 1 : 0; return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL: *(int*)data = 0; return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_BUFFER_ID: *(unsigned long long*)data = (unsigned long long)(a ? (a - g_allocs) + 1 : 0); return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_SYNC_MEMOPS: *(int*)data = 1; return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_MAPPED: *(int*)data = a ? 1 : 0; return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_RANGE_START_ADDR: *(CUdeviceptr*)data = a ? (CUdeviceptr)(size_t)a->base : 0; return CUDA_SUCCESS;
        case CU_POINTER_ATTRIBUTE_RANGE_SIZE: *(size_t*)data = a ? a->size : 0; return CUDA_SUCCESS;
        default: return CUDA_ERROR_NOT_SUPPORTED;
    }
}
CUresult cuPointerGetAttribute(void* data, CUpointer_attribute attr, CUdeviceptr ptr) {
    REQUIRE_INIT(); if (!data) return CUDA_ERROR_INVALID_VALUE; return pointer_attr(attr, data, ptr);
}
CUresult cuPointerGetAttributes(unsigned int n, CUpointer_attribute* attrs, void** data, CUdeviceptr ptr) {
    REQUIRE_INIT(); if (!attrs || !data) return CUDA_ERROR_INVALID_VALUE;
    for (unsigned int i = 0; i < n; i++) { CUresult r = pointer_attr(attrs[i], data[i], ptr); if (r != CUDA_SUCCESS && r != CUDA_ERROR_NOT_SUPPORTED) return r; }
    return CUDA_SUCCESS;
}
CUresult cuPointerSetAttribute(const void* value, CUpointer_attribute attr, CUdeviceptr ptr) { (void)value; (void)attr; (void)ptr; REQUIRE_INIT(); return CUDA_SUCCESS; }

/* -------------------------------------------------------------------------------------------------
 * kernel launch (real: compiles the entry's PTX and runs the whole grid on the CPU backend)
 * ------------------------------------------------------------------------------------------------- */
static CUresult launch_impl(CUfunction f, unsigned gx, unsigned gy, unsigned gz,
                            unsigned bx, unsigned by, unsigned bz,
                            void** kernelParams, void** extra) {
    REQUIRE_INIT();
    if (!f || !f->mod) return CUDA_ERROR_INVALID_HANDLE;
    static Program prog; /* single-threaded launch: reuse one program buffer */
    unsigned block[3] = { bx, by, bz };
    if (ptx_compile(f->mod->ptx, f->entry, block, &prog) != 0) return CUDA_ERROR_INVALID_IMAGE;

    unsigned char blob[512]; memset(blob, 0, sizeof(blob));
    if (prog.param_bytes > (int)sizeof(blob)) return CUDA_ERROR_NOT_SUPPORTED;
    if (kernelParams) {
        /* each kernelParams[i] points to the i-th argument value */
        for (int i = 0; i < prog.nparam; i++) {
            if (!kernelParams[i]) return CUDA_ERROR_INVALID_VALUE;
            memcpy(blob + prog.params[i].offset, kernelParams[i], prog.params[i].width);
        }
    } else if (extra) {
        /* the cudart `extra` ABI: CU_LAUNCH_PARAM_BUFFER_POINTER + _BUFFER_SIZE, _END-terminated */
        void* bufptr = NULL; size_t bufsize = 0;
        for (int i = 0; extra[i] != CU_LAUNCH_PARAM_END; i++) {
            if (extra[i] == CU_LAUNCH_PARAM_BUFFER_POINTER) bufptr = extra[++i];
            else if (extra[i] == CU_LAUNCH_PARAM_BUFFER_SIZE) bufsize = *(size_t*)extra[++i];
            else break;
        }
        if (!bufptr) return CUDA_ERROR_INVALID_VALUE;
        size_t n = (bufsize && bufsize <= sizeof(blob)) ? bufsize : (size_t)prog.param_bytes;
        memcpy(blob, bufptr, n);
    } else if (prog.nparam > 0) {
        return CUDA_ERROR_INVALID_VALUE; /* kernel has params but none were provided */
    }

    unsigned grid[3] = { gx, gy, gz };
    ptx_execute(&prog, blob, grid);
    return CUDA_SUCCESS;
}
CUresult cuLaunchKernel(CUfunction f,
                        unsigned int gx, unsigned int gy, unsigned int gz,
                        unsigned int bx, unsigned int by, unsigned int bz,
                        unsigned int sharedMemBytes, CUstream stream,
                        void** kernelParams, void** extra) {
    (void)sharedMemBytes; (void)stream;
    return launch_impl(f, gx, gy, gz, bx, by, bz, kernelParams, extra);
}
CUresult cuLaunchKernelEx(const CUlaunchConfig* cfg, CUfunction f, void** kernelParams, void** extra) {
    REQUIRE_INIT(); if (!cfg) return CUDA_ERROR_INVALID_VALUE;
    return launch_impl(f, cfg->gridDimX, cfg->gridDimY, cfg->gridDimZ,
                       cfg->blockDimX, cfg->blockDimY, cfg->blockDimZ, kernelParams, extra);
}
CUresult cuLaunchCooperativeKernel(CUfunction f,
                                   unsigned int gx, unsigned int gy, unsigned int gz,
                                   unsigned int bx, unsigned int by, unsigned int bz,
                                   unsigned int sharedMemBytes, CUstream stream, void** kernelParams) {
    (void)sharedMemBytes; (void)stream;
    return launch_impl(f, gx, gy, gz, bx, by, bz, kernelParams, NULL);
}
CUresult cuLaunchHostFunc(CUstream stream, CUhostFn fn, void* userData) {
    REQUIRE_INIT(); (void)stream; if (!fn) return CUDA_ERROR_INVALID_VALUE;
    fn(userData); /* synchronous executor: run the host callback inline */
    return CUDA_SUCCESS;
}
CUresult cuFuncGetAttribute(int* pi, CUfunction_attribute attrib, CUfunction f) {
    REQUIRE_INIT(); if (!pi || !f) return CUDA_ERROR_INVALID_VALUE;
    switch (attrib) {
        case CU_FUNC_ATTRIBUTE_MAX_THREADS_PER_BLOCK: *pi = 1024; break;
        case CU_FUNC_ATTRIBUTE_SHARED_SIZE_BYTES: *pi = 0; break;
        case CU_FUNC_ATTRIBUTE_CONST_SIZE_BYTES: *pi = 0; break;
        case CU_FUNC_ATTRIBUTE_LOCAL_SIZE_BYTES: *pi = 0; break;
        case CU_FUNC_ATTRIBUTE_NUM_REGS: *pi = 32; break;
        case CU_FUNC_ATTRIBUTE_PTX_VERSION: *pi = g_cc_major * 10 + g_cc_minor; break;
        case CU_FUNC_ATTRIBUTE_BINARY_VERSION: *pi = g_cc_major * 10 + g_cc_minor; break;
        case CU_FUNC_ATTRIBUTE_CACHE_MODE_CA: *pi = 0; break;
        case CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES: *pi = f->max_dyn_shared; break;
        case CU_FUNC_ATTRIBUTE_PREFERRED_SHARED_MEMORY_CARVEOUT: *pi = -1; break;
        default: *pi = 0; break;
    }
    return CUDA_SUCCESS;
}
CUresult cuFuncSetAttribute(CUfunction f, CUfunction_attribute attrib, int value) {
    REQUIRE_INIT(); if (!f) return CUDA_ERROR_INVALID_VALUE;
    if (attrib == CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES) f->max_dyn_shared = value;
    return CUDA_SUCCESS;
}
CUresult cuFuncSetCacheConfig(CUfunction f, CUfunc_cache config) { (void)f; (void)config; REQUIRE_INIT(); return CUDA_SUCCESS; }
CUresult cuFuncSetSharedMemConfig(CUfunction f, CUsharedconfig config) { (void)f; (void)config; REQUIRE_INIT(); return CUDA_SUCCESS; }
CUresult cuFuncGetModule(CUmodule* m, CUfunction f) { REQUIRE_INIT(); if (!m || !f) return CUDA_ERROR_INVALID_VALUE; *m = f->mod; return CUDA_SUCCESS; }
CUresult cuFuncGetName(const char** name, CUfunction f) { REQUIRE_INIT(); if (!name || !f) return CUDA_ERROR_INVALID_VALUE; *name = f->entry; return CUDA_SUCCESS; }

/* -------------------------------------------------------------------------------------------------
 * occupancy (TIER 2: computed from the modeled SM limits — sane, not fabricated)
 * ------------------------------------------------------------------------------------------------- */
CUresult cuOccupancyMaxActiveBlocksPerMultiprocessor(int* numBlocks, CUfunction f, int blockSize, size_t dynSmem) {
    REQUIRE_INIT(); (void)f; (void)dynSmem; if (!numBlocks || blockSize <= 0) return CUDA_ERROR_INVALID_VALUE;
    int by_threads = 2048 / blockSize;            /* MAX_THREADS_PER_MULTIPROCESSOR / blockSize */
    int n = by_threads < 32 ? by_threads : 32;    /* cap at 32 resident blocks/SM */
    *numBlocks = n < 1 ? 1 : n;
    return CUDA_SUCCESS;
}
CUresult cuOccupancyMaxActiveBlocksPerMultiprocessorWithFlags(int* numBlocks, CUfunction f, int blockSize, size_t dynSmem, unsigned int flags) {
    (void)flags; return cuOccupancyMaxActiveBlocksPerMultiprocessor(numBlocks, f, blockSize, dynSmem);
}
CUresult cuOccupancyMaxPotentialBlockSize(int* minGridSize, int* blockSize, CUfunction f, CUoccupancyB2DSize b2d, size_t dynSmem, int blockSizeLimit) {
    REQUIRE_INIT(); (void)f; (void)b2d; (void)dynSmem;
    int bs = blockSizeLimit > 0 && blockSizeLimit < 256 ? blockSizeLimit : 256;
    if (blockSize) *blockSize = bs;
    if (minGridSize) *minGridSize = 32; /* MULTIPROCESSOR_COUNT */
    return CUDA_SUCCESS;
}
CUresult cuOccupancyMaxPotentialBlockSizeWithFlags(int* minGridSize, int* blockSize, CUfunction f, CUoccupancyB2DSize b2d, size_t dynSmem, int blockSizeLimit, unsigned int flags) {
    (void)flags; return cuOccupancyMaxPotentialBlockSize(minGridSize, blockSize, f, b2d, dynSmem, blockSizeLimit);
}
CUresult cuOccupancyAvailableDynamicSMemPerBlock(size_t* dynSmem, CUfunction f, int numBlocks, int blockSize) {
    REQUIRE_INIT(); (void)f; (void)numBlocks; (void)blockSize; if (!dynSmem) return CUDA_ERROR_INVALID_VALUE;
    *dynSmem = 49152; return CUDA_SUCCESS;
}

/* -------------------------------------------------------------------------------------------------
 * streams (synchronous executor: work completes at launch, so sync/query are trivially ready)
 * ------------------------------------------------------------------------------------------------- */
CUresult cuStreamCreate(CUstream* s, unsigned int flags) { REQUIRE_INIT(); if (!s) return CUDA_ERROR_INVALID_VALUE; struct CUstream_st* st = calloc(1, sizeof(*st)); if (!st) return CUDA_ERROR_OUT_OF_MEMORY; st->flags = flags; *s = st; return CUDA_SUCCESS; }
CUresult cuStreamCreateWithPriority(CUstream* s, unsigned int flags, int priority) { REQUIRE_INIT(); if (!s) return CUDA_ERROR_INVALID_VALUE; struct CUstream_st* st = calloc(1, sizeof(*st)); if (!st) return CUDA_ERROR_OUT_OF_MEMORY; st->flags = flags; st->priority = priority; *s = st; return CUDA_SUCCESS; }
CUresult cuStreamDestroy_v2(CUstream s) { if (!stream_is_default(s)) free(s); return CUDA_SUCCESS; }
CUresult cuStreamSynchronize(CUstream s) { REQUIRE_INIT(); (void)s; return CUDA_SUCCESS; }
CUresult cuStreamQuery(CUstream s) { REQUIRE_INIT(); (void)s; return CUDA_SUCCESS; } /* always ready */
CUresult cuStreamWaitEvent(CUstream s, CUevent e, unsigned int flags) { REQUIRE_INIT(); (void)s; (void)e; (void)flags; return CUDA_SUCCESS; }
CUresult cuStreamAddCallback(CUstream s, CUstreamCallback cb, void* userData, unsigned int flags) {
    REQUIRE_INIT(); (void)flags; if (!cb) return CUDA_ERROR_INVALID_VALUE;
    cb(s, CUDA_SUCCESS, userData); /* synchronous: fire the callback immediately with success */
    return CUDA_SUCCESS;
}
CUresult cuStreamGetFlags(CUstream s, unsigned int* flags) { REQUIRE_INIT(); if (!flags) return CUDA_ERROR_INVALID_VALUE; *flags = stream_is_default(s) ? 0 : s->flags; return CUDA_SUCCESS; }
CUresult cuStreamGetPriority(CUstream s, int* priority) { REQUIRE_INIT(); if (!priority) return CUDA_ERROR_INVALID_VALUE; *priority = stream_is_default(s) ? 0 : s->priority; return CUDA_SUCCESS; }
CUresult cuStreamGetCtx(CUstream s, CUcontext* pctx) { REQUIRE_INIT(); (void)s; if (!pctx) return CUDA_ERROR_INVALID_VALUE; *pctx = g_current_ctx; return CUDA_SUCCESS; }
CUresult cuStreamGetId(CUstream s, unsigned long long* id) { REQUIRE_INIT(); if (!id) return CUDA_ERROR_INVALID_VALUE; *id = (unsigned long long)(size_t)s; return CUDA_SUCCESS; }
CUresult cuStreamAttachMemAsync(CUstream s, CUdeviceptr dptr, size_t length, unsigned int flags) { REQUIRE_INIT(); (void)s; (void)dptr; (void)length; (void)flags; return CUDA_SUCCESS; }
CUresult cuStreamIsCapturing(CUstream s, int* status) { REQUIRE_INIT(); (void)s; if (status) *status = 0 /* CU_STREAM_CAPTURE_STATUS_NONE */; return CUDA_SUCCESS; }
CUresult cuThreadExchangeStreamCaptureMode(int* mode) { REQUIRE_INIT(); (void)mode; return CUDA_SUCCESS; }

/* -------------------------------------------------------------------------------------------------
 * events (real: cuEventRecord timestamps with the monotonic clock so cuEventElapsedTime is truthful)
 * ------------------------------------------------------------------------------------------------- */
CUresult cuEventCreate(CUevent* e, unsigned int flags) { REQUIRE_INIT(); if (!e) return CUDA_ERROR_INVALID_VALUE; struct CUevent_st* ev = calloc(1, sizeof(*ev)); if (!ev) return CUDA_ERROR_OUT_OF_MEMORY; ev->flags = flags; *e = ev; return CUDA_SUCCESS; }
CUresult cuEventRecord(CUevent e, CUstream s) { REQUIRE_INIT(); (void)s; if (!e) return CUDA_ERROR_INVALID_HANDLE; clock_gettime(CLOCK_MONOTONIC, &e->ts); e->recorded = 1; return CUDA_SUCCESS; }
CUresult cuEventRecordWithFlags(CUevent e, CUstream s, unsigned int flags) { (void)flags; return cuEventRecord(e, s); }
CUresult cuEventQuery(CUevent e) { REQUIRE_INIT(); if (!e) return CUDA_ERROR_INVALID_HANDLE; return e->recorded ? CUDA_SUCCESS : CUDA_ERROR_NOT_READY; }
CUresult cuEventSynchronize(CUevent e) { REQUIRE_INIT(); if (!e) return CUDA_ERROR_INVALID_HANDLE; return CUDA_SUCCESS; }
CUresult cuEventDestroy_v2(CUevent e) { if (e) free(e); return CUDA_SUCCESS; }
CUresult cuEventElapsedTime(float* ms, CUevent start, CUevent end) {
    REQUIRE_INIT(); if (!ms || !start || !end) return CUDA_ERROR_INVALID_VALUE;
    if (!start->recorded || !end->recorded) return CUDA_ERROR_NOT_READY;
    double a = start->ts.tv_sec * 1e3 + start->ts.tv_nsec / 1e6;
    double b = end->ts.tv_sec * 1e3 + end->ts.tv_nsec / 1e6;
    *ms = (float)(b - a);
    return CUDA_SUCCESS;
}

/* -------------------------------------------------------------------------------------------------
 * profiler control (valid no-ops)
 * ------------------------------------------------------------------------------------------------- */
CUresult cuProfilerInitialize(const char* cfg, const char* out, int fmt) { (void)cfg; (void)out; (void)fmt; return CUDA_SUCCESS; }
CUresult cuProfilerStart(void) { return CUDA_SUCCESS; }
CUresult cuProfilerStop(void) { return CUDA_SUCCESS; }

/* =================================================================================================
 * TIER 3 — present-but-honest stubs.
 * Every symbol below is EXPORTED so dlsym / cuGetProcAddress resolve and a version script matches the
 * real libcuda symbol table; each returns CUDA_ERROR_NOT_SUPPORTED — the spec-correct error for an op
 * the dd-gpu model does not serve — never a crash and never a fake success. Groups: graphs, IPC,
 * texture/surface objects + legacy tex/surf refs, arrays/mipmaps, 2D/3D + array copies, unified VMM,
 * memory pools, peer-device P2P attrs, external memory/semaphore interop, and GL/EGL/VDPAU graphics
 * interop. They are defined with an empty parameter list: on the SysV/AAPCS ABI a caller may pass the
 * real arguments (they are ignored) and receives the correct error return.
 * ================================================================================================= */
#define STUB_LIST(X) \
    /* JIT linker */ \
    X(cuLinkCreate_v2) X(cuLinkAddData_v2) X(cuLinkAddFile_v2) X(cuLinkComplete) X(cuLinkDestroy) \
    /* library / kernel API (12.x) */ \
    X(cuLibraryLoadData) X(cuLibraryLoadFromFile) X(cuLibraryUnload) X(cuLibraryGetKernel) \
    X(cuLibraryGetModule) X(cuKernelGetFunction) X(cuLibraryGetGlobal) X(cuLibraryGetManaged) \
    X(cuLibraryGetUnifiedFunction) X(cuKernelGetAttribute) X(cuKernelSetAttribute) \
    X(cuKernelSetCacheConfig) X(cuKernelGetName) X(cuFuncIsLoaded) X(cuFuncLoad) \
    /* arrays + mipmapped arrays */ \
    X(cuArrayCreate_v2) X(cuArray3DCreate_v2) X(cuArrayGetDescriptor_v2) X(cuArray3DGetDescriptor_v2) \
    X(cuArrayDestroy) X(cuArrayGetSparseProperties) X(cuArrayGetPlane) X(cuArrayGetMemoryRequirements) \
    X(cuMipmappedArrayCreate) X(cuMipmappedArrayGetLevel) X(cuMipmappedArrayDestroy) \
    X(cuMipmappedArrayGetSparseProperties) X(cuMipmappedArrayGetMemoryRequirements) \
    /* 2D/3D + array-backed copies (dd has no CUarray backing store) */ \
    X(cuMemcpy2D_v2) X(cuMemcpy2DUnaligned_v2) X(cuMemcpy3D_v2) X(cuMemcpy3DPeer) \
    X(cuMemcpy2DAsync_v2) X(cuMemcpy3DAsync_v2) X(cuMemcpy3DPeerAsync) \
    X(cuMemcpyHtoA_v2) X(cuMemcpyAtoH_v2) X(cuMemcpyAtoD_v2) X(cuMemcpyDtoA_v2) X(cuMemcpyAtoA_v2) \
    X(cuMemcpyHtoAAsync_v2) X(cuMemcpyAtoHAsync_v2) \
    X(cuMemsetD2D8_v2) X(cuMemsetD2D16_v2) X(cuMemsetD2D32_v2) \
    X(cuMemsetD2D8Async) X(cuMemsetD2D16Async) X(cuMemsetD2D32Async) \
    /* managed-memory range attrs + memory pools */ \
    X(cuMemRangeGetAttribute) X(cuMemRangeGetAttributes) \
    X(cuMemPoolCreate) X(cuMemPoolDestroy) X(cuMemPoolTrimTo) X(cuMemPoolSetAttribute) \
    X(cuMemPoolGetAttribute) X(cuMemPoolSetAccess) X(cuMemPoolGetAccess) \
    X(cuMemPoolExportToShareableHandle) X(cuMemPoolImportFromShareableHandle) \
    X(cuMemPoolExportPointer) X(cuMemPoolImportPointer) X(cuMemAllocFromPoolAsync) \
    X(cuDeviceGetDefaultMemPool) X(cuDeviceGetMemPool) X(cuDeviceSetMemPool) \
    /* low-level virtual memory management */ \
    X(cuMemAddressReserve) X(cuMemAddressFree) X(cuMemCreate) X(cuMemRelease) X(cuMemMap) \
    X(cuMemMapArrayAsync) X(cuMemUnmap) X(cuMemSetAccess) X(cuMemGetAccess) \
    X(cuMemExportToShareableHandle) X(cuMemImportFromShareableHandle) X(cuMemGetAllocationGranularity) \
    X(cuMemGetAllocationPropertiesFromHandle) X(cuMemRetainAllocationHandle) \
    /* IPC */ \
    X(cuIpcGetEventHandle) X(cuIpcOpenEventHandle) X(cuIpcGetMemHandle) X(cuIpcOpenMemHandle_v2) \
    X(cuIpcCloseMemHandle) \
    /* peer P2P attributes (single-device model) */ \
    X(cuDeviceGetP2PAttribute) \
    /* texture / surface objects */ \
    X(cuTexObjectCreate) X(cuTexObjectDestroy) X(cuTexObjectGetResourceDesc) X(cuTexObjectGetTextureDesc) \
    X(cuTexObjectGetResourceViewDesc) X(cuSurfObjectCreate) X(cuSurfObjectDestroy) X(cuSurfObjectGetResourceDesc) \
    /* legacy texture / surface references */ \
    X(cuTexRefCreate) X(cuTexRefDestroy) X(cuTexRefSetArray) X(cuTexRefSetAddress_v2) X(cuTexRefSetAddress2D_v3) \
    X(cuTexRefSetFormat) X(cuTexRefSetAddressMode) X(cuTexRefSetFilterMode) X(cuTexRefSetFlags) \
    X(cuTexRefGetAddress_v2) X(cuTexRefGetArray) X(cuTexRefGetFlags) X(cuTexRefSetMipmappedArray) \
    X(cuTexRefSetMipmapFilterMode) X(cuTexRefSetMipmapLevelBias) X(cuTexRefSetMipmapLevelClamp) \
    X(cuTexRefSetMaxAnisotropy) X(cuTexRefSetBorderColor) X(cuTexRefGetBorderColor) \
    X(cuSurfRefSetArray) X(cuSurfRefGetArray) \
    /* external memory / semaphore interop */ \
    X(cuImportExternalMemory) X(cuExternalMemoryGetMappedBuffer) X(cuExternalMemoryGetMappedMipmappedArray) \
    X(cuDestroyExternalMemory) X(cuImportExternalSemaphore) X(cuSignalExternalSemaphoresAsync) \
    X(cuWaitExternalSemaphoresAsync) X(cuDestroyExternalSemaphore) \
    /* stream memory ops + capture */ \
    X(cuStreamWaitValue32_v2) X(cuStreamWaitValue64_v2) X(cuStreamWriteValue32_v2) X(cuStreamWriteValue64_v2) \
    X(cuStreamBatchMemOp_v2) X(cuStreamBeginCapture_v2) X(cuStreamEndCapture) X(cuStreamGetCaptureInfo_v2) \
    X(cuStreamUpdateCaptureDependencies) \
    /* CUDA graphs */ \
    X(cuGraphCreate) X(cuGraphDestroy) X(cuGraphClone) X(cuGraphInstantiate) X(cuGraphInstantiateWithFlags) \
    X(cuGraphInstantiateWithParams) X(cuGraphExecDestroy) X(cuGraphLaunch) X(cuGraphUpload) \
    X(cuGraphExecUpdate_v2) X(cuGraphGetNodes) X(cuGraphGetRootNodes) X(cuGraphGetEdges) \
    X(cuGraphNodeGetType) X(cuGraphNodeGetDependencies) X(cuGraphNodeGetDependentNodes) \
    X(cuGraphAddDependencies) X(cuGraphRemoveDependencies) X(cuGraphDestroyNode) \
    X(cuGraphAddKernelNode_v2) X(cuGraphKernelNodeGetParams_v2) X(cuGraphKernelNodeSetParams_v2) \
    X(cuGraphAddMemcpyNode) X(cuGraphMemcpyNodeGetParams) X(cuGraphMemcpyNodeSetParams) \
    X(cuGraphAddMemsetNode) X(cuGraphMemsetNodeGetParams) X(cuGraphMemsetNodeSetParams) \
    X(cuGraphAddHostNode) X(cuGraphHostNodeGetParams) X(cuGraphHostNodeSetParams) \
    X(cuGraphAddChildGraphNode) X(cuGraphChildGraphNodeGetGraph) X(cuGraphAddEmptyNode) \
    X(cuGraphAddEventRecordNode) X(cuGraphEventRecordNodeGetEvent) X(cuGraphEventRecordNodeSetEvent) \
    X(cuGraphAddEventWaitNode) X(cuGraphEventWaitNodeGetEvent) X(cuGraphEventWaitNodeSetEvent) \
    X(cuGraphAddMemAllocNode) X(cuGraphAddMemFreeNode) X(cuGraphAddBatchMemOpNode) \
    X(cuGraphExecKernelNodeSetParams_v2) X(cuGraphExecMemcpyNodeSetParams) X(cuGraphExecMemsetNodeSetParams) \
    X(cuGraphExecHostNodeSetParams) X(cuGraphExecChildGraphNodeSetParams) \
    X(cuGraphExecEventRecordNodeSetEvent) X(cuGraphExecEventWaitNodeSetEvent) \
    X(cuGraphNodeSetEnabled) X(cuGraphNodeGetEnabled) X(cuGraphKernelNodeCopyAttributes) \
    X(cuGraphKernelNodeGetAttribute) X(cuGraphKernelNodeSetAttribute) X(cuGraphDebugDotPrint) \
    X(cuDeviceGraphMemTrim) X(cuDeviceGetGraphMemAttribute) X(cuDeviceSetGraphMemAttribute) \
    X(cuUserObjectCreate) X(cuUserObjectRetain) X(cuUserObjectRelease) \
    X(cuGraphRetainUserObject) X(cuGraphReleaseUserObject) \
    /* multi-device / cooperative / green contexts / multicast */ \
    X(cuLaunchCooperativeKernelMultiDevice) X(cuOccupancyMaxActiveClusters) X(cuOccupancyMaxPotentialClusterSize) \
    X(cuCtxGetExecAffinity) X(cuGreenCtxCreate) X(cuGreenCtxDestroy) X(cuGreenCtxRecordEvent) \
    X(cuGreenCtxWaitEvent) X(cuCtxFromGreenCtx) X(cuDeviceGetDevResource) \
    X(cuMulticastCreate) X(cuMulticastAddDevice) X(cuMulticastBindMem) X(cuMulticastBindAddr) \
    X(cuMulticastUnbind) X(cuMulticastGetGranularity) \
    /* graphics interop: generic + GL + EGL + VDPAU */ \
    X(cuGraphicsUnregisterResource) X(cuGraphicsMapResources) X(cuGraphicsUnmapResources) \
    X(cuGraphicsResourceGetMappedPointer_v2) X(cuGraphicsSubResourceGetMappedArray) \
    X(cuGraphicsResourceGetMappedMipmappedArray) X(cuGraphicsResourceSetMapFlags_v2) \
    X(cuGLGetDevices_v2) X(cuGraphicsGLRegisterBuffer) X(cuGraphicsGLRegisterImage) X(cuGLCtxCreate_v2) \
    X(cuWGLGetDevice) X(cuGraphicsEGLRegisterImage) X(cuEGLStreamConsumerConnect) X(cuEGLStreamProducerConnect) \
    X(cuVDPAUGetDevice) X(cuVDPAUCtxCreate_v2) X(cuGraphicsVDPAURegisterVideoSurface) \
    X(cuGraphicsVDPAURegisterOutputSurface) \
    /* tensor maps, coredump, RDMA, async-notification, export table */ \
    X(cuTensorMapEncodeTiled) X(cuTensorMapEncodeIm2col) X(cuTensorMapReplaceAddress) \
    X(cuCoredumpGetAttribute) X(cuCoredumpSetAttribute) X(cuCoredumpGetAttributeGlobal) \
    X(cuCoredumpSetAttributeGlobal) X(cuFlushGPUDirectRDMAWrites) X(cuDeviceGetNvSciSyncAttributes) \
    X(cuDeviceRegisterAsyncNotification) X(cuDeviceUnregisterAsyncNotification) X(cuGetExportTable)

#define DEF_STUB(n) CUresult n(void) { return CUDA_ERROR_NOT_SUPPORTED; }
STUB_LIST(DEF_STUB)
#undef DEF_STUB

/* =================================================================================================
 * cuGetProcAddress — the entry-point dispatch table newer apps resolve everything through, plus the
 * base-name -> versioned (_v2/_v3) mapping so e.g. cuGetProcAddress("cuMemAlloc", ...) returns the
 * _v2 implementation. Also feeds the ABI-exported base symbols (attribute aliases below).
 * ================================================================================================= */
typedef struct { const char* name; void* fn; } ProcEntry;

/* defined below cuGetProcAddress, but their addresses are taken by the proc table above them */
CUresult cuGetProcAddress(const char* symbol, void** pfn, int cudaVersion, cuuint64_t flags);
CUresult cuGetProcAddress_v2(const char* symbol, void** pfn, int cudaVersion, cuuint64_t flags,
                             CUdriverProcAddressQueryResult* status);

/* real (tier 1/2) entries — exact exported names */
#define REAL_LIST(X) \
    X(cuInit) X(cuDriverGetVersion) X(cuGetErrorString) X(cuGetErrorName) \
    X(cuGetProcAddress) X(cuGetProcAddress_v2) \
    X(cuDeviceGetCount) X(cuDeviceGet) X(cuDeviceGetName) X(cuDeviceGetUuid) X(cuDeviceGetUuid_v2) \
    X(cuDeviceGetLuid) X(cuDeviceTotalMem_v2) X(cuDeviceComputeCapability) X(cuDeviceGetAttribute) \
    X(cuDeviceGetPCIBusId) X(cuDeviceGetByPCIBusId) X(cuDeviceCanAccessPeer) X(cuDeviceGetProperties) \
    X(cuDevicePrimaryCtxRetain) X(cuDevicePrimaryCtxRelease_v2) X(cuDevicePrimaryCtxReset_v2) \
    X(cuDevicePrimaryCtxGetState) X(cuDevicePrimaryCtxSetFlags_v2) \
    X(cuCtxCreate_v2) X(cuCtxCreate_v3) X(cuCtxDestroy_v2) X(cuCtxPushCurrent_v2) X(cuCtxPopCurrent_v2) \
    X(cuCtxSetCurrent) X(cuCtxGetCurrent) X(cuCtxGetDevice) X(cuCtxSynchronize) X(cuCtxGetApiVersion) \
    X(cuCtxGetFlags) X(cuCtxSetFlags) X(cuCtxGetId) X(cuCtxGetLimit) X(cuCtxSetLimit) \
    X(cuCtxGetCacheConfig) X(cuCtxSetCacheConfig) X(cuCtxGetSharedMemConfig) X(cuCtxSetSharedMemConfig) \
    X(cuCtxGetStreamPriorityRange) X(cuCtxResetPersistingL2Cache) X(cuCtxEnablePeerAccess) X(cuCtxDisablePeerAccess) \
    X(cuModuleLoad) X(cuModuleLoadData) X(cuModuleLoadDataEx) X(cuModuleLoadFatBinary) \
    X(cuModuleGetFunction) X(cuModuleGetGlobal_v2) X(cuModuleGetLoadingMode) X(cuModuleUnload) \
    X(cuModuleGetTexRef) X(cuModuleGetSurfRef) \
    X(cuMemGetInfo_v2) X(cuMemAlloc_v2) X(cuMemAllocPitch_v2) X(cuMemFree_v2) X(cuMemGetAddressRange_v2) \
    X(cuMemAllocHost_v2) X(cuMemHostAlloc) X(cuMemFreeHost) X(cuMemHostGetDevicePointer_v2) X(cuMemHostGetFlags) \
    X(cuMemHostRegister_v2) X(cuMemHostUnregister) X(cuMemAllocManaged) \
    X(cuMemcpy) X(cuMemcpyHtoD_v2) X(cuMemcpyDtoH_v2) X(cuMemcpyDtoD_v2) X(cuMemcpyPeer) X(cuMemcpyAsync) \
    X(cuMemcpyHtoDAsync_v2) X(cuMemcpyDtoHAsync_v2) X(cuMemcpyDtoDAsync_v2) X(cuMemcpyPeerAsync) \
    X(cuMemsetD8_v2) X(cuMemsetD16_v2) X(cuMemsetD32_v2) X(cuMemsetD8Async) X(cuMemsetD16Async) X(cuMemsetD32Async) \
    X(cuMemPrefetchAsync) X(cuMemAdvise) X(cuMemAllocAsync) X(cuMemFreeAsync) \
    X(cuPointerGetAttribute) X(cuPointerGetAttributes) X(cuPointerSetAttribute) \
    X(cuLaunchKernel) X(cuLaunchKernelEx) X(cuLaunchCooperativeKernel) X(cuLaunchHostFunc) \
    X(cuFuncGetAttribute) X(cuFuncSetAttribute) X(cuFuncSetCacheConfig) X(cuFuncSetSharedMemConfig) \
    X(cuFuncGetModule) X(cuFuncGetName) \
    X(cuOccupancyMaxActiveBlocksPerMultiprocessor) X(cuOccupancyMaxActiveBlocksPerMultiprocessorWithFlags) \
    X(cuOccupancyMaxPotentialBlockSize) X(cuOccupancyMaxPotentialBlockSizeWithFlags) \
    X(cuOccupancyAvailableDynamicSMemPerBlock) \
    X(cuStreamCreate) X(cuStreamCreateWithPriority) X(cuStreamDestroy_v2) X(cuStreamSynchronize) X(cuStreamQuery) \
    X(cuStreamWaitEvent) X(cuStreamAddCallback) X(cuStreamGetFlags) X(cuStreamGetPriority) X(cuStreamGetCtx) \
    X(cuStreamGetId) X(cuStreamAttachMemAsync) X(cuStreamIsCapturing) X(cuThreadExchangeStreamCaptureMode) \
    X(cuEventCreate) X(cuEventRecord) X(cuEventRecordWithFlags) X(cuEventQuery) X(cuEventSynchronize) \
    X(cuEventDestroy_v2) X(cuEventElapsedTime) \
    X(cuProfilerInitialize) X(cuProfilerStart) X(cuProfilerStop)

/* base-name -> newest-version alias entries (what cuGetProcAddress(base, ver>=X) must return) */
#define ALIAS_LIST(X) \
    X("cuDeviceTotalMem", cuDeviceTotalMem_v2) \
    X("cuCtxCreate", cuCtxCreate_v2) X("cuCtxDestroy", cuCtxDestroy_v2) \
    X("cuCtxPushCurrent", cuCtxPushCurrent_v2) X("cuCtxPopCurrent", cuCtxPopCurrent_v2) \
    X("cuDevicePrimaryCtxRelease", cuDevicePrimaryCtxRelease_v2) \
    X("cuDevicePrimaryCtxReset", cuDevicePrimaryCtxReset_v2) \
    X("cuDevicePrimaryCtxSetFlags", cuDevicePrimaryCtxSetFlags_v2) \
    X("cuModuleGetGlobal", cuModuleGetGlobal_v2) \
    X("cuMemGetInfo", cuMemGetInfo_v2) X("cuMemAlloc", cuMemAlloc_v2) X("cuMemAllocPitch", cuMemAllocPitch_v2) \
    X("cuMemFree", cuMemFree_v2) X("cuMemGetAddressRange", cuMemGetAddressRange_v2) \
    X("cuMemAllocHost", cuMemAllocHost_v2) X("cuMemHostGetDevicePointer", cuMemHostGetDevicePointer_v2) \
    X("cuMemHostRegister", cuMemHostRegister_v2) \
    X("cuMemcpyHtoD", cuMemcpyHtoD_v2) X("cuMemcpyDtoH", cuMemcpyDtoH_v2) X("cuMemcpyDtoD", cuMemcpyDtoD_v2) \
    X("cuMemcpyHtoDAsync", cuMemcpyHtoDAsync_v2) X("cuMemcpyDtoHAsync", cuMemcpyDtoHAsync_v2) \
    X("cuMemcpyDtoDAsync", cuMemcpyDtoDAsync_v2) \
    X("cuMemsetD8", cuMemsetD8_v2) X("cuMemsetD16", cuMemsetD16_v2) X("cuMemsetD32", cuMemsetD32_v2) \
    X("cuStreamDestroy", cuStreamDestroy_v2) X("cuEventDestroy", cuEventDestroy_v2) \
    X("cuStreamWaitValue32", cuStreamWaitValue32_v2) X("cuStreamWriteValue32", cuStreamWriteValue32_v2) \
    X("cuLinkCreate", cuLinkCreate_v2) X("cuLinkAddData", cuLinkAddData_v2) X("cuLinkAddFile", cuLinkAddFile_v2) \
    X("cuGraphInstantiate", cuGraphInstantiate) X("cuGraphExecUpdate", cuGraphExecUpdate_v2)

static const ProcEntry g_proc_table[] = {
#define TAB_REAL(n)   { #n, (void*)n },
#define TAB_STUB(n)   { #n, (void*)n },
#define TAB_ALIAS(s,n){ s,  (void*)n },
    REAL_LIST(TAB_REAL)
    STUB_LIST(TAB_STUB)
    ALIAS_LIST(TAB_ALIAS)
#undef TAB_REAL
#undef TAB_STUB
#undef TAB_ALIAS
};
static const size_t g_proc_count = sizeof(g_proc_table) / sizeof(g_proc_table[0]);

CUresult cuGetProcAddress(const char* symbol, void** pfn, int cudaVersion, cuuint64_t flags) {
    (void)cudaVersion; (void)flags;
    if (!symbol || !pfn) return CUDA_ERROR_INVALID_VALUE;
    for (size_t i = 0; i < g_proc_count; i++)
        if (strcmp(symbol, g_proc_table[i].name) == 0) { *pfn = g_proc_table[i].fn; return CUDA_SUCCESS; }
    *pfn = NULL;
    return CUDA_ERROR_NOT_FOUND;
}
CUresult cuGetProcAddress_v2(const char* symbol, void** pfn, int cudaVersion, cuuint64_t flags,
                             CUdriverProcAddressQueryResult* status) {
    CUresult r = cuGetProcAddress(symbol, pfn, cudaVersion, flags);
    if (status) *status = (r == CUDA_SUCCESS) ? CU_GET_PROC_ADDRESS_SUCCESS : CU_GET_PROC_ADDRESS_SYMBOL_NOT_FOUND;
    return r;
}

/* =================================================================================================
 * ABI-exported aliases so plain dlsym("cuMemAlloc") etc. resolve to the versioned impl, and the
 * per-thread-default-stream (_ptds/_ptsz) variants exist as real symbols (dd's executor is
 * synchronous, so the default-stream variant is identical to the base entry point).
 * ================================================================================================= */
#define ALIAS(newname, target, ...) \
    CUresult newname(__VA_ARGS__) __attribute__((alias(#target)))

/* base (non-_v2) names */
ALIAS(cuDeviceTotalMem, cuDeviceTotalMem_v2, size_t*, CUdevice);
ALIAS(cuCtxCreate, cuCtxCreate_v2, CUcontext*, unsigned int, CUdevice);
ALIAS(cuCtxDestroy, cuCtxDestroy_v2, CUcontext);
ALIAS(cuCtxPushCurrent, cuCtxPushCurrent_v2, CUcontext);
ALIAS(cuCtxPopCurrent, cuCtxPopCurrent_v2, CUcontext*);
ALIAS(cuDevicePrimaryCtxRelease, cuDevicePrimaryCtxRelease_v2, CUdevice);
ALIAS(cuDevicePrimaryCtxReset, cuDevicePrimaryCtxReset_v2, CUdevice);
ALIAS(cuDevicePrimaryCtxSetFlags, cuDevicePrimaryCtxSetFlags_v2, CUdevice, unsigned int);
ALIAS(cuModuleGetGlobal, cuModuleGetGlobal_v2, CUdeviceptr*, size_t*, CUmodule, const char*);
ALIAS(cuMemGetInfo, cuMemGetInfo_v2, size_t*, size_t*);
ALIAS(cuMemAlloc, cuMemAlloc_v2, CUdeviceptr*, size_t);
ALIAS(cuMemAllocPitch, cuMemAllocPitch_v2, CUdeviceptr*, size_t*, size_t, size_t, unsigned int);
ALIAS(cuMemFree, cuMemFree_v2, CUdeviceptr);
ALIAS(cuMemGetAddressRange, cuMemGetAddressRange_v2, CUdeviceptr*, size_t*, CUdeviceptr);
ALIAS(cuMemAllocHost, cuMemAllocHost_v2, void**, size_t);
ALIAS(cuMemHostGetDevicePointer, cuMemHostGetDevicePointer_v2, CUdeviceptr*, void*, unsigned int);
ALIAS(cuMemHostRegister, cuMemHostRegister_v2, void*, size_t, unsigned int);
ALIAS(cuMemcpyHtoD, cuMemcpyHtoD_v2, CUdeviceptr, const void*, size_t);
ALIAS(cuMemcpyDtoH, cuMemcpyDtoH_v2, void*, CUdeviceptr, size_t);
ALIAS(cuMemcpyDtoD, cuMemcpyDtoD_v2, CUdeviceptr, CUdeviceptr, size_t);
ALIAS(cuMemcpyHtoDAsync, cuMemcpyHtoDAsync_v2, CUdeviceptr, const void*, size_t, CUstream);
ALIAS(cuMemcpyDtoHAsync, cuMemcpyDtoHAsync_v2, void*, CUdeviceptr, size_t, CUstream);
ALIAS(cuMemsetD8, cuMemsetD8_v2, CUdeviceptr, unsigned char, size_t);
ALIAS(cuMemsetD16, cuMemsetD16_v2, CUdeviceptr, unsigned short, size_t);
ALIAS(cuMemsetD32, cuMemsetD32_v2, CUdeviceptr, unsigned int, size_t);
ALIAS(cuStreamDestroy, cuStreamDestroy_v2, CUstream);
ALIAS(cuEventDestroy, cuEventDestroy_v2, CUevent);

/* per-thread default stream (_ptds / _ptsz) variants — identical under a synchronous executor */
ALIAS(cuLaunchKernel_ptsz, cuLaunchKernel, CUfunction, unsigned, unsigned, unsigned, unsigned, unsigned, unsigned, unsigned, CUstream, void**, void**);
ALIAS(cuLaunchKernelEx_ptsz, cuLaunchKernelEx, const CUlaunchConfig*, CUfunction, void**, void**);
ALIAS(cuLaunchCooperativeKernel_ptsz, cuLaunchCooperativeKernel, CUfunction, unsigned, unsigned, unsigned, unsigned, unsigned, unsigned, unsigned, CUstream, void**);
ALIAS(cuLaunchHostFunc_ptsz, cuLaunchHostFunc, CUstream, CUhostFn, void*);
ALIAS(cuStreamSynchronize_ptsz, cuStreamSynchronize, CUstream);
ALIAS(cuStreamQuery_ptsz, cuStreamQuery, CUstream);
ALIAS(cuStreamWaitEvent_ptsz, cuStreamWaitEvent, CUstream, CUevent, unsigned int);
ALIAS(cuStreamAddCallback_ptsz, cuStreamAddCallback, CUstream, CUstreamCallback, void*, unsigned int);
ALIAS(cuStreamAttachMemAsync_ptsz, cuStreamAttachMemAsync, CUstream, CUdeviceptr, size_t, unsigned int);
ALIAS(cuStreamGetFlags_ptsz, cuStreamGetFlags, CUstream, unsigned int*);
ALIAS(cuStreamGetPriority_ptsz, cuStreamGetPriority, CUstream, int*);
ALIAS(cuEventRecord_ptsz, cuEventRecord, CUevent, CUstream);
ALIAS(cuEventRecordWithFlags_ptsz, cuEventRecordWithFlags, CUevent, CUstream, unsigned int);
ALIAS(cuMemcpyAsync_ptsz, cuMemcpyAsync, CUdeviceptr, CUdeviceptr, size_t, CUstream);
ALIAS(cuMemcpyHtoDAsync_v2_ptds, cuMemcpyHtoDAsync_v2, CUdeviceptr, const void*, size_t, CUstream);
ALIAS(cuMemcpyDtoHAsync_v2_ptds, cuMemcpyDtoHAsync_v2, void*, CUdeviceptr, size_t, CUstream);
ALIAS(cuMemcpyDtoDAsync_v2_ptds, cuMemcpyDtoDAsync_v2, CUdeviceptr, CUdeviceptr, size_t, CUstream);
ALIAS(cuMemcpyHtoD_v2_ptds, cuMemcpyHtoD_v2, CUdeviceptr, const void*, size_t);
ALIAS(cuMemcpyDtoH_v2_ptds, cuMemcpyDtoH_v2, void*, CUdeviceptr, size_t);
ALIAS(cuMemcpyDtoD_v2_ptds, cuMemcpyDtoD_v2, CUdeviceptr, CUdeviceptr, size_t);
ALIAS(cuMemcpy_ptds, cuMemcpy, CUdeviceptr, CUdeviceptr, size_t);
ALIAS(cuMemsetD8_v2_ptds, cuMemsetD8_v2, CUdeviceptr, unsigned char, size_t);
ALIAS(cuMemsetD16_v2_ptds, cuMemsetD16_v2, CUdeviceptr, unsigned short, size_t);
ALIAS(cuMemsetD32_v2_ptds, cuMemsetD32_v2, CUdeviceptr, unsigned int, size_t);
ALIAS(cuMemsetD8Async_ptsz, cuMemsetD8Async, CUdeviceptr, unsigned char, size_t, CUstream);
ALIAS(cuMemsetD32Async_ptsz, cuMemsetD32Async, CUdeviceptr, unsigned int, size_t, CUstream);
ALIAS(cuMemPrefetchAsync_ptsz, cuMemPrefetchAsync, CUdeviceptr, size_t, CUdevice, CUstream);
ALIAS(cuMemAllocAsync_ptsz, cuMemAllocAsync, CUdeviceptr*, size_t, CUstream);
ALIAS(cuMemFreeAsync_ptsz, cuMemFreeAsync, CUdeviceptr, CUstream);
