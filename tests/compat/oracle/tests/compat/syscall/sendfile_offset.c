// sendfile(2) with a non-NULL offset pointer must NOT change the input fd's file position: it reads from
// *offset and advances *offset only. A NULL offset reads from (and advances) the input file position.
// count larger than the remaining file clamps at EOF.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/sendfile.h>

int main(void) {
    char path[64];
    snprintf(path, sizeof path, "/tmp/hl_sendfile_%d", (int)getpid());
    int in = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    write(in, "0123456789", 10);
    int p[2];
    pipe(p);

    // Explicit offset: input position (set to 3) stays 3; *off advances 2->6.
    lseek(in, 3, SEEK_SET);
    off_t off = 2;
    ssize_t n = sendfile(p[1], in, &off, 4);
    off_t inpos = lseek(in, 0, SEEK_CUR);
    char buf[16] = {0};
    read(p[0], buf, n > 0 ? n : 0);
    printf("sendfile n=%zd off=%ld inpos=%ld data=[%.*s]\n", n, (long)off, (long)inpos,
           (int)(n > 0 ? n : 0), buf);

    // count > remaining file: clamps to EOF.
    off_t off2 = 8;
    ssize_t n2 = sendfile(p[1], in, &off2, 100);
    char b2[16] = {0};
    read(p[0], b2, n2 > 0 ? n2 : 0);
    printf("sendfile clamp n=%zd off=%ld data=[%.*s]\n", n2, (long)off2, (int)(n2 > 0 ? n2 : 0), b2);

    // NULL offset: advances the input file position.
    lseek(in, 1, SEEK_SET);
    ssize_t n3 = sendfile(p[1], in, NULL, 3);
    off_t inpos3 = lseek(in, 0, SEEK_CUR);
    char b3[16] = {0};
    read(p[0], b3, n3 > 0 ? n3 : 0);
    printf("sendfile null n=%zd inpos=%ld data=[%.*s]\n", n3, (long)inpos3, (int)(n3 > 0 ? n3 : 0), b3);

    close(in);
    close(p[0]);
    close(p[1]);
    unlink(path);
    return 0;
}
