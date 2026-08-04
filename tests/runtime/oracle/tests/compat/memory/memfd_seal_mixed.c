// Guards the registry-empty fast path for the per-write memfd-seal probe: once ANY memfd exists (the
// shared registry is non-empty), F_SEAL_WRITE enforcement on write(2)/pwrite/writev must still fire, while
// writes to non-memfd fds (pipe, regular file) in the SAME process are unaffected. This is the exact
// invariant the "skip the fstat when no memfd has ever been created" optimization must preserve.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#define F_SEAL_WRITE 0x0008
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002U
#endif

extern int memfd_create(const char *, unsigned int);

int main(void) {
    // A sealed memfd -> registry is now non-empty for the rest of the process.
    int m = memfd_create("hl_mixed", MFD_ALLOW_SEALING);
    if (m < 0) return 1;
    if (write(m, "abcd", 4) != 4) return 1;
    int seal_ok = fcntl(m, F_ADD_SEALS, F_SEAL_WRITE) == 0;

    // write(2) to the sealed memfd -> EPERM
    ssize_t w = write(m, "z", 1);
    int w_eperm = (w < 0 && errno == EPERM);
    // pwrite to the sealed memfd -> EPERM
    ssize_t pw = pwrite(m, "z", 1, 0);
    int pw_eperm = (pw < 0 && errno == EPERM);
    // writev to the sealed memfd -> EPERM
    struct iovec iov = {.iov_base = "z", .iov_len = 1};
    ssize_t wv = writev(m, &iov, 1);
    int wv_eperm = (wv < 0 && errno == EPERM);

    // Non-memfd writes in the same process are unaffected.
    int p[2];
    if (pipe(p) != 0) return 1;
    int pipe_ok = (write(p[1], "hi", 2) == 2);
    char pb[2];
    int pipe_rd = (read(p[0], pb, 2) == 2 && pb[0] == 'h' && pb[1] == 'i');

    char tmpl[] = "/tmp/hl_mixed_XXXXXX";
    int f = mkstemp(tmpl);
    int file_ok = (f >= 0 && write(f, "file", 4) == 4);
    if (f >= 0) {
        unlink(tmpl);
        close(f);
    }

    // Sealed memfd content is intact (the refused writes did not mutate it).
    char buf[8] = {0};
    int rd = (pread(m, buf, 4, 0) == 4 && memcmp(buf, "abcd", 4) == 0);

    printf("mixed seal=%d w_eperm=%d pw_eperm=%d wv_eperm=%d pipe=%d pipe_rd=%d file=%d intact=%d\n", seal_ok,
           w_eperm, pw_eperm, wv_eperm, pipe_ok, pipe_rd, file_ok, rd);
    close(m);
    close(p[0]);
    close(p[1]);
    return 0;
}
