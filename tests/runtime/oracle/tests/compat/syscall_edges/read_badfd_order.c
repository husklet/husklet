#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/uio.h>
#include <unistd.h>

static void show(const char *what, long rc) {
    printf("%s rc=%ld errno=%d\n", what, rc, rc < 0 ? errno : 0);
}

int main(void) {
    errno = 0;
    show("read_badfd_nullbuf", read(-1, NULL, 1));
    errno = 0;
    show("write_badfd_nullbuf", write(-1, NULL, 1));
    errno = 0;
    show("pread_badfd_nullbuf", pread(-1, NULL, 1, 0));
    errno = 0;
    show("pwrite_badfd_nullbuf", pwrite(-1, NULL, 1, 0));
    errno = 0;
    show("readv_badfd_nullvec", readv(-1, NULL, 1));
    errno = 0;
    show("writev_badfd_nullvec", writev(-1, NULL, 1));
    errno = 0;
    show("read_hifd_nullbuf", read(9999, NULL, 1));

    int devnull = open("/dev/null", O_RDONLY);
    if (devnull < 0) return 1;
    errno = 0;
    show("read_eof_nullbuf", read(devnull, NULL, 1));
    errno = 0;
    show("read_zero_nullbuf", read(devnull, NULL, 0));
    close(devnull);

    int urandom = open("/dev/urandom", O_RDONLY);
    if (urandom < 0) return 1;
    errno = 0;
    show("read_goodfd_nullbuf", read(urandom, NULL, 1));
    // A live descriptor opened the wrong way round is EBADF too: the FMODE_READ/FMODE_WRITE test sits beside
    // fdget_pos, ahead of the buffer, so the access mode outranks a bad buffer as well.
    errno = 0;
    show("write_rdonly_nullbuf", write(urandom, NULL, 1));
    close(urandom);

    int sink = open("/dev/null", O_WRONLY);
    if (sink < 0) return 1;
    errno = 0;
    show("read_wronly_nullbuf", read(sink, NULL, 1));
    close(sink);
    return 0;
}
