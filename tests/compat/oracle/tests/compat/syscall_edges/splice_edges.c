// syscall-compat regression: the splice family validates its arguments before moving any bytes.
//
// Linux defines exactly four SPLICE_F_* bits (MOVE 1 / NONBLOCK 2 / MORE 4 / GIFT 8); any other bit is
// EINVAL. splice(2) needs at least one pipe endpoint (and refuses an offset for the pipe side with
// ESPIPE), tee(2) needs two pipes, and copy_file_range(2) defines no flags at all. sendfile(2) runs its
// count through rw_verify_area(), which rejects a count that is negative as ssize_t with EINVAL.
//
// The engine validated NONE of that: every bad-flag call silently returned 0 (or copied the bytes
// anyway), splice happily moved data between two regular files, and sendfile with SIZE_MAX looped to EOF
// and returned a 32-bit-truncated byte count. Separately, vmsplice's iovec was missing from the non-PIE
// pointer-rebase set, so on a static (non-PIE) guest EVERY vmsplice returned EFAULT -- the round-trip
// below is the regression pin for that.
// Arch-neutral: errnos, counts and payload bytes only.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static char payload[16] = "HELLO";

static int ec(long r) {
    return r == -1 ? errno : 0;
}

int main(void) {
    int fd = open("hl-splice-edges", O_RDWR | O_CREAT | O_TRUNC, 0644);
    unlink("hl-splice-edges");
    if (write(fd, "0123456789", 10) != 10) return 1;
    lseek(fd, 0, SEEK_SET);
    int fd2 = open("hl-splice-edges2", O_RDWR | O_CREAT | O_TRUNC, 0644);
    unlink("hl-splice-edges2");
    int p[2], q[2];
    if (pipe(p) || pipe(q)) return 1;

    // Undefined SPLICE_F_* bit -> EINVAL, and nothing moves.
    printf("splice_badflag_errno=%d\n", ec(syscall(SYS_splice, fd, (void *)0, p[1], (void *)0, 4, 0x99)));
    printf("tee_badflag_errno=%d\n", ec(syscall(SYS_tee, p[0], q[1], 4, 0x99)));
    printf("vmsplice_badflag_errno=%d\n", ec(syscall(SYS_vmsplice, p[1], (void *)0, 0, 0x99)));
    // copy_file_range defines no flags at all.
    printf("cfr_badflag_errno=%d\n", ec(syscall(SYS_copy_file_range, fd, (void *)0, fd2, (void *)0, 4, 1)));

    // splice needs a pipe end; regular->regular is EINVAL.
    printf("splice_bothregular_errno=%d\n", ec(syscall(SYS_splice, fd, (void *)0, fd2, (void *)0, 4, 0)));
    // tee needs BOTH ends to be pipes.
    printf("tee_notpipe_errno=%d\n", ec(syscall(SYS_tee, fd, p[1], 4, 0)));
    // An offset may only be given for the non-pipe side.
    {
        long long off = 0;
        printf("splice_pipe_offset_errno=%d\n", ec(syscall(SYS_splice, p[0], &off, fd2, (void *)0, 4, 0)));
    }

    // sendfile: a count that is negative as ssize_t is EINVAL (rw_verify_area).
    printf("sendfile_negcount_errno=%d\n", ec(syscall(SYS_sendfile, fd2, fd, (void *)0, (size_t)-1)));

    // Control: the valid forms still work end to end.
    lseek(fd, 0, SEEK_SET);
    long r = syscall(SYS_splice, fd, (void *)0, p[1], (void *)0, 5, 0);
    char buf[16];
    memset(buf, 0, sizeof buf);
    long got = read(p[0], buf, 5);
    printf("splice_ok=%ld read=%ld data=%s\n", r, got, buf);

    if (write(p[1], "xyz", 3) != 3) return 1;
    r = syscall(SYS_tee, p[0], q[1], 3, 0);
    memset(buf, 0, sizeof buf);
    got = read(q[0], buf, 3);
    printf("tee_ok=%ld read=%ld data=%s\n", r, got, buf);
    // tee does NOT consume the source.
    memset(buf, 0, sizeof buf);
    got = read(p[0], buf, 3);
    printf("tee_source_intact=%ld data=%s\n", got, buf);

    // vmsplice write end: gather user memory into the pipe, then read it back.
    struct iovec iv;
    iv.iov_base = payload;
    iv.iov_len = 5;
    r = syscall(SYS_vmsplice, q[1], &iv, 1, 0);
    memset(buf, 0, sizeof buf);
    got = read(q[0], buf, 5);
    printf("vmsplice_ok=%ld read=%ld data=%s\n", r, got, buf);
    return 0;
}
