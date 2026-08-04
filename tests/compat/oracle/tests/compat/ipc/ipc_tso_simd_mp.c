/*
 * Message-passing (MP) litmus for x86-TSO StoreStore / LoadLoad through PACKED-SSE
 * payloads. A producer publishes a 16-byte SIMD payload and THEN raises a flag; a
 * consumer that observes the flag must observe the whole payload (x86-TSO StoreStore),
 * and its payload read must not be hoisted past the flag load (LoadLoad). The flag uses
 * C11 acquire/release ONLY as the compiler barrier -- on x86-64 a release store and an
 * acquire load of an aligned word both compile to a plain MOV (no fence instruction),
 * so the guest instruction stream is ordinary loads/stores and the actual run-time
 * ordering rests on the translator's emitted per-access memory-barrier edges. This is
 * the direct guard for the guest store-barrier encoding: if the store barrier failed to
 * keep the payload stores ahead of the flag store, the consumer would read a stale/torn
 * payload -> bad != 0. Ping-pong both directions so LoadLoad is stressed symmetrically.
 * Bit-exact vs the qemu-x86_64 oracle and native x86 (all print bad=0).
 */
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <emmintrin.h>

enum { ROUNDS = 400000 };

struct cell {
    _Atomic uint32_t flag; /* publication flag; on x86 release/acquire == plain MOV */
    unsigned char pad[12];
    unsigned char payload[16]; /* 16-byte SIMD payload, published before flag */
};
static struct cell fwd; /* producer -> consumer */
static struct cell bwd; /* consumer -> producer (ack) */
static int bad;

static __m128i mk(uint32_t n) {
    return _mm_set_epi32((int)~n, (int)(n * 2654435761u), (int)(n ^ 0x5a5a5a5au), (int)n);
}

static void *consumer(void *unused) {
    (void)unused;
    for (uint32_t n = 1; n <= ROUNDS; n++) {
        while (atomic_load_explicit(&fwd.flag, memory_order_acquire) != n) {
        }
        __m128i got = _mm_loadu_si128((const __m128i *)fwd.payload);
        if (_mm_movemask_epi8(_mm_cmpeq_epi8(got, mk(n))) != 0xFFFF) bad = 1;
        _mm_storeu_si128((__m128i *)bwd.payload, mk(~n)); /* ack payload first */
        atomic_store_explicit(&bwd.flag, n, memory_order_release);
    }
    return NULL;
}

int main(void) {
    pthread_t th;
    if (pthread_create(&th, NULL, consumer, NULL)) return 2;
    for (uint32_t n = 1; n <= ROUNDS; n++) {
        _mm_storeu_si128((__m128i *)fwd.payload, mk(n));           /* payload first */
        atomic_store_explicit(&fwd.flag, n, memory_order_release); /* ...then flag */
        while (atomic_load_explicit(&bwd.flag, memory_order_acquire) != n) {
        }
        __m128i got = _mm_loadu_si128((const __m128i *)bwd.payload);
        if (_mm_movemask_epi8(_mm_cmpeq_epi8(got, mk(~n))) != 0xFFFF) bad = 1;
    }
    pthread_join(th, NULL);
    printf("simd_mp rounds=%u bad=%d\n", ROUNDS, bad);
    return bad;
}
