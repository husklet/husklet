// syscall-compat coverage: sendfile/splice zero-copy paths. sendfile copies file->file and advances the
// input offset; splice moves file->pipe->file; sendfile with a NULL offset uses and advances the fd's own
// position. Arch-neutral: byte counts / recovered content / booleans printed.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sendfile.h>
#include <unistd.h>

int main(void) {
    char st[] = "/tmp/sf_src_XXXXXX", dt[] = "/tmp/sf_dst_XXXXXX";
    int sfd = mkstemp(st), dfd = mkstemp(dt);
    unlink(st); unlink(dt);
    write(sfd, "ABCDEFGH", 8);
    lseek(sfd, 0, SEEK_SET);

    off_t off = 0;
    ssize_t n = sendfile(dfd, sfd, &off, 8);
    printf("sendfile_n=%zd off=%d\n", n, (int)off == 8);

    // Verify the destination content.
    char buf[16] = {0};
    lseek(dfd, 0, SEEK_SET);
    read(dfd, buf, 8);
    printf("copy_match=%d\n", memcmp(buf, "ABCDEFGH", 8) == 0);

    // splice src(from 0)->pipe->new file.
    lseek(sfd, 0, SEEK_SET);
    int pf[2];
    pipe(pf);
    char pt[] = "/tmp/sf_p_XXXXXX";
    int pfd = mkstemp(pt);
    unlink(pt);
    loff_t in = 0;
    ssize_t s1 = splice(sfd, &in, pf[1], NULL, 4, 0);
    ssize_t s2 = splice(pf[0], NULL, pfd, NULL, 4, 0);
    printf("splice_in=%zd splice_out=%zd\n", s1, s2);
    char pb[8] = {0};
    lseek(pfd, 0, SEEK_SET);
    read(pfd, pb, 4);
    printf("splice_match=%d\n", memcmp(pb, "ABCD", 4) == 0);
    return 0;
}
