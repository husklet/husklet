// EFD_SEMAPHORE: each read decrements the counter by exactly 1 and returns 1, until it hits 0.
#include <sys/eventfd.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
int main(void){
    int fd = eventfd(0, EFD_SEMAPHORE | EFD_NONBLOCK);
    uint64_t add = 3;
    write(fd, &add, 8);
    uint64_t v; int reads = 0;
    while (read(fd, &v, 8) == 8) { reads++; }
    int e = errno;
    printf("semaphore reads=%d last_value=%lu drained_errno=%s\n",
           reads, (unsigned long)v, strerror(e));   // reads=3 last_value=1 EAGAIN
    return 0;
}
