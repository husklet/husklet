// syscall-compat regression: open(2) flag matrix errno contract. O_EXCL on an existing file -> EEXIST;
// O_DIRECTORY on a regular file -> ENOTDIR; O_WRONLY on a directory -> EISDIR; O_NOFOLLOW on a symlink ->
// ELOOP; opening a missing path -> ENOENT; a path with a non-directory component -> ENOTDIR; O_PATH opens a
// symlink itself (no ELOOP with O_NOFOLLOW|O_PATH). Arch-neutral: errnos/booleans only.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(void) {
    char dir[] = "/tmp/openflags_XXXXXX";
    mkdtemp(dir);
    char file[256], link[256], missing[256], through[256];
    snprintf(file, sizeof(file), "%s/f", dir);
    snprintf(link, sizeof(link), "%s/l", dir);
    snprintf(missing, sizeof(missing), "%s/nope", dir);
    snprintf(through, sizeof(through), "%s/f/x", dir);
    int fd = open(file, O_CREAT | O_WRONLY, 0644);
    close(fd);
    symlink(file, link);

    printf("excl_errno=%d\n", open(file, O_CREAT | O_EXCL | O_WRONLY, 0644) == -1 ? errno : 0);
    printf("odir_on_file_errno=%d\n", open(file, O_RDONLY | O_DIRECTORY) == -1 ? errno : 0);
    printf("wronly_on_dir_errno=%d\n", open(dir, O_WRONLY) == -1 ? errno : 0);
    printf("nofollow_symlink_errno=%d\n", open(link, O_RDONLY | O_NOFOLLOW) == -1 ? errno : 0);
    printf("missing_errno=%d\n", open(missing, O_RDONLY) == -1 ? errno : 0);
    printf("notdir_component_errno=%d\n", open(through, O_RDONLY) == -1 ? errno : 0);
    // O_PATH|O_NOFOLLOW opens the symlink itself -> succeeds.
    int pf = open(link, O_PATH | O_NOFOLLOW);
    printf("opath_nofollow_ok=%d\n", pf >= 0);

    unlink(link); unlink(file); rmdir(dir);
    return 0;
}
