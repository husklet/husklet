#include "../src/arch/aarch64/broadcast.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "broadcast:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static uint32_t duplicate_element(unsigned q, unsigned scalar, unsigned size,
                                  unsigned lane, unsigned source, unsigned destination) {
    uint32_t immediate = (1u << size) | (lane << (size + 1));
    uint32_t base = scalar ? 0x5e000400u : 0x0e000400u | (q << 30);
    return base | (immediate << 16) | (source << 5) | destination;
}

static int accepted(uint32_t word) {
    uint8_t code[64];
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, sizeof(code)));
    return hl_a64_broadcast_body(&assembler, word);
}

int main(void) {
    for (unsigned size = 0; size < 4; size++) {
        unsigned lanes = 16u >> size;
        for (unsigned lane = 0; lane < lanes; lane++) {
            CHECK(accepted(duplicate_element(1, 0, size, lane, 30, 31)));
            CHECK(accepted(duplicate_element(0, 1, size, lane, 30, 31)));
            if (size < 3) CHECK(accepted(duplicate_element(0, 0, size, lane, 30, 31)));
        }
    }
    CHECK(!accepted(0x0e000400u));                 /* imm5 == 0 */
    CHECK(!accepted(duplicate_element(0, 0, 3, 0, 1, 0))); /* vector 1D */
    CHECK(!accepted(0x0e000c00u));                 /* general imm5 == 0 */
    CHECK(!accepted(0x0e008400u));                 /* unrelated copy opcode */
    CHECK(!accepted(duplicate_element(1, 0, 1, 0, 1, 0) | (1u << 29) | (1u << 11))); /* INS */
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        0x4e010c20u, /* dup v0.16b,w1 */
        0x4e010f80u, /* dup v0.16b,w28 */
        0x4e080c20u, /* dup v0.2d,x1 */
        0x0e0207dfu, /* dup v31.4h,v30.h[0] */
        duplicate_element(0, 0, 0, 15, 29, 2),
        duplicate_element(1, 0, 2, 3, 3, 4),
        duplicate_element(1, 0, 3, 1, 5, 6),
        duplicate_element(0, 1, 0, 9, 7, 8),
        duplicate_element(0, 1, 3, 1, 9, 10),
        duplicate_element(1, 0, 1, 2, 11, 11), /* source aliases destination */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_broadcast_emit(&assembler, words[index], 0x7000 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    hl_native_aarch64_cpu cpu = {0};
    cpu.registers[1] = 0xa5;
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.vectors[0] == UINT64_C(0xa5a5a5a5a5a5a5a5));
    CHECK(cpu.vectors[1] == UINT64_C(0xa5a5a5a5a5a5a5a5));
    cpu.registers[28] = 0x3c;
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.vectors[0] == UINT64_C(0x3c3c3c3c3c3c3c3c));
    CHECK(cpu.vectors[1] == UINT64_C(0x3c3c3c3c3c3c3c3c));
    cpu.registers[1] = UINT64_C(0x0123456789abcdef);
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.vectors[0] == cpu.registers[1]);
    CHECK(cpu.vectors[1] == cpu.registers[1]);
    for (unsigned reg = 0; reg < 31; reg++) cpu.registers[reg] = UINT64_C(0x1000) + reg;
    uint64_t registers[31];
    memcpy(registers, cpu.registers, sizeof(registers));
    cpu.flags = UINT64_C(0xa0000000);
    cpu.vectors[60] = UINT64_C(0x776655443322abcd);
    cpu.vectors[61] = UINT64_C(0xffeeddccbbaa9988);
    execute(&cpu, code + offsets[3]);
    CHECK(cpu.vectors[62] == UINT64_C(0xabcdabcdabcdabcd));
    CHECK(cpu.vectors[63] == 0);
    CHECK(cpu.flags == UINT64_C(0xa0000000));
    CHECK(memcmp(registers, cpu.registers, sizeof(registers)) == 0);
    cpu.vectors[58] = UINT64_C(0x0706050403020100);
    cpu.vectors[59] = UINT64_C(0x0f0e0d0c0b0a0908);
    execute(&cpu, code + offsets[4]);
    CHECK(cpu.vectors[4] == UINT64_C(0x0f0f0f0f0f0f0f0f));
    CHECK(cpu.vectors[5] == 0);
    cpu.vectors[6] = UINT64_C(0x3333333322222222);
    cpu.vectors[7] = UINT64_C(0x7777777766666666);
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.vectors[8] == UINT64_C(0x7777777777777777));
    CHECK(cpu.vectors[9] == UINT64_C(0x7777777777777777));
    cpu.vectors[10] = UINT64_C(0x1111111111111111);
    cpu.vectors[11] = UINT64_C(0xfeedfacecafebeef);
    execute(&cpu, code + offsets[6]);
    CHECK(cpu.vectors[12] == UINT64_C(0xfeedfacecafebeef));
    CHECK(cpu.vectors[13] == UINT64_C(0xfeedfacecafebeef));
    cpu.vectors[14] = UINT64_C(0x0908070605040302);
    cpu.vectors[15] = UINT64_C(0x11100f0e0d0c0b0a);
    execute(&cpu, code + offsets[7]);
    CHECK(cpu.vectors[16] == UINT64_C(0x0b));
    CHECK(cpu.vectors[17] == 0);
    cpu.vectors[18] = UINT64_C(0x0123456789abcdef);
    cpu.vectors[19] = UINT64_C(0xfedcba9876543210);
    execute(&cpu, code + offsets[8]);
    CHECK(cpu.vectors[20] == UINT64_C(0xfedcba9876543210));
    CHECK(cpu.vectors[21] == 0);
    cpu.vectors[22] = UINT64_C(0x3333222211110000);
    cpu.vectors[23] = UINT64_C(0x7777666655554444);
    execute(&cpu, code + offsets[9]);
    CHECK(cpu.vectors[22] == UINT64_C(0x2222222222222222));
    CHECK(cpu.vectors[23] == UINT64_C(0x2222222222222222));
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
