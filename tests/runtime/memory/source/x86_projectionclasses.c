#define _GNU_SOURCE
#include <immintrin.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#if defined(__x86_64__)
static unsigned char *logical;
static volatile int altstack_ok;

static void on_signal(int signal) {
    (void)signal;
    unsigned char marker;
    altstack_ok = &marker >= logical + 5 * 4096 && &marker < logical + 6 * 4096;
}

__attribute__((noinline)) static uintptr_t stack_call(unsigned depth) {
    volatile uintptr_t marker = (uintptr_t)&depth;
    return depth == 0 ? marker : stack_call(depth - 1);
}

static void *stack_thread(void *opaque) {
    uintptr_t *result = opaque;
    *result = stack_call(8);
    return NULL;
}

static void *atomic_thread(void *opaque) {
    uint64_t *value = opaque;
    for (unsigned iteration = 0; iteration < 1000; iteration++)
        __atomic_fetch_add(value, 1, __ATOMIC_SEQ_CST);
    return NULL;
}

__attribute__((target("avx2"), noinline)) static int gather_values(const int *values) {
    __m128i indexes = _mm_setr_epi32(0, 2, 4, 6);
    __m128i gathered = _mm_i32gather_epi32(values, indexes, 4);
    int output[4];
    _mm_storeu_si128((__m128i *)output, gathered);
    return output[0] == 10 && output[1] == 30 && output[2] == 50 && output[3] == 70;
}

int main(void) {
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "x86-projection-classes", 0u);
    if (fd < 0 || ftruncate(fd, 12 * (off_t)page) != 0) return 2;
    logical = mmap(NULL, 14 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (logical == MAP_FAILED) return 3;
    for (size_t index = 0; index < 12; index++) {
        if (mmap(logical + index * page, page, PROT_READ | PROT_WRITE,
                 MAP_SHARED | MAP_FIXED, fd, (off_t)(index * page)) != logical + index * page)
            return 4;
    }

    uint64_t scalar_in = UINT64_C(0x8877665544332211), scalar_out = 0;
    memcpy(logical + page - 4, &scalar_in, 8);
    __asm__ volatile("movq (%1), %0" : "=r"(scalar_out) : "r"(logical + page - 4) : "memory");
    int scalar = scalar_out == scalar_in;

    for (unsigned i = 0; i < 32; i++) logical[2 * page + i] = (unsigned char)(i + 1);
    __m128i sse16 = _mm_loadu_si128((const __m128i *)(logical + 2 * page));
    unsigned char wide[32];
    _mm_storeu_si128((__m128i *)wide, sse16);
    _mm_storeu_si128((__m128i *)(wide + 16), _mm_loadu_si128((const __m128i *)(logical + 2 * page + 16)));
    int vector = (unsigned)_mm_cvtsi128_si32(sse16) == UINT32_C(0x04030201) && wide[31] == 32;

    uint64_t locked = 7, add = 5;
    memcpy(logical + 3 * page, &locked, 8);
    __asm__ volatile("lock xaddq %0, (%1)" : "+r"(add) : "r"(logical + 3 * page) : "memory", "cc");
    unsigned long long expected_lo = 12, expected_hi = 9;
    unsigned char swapped;
    uint64_t *pair = (uint64_t *)(logical + 3 * page + 16);
    pair[0] = 12;
    pair[1] = 9;
    uint64_t desired_lo = 21, desired_hi = 34;
    __asm__ volatile("lock cmpxchg16b (%5); sete %0"
                     : "=q"(swapped), "+a"(expected_lo), "+d"(expected_hi)
                     : "b"(desired_lo), "c"(desired_hi), "r"(pair)
                     : "memory", "cc");
    int atomic = add == 7 && *(uint64_t *)(logical + 3 * page) == 12 && swapped && pair[0] == 21 && pair[1] == 34;

    uint8_t cmp1 = 1, expected1 = 1;
    uint16_t cmp2 = 2, expected2 = 2;
    uint32_t cmp4 = 4, expected4 = 4;
    uint64_t cmp8 = 8, expected8 = 8;
    int compare_widths = __atomic_compare_exchange_n(&cmp1, &expected1, 11, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST) &&
                         __atomic_compare_exchange_n(&cmp2, &expected2, 22, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST) &&
                         __atomic_compare_exchange_n(&cmp4, &expected4, 44, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST) &&
                         __atomic_compare_exchange_n(&cmp8, &expected8, 88, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);

    uint64_t rotated = UINT64_C(0x8000000000000001);
    memcpy(logical + 3 * page + 48, &rotated, 8);
    __asm__ volatile("rolq $1, (%0)" : : "r"(logical + 3 * page + 48) : "memory", "cc");
    int rotate = *(uint64_t *)(logical + 3 * page + 48) == 3;
    uint8_t rotate1 = 0x81;
    uint16_t rotate2 = 0x8001;
    uint32_t rotate4 = UINT32_C(0x80000001);
    __asm__ volatile("rolb $1, %0; rolw $1, %1; roll $1, %2" : "+m"(rotate1), "+m"(rotate2), "+m"(rotate4) : : "cc");
    int rotate_widths = rotate1 == 3 && rotate2 == 3 && rotate4 == 3;

    double fp = 3.5, fp_out = 0;
    memcpy(logical + 4 * page, &fp, sizeof fp);
    __asm__ volatile("fldl (%1); fstpl (%0)" : : "r"(logical + 4 * page + 8), "r"(logical + 4 * page) : "memory");
    memcpy(&fp_out, logical + 4 * page + 8, sizeof fp_out);
    __asm__ volatile("fxsave64 (%0)" : : "r"(logical + 4 * page + 512) : "memory");
    int x87 = fp_out == fp && logical[4 * page + 512] == 0x7f;

    stack_t stack = {.ss_sp = logical + 5 * page, .ss_size = page, .ss_flags = 0};
    struct sigaction action = {.sa_handler = on_signal, .sa_flags = SA_ONSTACK};
    sigemptyset(&action.sa_mask);
    if (sigaltstack(&stack, NULL) != 0 || sigaction(SIGUSR1, &action, NULL) != 0 || raise(SIGUSR1) != 0) return 5;

    pthread_attr_t attributes;
    pthread_t thread;
    uintptr_t stack_result = 0;
    pthread_attr_init(&attributes);
    pthread_attr_setstack(&attributes, logical + 6 * page, 4 * page);
    int thread_ok = pthread_create(&thread, &attributes, stack_thread, &stack_result) == 0 &&
                    pthread_join(thread, NULL) == 0 && stack_result >= (uintptr_t)(logical + 6 * page) &&
                    stack_result < (uintptr_t)(logical + 10 * page);
    pthread_attr_destroy(&attributes);

    uint64_t *alias_a = (uint64_t *)(logical + 3 * page + 128);
    unsigned char *alias_page = logical + 11 * page;
    int alias = mmap(alias_page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, (off_t)(3 * page)) ==
                alias_page;
    uint64_t *alias_b = (uint64_t *)(alias_page + 128);
    *alias_a = 0;
    pthread_t first_thread, second_thread;
    alias = alias && pthread_create(&first_thread, NULL, atomic_thread, alias_a) == 0 &&
            pthread_create(&second_thread, NULL, atomic_thread, alias_b) == 0 && pthread_join(first_thread, NULL) == 0 &&
            pthread_join(second_thread, NULL) == 0 && *alias_a == 2000 && *alias_b == 2000;

    int pipefd[2];
    int legacy = pipe(pipefd) == 0;
    memcpy(logical + 10 * page, "legacy", 6);
    legacy = legacy && syscall(SYS_write, pipefd[1], logical + 10 * page, 6) == 6;
    memset(logical + 10 * page + 32, 0, 6);
    legacy = legacy && syscall(SYS_read, pipefd[0], logical + 10 * page + 32, 6) == 6 &&
             memcmp(logical + 10 * page + 32, "legacy", 6) == 0;

    unsigned char *code = logical + 10 * page + 128;
    unsigned char first[] = {0xb8, 11, 0, 0, 0, 0xc3};
    memcpy(code, first, sizeof first);
    if (mprotect(logical + 10 * page, page, PROT_READ | PROT_WRITE | PROT_EXEC) != 0) return 6;
    int (*generated)(void) = (int (*)(void))code;
    int before = generated();
    code[1] = 29;
    __builtin___clear_cache((char *)code, (char *)code + sizeof first);
    int smc = before == 11 && generated() == 29;

    printf("x86-projection-core scalar=%d vector=%d atomic=%d cmp-widths=%d rotate=%d rotate-widths=%d x87=%d altstack=%d stack=%d alias=%d legacy=%d smc=%d\n",
           scalar, vector, atomic, compare_widths, rotate, rotate_widths, x87, altstack_ok, thread_ok, alias, legacy, smc);
    fflush(stdout);
    int values[8] = {10, 20, 30, 40, 50, 60, 70, 80};
    memcpy(logical + 4 * page + 2048, values, sizeof values);
    int gather = gather_values((const int *)(logical + 4 * page + 2048));
    printf("x86-projection-avx gather=%d\n", gather);
    return !(scalar && vector && atomic && compare_widths && rotate && rotate_widths && x87 && gather && altstack_ok &&
             thread_ok && alias && legacy && smc);
}
#else
int main(void) { return 0; }
#endif
