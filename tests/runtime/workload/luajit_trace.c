#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

enum { CODE_WORDS = 64 };

static uint32_t branch(uint32_t opcode, unsigned from, unsigned to) {
    int32_t displacement = (int32_t)to - (int32_t)from;
    return opcode | ((uint32_t)displacement & 0x03ffffffu);
}

static uint32_t conditional(uint32_t condition, unsigned from, unsigned to) {
    int32_t displacement = (int32_t)to - (int32_t)from;
    return 0x54000000u | (((uint32_t)displacement & 0x7ffffu) << 5) | condition;
}

static void emit_trace(uint32_t *code) {
    enum { LOOP = 5, DONE = 14, FAIL = 17, HELPER = 21, HANDLER = 36, STUB0 = 40, STUB3 = 43 };

    code[0] = 0xaa1e03f9u;                         /* mov x25, x30 */
    code[1] = 0x5280003bu;                         /* mov w27, #1 */
    code[2] = 0x5280001cu;                         /* mov w28, #0 */
    code[3] = 0x52800c93u;                         /* mov w19, #100 */
    code[4] = 0xd503201fu;                         /* nop */
    code[LOOP + 0] = 0x528000e1u;                  /* mov w1, #7 */
    code[LOOP + 1] = 0xaa1c03fau;                  /* mov x26, x28 */
    code[LOOP + 2] = 0xaa1b03e0u;                  /* mov x0, x27 */
    code[LOOP + 3] = branch(0x94000000u, LOOP + 3, HELPER);
    code[LOOP + 4] = 0x2b1a001cu;                  /* adds w28, w0, w26 */
    code[LOOP + 5] = conditional(6, LOOP + 5, FAIL); /* b.vs fail */
    code[LOOP + 6] = 0x1100077bu;                  /* add w27, w27, #1 */
    code[LOOP + 7] = 0x6b13037fu;                  /* cmp w27, w19 */
    code[LOOP + 8] = conditional(13, LOOP + 8, LOOP); /* b.le loop */
    code[DONE] = branch(0x14000000u, DONE, STUB3); /* b exit stub 3 */
    code[FAIL] = branch(0x14000000u, FAIL, STUB0); /* b exit stub 0 */

    /* LuaJIT's integer modulo helper, copied from its generated VM assembly. */
    static const uint32_t helper[] = {
        0x4a010003u, 0x7100007fu, 0x4a807c02u, 0x4a817c23u, 0x4b807c42u,
        0x4b817c63u, 0x1ac30840u, 0x1b038800u, 0x7a404804u, 0x4b030002u,
        0x1a820000u, 0x4a010002u, 0x7100005fu, 0x5a805400u, 0xd65f03c0u,
    };
    for (unsigned i = 0; i < sizeof helper / sizeof helper[0]; i++) code[HELPER + i] = helper[i];

    code[HANDLER + 0] = 0x2a1c03e0u;               /* mov w0, w28 */
    code[HANDLER + 1] = 0xaa1903feu;               /* mov x30, x25 */
    code[HANDLER + 2] = 0xd65f03c0u;               /* ret */
    code[HANDLER + 3] = 0xd503201fu;               /* nop */
    for (unsigned stub = STUB0; stub <= STUB3; stub++)
        code[stub] = branch(0x94000000u, stub, HANDLER); /* packed LuaJIT-style exit stubs */
}

static int run_trace(uint32_t *code) {
    int result;
    __asm__ volatile("blr %1\n\tmov %w0, w0"
                     : "=r"(result)
                     : "r"(code)
                     : "cc", "memory", "x0", "x1", "x2", "x3", "x4", "x19", "x25", "x26", "x27",
                       "x28", "x30");
    return result;
}

int main(void) {
    const size_t size = (size_t)sysconf(_SC_PAGESIZE);
    unsigned tested = 0;
    for (unsigned i = 0; i < 256; i++) {
        uintptr_t candidate = 0x100000000ull + (uintptr_t)i * 0x200000ull;
        uint32_t *code = mmap((void *)candidate, size, PROT_READ | PROT_WRITE | PROT_EXEC,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (code == MAP_FAILED) continue;
        emit_trace(code);
        __builtin___clear_cache((char *)code, (char *)code + CODE_WORDS * sizeof(*code));
        int result = run_trace(code);
        munmap(code, size);
        tested++;
        if (result != 297) {
            printf("luajit trace fail address=%#llx result=%d\n",
                   (unsigned long long)candidate, result);
            return 1;
        }
    }
    if (!tested) return 2;
    printf("luajit trace ok sum=297\n");
    return 0;
}
