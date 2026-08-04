#include <stdint.h>

#if defined(__x86_64__)
static void finish(long status) {
    __asm__ volatile("syscall" : : "a"(60L), "D"(status) : "rcx", "r11", "memory");
    for (;;) {}
}
#elif defined(__aarch64__)
static void finish(long status) {
    register long x0 __asm__("x0") = status;
    register long x8 __asm__("x8") = 93;
    __asm__ volatile("svc 0" : : "r"(x0), "r"(x8) : "memory");
    for (;;) {}
}
#else
#error unsupported guest architecture
#endif

static long write_result(void) {
    static const char result[] = "environment-ok\n";
#if defined(__x86_64__)
    long written;
    __asm__ volatile("syscall"
                     : "=a"(written)
                     : "a"(1L), "D"(1L), "S"(result), "d"(sizeof(result) - 1)
                     : "rcx", "r11", "memory");
    return written;
#else
    register long x0 __asm__("x0") = 1;
    register const char *x1 __asm__("x1") = result;
    register long x2 __asm__("x2") = sizeof(result) - 1;
    register long x8 __asm__("x8") = 64;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
#endif
}

static int equal(const unsigned char *left, const unsigned char *right) {
    while (*left == *right) {
        if (*left == 0)
            return 1;
        ++left;
        ++right;
    }
    return 0;
}

void entry(uintptr_t *stack) {
    uintptr_t count = *stack;
    unsigned char **environment = (unsigned char **)(stack + count + 2);
    static const unsigned char first[] = {'E', 'M', 'P', 'T', 'Y', '=', 0};
    static const unsigned char second[] = {'T', 'Z', '=', 'U', 'T', 'C', 0};
    if (!environment[0] || !environment[1] || environment[2])
        finish(1);
    if (!equal(environment[0], first) || !equal(environment[1], second))
        finish(2);
    if (write_result() != 15)
        finish(3);
    finish(0);
}

#if defined(__x86_64__)
__asm__(".global _start\n_start:\nmov %rsp, %rdi\njmp entry");
#elif defined(__aarch64__)
__asm__(".global _start\n_start:\nmov x0, sp\nb entry");
#endif
