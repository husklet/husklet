#include "../src/arch/x86_64/frontend.h"
#include "../src/arch/x86_64/entry.h"
#include "../include/cpu.h"
#include "../include/executor.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(expression) do { if (!(expression)) { \
    fprintf(stderr, "x86_xadd:%d: %s\n", __LINE__, #expression); return __LINE__; \
} } while (0)

static uint32_t host[512];

#if defined(__aarch64__)
extern void hl_x86_test_enter(hl_native_x86_64_cpu *, void *);
#endif

static hl_x86_a64_status emit_capacity(const uint8_t *guest, size_t size, uint64_t flags,
                                       uint32_t capacity, hl_x86_a64_result *result) {
    hl_x86_a64_provenance provenance[2] = {0};
    hl_x86_a64_request request = {
        .abi = HL_X86_A64_FRONTEND_ABI, .size = sizeof request,
        .guest_pc = UINT64_C(0x400000), .guest_bytes = guest, .guest_size = size,
        .max_instructions = 1u, .host_words = host, .host_capacity = capacity,
        .provenance = provenance, .provenance_capacity = 2u, .flags = flags,
    };
    return hl_x86_a64_emit(&request, result);
}

static hl_x86_a64_status emit(const uint8_t *guest, size_t size, uint64_t flags,
                              hl_x86_a64_result *result) {
    return emit_capacity(guest, size, flags, 512u, result);
}

static int execute(const uint8_t *guest, size_t size, uint64_t flags,
                   hl_native_x86_64_cpu *cpu) {
#if !defined(__aarch64__)
    (void)guest; (void)size; (void)flags; (void)cpu;
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code;
    hl_x86_a64_result result;
    CHECK(page > 0);
    code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(emit(guest, size, flags, &result) == HL_X86_A64_OK);
    CHECK((result.word_count + 1u) * sizeof(uint32_t) <= (size_t)page);
    memcpy(code, host, result.word_count * sizeof(uint32_t));
    ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
    __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    hl_x86_test_enter(cpu, code);
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int runtime(void) {
#if defined(__aarch64__)
    static const uint8_t byte[] = {0x0fu, 0xc0u, 0xe0u};
    static const uint8_t word_alias[] = {0x66u, 0x0fu, 0xc1u, 0xc0u};
    static const uint8_t dword[] = {0x0fu, 0xc1u, 0xc8u};
    static const uint8_t rex[] = {0x4du, 0x0fu, 0xc1u, 0xc8u};
    static const uint8_t memory[] = {0x48u, 0x0fu, 0xc1u, 0x08u};
    static const uint8_t locked[] = {0xf0u, 0x48u, 0x0fu, 0xc1u, 0x08u};
    static const uint8_t locked_widths[][5] = {
        {0xf0u, 0x0fu, 0xc0u, 0x08u},
        {0xf0u, 0x66u, 0x0fu, 0xc1u, 0x08u},
        {0xf0u, 0x0fu, 0xc1u, 0x08u},
        {0xf0u, 0x48u, 0x0fu, 0xc1u, 0x08u},
    };
    static const size_t locked_sizes[] = {4u, 5u, 4u, 5u};
    static const uint8_t widths[] = {1u, 2u, 4u, 8u};
    _Alignas(8) uint8_t bytes[24] = {0};
    hl_native_x86_64_cpu cpu = {0};
    uint64_t value;

    cpu.registers[0] = UINT64_C(0x1122334455667f01);
    cpu.flags = 2u;
    CHECK(execute(byte, sizeof byte, 0u, &cpu) == 0);
    CHECK(cpu.registers[0] == UINT64_C(0x1122334455660180));
    CHECK(cpu.flags == UINT64_C(0x892));

    cpu.registers[0] = UINT64_C(0xaaaabbbbcccc8000);
    cpu.flags = 2u;
    CHECK(execute(word_alias, sizeof word_alias, 0u, &cpu) == 0);
    CHECK(cpu.registers[0] == UINT64_C(0xaaaabbbbcccc0000));
    CHECK(cpu.flags == UINT64_C(0x847));

    cpu.registers[0] = UINT64_C(0xffffffff);
    cpu.registers[1] = 1u;
    cpu.flags = 2u;
    CHECK(execute(dword, sizeof dword, 0u, &cpu) == 0);
    CHECK(cpu.registers[0] == 0u && cpu.registers[1] == UINT64_C(0xffffffff));
    CHECK(cpu.flags == UINT64_C(0x57));

    cpu.registers[8] = UINT64_C(0x8000000000000000);
    cpu.registers[9] = UINT64_C(0x8000000000000000);
    cpu.flags = 2u;
    CHECK(execute(rex, sizeof rex, 0u, &cpu) == 0);
    CHECK(cpu.registers[8] == 0u && cpu.registers[9] == UINT64_C(0x8000000000000000));
    CHECK(cpu.flags == UINT64_C(0x847));

    memcpy(bytes + 8, &(uint64_t){5u}, 8u);
    memset(&cpu, 0, sizeof cpu);
    cpu.registers[0] = UINT64_C(0x1008); cpu.registers[1] = 7u; cpu.flags = 2u;
    cpu.memory_first = UINT64_C(0x1000); cpu.memory_last = UINT64_C(0x1018);
    cpu.memory_delta = (uint64_t)(uintptr_t)bytes - UINT64_C(0x1000);
    cpu.memory_permissions = 7u; cpu.dirty_first = UINT64_MAX;
    CHECK(execute(memory, sizeof memory, HL_X86_A64_LSE, &cpu) == 0);
    memcpy(&value, bytes + 8, 8u);
    CHECK(value == 12u && cpu.registers[1] == 5u);
    CHECK(cpu.flags == 6u);
    CHECK(cpu.memory_written == 1u && cpu.dirty_first == UINT64_C(0x1008) &&
          cpu.dirty_last == UINT64_C(0x1010) && cpu.executable_written == 7u);

    for (size_t index = 0; index < sizeof widths / sizeof widths[0]; ++index) {
        uint64_t mask = widths[index] == 8u ? UINT64_MAX :
                        (UINT64_C(1) << (widths[index] * 8u)) - 1u;
        memcpy(bytes + 8, &(uint64_t){5u}, 8u);
        memset(&cpu, 0, sizeof cpu);
        cpu.registers[0] = UINT64_C(0x1008); cpu.registers[1] = 7u; cpu.flags = 2u;
        cpu.memory_first = UINT64_C(0x1000); cpu.memory_last = UINT64_C(0x1018);
        cpu.memory_delta = (uint64_t)(uintptr_t)bytes - UINT64_C(0x1000);
        cpu.memory_permissions = 7u; cpu.dirty_first = UINT64_MAX;
        CHECK(execute(locked_widths[index], locked_sizes[index], HL_X86_A64_LSE, &cpu) == 0);
        memcpy(&value, bytes + 8, 8u);
        CHECK((value & mask) == 12u && (cpu.registers[1] & mask) == 5u);
        CHECK(cpu.flags == 6u && cpu.memory_written == 1u);
        CHECK(cpu.dirty_first == UINT64_C(0x1008) &&
              cpu.dirty_last == UINT64_C(0x1008) + widths[index]);
        CHECK(cpu.dirty_view_first == UINT64_C(0x1000) &&
              cpu.dirty_view_last == UINT64_C(0x1018) && cpu.executable_written == 7u);
    }

    memcpy(bytes + 8, &(uint64_t){11u}, 8u);
    cpu.registers[1] = 4u; cpu.memory_permissions = 1u; cpu.flags = UINT64_C(0xad6);
    cpu.memory_written = 0u; cpu.dirty_first = UINT64_MAX; cpu.dirty_last = 0u;
    CHECK(execute(locked, sizeof locked, HL_X86_A64_LSE, &cpu) == 0);
    memcpy(&value, bytes + 8, 8u);
    CHECK(value == 11u && cpu.registers[1] == 4u && cpu.flags == UINT64_C(0xad6));
    CHECK(cpu.memory_written == 0u && cpu.dirty_first == UINT64_MAX);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_address == UINT64_C(0x1008) &&
          cpu.fault_access == 3u && cpu.fault_size == 8u && cpu.program == UINT64_C(0x400000));

    cpu.registers[0] = UINT64_C(0x1009); cpu.memory_permissions = 3u;
    cpu.reason = 0u; cpu.fault_access = 0u; cpu.memory_written = 0u;
    cpu.dirty_first = UINT64_MAX; cpu.dirty_last = 0u; cpu.flags = UINT64_C(0xad6);
    CHECK(execute(locked, sizeof locked, HL_X86_A64_LSE, &cpu) == 0);
    memcpy(&value, bytes + 8, 8u);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == 0u);
    CHECK(value == 11u && cpu.registers[1] == 4u && cpu.memory_written == 0u &&
          cpu.flags == UINT64_C(0xad6) && cpu.dirty_first == UINT64_MAX);
#endif
    return 0;
}

int main(void) {
    static const uint8_t register_forms[][5] = {
        {0x0fu, 0xc0u, 0xe0u},                 /* xadd ah,al */
        {0x66u, 0x0fu, 0xc1u, 0xc0u},         /* aliasing xadd ax,ax */
        {0x0fu, 0xc1u, 0xc8u},                /* xadd ecx,eax */
        {0x4du, 0x0fu, 0xc1u, 0xc8u},         /* REX.R/B xadd r9,r8 */
    };
    static const size_t sizes[] = {3u, 4u, 3u, 4u};
    static const uint8_t memory[] = {0x48u, 0x0fu, 0xc1u, 0x08u};
    static const uint8_t locked[] = {0xf0u, 0x48u, 0x0fu, 0xc1u, 0x08u};
    static const uint8_t bad_lock[] = {0xf0u, 0x0fu, 0xc1u, 0xc8u};
    hl_x86_a64_result result;
    size_t index;

    for (index = 0; index < sizeof sizes / sizeof sizes[0]; ++index) {
        CHECK(emit(register_forms[index], sizes[index], 0u, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1u);
    }
    CHECK(emit(memory, sizeof memory, HL_X86_A64_LSE, &result) == HL_X86_A64_OK);
    CHECK(emit(locked, sizeof locked, HL_X86_A64_LSE, &result) == HL_X86_A64_OK);
    CHECK(emit(memory, sizeof memory, 0u, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(emit(bad_lock, sizeof bad_lock, HL_X86_A64_LSE, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(emit_capacity(locked, sizeof locked, HL_X86_A64_LSE,
                        result.word_count - 1u, &result) == HL_X86_A64_CAPACITY);
    return runtime();
}
