// #267 (Erlang/OTP BeamAsm SIGSEGV) regression: MULTITHREADED self-modifying code sharing code pages.
// BeamAsm is a code-GENERATING guest -- it JITs Erlang to machine code at load time, from N scheduler +
// dirty + async threads, packing many small functions per 4KB page. hl translates that guest-generated code;
// when a thread writes a NEW function onto a page another thread has ALREADY executed (so hl has a
// translation for it), hl's SMC hook fires. The original bug: that hook dropped the whole translation map +
// IBTC UNLOCKED, racing every peer thread, and it fired on ANY same-page write (page-granular gate) even
// though no translated byte changed -> non-deterministic SIGSEGV/SIGBUS in the code cache under heavy
// threading. The fix: gate at cache-line (64B) granularity so a same-page APPEND is a no-op, and never drop
// the shared tables while a peer thread is live.
//
// This reproduces the pattern without the 4.7GB OTP image: T threads share one RWX arena of `return imm`
// slots. Every thread grabs slots off a shared bump pointer, writes its slot, flushes the icache, and calls
// it -- so slots from different threads interleave onto the same pages (the append-onto-a-live-page case)
// and, on later passes, threads REWRITE already-translated slots in place (the genuine line-hit case).
//
// A slot's immediate is (slot_index + pass * 7919) & 0xffff: it depends on the PASS, so a rewrite stores
// DIFFERENT bytes at an address that already has a translation, and any missed invalidation returns the
// previous pass's immediate and diverges the total. It stays deterministic because the barrier makes each
// pass call every slot exactly once, whichever thread claimed it. Both ISAs run this: on aarch64 the
// invalidation signal is the `ic ivau` __builtin___clear_cache emits, while on x86-64 the i-cache is
// coherent and clear_cache emits nothing, so the guest STORE is the only signal the engine can publish from.
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

#define THREADS 8
#define SLOTS 2048 // 2048 * 16B = 32KB = 8 pages -> heavy cross-thread page sharing
#define SLOT_BYTES 16
#define PASSES 24
#define PASS_STEP 7919u // makes each pass rewrite every slot with different bytes

static unsigned char *g_arena;
static atomic_uint g_next; // shared bump pointer over slots (the cross-thread page-sharing driver)
static pthread_barrier_t g_bar;
static uint64_t g_sum[THREADS];

// Emit a leaf function `int f(void){ return imm; }` at p.
static void emit_ret_imm(unsigned char *p, uint16_t imm) {
#if defined(__aarch64__)
    uint32_t *w = (uint32_t *)p;
    w[0] = 0x52800000u | ((uint32_t)imm << 5); // movz w0, #imm
    w[1] = 0xd65f03c0u;                        // ret
#elif defined(__x86_64__)
    p[0] = 0xB8; // mov eax, imm32
    p[1] = (unsigned char)(imm & 0xff);
    p[2] = (unsigned char)(imm >> 8);
    p[3] = 0;
    p[4] = 0;
    p[5] = 0xC3; // ret
#else
#error "smc_threads needs an emitter for this ISA"
#endif
}

static void *worker(void *arg) {
    long id = (long)arg;
    uint64_t sum = 0;
    for (unsigned pass = 0; pass < PASSES; pass++) {
        for (;;) {
            unsigned s = atomic_fetch_add_explicit(&g_next, 1, memory_order_relaxed);
            if (s >= SLOTS) break;
            unsigned char *slot = g_arena + (size_t)s * SLOT_BYTES;
            emit_ret_imm(slot, (uint16_t)((s + pass * PASS_STEP) & 0xffff));
            __builtin___clear_cache((char *)slot, (char *)slot + 8); // aarch64: ic ivau; x86: nothing
            uint32_t (*f)(void) = (uint32_t (*)(void))slot;
            sum += f(); // MUST observe this pass's imm, never the previous pass's translation
        }
        pthread_barrier_wait(&g_bar);                                         // all threads finished this pass
        if (id == 0) atomic_store_explicit(&g_next, 0, memory_order_relaxed); // leader resets the bump
        pthread_barrier_wait(&g_bar); // reset published before anyone starts the next pass
    }
    g_sum[id] = sum;
    return NULL;
}

int main(void) {
    g_arena =
        mmap(NULL, (size_t)SLOTS * SLOT_BYTES, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (g_arena == MAP_FAILED) {
        perror("mmap");
        return 1;
    }
    pthread_barrier_init(&g_bar, NULL, THREADS);
    pthread_t t[THREADS];
    for (long i = 0; i < THREADS; i++)
        pthread_create(&t[i], NULL, worker, (void *)i);
    for (int i = 0; i < THREADS; i++)
        pthread_join(t[i], NULL);
    uint64_t total = 0;
    for (int i = 0; i < THREADS; i++)
        total += g_sum[i];
    // Each pass calls every slot exactly once: sum_{s}((s + pass*PASS_STEP) & 0xffff) per pass.
    printf("smc_threads total=%llu\n", (unsigned long long)total);
    return 0;
}
