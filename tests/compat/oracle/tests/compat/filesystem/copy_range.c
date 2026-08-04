// copy_file_range(2): kernel-side copy between two files with an explicit offset.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_copyrange_%d", (int)getpid());
    mkdir(dir, 0755);
    char src[192], dst[192];
    snprintf(src, sizeof src, "%s/src", dir);
    snprintf(dst, sizeof dst, "%s/dst", dir);

    int sfd = open(src, O_CREAT | O_RDWR | O_TRUNC, 0644);
    const char payload[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    size_t n = sizeof payload - 1;
    write(sfd, payload, n);

    int dfd = open(dst, O_CREAT | O_RDWR | O_TRUNC, 0644);
    off_t in_off = 10;   // start copying at 'K'
    off_t out_off = 0;
    ssize_t copied = copy_file_range(sfd, &in_off, dfd, &out_off, 16, 0);
    int count_ok = copied == 16;
    int offsets_ok = in_off == 26 && out_off == 16;

    char buf[32] = {0};
    pread(dfd, buf, 16, 0);
    int content_ok = memcmp(buf, payload + 10, 16) == 0;
    struct stat st;
    fstat(dfd, &st);
    int size_ok = st.st_size == 16;

    close(sfd);
    close(dfd);
    unlink(src);
    unlink(dst);
    rmdir(dir);
    printf("copy-range count=%d offsets=%d content=%d size=%d bytes=%.16s\n",
           count_ok, offsets_ok, content_ok, size_ok, buf);
    return 0;
}
