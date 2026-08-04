// memf adoption with MANY descriptors open at once. The engine adopts a just-unlinked, singly-held O_RDWR
// regular file into a RAM cache by locating the matching descriptor among the process's open fds. This
// exercises that location step at scale: dozens of distinct unlinked scratch files are held open together
// (pushing fd numbers well above the low range), and every one must read back EXACTLY its own bytes -- the
// correct file adopted, none cross-contaminated -- regardless of how the adoption scan is bounded. A run of
// padding fds first guarantees the scratch files land at high, non-contiguous descriptor numbers. Output is
// deterministic and diffed against a native run.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

enum { N = 48, PAD = 64 };

// Make an O_RDWR temp file under /tmp seeded with a per-index byte pattern, unlink it (keeping it open) so it
// becomes an anonymous adoption candidate, and return the fd. len is 32..287 bytes (index-dependent).
static int mk_unlinked(int idx, int *out_len) {
    char t[] = "/tmp/hl_manyfd_XXXXXX";
    int fd = mkstemp(t);
    if (fd < 0) return -1;
    int len = 32 + ((idx * 7) % 256);
    unsigned char buf[288];
    for (int i = 0; i < len; i++) buf[i] = (unsigned char)((idx * 31 + i * 3) & 0xff);
    if (write(fd, buf, (size_t)len) != len) {
        close(fd);
        return -1;
    }
    unlink(t);
    *out_len = len;
    return fd;
}

static int check(int fd, int idx, int len) {
    unsigned char buf[288] = {0};
    if (pread(fd, buf, (size_t)len, 0) != len) return 0;
    for (int i = 0; i < len; i++)
        if (buf[i] != (unsigned char)((idx * 31 + i * 3) & 0xff)) return 0;
    return 1;
}

int main(void) {
    // Padding fds push the scratch files to higher descriptor numbers so the scan must reach them.
    int pad[PAD];
    int npad = 0;
    for (int i = 0; i < PAD; i++) {
        int p = open("/dev/null", O_RDONLY);
        if (p < 0) break;
        pad[npad++] = p;
    }

    int fds[N];
    int lens[N];
    int made = 0;
    for (int i = 0; i < N; i++) {
        int len = 0;
        int fd = mk_unlinked(i, &len);
        if (fd < 0) break;
        fds[made] = fd;
        lens[made] = len;
        made++;
    }

    // Every held-open unlinked scratch file reads back exactly its own pattern.
    int all_ok = (made == N);
    for (int i = 0; i < made; i++)
        if (!check(fds[i], i, lens[i])) all_ok = 0;
    printf("made=%d all_ok=%d\n", made, all_ok);

    // Post-"adoption" mutation on a high-index file: overwrite then extend, read both back.
    int hi = made - 1;
    int mid_ok = 0, grow_ok = 0;
    if (hi >= 0) {
        if (pwrite(fds[hi], "QQQQ", 4, 8) == 4) {
            unsigned char m[4] = {0};
            mid_ok = (pread(fds[hi], m, 4, 8) == 4 && memcmp(m, "QQQQ", 4) == 0);
        }
        off_t end = lens[hi] + 100;
        if (pwrite(fds[hi], "TAILTAIL", 8, end) == 8) {
            unsigned char g[8] = {0};
            grow_ok = (pread(fds[hi], g, 8, end) == 8 && memcmp(g, "TAILTAIL", 8) == 0);
        }
    }
    printf("mid_ok=%d grow_ok=%d\n", mid_ok, grow_ok);

    for (int i = 0; i < made; i++) close(fds[i]);
    for (int i = 0; i < npad; i++) close(pad[i]);
    return 0;
}
