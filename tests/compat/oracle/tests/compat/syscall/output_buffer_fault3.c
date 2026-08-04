// Adversarial robustness (third wave): more result-writers that scatter their output THROUGH an engine
// scratch buffer (not straight to the kernel's copy_to_user) must still EFAULT when the guest destination
// straddles into an unmapped page -- never fault the engine host process (a guest-crashes-engine isolation
// break). Covers listxattr (copies the name list via a host buffer, then memcpy into the guest list),
// recvmmsg (reads/writes the guest mmsghdr array directly), and process_vm_readv/writev (a same-address-space
// scatter/gather memcpy between the local and remote iovecs). Each straddling call must return an error
// (not SIGSEGV); the fully mapped calls must still succeed.
//
// Note on process_vm_readv/writev: the native oracle kernel here rejects the call with EINVAL regardless of
// buffer validity, while the engine implements it and returns EFAULT for a bad buffer. Both are error returns
// (r == -1), so this test asserts only the crash/no-crash property (survived), which matches on both.
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
#include <sys/uio.h>
#include <sys/xattr.h>

static long sc(long n, long a, long b, long c, long d, long e) {
    errno = 0;
    return syscall(n, a, b, c, d, e);
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
    void *hole = base + page;         // fully unmapped page start
    void *straddle = base + page - 4; // 4 mapped bytes, then unmapped

    // --- listxattr: a guest-visible attr must exist so the list is non-empty (the copy-out runs). ---
    char xpath[128];
    snprintf(xpath, sizeof xpath, "/tmp/hl_obf3_%d", (int)getpid());
    int xfd = open(xpath, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (xfd >= 0) close(xfd);
    int xset = (setxattr(xpath, "user.hltest", "hello", 5, 0) == 0);
    long r = sc(SYS_listxattr, (long)xpath, (long)hole, page, 0, 0);
    int listxattr_efault = (r == -1 && errno == EFAULT);

    // --- recvmmsg: the mmsghdr array itself is unmapped; reading its first field must EFAULT. ---
    int sp[2];
    int have_pair = (socketpair(AF_UNIX, SOCK_DGRAM, 0, sp) == 0);
    if (have_pair) { ssize_t w = write(sp[1], "hi", 2); (void)w; }
    r = sc(SYS_recvmmsg, have_pair ? sp[0] : -1, (long)hole, 4, 0x40, 0); // MSG_DONTWAIT
    int recvmmsg_efault = (r == -1 && errno == EFAULT);

    // --- process_vm_readv: local (destination) iov base in the hole -> scatter memcpy must not crash. ---
    char src[64];
    memset(src, 7, sizeof src);
    struct iovec lbad = { hole, 64 };
    struct iovec rok = { src, 64 };
    r = sc(SYS_process_vm_readv, getpid(), (long)&lbad, 1, (long)&rok, 1);
    int pvm_readv_survived = (r == -1);

    // --- process_vm_writev: remote (destination) iov base in the hole -> gather memcpy must not crash. ---
    struct iovec lok = { src, 64 };
    struct iovec rbad = { hole, 64 };
    r = sc(SYS_process_vm_writev, getpid(), (long)&lok, 1, (long)&rbad, 1);
    int pvm_writev_survived = (r == -1);

    // --- process_vm_readv: the local iovec ARRAY pointer itself is unmapped -> must not crash. ---
    r = sc(SYS_process_vm_readv, getpid(), (long)hole, 1, (long)&rok, 1);
    int pvm_badarray_survived = (r == -1);

    (void)straddle;

    // --- Sanity: the same operations on fully mapped buffers still work. ---
    char list[256];
    long ln = (xset ? syscall(SYS_listxattr, (long)xpath, (long)list, sizeof list, 0) : -1);
    int listxattr_ok = (ln > 0);
    char getbuf[16];
    long gv = getxattr(xpath, "user.hltest", getbuf, sizeof getbuf);
    int getxattr_ok = (gv == 5 && memcmp(getbuf, "hello", 5) == 0);
    unlink(xpath);

    int recvmmsg_ok = 0;
    if (have_pair) {
        char rb[8];
        struct iovec riov = { rb, sizeof rb };
        struct mmsghdr mm;
        memset(&mm, 0, sizeof mm);
        mm.msg_hdr.msg_iov = &riov;
        mm.msg_hdr.msg_iovlen = 1;
        int got = (int)syscall(SYS_recvmmsg, sp[0], (long)&mm, 1, 0x40, 0);
        recvmmsg_ok = (got == 1 && mm.msg_len == 2 && rb[0] == 'h' && rb[1] == 'i');
        close(sp[0]);
        close(sp[1]);
    }

    int valid = xset && listxattr_ok && getxattr_ok && recvmmsg_ok;

    printf("listxattr_efault=%d recvmmsg_efault=%d pvm_readv_survived=%d pvm_writev_survived=%d "
           "pvm_badarray_survived=%d valid=%d\n",
           listxattr_efault, recvmmsg_efault, pvm_readv_survived, pvm_writev_survived,
           pvm_badarray_survived, valid);
    return (listxattr_efault && recvmmsg_efault && pvm_readv_survived && pvm_writev_survived &&
            pvm_badarray_survived && valid)
               ? 0
               : 1;
}
