// Adversarial robustness: syscalls that write their result straight into a guest-supplied buffer must
// EFAULT (like Linux copy_to_user) when that buffer straddles an unmapped page, never fault the engine
// host process. Covers socketpair (fd pair), getsockname (sockaddr + addrlen), and readlinkat (link
// target). A missing mapping check crashes the engine with SIGSEGV -- a guest-crashes-engine isolation
// break. Fully mapped buffers must still succeed.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>

static long sc(long n, long a, long b, long c, long d, long e) {
    errno = 0;
    return syscall(n, a, b, c, d, e);
}

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 10;

    // Two mapped pages, second unmapped: pointers near the boundary straddle into the hole.
    unsigned char *base = mmap(NULL, (size_t)page * 2, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) return 11;
    if (munmap(base + page, (size_t)page) != 0) return 12;
    void *straddle = base + page - 4; // 4 mapped bytes, then unmapped

    // socketpair: fd pair written to the straddling array.
    long r = sc(SYS_socketpair, AF_UNIX, SOCK_STREAM, 0, (long)straddle, 0);
    int socketpair_efault = (r == -1 && errno == EFAULT);

    // getsockname: addrlen cell sits on the unmapped page, addr buffer straddles.
    int s = socket(AF_INET, SOCK_STREAM, 0);
    r = sc(SYS_getsockname, s, (long)straddle, (long)(base + page), 0, 0);
    int getsockname_efault = (r == -1 && errno == EFAULT);

    // readlinkat: link target written to the straddling buffer.
    r = sc(SYS_readlinkat, AT_FDCWD, (long)"/proc/self/exe", (long)straddle, (long)page, 0);
    int readlinkat_efault = (r == -1 && errno == EFAULT);

    // Sanity: the same calls on fully mapped buffers still work.
    int sv[2];
    int sp_ok = (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    if (sp_ok) { close(sv[0]); close(sv[1]); }
    char lbuf[256];
    ssize_t ln = readlink("/proc/self/exe", lbuf, sizeof lbuf);
    int rl_ok = (ln > 0);

    printf("socketpair_efault=%d getsockname_efault=%d readlinkat_efault=%d valid=%d\n",
           socketpair_efault, getsockname_efault, readlinkat_efault, sp_ok && rl_ok);
    return (socketpair_efault && getsockname_efault && readlinkat_efault && sp_ok && rl_ok) ? 0 : 1;
}
