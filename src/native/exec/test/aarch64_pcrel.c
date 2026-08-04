#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/pcrel.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "pcrel:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t pcrel(int page, int32_t immediate, unsigned destination) {
    uint32_t encoded = (uint32_t)immediate & 0x1FFFFFu;
    return (page ? 0x90000000u : 0x10000000u) | ((encoded & 3u) << 29) |
           ((encoded >> 2) << 5) | destination;
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const unsigned stolen[] = {16, 17, 18, 28, 30};
    uint32_t words[13];
    uint64_t pcs[13];
    uint64_t expected[13];
    size_t offsets[13];
    size_t count = 0;
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++) {
        words[count] = pcrel(0, 7, stolen[i]);
        pcs[count] = 0x1003 + i * 0x10;
        expected[count] = pcs[count] + 7;
        count++;
    }
    words[count] = pcrel(0, -4, 1); pcs[count] = 0x2000; expected[count++] = 0x1ffc;
    words[count] = pcrel(0, 0x0fffff, 1); pcs[count] = 3; expected[count++] = 0x100002;
    words[count] = pcrel(0, -0x100000, 1); pcs[count] = 0; expected[count++] = UINT64_C(0xfffffffffff00000);
    words[count] = pcrel(1, 1, 1); pcs[count] = 0x12fff; expected[count++] = 0x13000;
    words[count] = pcrel(1, -2, 1); pcs[count] = 0x12abc; expected[count++] = 0x10000;
    words[count] = pcrel(1, 1, 1); pcs[count] = UINT64_C(0xfffffffffffff123); expected[count++] = 0;
    words[count] = pcrel(0, 5, 1); pcs[count] = UINT64_MAX - 1; expected[count++] = 3;
    words[count] = pcrel(0, 5, 31); pcs[count] = UINT64_MAX - 1; expected[count++] = 0;

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_pcrel_emit(&assembler, words[i], pcs[i]));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xa0000000);
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++) {
        execute(&cpu, code + offsets[i]);
        CHECK(cpu.registers[stolen[i]] == expected[i]);
    }
    for (size_t i = 5; i < count - 1; i++) {
        execute(&cpu, code + offsets[i]);
        CHECK(cpu.registers[1] == expected[i]);
    }
    cpu.registers[30] = UINT64_C(0x5555555555555555);
    execute(&cpu, code + offsets[count - 1]);
    CHECK(cpu.registers[30] == UINT64_C(0x5555555555555555));
    CHECK(cpu.flags == UINT64_C(0xa0000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_PCREL_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_pcrel_emit(&assembler, words[0], 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_pcrel_emit(&assembler, 0xd503201fu, 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
