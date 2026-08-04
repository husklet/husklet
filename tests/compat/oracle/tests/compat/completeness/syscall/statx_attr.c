/* statx result-mask and attribute semantics beyond the basic size probe. After statx(STATX_ALL) on a
   known regular file the kernel MUST report the core fields it filled in its stx_mask (TYPE, MODE,
   SIZE, NLINK, INO), the S_IFREG type bit, the exact size, and a stx_attributes value confined to the
   bits it advertised as supported in stx_attributes_mask. Derived booleans only, arch-neutral. */
#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <sys/stat.h>

int main(void) {
    const char *p = "/tmp/hlc_statx_attr";
    unlink(p);
    int fd = open(p, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    write(fd, "0123456789ABCDEF", 16);
    close(fd);

    struct statx stx;
    memset(&stx, 0xEE, sizeof stx);        /* poison to catch unfilled fields */
    long r = syscall(SYS_statx, AT_FDCWD, p, 0, STATX_ALL, &stx);

    int core_mask = (stx.stx_mask & STATX_TYPE) && (stx.stx_mask & STATX_MODE) &&
                    (stx.stx_mask & STATX_SIZE) && (stx.stx_mask & STATX_NLINK) &&
                    (stx.stx_mask & STATX_INO);
    int is_reg = (stx.stx_mode & S_IFMT) == S_IFREG;
    int size_ok = stx.stx_size == 16;
    /* every set attribute bit must be advertised as supported */
    int attr_confined = (stx.stx_attributes & ~stx.stx_attributes_mask) == 0;
    int blksize_ok = stx.stx_blksize > 0;

    printf("statx_attr ok=%d core_mask=%d isreg=%d size=%d attr_confined=%d blksize=%d\n",
           r == 0, core_mask, is_reg, size_ok, attr_confined, blksize_ok);
    unlink(p);
    return 0;
}
