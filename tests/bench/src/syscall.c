#define _GNU_SOURCE

#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

/* A long dependency chain prevents the compiler from deleting or coalescing calls. */
int main(void) {
    uint64_t checksum = 0;
    unsigned long i;
    for (i = 0; i < 1000000UL; ++i)
        checksum += (uint64_t)syscall(SYS_gettid);
    printf("syscall calls=%lu nonzero=%d\n", i, checksum != 0);
    return checksum == 0;
}
