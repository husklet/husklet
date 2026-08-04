#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static const unsigned char elf_readonly_page[16384]
    __attribute__((aligned(16384), section(".elfwrite"))) = {[4096] = 0x31};

int main(void) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    uintptr_t byte = (uintptr_t)&elf_readonly_page[4096];
    uintptr_t first = byte & ~(uintptr_t)(page - 1);
    int protected = mprotect((void *)first, page, PROT_READ | PROT_WRITE) == 0;
    if (protected) *(volatile unsigned char *)byte = 0x72;
    unsigned value = *(volatile const unsigned char *)byte;
    printf("elf rodata write protect=%d value=%x\n", protected, value);
    return !(protected && value == 0x72);
}
