// sysfault.c — generic slow-path syscall ARGUMENT pointer validation (#395, the kernel's access_ok()
// contract). A bad/unmapped guest pointer handed to a syscall whose RESULT the engine writes/reads via a
// struct-fill (memcpy/memset), NOT via a host syscall, must surface as -EFAULT to the guest — never fault
// the engine, never wrongly succeed. This is the slow (svc_*) dispatch path; the x86 vDSO fast clock path
// is covered separately by clockefault.c (#218). RAW syscalls (SYS_*) so we bypass any libc pointer check
// and hit hl's dispatch directly (x86 numbers are normalized to the aarch64 table inside hl). Diffed
// byte-exact vs the native oracle (.oracle()); before the fix these returned 0 (or crashed the engine).
//
//   nanosleep(bad,·)          -> EFAULT     getrusage(SELF,bad)   -> EFAULT
//   mincore(map,len,bad)      -> EFAULT     fstat(fd,bad)         -> EFAULT
//   newfstatat(AT,"/",bad,0)  -> EFAULT     rt_sigaction(sig,bad) -> EFAULT
//   read(fd,bad,n)/write      -> EFAULT     + a valid control call for each -> ok(0)
//
// The `straddle` pointer (last 8 bytes of a mapped 64KB chunk, tail in the munmap'd hole above it) proves
// the guard validates the WHOLE range, not just the first byte. It is probed IMMEDIATELY after the hole is
// carved, before any libc allocation can re-map the hole (64KB-granular, exactly like edge_efault.c), so
// the result is deterministic on a 4KB- or 16KB-page kernel. Every verdict must match a native Linux run.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#define CHUNK (64 * 1024)

// EFAULT verdict for a call the kernel FAILS: -1 with errno==EFAULT.
static const char *ef(long r) { return (r == -1 && errno == EFAULT) ? "EFAULT" : (r == -1 ? "err" : "ok"); }
// ok verdict for a call the kernel SUCCEEDS (>=0). read/write return a count; the struct calls return 0.
static const char *ok(long r) { return (r >= 0) ? "ok" : (errno == EFAULT ? "EFAULT" : "err"); }

int main(void) {
    long pg = sysconf(_SC_PAGESIZE);
    void *bad = (void *)0x0000123400000000ULL; // far, never mapped

    // ---- all libc allocations FIRST (mkstemp/malloc), so nothing re-maps the hole we carve below ----
    char path[] = "/tmp/sysfaultXXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 1;
    unlink(path);
    if (write(fd, "hello world!!", 13) != 13) return 1;
    lseek(fd, 0, SEEK_SET);

    // ---- carve [rw 64KB][hole 64KB]; probe the straddle IMMEDIATELY (no allocation in between) ----
    char *rw = mmap(NULL, 2 * CHUNK, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (rw == MAP_FAILED) return 1;
    if (munmap(rw + CHUNK, CHUNK) != 0) return 1;
    void *straddle = (void *)(rw + CHUNK - 8); // 8 mapped bytes, tail in the hole
    errno = 0; const char *ns_str = ef(syscall(SYS_nanosleep, straddle, (void *)0));
    errno = 0; const char *ru_str = ef(syscall(SYS_getrusage, RUSAGE_SELF, straddle));
    errno = 0; const char *fs_str = ef(syscall(SYS_fstat, fd, straddle));

    struct timespec req = {0, 1000000}; // 1ms — a real (short) sleep for the control call
    struct rusage ru;
    struct stat st;
    unsigned char vec;
    char rbuf[16];

    // ---- nanosleep(request, remain) : request read by the kernel ----
    errno = 0; const char *ns_bad = ef(syscall(SYS_nanosleep, bad, (void *)0));
    errno = 0; const char *ns_ok  = ok(syscall(SYS_nanosleep, &req, (void *)0));

    // ---- getrusage(who, usage) : usage written by the engine ----
    errno = 0; const char *ru_bad = ef(syscall(SYS_getrusage, RUSAGE_SELF, bad));
    errno = 0; const char *ru_ok  = ok(syscall(SYS_getrusage, RUSAGE_SELF, &ru));

    // ---- mincore(addr, len, vec) : vec written by the engine (rw's lower chunk is still mapped) ----
    errno = 0; const char *mc_bad = ef(syscall(SYS_mincore, rw, (size_t)pg, bad));
    errno = 0; const char *mc_ok  = ok(syscall(SYS_mincore, rw, (size_t)pg, &vec));

    // ---- fstat(fd, statbuf) : statbuf written by the engine ----
    errno = 0; const char *fs_bad = ef(syscall(SYS_fstat, fd, bad));
    errno = 0; const char *fs_ok  = ok(syscall(SYS_fstat, fd, &st));

    // ---- newfstatat(dirfd, path, statbuf, flags) : statbuf written by the engine ----
    errno = 0; const char *at_bad = ef(syscall(SYS_newfstatat, AT_FDCWD, "/", bad, 0));
    errno = 0; const char *at_ok  = ok(syscall(SYS_newfstatat, AT_FDCWD, "/", &st, 0));

    // ---- rt_sigaction(sig, act, oldact, sigsetsize) : act read / oldact written by the engine ----
    errno = 0; const char *sa_bad = ef(syscall(SYS_rt_sigaction, SIGUSR1, bad, (void *)0, 8));
    errno = 0; const char *sa_old = ef(syscall(SYS_rt_sigaction, SIGUSR1, (void *)0, bad, 8));
    errno = 0; const char *sa_ok  = ok(syscall(SYS_rt_sigaction, SIGUSR1, (void *)0, (void *)0, 8));

    // ---- read/write with a bad buffer : served by the host syscall, must still be EFAULT ----
    errno = 0; const char *rd_bad = ef(syscall(SYS_read, fd, bad, 16));
    errno = 0; const char *wr_bad = ef(syscall(SYS_write, fd, bad, 16));
    lseek(fd, 0, SEEK_SET);
    errno = 0; const char *rd_ok  = ok(syscall(SYS_read, fd, rbuf, 13));

    close(fd);
    printf("sysfault ns_str=%s ru_str=%s fs_str=%s ns_bad=%s ns_ok=%s ru_bad=%s ru_ok=%s "
           "mc_bad=%s mc_ok=%s fs_bad=%s fs_ok=%s at_bad=%s at_ok=%s "
           "sa_bad=%s sa_old=%s sa_ok=%s rd_bad=%s wr_bad=%s rd_ok=%s\n",
           ns_str, ru_str, fs_str, ns_bad, ns_ok, ru_bad, ru_ok, mc_bad, mc_ok, fs_bad, fs_ok,
           at_bad, at_ok, sa_bad, sa_old, sa_ok, rd_bad, wr_bad, rd_ok);
    return 0;
}
