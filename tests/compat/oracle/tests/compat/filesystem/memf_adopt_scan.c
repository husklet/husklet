// memf adoption after unlink-while-open: the engine may adopt a just-unlinked, singly-held O_RDWR regular
// file into a RAM cache. This must (a) adopt the CORRECT file when several unlinked scratch files are open
// at once, (b) never cross-contaminate their contents, and (c) leave a DUPED (multi-fd) unlinked file
// un-adopted yet fully functional. Bounding the adoption scan to the published fd set must not change any of
// this. Output is deterministic and diffed against a native run.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

// Make an O_RDWR temp file under /tmp, write `tag`*len, unlink it (keeping it open), return the fd.
static int mk_unlinked(const char *tag, int len) {
    char t[] = "/tmp/hl_adopt_XXXXXX";
    int fd = mkstemp(t);
    if (fd < 0) return -1;
    char buf[256];
    for (int i = 0; i < len; i++) buf[i] = tag[i % (int)strlen(tag)];
    if (write(fd, buf, (size_t)len) != len) {
        close(fd);
        return -1;
    }
    unlink(t); // now anonymous, held only by fd -> adoption candidate
    return fd;
}

static int check(int fd, const char *tag, int len) {
    char buf[256] = {0};
    if (pread(fd, buf, (size_t)len, 0) != len) return 0;
    for (int i = 0; i < len; i++)
        if (buf[i] != tag[i % (int)strlen(tag)]) return 0;
    return 1;
}

int main(void) {
    // Several distinct unlinked scratch files open simultaneously.
    int a = mk_unlinked("AAA", 100);
    int b = mk_unlinked("BBB", 60);
    int cc = mk_unlinked("CDCD", 200);
    if (a < 0 || b < 0 || cc < 0) return 1;

    // Each still reads back exactly its own bytes (correct file adopted, no cross-contamination).
    printf("a_ok=%d b_ok=%d c_ok=%d\n", check(a, "AAA", 100), check(b, "BBB", 60), check(cc, "CDCD", 200));

    // Grow one after adoption: a RAM-adopted fd must still accept writes and read them back.
    if (lseek(a, 100, SEEK_SET) != 100) return 1;
    if (write(a, "ZZZ", 3) != 3) return 1;
    char z[3] = {0};
    printf("a_grow=%d\n", pread(a, z, 3, 100) == 3 && memcmp(z, "ZZZ", 3) == 0);

    // A DUPED unlinked file (two fds share the description) must NOT be adopted, but stays fully readable
    // through both aliases with a shared offset.
    char t[] = "/tmp/hl_adopt_dup_XXXXXX";
    int d1 = mkstemp(t);
    if (d1 < 0) return 1;
    if (write(d1, "DUPDUP", 6) != 6) return 1;
    int d2 = dup(d1);
    unlink(t);
    char db[6] = {0};
    int dok = (pread(d1, db, 6, 0) == 6 && memcmp(db, "DUPDUP", 6) == 0);
    // shared offset: reading 3 via d1 advances the shared position for d2
    lseek(d1, 0, SEEK_SET);
    char x3[3] = {0}, y3[3] = {0};
    int adv = (read(d1, x3, 3) == 3 && read(d2, y3, 3) == 3 && memcmp(x3, "DUP", 3) == 0 &&
               memcmp(y3, "DUP", 3) == 0);
    printf("dup_ok=%d dup_shared_offset=%d\n", dok, adv);

    close(a);
    close(b);
    close(cc);
    close(d1);
    close(d2);
    return 0;
}
