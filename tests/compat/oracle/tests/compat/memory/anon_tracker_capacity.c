#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

enum { MAPPINGS = 2304, LENGTH = 0x4000, STRIDE = 0x8000, TARGET = 2200 };

int main(void) {
    uintptr_t base = UINT64_C(0x500000000);
    for (unsigned index = 0; index < MAPPINGS; ++index) {
        unsigned char *address = (unsigned char *)(base + (uintptr_t)index * STRIDE);
        if (mmap(address, LENGTH, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) != address)
            return 2;
        address[0] = 0xa5;
    }

    int readable = 1;
    for (unsigned index = 0; index < MAPPINGS; ++index) {
        unsigned char *address = (unsigned char *)(base + (uintptr_t)index * STRIDE);
        if (address[0] != 0xa5) readable = 0;
    }

    unsigned char *target = (unsigned char *)(base + (uintptr_t)TARGET * STRIDE);
    if (madvise(target, LENGTH, MADV_WIPEONFORK) != 0) return 3;
    pid_t child = fork();
    if (child < 0) return 4;
    if (child == 0) _exit(target[0] == 0 ? 0 : 1);
    int status = 0;
    if (waitpid(child, &status, 0) != child) return 5;

    int wiped = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int unmapped = 1;
    for (unsigned index = 0; index < MAPPINGS; ++index) {
        void *address = (void *)(base + (uintptr_t)index * STRIDE);
        if (munmap(address, LENGTH) != 0) unmapped = 0;
    }
    printf("anon-tracker-capacity mappings=%u readable=%d wiped=%d unmapped=%d\n",
           MAPPINGS, readable, wiped, unmapped);
    return readable && wiped && unmapped ? 0 : 1;
}
