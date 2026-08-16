#define _GNU_SOURCE
#include <immintrin.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#if defined(__x86_64__)
static unsigned char area[16384] __attribute__((aligned(4096)));
static sigjmp_buf escape;
static volatile uintptr_t fault_address;

static void fault(int signal, siginfo_t *info, void *context) {
    (void)signal;
    (void)context;
    fault_address = (uintptr_t)info->si_addr;
    siglongjmp(escape, 1);
}

static int clean(const unsigned char *bytes, size_t length) {
    for (size_t index = 0; index < length; ++index)
        if (bytes[index] != 0xa5) return 0;
    return 1;
}

static int scalar_probe(unsigned char *boundary) {
    unsigned char *target = boundary - 4;
    uint64_t value = UINT64_C(0x1122334455667788);
    fault_address = 0;
    memset(target, 0xa5, 4);
    if (sigsetjmp(escape, 1) == 0) {
        __asm__ volatile("movq %1, (%0)" : : "r"(target), "r"(value) : "memory");
        return 0;
    }
    return fault_address == (uintptr_t)boundary && clean(target, 4);
}

static int vector_probe(unsigned char *boundary) {
    unsigned char *target = boundary - 8;
    __m128i value = _mm_set1_epi8(0x37);
    fault_address = 0;
    memset(target, 0xa5, 8);
    if (sigsetjmp(escape, 1) == 0) {
        _mm_storeu_si128((__m128i *)target, value);
        return 0;
    }
    return fault_address == (uintptr_t)boundary && clean(target, 8);
}

static int writable_scalar_probe(unsigned char *boundary) {
    unsigned char *target = boundary - 4;
    uint64_t value = UINT64_C(0x1122334455667788);
    uint64_t observed = 0;
    memset(target, 0xa5, sizeof value);
    __asm__ volatile("movq %1, (%0)" : : "r"(target), "r"(value) : "memory");
    memcpy(&observed, target, sizeof observed);
    return observed == value;
}

static int writable_vector_probe(unsigned char *boundary) {
    unsigned char *target = boundary - 8;
    __m128i value = _mm_set1_epi8(0x37);
    memset(target, 0xa5, sizeof value);
    _mm_storeu_si128((__m128i *)target, value);
    for (size_t index = 0; index < sizeof value; ++index)
        if (target[index] != 0x37) return 0;
    return 1;
}

static int writable_stack_probe(void) {
    volatile unsigned char storage[3 * 4096];
    uintptr_t boundary = ((uintptr_t)&storage[4095]) & ~(uintptr_t)4095;
    unsigned char *target = (unsigned char *)boundary - 4;
    uint64_t value = UINT64_C(0x8877665544332211);
    uint64_t observed = 0;
    memset(target, 0xa5, sizeof value);
    __asm__ volatile("movq %1, (%0)" : : "r"(target), "r"(value) : "memory");
    memcpy(&observed, target, sizeof observed);
    return observed == value;
}

int main(void) {
    setbuf(stdout, NULL);
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0 || sigaction(SIGBUS, &action, NULL) != 0) return 2;
    const size_t page = 4096;
    memset(area, 0xa5, sizeof area);

    int writable_stack = writable_stack_probe();
    int writable_scalar = writable_scalar_probe(area + page);
    int writable_vector = writable_vector_probe(area + page);

    if (mprotect(area + page, page, PROT_NONE) != 0) return 3;
    int protected_scalar = scalar_probe(area + page);
    int protected_vector = vector_probe(area + page);
    if (mprotect(area + page, page, PROT_READ | PROT_WRITE) != 0) return 4;

    if (mprotect(area + 2 * page, page, PROT_READ) != 0) return 5;
    int readonly_scalar = scalar_probe(area + 2 * page);
    int readonly_vector = vector_probe(area + 2 * page);
    if (mprotect(area + 2 * page, page, PROT_READ | PROT_WRITE) != 0) return 6;

    if (munmap(area + 3 * page, page) != 0) return 7;
    int absent_scalar = scalar_probe(area + 3 * page);
    int absent_vector = vector_probe(area + 3 * page);

    printf("direct-store writable=%d/%d/%d protected=%d/%d readonly=%d/%d absent=%d/%d\n", writable_stack,
           writable_scalar, writable_vector, protected_scalar, protected_vector, readonly_scalar, readonly_vector,
           absent_scalar, absent_vector);
    return !(writable_stack && writable_scalar && writable_vector && protected_scalar && protected_vector &&
             readonly_scalar && readonly_vector && absent_scalar && absent_vector);
}
#else
int main(void) {
    return 0;
}
#endif
