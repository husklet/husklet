// sendfile(2): file-to-file copy with an in/out offset, plus NULL-offset streaming.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/sendfile.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_sendfile_%d", (int)getpid());
    mkdir(dir, 0755);
    char src[192], dst[192];
    snprintf(src, sizeof src, "%s/src", dir);
    snprintf(dst, sizeof dst, "%s/dst", dir);

    int sfd = open(src, O_CREAT | O_RDWR | O_TRUNC, 0644);
    const char payload[] = "sendfile-payload-0123456789";
    size_t n = sizeof payload - 1;
    write(sfd, payload, n);

    // Copy a middle slice with an explicit offset pointer.
    int dfd = open(dst, O_CREAT | O_RDWR | O_TRUNC, 0644);
    off_t off = 9;
    ssize_t sent = sendfile(dfd, sfd, &off, 8);
    int slice_ok = sent == 8 && off == 17;
    char buf[16] = {0};
    pread(dfd, buf, 8, 0);
    int slice_content = memcmp(buf, payload + 9, 8) == 0;

    // NULL offset streams from the source file position (rewound to 0).
    lseek(sfd, 0, SEEK_SET);
    int dfd2 = open(dst, O_RDWR | O_TRUNC);
    ssize_t all = sendfile(dfd2, sfd, NULL, n);
    int stream_ok = all == (ssize_t)n && lseek(sfd, 0, SEEK_CUR) == (off_t)n;
    char full[64] = {0};
    pread(dfd2, full, n, 0);
    int full_ok = memcmp(full, payload, n) == 0;

    close(sfd); close(dfd); close(dfd2);
    unlink(src); unlink(dst);
    rmdir(dir);
    printf("sendfile-probe slice=%d slice-content=%d stream=%d full-content=%d\n",
           slice_ok, slice_content, stream_ok, full_ok);
    return 0;
}
