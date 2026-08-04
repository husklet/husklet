#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <emmintrin.h>

enum { ROUNDS = 100000 };
struct shared {
    _Atomic uint32_t seq;
    _Atomic uint32_t ack;
    unsigned char bytes[40];
};
static struct shared s;
static int bad;

static void *consumer(void *unused) {
    (void)unused;
    for (uint32_t n = 1; n <= ROUNDS; n++) {
        while (atomic_load_explicit(&s.seq, memory_order_acquire) != n) {}
        uint64_t scalar;
        __m128i vector = _mm_loadu_si128((const __m128i *)(s.bytes + 9));
        memcpy(&scalar, s.bytes + 1, sizeof scalar);
        uint64_t lanes[2];
        _mm_storeu_si128((__m128i *)lanes, vector);
        if (scalar != (0x9e3779b97f4a7c15ULL ^ n) || lanes[0] != n || lanes[1] != ~((uint64_t)n)) bad = 1;
        atomic_store_explicit(&s.ack, n, memory_order_release);
    }
    return NULL;
}

int main(void) {
    pthread_t th;
    if (pthread_create(&th, NULL, consumer, NULL)) return 2;
    for (uint32_t n = 1; n <= ROUNDS; n++) {
        while (atomic_load_explicit(&s.ack, memory_order_acquire) != n - 1) {}
        uint64_t scalar = 0x9e3779b97f4a7c15ULL ^ n;
        uint64_t lanes[2] = {n, ~((uint64_t)n)};
        memcpy(s.bytes + 1, &scalar, sizeof scalar);
        _mm_storeu_si128((__m128i *)(s.bytes + 9), _mm_loadu_si128((const __m128i *)lanes));
        atomic_store_explicit(&s.seq, n, memory_order_release);
    }
    pthread_join(th, NULL);
    printf("tso_unaligned rounds=%u bad=%d\n", ROUNDS, bad);
    return bad;
}
