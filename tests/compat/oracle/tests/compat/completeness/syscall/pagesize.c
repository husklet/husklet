#include "compat.h"

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/auxv.h>
#include <unistd.h>

int main(void) {
    struct {
        uint64_t type;
        uint64_t value;
    } entry;
    unsigned long pagesz = getauxval(AT_PAGESZ);
    unsigned long proc_pagesz = 0;
    long libc_pagesz = sysconf(_SC_PAGESIZE);
    int fd = open("/proc/self/auxv", O_RDONLY);

    if (fd < 0) return 2;
    while (read(fd, &entry, sizeof(entry)) == (ssize_t)sizeof(entry)) {
        if (entry.type == AT_PAGESZ) proc_pagesz = (unsigned long)entry.value;
        if (entry.type == AT_NULL) break;
    }
    close(fd);
    if (pagesz != 4096 || libc_pagesz != 4096 || proc_pagesz != 4096) return 3;
    printf("pagesize auxv=%lu libc=%ld proc=%lu\n", pagesz, libc_pagesz, proc_pagesz);
    return 0;
}
