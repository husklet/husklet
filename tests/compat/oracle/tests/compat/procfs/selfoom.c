// The OOM-killer tunables oom_score, oom_score_adj and the legacy oom_adj are read/written by container
// runtimes (Docker's --oom-score-adj), systemd and kubelet. Assert their structural contract: oom_score is
// a non-negative integer, oom_score_adj parses as an integer in the kernel-valid [-1000,1000] range, and a
// value written to oom_score_adj reads back (the file is live, not a read-only stub). Derived + range-checked,
// oracle-neutral (absolute oom_score is host-variant, so only its shape is asserted).
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

int main(void) {
    char b[128];
    int score_ok = pf_read("/proc/self/oom_score", b, sizeof b) > 0;
    long score = score_ok ? atol(b) : -1;
    score_ok = score_ok && score >= 0;

    int adj_read = pf_read("/proc/self/oom_score_adj", b, sizeof b) > 0;
    long adj = adj_read ? atol(b) : 100000;
    int adj_ok = adj_read && adj >= -1000 && adj <= 1000;

    int wrote = 0;
    int same_fd = 0;
    int fd = open("/proc/self/oom_score_adj", O_RDWR);
    if (fd >= 0) {
        int peer = dup(fd);
        wrote = write(fd, "250\n", 4) == 4 && lseek(fd, 0, SEEK_SET) == 0 && write(fd, "-7\n", 3) == 3;
        if (wrote && peer >= 0 && lseek(fd, 0, SEEK_SET) == 0) {
            ssize_t n = read(peer, b, sizeof b - 1);
            if (n >= 0) {
                b[n] = 0;
                same_fd = strcmp(b, "-7\n") == 0;
            }
        }
        if (peer >= 0) close(peer);
        close(fd);
    }
    int rb_ok = 0;
    if (wrote && pf_read("/proc/self/oom_score_adj", b, sizeof b) > 0) rb_ok = atol(b) == -7;

    int ok = score_ok && adj_ok && wrote && same_fd && rb_ok;
    printf("selfoom ok=%d\n", ok);
    return 0;
}
