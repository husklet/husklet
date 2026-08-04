// Adversarial robustness (second wave): more syscalls that write their result straight into a
// guest-supplied buffer must EFAULT (like Linux copy_to_user / copy_from_user) when that buffer
// straddles an unmapped page, never fault the engine host process. Covers getpeername (sockaddr +
// addrlen), accept (peer sockaddr), rt_sigprocmask (oldset written + set read -- including the x86
// inline W4F fast path), and sched_getattr (attr struct). A missing mapping check crashes the engine
// with SIGSEGV -- a guest-crashes-engine isolation break. Fully mapped buffers must still succeed.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <netinet/in.h>

static long sc(long n, long a, long b, long c, long d, long e) {
    errno = 0;
    return syscall(n, a, b, c, d, e);
}

// Build a connected loopback TCP pair; returns the listening fd and stores the connected client fd.
static int connected_pair(int *client) {
    int l = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(0x7f000001);
    a.sin_port = 0;
    if (bind(l, (struct sockaddr *)&a, sizeof a) != 0) return -1;
    if (listen(l, 1) != 0) return -1;
    socklen_t al = sizeof a;
    if (getsockname(l, (struct sockaddr *)&a, &al) != 0) return -1;
    int c = socket(AF_INET, SOCK_STREAM, 0);
    if (connect(c, (struct sockaddr *)&a, sizeof a) != 0) return -1;
    *client = c;
    return l;
}

int main(void) {
    signal(SIGPIPE, SIG_IGN);
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 10;

    // Two mapped pages, second unmapped: pointers near the boundary straddle into the hole.
    unsigned char *base = mmap(NULL, (size_t)page * 2, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) return 11;
    if (munmap(base + page, (size_t)page) != 0) return 12;
    void *straddle = base + page - 4;   // 4 mapped bytes, then unmapped
    void *hole = base + page;           // fully unmapped page start
    *(socklen_t *)straddle = 64;        // addrlen capacity for the socket cases

    // getpeername: addrlen cell straddles, addr buffer fully in the hole.
    int cli = -1;
    int lst = connected_pair(&cli);
    long r = sc(SYS_getpeername, cli, (long)hole, (long)straddle, 0, 0);
    int getpeername_efault = (r == -1 && errno == EFAULT);

    // accept: a connection is pending on the listener; the peer sockaddr write must EFAULT.
    r = sc(SYS_accept, lst, (long)hole, (long)straddle, 0, 0);
    int accept_efault = (r == -1 && errno == EFAULT);

    // rt_sigprocmask: oldset written into the straddling buffer (set NULL).
    r = sc(SYS_rt_sigprocmask, SIG_BLOCK, 0, (long)straddle, 8, 0);
    int sigprocmask_old_efault = (r == -1 && errno == EFAULT);

    // rt_sigprocmask: set read from the straddling buffer (oldset NULL).
    r = sc(SYS_rt_sigprocmask, SIG_BLOCK, (long)straddle, 0, 8, 0);
    int sigprocmask_set_efault = (r == -1 && errno == EFAULT);

    // sched_getattr: attr struct written into the straddling buffer.
    r = sc(SYS_sched_getattr, 0, (long)straddle, 48, 0, 0);
    int sched_getattr_efault = (r == -1 && errno == EFAULT);

    // Sanity: the same calls on fully mapped buffers still work.
    int c2 = -1;
    int l2 = connected_pair(&c2);
    struct sockaddr_in pa;
    socklen_t pl = sizeof pa;
    int gp_ok = (getpeername(c2, (struct sockaddr *)&pa, &pl) == 0);
    socklen_t sset;
    int spm_ok = (syscall(SYS_rt_sigprocmask, SIG_BLOCK, 0, (long)&sset, 8) == 0);
    unsigned char attr[64];
    int sga_ok = (syscall(SYS_sched_getattr, 0, (long)attr, 48, 0) == 0);
    (void)l2;

    printf("getpeername_efault=%d accept_efault=%d sigprocmask_old_efault=%d sigprocmask_set_efault=%d "
           "sched_getattr_efault=%d valid=%d\n",
           getpeername_efault, accept_efault, sigprocmask_old_efault, sigprocmask_set_efault,
           sched_getattr_efault, gp_ok && spm_ok && sga_ok);
    return (getpeername_efault && accept_efault && sigprocmask_old_efault && sigprocmask_set_efault &&
            sched_getattr_efault && gp_ok && spm_ok && sga_ok)
               ? 0
               : 1;
}
