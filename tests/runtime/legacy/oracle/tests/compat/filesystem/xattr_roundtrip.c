// Extended attributes: set/get/list/remove in the user namespace round-trip, XATTR_CREATE
// and XATTR_REPLACE enforce existence, and a missing attribute reports ENODATA. On a
// filesystem without xattr support ENOTSUP is an acceptable, recorded outcome.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/xattr.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_xattr_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    close(open(path, O_CREAT | O_WRONLY, 0644));

    int rc = setxattr(path, "user.one", "first", 5, 0);
    if (rc != 0 && errno == ENOTSUP) {
        unlink(path);
        rmdir(dir);
        printf("xattr-roundtrip supported=0\n");
        return 0;
    }

    int set_ok = rc == 0;
    setxattr(path, "user.two", "second-value", 12, 0);

    char buf[32] = {0};
    ssize_t g = getxattr(path, "user.one", buf, sizeof buf);
    int get_ok = g == 5 && memcmp(buf, "first", 5) == 0;

    // XATTR_CREATE on an existing name -> EEXIST.
    errno = 0;
    int create_dup = setxattr(path, "user.one", "x", 1, XATTR_CREATE) != 0 && errno == EEXIST;
    // XATTR_REPLACE on a missing name -> ENODATA.
    errno = 0;
    int replace_missing = setxattr(path, "user.absent", "x", 1, XATTR_REPLACE) != 0 && errno == ENODATA;

    // listxattr contains both user names.
    char list[256] = {0};
    ssize_t l = listxattr(path, list, sizeof list);
    int has_one = 0, has_two = 0;
    for (ssize_t i = 0; i < l; i += (ssize_t)strlen(list + i) + 1) {
        if (strcmp(list + i, "user.one") == 0) has_one = 1;
        if (strcmp(list + i, "user.two") == 0) has_two = 1;
    }
    int list_ok = has_one && has_two;

    // Remove and confirm ENODATA on read.
    int rm_ok = removexattr(path, "user.one") == 0;
    errno = 0;
    int gone = getxattr(path, "user.one", buf, sizeof buf) == -1 && errno == ENODATA;

    unlink(path);
    rmdir(dir);
    printf("xattr-roundtrip supported=1 set=%d get=%d create-dup=%d replace-missing=%d list=%d remove=%d gone=%d\n",
           set_ok, get_ok, create_dup, replace_missing, list_ok, rm_ok, gone);
    return 0;
}
