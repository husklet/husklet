// syscall-compat regression: kernel-side bad-pointer validation. Raw syscalls handed a (void*)-1 user buffer
// must fail with EFAULT from the kernel, never fault the calling task. Each probe runs in its own child so a
// mis-translation that faults is observed deterministically (reported as sig=N) rather than killing the
// harness. Arch-neutral: per-probe errno (or signal) printed. Output is unbuffered so a crash cannot hide it.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#define BAD ((long)-1)

// Runs `syscall(nr,...)` in a child; returns the errno on failure, 0 on success, or -sig if it was killed.
static int probe(long nr, long a, long b, long c, long d, long e) {
    pid_t p = fork();
    if (p == 0) {
        long r = syscall(nr, a, b, c, d, e);
        _exit(r == -1 ? (errno & 0x7f) : 0);
    }
    int st = 0;
    waitpid(p, &st, 0);
    if (WIFSIGNALED(st)) return -WTERMSIG(st);
    return WEXITSTATUS(st);
}

// PAGE-STRADDLE variant of the bad-pointer probe. The BAD=(void*)-1 probes above catch handlers that skip
// validation entirely (the pointer is wholly unmapped). They do NOT catch the subtler failure mode where a
// handler validates a multi-byte output object with a SINGLE-PAGE / first-byte-only check
// (host_addr_mapped(ptr)) and then reads/writes the whole object: a pointer whose object STRADDLES a page
// boundary into an inaccessible page passes the first-page check but faults the engine on the copy, where
// the kernel returns EFAULT. (This is exactly the get_mempolicy case-236 bug: it validated `mode` with a
// single-page probe, then wrote all 4 bytes.) We reproduce it deterministically with a PROT_NONE guard page:
// map two pages, mprotect the SECOND PROT_NONE, and place the object `off` bytes before the guard so it
// straddles into the inaccessible page. hl force-maps guest anon memory host-RW, so PROT_NONE is the one
// guest access class whose "inaccessible" intent lives only in the engine's own registry (the LTP
// tst_get_bad_addr idiom) -- a handler that consults it via host_range_mapped(ptr, sizeof obj) EFAULTs; one
// that uses host_addr_mapped(ptr) writes into the guard page and wrongly succeeds. Each output syscall below
// copies a fixed-size struct straight into the guest buffer, so the correct answer is uniformly EFAULT(14).
static long straddle_ptr(size_t off) {
    long ps = sysconf(_SC_PAGESIZE);
    char *b = mmap(NULL, (size_t)ps * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (b == MAP_FAILED) _exit(97);
    if (mprotect(b + ps, (size_t)ps, PROT_NONE) != 0) _exit(96); // second page inaccessible
    return (long)(b + ps - off);                                 // object of size > off straddles the guard
}

// Like probe(), but arg index `ai` (0..4) is replaced with a straddling output pointer built in the child so
// each probe gets a fresh guard mapping. `off` is how many bytes of the object sit in the mapped page.
static int straddle(long nr, size_t off, int ai, long a, long b, long c, long d, long e) {
    pid_t p = fork();
    if (p == 0) {
        long args[5] = {a, b, c, d, e};
        args[ai] = straddle_ptr(off);
        long r = syscall(nr, args[0], args[1], args[2], args[3], args[4]);
        _exit(r == -1 ? (errno & 0x7f) : 0);
    }
    int st = 0;
    waitpid(p, &st, 0);
    if (WIFSIGNALED(st)) return -WTERMSIG(st);
    return WEXITSTATUS(st);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    int fd = open("/dev/zero", O_RDONLY);
    printf("getrusage=%d\n", probe(SYS_getrusage, 0, BAD, 0, 0, 0));
    printf("newfstatat=%d\n", probe(SYS_newfstatat, fd, (long)"", BAD, 0x1000 /*AT_EMPTY_PATH*/, 0));
    printf("gettimeofday=%d\n", probe(SYS_gettimeofday, BAD, 0, 0, 0, 0));
    printf("times=%d\n", probe(SYS_times, BAD, 0, 0, 0, 0));
    printf("read=%d\n", probe(SYS_read, fd, BAD, 16, 0, 0));
    printf("pipe2=%d\n", probe(SYS_pipe2, BAD, 0, 0, 0, 0));
    // inotify_add_watch with a wild path pointer: the kernel returns EFAULT; a handler that hands the
    // pointer straight to its path resolver faults the engine and kills the guest with sig=11 instead.
    int ino = syscall(SYS_inotify_init1, 0);
    printf("inotify_add_watch=%d\n", probe(SYS_inotify_add_watch, ino, BAD, 2 /*IN_MODIFY*/, 0, 0));
    // xattr family: the path (arg0) and the attribute name (arg1) are C strings the handler resolves/copies
    // before any host syscall. A wild pointer to either must surface the kernel's EFAULT, not fault the
    // engine's own path walker / name snprintf (which killed the guest with sig=11 before the guard). A real
    // backing file makes the name-pointer probe reach the copy step. The value buffer (arg2) is checked by
    // the host set/get, so its bad-pointer case is already EFAULT and is covered by setxattr_badval.
    int xf = open("/tmp/.efault_xattr_probe", O_CREAT | O_RDWR, 0644);
    (void)xf;
    printf("setxattr_badpath=%d\n", probe(SYS_setxattr, BAD, (long)"user.x", (long)"v", 1, 0));
    printf("setxattr_badname=%d\n", probe(SYS_setxattr, (long)"/tmp/.efault_xattr_probe", BAD, (long)"v", 1, 0));
    printf("setxattr_badval=%d\n",
           probe(SYS_setxattr, (long)"/tmp/.efault_xattr_probe", (long)"user.x", BAD, 4, 0));
    printf("getxattr_badname=%d\n",
           probe(SYS_getxattr, (long)"/tmp/.efault_xattr_probe", BAD, (long)"v", 4, 0));
    printf("removexattr_badname=%d\n", probe(SYS_removexattr, (long)"/tmp/.efault_xattr_probe", BAD, 0, 0, 0));
    // sendmsg/recvmsg with a NULL or wild msghdr pointer: on a real socket fd the kernel reaches the
    // copy_from_user of the msghdr and returns EFAULT. A handler that dereferences the msghdr struct
    // (iov ptr/count, control, flags) before validating it faults the engine (sig=11) instead. Uses a
    // datagram socket so the fd passes sockfd_lookup and the EFAULT is on the msghdr, not ENOTSOCK/EBADF.
    int sk = socket(AF_INET, SOCK_DGRAM, 0);
    printf("sendmsg_nullhdr=%d\n", probe(SYS_sendmsg, sk, 0, 0, 0, 0));
    printf("sendmsg_badhdr=%d\n", probe(SYS_sendmsg, sk, BAD, 0, 0, 0));
    printf("recvmsg_nullhdr=%d\n", probe(SYS_recvmsg, sk, 0, 0, 0, 0));
    printf("recvmsg_badhdr=%d\n", probe(SYS_recvmsg, sk, BAD, 0, 0, 0));
    // sendmsg/sendmmsg with a VALID msghdr but a WILD msg_name pointer. The msghdr struct itself is readable,
    // so the engine passes its up-front msghdr guard and reaches the DNS/icmp destination classifiers and the
    // sockaddr Linux<->host translator -- all of which deref msg_name directly. A wild msg_name must surface
    // EFAULT for the (sub)message, not fault the engine (sig=11). A valid 1-byte iov keeps the fault on the
    // name, not the payload. (Regression: sendmmsg's DNS pre-scan derefed submessage-0 msg_name unguarded.)
    char payload = 'x';
    struct iovec iov = {.iov_base = &payload, .iov_len = 1};
    struct msghdr smsg;
    memset(&smsg, 0, sizeof smsg);
    smsg.msg_name = (void *)BAD;
    smsg.msg_namelen = 16;
    smsg.msg_iov = &iov;
    smsg.msg_iovlen = 1;
    printf("sendmsg_badname=%d\n", probe(SYS_sendmsg, sk, (long)&smsg, 0, 0, 0));
    struct mmsghdr mmsg;
    memset(&mmsg, 0, sizeof mmsg);
    mmsg.msg_hdr = smsg;
    printf("sendmmsg_badname=%d\n", probe(SYS_sendmmsg, sk, (long)&mmsg, 1, 0, 0));
    // rt-signal family with a wild siginfo/sigset/timeout pointer: these handlers are serviced IN the engine
    // (they read the sigset/siginfo/timeout struct through the guest pointer directly), so a wild pointer
    // must surface the kernel's copy_{from,to}_user EFAULT rather than faulting the engine (sig=11). The
    // kernel copies the struct in/out before it does anything else, so a bad pointer EFAULTs regardless of
    // the tgid/sig args. A real sigset backs the timeout probe so the fault is on the timeout, not the set.
    sigset_t rtset;
    sigemptyset(&rtset);
    sigaddset(&rtset, SIGRTMIN);
    long self = getpid();
    printf("rt_sigqueueinfo_badinfo=%d\n", probe(SYS_rt_sigqueueinfo, self, SIGRTMIN, BAD, 0, 0));
    printf("rt_tgsigqueueinfo_badinfo=%d\n", probe(SYS_rt_tgsigqueueinfo, self, self, SIGRTMIN, BAD, 0));
    printf("rt_sigtimedwait_badset=%d\n", probe(SYS_rt_sigtimedwait, BAD, 0, 0, 8, 0));
    printf("rt_sigtimedwait_badtimeout=%d\n", probe(SYS_rt_sigtimedwait, (long)&rtset, 0, BAD, 8, 0));
    // wait4 with a wild status pointer: the kernel reaps the zombie, THEN faults on the put_user of the
    // status word (a re-wait returns ECHILD -- the child is already gone), so the syscall returns EFAULT.
    // A handler that writes the status through the guest pointer without validating it faults the engine
    // (sig=11) instead. Needs a real reapable child, so this probe forks a grandchild rather than using
    // the shared helper (whose child has none to wait on -> ECHILD, not EFAULT).
    {
        pid_t w = fork();
        if (w == 0) {
            pid_t g = fork();
            if (g == 0) _exit(3);
            long r = syscall(SYS_wait4, g, BAD, 0, 0, 0);
            _exit(r == -1 ? (errno & 0x7f) : 0);
        }
        int st = 0;
        waitpid(w, &st, 0);
        printf("wait4_badstatus=%d\n", WIFSIGNALED(st) ? -WTERMSIG(st) : WEXITSTATUS(st));
    }
    // get_mempolicy(mode*, ...) with the mode int STRADDLING a page boundary into an unmapped page: the
    // kernel's put_user of the full int returns EFAULT. A handler that validates only the pointer's FIRST
    // page (not the whole int write) faults the engine (sig=11) instead -- the exact gap getcpu already
    // guards against. Two mapped pages, the second unmapped, the int placed 2 bytes before the hole.
    {
        pid_t p = fork();
        if (p == 0) {
            long pg = sysconf(_SC_PAGESIZE);
            char *m = mmap(NULL, 2 * pg, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if (m == MAP_FAILED) _exit(99);
            munmap(m + pg, pg);
            long r = syscall(SYS_get_mempolicy, m + pg - 2, 0, 0, 0, 0);
            _exit(r == -1 ? (errno & 0x7f) : 0);
        }
        int st = 0;
        waitpid(p, &st, 0);
        printf("get_mempolicy_straddle=%d\n", WIFSIGNALED(st) ? -WTERMSIG(st) : WEXITSTATUS(st));
    }
    // Page-straddle probes: a fixed-size output object placed a few bytes before a PROT_NONE guard page.
    // Each must EFAULT(14) -- a single-page-validated handler would instead fault the engine (sig=11) or
    // silently write into the guard page (=0). getresuid/getresgid write three uid_t; the time/resource
    // copyout syscalls write a 16..struct-sized object; uname/sysinfo write a large struct. `off` keeps the
    // start of the object in the mapped page so the check must span the object to see the guard.
    printf("straddle_getresuid=%d\n", straddle(SYS_getresuid, 2, 0, 0, 0, 0, 0, 0));
    printf("straddle_getresgid=%d\n", straddle(SYS_getresgid, 2, 0, 0, 0, 0, 0, 0));
    printf("straddle_gettimeofday=%d\n", straddle(SYS_gettimeofday, 8, 0, 0, 0, 0, 0, 0));
    printf("straddle_clock_gettime=%d\n", straddle(SYS_clock_gettime, 8, 1, 0, 0, 0, 0, 0));
    printf("straddle_clock_getres=%d\n", straddle(SYS_clock_getres, 8, 1, 0, 0, 0, 0, 0));
    printf("straddle_times=%d\n", straddle(SYS_times, 8, 0, 0, 0, 0, 0, 0));
    printf("straddle_getrusage=%d\n", straddle(SYS_getrusage, 8, 1, 0, 0, 0, 0, 0));
    printf("straddle_uname=%d\n", straddle(SYS_uname, 8, 0, 0, 0, 0, 0, 0));
    printf("straddle_sysinfo=%d\n", straddle(SYS_sysinfo, 8, 0, 0, 0, 0, 0, 0));
    printf("straddle_prlimit64=%d\n", straddle(SYS_prlimit64, 8, 3, 0, 7 /*RLIMIT_NOFILE*/, 0, 0, 0));
    printf("straddle_rt_sigpending=%d\n", straddle(SYS_rt_sigpending, 4, 0, 0, 8, 0, 0, 0));
    return 0;
}
