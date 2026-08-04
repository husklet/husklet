#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/pair.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/arch/aarch64/single.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "scalar-addressing:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    _Static_assert(sizeof(entry) == sizeof(address), "native code pointer size drifted");
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    const uint32_t words[] = {
        UINT32_C(0xa9bf04a0), /* stp x0,x1,[x5,#-16]! */
        UINT32_C(0xa8c118a5), /* ldp x5,x6,[x5],#16; writeback wins overlap */
        UINT32_C(0xad8104e0), /* stp q0,q1,[x7,#32]! */
        UINT32_C(0xacc10ce2), /* ldp q2,q3,[x7],#32 */
        UINT32_C(0xa9bf4650), /* stp x16,x17,[x18,#-16]! */
        UINT32_C(0xa8c14e52), /* ldp x18,x19,[x18],#16; stolen overlap */
    };
    const uint32_t register_words[] = {
        UINT32_C(0xf86658a0), /* ldr x0,[x5,w6,uxtw #3] */
        UINT32_C(0xf86678a1), /* ldr x1,[x5,x6,lsl #3] */
        UINT32_C(0xf866d8a2), /* ldr x2,[x5,w6,sxtw #3] */
        UINT32_C(0xf866f8a3), /* ldr x3,[x5,x6,sxtx #3] */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    size_t register_offsets[sizeof(register_words) / sizeof(register_words[0])];
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_pair_emit(&assembler, words[index], 0x6000 + index * 4));
    }
    for (size_t index = 0; index < sizeof(register_words) / sizeof(register_words[0]); index++) {
        register_offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_single_emit(&assembler, register_words[index], 0x6100 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    _Alignas(16) uint8_t memory[256] = {0};
    uint64_t first = (uint64_t)(uintptr_t)memory;
    uint64_t last = (uint64_t)(uintptr_t)(memory + sizeof(memory));
    hl_native_aarch64_cpu cpu = {0};
    cpu.memory_first = first;
    cpu.memory_last = last;
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.flags = UINT64_C(0xa0000000);

    cpu.registers[0] = UINT64_C(0x0011223344556677);
    cpu.registers[1] = UINT64_C(0x8899aabbccddeeff);
    cpu.registers[5] = first + 64;
    cpu.dirty_first = UINT64_MAX;
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x6004);
    CHECK(cpu.registers[5] == first + 48);
    CHECK(*(uint64_t *)(void *)(memory + 48) == cpu.registers[0]);
    CHECK(*(uint64_t *)(void *)(memory + 56) == cpu.registers[1]);
    CHECK(cpu.dirty_first == first + 48 && cpu.dirty_last == first + 64);
    CHECK(cpu.dirty_view_first == first && cpu.dirty_view_last == last);
    CHECK(cpu.flags == UINT64_C(0xa0000000));

    *(uint64_t *)(void *)(memory + 120) = UINT64_C(0x1201201201201201);
    *(uint64_t *)(void *)(memory + 136) = UINT64_C(0x1361361361361361);
    *(uint64_t *)(void *)(memory + 144) = UINT64_C(0x1441441441441441);
    cpu.registers[5] = first + 128;
    const uint64_t indexes[] = {1, 2, UINT32_MAX, UINT64_MAX - 1};
    const uint64_t expected[] = {
        UINT64_C(0x1361361361361361), UINT64_C(0x1441441441441441),
        UINT64_C(0x1201201201201201), UINT64_C(0x1201201201201201),
    };
    for (size_t index = 0; index < 4; index++) {
        cpu.registers[6] = indexes[index];
        execute(&cpu, code + register_offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x6104 + index * 4);
        CHECK(cpu.registers[index] == expected[index]);
        CHECK(cpu.flags == UINT64_C(0xa0000000));
    }

    *(uint64_t *)(void *)(memory + 80) = UINT64_C(0xdeadbeefdeadbeef);
    *(uint64_t *)(void *)(memory + 88) = UINT64_C(0x1020304050607080);
    cpu.registers[5] = first + 80;
    cpu.registers[6] = 0;
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x6008);
    CHECK(cpu.registers[5] == first + 96); /* retained last-writeback overlap */
    CHECK(cpu.registers[6] == UINT64_C(0x1020304050607080));
    CHECK(cpu.flags == UINT64_C(0xa0000000));

    {
        uint8_t rejected[4096] = {0};
        const uint32_t base = UINT32_C(0xf86678a0);
        const unsigned invalid_options[] = {0, 1, 4, 5};
        hl_a64_assembler reject_assembler;
        for (size_t index = 0; index < 4; index++) {
            uint32_t invalid = (base & ~(UINT32_C(7) << 13)) |
                               ((uint32_t)invalid_options[index] << 13);
            CHECK(hl_a64_assembler_begin(&reject_assembler, rejected, rejected, sizeof(rejected)));
            CHECK(!hl_a64_single_emit(&reject_assembler, invalid, 0x6200 + index * 4));
            CHECK(hl_a64_assembler_size(&reject_assembler) == 0);
        }
        const uint32_t pointer_auth[] = {UINT32_C(0xf82004a4), UINT32_C(0xf8a004a4)};
        for (size_t index = 0; index < 2; index++) {
            CHECK(hl_a64_assembler_begin(&reject_assembler, rejected, rejected, sizeof(rejected)));
            CHECK(!hl_a64_single_emit(&reject_assembler, pointer_auth[index], 0x6240 + index * 4));
            CHECK(hl_a64_assembler_size(&reject_assembler) == 0);
        }
        for (size_t index = 0; index < sizeof(rejected); index++) CHECK(rejected[index] == 0);
    }

    cpu.vectors[0] = UINT64_C(0x1111111111111111);
    cpu.vectors[1] = UINT64_C(0x2222222222222222);
    cpu.vectors[2] = UINT64_C(0x3333333333333333);
    cpu.vectors[3] = UINT64_C(0x4444444444444444);
    cpu.registers[7] = first + 64;
    cpu.dirty_first = UINT64_MAX;
    cpu.dirty_last = 0;
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.registers[7] == first + 96);
    CHECK(cpu.dirty_first == first + 96 && cpu.dirty_last == first + 128);
    memset(cpu.vectors + 4, 0, 4 * sizeof(cpu.vectors[0]));
    cpu.registers[7] = first + 96;
    execute(&cpu, code + offsets[3]);
    CHECK(cpu.registers[7] == first + 128);
    CHECK(cpu.vectors[4] == UINT64_C(0x1111111111111111));
    CHECK(cpu.vectors[5] == UINT64_C(0x2222222222222222));
    CHECK(cpu.vectors[6] == UINT64_C(0x3333333333333333));
    CHECK(cpu.vectors[7] == UINT64_C(0x4444444444444444));

    cpu.registers[16] = UINT64_C(0x1616161616161616);
    cpu.registers[17] = UINT64_C(0x1717171717171717);
    cpu.registers[18] = first + 176;
    cpu.dirty_first = UINT64_MAX;
    execute(&cpu, code + offsets[4]);
    CHECK(cpu.registers[18] == first + 160);
    CHECK(*(uint64_t *)(void *)(memory + 160) == UINT64_C(0x1616161616161616));
    CHECK(*(uint64_t *)(void *)(memory + 168) == UINT64_C(0x1717171717171717));
    *(uint64_t *)(void *)(memory + 192) = UINT64_C(0xaaaaaaaaaaaaaaaa);
    *(uint64_t *)(void *)(memory + 200) = UINT64_C(0xbbbbbbbbbbbbbbbb);
    cpu.registers[18] = first + 192;
    cpu.registers[19] = 0;
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.registers[18] == first + 208);
    CHECK(cpu.registers[19] == UINT64_C(0xbbbbbbbbbbbbbbbb));

    memset(memory + 32, 0x5a, 16);
    cpu.registers[0] = UINT64_C(0xaaaaaaaaaaaaaaaa);
    cpu.registers[1] = UINT64_C(0xbbbbbbbbbbbbbbbb);
    cpu.registers[5] = first + 48;
    cpu.memory_first = first + 40; /* pre-index address first+32 is rejected */
    cpu.dirty_first = UINT64_MAX;
    cpu.dirty_last = 0;
    cpu.memory_written = 0;
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.program == 0x6000);
    CHECK(cpu.registers[5] == first + 48);
    CHECK(cpu.dirty_first == UINT64_MAX && cpu.dirty_last == 0 && cpu.memory_written == 0);
    for (size_t index = 32; index < 48; index++) CHECK(memory[index] == 0x5a);
    CHECK(cpu.flags == UINT64_C(0xa0000000));

    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
