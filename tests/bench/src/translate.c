#include <stdint.h>

__attribute__((noinline)) static uint64_t translate_block(unsigned block, uint64_t value) {
    switch (block) {
#define TRANSLATE_CASE(n) case n: return (value ^ (UINT64_C(0x9e3779b97f4a7c15) * ((n) + 1u))) + (n);
#define TRANSLATE_8(n)                                                                                               \
    TRANSLATE_CASE((n) + 0)                                                                                          \
    TRANSLATE_CASE((n) + 1)                                                                                          \
    TRANSLATE_CASE((n) + 2)                                                                                          \
    TRANSLATE_CASE((n) + 3)                                                                                          \
    TRANSLATE_CASE((n) + 4)                                                                                          \
    TRANSLATE_CASE((n) + 5)                                                                                          \
    TRANSLATE_CASE((n) + 6)                                                                                          \
    TRANSLATE_CASE((n) + 7)
#define TRANSLATE_64(n)                                                                                              \
    TRANSLATE_8((n) + 0)                                                                                             \
    TRANSLATE_8((n) + 8)                                                                                             \
    TRANSLATE_8((n) + 16)                                                                                            \
    TRANSLATE_8((n) + 24)                                                                                            \
    TRANSLATE_8((n) + 32)                                                                                            \
    TRANSLATE_8((n) + 40)                                                                                            \
    TRANSLATE_8((n) + 48)                                                                                            \
    TRANSLATE_8((n) + 56)
        TRANSLATE_64(0)
        TRANSLATE_64(64)
        TRANSLATE_64(128)
        TRANSLATE_64(192)
        TRANSLATE_64(256)
        TRANSLATE_64(320)
        TRANSLATE_64(384)
        TRANSLATE_64(448)
        TRANSLATE_64(512)
        TRANSLATE_64(576)
        TRANSLATE_64(640)
        TRANSLATE_64(704)
        TRANSLATE_64(768)
        TRANSLATE_64(832)
        TRANSLATE_64(896)
        TRANSLATE_64(960)
    }
    return 0;
}

int main(void) {
    volatile uint64_t value = UINT64_C(0x123456789abcdef0);
    for (unsigned block = 0; block < 1024; ++block) value = translate_block(block, value);
    return value == 0 ? 1 : 0;
}
