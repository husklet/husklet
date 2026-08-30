#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

extern unsigned char fs_bridge_candidate[], fs_bridge_candidate_end[];

__asm__(".text\n"
        ".globl fs_bridge_candidate\n"
        ".type fs_bridge_candidate,@function\n"
        "fs_bridge_candidate:\n"
        " nop\n"
        " .byte 0x64,0x48,0x8b,0x04,0x25,0x40,0,0,0\n"
        " ret\n"
        ".globl fs_bridge_candidate_end\n"
        "fs_bridge_candidate_end:\n"
        ".size fs_bridge_candidate,fs_bridge_candidate_end-fs_bridge_candidate\n"
        ".globl call_with_fs\n"
        ".type call_with_fs,@function\n"
        "call_with_fs:\n"
        " sub $16,%rsp\n"
        " mov %rdi,8(%rsp)\n"
        " mov $0x1003,%edi; mov %rsp,%rsi; mov $158,%eax; syscall; test %rax,%rax; jne 1f\n"
        " mov $0x1002,%edi; mov 8(%rsp),%rsi; mov $158,%eax; syscall; test %rax,%rax; jne 1f\n"
        " call fs_bridge_candidate\n"
        " mov %rax,8(%rsp)\n"
        " mov $0x1002,%edi; mov (%rsp),%rsi; mov $158,%eax; syscall\n"
        " mov 8(%rsp),%rax; add $16,%rsp; ret\n"
        "1: mov $-1,%rax; add $16,%rsp; ret\n"
        ".size call_with_fs,.-call_with_fs\n");

extern uint64_t call_with_fs(void *tls);

int main(void) {
    size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    unsigned char *mapping = mmap(NULL, page_size, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED) return 2;
    *(uint64_t *)(mapping + 0x40) = UINT64_C(0x1122334455667788);
    *(uint64_t *)(mapping + 0x48) = UINT64_C(0x8877665544332211);
    uint64_t before = call_with_fs(mapping);
    uintptr_t page = (uintptr_t)fs_bridge_candidate & ~(page_size - 1);
    if (mprotect((void *)page, page_size, PROT_READ | PROT_WRITE | PROT_EXEC) != 0) return 3;
    if (fs_bridge_candidate_end - fs_bridge_candidate != 11 || fs_bridge_candidate[0] != 0x90 ||
        fs_bridge_candidate[1] != 0x64 || fs_bridge_candidate[6] != 0x40)
        return 4;
    fs_bridge_candidate[6] = 0x48;
    __builtin___clear_cache((char *)fs_bridge_candidate, (char *)fs_bridge_candidate_end);
    if (mprotect((void *)page, page_size, PROT_READ | PROT_EXEC) != 0) return 5;
    uint64_t after = call_with_fs(mapping);
    printf("fs load bridge smc=%016llx,%016llx\n", (unsigned long long)before,
           (unsigned long long)after);
    return before == UINT64_C(0x1122334455667788) && after == UINT64_C(0x8877665544332211) ? 0 : 6;
}
