// openat2(2) with RESOLVE_* flags: symlink and beneath containment.
// Linux 5.6+ -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/openat2.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static int openat2_call(int dirfd, const char *path, struct open_how *how) {
    return (int)syscall(__NR_openat2, dirfd, path, how, sizeof *how);
}

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_openat2_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "file", O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(fd, "data", 4);
    close(fd);
    symlinkat("file", dfd, "sym");
    symlinkat("/etc/hostname", dfd, "abs");

    // Plain open through the symlink succeeds.
    struct open_how how = {.flags = O_RDONLY};
    int plain = openat2_call(dfd, "sym", &how);
    int plain_ok = plain >= 0;
    if (plain >= 0) close(plain);

    // RESOLVE_NO_SYMLINKS rejects the symlink with ELOOP.
    how = (struct open_how){.flags = O_RDONLY, .resolve = RESOLVE_NO_SYMLINKS};
    errno = 0;
    int nosym = openat2_call(dfd, "sym", &how);
    int nosym_eloop = nosym < 0 && errno == ELOOP;
    if (nosym >= 0) close(nosym);

    // RESOLVE_BENEATH rejects an absolute-target escape with EXDEV.
    how = (struct open_how){.flags = O_RDONLY, .resolve = RESOLVE_BENEATH};
    errno = 0;
    int beneath = openat2_call(dfd, "abs", &how);
    int beneath_blocked = beneath < 0 && (errno == EXDEV || errno == ELOOP);
    if (beneath >= 0) close(beneath);

    // RESOLVE_BENEATH also rejects a leading "/".
    errno = 0;
    int slash = openat2_call(dfd, "/etc/hostname", &how);
    int slash_blocked = slash < 0 && errno == EXDEV;
    if (slash >= 0) close(slash);

    unlinkat(dfd, "abs", 0);
    unlinkat(dfd, "sym", 0);
    unlinkat(dfd, "file", 0);
    close(dfd);
    rmdir(dir);
    printf("openat2-resolve plain=%d nosym-eloop=%d beneath-blocked=%d slash-blocked=%d\n",
           plain_ok, nosym_eloop, beneath_blocked, slash_blocked);
    return 0;
}
