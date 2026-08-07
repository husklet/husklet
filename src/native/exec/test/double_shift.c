#include "../src/arch/x86_64/frontend.h"
#include "../src/arch/x86_64/flags.h"
#include "../include/cpu.h"
#include "../include/executor.h"

#include <stdint.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#if defined(__aarch64__)
extern void hl_x86_test_enter(hl_native_x86_64_cpu *, void *);
#endif

#define CHECK(expression)                                                        \
    do {                                                                         \
        if (!(expression)) {                                                     \
            fprintf(stderr, "x86_double_shift:%d: %s\n", __LINE__, #expression); \
            return __LINE__;                                                     \
        }                                                                        \
    } while (0)

static hl_x86_a64_status translate(const uint8_t *guest, size_t size,
                                   hl_x86_a64_result *result) {
    static uint32_t host[2048];
    static hl_x86_a64_provenance provenance[8];
    hl_x86_a64_request request;

    memset(&request, 0, sizeof request);
    memset(host, 0, sizeof host);
    memset(provenance, 0, sizeof provenance);
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = UINT64_C(0x400000);
    request.guest_bytes = guest;
    request.guest_size = size;
    request.max_instructions = 1u;
    request.host_words = host;
    request.host_capacity = sizeof host / sizeof host[0];
    request.provenance = provenance;
    request.provenance_capacity = sizeof provenance / sizeof provenance[0];
    request.flags = HL_X86_A64_LSE;
    return hl_x86_a64_emit(&request, result);
}

static int accepts_all_forms(void) {
    static const uint8_t opcodes[] = {0xa4u, 0xa5u, 0xacu, 0xadu};
    static const uint8_t prefixes[][2] = {{0x66u, 0u}, {0u, 0u}, {0x48u, 0u}};
    size_t opcode;
    size_t width;

    for (opcode = 0; opcode < sizeof opcodes; ++opcode) {
        for (width = 0; width < sizeof prefixes / sizeof prefixes[0]; ++width) {
            uint8_t reg[6];
            uint8_t memory[7];
            size_t r = 0u;
            size_t m = 0u;
            hl_x86_a64_result result;
            if (prefixes[width][0] != 0u) {
                reg[r++] = prefixes[width][0];
                memory[m++] = prefixes[width][0];
            }
            reg[r++] = 0x0fu; reg[r++] = opcodes[opcode]; reg[r++] = 0xd8u;
            memory[m++] = 0x0fu; memory[m++] = opcodes[opcode];
            memory[m++] = 0x5cu; memory[m++] = 0x24u; memory[m++] = 0x08u;
            if ((opcodes[opcode] & 1u) == 0u) {
                reg[r++] = 0x01u;
                memory[m++] = 0xffu;
            }
            CHECK(translate(reg, r, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1u && result.exit_pc == UINT64_C(0x400000) + r);
            CHECK(translate(memory, m, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1u && result.exit_pc == UINT64_C(0x400000) + m);
        }
    }
    return 0;
}

static int rejects_prefixes_and_truncation(void) {
    static const uint8_t lock[] = {0xf0u, 0x0fu, 0xa4u, 0xd8u, 1u};
    static const uint8_t repne[] = {0xf2u, 0x0fu, 0xadu, 0xd8u};
    static const uint8_t rep[] = {0xf3u, 0x0fu, 0xa5u, 0xd8u};
    static const uint8_t no_modrm[] = {0x0fu, 0xa4u};
    static const uint8_t no_immediate[] = {0x0fu, 0xacu, 0xd8u};
    hl_x86_a64_result result;

    CHECK(translate(lock, sizeof lock, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(translate(repne, sizeof repne, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(translate(rep, sizeof rep, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(translate(no_modrm, sizeof no_modrm, &result) == HL_X86_A64_TRUNCATED);
    CHECK(translate(no_immediate, sizeof no_immediate, &result) == HL_X86_A64_TRUNCATED);
    return 0;
}

static int register_semantics(void) {
#if defined(__aarch64__)
    static const uint8_t shld[] = {0x48u, 0x0fu, 0xa4u, 0xd8u, 1u};
    static const uint8_t shrd_cl[] = {0x48u, 0x0fu, 0xadu, 0xd8u};
    uint32_t host[2048] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request;
    hl_x86_a64_result result;
    hl_native_x86_64_cpu cpu = {0};
    long page = sysconf(_SC_PAGESIZE);
    void *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    memset(&request, 0, sizeof request);
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = UINT64_C(0x400000);
    request.guest_bytes = shld;
    request.guest_size = sizeof shld;
    request.max_instructions = 1u;
    request.host_words = host;
    request.host_capacity = sizeof host / sizeof host[0];
    request.provenance = provenance;
    request.provenance_capacity = 8u;
    request.flags = HL_X86_A64_LSE;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    memcpy(code, host, result.word_count * sizeof host[0]);
    ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
    __builtin___clear_cache(code, (char *)code + (result.word_count + 1u) * sizeof host[0]);
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    cpu.registers[0] = UINT64_C(0x8000000000000001);
    cpu.registers[3] = UINT64_C(0x4000000000000000);
    cpu.flags = UINT64_C(0x202);
    hl_x86_test_enter(&cpu, code);
    CHECK(cpu.registers[0] == UINT64_C(2));
    CHECK((cpu.flags & UINT64_C(0x8c5)) == UINT64_C(0x801));

    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    request.guest_bytes = shrd_cl;
    request.guest_size = sizeof shrd_cl;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    memcpy(code, host, result.word_count * sizeof host[0]);
    ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
    __builtin___clear_cache(code, (char *)code + (result.word_count + 1u) * sizeof host[0]);
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    cpu.registers[0] = UINT64_C(0x1122334455667788);
    cpu.registers[1] = 64u;
    cpu.registers[3] = UINT64_C(0x8877665544332211);
    cpu.flags = UINT64_C(0xad7);
    hl_x86_test_enter(&cpu, code);
    CHECK(cpu.registers[0] == UINT64_C(0x1122334455667788));
    CHECK(cpu.flags == UINT64_C(0xad7));
    CHECK(munmap(code, (size_t)page) == 0);
#endif
    return 0;
}

static int memory_semantics(void) {
#if defined(__aarch64__)
    static const uint8_t zero[] = {0x48u, 0x0fu, 0xa4u, 0x18u, 0u};
    static const uint8_t one[] = {0x48u, 0x0fu, 0xa4u, 0x18u, 1u};
    const uint8_t *forms[] = {zero, zero, one, one, one};
    uint32_t host[2048] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    long page = sysconf(_SC_PAGESIZE);
    void *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned form;
    CHECK(code != MAP_FAILED);
    for (form = 0u; form < 5u; ++form) {
        hl_x86_a64_request request;
        hl_x86_a64_result result;
        hl_native_x86_64_cpu cpu = {0};
        uint64_t backing = UINT64_C(0x8000000000000001);
        memset(&request, 0, sizeof request);
        request.abi = HL_X86_A64_FRONTEND_ABI;
        request.size = sizeof request;
        request.guest_pc = UINT64_C(0x400000);
        request.guest_bytes = forms[form];
        request.guest_size = 5u;
        request.max_instructions = 1u;
        request.host_words = host;
        request.host_capacity = sizeof host / sizeof host[0];
        request.provenance = provenance;
        request.provenance_capacity = 8u;
        request.flags = HL_X86_A64_LSE;
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        memcpy(code, host, result.word_count * sizeof host[0]);
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache(code, (char *)code + (result.word_count + 1u) * sizeof host[0]);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = UINT64_C(0x2000);
        cpu.registers[3] = UINT64_C(0x4000000000000000);
        cpu.memory_first = form == 4u ? UINT64_C(0x2008) : UINT64_C(0x2000);
        cpu.memory_last = form == 4u ? UINT64_C(0x2010) : UINT64_C(0x2008);
        cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
        cpu.memory_permissions = form == 0u || form == 2u ? 7u : 1u;
        cpu.dirty_first = UINT64_MAX;
        cpu.flags = UINT64_C(0xad7);
        hl_x86_test_enter(&cpu, code);
        if (form == 0u) {
            CHECK(backing == UINT64_C(0x8000000000000001));
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.reason == 0u);
            CHECK(cpu.memory_written == 1u);
            CHECK(cpu.dirty_first == UINT64_C(0x2000) && cpu.dirty_last == UINT64_C(0x2008));
            CHECK((cpu.executable_written & 4u) != 0u);
        } else if (form == 2u) {
            CHECK(backing == UINT64_C(2));
            CHECK(cpu.memory_written == 1u);
            CHECK(cpu.dirty_first == UINT64_C(0x2000) && cpu.dirty_last == UINT64_C(0x2008));
            CHECK((cpu.executable_written & 4u) != 0u);
        } else {
            CHECK(backing == UINT64_C(0x8000000000000001));
            CHECK(cpu.flags == UINT64_C(0xad7));
            CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK);
            CHECK(cpu.fault_access == (HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE));
            CHECK(cpu.fault_address == UINT64_C(0x2000) && cpu.fault_size == 8u);
            CHECK(cpu.memory_written == 0u && cpu.dirty_first == UINT64_MAX);
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
#endif
    return 0;
}

#if defined(__aarch64__)
static uint64_t width_mask(unsigned bits) {
    return bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
}
static unsigned even_parity(uint64_t value) {
    unsigned ones = 0u;
    unsigned bit;
    for (bit = 0u; bit < 8u; ++bit) ones += (unsigned)(value >> bit & 1u);
    return (ones & 1u) == 0u;
}
#endif

static int exhaustive_register_counts(void) {
#if defined(__aarch64__)
    static const unsigned widths[] = {16u, 32u, 64u};
    static const unsigned counts[] = {0u, 1u, 15u, 16u, 17u, 31u, 32u, 33u,
                                      63u, 64u, 65u, 255u};
    unsigned width_index;
    unsigned memory;
    unsigned right;
    unsigned variable;
    unsigned count_index;
    long page = sysconf(_SC_PAGESIZE);
    void *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    for (width_index = 0u; width_index < 3u; ++width_index) {
        unsigned bits = widths[width_index];
        uint64_t mask = width_mask(bits);
        for (memory = 0u; memory < 2u; ++memory) {
        for (right = 0u; right < 2u; ++right) {
            for (variable = 0u; variable < 2u; ++variable) {
                for (count_index = 0u; count_index < sizeof counts / sizeof counts[0]; ++count_index) {
                    uint8_t guest[6];
                    size_t size = 0u;
                    uint32_t host[2048] = {0};
                    hl_x86_a64_provenance provenance[8] = {0};
                    hl_x86_a64_request request;
                    hl_x86_a64_result result;
                    hl_native_x86_64_cpu cpu = {0};
                    uint64_t destination = UINT64_C(0xa55a800180017ffe);
                    uint64_t source = UINT64_C(0x6996f00f55aa33cc);
                    uint64_t prior = UINT64_C(0xad7);
                    uint64_t backing = destination & mask;
                    unsigned count = counts[count_index] & (bits == 64u ? 63u : 31u);
                    uint64_t expected;
                    uint64_t expected_flags = prior;
                    if (bits == 16u) guest[size++] = 0x66u;
                    if (bits == 64u) guest[size++] = 0x48u;
                    guest[size++] = 0x0fu;
                    guest[size++] = (uint8_t)(right ? (variable ? 0xadu : 0xacu) :
                                                       (variable ? 0xa5u : 0xa4u));
                    guest[size++] = memory ? 0x1fu : 0xd8u;
                    if (!variable) guest[size++] = (uint8_t)counts[count_index];
                    memset(&request, 0, sizeof request);
                    request.abi = HL_X86_A64_FRONTEND_ABI;
                    request.size = sizeof request;
                    request.guest_pc = UINT64_C(0x400000);
                    request.guest_bytes = guest;
                    request.guest_size = size;
                    request.max_instructions = 1u;
                    request.host_words = host;
                    request.host_capacity = sizeof host / sizeof host[0];
                    request.provenance = provenance;
                    request.provenance_capacity = 8u;
                    request.flags = HL_X86_A64_LSE;
                    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
                    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
                    memcpy(code, host, result.word_count * sizeof host[0]);
                    ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
                    __builtin___clear_cache(code, (char *)code + (result.word_count + 1u) * sizeof host[0]);
                    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                    cpu.registers[0] = destination;
                    cpu.registers[1] = counts[count_index];
                    cpu.registers[3] = source;
                    cpu.registers[7] = UINT64_C(0x2000);
                    cpu.memory_first = UINT64_C(0x2000);
                    cpu.memory_last = UINT64_C(0x2000) + bits / 8u;
                    cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
                    cpu.memory_permissions = 7u;
                    cpu.dirty_first = UINT64_MAX;
                    cpu.flags = prior;
                    hl_x86_test_enter(&cpu, code);
                    if (count == 0u || (bits == 16u && count > 16u)) {
                        /* The destination is still written, so the 32-bit form clears the
                           upper half while the 16-bit form preserves it. */
                        expected = bits == 32u ? destination & mask : destination;
                    } else {
                        uint64_t low_destination = destination & mask;
                        uint64_t low_source = source & mask;
                        unsigned carry = right ? (unsigned)(low_destination >> (count - 1u) & 1u) :
                                                 (unsigned)(low_destination >> (bits - count) & 1u);
                        expected = right ? (low_destination >> count) |
                                               ((low_source << (bits - count)) & mask) :
                                           ((low_destination << count) & mask) |
                                               (low_source >> (bits - count));
                        expected_flags &= ~(HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF |
                                            HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF |
                                            HL_X86_RFLAGS_OF);
                        if (carry != 0u) expected_flags |= HL_X86_RFLAGS_CF;
                        if (even_parity(expected)) expected_flags |= HL_X86_RFLAGS_PF;
                        if (expected == 0u) expected_flags |= HL_X86_RFLAGS_ZF;
                        if (expected >> (bits - 1u) & 1u) expected_flags |= HL_X86_RFLAGS_SF;
                        if (count == 1u &&
                            (right ? ((low_destination ^ expected) >> (bits - 1u) & 1u) :
                                     ((expected >> (bits - 1u) & 1u) ^ carry)))
                            expected_flags |= HL_X86_RFLAGS_OF;
                        expected = bits == 16u ? (destination & ~mask) | expected : expected;
                    }
                    if (memory == 0u && cpu.registers[0] != expected)
                        fprintf(stderr, "mismatch bits=%u right=%u variable=%u count=%u got=%#llx expected=%#llx\n",
                                bits, right, variable, counts[count_index],
                                (unsigned long long)cpu.registers[0], (unsigned long long)expected);
                    if (memory != 0u) {
                        CHECK(backing == (expected & mask));
                        if (count == 0u || (bits == 16u && count > 16u)) {
                            CHECK(cpu.memory_written == 1u);
                            CHECK(cpu.dirty_first == UINT64_C(0x2000));
                            CHECK(cpu.dirty_last == UINT64_C(0x2000) + bits / 8u);
                            CHECK((cpu.executable_written & 4u) != 0u);
                        } else {
                            CHECK(cpu.memory_written == 1u);
                            CHECK(cpu.dirty_first == UINT64_C(0x2000));
                            CHECK(cpu.dirty_last == UINT64_C(0x2000) + bits / 8u);
                            CHECK((cpu.executable_written & 4u) != 0u);
                        }
                    } else {
                        CHECK(cpu.registers[0] == expected);
                    }
                    CHECK(cpu.flags == expected_flags);
                }
            }
        }
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
#endif
    return 0;
}

#if defined(__x86_64__)
static sigjmp_buf probe_jump;
static volatile sig_atomic_t probe_faulted;

static void probe_fault(int signal_number) {
    (void)signal_number;
    probe_faulted = 1;
    siglongjmp(probe_jump, 1);
}

static int direct_x86_memory_probe(void) {
    struct sigaction action;
    struct sigaction old_bus;
    struct sigaction old_segv;
    long page = sysconf(_SC_PAGESIZE);
    uint64_t *memory = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint64_t source = UINT64_C(0x6996f00f55aa33cc);
    CHECK(memory != MAP_FAILED);
    *memory = UINT64_C(0xa55a800180017ffe);
    memset(&action, 0, sizeof action);
    action.sa_handler = probe_fault;
    sigemptyset(&action.sa_mask);
    CHECK(sigaction(SIGSEGV, &action, &old_segv) == 0);
    CHECK(sigaction(SIGBUS, &action, &old_bus) == 0);
    CHECK(mprotect(memory, (size_t)page, PROT_READ) == 0);
    probe_faulted = 0;
    if (sigsetjmp(probe_jump, 1) == 0)
        __asm__ volatile("shldq $0, %1, %0" : "+m"(*memory) : "r"(source) : "cc");
    CHECK(probe_faulted != 0);
    CHECK(mprotect(memory, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    *memory = UINT64_C(0xa55a800180017ffe);
    CHECK(mprotect(memory, (size_t)page, PROT_READ) == 0);
    probe_faulted = 0;
    if (sigsetjmp(probe_jump, 1) == 0)
        __asm__ volatile("shldw $17, %w1, %0" : "+m"(*(uint16_t *)memory) : "r"(source) : "cc");
    CHECK(probe_faulted != 0);
    CHECK(mprotect(memory, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    *memory = UINT64_C(0xa55a800180017ffe);
    CHECK(mprotect(memory, (size_t)page, PROT_READ) == 0);
    probe_faulted = 0;
    if (sigsetjmp(probe_jump, 1) == 0)
        __asm__ volatile("shrdw $31, %w1, %0" : "+m"(*(uint16_t *)memory) : "r"(source) : "cc");
    CHECK(probe_faulted != 0);
    CHECK(mprotect(memory, (size_t)page, PROT_NONE) == 0);
    probe_faulted = 0;
    if (sigsetjmp(probe_jump, 1) == 0)
        __asm__ volatile("shrdq $0, %1, %0" : "+m"(*memory) : "r"(source) : "cc");
    CHECK(probe_faulted != 0);
    CHECK(sigaction(SIGBUS, &old_bus, NULL) == 0);
    CHECK(sigaction(SIGSEGV, &old_segv, NULL) == 0);
    CHECK(munmap(memory, (size_t)page) == 0);
    return 0;
}
#else
static int direct_x86_memory_probe(void) {
    return 0;
}
#endif

int main(void) {
    int status = accepts_all_forms();
    if (status != 0) return status;
    status = rejects_prefixes_and_truncation();
    if (status != 0) return status;
    status = register_semantics();
    if (status != 0) return status;
    status = memory_semantics();
    if (status != 0) return status;
    status = exhaustive_register_counts();
    if (status != 0) return status;
    return direct_x86_memory_probe();
}
