/* getdents64 directory enumeration over /proc. A guest walking a directory must see "." and ".." and
   at least the self/thread-group entries; the record stream must be well-formed (each d_reclen
   advances within the buffer, d_name NUL-terminated). We print derived booleans (dot/dotdot present,
   found a numeric pid dir, records well-formed) — arch-neutral and host-independent. */
#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <stdint.h>

struct linux_dirent64 {
    uint64_t d_ino;
    int64_t  d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

int main(void) {
    int fd = open("/proc", O_RDONLY | O_DIRECTORY);
    if (fd < 0) { printf("open_ok=0\n"); return 0; }
    char buf[8192];
    int has_dot = 0, has_dotdot = 0, has_numeric = 0, wellformed = 1, count = 0;
    for (;;) {
        long n = syscall(SYS_getdents64, fd, buf, sizeof buf);
        if (n < 0) { wellformed = 0; break; }
        if (n == 0) break;
        for (long off = 0; off < n;) {
            struct linux_dirent64 *d = (struct linux_dirent64 *)(buf + off);
            if (d->d_reclen == 0 || off + d->d_reclen > n) { wellformed = 0; break; }
            count++;
            if (strcmp(d->d_name, ".") == 0) has_dot = 1;
            else if (strcmp(d->d_name, "..") == 0) has_dotdot = 1;
            else if (d->d_name[0] >= '1' && d->d_name[0] <= '9') has_numeric = 1;
            off += d->d_reclen;
        }
        if (!wellformed) break;
    }
    close(fd);
    printf("open_ok=1 dot=%d dotdot=%d has_pid=%d wellformed=%d count_positive=%d\n",
           has_dot, has_dotdot, has_numeric, wellformed, count > 0);
    return 0;
}
