// EDGE: sparse-file seeking. Write a byte at offset 0 and at offset 1 MiB (leaving a hole), then use
// SEEK_DATA / SEEK_HOLE to discover the layout: SEEK_HOLE from 0 finds the hole start, SEEK_DATA from
// the middle finds the next data. macOS/HFS+APFS report different granularity or ENXIO. Diffed vs
// native -> oracle.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    const char *p = "/tmp/hl_sparse";
    int fd = open(p, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0 || pwrite(fd, "A", 1, 0) != 1 || pwrite(fd, "B", 1, 1 << 20) != 1) return 1;
    off_t end = lseek(fd, 0, SEEK_END);
    off_t hole = lseek(fd, 0, SEEK_HOLE);    // first hole at/after 0
    off_t data = lseek(fd, 4096, SEEK_DATA); // next data at/after 4K -> ~1 MiB
    int hole_ok = (hole > 0 && hole < (1 << 20)); // a hole exists before the 1 MiB mark
    int data_ok = (data >= (1 << 20) - 4096 && data <= (1 << 20)); // data resumes near 1 MiB
    int inside_hole_ok = lseek(fd, 8192, SEEK_HOLE) == 8192;
    int inside_data_ok = lseek(fd, 1 << 20, SEEK_DATA) == (1 << 20);
    int alias = dup(fd);
    off_t alias_data = lseek(alias, 4096, SEEK_DATA);
    int shared_offset_ok = alias_data >= (1 << 20) - 4096 && lseek(fd, 0, SEEK_CUR) == alias_data;
    errno = 0;
    off_t data_eof = lseek(fd, end, SEEK_DATA);
    int data_eof_ok = data_eof == -1;
    errno = 0;
    off_t hole_eof = lseek(fd, end, SEEK_HOLE);
    int hole_eof_ok = hole_eof == -1;
    errno = 0;
    // A negative offset must be REJECTED; the errno is not portable and asserting one made this a
    // host test rather than an engine test. Linux returns ENXIO (the SEEK_DATA/SEEK_HOLE range check
    // in generic_file_llseek_size fires before the EINVAL path): verified on ext4 and tmpfs with a
    // plain `cc` build, which the engine reproduces exactly. macOS/APFS returns EINVAL. Accept both.
    int negative_ok = lseek(fd, -1, SEEK_DATA) == -1 && (errno == EINVAL || errno == ENXIO);
    int truncate_ok = ftruncate(fd, 8192) == 0 && lseek(fd, 4096, SEEK_DATA) == -1 &&
                      lseek(fd, 4096, SEEK_HOLE) == 4096;
    int extend_ok = ftruncate(fd, 16384) == 0 && lseek(fd, 8192, SEEK_HOLE) == 8192;
    close(alias);
    close(fd);
    unlink(p);
    printf("lseekhole end=%ld hole=%d data=%d inside=%d/%d eof=%d/%d negative=%d shared=%d truncate=%d "
           "extend=%d\n",
           (long)end, hole_ok, data_ok, inside_hole_ok, inside_data_ok, data_eof_ok, hole_eof_ok, negative_ok,
           shared_offset_ok, truncate_ok, extend_ok);
    return 0;
}
