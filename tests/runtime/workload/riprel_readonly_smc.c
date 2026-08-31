#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

__attribute__((visibility("hidden"))) const volatile unsigned char riprel_target = 0x10;
extern unsigned char riprel_candidate[], riprel_candidate_end[];

__asm__(
    ".text\n"
    ".globl riprel_candidate\n"
    ".type riprel_candidate,@function\n"
    "riprel_candidate:\n"
    "cmpb $0x20, riprel_target(%rip)\n"
    "sete %al\n"
    "movzbl %al, %eax\n"
    "ret\n"
    ".globl riprel_candidate_end\n"
    "riprel_candidate_end:\n"
    ".size riprel_candidate, riprel_candidate_end-riprel_candidate\n");

static int call_candidate(void) {
    return ((int (*)(void))(uintptr_t)riprel_candidate)();
}

int main(void) {
    if ((size_t)(riprel_candidate_end - riprel_candidate) < 7 || riprel_candidate[0] != 0x80 ||
        riprel_candidate[1] != 0x3d || riprel_candidate[6] != 0x20)
        return 2;
    int before = call_candidate();
    size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    uintptr_t page = (uintptr_t)riprel_candidate & ~(page_size - 1);
    if (mprotect((void *)page, page_size, PROT_READ | PROT_WRITE | PROT_EXEC) != 0) return 3;
    riprel_candidate[6] = 0x10;
    __builtin___clear_cache((char *)riprel_candidate, (char *)riprel_candidate_end);
    if (mprotect((void *)page, page_size, PROT_READ | PROT_EXEC) != 0) return 4;
    int after = call_candidate();
    printf("riprel readonly smc=%d,%d\n", before, after);
    return before == 0 && after == 1 ? 0 : 5;
}
