#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 1;
    unsigned char *reserved =
        mmap(NULL, (size_t)page * 16, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reserved == MAP_FAILED) return 2;
    if (mprotect(reserved, (size_t)page, PROT_READ | PROT_WRITE) != 0) return 3;

    reserved[8] = 0x5a;
    printf("none_commit=%02x\n", reserved[8]);
    return munmap(reserved, (size_t)page * 16) == 0 ? 0 : 4;
}
