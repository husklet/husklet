#include <stdint.h>

#define AT_FDCWD -100L
#define AT_EMPTY_PATH 0x1000L
#define AT_SYMLINK_NOFOLLOW 0x100L
#define AT_REMOVEDIR 0x200L
#define O_CREAT 0x40L
#define O_WRONLY 1L

#if defined(__x86_64__)
#define SYS_EXIT 60
#define SYS_CLOSE 3
#define SYS_FORK 57
#define SYS_WAIT4 61
#define SYS_OPENAT 257
#define SYS_MKDIRAT 258
#define SYS_NEWFSTATAT 262
#define SYS_UNLINKAT 263
#define SYS_RENAMEAT 264
#define SYS_LINKAT 265
#define SYS_SYMLINKAT 266
#define SYS_READLINKAT 267
#define SYS_STATX 332
#define O_DIRECTORY 0x10000L

static long call6(long n, long a, long b, long c, long d, long e, long f) {
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    long result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return result;
}
#elif defined(__aarch64__)
#define SYS_EXIT 93
#define SYS_CLOSE 57
#define SYS_FORK 220
#define SYS_WAIT4 260
#define SYS_OPENAT 56
#define SYS_MKDIRAT 34
#define SYS_NEWFSTATAT 79
#define SYS_UNLINKAT 35
#define SYS_RENAMEAT 38
#define SYS_LINKAT 37
#define SYS_SYMLINKAT 36
#define SYS_READLINKAT 78
#define SYS_STATX 291
#define O_DIRECTORY 0x4000L

static long call6(long n, long a, long b, long c, long d, long e, long f) {
    register long x0 __asm__("x0") = a;
    register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c;
    register long x3 __asm__("x3") = d;
    register long x4 __asm__("x4") = e;
    register long x5 __asm__("x5") = f;
    register long x8 __asm__("x8") = n;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8) : "memory");
    return x0;
}
#else
#error unsupported guest architecture
#endif

static void finish(long status) {
    call6(SYS_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) {}
}

void _start(void) {
    static const char target[] = "target";
    static const char link[] = "link";
    static const char empty[] = "";
    static const char root[] = ".";
    static const char work[] = "work";
    static const char a[] = "work/a";
    static const char b[] = "work/b";
    static const char c[] = "work/c";
    static const char work_link[] = "work/l";
    static const char relative_target[] = "target";
    static const char forked[] = "forked";
    unsigned char stat_buffer[512];
    unsigned char link_buffer[16];
    long target_fd = call6(SYS_OPENAT, AT_FDCWD, (long)target, O_CREAT | O_WRONLY, 0644, 0, 0);
    if (target_fd < 0) finish(25);
    if (call6(SYS_CLOSE, target_fd, 0, 0, 0, 0, 0) != 0) finish(26);
    if (call6(SYS_SYMLINKAT, (long)target, AT_FDCWD, (long)link, 0, 0, 0) != 0) finish(27);
    if (call6(SYS_NEWFSTATAT, AT_FDCWD, (long)target, (long)stat_buffer, 0, 0, 0) != 0) finish(1);
    if (call6(SYS_NEWFSTATAT, AT_FDCWD, (long)link, (long)stat_buffer, AT_SYMLINK_NOFOLLOW, 0, 0) != 0) finish(2);
    if (call6(SYS_STATX, AT_FDCWD, (long)target, 0, 0x7ff, (long)stat_buffer, 0) != 0) finish(3);
    long count = call6(SYS_READLINKAT, AT_FDCWD, (long)link, (long)link_buffer, sizeof(link_buffer), 0, 0);
    if (count != 6 || link_buffer[0] != 't' || link_buffer[5] != 't') finish(4);
    long descriptor = call6(SYS_OPENAT, AT_FDCWD, (long)target, 0, 0, 0, 0);
    if (descriptor < 0) finish(5);
    if (call6(SYS_NEWFSTATAT, descriptor, (long)empty, (long)stat_buffer, AT_EMPTY_PATH, 0, 0) != 0) finish(6);
    long directory = call6(SYS_OPENAT, AT_FDCWD, (long)root, O_DIRECTORY, 0, 0, 0);
    if (directory < 0) finish(7);
    if (call6(SYS_MKDIRAT, directory, (long)work, 0755, 0, 0, 0) != 0) finish(8);
    long file = call6(SYS_OPENAT, directory, (long)a, O_CREAT | O_WRONLY, 0644, 0, 0);
    if (file < 0) finish(9);
    if (call6(SYS_CLOSE, file, 0, 0, 0, 0, 0) != 0) finish(10);
    if (call6(SYS_RENAMEAT, directory, (long)a, directory, (long)b, 0, 0) != 0) finish(11);
    if (call6(SYS_LINKAT, directory, (long)b, directory, (long)c, 0, 0) != 0) finish(12);
    if (call6(SYS_UNLINKAT, directory, (long)b, 0, 0, 0, 0) != 0) finish(13);
    if (call6(SYS_UNLINKAT, directory, (long)c, 0, 0, 0, 0) != 0) finish(14);
    if (call6(SYS_SYMLINKAT, (long)relative_target, directory, (long)work_link, 0, 0, 0) != 0) finish(15);
    if (call6(SYS_NEWFSTATAT, directory, (long)work_link, (long)stat_buffer, AT_SYMLINK_NOFOLLOW, 0, 0) != 0)
        finish(16);
    if (call6(SYS_UNLINKAT, directory, (long)work_link, 0, 0, 0, 0) != 0) finish(17);
    if (call6(SYS_UNLINKAT, directory, (long)work, AT_REMOVEDIR, 0, 0, 0) != 0) finish(18);
#if defined(__aarch64__)
    long fork_flags = 17;
#else
    long fork_flags = 0;
#endif
    long child = call6(SYS_FORK, fork_flags, 0, 0, 0, 0, 0);
    if (child < 0) finish(19);
    if (child == 0) {
        long created = call6(SYS_OPENAT, AT_FDCWD, (long)forked, O_CREAT | O_WRONLY, 0644, 0, 0);
        if (created < 0) finish(20);
        call6(SYS_CLOSE, created, 0, 0, 0, 0, 0);
        finish(0);
    }
    uint32_t status = 0;
    if (call6(SYS_WAIT4, child, (long)&status, 0, 0, 0, 0) != child) finish(21);
    if (status != 0) finish(22);
    if (call6(SYS_NEWFSTATAT, AT_FDCWD, (long)forked, (long)stat_buffer, 0, 0, 0) != 0) finish(23);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)forked, 0, 0, 0, 0) != 0) finish(24);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)link, 0, 0, 0, 0) != 0) finish(28);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)target, 0, 0, 0, 0) != 0) finish(29);
    finish(0);
}
