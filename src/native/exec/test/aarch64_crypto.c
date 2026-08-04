#include "../src/arch/aarch64/crypto.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "crypto:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t two(unsigned prefix, unsigned opcode, unsigned rn, unsigned rd) {
    return prefix | (opcode << 12) | (rn << 5) | rd;
}

static uint32_t three(unsigned opcode, unsigned rm, unsigned rn, unsigned rd) {
    return UINT32_C(0x5e000000) | (rm << 16) | (opcode << 12) | (rn << 5) | rd;
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
    uint32_t words[14] = {
        two(UINT32_C(0x4e280800), 4, 1, 0), /* AESE */
        two(UINT32_C(0x4e280800), 5, 2, 2), /* AESD, overlap */
        two(UINT32_C(0x4e280800), 6, 3, 4), /* AESMC */
        two(UINT32_C(0x4e280800), 7, 5, 5), /* AESIMC, overlap */
        two(UINT32_C(0x5e280800), 0, 6, 6), /* SHA1H, overlap */
        two(UINT32_C(0x5e280800), 1, 7, 8), /* SHA1SU1 */
        two(UINT32_C(0x5e280800), 2, 9, 9), /* SHA256SU0, overlap */
        three(0, 10, 11, 10),              /* SHA1C, rd==rm */
        three(1, 12, 12, 13),              /* SHA1P, rn==rm */
        three(2, 14, 15, 15),              /* SHA1M, rd==rn */
        three(3, 16, 16, 16),              /* SHA1SU0, all overlap */
        three(4, 17, 18, 19),              /* SHA256H */
        three(5, 20, 21, 21),              /* SHA256H2, rd==rn */
        three(6, 22, 22, 22),              /* SHA256SU1, all overlap */
    };
    uint8_t body[sizeof(uint32_t)] = {0};
    hl_a64_assembler assembler;
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        memset(body, 0, sizeof(body));
        CHECK(hl_a64_assembler_begin(&assembler, body, body, sizeof(body)));
        int available = hl_a64_crypto_host_supports(words[index]);
        CHECK(hl_a64_crypto_body(&assembler, words[index]) == available);
        if (available) CHECK(memcmp(body, &words[index], sizeof(words[index])) == 0);
        else CHECK(hl_a64_assembler_size(&assembler) == 0);
    }

    for (unsigned opcode = 0; opcode < 32; opcode++) {
        uint32_t aes = two(UINT32_C(0x4e280800), opcode, 1, 0);
        uint32_t sha = two(UINT32_C(0x5e280800), opcode, 1, 0);
        CHECK(hl_a64_crypto_host_supports(aes) ==
              (opcode >= 4 && opcode <= 7 && hl_a64_crypto_host_supports(words[0])));
        enum { SHA1_TWO = 4, SHA2_TWO = 6 };
        int expected = opcode <= 1 ? hl_a64_crypto_host_supports(words[SHA1_TWO])
                                   : opcode == 2 ? hl_a64_crypto_host_supports(words[SHA2_TWO]) : 0;
        CHECK(hl_a64_crypto_host_supports(sha) == expected);
    }
    for (unsigned opcode = 0; opcode < 8; opcode++) {
        uint32_t word = three(opcode, 2, 1, 0);
        int expected = opcode <= 3 ? hl_a64_crypto_host_supports(words[7])
                                   : opcode <= 6 ? hl_a64_crypto_host_supports(words[11]) : 0;
        CHECK(hl_a64_crypto_host_supports(word) == expected);
    }
    CHECK(!hl_a64_crypto_host_supports(0));
    CHECK(!hl_a64_crypto_host_supports(words[0] ^ UINT32_C(0x00400000)));
    CHECK(!hl_a64_crypto_host_supports(words[7] ^ UINT32_C(0x00200000)));

    uint8_t short_buffer[HL_A64_CRYPTO_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_crypto_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    if (!hl_a64_crypto_host_supports(words[0]) || !hl_a64_crypto_host_supports(words[4]) ||
        !hl_a64_crypto_host_supports(words[6]))
        return 0;
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[14];
    for (size_t index = 0; index < 14; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_crypto_emit(&assembler, words[index], UINT64_C(0x8000) + index * 4));
    }
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    for (size_t index = 0; index < 64; index++) cpu.vectors[index] = UINT64_C(0x1020304050607080) + index;
    for (size_t index = 0; index < 31; index++) cpu.registers[index] = UINT64_C(0x8877665544332200) + index;
    cpu.stack = (uint64_t)(uintptr_t)(code + capacity - 16);
    cpu.flags = UINT64_C(0xa0000000);
    cpu.fpcr = UINT64_C(0x00400000);
    cpu.fpsr = UINT64_C(0x00000010);
    for (size_t index = 0; index < 14; index++) {
        uint64_t registers[31];
        uint64_t vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        unsigned destination = words[index] & 31u;
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0x8004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0xa0000000) && cpu.fpcr == UINT64_C(0x00400000) &&
              cpu.fpsr == UINT64_C(0x00000010));
        CHECK(memcmp(cpu.registers, registers, sizeof(registers)) == 0);
        for (unsigned vector = 0; vector < 32; vector++)
            if (vector != destination)
                CHECK(memcmp(&cpu.vectors[vector * 2], &vectors[vector * 2], 16) == 0);
    }
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
