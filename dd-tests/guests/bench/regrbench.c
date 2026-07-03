// regrbench.c — self-timed guest kernels reproducing the v0.9.19 dd-amd redis/python regression
// (redis SET −25%, GET −17%; python-cpu +10% vs v0.9.18) WITHOUT the daemon/image flakiness.
//
// Protocol identical to microbench.c: prints "KERNEL <name> <ns>" per kernel; startup excluded.
// Compile with -fno-builtin so memcpy/strlen/strcmp/memchr resolve to REAL glibc ifunc'd calls
// (the CPUID-model / ERMS / FSRM wave-1 changes steer exactly those ifuncs).
//
// Kernels:
//   rset     redis-SET-ish: RESP parse (memchr \r\n) + sds-like small memcpy + siphash-ish key
//            hash + chained dict insert (malloc + memcmp) — small-string/small-copy heavy.
//   rget     redis-GET-ish: hash + dict lookup (memcmp) + reply build (small memcpy + int fmt).
//   pyfloat  CPython-float-ish: dependent scalar-double chain (scalar SSE on x86) + branchy.
//   pydict   CPython-dict-ish: string-keyed open-addressing table, interning memcmp, strlen.
//   dispatch bytecode-interpreter-ish: megamorphic indirect calls through a handler table.
//   smallcpy glibc memcpy sweep 3..256 B (redis-benchmark sizes), runtime lengths.
//   bigcpy   glibc memcpy 4 KiB / 64 KiB — the ERMS funnel win must stay 3.6-4.7x.
//   strops   strlen/strcmp/memchr/memcmp over short SDS-ish strings.

#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static inline uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static volatile uint64_t sink_u64;
static volatile double sink_f64;

// opaque length: keeps gcc from constant-folding memcpy/strlen sizes even with -fno-builtin off
static volatile size_t vlen;

// ── tiny siphash-ish mixer over bytes (stands in for redis siphash1-3 key hashing) ──
static uint64_t byte_hash(const void *p, size_t n) {
    const uint8_t *b = p;
    uint64_t h = 0x9e3779b97f4a7c15ull;
    for (size_t i = 0; i < n; i++) {
        h ^= b[i];
        h *= 0x100000001b3ull;
        h ^= h >> 29;
    }
    return h;
}

// ─────────────────────────── rset / rget: redis-protocol-ish ───────────────────────────

#define DICT_SZ (1u << 17) // 128k buckets
struct dent { struct dent *next; uint32_t klen, vlen; char kv[]; };
static struct dent *g_tab[DICT_SZ];

// build the RESP SET command for key i into buf; returns length. Mirrors redis-benchmark:
// *3\r\n$3\r\nSET\r\n$16\r\nkey:0000000000NN\r\n$3\r\nxxx\r\n
static int mk_set_cmd(char *buf, unsigned i) {
    char key[32], val[8];
    int kl = snprintf(key, sizeof key, "key:%012u", i % 100000u);
    int vl = snprintf(val, sizeof val, "%03u", i % 1000u);
    return snprintf(buf, 128, "*3\r\n$3\r\nSET\r\n$%d\r\n%s\r\n$%d\r\n%s\r\n", kl, key, vl, val);
}

// RESP parse: memchr-driven, small memcpy of each bulk into a stack sds — like redis's
// processMultibulkBuffer + sdsnewlen.
static int parse_resp(const char *buf, int len, char out[3][64], int outlen[3]) {
    const char *p = buf, *end = buf + len;
    if (*p++ != '*') return -1;
    int argc = atoi(p);
    p = memchr(p, '\n', (size_t)(end - p));
    if (!p++) return -1;
    for (int a = 0; a < argc; a++) {
        if (*p++ != '$') return -1;
        int bl = atoi(p);
        p = memchr(p, '\n', (size_t)(end - p));
        if (!p++) return -1;
        memcpy(out[a], p, (size_t)bl); // the sdsnewlen small copy
        out[a][bl] = 0;
        outlen[a] = bl;
        p += bl + 2;
    }
    return argc;
}

static void dict_set(const char *k, int kl, const char *v, int vl) {
    uint64_t h = byte_hash(k, (size_t)kl);
    struct dent **slot = &g_tab[h & (DICT_SZ - 1)];
    for (struct dent *e = *slot; e; e = e->next)
        if (e->klen == (uint32_t)kl && memcmp(e->kv, k, (size_t)kl) == 0) {
            if ((uint32_t)vl <= e->vlen) { memcpy(e->kv + e->klen, v, (size_t)vl); e->vlen = (uint32_t)vl; return; }
            return; // keep it simple: no realloc path
        }
    struct dent *e = malloc(sizeof *e + (size_t)kl + 64);
    e->klen = (uint32_t)kl; e->vlen = (uint32_t)vl;
    memcpy(e->kv, k, (size_t)kl);
    memcpy(e->kv + kl, v, (size_t)vl);
    e->next = *slot; *slot = e;
}

static struct dent *dict_get(const char *k, int kl) {
    uint64_t h = byte_hash(k, (size_t)kl);
    for (struct dent *e = g_tab[h & (DICT_SZ - 1)]; e; e = e->next)
        if (e->klen == (uint32_t)kl && memcmp(e->kv, k, (size_t)kl) == 0) return e;
    return NULL;
}

static uint64_t k_rset(void) {
    const uint64_t N = 6000000ull;
    char cmd[128], out[3][64];
    int ol[3];
    uint64_t acc = 0;
    for (uint64_t i = 0; i < N; i++) {
        int len = mk_set_cmd(cmd, (unsigned)i);
        if (parse_resp(cmd, len, out, ol) == 3) {
            dict_set(out[1], ol[1], out[2], ol[2]);
            acc += (uint64_t)strlen(out[1]); // sdslen-ish touch
        }
    }
    sink_u64 = acc;
    return N;
}

static uint64_t k_rget(void) {
    // pre-populate
    char cmd[128], out[3][64];
    int ol[3];
    for (unsigned i = 0; i < 100000u; i++) {
        int len = mk_set_cmd(cmd, i);
        if (parse_resp(cmd, len, out, ol) == 3) dict_set(out[1], ol[1], out[2], ol[2]);
    }
    const uint64_t N = 8000000ull;
    char key[32], reply[128];
    uint64_t acc = 0;
    for (uint64_t i = 0; i < N; i++) {
        int kl = snprintf(key, sizeof key, "key:%012u", (unsigned)(i * 2654435761ull) % 100000u);
        struct dent *e = dict_get(key, kl);
        if (e) { // addReplyBulk-ish: $<len>\r\n<val>\r\n
            int n = snprintf(reply, sizeof reply, "$%u\r\n", e->vlen);
            memcpy(reply + n, e->kv + e->klen, e->vlen);
            memcpy(reply + n + e->vlen, "\r\n", 2);
            acc += (uint64_t)reply[n];
        }
    }
    sink_u64 = acc;
    return N;
}

// ─────────────────────────── pyfloat / pydict: CPython-ish ───────────────────────────

static uint64_t k_pyfloat(void) {
    const uint64_t N = 130000000ull;
    double x = 1.000000001, acc = 0.0;
    for (uint64_t i = 0; i < N; i++) {
        x = x * 1.0000000001 + 0.5;          // scalar SSE mulsd/addsd chain
        double y = x - (double)(uint64_t)x;  // cvttsd2si + cvtsi2sd
        acc += y > 0.5 ? y * 0.25 : y + 0.125;
        if (x > 2.0) x -= 1.0;
    }
    sink_f64 = acc;
    return N;
}

#define PYD_SZ (1u << 15)
struct pyslot { uint64_t hash; char *key; uint64_t val; };
static struct pyslot g_pyd[PYD_SZ];

static uint64_t k_pydict(void) {
    // intern 4096 identifier-like strings
    enum { NKEYS = 4096 };
    static char *interned[NKEYS];
    for (int i = 0; i < NKEYS; i++) {
        char tmp[40];
        int n = snprintf(tmp, sizeof tmp, "attribute_name_%d_of_object", i);
        interned[i] = malloc((size_t)n + 1);
        memcpy(interned[i], tmp, (size_t)n + 1);
    }
    const uint64_t N = 26000000ull;
    uint64_t acc = 0, r = 0x243f6a8885a308d3ull;
    for (uint64_t i = 0; i < N; i++) {
        r ^= r << 13; r ^= r >> 7; r ^= r << 17;
        char *k = interned[r & (NKEYS - 1)];
        size_t kl = strlen(k);
        uint64_t h = byte_hash(k, kl);
        size_t j = h & (PYD_SZ - 1), perturb = h;
        for (;;) {
            struct pyslot *s = &g_pyd[j];
            if (!s->key) { s->key = k; s->hash = h; s->val = i; break; }
            if (s->hash == h && (s->key == k || strcmp(s->key, k) == 0)) { s->val += 1; acc += s->val; break; }
            perturb >>= 5;
            j = (j * 5 + perturb + 1) & (PYD_SZ - 1);
        }
    }
    sink_u64 = acc;
    return N;
}

// ─────────────────────────── dispatch: interpreter-ish indirect calls ───────────────────────────

typedef uint64_t (*hfn)(uint64_t);
static uint64_t h_add(uint64_t x) { return x + 0x9e3779b9u; }
static uint64_t h_xor(uint64_t x) { return x ^ (x << 7); }
static uint64_t h_mul(uint64_t x) { return x * 2654435761ull; }
static uint64_t h_shr(uint64_t x) { return x ^ (x >> 11); }
static uint64_t h_rot(uint64_t x) { return (x << 13) | (x >> 51); }
static uint64_t h_sub(uint64_t x) { return x - 0x61c88647u; }
static uint64_t h_and(uint64_t x) { return x & (x >> 3 | 0xffffull); }
static uint64_t h_or(uint64_t x) { return x | (x << 5) >> 9; }

static uint64_t k_dispatch(void) {
    static const hfn tab[8] = { h_add, h_xor, h_mul, h_shr, h_rot, h_sub, h_and, h_or };
    const uint64_t N = 220000000ull;
    uint64_t x = 0xdeadbeefcafebabeull, acc = 0;
    for (uint64_t i = 0; i < N; i++) {
        x = tab[x & 7](x);   // data-dependent megamorphic indirect call (IBTC path)
        acc += x;
    }
    sink_u64 = acc;
    return N;
}

// ─────────────────────────── smallcpy / bigcpy / strops ───────────────────────────

static uint64_t k_smallcpy(void) {
    static const size_t sizes[] = { 3, 8, 16, 24, 32, 48, 64, 96, 128, 192, 256 };
    enum { NS = sizeof sizes / sizeof sizes[0] };
    char src[512], dst[512];
    for (int i = 0; i < 512; i++) src[i] = (char)(i * 7);
    const uint64_t N = 30000000ull;
    uint64_t acc = 0;
    for (uint64_t i = 0; i < N; i++) {
        vlen = sizes[i % NS];
        memcpy(dst, src, vlen);          // real glibc call, runtime length
        acc += (uint64_t)(uint8_t)dst[0];
        src[i & 255] ^= 1;               // defeat any value caching
    }
    sink_u64 = acc;
    return N;
}

static uint64_t k_bigcpy(void) {
    enum { BIG = 64 * 1024, MED = 4096 };
    char *a = malloc(BIG), *b = malloc(BIG);
    memset(a, 0x5a, BIG);
    const uint64_t N = 120000ull;
    uint64_t acc = 0;
    for (uint64_t i = 0; i < N; i++) {
        vlen = (i & 1) ? BIG : MED;
        memcpy(b, a, vlen);
        acc += (uint64_t)(uint8_t)b[i & 4095];
        a[i & 4095] ^= 1;
    }
    sink_u64 = acc;
    return N;
}

static uint64_t k_strops(void) {
    enum { NSTR = 256 };
    static char *strs[NSTR];
    for (int i = 0; i < NSTR; i++) {
        char tmp[64];
        int n = snprintf(tmp, sizeof tmp, "key:%012d:field_%d_suffix", i, i * 31);
        strs[i] = malloc((size_t)n + 1);
        memcpy(strs[i], tmp, (size_t)n + 1);
    }
    const uint64_t N = 22000000ull;
    uint64_t acc = 0, r = 0x2545f4914f6cdd1dull;
    for (uint64_t i = 0; i < N; i++) {
        r ^= r << 13; r ^= r >> 7; r ^= r << 17;
        char *a = strs[r & (NSTR - 1)], *b = strs[(r >> 8) & (NSTR - 1)];
        acc += strlen(a);
        acc += (uint64_t)(strcmp(a, b) != 0);
        const char *c = memchr(a, ':', 32);
        acc += c ? (uint64_t)(c - a) : 0;
        acc += (uint64_t)(memcmp(a, b, 16) == 0);
    }
    sink_u64 = acc;
    return N;
}

// ─────────────────────────────────── driver ───────────────────────────────────

struct kern { const char *name; uint64_t (*fn)(void); };
static const struct kern KERNELS[] = {
    { "rset", k_rset },      { "rget", k_rget },
    { "pyfloat", k_pyfloat },{ "pydict", k_pydict },
    { "dispatch", k_dispatch },
    { "smallcpy", k_smallcpy }, { "bigcpy", k_bigcpy }, { "strops", k_strops },
};

int main(int argc, char **argv) {
    const char *want = argc > 1 ? argv[1] : "all";
    int matched = 0;
    for (size_t i = 0; i < sizeof KERNELS / sizeof KERNELS[0]; i++) {
        if (strcmp(want, "all") != 0 && strcmp(want, KERNELS[i].name) != 0) continue;
        matched = 1;
        uint64_t t0 = now_ns();
        KERNELS[i].fn();
        uint64_t t1 = now_ns();
        printf("KERNEL %s %llu\n", KERNELS[i].name, (unsigned long long)(t1 - t0));
        fflush(stdout);
    }
    if (!matched) { fprintf(stderr, "unknown kernel %s\n", want); return 1; }
    return 0;
}
