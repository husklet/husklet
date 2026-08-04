// /proc/self/exe is the magic link to the running executable image. openjdk, python's sys.executable,
// busybox multicall and self-re-exec code all readlink it and then open(2) the result. The kernel
// guarantees: the link is an absolute path, and opening it yields the very ELF image we are running
// (first four bytes 0x7f 'E' 'L' 'F'). A synthesized link that points nowhere or to a non-openable
// path breaks self-re-exec. Derived + deterministic (ELF magic, absoluteness), oracle-neutral.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char path[4096];
    ssize_t n = readlink("/proc/self/exe", path, sizeof path - 1);
    int abs_ok = 0, elf_ok = 0;
    if (n > 0) {
        path[n] = 0;
        abs_ok = path[0] == '/';
        int fd = open(path, O_RDONLY);
        if (fd >= 0) {
            unsigned char m[4] = {0};
            elf_ok = read(fd, m, 4) == 4 && m[0] == 0x7f && m[1] == 'E' && m[2] == 'L' && m[3] == 'F';
            close(fd);
        }
    }
    int ok = abs_ok && elf_ok;
    printf("selfexe ok=%d\n", ok);
    return 0;
}
