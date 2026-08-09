#include "../src/arch/aarch64/ordered.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/projection.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "ordered:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static uint64_t mask(unsigned bytes) {
    return bytes == 8 ? UINT64_MAX : (UINT64_C(1) << (bytes * 8)) - 1;
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 8;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[16];
    unsigned count = 0;
    for (unsigned size = 0; size < 4; ++size)
        for (unsigned load = 0; load < 2; ++load)
            for (unsigned limited = 0; limited < 2; ++limited) {
                uint32_t word = UINT32_C(0x089f7c00) | (size << 30) |
                                (load << 22) | (limited << 15) | (1u << 5) | 2u;
                offsets[count++] = hl_a64_assembler_size(&assembler);
                CHECK(hl_a64_ordered_emit(&assembler, word, 0x4000 + count * 4));
            }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    _Alignas(8) uint64_t data = UINT64_C(0x8877665544332211);
    hl_native_aarch64_cpu cpu = {0};
    cpu.memory_first = (uint64_t)(uintptr_t)&data;
    cpu.memory_last = cpu.memory_first + sizeof(data);
    count = 0;
    for (unsigned size = 0; size < 4; ++size) {
        unsigned bytes = 1u << size;
        for (unsigned load = 0; load < 2; ++load)
            for (unsigned limited = 0; limited < 2; ++limited) {
                uint64_t value = UINT64_C(0xfedcba9876543210);
                data = UINT64_C(0x8877665544332211);
                memset(&cpu.registers, 0, sizeof(cpu.registers));
                cpu.registers[1] = (uint64_t)(uintptr_t)&data;
                cpu.registers[2] = value;
                cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
                cpu.memory_written = 0;
                execute(&cpu, code + offsets[count]);
                CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
                if (load) {
                    CHECK(cpu.registers[2] == (UINT64_C(0x8877665544332211) & mask(bytes)));
                    CHECK(cpu.memory_written == 0);
                } else {
                    CHECK((data & mask(bytes)) == (value & mask(bytes)));
                    CHECK(cpu.memory_written == 1);
                }
                ++count;
            }
    }
    cpu.memory_permissions = HL_A64_PERMISSION_READ;
    cpu.registers[1] = (uint64_t)(uintptr_t)&data;
    cpu.registers[2] = 1;
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK);
    CHECK(cpu.fault_access == HL_A64_PERMISSION_WRITE && cpu.fault_size == 1);

    /* Exact-journal capacity is checked before the host store. The existing
     * interval and sixteen retained records leave no exact slot for a
     * disjoint byte, so the default conservatively reports overflow and keeps
     * executing the ordered store. */
    uint8_t bounded[32] = {0};
    memset(&cpu, 0, sizeof(cpu));
    cpu.memory_first = (uint64_t)(uintptr_t)bounded;
    cpu.memory_last = cpu.memory_first + sizeof(bounded);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.dirty_first = cpu.memory_first;
    cpu.dirty_last = cpu.memory_first + 1;
    cpu.dirty_view_first = cpu.memory_first;
    cpu.dirty_view_last = cpu.memory_last;
    cpu.dirty_count = 16;
    cpu.registers[1] = cpu.memory_first + 8;
    cpu.registers[2] = 0xff;
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
    CHECK(bounded[8] == 0xff);
    CHECK(cpu.dirty_count == 16 && cpu.dirty_overflow == 1);
    CHECK(cpu.dirty_first == cpu.memory_first + 8 && cpu.dirty_last == cpu.memory_first + 9);
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
