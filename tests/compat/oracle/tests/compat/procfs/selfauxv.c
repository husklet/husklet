// /proc/self/auxv is the raw ELF auxiliary vector (array of type/value longs, terminated by AT_NULL).
// glibc, musl and Go read it for AT_PAGESZ, AT_HWCAP and AT_RANDOM at startup; a wrong or missing entry
// breaks libc bring-up. Assert the entries whose values are knowable/derivable in-process, not host
// magic: AT_PAGESZ equals sysconf(_SC_PAGESIZE); AT_PHENT (program-header entry size) is 56 on both
// LP64 ISAs; AT_PHDR, AT_ENTRY and AT_PAGESZ are all present; the vector terminates with AT_NULL.
#define _GNU_SOURCE
#include <elf.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    unsigned long buf[512];
    int fd = open("/proc/self/auxv", O_RDONLY);
    int len = 0, r;
    while (fd >= 0 && len < (int)sizeof buf && (r = (int)read(fd, (char *)buf + len, sizeof buf - len)) > 0)
        len += r;
    if (fd >= 0) close(fd);
    int npairs = len / (int)(2 * sizeof(unsigned long));
    unsigned long pagesz = 0, phent = 0;
    int has_phdr = 0, has_entry = 0, has_pagesz = 0, terminated = 0;
    for (int i = 0; i < npairs; i++) {
        unsigned long type = buf[2 * i], val = buf[2 * i + 1];
        switch (type) {
            case AT_PAGESZ: pagesz = val; has_pagesz = 1; break;
            case AT_PHENT:  phent = val; break;
            case AT_PHDR:   has_phdr = 1; break;
            case AT_ENTRY:  has_entry = 1; break;
            case AT_NULL:   terminated = 1; break;
        }
        if (type == AT_NULL) break;
    }
    int pagesz_ok = has_pagesz && pagesz == (unsigned long)sysconf(_SC_PAGESIZE);
    int phent_ok = phent == sizeof(Elf64_Phdr);
    int ok = pagesz_ok && phent_ok && has_phdr && has_entry && terminated;
    printf("selfauxv ok=%d\n", ok);
    return 0;
}
