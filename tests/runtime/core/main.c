#include <stdint.h>

#if defined(__aarch64__)
#define SYS_EXIT 93
#define SYS_GETPID 172
#define SYS_WRITE 64
static long call3(long number, long first, long second, long third) {
    register long x0 __asm__("x0") = first;
    register long x1 __asm__("x1") = second;
    register long x2 __asm__("x2") = third;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
}
#elif defined(__x86_64__)
#define SYS_EXIT 60
#define SYS_GETPID 39
#define SYS_WRITE 1
static long call3(long number, long first, long second, long third) {
    long result;
    __asm__ volatile("syscall" : "=a"(result) : "a"(number), "D"(first), "S"(second), "d"(third)
                     : "rcx", "r11", "memory");
    return result;
}
#else
#error unsupported guest architecture
#endif

static __attribute__((noreturn)) void finish(long status) {
    call3(SYS_EXIT, status, 0, 0);
    for (;;) {}
}

static unsigned long length(const char *value) {
    unsigned long result = 0;
    while (value[result] != '\0') ++result;
    return result;
}

static int equal(const char *left, const char *right) {
    while (*left == *right) {
        if (*left == '\0') return 1;
        ++left;
        ++right;
    }
    return 0;
}

static void output(const char *value) {
    unsigned long size = length(value);
    if (call3(SYS_WRITE, 1, (long)value, (long)size) != (long)size) finish(111);
}

static __attribute__((noreturn)) void run(const char *name) {
    if (equal(name, "exit")) finish(42);
    if (equal(name, "status")) {
        output("status\n");
        finish(17);
    }
    if (equal(name, "write")) {
        output("runtime-core write ok\n");
        finish(0);
    }
    if (equal(name, "getpid")) {
        if (call3(SYS_GETPID, 0, 0, 0) <= 0) finish(1);
        output("runtime-core getpid ok\n");
        finish(0);
    }
    output("unknown runtime-core case\n");
    finish(64);
}

static __attribute__((noreturn)) void entry(uintptr_t *stack) {
    unsigned long count = *stack;
    char **arguments = (char **)(stack + 1);
    if (count != 2 || arguments[1] == 0) finish(64);
    run(arguments[1]);
}

#if defined(__aarch64__)
__asm__(".global _start\n_start:\nmov x0, sp\nb entry");
#else
__asm__(".global _start\n_start:\nmov %rsp, %rdi\njmp entry");
#endif
