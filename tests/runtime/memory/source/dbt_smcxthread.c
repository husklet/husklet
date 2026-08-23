// Cross-thread SMC: a second thread patches a leaf while the first is still spinning on it, so a
// translation built before the patch is genuinely reused after it and the executable token is the
// only thing that can retire it. The single-threaded sibling (dbt-smc-hotpatch) cannot test this --
// there the patching store is itself a guest fallback, so native always exits at the patch.
// The spinner never sleeps: the writer waits for a published spin count before patching, and the
// spinner only judges a call whose publication state is unambiguous on both sides of it.
#include "dbt.h"
#include <pthread.h>

#define ROUNDS 64
#define WARM 8000
#define LIMIT 3000000

static unsigned char *code;
static size_t code_sz = 4096;

// A bare `movz w0,#imm; ret` leaf is served as a relocation target resolved from freshly read guest
// bytes, so a stale translation of it never survives to be observed. Prefixing real work makes the
// body a translation unit the scheduler re-enters in its own right, with the patched word inside the
// entry's forward source window. Emits `int f(void){ ...FILL adds...; return imm; }`.
#define FILL 48

static void emit_body(unsigned char *buf, int imm) {
#if defined(__aarch64__)
    uint32_t *w = (uint32_t *)buf;
    for (int i = 0; i < FILL; i++)
        w[i] = 0x91000421u;                                  // add x1, x1, #1
    w[FILL] = 0x52800000u | ((uint32_t)(imm & 0xffff) << 5); // movz w0, #imm16
    w[FILL + 1] = 0xD65F03C0u;                               // ret
#elif defined(__x86_64__)
    unsigned char *p = buf;
    for (int i = 0; i < FILL; i++) {
        *p++ = 0x48;
        *p++ = 0x83;
        *p++ = 0xC6;
        *p++ = 0x01; // add rsi, 1
    }
    *p++ = 0xB8; // mov eax, imm32
    *p++ = (unsigned char)(imm & 0xff);
    *p++ = (unsigned char)((imm >> 8) & 0xff);
    *p++ = (unsigned char)((imm >> 16) & 0xff);
    *p++ = (unsigned char)((imm >> 24) & 0xff);
    *p++ = 0xC3; // ret
#else
    (void)buf;
    (void)imm;
#endif
}

static int nextval;  // value the writer is installing this round; read only once gen is published
static int arming;   // round number+1 before the writer touches the code, so a call that observes
                     // it unset afterwards is known to have run entirely on the old body
static int gen;      // round number+1 once the patch for that round is flushed and visible
static int ready;    // round number+1 once the spinner is spinning on the previous value
static long spins;   // spinner's iteration count within the current round
static int finished; // spinner has left the loop; releases a waiting writer

static uint64_t acc;
static int stale_round = -1; // saw the OLD constant on a call that began after the patch published
static int wild_round = -1;  // saw neither constant with no patch in flight
static int lost_round = -1;  // exhausted LIMIT without the writer ever publishing

static void *spinner(void *unused) {
    (void)unused;
    int (*f)(void) = (int (*)(void))code;
    int prev = 0;
    for (int r = 0; r < ROUNDS; r++) {
        __atomic_store_n(&spins, 0, __ATOMIC_RELAXED);
        __atomic_store_n(&ready, r + 1, __ATOMIC_RELEASE);
        long i = 0;
        int settled = 0;
        for (; i < LIMIT; i++) {
            __atomic_store_n(&spins, i, __ATOMIC_RELAXED);
            int before = __atomic_load_n(&gen, __ATOMIC_ACQUIRE);
            int got = f();
            int armed = __atomic_load_n(&arming, __ATOMIC_ACQUIRE);
            if (before == r + 1) {
                // The patch and its icache maintenance completed before this call began.
                if (got != nextval) {
                    stale_round = r;
                    goto out;
                }
                settled = 1;
                break;
            }
            if (armed == r + 1) continue; // patch was in flight across the call; it proves nothing
            if (got != prev) {
                wild_round = r;
                goto out;
            }
        }
        if (!settled) {
            lost_round = r;
            goto out;
        }
        prev = nextval;
        acc = acc * 1000003ULL + (uint64_t)prev;
    }
out:
    __atomic_store_n(&finished, 1, __ATOMIC_RELEASE);
    return NULL;
}

int main(void) {
    code = dbt_alloc(code_sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    emit_body(code, 0);
    dbt_flush(code, code_sz);

    pthread_t th;
    if (pthread_create(&th, NULL, spinner, NULL) != 0) {
        perror("pthread_create");
        return 1;
    }

    int rounds = 0;
    for (int r = 0; r < ROUNDS; r++) {
        while (__atomic_load_n(&ready, __ATOMIC_ACQUIRE) != r + 1) {
            if (__atomic_load_n(&finished, __ATOMIC_ACQUIRE)) goto out;
        }
        // Synchronise the patch against a known execution point rather than a sleep: the spinner has
        // called the leaf WARM times this round, so it is warm enough to hold a native translation.
        while (__atomic_load_n(&spins, __ATOMIC_RELAXED) < WARM) {
            if (__atomic_load_n(&finished, __ATOMIC_ACQUIRE)) goto out;
        }
        nextval = 1 + ((r * 7919 + 13) % 30000);
        __atomic_store_n(&arming, r + 1, __ATOMIC_RELEASE);
        emit_body(code, nextval);
        dbt_flush(code, code_sz);
        __atomic_store_n(&gen, r + 1, __ATOMIC_RELEASE);
        rounds = r + 1;
    }
out:
    pthread_join(th, NULL);
    printf("smc-xthread rounds=%d acc=%llu stale=%d wild=%d lost=%d\n", rounds, (unsigned long long)acc, stale_round,
           wild_round, lost_round);
    // Exit 3, not 1, so a stale translation is distinguishable from dbt_alloc failing the mmap.
    if (stale_round >= 0 || wild_round >= 0 || lost_round >= 0 || rounds != ROUNDS) return 3;
    return 0;
}
