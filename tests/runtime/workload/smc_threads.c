// #267 (Erlang/OTP BeamAsm SIGSEGV) regression: MULTITHREADED self-modifying code sharing code pages.
// BeamAsm is a code-GENERATING guest -- it JITs Erlang to arm64 at load time, from N scheduler + dirty +
// async threads, packing many small functions per 4KB page. hl translates that guest-generated code; when a
// thread writes a NEW function onto a page another thread has ALREADY executed (so hl has a translation for
// it) and issues the icache-flush (`ic ivau`), hl's SMC hook fires. The original bug: that hook dropped the
// whole translation map + IBTC UNLOCKED, racing every peer thread, and it fired on ANY same-page write
// (page-granular gate) even though no translated byte changed -> non-deterministic SIGSEGV/SIGBUS in the
// code cache under heavy threading. The fix: gate at cache-line (64B) granularity so a same-page APPEND is a
// no-op, and never drop the shared tables while a peer thread is live.
//
// This reproduces the pattern without the 4.7GB OTP image: T threads share one RWX arena of `movz w0,#imm;
// ret` slots. Every thread grabs slots off a shared bump pointer, writes its slot, flushes the icache, and
// calls it -- so slots from different threads interleave onto the same pages (the append-onto-a-live-page
// case) and, on later passes, threads REWRITE already-translated slots in place (the genuine line-hit case).
// Determinism (required for the oracle diff): a slot's immediate is ALWAYS slot_index & 0xffff, so a rewrite
// stores identical bytes -> the result is the same whether hl re-translates or keeps the prior translation.
// aarch64 machine code -> Linux/aarch64 only; diffed vs a native run.
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

#define THREADS 8
#define SLOTS 2048        // 2048 * 16B = 32KB = 8 pages -> heavy cross-thread page sharing
#define SLOT_WORDS 4      // 16B per slot: movz; ret; nop; nop
#define PASSES 24

static uint32_t *g_arena;
static atomic_uint g_next;         // shared bump pointer over slots (the cross-thread page-sharing driver)
static pthread_barrier_t g_bar;
static uint64_t g_sum[THREADS];

static void *worker(void *arg) {
    long id = (long)arg;
    uint64_t sum = 0;
    for (int pass = 0; pass < PASSES; pass++) {
        for (;;) {
            unsigned s = atomic_fetch_add_explicit(&g_next, 1, memory_order_relaxed);
            if (s >= SLOTS) break;
            uint32_t *slot = g_arena + (size_t)s * SLOT_WORDS;
            uint16_t imm = (uint16_t)(s & 0xffff);
            slot[0] = 0x52800000u | ((uint32_t)imm << 5); // movz w0, #imm
            slot[1] = 0xd65f03c0u;                        // ret
            __builtin___clear_cache((char *)slot, (char *)(slot + 2)); // guest icache-flush -> hl SMC hook
            uint32_t (*f)(void) = (uint32_t (*)(void))slot;
            sum += f(); // MUST observe imm (never a stale/garbage translation, never a race fault)
        }
        pthread_barrier_wait(&g_bar);   // all threads finished this pass
        if (id == 0) atomic_store_explicit(&g_next, 0, memory_order_relaxed); // leader resets the bump
        pthread_barrier_wait(&g_bar);   // reset published before anyone starts the next pass
    }
    g_sum[id] = sum;
    return NULL;
}

int main(void) {
    g_arena = mmap(NULL, SLOTS * SLOT_WORDS * sizeof(uint32_t), PROT_READ | PROT_WRITE | PROT_EXEC,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (g_arena == MAP_FAILED) { perror("mmap"); return 1; }
    pthread_barrier_init(&g_bar, NULL, THREADS);
    pthread_t t[THREADS];
    for (long i = 0; i < THREADS; i++) pthread_create(&t[i], NULL, worker, (void *)i);
    for (int i = 0; i < THREADS; i++) pthread_join(t[i], NULL);
    uint64_t total = 0;
    for (int i = 0; i < THREADS; i++) total += g_sum[i];
    // Each of PASSES passes calls every slot exactly once: sum_{s=0}^{SLOTS-1}(s & 0xffff) per pass.
    printf("smc_threads total=%llu\n", (unsigned long long)total);
    return 0;
}
