#define _GNU_SOURCE
// O_NONBLOCK pipe: writing fills the buffer until EAGAIN; draining then makes it writable again.
#include <unistd.h>
#include <fcntl.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
int main(void){
    int p[2];
    if (pipe2(p, O_NONBLOCK)) return 1;
    char buf[4096]; memset(buf, 'x', sizeof buf);
    long total = 0; ssize_t n;
    while ((n = write(p[1], buf, sizeof buf)) > 0) total += n;
    int e = errno;
    int full_eagain = (n == -1 && e == EAGAIN);
    char rb[4096]; ssize_t got = read(p[0], rb, sizeof rb);
    // after draining a block, at least one more write succeeds
    ssize_t again = write(p[1], buf, sizeof buf);
    printf("nonblock filled_multiple_of_4096=%ld full_eagain=%d\n", total % 4096, full_eagain);
    printf("nonblock drained=%zd rewritable=%d\n", got, again > 0);
    return 0;
}
