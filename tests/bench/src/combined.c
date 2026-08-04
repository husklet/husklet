/*
 * combined_bench.c - single-process, self-timing multi-phase benchmark.
 *
 * Runs many workload phases sequentially in ONE process. Each phase is timed
 * FROM INSIDE THE GUEST: on aarch64 with the ARM generic timer (cntvct_el0 /
 * cntfrq_el0), and on every other arch (x86_64) with clock_gettime(
 * CLOCK_MONOTONIC). The SAME source therefore builds and self-times on both
 * aarch64 and x86_64 cells. The reported per-phase microseconds reflect only
 * in-guest execution: engine startup and worker fork are paid once, before any
 * phase runs, and are excluded from every phase measurement. Because the timing
 * is inside the guest, the per-phase us are directly comparable across backends
 * (native / qemu-user / hl-engine / docker) -- process/startup/isolation cost is
 * automatically excluded.
 *
 * Output: one line per phase, e.g.  "PHASE compute us=1234 ok=1"
 * Default iteration counts are fixed and each phase is warmed before timing.
 * `--divisor N` bounds expensive nested-engine probes, and `--phase NAME`
 * isolates one bottleneck. Checksums remain comparable when invocations use
 * the same arch and divisor.
 *
 * The sqlite phase is compiled only when HL_BENCH_SQLITE is defined (it links a
 * static libsqlite3 for the target arch, supplied by nix for both arches); the
 * runner tolerates its absence.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#if defined(__APPLE__)
#include <pthread.h>
#endif

/* Compiler barrier: force VAL to be treated as opaque (its value unknown to the
 * optimizer), preventing constant-folding / elision of work that produces it.
 * Portable across gcc and clang on Linux and Darwin. */
#if defined(__GNUC__) || defined(__clang__)
#define HL_OPAQUE(val) __asm__ __volatile__("" : "+r"(val))
#else
#define HL_OPAQUE(val) ((void)(val))
#endif

#ifdef HL_BENCH_SQLITE
#include <sqlite3.h>
#endif

/* ---- portable in-guest timing ------------------------------------------- *
 * aarch64: ARM generic timer (cntvct_el0 ticks, cntfrq_el0 Hz).
 * else   : CLOCK_MONOTONIC nanoseconds (freq == 1e9), so the same source
 *          self-times on x86_64 too.
 */
#if defined(__aarch64__)
static inline uint64_t timer_count(void) {
    uint64_t v;
    __asm__ __volatile__("mrs %0, cntvct_el0" : "=r"(v));
    return v;
}
static inline uint64_t timer_freq(void) {
    uint64_t v;
    __asm__ __volatile__("mrs %0, cntfrq_el0" : "=r"(v));
    return v;
}
#else
#include <time.h>
static inline uint64_t timer_count(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * UINT64_C(1000000000) + (uint64_t)ts.tv_nsec;
}
static inline uint64_t timer_freq(void) { return UINT64_C(1000000000); }
#endif

static uint64_t g_freq;

/* Convert an elapsed tick count to microseconds. */
static uint64_t ticks_to_us(uint64_t ticks) {
    return (ticks * UINT64_C(1000000)) / g_freq;
}

/* Sink to keep the optimizer from deleting computed results. */
static volatile uint64_t g_sink;

/* ---------------- phase: compute (integer + float busyloop) ---------------- */

static uint64_t phase_compute(unsigned iters) {
    /* Ported shape of a busyloop: long dependency chain, int + float mix. */
    uint64_t acc = 1469598103934665603ULL;
    double f = 1.0;
    for (unsigned i = 0; i < iters; ++i) {
        acc ^= (uint64_t)i;
        acc *= 1099511628211ULL;
        acc = (acc << 13) | (acc >> 51);
        f = f * 1.0000001 + 0.5;
        if (f > 1.0e6) f -= 1.0e6;
    }
    return acc ^ (uint64_t)f;
}

/* ---------------- phase: integer div/mul-heavy ---------------- */

static uint64_t phase_intdiv(unsigned iters) {
    uint64_t acc = 0xdeadbeefcafef00dULL;
    for (unsigned i = 0; i < iters; ++i) {
        uint64_t x = acc + i + 1;
        acc += x / (((uint64_t)(i % 1000)) + 3);
        acc ^= x * UINT64_C(2654435761);
        acc += x % (((uint64_t)(i % 777)) + 5);
        acc = acc * 6364136223846793005ULL + 1442695040888963407ULL;
    }
    return acc;
}

/* ---------------- phase: float / SIMD (auto-vectorizes to NEON / SSE) ------- */

static uint64_t phase_float_simd(unsigned iters) {
    enum { N = 4096 };
    static float a[N], b[N], c[N];
    for (int i = 0; i < N; ++i) {
        a[i] = (float)i * 0.5f + 1.0f;
        b[i] = (float)i * 0.25f + 2.0f;
    }
    uint64_t acc = 0;
    for (unsigned it = 0; it < iters; ++it) {
        float bias = (float)(it % 1000) * 0.001f;
        for (int i = 0; i < N; ++i) {
            c[i] = a[i] * b[i] + bias;
        }
        /* Truncate to integer so the checksum is bit-stable across backends. */
        for (int i = 0; i < N; i += 512) acc += (uint64_t)c[i];
    }
    return acc;
}

/* ---------------- phase: crypto (AES-128 block encryption) ----------------
 * Exercises the guest's AES instructions -- x86 AES-NI (aesenc/aesenclast),
 * aarch64 (AESE/AESMC) -- which the engine inlines to the host crypto
 * extension. A CBC-style chain (output feeds the next block's input) blocks
 * the compiler from hoisting the work. The checksum is stable across envs for
 * a given arch (same binary). If the toolchain lacks crypto intrinsics the
 * phase degrades to a portable scalar mix so the bench still builds/runs. */
#if defined(__x86_64__) && (defined(__GNUC__) || defined(__clang__))
#include <wmmintrin.h>
#include <emmintrin.h>
__attribute__((target("aes,sse2")))
static uint64_t phase_crypto(unsigned iters) {
    enum { BLK = 1024 };
    static __m128i buf[BLK];
    __m128i rk[11];
    for (int r = 0; r < 11; ++r)
        rk[r] = _mm_set_epi32(0x01020304 + r, 0x05060708 + r, 0x090a0b0c + r, 0x0d0e0f10 + r);
    __m128i seed = _mm_set_epi32((int)0xdeadbeef, 0x12345678, (int)0x9abcdef0, 0x0f1e2d3c);
    for (int i = 0; i < BLK; ++i) {
        buf[i] = seed;
        seed = _mm_add_epi32(seed, _mm_set1_epi32((int)0x9e3779b9));
    }
    __m128i chain = _mm_setzero_si128();
    for (unsigned it = 0; it < iters; ++it) {
        for (int i = 0; i < BLK; ++i) {
            __m128i b = _mm_xor_si128(_mm_xor_si128(buf[i], chain), rk[0]);
            b = _mm_aesenc_si128(b, rk[1]);
            b = _mm_aesenc_si128(b, rk[2]);
            b = _mm_aesenc_si128(b, rk[3]);
            b = _mm_aesenc_si128(b, rk[4]);
            b = _mm_aesenc_si128(b, rk[5]);
            b = _mm_aesenc_si128(b, rk[6]);
            b = _mm_aesenc_si128(b, rk[7]);
            b = _mm_aesenc_si128(b, rk[8]);
            b = _mm_aesenc_si128(b, rk[9]);
            b = _mm_aesenclast_si128(b, rk[10]);
            buf[i] = b;
            chain = b;
        }
    }
    uint64_t acc = 0;
    for (int i = 0; i < BLK; i += 64) acc += (uint64_t)_mm_cvtsi128_si64(buf[i]);
    return acc;
}
#elif defined(__aarch64__) && (defined(__GNUC__) || defined(__clang__))
#include <arm_neon.h>
__attribute__((target("arch=armv8-a+crypto")))
static uint64_t phase_crypto(unsigned iters) {
    enum { BLK = 1024 };
    static uint8x16_t buf[BLK];
    uint8x16_t rk[11];
    for (int r = 0; r < 11; ++r) {
        uint8_t k[16];
        for (int j = 0; j < 16; ++j) k[j] = (uint8_t)(r * 16 + j + 1);
        rk[r] = vld1q_u8(k);
    }
    for (int i = 0; i < BLK; ++i) {
        uint8_t s[16];
        for (int j = 0; j < 16; ++j) s[j] = (uint8_t)(i * 7 + j * 13 + 1);
        buf[i] = vld1q_u8(s);
    }
    uint8x16_t chain = vdupq_n_u8(0);
    for (unsigned it = 0; it < iters; ++it) {
        for (int i = 0; i < BLK; ++i) {
            uint8x16_t b = veorq_u8(buf[i], chain);
            b = vaesmcq_u8(vaeseq_u8(b, rk[0]));
            b = vaesmcq_u8(vaeseq_u8(b, rk[1]));
            b = vaesmcq_u8(vaeseq_u8(b, rk[2]));
            b = vaesmcq_u8(vaeseq_u8(b, rk[3]));
            b = vaesmcq_u8(vaeseq_u8(b, rk[4]));
            b = vaesmcq_u8(vaeseq_u8(b, rk[5]));
            b = vaesmcq_u8(vaeseq_u8(b, rk[6]));
            b = vaesmcq_u8(vaeseq_u8(b, rk[7]));
            b = vaesmcq_u8(vaeseq_u8(b, rk[8]));
            b = vaeseq_u8(b, rk[9]);
            b = veorq_u8(b, rk[10]);
            buf[i] = b;
            chain = b;
        }
    }
    uint64_t acc = 0;
    for (int i = 0; i < BLK; i += 64) acc += vgetq_lane_u64(vreinterpretq_u64_u8(buf[i]), 0);
    return acc;
}
#else
static uint64_t phase_crypto(unsigned iters) {
    enum { BLK = 1024 };
    static uint64_t buf[BLK * 2];
    for (int i = 0; i < BLK * 2; ++i) buf[i] = (uint64_t)(i * 0x9e3779b97f4a7c15ULL + 1);
    uint64_t chain = 0;
    for (unsigned it = 0; it < iters; ++it)
        for (int i = 0; i < BLK * 2; ++i) {
            uint64_t x = buf[i] ^ chain;
            x ^= x >> 33; x *= 0xff51afd7ed558ccdULL; x ^= x >> 33;
            buf[i] = x; chain = x;
        }
    uint64_t acc = 0;
    for (int i = 0; i < BLK * 2; i += 128) acc += buf[i];
    return acc;
}
#endif

/* ---------------- phase: atomics / CAS loop ---------------- */

static uint64_t phase_atomics(unsigned iters) {
    uint64_t v = 0;
    for (unsigned i = 0; i < iters; ++i) {
        uint64_t old, neu;
        do {
            old = __atomic_load_n(&v, __ATOMIC_RELAXED);
            neu = old + ((i & 7) + 1);
        } while (!__atomic_compare_exchange_n(&v, &old, neu, 0, __ATOMIC_SEQ_CST,
                                              __ATOMIC_RELAXED));
        __atomic_fetch_add(&v, 1, __ATOMIC_SEQ_CST);
    }
    return v;
}

/* ---------------- phase: malloc / free churn ---------------- */

static uint64_t phase_malloc(unsigned iters) {
    enum { SLOTS = 64 };
    unsigned char *p[SLOTS];
    memset(p, 0, sizeof(p));
    uint64_t acc = 0;
    for (unsigned i = 0; i < iters; ++i) {
        unsigned s = i % SLOTS;
        free(p[s]);
        size_t sz = ((size_t)(i * 37U) % 4096U) + 16U;
        unsigned char *q = malloc(sz);
        if (q) {
            q[0] = (unsigned char)i;
            q[sz - 1] = (unsigned char)(i >> 3);
            acc += (uint64_t)q[0] + q[sz - 1];
        }
        p[s] = q;
    }
    for (int s = 0; s < SLOTS; ++s) free(p[s]);
    return acc;
}

/* ---------------- phase: string (strlen / strcmp / memmove) ---------------- */

static uint64_t phase_string(unsigned iters) {
    enum { LEN = 1024 };
    static char buf[LEN + 1], buf2[LEN + 1];
    for (int i = 0; i < LEN; ++i) buf[i] = (char)('a' + (i % 26));
    buf[LEN] = '\0';
    uint64_t acc = 0;
    for (unsigned it = 0; it < iters; ++it) {
        acc += strlen(buf);
        memmove(buf2, buf, LEN + 1);
        buf2[it % LEN] = (char)('A' + (it % 26)); /* force a difference */
        int r = strcmp(buf, buf2);
        acc += (r > 0) ? 1U : (r < 0) ? 2U : 0U; /* sign only: portable */
        acc += (unsigned char)buf2[(it * 13U) % LEN];
    }
    return acc;
}

/* ---------------- phase: branch-heavy (unpredictable) ---------------- */

static uint64_t phase_branch(unsigned iters) {
    uint64_t acc = 0, x = 0x9e3779b97f4a7c15ULL;
    for (unsigned i = 0; i < iters; ++i) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17; /* xorshift64: data-dependent, unpredictable */
        if (x & 1) acc += 3;
        else acc -= 1;
        if (x & 2) acc ^= x;
        if ((x & 0xff) > 128) acc += x >> 3;
        else acc += x >> 5;
        switch (x & 7) {
            case 0: acc++; break;
            case 3: acc += 2; break;
            case 5: acc += x & 0xf; break;
            default: acc ^= 1; break;
        }
    }
    return acc;
}

/* ---------------- phase: function-call-heavy (recursion + indirect) --------- *
 * Deep recursion stresses return prediction / the engine's stolen-x30 handling,
 * and the per-iter indirect call stresses the IBTC dispatch path.
 */
static uint64_t fib_rec(unsigned n) {
    if (n < 2) return n;
    return fib_rec(n - 1) + fib_rec(n - 2);
}
typedef uint64_t (*op_fn)(uint64_t, uint64_t);
static uint64_t op_add(uint64_t a, uint64_t b) { return a + b; }
static uint64_t op_xor(uint64_t a, uint64_t b) { return a ^ (b << 1); }
static uint64_t op_mul(uint64_t a, uint64_t b) { return a * (b | 1); }
static uint64_t op_rot(uint64_t a, uint64_t b) {
    unsigned s = (unsigned)(b & 63);
    return (a << s) | (a >> ((64 - s) & 63));
}
static op_fn const OPS[4] = {op_add, op_xor, op_mul, op_rot};

static uint64_t phase_calls(unsigned iters) {
    uint64_t acc = 0;
    for (unsigned i = 0; i < iters; ++i) {
        unsigned n = 18; /* ~8361 calls each iter */
        /* Make n opaque so clang/gcc cannot constant-fold fib_rec(18) to a
         * compile-time value and elide the recursion (~1us artifact on clang).
         * The value is still 18, so the checksum is unchanged. */
        HL_OPAQUE(n);
        acc += fib_rec(n);
        op_fn f = OPS[i & 3]; /* indirect call, rotating target */
        acc = f(acc, i);
    }
    return acc;
}

/* ---------------- phase: TLB / page-walk (large stride over big array) ------ */

static uint64_t phase_tlb(unsigned iters) {
    enum { PAGES = 8192 };
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    if (page == 0) page = 4096;
    size_t sz = (size_t)PAGES * page;
    unsigned char *m = mmap(NULL, sz, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) return 0;
    for (size_t i = 0; i < PAGES; ++i) m[i * page] = (unsigned char)i; /* fault in */
    uint64_t acc = 0;
    for (unsigned it = 0; it < iters; ++it) {
        for (size_t i = 0; i < PAGES; ++i) {
            /* Stride 4099 pages (coprime with PAGES) defeats the TLB. */
            size_t idx = ((i * 4099U) % PAGES) * page;
            acc += m[idx];
            m[idx] = (unsigned char)acc;
        }
    }
    (void)munmap(m, sz);
    return acc;
}

/* ---------------- phase: syscall (thread-id loop) ---------------- *
 * Portable per-OS thread-id read that actually enters the kernel / libc each
 * call (an external, non-inlinable call), so the phase does real syscall-shaped
 * work on both Linux and Darwin:
 *   Linux : gettid (via SYS_gettid; the raw syscall the engine must translate).
 *   Darwin: pthread_threadid_np (SYS_gettid is deprecated/unsupported there).
 * The ok= checksum is thread-id-based and so legitimately differs across OSes;
 * the summary excludes the syscall phase from checksum-divergence checks. */
static uint64_t read_thread_id(void) {
#if defined(__APPLE__)
    uint64_t tid = 0;
    pthread_threadid_np(NULL, &tid);
    return tid;
#elif defined(__linux__) && defined(SYS_gettid)
    return (uint64_t)syscall(SYS_gettid);
#else
    return (uint64_t)getpid();
#endif
}

static uint64_t phase_syscall(unsigned iters) {
    uint64_t checksum = 0;
    for (unsigned i = 0; i < iters; ++i)
        checksum += read_thread_id();
    return checksum;
}

/* ---------------- phase: signal delivery (SIGALRM handler count) ------------ *
 * raise(SIGALRM) synchronously delivers to a counting handler. The count is
 * deterministic (== iters), and each delivery exercises the engine's full
 * signal-frame build/restore path -- a real engine cost surface.
 */
static volatile sig_atomic_t g_sig_count;
static void on_sigalrm(int signo) {
    (void)signo;
    g_sig_count++;
}

static uint64_t phase_signal(unsigned iters) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_sigalrm;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGALRM, &sa, NULL) != 0) return 0;
    g_sig_count = 0;
    for (unsigned i = 0; i < iters; ++i) raise(SIGALRM);
    return (uint64_t)g_sig_count;
}

/* ---------------- phase: mmap (map / touch / mprotect / unmap) ---------------- */

static uint64_t phase_mmap(unsigned iters) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    uint64_t ok = 0;
    for (unsigned i = 0; i < iters; ++i) {
        unsigned char *p = mmap(NULL, page, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) return ok;
        p[i % page] = (unsigned char)i;
        if (mprotect(p, page, PROT_READ) == 0 && p[i % page] == (unsigned char)i &&
            munmap(p, page) == 0)
            ok++;
    }
    return ok;
}

/* ---------------- phase: file (pwrite / pread loop) ---------------- */

static int full_ok_write(int fd, const void *buf, size_t n, off_t off) {
    return pwrite(fd, buf, n, off) == (ssize_t)n;
}

static uint64_t phase_file(unsigned iters) {
    char path[] = "/tmp/hl-combined-file-XXXXXX";
    unsigned char block[4096];
    int fd = mkstemp(path);
    if (fd < 0) return 0;
    (void)unlink(path);
    memset(block, 0x5a, sizeof(block));
    uint64_t ok = 0;
    for (unsigned i = 0; i < iters; ++i) {
        off_t offset = (off_t)(i % 256U) * (off_t)sizeof(block);
        if (!full_ok_write(fd, block, sizeof(block), offset)) break;
        if (pread(fd, block, sizeof(block), offset) == (ssize_t)sizeof(block) &&
            block[0] == 0x5a)
            ok++;
    }
    (void)close(fd);
    return ok;
}

/* ---------------- phase: pipe (write/read round-trip) ---------------- */

static int full_write(int fd, const void *buffer, size_t size) {
    const unsigned char *cursor = buffer;
    while (size != 0) {
        ssize_t written = write(fd, cursor, size);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) return -1;
        cursor += (size_t)written;
        size -= (size_t)written;
    }
    return 0;
}

static int full_read(int fd, void *buffer, size_t size) {
    unsigned char *cursor = buffer;
    while (size != 0) {
        ssize_t got = read(fd, cursor, size);
        if (got < 0 && errno == EINTR) continue;
        if (got <= 0) return -1;
        cursor += (size_t)got;
        size -= (size_t)got;
    }
    return 0;
}

static uint64_t phase_pipe(unsigned iters) {
    int fds[2];
    uint64_t value = UINT64_C(0x123456789abcdef0);
    uint64_t ok = 0;
    if (pipe(fds) != 0) return 0;
    for (unsigned i = 0; i < iters; ++i) {
        if (full_write(fds[1], &value, sizeof(value)) != 0 ||
            full_read(fds[0], &value, sizeof(value)) != 0)
            break;
        ok++;
    }
    (void)close(fds[0]);
    (void)close(fds[1]);
    return ok;
}

/* ---------------- phase: memory (memcpy + memcmp bandwidth) ---------------- */

static uint64_t phase_memory(unsigned iters) {
    enum { BUF = 1 << 20 }; /* 1 MiB */
    unsigned char *a = malloc(BUF);
    unsigned char *b = malloc(BUF);
    if (a == NULL || b == NULL) {
        free(a);
        free(b);
        return 0;
    }
    memset(a, 0x5a, BUF);
    memset(b, 0x00, BUF);
    uint64_t acc = 0;
    for (unsigned i = 0; i < iters; ++i) {
        memcpy(b, a, BUF);
        b[i % BUF] = (unsigned char)i; /* defeat memcpy elision */
        int m = memcmp(a, b, BUF);
        acc += (m > 0) ? 1U : (m < 0) ? 2U : 0U; /* sign only: arch-stable */
        acc += b[(i * 7) % BUF];
    }
    free(a);
    free(b);
    return acc;
}

/* ---------------- phase: sqlite (in-memory insert/select) ---------------- */

#ifdef HL_BENCH_SQLITE
static uint64_t phase_sqlite(unsigned iters) {
    sqlite3 *db;
    if (sqlite3_open(":memory:", &db)) return 0;
    char *err = NULL;
    sqlite3_exec(db, "CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER, s TEXT);",
                 0, 0, &err);
    sqlite3_exec(db, "BEGIN;", 0, 0, &err);
    sqlite3_stmt *st;
    sqlite3_prepare_v2(db, "INSERT INTO t(v,s) VALUES(?,?)", -1, &st, 0);
    for (unsigned i = 1; i <= iters; ++i) {
        char s[32];
        snprintf(s, sizeof(s), "row-%u", (i * 7U) % 100U);
        sqlite3_bind_int(st, 1, (int)((i * i) % 1000003U));
        sqlite3_bind_text(st, 2, s, -1, SQLITE_TRANSIENT);
        sqlite3_step(st);
        sqlite3_reset(st);
    }
    sqlite3_finalize(st);
    sqlite3_exec(db, "COMMIT;", 0, 0, &err);
    sqlite3_stmt *q;
    sqlite3_prepare_v2(db, "SELECT COUNT(*), SUM(v) FROM t", -1, &q, 0);
    sqlite3_step(q);
    uint64_t acc = (uint64_t)sqlite3_column_int(q, 0) ^
                   (uint64_t)sqlite3_column_int64(q, 1);
    sqlite3_finalize(q);
    sqlite3_close(db);
    return acc;
}
#endif

/* ---------------- driver ---------------- */

static uint64_t run_phase(const char *name, uint64_t (*fn)(unsigned), unsigned iters,
                          unsigned warm) {
    /* Warm: a few throwaway iterations so translation/cache is primed. */
    for (unsigned w = 0; w < warm; ++w) g_sink += fn(iters / 20 + 1);
    uint64_t t0 = timer_count();
    uint64_t r = fn(iters);
    uint64_t t1 = timer_count();
    g_sink += r;
    uint64_t us = ticks_to_us(t1 - t0);
    printf("PHASE %s us=%llu ok=%llu\n", name, (unsigned long long)us,
           (unsigned long long)r);
    fflush(stdout);
    return us;
}

static unsigned divided(unsigned count, unsigned divisor) {
    unsigned value = count / divisor;
    return value == 0 ? 1 : value;
}

static int parse_options(int argc, char **argv, unsigned *divisor, const char **phase) {
    for (int index = 1; index < argc; index += 2) {
        if (index + 1 >= argc) return -1;
        if (strcmp(argv[index], "--phase") == 0) {
            if (*phase != NULL || argv[index + 1][0] == '\0') return -1;
            *phase = argv[index + 1];
            continue;
        }
        if (strcmp(argv[index], "--divisor") != 0 || *divisor != 1) return -1;

        char *end = NULL;
        errno = 0;
        unsigned long value = strtoul(argv[index + 1], &end, 10);
        if (errno != 0 || end == argv[index + 1] || *end != '\0' || value == 0 ||
            value > UINT_MAX)
            return -1;
        *divisor = (unsigned)value;
    }
    return 0;
}

int main(int argc, char **argv) {
    unsigned divisor = 1;
    const char *selected_phase = NULL;
    if (parse_options(argc, argv, &divisor, &selected_phase) != 0) {
        fprintf(stderr,
                "usage: %s [--divisor positive-integer] [--phase phase-name]\n",
                argv[0]);
        return 2;
    }

    g_freq = timer_freq();
    if (g_freq == 0) {
        fprintf(stderr, "timer freq is zero\n");
        return 1;
    }
    printf("cntfrq=%llu divisor=%u\n", (unsigned long long)g_freq, divisor);
    fflush(stdout);

    int phase_matched = 0;
#define RUN_PHASE(name, function, count, warm)                                     \
    do {                                                                            \
        if (selected_phase == NULL || strcmp(selected_phase, (name)) == 0) {        \
            run_phase((name), (function), divided((count), divisor), (warm));       \
            phase_matched = 1;                                                       \
        }                                                                            \
    } while (0)

    /* compute appears twice to expose cold-translation vs warm delta. */
    RUN_PHASE("compute_cold", phase_compute, 40000000, 0);
    RUN_PHASE("compute", phase_compute, 40000000, 1);
    /* Iteration counts are sized so every phase runs long enough (~150-500ms
     * on native arm64) to give stable medians and swamp per-call timer/OS
     * noise -- the sub-10ms phases in the first cut were dominated by jitter.
     * Checksums change with the counts but stay consistent across envs (same
     * binary, same iters), so cross-env divergence detection is unaffected. */
    /* CPU / ALU surface */
    RUN_PHASE("intdiv", phase_intdiv, 60000000, 1);
    RUN_PHASE("float_simd", phase_float_simd, 500000, 3);
    RUN_PHASE("crypto", phase_crypto, 120000, 2);
    RUN_PHASE("atomics", phase_atomics, 50000000, 3);
    RUN_PHASE("branch", phase_branch, 60000000, 1);
    RUN_PHASE("calls", phase_calls, 60000, 2);
    /* memory / allocator surface */
    RUN_PHASE("malloc", phase_malloc, 12000000, 2);
    RUN_PHASE("string", phase_string, 5000000, 2);
    RUN_PHASE("memory", phase_memory, 8000, 2);
    RUN_PHASE("tlb", phase_tlb, 3000, 2);
    /* OS / kernel surface */
    RUN_PHASE("syscall", phase_syscall, 10000000, 2);
    RUN_PHASE("signal", phase_signal, 1000000, 2);
    RUN_PHASE("mmap", phase_mmap, 150000, 2);
    RUN_PHASE("file", phase_file, 400000, 2);
    RUN_PHASE("pipe", phase_pipe, 1500000, 2);
#ifdef HL_BENCH_SQLITE
    RUN_PHASE("sqlite", phase_sqlite, 300000, 1);
#endif
    if (selected_phase != NULL && !phase_matched) {
        fprintf(stderr, "unknown phase: %s\n", selected_phase);
        return 2;
    }
#undef RUN_PHASE
    return 0;
}
