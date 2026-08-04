// copy_file_range(2): a same-file copy whose source and destination ranges overlap must fail with EINVAL
// rather than copying through the overlap and corrupting the file. Non-overlapping same-file copies and
// cross-file copies (with len clamped to the source remainder) advance their offsets normally.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>

int main(void) {
    char sp[64], dp[64];
    snprintf(sp, sizeof sp, "/tmp/hl_cfrov_s_%d", (int)getpid());
    snprintf(dp, sizeof dp, "/tmp/hl_cfrov_d_%d", (int)getpid());
    int fd = open(sp, O_RDWR | O_CREAT | O_TRUNC, 0644);
    write(fd, "ABCDEFGHIJ", 10);

    // Overlapping ranges [0,5) -> [3,8) in the same file: EINVAL.
    off_t in = 0, out = 3;
    errno = 0;
    ssize_t r = copy_file_range(fd, &in, fd, &out, 5, 0);
    printf("overlap rc=%zd errno=%d\n", r, r < 0 ? errno : 0);

    // Non-overlapping same-file [0,3) -> [7,10): offsets advance.
    off_t in2 = 0, out2 = 7;
    ssize_t r2 = copy_file_range(fd, &in2, fd, &out2, 3, 0);
    printf("nonoverlap rc=%zd in=%ld out=%ld\n", r2, (long)in2, (long)out2);

    // Cross-file with len > source remainder: clamps to the 2 available bytes.
    int dst = open(dp, O_RDWR | O_CREAT | O_TRUNC, 0644);
    off_t si = 8, di = 0;
    ssize_t r3 = copy_file_range(fd, &si, dst, &di, 1000, 0);
    char buf[16] = {0};
    ssize_t rd = pread(dst, buf, sizeof buf, 0);
    printf("clamp rc=%zd wrote=%zd data=[%.*s]\n", r3, rd, (int)(rd > 0 ? rd : 0), buf);

    close(fd);
    close(dst);
    unlink(sp);
    unlink(dp);
    return 0;
}
