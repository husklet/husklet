#pragma once
#if defined(__aarch64__)
static inline long guest_call(long n, long a, long b, long c) {
    register long x0 __asm__("x0") = a, x1 __asm__("x1") = b, x2 __asm__("x2") = c;
    register long x8 __asm__("x8") = n;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
}
#define GUEST_WRITE 64
#define GUEST_EXIT 93
#define GUEST_GETPID 172
#elif defined(__x86_64__)
static inline long guest_call(long n, long a, long b, long c) {
    register long rax __asm__("rax") = n, rdi __asm__("rdi") = a;
    register long rsi __asm__("rsi") = b, rdx __asm__("rdx") = c;
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
    guest_call(GUEST_EXIT, status, 0, 0); __builtin_unreachable();
}
static inline long guest_write(const char *bytes, long length) {
    return guest_call(GUEST_WRITE, 1, (long)bytes, length);
}
