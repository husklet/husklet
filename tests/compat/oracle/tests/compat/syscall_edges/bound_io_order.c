/*
 * Ordering contract for a file reached through a bound volume (the matrix runner maps the guest's /tmp to
 * one), which the engine services through a typed provider rather than a host descriptor.  That route
 * bounces the guest buffer, so the bounce must not be able to fail ahead of the checks Linux performs
 * first: the descriptor's access mode (EBADF, tested beside fdget_pos) and the source itself (a read at
 * EOF moves nothing and never reaches copy_to_user).  /dev/shm is deliberately covered too -- it is not a
 * volume, so it pins the same answers on the ordinary host-descriptor route.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>

static void show(const char *what, long rc) {
    printf("%s rc=%ld errno=%d\n", what, rc, rc < 0 ? errno : 0);
}

static struct iovec bad_vector = {.iov_base = NULL, .iov_len = 1};
static char label[160];
static const char *tag_prefix = "";

static const char *tagged(const char *name) {
    snprintf(label, sizeof label, "%s.%s", tag_prefix, name);
    return label;
}

static int suite(const char *tag, const char *directory) {
    char path[256];
    tag_prefix = tag;
    snprintf(path, sizeof path, "%s/bound_io_order_%s", directory, tag);
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return 1;
    if (write(fd, "abcdefgh", 8) != 8) return 1;
    close(fd);

    fd = open(path, O_RDONLY);
    if (fd < 0) return 1;
    if (lseek(fd, 3, SEEK_SET) != 3) return 1;
    errno = 0;
    show(tagged("read_mid_nullbuf"), read(fd, NULL, 1));
    printf("%s pos=%ld\n", tagged("read_mid_pos"), (long)lseek(fd, 0, SEEK_CUR));
    char byte = 0;
    errno = 0;
    show(tagged("read_mid_next"), read(fd, &byte, 1));
    printf("%s byte=%c\n", tagged("read_mid_byte"), byte ? byte : '?');
    errno = 0;
    show(tagged("read_mid_badvec"), readv(fd, &bad_vector, 1));
    errno = 0;
    show(tagged("pread_mid_nullbuf"), pread(fd, NULL, 1, 0));

    if (lseek(fd, 0, SEEK_END) != 8) return 1;
    errno = 0;
    show(tagged("read_eof_nullbuf"), read(fd, NULL, 1));
    errno = 0;
    show(tagged("read_eof_badvec"), readv(fd, &bad_vector, 1));
    errno = 0;
    show(tagged("preadv2_eof_badvec"), preadv2(fd, &bad_vector, 1, -1, 0));
    errno = 0;
    show(tagged("pread_eof_nullbuf"), pread(fd, NULL, 1, 8));
    errno = 0;
    show(tagged("preadv_eof_badvec"), preadv(fd, &bad_vector, 1, 8));
    /* The iovec ARRAY is dereferenced by import_iovec, so this faults even at EOF. */
    errno = 0;
    show(tagged("read_eof_nullvec"), readv(fd, NULL, 1));

    /* Access mode outranks every buffer complaint. */
    errno = 0;
    show(tagged("rdonly_write_nullbuf"), write(fd, NULL, 1));
    errno = 0;
    show(tagged("rdonly_writev_badvec"), writev(fd, &bad_vector, 1));
    errno = 0;
    show(tagged("rdonly_writev_nullvec"), writev(fd, NULL, 1));
    errno = 0;
    show(tagged("rdonly_pwrite_nullbuf"), pwrite(fd, NULL, 1, 0));
    errno = 0;
    show(tagged("rdonly_pwritev2_badvec"), pwritev2(fd, &bad_vector, 1, 0, 0));
    close(fd);

    fd = open(path, O_WRONLY);
    if (fd < 0) return 1;
    errno = 0;
    show(tagged("wronly_read_nullbuf"), read(fd, NULL, 1));
    errno = 0;
    show(tagged("wronly_readv_badvec"), readv(fd, &bad_vector, 1));
    errno = 0;
    show(tagged("wronly_readv_nullvec"), readv(fd, NULL, 1));
    errno = 0;
    show(tagged("wronly_pread_nullbuf"), pread(fd, NULL, 1, 0));
    errno = 0;
    show(tagged("wronly_write_nullbuf"), write(fd, NULL, 1));
    close(fd);
    unlink(path);

    /* A directory is rejected by the file's own read handler, also before the buffer. */
    fd = open(directory, O_RDONLY | O_DIRECTORY);
    if (fd < 0) return 1;
    errno = 0;
    show(tagged("dir_read_nullbuf"), read(fd, NULL, 1));
    errno = 0;
    show(tagged("dir_read_badvec"), readv(fd, &bad_vector, 1));
    close(fd);
    return 0;
}

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (suite("volume", "/tmp")) return 1;
    if (suite("shm", "/dev/shm")) return 1;
    return 0;
}
