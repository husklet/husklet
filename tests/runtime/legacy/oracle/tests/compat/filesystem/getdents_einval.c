// getdents64(2): a result buffer too small to hold even the first pending entry must fail with
// EINVAL, NOT silently report end-of-directory (which truncates the listing to empty). Also checks
// that a large directory read with a small-but-sufficient buffer returns every entry exactly once
// across the multi-syscall continuation, and that a drained directory returns 0.
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

struct ld64 { unsigned long d_ino; long d_off; unsigned short d_reclen; unsigned char d_type; char d_name[]; };

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_gde_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    const int N = 300;
    for (int i = 0; i < N; i++) {
        char nm[32];
        snprintf(nm, sizeof nm, "f%04d", i);
        close(openat(dfd, nm, O_CREAT | O_WRONLY, 0644));
    }

    // 1) buffer too small for even one entry -> EINVAL
    int fd1 = open(dir, O_RDONLY | O_DIRECTORY);
    char tiny[8];
    long r1 = syscall(SYS_getdents64, fd1, tiny, sizeof tiny);
    int tiny_einval = (r1 < 0 && errno == EINVAL);
    close(fd1);

    // 2) small-but-sufficient buffer: every entry returned exactly once, no dup/drop/loop
    int fd2 = open(dir, O_RDONLY | O_DIRECTORY);
    char buf[512];
    static char seen[512];
    memset(seen, 0, sizeof seen);
    int total = 0, dup = 0;
    for (;;) {
        long r = syscall(SYS_getdents64, fd2, buf, sizeof buf);
        if (r <= 0) break;
        for (long off = 0; off < r;) {
            struct ld64 *e = (struct ld64 *)(buf + off);
            if (strcmp(e->d_name, ".") && strcmp(e->d_name, "..")) {
                total++;
                int idx = (e->d_name[0] == 'f') ? atoi(e->d_name + 1) : -1;
                if (idx >= 0 && idx < 512) { if (seen[idx]) dup++; seen[idx] = 1; }
            }
            if (e->d_reclen == 0) { off = r; break; }
            off += e->d_reclen;
        }
    }
    int missing = 0;
    for (int i = 0; i < N; i++) if (!seen[i]) missing++;
    // 3) after full drain, another read returns 0
    long r3 = syscall(SYS_getdents64, fd2, buf, sizeof buf);
    close(fd2);

    printf("getdents-einval tiny_einval=%d total=%d dup=%d missing=%d drained=%ld\n",
           tiny_einval, total, dup, missing, r3);

    for (int i = 0; i < N; i++) {
        char nm[32];
        snprintf(nm, sizeof nm, "f%04d", i);
        unlinkat(dfd, nm, 0);
    }
    close(dfd);
    rmdir(dir);
    return 0;
}
