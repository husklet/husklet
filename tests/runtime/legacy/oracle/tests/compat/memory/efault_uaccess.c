// memory-compat isolation: a syscall handed a pointer into an inaccessible region must return EFAULT --
// the kernel's copy_to_user / copy_from_user boundary -- rather than crash the engine or partially transfer.
//   - read() into a PROT_NONE destination buffer -> EFAULT (copy_to_user cannot write it)
//   - write() from an unmapped source buffer      -> EFAULT (copy_from_user cannot read it)
//   - read() into an unmapped high-VA destination -> EFAULT
// The transfer must not partially land and the engine must survive. Arch-neutral output.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);

    // read() into a PROT_NONE buffer
    int fds[2];
    if (pipe(fds) != 0) return 2;
    if (write(fds[1], "abcd", 4) != 4) return 3;
    unsigned char *none = mmap(NULL, ps, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (none == MAP_FAILED) return 4;
    ssize_t r1 = read(fds[0], none, 4);
    int read_none_efault = (r1 == -1 && errno == EFAULT);

    // write() from an unmapped source buffer (never-mapped high canonical VA)
    ssize_t r2 = write(fds[1], (const void *)0x123456789000ULL, 4);
    int write_unmapped_efault = (r2 == -1 && errno == EFAULT);

    // read() into an unmapped high-VA destination
    ssize_t r3 = read(fds[0], (void *)0x123456789000ULL, 4);
    int read_unmapped_efault = (r3 == -1 && errno == EFAULT);

    printf("read_none_efault=%d write_unmapped_efault=%d read_unmapped_efault=%d\n",
           read_none_efault, write_unmapped_efault, read_unmapped_efault);
    return 0;
}
