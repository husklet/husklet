// syscall-compat regression: pathname resolution errno contract. A single component > NAME_MAX -> 
// ENAMETOOLONG; stat of a missing path -> ENOENT; readlink of a non-symlink ->
// EINVAL; mkdir of an existing dir -> EEXIST; rmdir of a non-empty dir -> ENOTEMPTY. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[] = "/tmp/patherr_XXXXXX";
    mkdtemp(dir);
    struct stat st;

    // 300-char component -> ENAMETOOLONG (NAME_MAX is 255).
    char longname[512];
    int n = snprintf(longname, sizeof(longname), "%s/", dir);
    memset(longname + n, 'a', 300);
    longname[n + 300] = 0;
    printf("nametoolong_errno=%d\n", stat(longname, &st) == -1 ? errno : 0);

    // Missing path -> ENOENT.
    char miss[256];
    snprintf(miss, sizeof(miss), "%s/absent", dir);
    printf("enoent_errno=%d\n", stat(miss, &st) == -1 ? errno : 0);

    // readlink on the directory (not a symlink) -> EINVAL.
    char buf[64];
    printf("readlink_nonlink_errno=%d\n", readlink(dir, buf, sizeof(buf)) == -1 ? errno : 0);

    // mkdir over existing dir -> EEXIST.
    printf("mkdir_exist_errno=%d\n", mkdir(dir, 0755) == -1 ? errno : 0);

    // rmdir of a non-empty dir -> ENOTEMPTY.
    char child[256];
    snprintf(child, sizeof(child), "%s/c", dir);
    mkdir(child, 0755);
    printf("rmdir_notempty_errno=%d\n", rmdir(dir) == -1 ? errno : 0);

    rmdir(child); rmdir(dir);
    return 0;
}
