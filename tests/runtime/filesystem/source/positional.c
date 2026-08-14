#include <stdint.h>

#define AT_FDCWD -100L
#define O_CREAT 0x40L
#define O_RDWR 2L
#define O_WRONLY 1L
#define O_APPEND 0x400L
#define SEEK_SET 0L
#define SEEK_CUR 1L
#define PROT_READ 1L
#define PROT_WRITE 2L
#define MAP_PRIVATE 2L
#define MAP_ANONYMOUS 0x20L

#if defined(__x86_64__)
#define SYS_EXIT 60
#define SYS_CLOSE 3
#define SYS_DUP 32
#define SYS_LSEEK 8
#define SYS_PREAD64 17
#define SYS_PWRITE64 18
#define SYS_PREADV 295
#define SYS_PWRITEV 296
#define SYS_READ 0
#define SYS_WRITE 1
#define SYS_OPENAT 257
#define SYS_UNLINKAT 263
#define SYS_MMAP 9
#define SYS_MUNMAP 11
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
#define SYS_LSEEK 62
#define SYS_PREAD64 67
#define SYS_PWRITE64 68
#define SYS_PREADV 69
#define SYS_PWRITEV 70
#define SYS_READ 63
#define SYS_WRITE 64
#define SYS_OPENAT 56
#define SYS_UNLINKAT 35
#define SYS_MMAP 222
#define SYS_MUNMAP 215
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

static int same(const unsigned char *left, const char *right, int length) {
    for (int i = 0; i < length; i++) if (left[i] != (unsigned char)right[i]) return 0;
    return 1;
}

struct vector { void *base; unsigned long length; };

void _start(void) {
    static const char path[] = "/positional";
    static const char initial[] = "abcdef";
    static const char patch[] = "XY";
    static const char append[] = "Z";
    unsigned char bytes[16];
    long fd = call6(SYS_OPENAT, AT_FDCWD, (long)path, O_CREAT | O_RDWR, 0644, 0, 0);
    if (fd < 0) finish(1);
    if (call6(SYS_WRITE, fd, (long)initial, 6, 0, 0, 0) != 6) finish(2);
    long alias = call6(SYS_DUP, fd, 0, 0, 0, 0, 0);
    if (alias < 0) finish(3);
    if (call6(SYS_PREAD64, alias, (long)bytes, 3, 1, 0, 0) != 3
        || !same(bytes, "bcd", 3)) finish(4);
    if (call6(SYS_LSEEK, fd, 0, SEEK_CUR, 0, 0, 0) != 6) finish(5);
    if (call6(SYS_PWRITE64, fd, (long)patch, 2, 2, 0, 0) != 2) finish(6);
    if (call6(SYS_LSEEK, alias, 0, SEEK_CUR, 0, 0, 0) != 6) finish(7);
    if (call6(SYS_PREAD64, fd, (long)bytes, 4, 5, 0, 0) != 1 || bytes[0] != 'f') finish(8);
    if (call6(SYS_PREAD64, -1, 1, 3, 0, 0, 0) != -9) finish(9);
    if (call6(SYS_PREAD64, fd, 1, 3, 0, 0, 0) != -14) finish(10);
    if (call6(SYS_PWRITE64, -1, 1, 3, 0, 0, 0) != -9) finish(11);
    if (call6(SYS_PWRITE64, fd, 1, 3, 0, 0, 0) != -14) finish(12);
    unsigned char first[2], second[2];
    volatile struct vector reads[2];
    reads[0].base = first; reads[0].length = 2;
    reads[1].base = second; reads[1].length = 2;
    if (call6(SYS_PREADV, alias, (long)reads, 2, 1, 0, 0) != 4
        || !same(first, "bX", 2) || !same(second, "Ye", 2)) finish(22);
    if (call6(SYS_LSEEK, fd, 0, SEEK_CUR, 0, 0, 0) != 6) finish(23);
    static const char one[] = "12", two[] = "34";
    volatile struct vector writes[2];
    writes[0].base = (void *)one; writes[0].length = 2;
    writes[1].base = (void *)two; writes[1].length = 2;
    if (call6(SYS_PWRITEV, fd, (long)writes, 2, 1, 0, 0) != 4) finish(24);
    if (call6(SYS_LSEEK, alias, 0, SEEK_CUR, 0, 0, 0) != 6) finish(25);
    volatile struct vector partial[2];
    partial[0].base = (void *)"Q"; partial[0].length = 1;
    partial[1].base = (void *)1; partial[1].length = 1;
    if (call6(SYS_PWRITEV, fd, (long)partial, 2, 5, 0, 0) != -14) finish(26);
    volatile struct vector bad_read[2];
    bad_read[0].base = first; bad_read[0].length = 1;
    bad_read[1].base = (void *)1; bad_read[1].length = 1;
    first[0] = 0;
    /* Linux publishes the bytes copied before the fault and only reports
       EFAULT when nothing at all was transferred. */
    if (call6(SYS_PREADV, fd, (long)bad_read, 2, 0, 0, 0) != 1
        || first[0] != 'a') finish(27);
    if (call6(SYS_LSEEK, fd, 0, SEEK_CUR, 0, 0, 0) != 6) finish(36);
    volatile struct vector lead_fault[2];
    lead_fault[0].base = (void *)1; lead_fault[0].length = 1;
    lead_fault[1].base = first; lead_fault[1].length = 1;
    if (call6(SYS_PREADV, fd, (long)lead_fault, 2, 0, 0, 0) != -14) finish(37);
    if (call6(SYS_PREADV, -1, 1, 2, 0, 0, 0) != -9) finish(28);
    if (call6(SYS_PWRITEV, -1, 1, 2, 0, 0, 0) != -9) finish(29);
    if (call6(SYS_PREADV, fd, 1, 1025, 0, 0, 0) != -22) finish(30);
    if (call6(SYS_PWRITEV, fd, 1, 1025, 0, 0, 0) != -22) finish(31);
    if (call6(SYS_PREADV, -1, 1, 1025, 0, 0, 0) != -9) finish(38);
    if (call6(SYS_PWRITEV, -1, 1, 1025, 0, 0, 0) != -9) finish(39);
    if (call6(SYS_PREADV, fd, 1, 0, 0, 0, 0) != 0) finish(32);
    if (call6(SYS_PWRITEV, fd, 1, 0, 0, 0, 0) != 0) finish(33);
    if (call6(SYS_PREADV, fd, (long)reads, 1, -1, -1, 0) != -22) finish(34);
    unsigned char tail[4] = {0};
    volatile struct vector short_read[2];
    short_read[0].base = tail; short_read[0].length = 1;
    short_read[1].base = tail + 1; short_read[1].length = 3;
    if (call6(SYS_PREADV, fd, (long)short_read, 2, 4, 0, 0) != 2
        || !same(tail, "4f", 2)) finish(35);
    long appender = call6(SYS_OPENAT, AT_FDCWD, (long)path, O_WRONLY | O_APPEND, 0, 0, 0);
    if (appender < 0) finish(13);
    if (call6(SYS_PWRITE64, appender, (long)append, 1, 0, 0, 0) != 1) finish(14);
    if (call6(SYS_LSEEK, appender, 0, SEEK_CUR, 0, 0, 0) != 0) finish(15);
    if (call6(SYS_LSEEK, fd, 0, SEEK_SET, 0, 0, 0) != 0) finish(16);
    if (call6(SYS_READ, fd, (long)bytes, 7, 0, 0, 0) != 7
        || !same(bytes, "a1234fZ", 7)) finish(17);
    long crossing_address = call6(SYS_MMAP, 0, 8192, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (crossing_address < 0) finish(40);
    for (int i = 0; i < 4096; ++i) ((unsigned char *)crossing_address)[i] = 'R';
    if (call6(SYS_MUNMAP, crossing_address + 4096, 4096, 0, 0, 0, 0) != 0) finish(41);
    volatile struct vector crossing = {(void *)crossing_address, 8192};
    if (call6(SYS_PWRITEV, -1, (long)&crossing, 1, 8192, 0, 0) != -9) finish(42);
    if (call6(SYS_PWRITEV, fd, (long)&crossing, 1, 8192, 0, 0) != 4096) finish(43);
    long read_crossing_address = call6(SYS_MMAP, 0, 8192, PROT_READ | PROT_WRITE,
                                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (read_crossing_address < 0) finish(44);
    if (call6(SYS_MUNMAP, read_crossing_address + 4096, 4096, 0, 0, 0, 0) != 0) finish(45);
    volatile struct vector read_crossing = {(void *)read_crossing_address, 8192};
    if (call6(SYS_PREADV, fd, (long)&read_crossing, 1, 8192, 0, 0) != 4096
        || ((unsigned char *)read_crossing_address)[0] != 'R'
        || ((unsigned char *)read_crossing_address)[4095] != 'R') finish(46);
    if (call6(SYS_CLOSE, appender, 0, 0, 0, 0, 0) != 0) finish(18);
    if (call6(SYS_CLOSE, alias, 0, 0, 0, 0, 0) != 0) finish(19);
    if (call6(SYS_CLOSE, fd, 0, 0, 0, 0, 0) != 0) finish(20);
    if (call6(SYS_UNLINKAT, AT_FDCWD, (long)path, 0, 0, 0, 0) != 0) finish(21);
    finish(0);
}
