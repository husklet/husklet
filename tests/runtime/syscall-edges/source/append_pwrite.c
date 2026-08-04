// syscall-compat regression: O_APPEND beats the explicit pwrite offset, plus three neighbouring
// argument-validation contracts the engine got wrong.
//
// 1. pwrite(2) on an O_APPEND descriptor APPENDS: Linux's generic_write_checks() moves ki_pos to i_size
//    for every write to an O_APPEND file, pwrite64 included, so the supplied offset is ignored. The
//    engine's RAM write-back cache honoured the offset and OVERWROTE the head of the file -- "AAAA"
//    followed by pwrite("B", offset 0) became "BAAA" instead of "AAAAB". Silent data corruption.
// 2. mremap(2) with old_size == 0 is EINVAL for a private mapping (it is only meaningful for a MAP_SHARED
//    source). The engine had no check and handed back a fresh anonymous mapping.
// 3. fallocate(2) with a mode bit outside FALLOC_FL_SUPPORTED_MASK is EOPNOTSUPP, not EINVAL; EINVAL is
//    reserved for the range/combination checks.
// 4. /proc/self/fd/N only exists while N is open, so opening the name of a closed descriptor is ENOENT.
//    The engine parsed the number and fell through to dup(), surfacing that dup's EBADF.
// Arch-neutral: errnos, byte counts and payload bytes only.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static int ec(long r) {
    return r == -1 ? errno : 0;
}

int main(void) {
    // --- pwrite on an O_APPEND fd appends, whatever offset is asked for.
    int fd = open("/tmp/hl-append-pwrite", O_RDWR | O_CREAT | O_TRUNC | O_APPEND, 0644);
    unlink("/tmp/hl-append-pwrite");
    if (write(fd, "AAAA", 4) != 4) return 1;
    long r = pwrite(fd, "B", 1, 0);
    char buf[32];
    memset(buf, 0, sizeof buf);
    long got = pread(fd, buf, sizeof buf - 1, 0);
    printf("append_pwrite_ret=%ld len=%ld data=%s\n", r, got, buf);
    // A second pwrite deep past EOF still appends rather than creating a hole.
    r = pwrite(fd, "C", 1, 4096);
    memset(buf, 0, sizeof buf);
    got = pread(fd, buf, sizeof buf - 1, 0);
    printf("append_pwrite2_ret=%ld len=%ld data=%s\n", r, got, buf);
    // Control: the same sequence on a NON-append fd honours the offset.
    int nfd = open("/tmp/hl-append-pwrite-n", O_RDWR | O_CREAT | O_TRUNC, 0644);
    unlink("/tmp/hl-append-pwrite-n");
    if (write(nfd, "AAAA", 4) != 4) return 1;
    r = pwrite(nfd, "B", 1, 0);
    memset(buf, 0, sizeof buf);
    got = pread(nfd, buf, sizeof buf - 1, 0);
    printf("noappend_pwrite_ret=%ld len=%ld data=%s\n", r, got, buf);

    // --- mremap old_size == 0 on a private mapping is EINVAL.
    void *m = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    printf("mremap_zero_oldsize_errno=%d\n",
           ec((long)syscall(SYS_mremap, m, (size_t)0, (size_t)8192, 1 /*MREMAP_MAYMOVE*/, 0)));
    // Control: a normal shrink still succeeds in place.
    printf("mremap_shrink_inplace=%d\n",
           (void *)syscall(SYS_mremap, m, (size_t)8192, (size_t)4096, 0, 0) == m);

    // --- fallocate with an unsupported mode bit is EOPNOTSUPP; a bad range is still EINVAL.
    printf("fallocate_badmode_errno=%d\n", ec(syscall(SYS_fallocate, nfd, 0x80, 0, 10)));
    printf("fallocate_stale_mode_errno=%d\n", ec(syscall(SYS_fallocate, nfd, 0x04, 0, 10)));
    printf("fallocate_zerolen_errno=%d\n", ec(syscall(SYS_fallocate, nfd, 0, 0, 0)));

    // --- /proc/self/fd/<closed> is ENOENT, not EBADF.
    int spare = dup(nfd);
    char path[64];
    snprintf(path, sizeof path, "/proc/self/fd/%d", spare);
    int reopened = open(path, O_RDONLY);
    printf("procfd_open_live=%d\n", reopened >= 0);
    if (reopened >= 0) close(reopened);
    close(spare);
    printf("procfd_open_closed_errno=%d\n", ec(open(path, O_RDONLY)));

    close(nfd);
    close(fd);
    return 0;
}
