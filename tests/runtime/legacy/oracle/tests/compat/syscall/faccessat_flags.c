// syscall-compat coverage: faccessat/faccessat2 and access. access on a readable file with R_OK -> 0;
// F_OK on a missing path -> ENOENT; X_OK on a non-executable regular file -> EACCES (unprivileged);
// faccessat2 with AT_EMPTY_PATH on a dirfd checks the directory itself. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <unistd.h>
#ifndef __NR_faccessat2
#define __NR_faccessat2 439
#endif
#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif

int main(void) {
    char dir[] = "/tmp/faccess_XXXXXX";
    mkdtemp(dir);
    char file[256], miss[256];
    snprintf(file, sizeof(file), "%s/f", dir);
    snprintf(miss, sizeof(miss), "%s/absent", dir);
    int fd = open(file, O_CREAT | O_WRONLY, 0644); // 0644: readable, not executable
    close(fd);

    printf("read_ok=%d\n", access(file, R_OK) == 0);
    printf("missing_errno=%d\n", access(miss, F_OK) == -1 ? errno : 0);
    printf("noexec_errno=%d\n", access(file, X_OK) == -1 ? errno : 0);
    // faccessat2 AT_EMPTY_PATH against an O_PATH dir fd checks the directory (readable) -> 0.
    int dfd = open(dir, O_PATH);
    long r = syscall(__NR_faccessat2, dfd, "", R_OK, AT_EMPTY_PATH);
    printf("faccessat2_empty_ok=%d\n", r == 0);

    unlink(file); rmdir(dir);
    return 0;
}
