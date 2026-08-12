#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/fp_convert.h"
#include "../src/arch/aarch64/fp_move.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "fp:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        0x9e660081u, /* fmov x1,d4 */
        0x1e2603e0u, /* fmov w0,s31 */
        0x9e6603dcu, /* fmov x28,d30 */
        0x9e67001fu, /* fmov d31,x0 */
        0x1e2703c1u, /* fmov s1,w30 */
        0x9eaf0382u, /* fmov v2.d[1],x28 */
        0x9eae005eu, /* fmov x30,v2.d[1] */
        0x9e6703e3u, /* fmov d3,xzr */
    };
    const uint32_t conversions[] = {
        0x1e22003cu, /* scvtf s28,w1 */
        0x9e3903e5u, /* fcvtzu x5,s31 */
        0x1e220380u, /* scvtf s0,w28 */
        0x9e3903fcu, /* fcvtzu x28,s31 */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    size_t conversion_offsets[sizeof(conversions) / sizeof(conversions[0])];
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_fp_move_emit(&assembler, words[index], 0xc000 + index * 4));
    }
    for (size_t index = 0; index < sizeof(conversions) / sizeof(conversions[0]); index++) {
        conversion_offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_fp_convert_emit(&assembler, conversions[index], 0xc100 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xb0000000);
    cpu.tls = UINT64_C(0x123456789abcdef0);
    cpu.vectors[8] = UINT64_C(0x0102030405060708);
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.registers[1] == UINT64_C(0x0102030405060708));
    cpu.vectors[62] = UINT64_C(0xaabbccdddeadbeef);
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.registers[0] == UINT64_C(0xdeadbeef));
    cpu.vectors[60] = UINT64_C(0x8877665544332211);
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.registers[28] == UINT64_C(0x8877665544332211));
    cpu.registers[0] = UINT64_C(0x0f1e2d3c4b5a6978);
    cpu.vectors[63] = UINT64_MAX;
    execute(&cpu, code + offsets[3]);
    CHECK(cpu.vectors[62] == cpu.registers[0] && cpu.vectors[63] == 0);
    cpu.registers[30] = UINT64_C(0xaabbccdd89abcdef);
    cpu.vectors[3] = UINT64_MAX;
    execute(&cpu, code + offsets[4]);
    CHECK(cpu.vectors[2] == UINT64_C(0x89abcdef) && cpu.vectors[3] == 0);
    cpu.registers[28] = UINT64_C(0xfedcba9876543210);
    cpu.vectors[4] = UINT64_C(0x1122334455667788);
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.vectors[4] == UINT64_C(0x1122334455667788));
    CHECK(cpu.vectors[5] == UINT64_C(0xfedcba9876543210));
    execute(&cpu, code + offsets[6]);
    CHECK(cpu.registers[30] == UINT64_C(0xfedcba9876543210));
    cpu.vectors[6] = cpu.vectors[7] = UINT64_MAX;
    execute(&cpu, code + offsets[7]);
    CHECK(cpu.vectors[6] == 0 && cpu.vectors[7] == 0);
    cpu.registers[1] = 7;
    execute(&cpu, code + conversion_offsets[0]);
    CHECK((uint32_t)cpu.vectors[56] == UINT32_C(0x40e00000));
    cpu.vectors[62] = UINT32_C(0x40fccccd); /* 7.9f */
    execute(&cpu, code + conversion_offsets[1]);
    CHECK(cpu.registers[5] == 7);
    cpu.registers[28] = 9;
    execute(&cpu, code + conversion_offsets[2]);
    CHECK((uint32_t)cpu.vectors[0] == UINT32_C(0x41100000));
    cpu.vectors[62] = UINT32_C(0x41180000); /* 9.5f */
    execute(&cpu, code + conversion_offsets[3]);
    CHECK(cpu.registers[28] == 9);
    CHECK(cpu.flags == UINT64_C(0xb0000000));
    CHECK(cpu.tls == UINT64_C(0x123456789abcdef0));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_FP_MOVE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_fp_move_emit(&assembler, words[0], 0xd000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_fp_move_emit(&assembler, 0x9e260020u, 0xd000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_fp_move_emit(&assembler, 0x9e780020u, 0xd000));
    CHECK(!hl_a64_fp_convert_emit(&assembler, 0x1ee20020u, 0xd000));
    return 0;
#endif
}
