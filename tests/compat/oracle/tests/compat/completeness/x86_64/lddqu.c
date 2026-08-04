#include <stdint.h>
#include <stdio.h>
#include <tmmintrin.h>

__attribute__((target("sse3"), noinline)) static __m128i load(const void *address) {
    return _mm_lddqu_si128((const __m128i *)address);
}

int main(void) {
    uint8_t bytes[32];
    uint8_t result[16];

    for (unsigned i = 0; i < sizeof bytes; ++i)
        bytes[i] = (uint8_t)(i * 7u + 3u);

    _mm_storeu_si128((__m128i *)result, load(bytes + 1));
    for (unsigned i = 0; i < sizeof result; ++i)
        printf("%02x", result[i]);
    putchar('\n');
    return 0;
}
