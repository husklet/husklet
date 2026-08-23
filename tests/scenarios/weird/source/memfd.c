#define _GNU_SOURCE
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

int main() {
    int fd = memfd_create("dd", 0);
    if (fd < 0) {
        perror("memfd");
        return 1;
    }
    write(fd, "hl-memfd-ok", 11);
    char b[32] = {0};
    lseek(fd, 0, SEEK_SET);
    read(fd, b, 11);
    printf("%s\n", b);
    return 0;
}
