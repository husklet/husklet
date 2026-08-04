// splice/tee semantics: tee duplicates pipe bytes without consuming the source; tee on an
// empty pipe whose write end is closed reports EOF (returns 0, not EAGAIN); splice moves bytes
// between pipes and consumes the source. Portable across the native kernel and the engine shim.
#define _GNU_SOURCE
#include <fcntl.h>
#include <unistd.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>

int main(void) {
    // 1) tee duplicates without consuming
    int a[2], b[2];
    if (pipe(a) || pipe(b)) { perror("pipe"); return 1; }
    if (write(a[1], "hello", 5) != 5) return 2;
    ssize_t t = tee(a[0], b[1], 64, 0);
    char sb[16], db[16];
    ssize_t sn = read(a[0], sb, sizeof sb);   // source still intact
    ssize_t dn = read(b[0], db, sizeof db);   // duplicate present
    sb[sn > 0 ? sn : 0] = 0; db[dn > 0 ? dn : 0] = 0;
    printf("tee dup=%zd src='%s' dst='%s'\n", t, sb, db);
    close(a[0]); close(a[1]); close(b[0]); close(b[1]);

    // 2) tee at EOF: empty pipe, write end closed -> returns 0
    int c[2], d[2];
    if (pipe(c) || pipe(d)) return 3;
    close(c[1]);
    errno = 0;
    ssize_t te = tee(c[0], d[1], 64, 0);
    printf("tee_eof ret=%zd errno=%d\n", te, te < 0 ? errno : 0);
    close(c[0]); close(d[0]); close(d[1]);

    // 3) splice consumes the source pipe
    int e[2], f[2];
    if (pipe(e) || pipe(f)) return 4;
    if (write(e[1], "world", 5) != 5) return 5;
    ssize_t s = splice(e[0], NULL, f[1], NULL, 64, 0);
    char fb[16]; ssize_t fn = read(f[0], fb, sizeof fb); fb[fn > 0 ? fn : 0] = 0;
    // source now empty: make it nonblocking and confirm no bytes remain
    fcntl(e[0], F_SETFL, O_NONBLOCK);
    char rb[16]; errno = 0; ssize_t rn = read(e[0], rb, sizeof rb);
    printf("splice moved=%zd dst='%s' src_drained=%d\n", s, fb, (rn < 0 && errno == EAGAIN));
    return 0;
}
