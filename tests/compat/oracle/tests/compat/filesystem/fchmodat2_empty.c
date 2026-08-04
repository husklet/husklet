#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/xattr.h>
#include <unistd.h>

int main(void) {
    char path[] = "/tmp/hl-fchmodat2-empty.XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 2;
    if (fchmod(fd, 0644) != 0) {
        close(fd);
        unlink(path);
        return 3;
    }

    int changed = syscall(__NR_fchmodat2, fd, "", 0600, AT_EMPTY_PATH) == 0;
    struct stat status;
    int visible = fstat(fd, &status) == 0 && (status.st_mode & 07777) == 0600;
    const char metadata[] = "cp-mode";
    char copied[sizeof metadata] = {0};
    int xattr = fsetxattr(fd, "user.hl-copy-mode", metadata, sizeof metadata, 0) == 0 &&
                fgetxattr(fd, "user.hl-copy-mode", copied, sizeof copied) == (ssize_t)sizeof metadata &&
                !memcmp(copied, metadata, sizeof metadata);

    close(fd);
    unlink(path);
    printf("fchmodat2-empty changed=%d visible=%d fd-xattr=%d\n", changed, visible, xattr);
    return changed && visible && xattr ? 0 : 1;
}
