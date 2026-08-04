#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/arch/aarch64/structure.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "structures:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static int multiple(unsigned opcode, unsigned q, unsigned size, uint64_t *bytes) {
    unsigned registers;
    switch (opcode) {
        case 0:
        case 2: registers = 4; break;
        case 4:
        case 6: registers = 3; break;
        case 8:
        case 10: registers = 2; break;
        case 7: registers = 1; break;
        default: return 0;
    }
    if (size == 3 && q == 0 && registers > 1) return 0;
    *bytes = (uint64_t)registers * (q ? 16u : 8u);
    return 1;
}

static int single(unsigned opcode, unsigned r, unsigned load, unsigned selector,
                  unsigned size, uint64_t *bytes) {
    unsigned registers = (r ? 2u : 1u) + ((opcode & 1u) ? 2u : 0u);
    unsigned element;
    switch (opcode >> 1) {
        case 0: element = 0; break;
        case 1:
            if (size & 1u) return 0;
            element = 1;
            break;
        case 2:
            if (size == 0)
                element = 2;
            else if (size == 1 && selector == 0)
                element = 3;
            else
                return 0;
            break;
        default:
            if (!load || selector) return 0;
            element = size;
            break;
    }
    *bytes = (uint64_t)registers << element;
    return 1;
}

static int allocation_matrix(void) {
    uint8_t buffer[4096];
    for (unsigned q = 0; q < 2; q++)
        for (unsigned load = 0; load < 2; load++)
            for (unsigned post = 0; post < 2; post++)
                for (unsigned opcode = 0; opcode < 16; opcode++)
                    for (unsigned size = 0; size < 4; size++) {
                        uint64_t expected = 0;
                        int accepted = multiple(opcode, q, size, &expected);
                        uint32_t word = UINT32_C(0x0c000000) | (q << 30) | (load << 22) |
                                        (post << 23) | ((post ? 31u : 0u) << 16) |
                                        (opcode << 12) | (size << 10) | (1u << 5) | 28u;
                        hl_a64_assembler assembler;
                        hl_a64_guard guard = {0};
                        hl_a64_memory_sites sites = {0};
                        memset(buffer, 0, sizeof(buffer));
                        CHECK(hl_a64_assembler_begin(&assembler, buffer, buffer, sizeof(buffer)));
                        CHECK(hl_a64_structure_body(&assembler, word, 0x4000, &guard, &sites) == accepted);
                        if (accepted) {
                            CHECK(hl_a64_assembler_size(&assembler) != 0);
                            CHECK(sites.count == 1 && sites.entries[0].width == expected);
                        } else {
                            CHECK(hl_a64_assembler_size(&assembler) == 0 && sites.count == 0);
                        }
                    }
    for (unsigned q = 0; q < 2; q++)
        for (unsigned load = 0; load < 2; load++)
            for (unsigned post = 0; post < 2; post++)
                for (unsigned r = 0; r < 2; r++)
                    for (unsigned opcode = 0; opcode < 8; opcode++)
                        for (unsigned selector = 0; selector < 2; selector++)
                            for (unsigned size = 0; size < 4; size++) {
                                uint64_t expected = 0;
                                int accepted = single(opcode, r, load, selector, size, &expected);
                                uint32_t word = UINT32_C(0x0d000000) | (q << 30) | (load << 22) |
                                                (post << 23) | (r << 21) |
                                                ((post ? 31u : 0u) << 16) | (opcode << 13) |
                                                (selector << 12) | (size << 10) | (1u << 5) | 28u;
                                hl_a64_assembler assembler;
                                hl_a64_guard guard = {0};
                                hl_a64_memory_sites sites = {0};
                                memset(buffer, 0, sizeof(buffer));
                                CHECK(hl_a64_assembler_begin(&assembler, buffer, buffer, sizeof(buffer)));
                                CHECK(hl_a64_structure_body(&assembler, word, 0x5000,
                                                            &guard, &sites) == accepted);
                                if (accepted) {
                                    CHECK(hl_a64_assembler_size(&assembler) != 0);
                                    CHECK(sites.count == 1 && sites.entries[0].width == expected);
                                } else {
                                    CHECK(hl_a64_assembler_size(&assembler) == 0 && sites.count == 0);
                                }
                            }
    return 0;
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
    if (allocation_matrix() != 0) return 1;
#if !defined(__aarch64__)
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    const uint32_t words[] = {
        UINT32_C(0x0dc670be), /* ld3 {v30.h-v0.h}[2],[x5],x6 */
        UINT32_C(0x0dbfb21e), /* st4 {v30.s-v1.s}[1],[x16],#16 */
        UINT32_C(0x4df1e65e), /* ld4r {v30.8h-v1.8h},[x18],x17 */
    };
    size_t offsets[3];
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < 3; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_structure_emit(&assembler, words[index], 0x7000 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    _Alignas(16) uint8_t data[128] = {0};
    hl_native_aarch64_cpu cpu = {0};
    uint64_t first = (uint64_t)(uintptr_t)data;
    cpu.memory_first = first;
    cpu.memory_last = first + sizeof(data);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.flags = UINT64_C(0x60000000);

    const uint16_t lanes[] = {0x1111, 0x2222, 0x3333};
    memcpy(data + 16, lanes, sizeof(lanes));
    cpu.registers[5] = first + 16;
    cpu.registers[6] = 24;
    memset(cpu.vectors + 60, 0xa5, 4 * sizeof(cpu.vectors[0]));
    memset(cpu.vectors, 0xa5, 2 * sizeof(cpu.vectors[0]));
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.registers[5] == first + 40);
    CHECK(((uint16_t *)cpu.vectors)[30 * 8 + 2] == lanes[0]);
    CHECK(((uint16_t *)cpu.vectors)[31 * 8 + 2] == lanes[1]);
    CHECK(((uint16_t *)cpu.vectors)[2] == lanes[2]);
    CHECK(cpu.flags == UINT64_C(0x60000000));

    uint32_t *vectors32 = (uint32_t *)cpu.vectors;
    vectors32[30 * 4 + 1] = UINT32_C(0x30303030);
    vectors32[31 * 4 + 1] = UINT32_C(0x31313131);
    vectors32[0 * 4 + 1] = UINT32_C(0x00000000);
    vectors32[1 * 4 + 1] = UINT32_C(0x01010101);
    cpu.registers[16] = first + 48;
    cpu.dirty_first = UINT64_MAX;
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.registers[16] == first + 64);
    CHECK(((uint32_t *)(void *)(data + 48))[0] == UINT32_C(0x30303030));
    CHECK(((uint32_t *)(void *)(data + 48))[1] == UINT32_C(0x31313131));
    CHECK(((uint32_t *)(void *)(data + 48))[2] == UINT32_C(0x00000000));
    CHECK(((uint32_t *)(void *)(data + 48))[3] == UINT32_C(0x01010101));
    CHECK(cpu.dirty_first == first + 48 && cpu.dirty_last == first + 64);
    CHECK(cpu.dirty_view_first == first && cpu.dirty_view_last == first + sizeof(data));

    const uint16_t replicate[] = {0xaaaa, 0xbbbb, 0xcccc, 0xdddd};
    memcpy(data + 80, replicate, sizeof(replicate));
    cpu.registers[18] = first + 80;
    cpu.registers[17] = 32;
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.registers[18] == first + 112);
    for (unsigned reg = 30; reg < 34; reg++) {
        unsigned actual = reg & 31u;
        for (unsigned lane = 0; lane < 8; lane++)
            CHECK(((uint16_t *)cpu.vectors)[actual * 8 + lane] == replicate[reg - 30]);
    }

    uint64_t before30[2], before31[2], before0[2];
    memcpy(before30, cpu.vectors + 60, sizeof(before30));
    memcpy(before31, cpu.vectors + 62, sizeof(before31));
    memcpy(before0, cpu.vectors, sizeof(before0));
    cpu.registers[5] = first + 124;
    cpu.registers[6] = 24;
    cpu.memory_last = first + 128;
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.program == 0x7000);
    CHECK(cpu.registers[5] == first + 124);
    CHECK(memcmp(before30, cpu.vectors + 60, sizeof(before30)) == 0);
    CHECK(memcmp(before31, cpu.vectors + 62, sizeof(before31)) == 0);
    CHECK(memcmp(before0, cpu.vectors, sizeof(before0)) == 0);
    CHECK(cpu.flags == UINT64_C(0x60000000));
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
