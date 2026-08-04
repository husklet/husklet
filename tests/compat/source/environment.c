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
#endif

static int equal(const unsigned char *left, const unsigned char *right) {
    while (*left == *right) {
        if (*left == 0) return 1;
        ++left;
        ++right;
    }
    return 0;
}

void entry(uintptr_t *stack) {
    uintptr_t count = *stack;
    unsigned char **environment = (unsigned char **)(stack + count + 2);
    static const unsigned char first[] = {'T','Z','=','U','T','C',0xff,0};
    static const unsigned char second[] = {'E','M','P','T','Y','=',0};
    if (!environment[0] || !environment[1] || environment[2]) finish(1);
    if (!equal(environment[0], first) || !equal(environment[1], second)) finish(2);
    finish(0);
}

#if defined(__x86_64__)
__asm__(".global _start\n_start:\nmov %rsp, %rdi\njmp entry");
#elif defined(__aarch64__)
__asm__(".global _start\n_start:\nmov x0, sp\nb entry");
#endif
