#define _GNU_SOURCE
#include <immintrin.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <ucontext.h>

#if defined(__x86_64__)
static sigjmp_buf jump;
static volatile uintptr_t fault_address;

static void fault(int signal, siginfo_t *info, void *opaque) {
    (void)signal;
    (void)opaque;
    fault_address = (uintptr_t)info->si_addr;
    siglongjmp(jump, 1);
}

static int unchanged(const unsigned char *bytes, size_t length, unsigned char value) {
    for (size_t index = 0; index < length; index++)
        if (bytes[index] != value) return 0;
    return 1;
}

#define EXPECT_FAULT(statement)                                                                                        \
    __extension__({                                                                                                    \
        int caught;                                                                                                    \
        if (sigsetjmp(jump, 1) == 0) {                                                                                 \
            statement;                                                                                                 \
            caught = 0;                                                                                                \
        } else {                                                                                                       \
            caught = 1;                                                                                                \
        }                                                                                                              \
        caught;                                                                                                        \
    })

__attribute__((target("avx"), noinline)) static int avx_fault(unsigned char *pointer) {
    __m256i value = _mm256_set1_epi8(0x37);
    return EXPECT_FAULT(_mm256_storeu_si256((__m256i *)pointer, value));
}

int main(void) {
    setbuf(stdout, NULL);
    const size_t page = 4096;
    unsigned char *mapping = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED || mprotect(mapping + page, page, PROT_NONE) != 0) return 2;
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0 || sigaction(SIGBUS, &action, NULL) != 0) return 3;

    memset(mapping, 0xa5, page);
    uint64_t scalar = UINT64_C(0x1122334455667788);
    unsigned char *cross = mapping + page - 4;
    int scalar_fault = EXPECT_FAULT(__asm__ volatile("movq %1, (%0)" : : "r"(cross), "r"(scalar) : "memory"));
    int scalar_clean = unchanged(cross, 4, 0xa5) && fault_address >= (uintptr_t)(mapping + page);
    printf("scalar=%d/%d\n", scalar_fault, scalar_clean);

    memset(mapping + page - 32, 0xa5, 32);
    __m128i xmm = _mm_set1_epi8(0x26);
    int sse_fault = EXPECT_FAULT(_mm_storeu_si128((__m128i *)(mapping + page - 8), xmm));
    int sse_clean = unchanged(mapping + page - 8, 8, 0xa5);
    int avx = avx_fault(mapping + page - 16);
    int avx_clean = unchanged(mapping + page - 16, 16, 0xa5);
    printf("vector=%d/%d avx=%d/%d\n", sse_fault, sse_clean, avx, avx_clean);

    long double extended = 1.25L;
    memset(mapping + page - 64, 0xa5, 64);
    int m80 =
        EXPECT_FAULT(__asm__ volatile("fldt %1; fstpt (%0)" : : "r"(mapping + page - 5), "m"(extended) : "memory"));
    int m80_clean = unchanged(mapping + page - 5, 5, 0xa5);
    __asm__ volatile("fninit");
    int env28 = EXPECT_FAULT(__asm__ volatile("fnstenv (%0)" : : "r"(mapping + page - 14) : "memory"));
    int env28_clean = unchanged(mapping + page - 14, 14, 0xa5);
    __asm__ volatile("fninit");
    int save108 = EXPECT_FAULT(__asm__ volatile("fnsave (%0)" : : "r"(mapping + page - 54) : "memory"));
    int save108_clean = unchanged(mapping + page - 54, 54, 0xa5);
    __asm__ volatile("fninit");
    int save512 = EXPECT_FAULT(__asm__ volatile("fxsave64 (%0)" : : "r"(mapping + page - 256) : "memory"));
    int save512_clean = unchanged(mapping + page - 256, 256, 0xa5);
    printf("xstate m80=%d/%d env28=%d/%d save108=%d/%d save512=%d/%d\n", m80, m80_clean, env28, env28_clean, save108,
           save108_clean, save512, save512_clean);

    uint16_t expected2 = UINT16_C(0xa5a5);
    uint32_t expected4 = UINT32_C(0xa5a5a5a5);
    uint64_t expected8 = UINT64_C(0xa5a5a5a5a5a5a5a5);
    int cmp2 = EXPECT_FAULT(__asm__ volatile("lock cmpxchgw %2, (%1)" : "+a"(expected2) : "r"(mapping + page - 1),
                                             "r"((uint16_t)7) : "memory", "cc"));
    int cmp4 = EXPECT_FAULT(__asm__ volatile("lock cmpxchgl %2, (%1)" : "+a"(expected4) : "r"(mapping + page - 2),
                                             "r"((uint32_t)7) : "memory", "cc"));
    int cmp8 = EXPECT_FAULT(__asm__ volatile("lock cmpxchgq %2, (%1)" : "+a"(expected8) : "r"(mapping + page - 4),
                                             "r"((uint64_t)7) : "memory", "cc"));
    unsigned long long low = UINT64_C(0xa5a5a5a5a5a5a5a5), high = low;
    int cmp16 = EXPECT_FAULT(__asm__ volatile("lock cmpxchg16b (%4)" : "+a"(low), "+d"(high) : "b"((uint64_t)7),
                                              "c"((uint64_t)9), "r"(mapping + page - 8) : "memory", "cc"));
    int compare_clean = unchanged(mapping + page - 8, 8, 0xa5);
    printf("compare=%d%d%d%d/%d\n", cmp2, cmp4, cmp8, cmp16, compare_clean);

    int rotate2 = EXPECT_FAULT(__asm__ volatile("rolw $1, (%0)" : : "r"(mapping + page - 1) : "memory", "cc"));
    int rotate4 = EXPECT_FAULT(__asm__ volatile("roll $1, (%0)" : : "r"(mapping + page - 2) : "memory", "cc"));
    int rotate8 = EXPECT_FAULT(__asm__ volatile("rolq $1, (%0)" : : "r"(mapping + page - 4) : "memory", "cc"));
    int rotate_clean = unchanged(mapping + page - 8, 8, 0xa5);
    printf("rotate=%d%d%d/%d\n", rotate2, rotate4, rotate8, rotate_clean);

    printf("x86-projection-fault complete=1\n");
    return !(scalar_fault && scalar_clean && sse_fault && sse_clean && avx && avx_clean && m80 && m80_clean && env28 &&
             env28_clean && save108 && save108_clean && save512 && save512_clean && cmp2 && cmp4 && cmp8 && cmp16 &&
             compare_clean && rotate2 && rotate4 && rotate8 && rotate_clean);
}
#else
int main(void) {
    return 0;
}
#endif
