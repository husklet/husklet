// One thread patches a shared code region while others run it, with proper barrier synchronization:
// each round, a single writer rewrites a shared RWX leaf to a new constant and flushes the icache,
// all threads rendezvous at a barrier, then every reader thread calls the leaf and must observe the
// NEW value. This is the safe cross-thread SMC pattern (JIT patching under a stop-the-world barrier);
// the engine must make one thread's re-translation visible to the others. Deterministic checksum.
#include "dbt.h"
#include <pthread.h>

#define NT 6
#define ROUNDS 4000

static unsigned char *code;
static size_t code_sz = 4096;
static pthread_barrier_t before, after;
static volatile int expected;
static uint64_t sums[NT];
static int mismatch = 0;

static void *reader(void *arg) {
    long id = (long)arg;
    uint64_t s = 0;
    for (int r = 0; r < ROUNDS; r++) {
        pthread_barrier_wait(&before); // writer has patched + flushed
        int (*f)(void) = (int (*)(void))code;
        int got = f();
        if (got != expected) __atomic_store_n(&mismatch, 1, __ATOMIC_RELAXED);
        s = s * 31 + (uint64_t)got;
        pthread_barrier_wait(&after); // release writer to patch next round
    }
    sums[id] = s;
    return NULL;
}

int main(void) {
    code = dbt_alloc(code_sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    pthread_barrier_init(&before, NULL, NT + 1);
    pthread_barrier_init(&after, NULL, NT + 1);
    dbt_emit_ret_imm(code, 0);
    dbt_flush(code, code_sz);

    pthread_t th[NT];
    for (long i = 0; i < NT; i++) pthread_create(&th[i], NULL, reader, (void *)i);

    for (int r = 0; r < ROUNDS; r++) {
        expected = (r * 13 + 1) & 0x7fff;
        dbt_emit_ret_imm(code, expected); // writer patches BEFORE the barrier release
        dbt_flush(code, code_sz);
        pthread_barrier_wait(&before);    // readers now run the new code
        pthread_barrier_wait(&after);     // wait until all readers done this round
    }
    for (int i = 0; i < NT; i++) pthread_join(th[i], NULL);

    uint64_t acc = 0;
    for (int i = 0; i < NT; i++) acc = acc * 1000003ULL + sums[i];
    printf("conc-patch acc=%llu mismatch=%d\n", (unsigned long long)acc, mismatch);
    return 0;
}
