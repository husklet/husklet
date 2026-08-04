#include "system.h"

#include "../../../include/executor.h"
#include "stub.h"

#include <stddef.h>

#define CPU 28
#define OFFSET_TLS ((int)offsetof(hl_native_aarch64_cpu, tls))

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int read_tls(uint32_t word) {
    uint32_t operation = word & 0xffffffe0u;
    return operation == 0xd53bd040u || operation == 0xd53bd060u;
}

static int write_tls(uint32_t word) {
    return (word & 0xffffffe0u) == 0xd51bd040u;
}

static int barrier(uint32_t word) {
    return (word & 0xfffff0ffu) == 0xd50330bfu; /* dmb <option> */
}

int hl_a64_system_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned reg = word & 31u;
    if (assembler == NULL || (!read_tls(word) && !write_tls(word) && !barrier(word))) return 0;
    if (barrier(word)) {
        hl_a64_emit32(assembler, word);
    } else if (read_tls(word)) {
        unsigned native = reg != 31 && stolen(reg) ? 16u : reg;
        hl_a64_ldr(assembler, (int)native, CPU, OFFSET_TLS);
        if (reg != 31 && stolen(reg)) hl_a64_str(assembler, 16, CPU, (int)reg * 8);
    } else if (reg == 31) {
        hl_a64_movz(assembler, 16, 0, 0);
        hl_a64_str(assembler, 16, CPU, OFFSET_TLS);
    } else if (stolen(reg)) {
        hl_a64_ldr(assembler, 16, CPU, (int)reg * 8);
        hl_a64_str(assembler, 16, CPU, OFFSET_TLS);
    } else {
        hl_a64_str(assembler, (int)reg, CPU, OFFSET_TLS);
    }
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_system_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_SYSTEM_MAX_BYTES ||
        (!read_tls(word) && !write_tls(word) && !barrier(word))) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_system_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
