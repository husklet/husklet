#include <stdint.h>

#if defined(__aarch64__)
#define GUEST_WRITE 64
#define GUEST_EXIT 93
static long guest_call(long number, long first, long second, long third) {
    register long x0 __asm__("x0") = first;
    register long x1 __asm__("x1") = second;
    register long x2 __asm__("x2") = third;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
}
#elif defined(__x86_64__)
#define GUEST_WRITE 1
#define GUEST_EXIT 60
static long guest_call(long number, long first, long second, long third) {
    long result;
    __asm__ volatile("syscall" : "=a"(result) : "a"(number), "D"(first), "S"(second), "d"(third)
                     : "rcx", "r11", "memory");
    return result;
}
#else
#error unsupported guest architecture
#endif

static __attribute__((noreturn)) void finish(long status) {
    guest_call(GUEST_EXIT, status, 0, 0);
    for (;;) {}
}

void _start(void) {
    static const char message[] = "status\n";
    if (guest_call(GUEST_WRITE, 1, (long)message, sizeof(message) - 1) != (long)(sizeof(message) - 1)) finish(111);
    finish(17);
}
