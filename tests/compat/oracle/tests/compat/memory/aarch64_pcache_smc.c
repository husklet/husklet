#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

#if !defined(__aarch64__)
int main(void) {
    return 0;
}
#else

/*
 * hl_patchable() must consist of exactly the two instructions below because
 * main() overwrites its first word in place. GCC accepts but ignores
 * __attribute__((naked)) on AArch64, so a C function can acquire a generated
 * prologue under frame-pointer builds. A top-level assembly symbol makes the
 * patch point invariant across toolchains and compiler flags.
 */
extern unsigned hl_patchable(void);
__asm__(".pushsection .text\n"
        ".balign 16\n"
        ".globl hl_patchable\n"
        ".type hl_patchable,%function\n"
        "hl_patchable:\n"
        "mov w0,#11\n"
        "ret\n"
        ".size hl_patchable,.-hl_patchable\n"
        ".popsection\n");

#define PATCHABLE_ENTRY_WORD 0x52800160u /* mov w0,#11 */
#define PATCHABLE_PATCH_WORD 0x528002c0u /* mov w0,#22 */

static unsigned call_patchable(void) {
    unsigned (*volatile function)(void) = hl_patchable;
    return function();
}

int main(int argc, char **argv, char **environment) {
    unsigned before = 0;
    for (unsigned i = 0; i < 256; i++) before = call_patchable();

    if (argc == 1) {
        /*
         * execve makes the engine save this translated image, then reload the
         * same binary into the same persistent-cache key.  The second image
         * therefore begins with hl_patchable() restored from pcache v9.
         */
        char *child_argv[] = {argv[0], (char *)"warm", NULL};
        execve("/proc/self/exe", child_argv, environment);
        return 2;
    }

    long queried = sysconf(_SC_PAGESIZE);
    size_t page_size = queried > 0 ? (size_t)queried : 4096u;
    uintptr_t page = (uintptr_t)hl_patchable & ~(uintptr_t)(page_size - 1);
    if (mprotect((void *)page, page_size,
                 PROT_READ | PROT_WRITE | PROT_EXEC) != 0)
        return 3;
    uint32_t *instruction = (uint32_t *)(uintptr_t)hl_patchable;
    if (instruction[0] != PATCHABLE_ENTRY_WORD) {
        printf("pcache-smc unexpected entry word=%08x\n", instruction[0]);
        return 4;
    }
    instruction[0] = PATCHABLE_PATCH_WORD;
    __builtin___clear_cache((char *)instruction, (char *)(instruction + 1));
    unsigned after = call_patchable();
    printf("pcache-smc before=%u after=%u\n", before, after);
    return before == 11 && after == 22 ? 0 : 1;
}

#endif
