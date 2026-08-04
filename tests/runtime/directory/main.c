#include <stdint.h>

#define AT_FDCWD -100L
#define AT_REMOVEDIR 0x200L
#define O_CREAT 0x40L
#define O_WRONLY 1L
#define DT_DIR 4
#define DT_REG 8
#define DT_LNK 10

#if defined(__x86_64__)
#define SYS_EXIT 60
#define SYS_CLOSE 3
#define SYS_DUP 32
#define SYS_GETDENTS64 217
#define SYS_OPENAT 257
#define SYS_MKDIRAT 258
#define SYS_UNLINKAT 263
#define SYS_SYMLINKAT 266
#define O_DIRECTORY 0x10000L
static long call6(long n, long a, long b, long c, long d, long e, long f) {
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    long result;
    __asm__ volatile("syscall" : "=a"(result)
        : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory");
    return result;
}
#elif defined(__aarch64__)
#define SYS_EXIT 93
#define SYS_CLOSE 57
#define SYS_DUP 23
#define SYS_GETDENTS64 61
#define SYS_OPENAT 56
#define SYS_MKDIRAT 34
#define SYS_UNLINKAT 35
#define SYS_SYMLINKAT 36
#define O_DIRECTORY 0x4000L
static long call6(long n, long a, long b, long c, long d, long e, long f) {
    register long x0 __asm__("x0") = a; register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c; register long x3 __asm__("x3") = d;
    register long x4 __asm__("x4") = e; register long x5 __asm__("x5") = f;
    register long x8 __asm__("x8") = n;
    __asm__ volatile("svc 0" : "+r"(x0)
        : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8) : "memory");
    return x0;
}
#else
#error unsupported guest architecture
#endif

static void finish(long status) {
    call6(SYS_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) {}
}

static uint16_t word16(const unsigned char *p) {
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static int64_t word64(const unsigned char *p) {
    uint64_t value = 0;
    for (int i = 0; i < 8; i++) value |= (uint64_t)p[i] << (i * 8);
    return (int64_t)value;
}

static int record(
    unsigned char *buffer,
    long count,
    const char *name,
    unsigned char type,
    int64_t *cookie
) {
    if (count != 24 || word16(buffer + 16) != 24 || buffer[18] != type) return 0;
    for (int i = 0; name[i] != 0; i++) if (buffer[19 + i] != (unsigned char)name[i]) return 0;
    int64_t next = word64(buffer + 8);
    if (next <= *cookie) return 0;
    *cookie = next;
    return 1;
}

void _start(void) {
    static const char dir[] = "enum";
    static const char file[] = "enum/a";
    static const char removed[] = "enum/deleted";
    static const char sub[] = "enum/sub";
    static const char link[] = "enum/l";
    static const char target[] = "a";
    unsigned char buffer[64];
    if (call6(SYS_MKDIRAT, AT_FDCWD, (long)dir, 0755, 0, 0, 0) != 0) finish(1);
    long created = call6(SYS_OPENAT, AT_FDCWD, (long)file,
        O_CREAT | O_WRONLY, 0644, 0, 0);
    if (created < 0 || call6(SYS_CLOSE, created, 0, 0, 0, 0, 0) != 0) finish(2);
    created = call6(SYS_OPENAT, AT_FDCWD, (long)removed,
        O_CREAT | O_WRONLY, 0644, 0, 0);
    if (created < 0 || call6(SYS_CLOSE, created, 0, 0, 0, 0, 0) != 0) finish(3);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)removed, 0, 0, 0, 0) != 0) finish(4);
    if (call6(SYS_SYMLINKAT, (long)target, AT_FDCWD, (long)link, 0, 0, 0) != 0) finish(5);
    if (call6(SYS_MKDIRAT, AT_FDCWD, (long)sub, 0755, 0, 0, 0) != 0) finish(6);
    long probe = call6(SYS_OPENAT, AT_FDCWD, (long)dir, O_DIRECTORY, 0, 0, 0);
    if (probe < 0) finish(7);
    if (call6(SYS_GETDENTS64, probe, (long)buffer, 1, 0, 0, 0) != -22) finish(8);
    if (call6(SYS_GETDENTS64, probe, 1, 4096, 0, 0, 0) != -14) finish(9);
    if (call6(SYS_GETDENTS64, -1, 1, 4096, 0, 0, 0) != -9) finish(10);
    if (call6(SYS_CLOSE, probe, 0, 0, 0, 0, 0) != 0) finish(11);
    long fd = call6(SYS_OPENAT, AT_FDCWD, (long)dir, O_DIRECTORY, 0, 0, 0);
    if (fd < 0) finish(11);
    long alias = call6(SYS_DUP, fd, 0, 0, 0, 0, 0);
    if (alias < 0) finish(11);
    int64_t cookie = -1;
    if (!record(buffer, call6(SYS_GETDENTS64, fd, (long)buffer, 24, 0, 0, 0),
            ".", DT_DIR, &cookie)) finish(12);
    if (!record(buffer, call6(SYS_GETDENTS64, alias, (long)buffer, 24, 0, 0, 0),
            "..", DT_DIR, &cookie)) finish(13);
    if (!record(buffer, call6(SYS_GETDENTS64, fd, (long)buffer, 24, 0, 0, 0),
            "a", DT_REG, &cookie)) finish(14);
    long count = call6(SYS_GETDENTS64, alias, (long)buffer, 24, 0, 0, 0);
    int link_first = record(buffer, count, "l", DT_LNK, &cookie);
    if (!link_first && !record(buffer, count, "sub", DT_DIR, &cookie)) finish(15);
    count = call6(SYS_GETDENTS64, fd, (long)buffer, 24, 0, 0, 0);
    if (!(link_first ? record(buffer, count, "sub", DT_DIR, &cookie)
                     : record(buffer, count, "l", DT_LNK, &cookie))) finish(16);
    if (call6(SYS_GETDENTS64, alias, (long)buffer, sizeof(buffer), 0, 0, 0) != 0) finish(17);
    if (call6(SYS_CLOSE, alias, 0, 0, 0, 0, 0) != 0) finish(18);
    if (call6(SYS_CLOSE, fd, 0, 0, 0, 0, 0) != 0) finish(19);
    if (call6(SYS_GETDENTS64, fd, 1, 4096, 0, 0, 0) != -9) finish(20);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)file, 0, 0, 0, 0) != 0) finish(21);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)link, 0, 0, 0, 0) != 0) finish(22);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)sub, AT_REMOVEDIR, 0, 0, 0) != 0) finish(23);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)dir, AT_REMOVEDIR, 0, 0, 0) != 0) finish(24);
    finish(0);
}
