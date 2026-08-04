#define _GNU_SOURCE
#include <elf.h>
#include <stdio.h>
#include <sys/auxv.h>
#include <unistd.h>

int main(int argc, char **argv) {
    unsigned long page = getauxval(AT_PAGESZ);
    unsigned long entry = getauxval(AT_ENTRY);
    int stack_ok = argc == 1 && argv != NULL && argv[0] != NULL && argv[0][0] == '/';
    printf("x86-rootfs-smoke main=1 stack=%d auxv=%d\n", stack_ok, page != 0 && entry != 0);
    return stack_ok && page != 0 && entry != 0 ? 0 : 1;
}
