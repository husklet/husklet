#include "../src/arch/aarch64/atomic.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/projection.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "atomic:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static uint64_t mask(unsigned bytes) {
    return bytes == 8 ? UINT64_MAX : (UINT64_C(1) << (bytes * 8)) - 1;
}

static int64_t sign_extend(uint64_t value, unsigned bytes) {
    unsigned bits = bytes * 8;
    if (bits == 64) return (int64_t)value;
    uint64_t sign = UINT64_C(1) << (bits - 1);
    return (int64_t)(((value & mask(bytes)) ^ sign) - sign);
}

static uint32_t cas_word(unsigned size, unsigned acquire, unsigned release,
                         unsigned rs, unsigned rn, unsigned rt) {
    return UINT32_C(0x08a07c00) | (size << 30) | (acquire << 22) | (release << 15) |
           (rs << 16) | (rn << 5) | rt;
}

static uint32_t rmw_word(unsigned size, unsigned acquire, unsigned release, unsigned o3,
                         unsigned opcode, unsigned rs, unsigned rn, unsigned rt) {
    return UINT32_C(0x38200000) | (size << 30) | (acquire << 23) | (release << 22) |
           (rs << 16) | (o3 << 15) | (opcode << 12) | (rn << 5) | rt;
}

/* The architectural result the interpreter must also produce. */
static uint64_t reference(unsigned o3, unsigned opcode, unsigned bytes, uint64_t old,
                          uint64_t operand) {
    uint64_t limit = mask(bytes);
    old &= limit;
    operand &= limit;
    if (o3) return operand; /* SWP */
    switch (opcode) {
        case 0: return (old + operand) & limit;
        case 1: return old & ~operand & limit;
        case 2: return (old ^ operand) & limit;
        case 3: return (old | operand) & limit;
        case 4: return sign_extend(old, bytes) > sign_extend(operand, bytes) ? old : operand;
        case 5: return sign_extend(old, bytes) < sign_extend(operand, bytes) ? old : operand;
        case 6: return old > operand ? old : operand;
        default: return old < operand ? old : operand;
    }
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    if (!hl_a64_atomic_host_supports()) {
        /* Without FEAT_LSE every word must stay interpreted. */
        uint8_t probe[256];
        hl_a64_assembler rejecting;
        CHECK(hl_a64_assembler_begin(&rejecting, probe, probe, sizeof(probe)));
        CHECK(!hl_a64_atomic_emit(&rejecting, cas_word(3, 1, 1, 0, 2, 1), 0x4000));
        return 0;
    }
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 512;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));

    _Alignas(16) uint64_t data = 0;
    hl_native_aarch64_cpu cpu;

    /* ---- rejected words keep their interpreted fallback ---- */
    {
        hl_a64_assembler probe;
        uint8_t storage[512];
        uint32_t definitions;
        CHECK(hl_a64_assembler_begin(&probe, storage, storage, sizeof(storage)));
        /* CASP (bit 23 clear) is a register-pair form and stays unlowered. */
        CHECK(!hl_a64_atomic_emit(&probe, UINT32_C(0x08207c00) | (2u << 5) | 1u, 0x4000));
        CHECK(!hl_a64_atomic_definitions(UINT32_C(0x08207c00) | (2u << 5) | 1u, &definitions));
        /* LDAPR shares the group with o3 set and a nonzero opcode. */
        CHECK(!hl_a64_atomic_emit(&probe, rmw_word(3, 1, 0, 1, 4, 31, 1, 0), 0x4000));
        CHECK(!hl_a64_atomic_definitions(rmw_word(3, 1, 0, 1, 4, 31, 1, 0), &definitions));
        /* Exclusive LDAXR/STLXR remain a separate pending closure. */
        CHECK(!hl_a64_atomic_emit(&probe, UINT32_C(0xc85ffc40), 0x4000));
        /* A stolen Rt has no staged temporary yet. */
        CHECK(!hl_a64_atomic_emit(&probe, rmw_word(3, 1, 1, 0, 0, 2, 1, 17), 0x4000));
        /* Precise results: CAS defines Rs, LD<op> defines Rt, ST<op> defines none. */
        CHECK(hl_a64_atomic_definitions(cas_word(3, 1, 1, 5, 1, 3), &definitions));
        CHECK(definitions == (UINT32_C(1) << 5));
        CHECK(hl_a64_atomic_definitions(rmw_word(3, 1, 1, 0, 0, 5, 1, 3), &definitions));
        CHECK(definitions == (UINT32_C(1) << 3));
        CHECK(hl_a64_atomic_definitions(rmw_word(3, 0, 0, 0, 0, 5, 1, 31), &definitions));
        CHECK(definitions == 0);
    }

    /* ---- compare-and-swap, every width and ordering, both outcomes ---- */
    size_t cas_offsets[4][2][2];
    for (unsigned size = 0; size < 4; ++size)
        for (unsigned acquire = 0; acquire < 2; ++acquire)
            for (unsigned release = 0; release < 2; ++release) {
                cas_offsets[size][acquire][release] = hl_a64_assembler_size(&assembler);
                CHECK(hl_a64_atomic_emit(&assembler, cas_word(size, acquire, release, 2, 1, 3),
                                         0x4000));
            }

    /* ---- SWP and the eight LD<op> forms, every width and ordering ---- */
    size_t rmw_offsets[4][2][8];
    for (unsigned size = 0; size < 4; ++size)
        for (unsigned o3 = 0; o3 < 2; ++o3)
            for (unsigned opcode = 0; opcode < 8; ++opcode) {
                if (o3 && opcode != 0) continue;
                rmw_offsets[size][o3][opcode] = hl_a64_assembler_size(&assembler);
                CHECK(hl_a64_atomic_emit(&assembler,
                                         rmw_word(size, 1, 1, o3, opcode, 2, 1, 3), 0x4000));
            }

    /* ---- ST<op> alias (Rt==31), stolen base, stolen Rs ---- */
    size_t store_alias = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_atomic_emit(&assembler, rmw_word(3, 0, 0, 0, 0, 2, 1, 31), 0x4000));
    size_t stolen_base = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_atomic_emit(&assembler, rmw_word(3, 1, 1, 0, 0, 2, 30, 3), 0x4000));
    size_t stolen_source = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_atomic_emit(&assembler, cas_word(3, 1, 1, 17, 1, 3), 0x4000));

    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    static const uint64_t seeds[] = {
        0, 1, UINT64_C(0x8877665544332211), UINT64_MAX,
        UINT64_C(0x8000000000000000), UINT64_C(0x7fffffffffffffff),
    };
    static const uint64_t operands[] = {
        0, 1, UINT64_C(0xfedcba9876543210), UINT64_MAX,
        UINT64_C(0x8000000000000000), UINT64_C(0x7fffffffffffffff),
    };

    for (unsigned size = 0; size < 4; ++size) {
        unsigned bytes = 1u << size;
        uint64_t limit = mask(bytes);
        for (unsigned s = 0; s < sizeof(seeds) / sizeof(seeds[0]); ++s)
            for (unsigned o = 0; o < sizeof(operands) / sizeof(operands[0]); ++o) {
                /* --- CAS, both the matching and the mismatching outcome --- */
                for (unsigned matching = 0; matching < 2; ++matching)
                    for (unsigned acquire = 0; acquire < 2; ++acquire)
                        for (unsigned release = 0; release < 2; ++release) {
                            uint64_t old = seeds[s];
                            uint64_t compare = matching ? old : ~old;
                            data = old;
                            memset(&cpu, 0, sizeof(cpu));
                            cpu.memory_first = (uint64_t)(uintptr_t)&data;
                            cpu.memory_last = cpu.memory_first + sizeof(data);
                            cpu.memory_permissions =
                                HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
                            cpu.registers[1] = (uint64_t)(uintptr_t)&data;
                            cpu.registers[2] = compare;
                            cpu.registers[3] = operands[o];
                            execute(&cpu, code + cas_offsets[size][acquire][release]);
                            CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
                            int hit = (compare & limit) == (old & limit);
                            uint64_t expected = hit ? (old & ~limit) | (operands[o] & limit)
                                                    : old;
                            CHECK((data & limit) == (expected & limit));
                            /* Rs receives the pre-op value, zero-extended. */
                            CHECK(cpu.registers[2] == (old & limit));
                            /* Rt is read-only for every CAS form. */
                            CHECK(cpu.registers[3] == operands[o]);
                            CHECK(cpu.memory_written == 1);
                        }

                /* --- SWP and LD<op> --- */
                for (unsigned o3 = 0; o3 < 2; ++o3)
                    for (unsigned opcode = 0; opcode < 8; ++opcode) {
                        if (o3 && opcode != 0) continue;
                        uint64_t old = seeds[s];
                        data = old;
                        memset(&cpu, 0, sizeof(cpu));
                        cpu.memory_first = (uint64_t)(uintptr_t)&data;
                        cpu.memory_last = cpu.memory_first + sizeof(data);
                        cpu.memory_permissions =
                            HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
                        cpu.registers[1] = (uint64_t)(uintptr_t)&data;
                        cpu.registers[2] = operands[o];
                        cpu.registers[3] = UINT64_C(0xa5a5a5a5a5a5a5a5);
                        execute(&cpu, code + rmw_offsets[size][o3][opcode]);
                        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
                        uint64_t stored = reference(o3, opcode, bytes, old, operands[o]);
                        CHECK((data & limit) == (stored & limit));
                        /* Rt receives the pre-op value, zero-extended. */
                        CHECK(cpu.registers[3] == (old & limit));
                        /* Rs is read-only. */
                        CHECK(cpu.registers[2] == operands[o]);
                        CHECK(cpu.memory_written == 1);
                    }
            }
    }

    /* ---- ST<op> alias writes memory and defines no register ---- */
    data = 7;
    memset(&cpu, 0, sizeof(cpu));
    cpu.memory_first = (uint64_t)(uintptr_t)&data;
    cpu.memory_last = cpu.memory_first + sizeof(data);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.registers[1] = (uint64_t)(uintptr_t)&data;
    cpu.registers[2] = 5;
    execute(&cpu, code + store_alias);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
    CHECK(data == 12 && cpu.registers[2] == 5 && cpu.memory_written == 1);

    /* ---- a stolen base is staged from the CPU record, never from the host ---- */
    data = 20;
    memset(&cpu, 0, sizeof(cpu));
    cpu.memory_first = (uint64_t)(uintptr_t)&data;
    cpu.memory_last = cpu.memory_first + sizeof(data);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.registers[30] = (uint64_t)(uintptr_t)&data;
    cpu.registers[2] = 3;
    cpu.registers[3] = 0;
    execute(&cpu, code + stolen_base);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
    CHECK(data == 23 && cpu.registers[3] == 20);
    CHECK(cpu.registers[30] == (uint64_t)(uintptr_t)&data);

    /* ---- a stolen Rs is staged in and its CAS result published back out ---- */
    data = 41;
    memset(&cpu, 0, sizeof(cpu));
    cpu.memory_first = (uint64_t)(uintptr_t)&data;
    cpu.memory_last = cpu.memory_first + sizeof(data);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.registers[1] = (uint64_t)(uintptr_t)&data;
    cpu.registers[17] = 41;
    cpu.registers[3] = 99;
    execute(&cpu, code + stolen_source);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
    CHECK(data == 99 && cpu.registers[17] == 41);
    data = 41;
    cpu.registers[17] = 40; /* mismatching compare leaves memory unchanged */
    cpu.registers[3] = 99;
    cpu.memory_written = 0;
    execute(&cpu, code + stolen_source);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
    CHECK(data == 41 && cpu.registers[17] == 41);

    /* ---- a read-only view faults as a write before the atomic runs ---- */
    data = 55;
    memset(&cpu, 0, sizeof(cpu));
    cpu.memory_first = (uint64_t)(uintptr_t)&data;
    cpu.memory_last = cpu.memory_first + sizeof(data);
    cpu.memory_permissions = HL_A64_PERMISSION_READ;
    cpu.registers[1] = (uint64_t)(uintptr_t)&data;
    cpu.registers[2] = 1;
    cpu.registers[3] = 0;
    execute(&cpu, code + rmw_offsets[3][0][0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK);
    CHECK(cpu.fault_access == HL_A64_PERMISSION_WRITE && cpu.fault_size == 8);
    CHECK(data == 55);

    /* ---- an address outside the view faults without mutating it ---- */
    data = 66;
    memset(&cpu, 0, sizeof(cpu));
    cpu.memory_first = (uint64_t)(uintptr_t)&data + 8;
    cpu.memory_last = cpu.memory_first + 8;
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.registers[1] = (uint64_t)(uintptr_t)&data;
    cpu.registers[2] = 1;
    execute(&cpu, code + rmw_offsets[3][0][0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK);
    CHECK(data == 66);

    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
