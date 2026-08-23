#pragma once

#if defined(__aarch64__)
static inline long guest_call(long number, long first, long second, long third) {
    register long x0 __asm__("x0") = first;
    register long x1 __asm__("x1") = second;
    register long x2 __asm__("x2") = third;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
}

#define GUEST_WRITE 64
#define GUEST_EXIT 93
#define GUEST_GETPID 172
#elif defined(__x86_64__)
static inline long guest_call(long number, long first, long second, long third) {
    register long rax __asm__("rax") = number;
    register long rdi __asm__("rdi") = first;
    register long rsi __asm__("rsi") = second;
    register long rdx __asm__("rdx") = third;
    __asm__ volatile("syscall" : "+r"(rax) : "r"(rdi), "r"(rsi), "r"(rdx) : "rcx", "r11", "memory");
    return rax;
}

#define GUEST_WRITE 1
#define GUEST_EXIT 60
#define GUEST_GETPID 39
#else
#error unsupported guest architecture
#endif

__attribute__((noreturn)) static inline void guest_exit(long status) {
    guest_call(GUEST_EXIT, status, 0, 0);
    __builtin_unreachable();
}

static inline long guest_write(const char *bytes, long length) {
    return guest_call(GUEST_WRITE, 1, (long)bytes, length);
}
