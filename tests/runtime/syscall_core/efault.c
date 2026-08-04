// edge_efault.c — syscall guest-pointer validation (the kernel's access_ok contract).
//
// The engine validates guest-supplied syscall buffers with host_range_mapped() before dereferencing
// them (os/linux/thread.c); a bad pointer must surface as -EFAULT to the guest, never fault the
// engine. This is the differential test for the fault-guarded-probe fast path (perf lever #5): every
// verdict below must match a native Linux run byte-for-byte (.oracle()). fcntl(F_SETLK/F_GETLK) is
// used as the probe surface because it validates a fixed 32-byte struct flock through exactly that
// helper (io.c case 25).
//
//   unmapped  flock* in a fully munmap'd hole            -> EFAULT
//   again     the SAME call repeated                     -> EFAULT (the probe guard must re-arm:
//             regression for the SIGSEGV re-unblock after the guard's siglongjmp)
//   straddle  flock* whose first 8 bytes are mapped but whose tail crosses into the hole -> EFAULT
//   null      flock* == NULL                             -> EFAULT
//   valid     a good pointer still takes the lock        -> 0
//   getlk     F_GETLK on our own lock reports F_UNLCK    -> 0 (own locks never conflict)
//
// Mappings are 64KB-granular and released with full-region munmap so the result is identical under
// a 16KB-host-page kernel (no sub-host-page munmap edge in play — that is #286's surface, not this
// test's). A PROT_NONE case is deliberately ABSENT: hl maps guest anon memory R+W and no-ops guest
// mprotect by design (see mem.c case 222/226 and the xfail'd edge/mprotect case), so guest-PROT_NONE
// pages are readable under hl — a divergence owned by that lane, not by the pointer-validation probe
// (which agrees with its mach_vm_region predecessor there: both report "mapped").
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHUNK (64 * 1024)

int main(void) {
    char path[] = "/tmp/efaultXXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) {
        perror("mkstemp");
        return 1;
    }
    unlink(path);
    if (write(fd, "x", 1) != 1) return 1;

    // [rw 64KB][hole 64KB] — reserve 128KB, drop the upper half wholesale.
    char *rw = mmap(NULL, 2 * CHUNK, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (rw == MAP_FAILED) return 1;
    if (munmap(rw + CHUNK, CHUNK) != 0) return 1;
    struct flock *hole = (struct flock *)(rw + CHUNK);          // fully unmapped
    struct flock *straddle = (struct flock *)(rw + CHUNK - 8);  // 8 mapped bytes, tail in the hole
    struct flock ok;

    int r, e;
    r = fcntl(fd, F_SETLK, hole);
    e = errno;
    int unmapped = (r == -1 && e == EFAULT);
    r = fcntl(fd, F_SETLK, hole);
    e = errno;
    int again = (r == -1 && e == EFAULT);
    r = fcntl(fd, F_SETLK, straddle);
    e = errno;
    int strad = (r == -1 && e == EFAULT);
    r = fcntl(fd, F_SETLK, (struct flock *)NULL);
    e = errno;
    int nullp = (r == -1 && e == EFAULT);

    memset(&ok, 0, sizeof ok);
    ok.l_type = F_WRLCK;
    ok.l_whence = SEEK_SET;
    ok.l_start = 0;
    ok.l_len = 1;
    int valid = (fcntl(fd, F_SETLK, &ok) == 0);
    memset(&ok, 0, sizeof ok);
    ok.l_type = F_WRLCK;
    ok.l_whence = SEEK_SET;
    ok.l_start = 0;
    ok.l_len = 1;
    int getlk = (fcntl(fd, F_GETLK, &ok) == 0 && ok.l_type == F_UNLCK);

    printf("efault unmapped=%d again=%d straddle=%d null=%d valid=%d getlk=%d\n",
           unmapped, again, strad, nullp, valid, getlk);
    return 0;
}
