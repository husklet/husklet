#if defined(__x86_64__)
#define NR_OPENAT 257
#define NR_CLOSE 3
#define NR_READ 0
#define NR_WRITE 1
#define NR_PWRITE 18
#define NR_LSEEK 8
#define NR_DUP 32
#define NR_FTRUNCATE 77
#define NR_EXIT 60
static long call6(long n, long a, long b, long c, long d, long e, long f) {
    long result;
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    __asm__ volatile("syscall" : "=a"(result)
        : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory");
    return result;
}
#elif defined(__aarch64__)
#define NR_OPENAT 56
#define NR_CLOSE 57
#define NR_READ 63
#define NR_WRITE 64
#define NR_PWRITE 68
#define NR_LSEEK 62
#define NR_DUP 23
#define NR_FTRUNCATE 46
#define NR_EXIT 93
static long call6(long n, long a, long b, long c, long d, long e, long f) {
    register long x8 __asm__("x8") = n;
    register long x0 __asm__("x0") = a;
    register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c;
    register long x3 __asm__("x3") = d;
    register long x4 __asm__("x4") = e;
    register long x5 __asm__("x5") = f;
    __asm__ volatile("svc 0" : "+r"(x0)
        : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5) : "memory");
    return x0;
}
#else
#error unsupported guest architecture
#endif

static long call1(long n, long a) { return call6(n, a, 0, 0, 0, 0, 0); }
static long call2(long n, long a, long b) { return call6(n, a, b, 0, 0, 0, 0); }
static long call3(long n, long a, long b, long c) { return call6(n, a, b, c, 0, 0, 0); }
static long call4(long n, long a, long b, long c, long d) { return call6(n, a, b, c, d, 0, 0); }

static void fail(long code) { call1(NR_EXIT, code); }

void _start(void) {
    char bytes[4];
    long file = call4(NR_OPENAT, -100, (long)"/created", 2 | 64 | 128, 0600);
    if (file < 0 || call3(NR_WRITE, file, (long)"abc", 3) != 3) fail(1);
    long alias = call1(NR_DUP, file);
    if (alias < 0 || call3(NR_LSEEK, file, 0, 0) != 0) fail(2);
    if (call3(NR_READ, file, (long)bytes, 1) != 1 || bytes[0] != 'a') fail(3);
    if (call3(NR_READ, alias, (long)bytes, 1) != 1 || bytes[0] != 'b') fail(4);
    if (call4(NR_PWRITE, file, (long)"Z", 1, 0) != 1) fail(5);
    if (call3(NR_READ, file, (long)bytes, 1) != 1 || bytes[0] != 'c') fail(10);
    call1(NR_CLOSE, alias);
    call1(NR_CLOSE, file);

    file = call4(NR_OPENAT, -100, (long)"/created", 1 | 1024, 0);
    if (file < 0 || call4(NR_PWRITE, file, (long)"D", 1, 0) != 1) fail(6);
    if (call2(NR_FTRUNCATE, file, 2) != 0) fail(7);
    call1(NR_CLOSE, file);
    file = call4(NR_OPENAT, -100, (long)"/created", 0, 0);
    if (file < 0 || call3(NR_READ, file, (long)bytes, sizeof bytes) != 2
        || bytes[0] != 'Z' || bytes[1] != 'b') fail(8);
    if (call3(NR_WRITE, file, (long)"x", 1) != -9) fail(9);
    call1(NR_CLOSE, file);
    call3(NR_WRITE, 1, (long)"projected-write-ok\n", 19);
    call1(NR_EXIT, 0);
}
