// Recursive directory removal exercises mkdirat/getdents/unlinkat/faccessat through a directory fd.
// A removed upper-layer directory must disappear immediately from the merged view.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    const char *root = "/tmp/hl-rm-tree";
    (void)unlinkat(AT_FDCWD, "/tmp/hl-rm-tree/a", AT_REMOVEDIR);
    (void)unlinkat(AT_FDCWD, root, AT_REMOVEDIR);
    int made_root = mkdir(root, 0755) == 0;
    int parent = open(root, O_RDONLY | O_DIRECTORY);
    int opened_parent = parent >= 0;
    int made_child = parent >= 0 && mkdirat(parent, "a", 0755) == 0;
    int child = parent < 0 ? -1 : openat(parent, "a", O_RDONLY | O_DIRECTORY);
    int child_errno = child < 0 ? errno : 0;
    int opened_child = child >= 0;
    int present = parent >= 0 && faccessat(parent, "a", F_OK, 0) == 0;
    char entries[1024];
    if (child >= 0) (void)syscall(SYS_getdents64, child, entries, sizeof entries);
    if (child >= 0) close(child);
    int removed = parent >= 0 && unlinkat(parent, "a", AT_REMOVEDIR) == 0;
    int remove_errno = removed ? 0 : errno;
    errno = 0;
    int gone = parent >= 0 && faccessat(parent, "a", F_OK, 0) < 0 && errno == ENOENT;
    if (parent >= 0) close(parent);
    (void)rmdir(root);
    printf("rm-tree root=%d parent=%d child=%d opened=%d errno=%d present=%d removed=%d remove-errno=%d gone=%d\n",
           made_root, opened_parent, made_child, opened_child, child_errno, present, removed, remove_errno, gone);
    return 0;
}
