#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/arch/aarch64/structure.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "structure:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    _Alignas(16) uint8_t data[128] = {0};
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    const uint32_t words[] = {
        0x4c407020u, /* ld1 {v0.16b},[x1] */
        0x4c007020u, /* st1 {v0.16b},[x1] */
        (0x4cdf7020u & ~(31u << 5)) | (28u << 5), /* ld1 ..., [x28],#16 */
        0x4c408020u, /* ld2 {v0.16b,v1.16b},[x1] */
        0x4ddfc020u, /* ld1r {v0.16b},[x1],#1 */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < sizeof(words) / sizeof(words[0]); i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_structure_emit(&assembler, words[i], 0x8000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.memory_first = (uint64_t)(uintptr_t)data;
    cpu.memory_last = (uint64_t)(uintptr_t)(data + sizeof(data));
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.registers[1] = (uint64_t)(uintptr_t)data;
    for (unsigned i = 0; i < 32; i++) data[i] = (uint8_t)(i + 1);
    execute(&cpu, code + offsets[0]);
    CHECK(memcmp(cpu.vectors, data, 16) == 0);

    for (unsigned i = 0; i < 16; i++) ((uint8_t *)cpu.vectors)[i] = (uint8_t)(0xe0 + i);
    memset(data, 0, 16);
    execute(&cpu, code + offsets[1]);
    CHECK(memcmp(data, cpu.vectors, 16) == 0);

    for (unsigned i = 0; i < 16; i++) data[i] = (uint8_t)(0x40 + i);
    cpu.registers[28] = (uint64_t)(uintptr_t)data;
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.registers[28] == (uint64_t)(uintptr_t)(data + 16));
    CHECK(memcmp(cpu.vectors, data, 16) == 0);

    for (unsigned i = 0; i < 32; i++) data[i] = (uint8_t)i;
    execute(&cpu, code + offsets[3]);
    for (unsigned i = 0; i < 16; i++) {
        CHECK(((uint8_t *)cpu.vectors)[i] == data[i * 2]);
        CHECK(((uint8_t *)cpu.vectors)[16 + i] == data[i * 2 + 1]);
    }
    data[0] = 0x7b;
    execute(&cpu, code + offsets[4]);
    for (unsigned i = 0; i < 16; i++) CHECK(((uint8_t *)cpu.vectors)[i] == 0x7b);
    CHECK(cpu.registers[1] == (uint64_t)(uintptr_t)(data + 1));

    memset(cpu.vectors, 0xa5, 16);
    cpu.registers[1] = (uint64_t)(uintptr_t)data;
    cpu.memory_last = (uint64_t)(uintptr_t)(data + 8);
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.program == 0x8000);
    for (unsigned i = 0; i < 16; i++) CHECK(((uint8_t *)cpu.vectors)[i] == 0xa5);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_STRUCTURE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_structure_emit(&assembler, words[0], 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
