/*
 * A read that would move zero bytes never reaches copy_to_user, so an unusable destination buffer is
 * invisible to it: EOF returns 0 and a non-blocking source with nothing ready returns EAGAIN.  When the
 * read WOULD have copied, EFAULT must not consume the pending data or move the file position.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <termios.h>
#include <unistd.h>

static void show(const char *what, long rc) {
    printf("%s rc=%ld errno=%d\n", what, rc, rc < 0 ? errno : 0);
}

static struct iovec bad_vector = {.iov_base = NULL, .iov_len = 1};

/* Seekable source: EFAULT must leave both the position and the byte behind it untouched. */
static int regular_file(void) {
    char path[] = "/tmp/read_eof_badbufXXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 1;
    unlink(path);
    if (write(fd, "abcdefgh", 8) != 8) return 1;
    if (lseek(fd, 3, SEEK_SET) != 3) return 1;
    errno = 0;
    show("regular_midfile_nullbuf", read(fd, NULL, 1));
    printf("regular_midfile_pos_after pos=%ld\n", (long)lseek(fd, 0, SEEK_CUR));
    char byte = 0;
    errno = 0;
    show("regular_midfile_next_read", read(fd, &byte, 1));
    printf("regular_midfile_next_byte byte=%c\n", byte ? byte : '?');
    errno = 0;
    show("regular_midfile_badvec", readv(fd, &bad_vector, 1));

    if (lseek(fd, 0, SEEK_END) < 0) return 1;
    errno = 0;
    show("regular_eof_nullbuf", read(fd, NULL, 1));
    errno = 0;
    show("regular_eof_badvec", readv(fd, &bad_vector, 1));
    errno = 0;
    show("regular_eof_nullbuf_pread", pread(fd, NULL, 1, 8));
    close(fd);
    return 0;
}

static int pipes(void) {
    int ends[2];
    if (pipe(ends)) return 1;
    if (write(ends[1], "Z", 1) != 1) return 1;
    errno = 0;
    show("pipe_data_nullbuf", read(ends[0], NULL, 1));
    char byte = 0;
    errno = 0;
    show("pipe_data_next_read", read(ends[0], &byte, 1));
    printf("pipe_data_next_byte byte=%c\n", byte ? byte : '?');
    close(ends[0]);
    close(ends[1]);

    /* Writer gone and nothing buffered: EOF, so the bad buffer is never consulted. */
    if (pipe(ends)) return 1;
    close(ends[1]);
    errno = 0;
    show("pipe_eof_nullbuf", read(ends[0], NULL, 1));
    errno = 0;
    show("pipe_eof_badvec", readv(ends[0], &bad_vector, 1));
    errno = 0;
    show("pipe_eof_nullvec", readv(ends[0], NULL, 1));
    errno = 0;
    show("pipe_eof_nullbuf_pread", pread(ends[0], NULL, 1, 0));
    errno = 0;
    show("pipe_eof_badvec_preadv2", preadv2(ends[0], &bad_vector, 1, -1, 0));
    close(ends[0]);

    /* Writer gone but data still buffered: not EOF yet. */
    if (pipe(ends)) return 1;
    if (write(ends[1], "Y", 1) != 1) return 1;
    close(ends[1]);
    errno = 0;
    show("pipe_closed_data_nullbuf", read(ends[0], NULL, 1));
    close(ends[0]);

    /* Live writer, nothing pending: not EOF -- a non-blocking read reports the would-block instead. */
    if (pipe2(ends, O_NONBLOCK)) return 1;
    errno = 0;
    show("pipe_live_empty_nullbuf", read(ends[0], NULL, 1));
    errno = 0;
    show("pipe_live_empty_badvec", readv(ends[0], &bad_vector, 1));
    close(ends[0]);
    close(ends[1]);

    if (pipe2(ends, O_NONBLOCK)) return 1;
    close(ends[1]);
    errno = 0;
    show("pipe_eof_nonblock_nullbuf", read(ends[0], NULL, 1));
    close(ends[0]);
    return 0;
}

static int socketpairs(void) {
    int ends[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, ends)) return 1;
    if (write(ends[1], "Z", 1) != 1) return 1;
    errno = 0;
    show("sock_data_nullbuf", read(ends[0], NULL, 1));
    char byte = 0;
    errno = 0;
    show("sock_data_next_read", read(ends[0], &byte, 1));
    printf("sock_data_next_byte byte=%c\n", byte ? byte : '?');
    close(ends[0]);
    close(ends[1]);

    if (socketpair(AF_UNIX, SOCK_STREAM, 0, ends)) return 1;
    close(ends[1]);
    errno = 0;
    show("sock_eof_nullbuf", read(ends[0], NULL, 1));
    errno = 0;
    show("sock_eof_badvec", readv(ends[0], &bad_vector, 1));
    /* Non-seekable, so the descriptor loses to the buffer for a positional read: ESPIPE. */
    errno = 0;
    show("sock_eof_badvec_preadv", preadv(ends[0], &bad_vector, 1, 0));
    close(ends[0]);

    /* Peer shut its write side down: EOF without the peer's descriptor going away. */
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, ends)) return 1;
    if (shutdown(ends[1], SHUT_WR)) return 1;
    errno = 0;
    show("sock_shutwr_nullbuf", read(ends[0], NULL, 1));
    close(ends[0]);
    close(ends[1]);

    if (socketpair(AF_UNIX, SOCK_STREAM, 0, ends)) return 1;
    if (fcntl(ends[0], F_SETFL, O_NONBLOCK)) return 1;
    errno = 0;
    show("sock_live_empty_nullbuf", read(ends[0], NULL, 1));
    close(ends[0]);
    close(ends[1]);
    return 0;
}

static int terminals(void) {
    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) return 1;
    if (grantpt(master) || unlockpt(master)) return 1;
    const char *name = ptsname(master);
    if (!name) return 1;
    int slave = open(name, O_RDWR | O_NOCTTY | O_NONBLOCK);
    if (slave < 0) return 1;

    errno = 0;
    show("tty_slave_live_empty_nullbuf", read(slave, NULL, 1));
    errno = 0;
    show("tty_slave_live_empty_badvec", readv(slave, &bad_vector, 1));

    /* A complete line is queued, so the read would copy: EFAULT. */
    if (write(master, "\n", 1) != 1) return 1;
    usleep(50000);
    errno = 0;
    show("tty_slave_data_nullbuf", read(slave, NULL, 1));

    if (fcntl(master, F_SETFL, O_NONBLOCK)) return 1;
    close(slave);
    usleep(50000);
    /* The echoed line is still queued on the master, so its hangup does not make it EOF. */
    errno = 0;
    show("tty_master_slave_gone_nullbuf", read(master, NULL, 1));
    close(master);

    /* A pty slave whose master is gone reads as EOF (and FIONREAD on it fails outright). */
    master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) return 1;
    if (grantpt(master) || unlockpt(master)) return 1;
    name = ptsname(master);
    if (!name) return 1;
    slave = open(name, O_RDWR | O_NOCTTY | O_NONBLOCK);
    if (slave < 0) return 1;
    close(master);
    usleep(50000);
    errno = 0;
    show("tty_slave_master_gone_nullbuf", read(slave, NULL, 1));
    errno = 0;
    show("tty_slave_master_gone_badvec", readv(slave, &bad_vector, 1));
    close(slave);
    return 0;
}

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (regular_file()) return 1;
    if (pipes()) return 1;
    if (socketpairs()) return 1;
    if (terminals()) return 1;
    return 0;
}
