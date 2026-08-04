#include <stdint.h>
#include <stdio.h>
#include <string.h>

#if defined(__x86_64__)
#include <setjmp.h>
#include <signal.h>
#include <sys/mman.h>
#include <unistd.h>
#include <emmintrin.h>
#endif

enum shift_op { SRL16, SRL32, SRL64, SRA16, SRA32, SLL16, SLL32, SLL64, OP_COUNT };

static uint64_t digest;
static void mix(const void *bytes, size_t length) {
    const uint8_t *p = bytes;
    for (size_t i = 0; i < length; ++i)
        digest = digest * UINT64_C(1000003) + p[i];
}

static unsigned lane_bits(enum shift_op op) {
    static const uint8_t widths[OP_COUNT] = {16, 32, 64, 16, 32, 16, 32, 64};
    return widths[op];
}

static void reference(uint8_t out[16], const uint8_t in[16], enum shift_op op, uint64_t count) {
    unsigned bits = lane_bits(op), bytes = bits / 8;
    for (unsigned offset = 0; offset < 16; offset += bytes) {
        uint64_t value = 0, result = 0;
        memcpy(&value, in + offset, bytes);
        if (op == SRA16 || op == SRA32) {
            if (bits == 16) {
                int16_t signed_value = (int16_t)value;
                int16_t shifted = count >= 16 ? (signed_value < 0 ? -1 : 0) : (int16_t)(signed_value >> count);
                memcpy(out + offset, &shifted, 2);
            } else {
                int32_t signed_value = (int32_t)value;
                int32_t shifted = count >= 32 ? (signed_value < 0 ? -1 : 0) : signed_value >> count;
                memcpy(out + offset, &shifted, 4);
            }
            continue;
        }
        if (count < bits)
            result = op >= SLL16 ? value << count : value >> count;
        memcpy(out + offset, &result, bytes);
    }
}

#if defined(__x86_64__)
#define REG_CASE(name, instruction) case name: __asm__ volatile(instruction " %1,%0" : "+x"(value) : "x"(count)); break
#define MEM_CASE(name, instruction) case name: __asm__ volatile(instruction " %1,%0" : "+x"(value) : "m"(*count)); break

static __m128i shift_register(__m128i value, __m128i count, enum shift_op op) {
    switch (op) {
    REG_CASE(SRL16, "psrlw"); REG_CASE(SRL32, "psrld"); REG_CASE(SRL64, "psrlq");
    REG_CASE(SRA16, "psraw"); REG_CASE(SRA32, "psrad");
    REG_CASE(SLL16, "psllw"); REG_CASE(SLL32, "pslld"); REG_CASE(SLL64, "psllq");
    default: __builtin_unreachable();
    }
    return value;
}

static __m128i shift_memory(__m128i value, const __m128i *count, enum shift_op op) {
    switch (op) {
    MEM_CASE(SRL16, "psrlw"); MEM_CASE(SRL32, "psrld"); MEM_CASE(SRL64, "psrlq");
    MEM_CASE(SRA16, "psraw"); MEM_CASE(SRA32, "psrad");
    MEM_CASE(SLL16, "psllw"); MEM_CASE(SLL32, "pslld"); MEM_CASE(SLL64, "psllq");
    default: __builtin_unreachable();
    }
    return value;
}

static sigjmp_buf fault_return;
static void fault_handler(int signal_number) {
    (void)signal_number;
    siglongjmp(fault_return, 1);
}

static int check_fault_before_commit(const uint8_t input[16]) {
    size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    void *guard = mmap(NULL, page_size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (guard == MAP_FAILED) return 1;
    struct sigaction action = {0}, old_action;
    action.sa_handler = fault_handler;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, &old_action) != 0) return 2;
    volatile __m128i destination = _mm_loadu_si128((const __m128i *)input);
    if (sigsetjmp(fault_return, 1) == 0)
        destination = shift_memory(destination, (const __m128i *)guard, SRL32);
    else {
        uint8_t after[16];
        _mm_storeu_si128((__m128i *)after, destination);
        if (memcmp(after, input, 16) != 0) return 3;
        sigaction(SIGSEGV, &old_action, NULL);
        munmap(guard, page_size);
        return 0;
    }
    return 4;
}
#endif

int main(void) {
    static const uint8_t input[16] = {
        0x81, 0xf0, 0x7e, 0x80, 0x55, 0xaa, 0x00, 0x80,
        0xff, 0x7f, 0x01, 0x80, 0x34, 0x12, 0xef, 0xcd
    };
    static const uint64_t counts[] = {0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, UINT64_MAX};
    for (enum shift_op op = 0; op < OP_COUNT; ++op) {
        for (size_t i = 0; i < sizeof(counts) / sizeof(counts[0]); ++i) {
            uint8_t expected[16], actual[16];
            uint64_t count_words[2] = {counts[i], UINT64_C(0xfedcba9876543210)};
            reference(expected, input, op, counts[i]);
#if defined(__x86_64__)
            __m128i source = _mm_loadu_si128((const __m128i *)input);
            __m128i count = _mm_loadu_si128((const __m128i *)count_words);
            _mm_storeu_si128((__m128i *)actual, shift_register(source, count, op));
            if (memcmp(actual, expected, 16) != 0) return 10 + op;
            _mm_storeu_si128((__m128i *)actual, shift_memory(source, (const __m128i *)count_words, op));
            if (memcmp(actual, expected, 16) != 0) return 30 + op;
#else
            memcpy(actual, expected, 16);
#endif
            mix(expected, sizeof(expected));
            mix(actual, sizeof(actual));
        }
    }
#if defined(__x86_64__)
    int fault_status = check_fault_before_commit(input);
    if (fault_status != 0) return 60 + fault_status;
#endif
    printf("legacy-xmm-scalar-shifts=%016llx\n", (unsigned long long)digest);
    return 0;
}
