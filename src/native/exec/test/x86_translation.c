#include "../src/arch/x86_64/frontend.h"
#include "../src/arch/x86_64/decode.h"
#include "../src/arch/x86_64/entry.h"
#include "../src/arch/x86_64/flags.h"
#include "../src/arch/x86_64/word.h"
#include "../src/arch/x86_64/frontend/private.h"
#include "../include/cpu.h"
#include "../include/executor.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "x86_translation:%d: %s\n", __LINE__, #expression);                                      \
            return __LINE__;                                                                                           \
        }                                                                                                              \
    } while (0)

#if defined(__aarch64__)
extern void hl_x86_test_enter(hl_native_x86_64_cpu *, void *);
extern void hl_x86_test_preserved(hl_native_x86_64_cpu *, void *, const void *, void *);
#endif

static hl_x86_a64_request request_for(const uint8_t *guest, size_t size, uint32_t *host,
                                      hl_x86_a64_provenance *provenance) {
    hl_x86_a64_request request;

    memset(&request, 0, sizeof request);
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = UINT64_C(0x400000);
    request.guest_bytes = guest;
    request.guest_size = size;
    request.max_instructions = HL_X86_A64_MAX_INSTRUCTIONS;
    request.host_words = host;
    request.host_capacity = 256;
    request.provenance = provenance;
    request.provenance_capacity = 8;
    request.flags = HL_X86_A64_LSE;
    return request;
}

static int straight_line(void) {
    const uint8_t guest[] = {0x48, 0xb8, 0x78, 0x56, 0x34, 0x12, 0, 0, 0, 0, 0x90};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH);
    CHECK(result.instruction_count == 2);
    CHECK(result.exit_pc == UINT64_C(0x40000b));
    CHECK(provenance[0].guest_pc == UINT64_C(0x400000));
    CHECK(provenance[0].guest_size == 10);
    CHECK(provenance[0].word_end > provenance[0].word_start);
    CHECK(provenance[1].guest_size == 1);
    CHECK(provenance[1].word_end == provenance[1].word_start);
    return 0;
}

static void packed_expected(uint8_t output[16], const uint8_t left[16],
                            const uint8_t right[16], unsigned width, int subtract) {
    unsigned offset;
    for (offset = 0; offset < 16u; offset += width) {
        uint64_t first = 0;
        uint64_t second = 0;
        uint64_t value;
        memcpy(&first, left + offset, width);
        memcpy(&second, right + offset, width);
        value = subtract != 0 ? first - second : first + second;
        memcpy(output + offset, &value, width);
    }
}

static int packed_integer_arithmetic(void) {
    static const uint8_t guest[] = {
        0x66, 0x0f, 0xfc, 0xc1, /* paddb xmm0,xmm1 */
        0x66, 0x0f, 0xfd, 0xc2, /* paddw xmm0,xmm2 */
        0x66, 0x0f, 0xfe, 0xc3, /* paddd xmm0,xmm3 */
        0x66, 0x0f, 0xd4, 0xc4, /* paddq xmm0,xmm4 */
        0x66, 0x0f, 0xf8, 0xc1, /* psubb xmm0,xmm1 */
        0x66, 0x0f, 0xf9, 0xc2, /* psubw xmm0,xmm2 */
        0x66, 0x0f, 0xfa, 0xc3, /* psubd xmm0,xmm3 */
        0x66, 0x0f, 0xfb, 0xc4, /* psubq xmm0,xmm4 */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;
    unsigned index;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.instruction_count == 8);
    for (index = 0; index < 8u; ++index) {
        CHECK(provenance[index].guest_size == 4);
        CHECK(provenance[index].word_end > provenance[index].word_start);
    }
    {
        static const uint8_t memory[] = {
            0x66, 0x0f, 0xfe, 0x03,       /* paddd xmm0,[rbx] */
            0x66, 0x0f, 0xfb, 0x4b, 0x10, /* psubq xmm1,[rbx+16] */
        };
        uint32_t memory_host[256] = {0};
        hl_x86_a64_provenance memory_provenance[8] = {0};
        hl_x86_a64_request memory_request =
            request_for(memory, sizeof memory, memory_host, memory_provenance);
        hl_x86_a64_result memory_result;
        CHECK(hl_x86_a64_emit(&memory_request, &memory_result) == HL_X86_A64_OK);
        CHECK(memory_result.exit == HL_X86_A64_FALLTHROUGH && memory_result.instruction_count == 2);
        CHECK(memory_provenance[0].guest_size == 4 && memory_provenance[1].guest_size == 5);
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0xc4, 0xe2, 0x6d, 0x28, 0x03};
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0}; uint8_t data[32] = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[3] = 0x4000; cpu.memory_first = 0x4000; cpu.memory_last = 0x401f;
        cpu.memory_delta = (uint64_t)(uintptr_t)data - 0x4000; cpu.memory_permissions = 1u;
        cpu.vectors[0] = UINT64_C(0xfeedfacecafebeef);
        cpu.vector_upper[0] = UINT64_C(0x0123456789abcdef);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == HL_NATIVE_ACCESS_READ);
        CHECK(cpu.fault_address == 0x4000 && cpu.fault_size == 32u);
        CHECK(cpu.vectors[0] == UINT64_C(0xfeedfacecafebeef));
        CHECK(cpu.vector_upper[0] == UINT64_C(0x0123456789abcdef));
        CHECK(munmap(code, (size_t)page) == 0);
    }
    {
        static const uint8_t opcodes[] = {0xfc, 0xfd, 0xfe, 0xd4, 0xf8, 0xf9, 0xfa, 0xfb};
        static const uint8_t widths[] = {1, 2, 4, 8, 1, 2, 4, 8};
        static const uint8_t left[16] = {
            0xff, 0x7f, 0x00, 0x80, 0xfe, 0xff, 0xff, 0x7f,
            0x01, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff,
        };
        static const uint8_t right[16] = {
            0x02, 0x01, 0xff, 0x80, 0x03, 0x00, 0x01, 0x80,
            0xff, 0xff, 0x00, 0x80, 0x01, 0x00, 0x00, 0x00,
        };
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        unsigned operation;
        CHECK(code != MAP_FAILED);
        for (operation = 0; operation < 8u; ++operation) {
            uint8_t one[] = {0x66, 0x0f, opcodes[operation], 0xc1};
            uint32_t one_host[256] = {0};
            hl_x86_a64_provenance one_provenance[8] = {0};
            hl_x86_a64_request one_request = request_for(one, sizeof one, one_host, one_provenance);
            hl_x86_a64_result one_result;
            hl_native_x86_64_cpu cpu = {0};
            uint8_t expected[16];
            CHECK(hl_x86_a64_emit(&one_request, &one_result) == HL_X86_A64_OK);
            memcpy(code, one_host, one_result.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[one_result.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (one_result.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            memcpy(&cpu.vectors[0], left, sizeof left);
            memcpy(&cpu.vectors[2], right, sizeof right);
            cpu.flags = UINT64_C(0xad7);
            cpu.mxcsr = UINT32_C(0x5f80);
            packed_expected(expected, left, right, widths[operation], operation >= 4u);
            hl_native_x86_64_enter(&cpu, code);
            CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
            CHECK(memcmp(&cpu.vectors[2], right, sizeof right) == 0);
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
        { /* Destination/source aliasing remains destructive and lane-local. */
            static const uint8_t alias[] = {0x66, 0x0f, 0xfe, 0xc0};
            uint32_t alias_host[256] = {0};
            hl_x86_a64_provenance alias_provenance[8] = {0};
            hl_x86_a64_request alias_request = request_for(alias, sizeof alias, alias_host, alias_provenance);
            hl_x86_a64_result alias_result;
            hl_native_x86_64_cpu cpu = {0};
            uint8_t expected[16];
            CHECK(hl_x86_a64_emit(&alias_request, &alias_result) == HL_X86_A64_OK);
            memcpy(code, alias_host, alias_result.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[alias_result.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (alias_result.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            memcpy(&cpu.vectors[0], left, sizeof left);
            packed_expected(expected, left, left, 4u, 0);
            hl_native_x86_64_enter(&cpu, code);
            CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
        { /* Guarded memory permits unaligned operands and faults before commit. */
            static const uint8_t memory[] = {0x66, 0x0f, 0xfe, 0x03};
            _Alignas(16) uint8_t storage[32] = {0};
            uint32_t memory_host[256] = {0};
            hl_x86_a64_provenance memory_provenance[8] = {0};
            hl_x86_a64_request memory_request =
                request_for(memory, sizeof memory, memory_host, memory_provenance);
            hl_x86_a64_result memory_result;
            hl_native_x86_64_cpu cpu = {0};
            uint8_t expected[16];
            CHECK(hl_x86_a64_emit(&memory_request, &memory_result) == HL_X86_A64_OK);
            memcpy(code, memory_host, memory_result.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[memory_result.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (memory_result.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            memcpy(storage + 1, right, sizeof right);
            memcpy(&cpu.vectors[0], left, sizeof left);
            cpu.registers[3] = UINT64_C(0x2001);
            cpu.memory_first = UINT64_C(0x2000);
            cpu.memory_last = UINT64_C(0x2020);
            cpu.memory_delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x2000);
            cpu.memory_permissions = 1u;
            cpu.flags = UINT64_C(0xad7);
            cpu.mxcsr = UINT32_C(0x5f80);
            packed_expected(expected, left, right, 4u, 0);
            hl_native_x86_64_enter(&cpu, code);
            CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
            memcpy(&cpu.vectors[0], left, sizeof left);
            cpu.reason = cpu.fault_access = cpu.fault_size = 0;
            cpu.memory_last = UINT64_C(0x2010);
            hl_native_x86_64_enter(&cpu, code);
            CHECK(memcmp(&cpu.vectors[0], left, sizeof left) == 0);
            CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == 1 && cpu.fault_size == 16);
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    { /* The same opcode bytes without 0x66 remain MMX and are not mis-admitted. */
        static const uint8_t mmx[] = {0x0f, 0xfe, 0xc1};
        uint32_t mmx_host[256] = {0};
        hl_x86_a64_provenance mmx_provenance[8] = {0};
        hl_x86_a64_request mmx_request = request_for(mmx, sizeof mmx, mmx_host, mmx_provenance);
        hl_x86_a64_result mmx_result;
        CHECK(hl_x86_a64_emit(&mmx_request, &mmx_result) == HL_X86_A64_UNSUPPORTED);
        CHECK(mmx_result.instruction_count == 0);
    }
    return 0;
}

static void packed_multiply_expected(uint8_t output[16], const uint8_t left[16],
                                     const uint8_t right[16], uint8_t opcode) {
    unsigned lane;
    memset(output, 0, 16);
    if (opcode == 0xf4u) {
        for (lane = 0; lane < 2u; ++lane) {
            uint32_t first;
            uint32_t second;
            uint64_t product;
            memcpy(&first, left + lane * 8u, sizeof first);
            memcpy(&second, right + lane * 8u, sizeof second);
            product = (uint64_t)first * (uint64_t)second;
            memcpy(output + lane * 8u, &product, sizeof product);
        }
        return;
    }
    for (lane = 0; lane < 8u; ++lane) {
        uint16_t first;
        uint16_t second;
        uint32_t product;
        uint16_t result;
        memcpy(&first, left + lane * 2u, sizeof first);
        memcpy(&second, right + lane * 2u, sizeof second);
        if (opcode == 0xe5u)
            product = (uint32_t)((int32_t)(int16_t)first * (int32_t)(int16_t)second);
        else
            product = (uint32_t)first * (uint32_t)second;
        result = (uint16_t)(opcode == 0xd5u ? product : product >> 16);
        memcpy(output + lane * 2u, &result, sizeof result);
    }
}

static int packed_integer_multiply(void) {
    static const uint8_t guest[] = {
        0x66, 0x0f, 0xd5, 0xc1, /* pmullw xmm0,xmm1 */
        0x66, 0x0f, 0xe4, 0xc2, /* pmulhuw xmm0,xmm2 */
        0x66, 0x0f, 0xe5, 0xc3, /* pmulhw xmm0,xmm3 */
        0x66, 0x0f, 0xf4, 0xc4, /* pmuludq xmm0,xmm4 */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;
    unsigned index;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.instruction_count == 4);
    for (index = 0; index < 4u; ++index)
        CHECK(provenance[index].guest_size == 4 && provenance[index].word_end > provenance[index].word_start);
    for (index = 0; index < 4u; ++index) {
        static const uint8_t opcodes[] = {0xd5, 0xe4, 0xe5, 0xf4};
        uint8_t memory[] = {0x66, 0x0f, opcodes[index], 0x4b, 0x10};
        uint32_t memory_host[256] = {0};
        hl_x86_a64_provenance memory_provenance[8] = {0};
        hl_x86_a64_request memory_request = request_for(memory, sizeof memory, memory_host, memory_provenance);
        hl_x86_a64_result memory_result;
        CHECK(hl_x86_a64_emit(&memory_request, &memory_result) == HL_X86_A64_OK);
        CHECK(memory_result.instruction_count == 1 && memory_provenance[0].guest_size == 5);
    }
    { /* VEX forms are three-operand, accept both widths, and include full-width memory. */
        static const uint8_t vex_opcodes[] = {0xd5, 0xe4, 0xe5, 0xf4};
        static const uint8_t vex_guest[] = {
            0xc5, 0xe9, 0xd5, 0xcb, /* vpmullw xmm1,xmm2,xmm3 */
            0xc5, 0xe9, 0xe4, 0xcb, /* vpmulhuw xmm1,xmm2,xmm3 */
            0xc5, 0xe9, 0xe5, 0xcb, /* vpmulhw xmm1,xmm2,xmm3 */
            0xc5, 0xe9, 0xf4, 0xcb, /* vpmuludq xmm1,xmm2,xmm3 */
            0xc5, 0xed, 0xd5, 0xcb, /* vpmullw ymm1,ymm2,ymm3 */
            0xc5, 0xed, 0xe4, 0xcb, /* vpmulhuw ymm1,ymm2,ymm3 */
            0xc5, 0xed, 0xe5, 0xcb, /* vpmulhw ymm1,ymm2,ymm3 */
            0xc5, 0xed, 0xf4, 0xcb, /* vpmuludq ymm1,ymm2,ymm3 */
        };
        uint32_t vex_host[512] = {0};
        hl_x86_a64_provenance vex_provenance[8] = {0};
        hl_x86_a64_request vex_request =
            request_for(vex_guest, sizeof vex_guest, vex_host, vex_provenance);
        hl_x86_a64_result vex_result;
        CHECK(hl_x86_a64_emit(&vex_request, &vex_result) == HL_X86_A64_OK);
        CHECK(vex_result.instruction_count == 8 && vex_result.exit == HL_X86_A64_FALLTHROUGH);
        for (index = 0; index < 8u; ++index)
            CHECK(vex_provenance[index].guest_size == 4 &&
                  vex_provenance[index].word_end > vex_provenance[index].word_start);
        for (index = 0; index < 4u; ++index) {
            uint8_t memory[] = {0xc5, 0xed, 0xd5, 0x4b, 0x10};
            uint32_t memory_host[256] = {0};
            hl_x86_a64_provenance memory_provenance[2] = {0};
            hl_x86_a64_request memory_request;
            hl_x86_a64_result memory_result;
            memory[2] = vex_opcodes[index];
            memory_request = request_for(memory, sizeof memory, memory_host, memory_provenance);
            CHECK(hl_x86_a64_emit(&memory_request, &memory_result) == HL_X86_A64_OK);
            CHECK(memory_result.instruction_count == 1 && memory_provenance[0].guest_size == 5);
        }
        for (index = 0; index < 8u; ++index) { /* Exact sizing is part of the emitter contract. */
            uint8_t one[] = {0xc5, index < 4u ? 0xe9 : 0xed,
                             vex_opcodes[index & 3u], 0xcb};
            uint32_t sized_host[256] = {0};
            hl_x86_a64_provenance sized_provenance[8] = {0};
            hl_x86_a64_request sized_request =
                request_for(one, sizeof one, sized_host, sized_provenance);
            hl_x86_a64_result sized_result;
            uint32_t required;

            CHECK(hl_x86_a64_emit(&sized_request, &sized_result) == HL_X86_A64_OK);
            required = sized_result.word_count;
            memset(sized_host, 0xa5, sizeof sized_host);
            memset(sized_provenance, 0xa5, sizeof sized_provenance);
            sized_request.host_capacity = required;
            CHECK(hl_x86_a64_emit(&sized_request, &sized_result) == HL_X86_A64_OK);
            CHECK(sized_result.word_count == required);
            memset(sized_host, 0xa5, sizeof sized_host);
            memset(sized_provenance, 0xa5, sizeof sized_provenance);
            sized_request.host_capacity = required - 1u;
            CHECK(hl_x86_a64_emit(&sized_request, &sized_result) == HL_X86_A64_CAPACITY);
            CHECK(sized_host[0] == UINT32_C(0xa5a5a5a5));
            CHECK(sized_host[required] == UINT32_C(0xa5a5a5a5));
            CHECK(sized_provenance[0].guest_pc == UINT64_C(0xa5a5a5a5a5a5a5a5));
        }
    }
#if defined(__aarch64__)
    {
        static const uint8_t opcodes[] = {0xd5, 0xe4, 0xe5, 0xf4};
        static const uint8_t left[16] = {
            0xff, 0x7f, 0x00, 0x80, 0xff, 0xff, 0x01, 0x00,
            0x34, 0x12, 0xcc, 0xed, 0x01, 0x80, 0xfe, 0x7f,
        };
        static const uint8_t right[16] = {
            0x02, 0x00, 0x02, 0x00, 0xff, 0xff, 0xff, 0xff,
            0x78, 0x56, 0x22, 0x11, 0x00, 0x80, 0x02, 0x00,
        };
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED);
        for (index = 0; index < 4u; ++index) {
            uint8_t one[] = {0x66, 0x0f, opcodes[index], 0xc1};
            uint32_t one_host[256] = {0};
            hl_x86_a64_provenance one_provenance[8] = {0};
            hl_x86_a64_request one_request = request_for(one, sizeof one, one_host, one_provenance);
            hl_x86_a64_result one_result;
            hl_native_x86_64_cpu cpu = {0};
            uint8_t expected[16];
            CHECK(hl_x86_a64_emit(&one_request, &one_result) == HL_X86_A64_OK);
            memcpy(code, one_host, one_result.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[one_result.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (one_result.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            memcpy(&cpu.vectors[0], left, sizeof left);
            memcpy(&cpu.vectors[2], right, sizeof right);
            cpu.flags = UINT64_C(0xad7);
            cpu.mxcsr = UINT32_C(0x5f80);
            packed_multiply_expected(expected, left, right, opcodes[index]);
            hl_native_x86_64_enter(&cpu, code);
            CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
            CHECK(memcmp(&cpu.vectors[2], right, sizeof right) == 0);
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            { /* Every opcode also executes through the guarded unaligned-memory path. */
                uint8_t memory[] = {0x66, 0x0f, opcodes[index], 0x03};
                _Alignas(16) uint8_t storage[32] = {0};
                uint32_t memory_host[256] = {0};
                hl_x86_a64_provenance memory_provenance[8] = {0};
                hl_x86_a64_request memory_request =
                    request_for(memory, sizeof memory, memory_host, memory_provenance);
                hl_x86_a64_result memory_result;
                memset(&cpu, 0, sizeof cpu);
                CHECK(hl_x86_a64_emit(&memory_request, &memory_result) == HL_X86_A64_OK);
                memcpy(code, memory_host, memory_result.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[memory_result.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (memory_result.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                memcpy(storage + 1, right, sizeof right);
                memcpy(&cpu.vectors[0], left, sizeof left);
                cpu.registers[3] = UINT64_C(0x2001);
                cpu.memory_first = UINT64_C(0x2000);
                cpu.memory_last = UINT64_C(0x2020);
                cpu.memory_delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x2000);
                cpu.memory_permissions = 1u;
                cpu.flags = UINT64_C(0xad7);
                cpu.mxcsr = UINT32_C(0x5f80);
                hl_native_x86_64_enter(&cpu, code);
                CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
                CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
        for (index = 0; index < 8u; ++index) { /* VEX non-destructive and upper-lane contract. */
            uint8_t one[] = {0xc5, index < 4u ? 0xe9 : 0xed,
                             opcodes[index & 3u], 0xcb};
            uint32_t one_host[256] = {0};
            hl_x86_a64_provenance one_provenance[2] = {0};
            hl_x86_a64_request one_request =
                request_for(one, sizeof one, one_host, one_provenance);
            hl_x86_a64_result one_result;
            hl_native_x86_64_cpu cpu = {0};
            uint8_t expected_low[16];
            uint8_t expected_high[16];
            uint8_t zero[16] = {0};
            CHECK(hl_x86_a64_emit(&one_request, &one_result) == HL_X86_A64_OK);
            memcpy(code, one_host, one_result.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[one_result.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code,
                                    (char *)code + (one_result.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            memset(&cpu.vectors[2], 0xa5, 16);
            memset(&cpu.vector_upper[2], 0xa5, 16);
            memcpy(&cpu.vectors[4], left, 16);
            memcpy(&cpu.vector_upper[4], right, 16);
            memcpy(&cpu.vectors[6], right, 16);
            memcpy(&cpu.vector_upper[6], left, 16);
            cpu.flags = UINT64_C(0xad7);
            cpu.mxcsr = UINT32_C(0x5f80);
            packed_multiply_expected(expected_low, left, right, opcodes[index & 3u]);
            packed_multiply_expected(expected_high, right, left, opcodes[index & 3u]);
            hl_native_x86_64_enter(&cpu, code);
            CHECK(memcmp(&cpu.vectors[2], expected_low, 16) == 0);
            CHECK(memcmp(&cpu.vector_upper[2], index < 4u ? zero : expected_high, 16) == 0);
            CHECK(memcmp(&cpu.vectors[4], left, 16) == 0);
            CHECK(memcmp(&cpu.vectors[6], right, 16) == 0);
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    { /* Unprefixed encodings are MMX and stay outside this XMM frontend. */
        static const uint8_t mmx[] = {0x0f, 0xd5, 0xc1};
        uint32_t mmx_host[256] = {0};
        hl_x86_a64_provenance mmx_provenance[8] = {0};
        hl_x86_a64_request mmx_request = request_for(mmx, sizeof mmx, mmx_host, mmx_provenance);
        hl_x86_a64_result mmx_result;
        CHECK(hl_x86_a64_emit(&mmx_request, &mmx_result) == HL_X86_A64_UNSUPPORTED);
        CHECK(mmx_result.instruction_count == 0);
    }
    return 0;
}

static unsigned pcmp_length(const uint8_t value[16]) {
    unsigned index;
    for (index = 0; index < 16u; ++index)
        if (value[index] == 0u) return index;
    return 16u;
}

static unsigned pcmp_equal_each(const uint8_t left[16], const uint8_t right[16],
                                uint8_t immediate) {
    unsigned left_length = pcmp_length(left);
    unsigned right_length = pcmp_length(right);
    unsigned result = 0;
    unsigned index;
    for (index = 0; index < 16u; ++index) {
        int equal = index < left_length && index < right_length
                        ? left[index] == right[index]
                        : index >= left_length && index >= right_length;
        if (equal) result |= 1u << index;
    }
    if ((immediate & 0x10u) != 0u)
        result ^= (immediate & 0x20u) != 0u
                      ? (1u << right_length) - 1u : UINT32_C(0xffff);
    return result & UINT32_C(0xffff);
}

static const struct {
    uint8_t left[16];
    uint8_t right[16];
    unsigned expected;
} pcmp_boundary_cases[] = {
    /* exact, no match, and mismatches before, at, and after a terminator */
    {{1, 2, 3, 0}, {1, 2, 3, 0}, UINT32_C(0xffff)},
    {{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16},
     {17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32},
     UINT32_C(0x0000)},
    {{1, 2, 3, 0}, {1, 9, 3, 0}, UINT32_C(0xfffd)},
    {{1, 2, 0}, {1, 2, 9, 0}, UINT32_C(0xfffb)},
    {{1, 2, 0, 4}, {1, 2, 0, 9}, UINT32_C(0xffff)},
};

static int pcmp_length_boundaries(void) {
    unsigned index;
    for (index = 0; index < sizeof pcmp_boundary_cases / sizeof pcmp_boundary_cases[0]; ++index)
        CHECK(pcmp_equal_each(pcmp_boundary_cases[index].left,
                              pcmp_boundary_cases[index].right, 0x08) ==
              pcmp_boundary_cases[index].expected);
    return 0;
}

/* PCMPISTRI once used w26 as scratch, silently corrupting the live instruction
 * budget the block prologue keeps there and faulting the run as X86_FATAL_BUDGET. */
static int pcmpistri_preserves_budget(void) {
    uint8_t guest[] = {0x66, 0x0f, 0x3a, 0x63, 0xc1, 0x1a};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;
    uint32_t index;

    request.flags |= HL_X86_A64_CHECKPOINTS | HL_X86_A64_LIVE_CHAIN;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1);
    for (index = 0; index < result.word_count; ++index) {
        uint32_t word = host[index];
        /* Only the checkpoint's own `sub x26,x26,#delta` may name x26. */
        if ((word & UINT32_C(0x1f)) == 26u) CHECK((word & UINT32_C(0xffc003ff)) == UINT32_C(0xd100035a));
    }
#if defined(__aarch64__)
    {
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        uint32_t *words = (uint32_t *)code;
        uint32_t cursor = 0;
        hl_native_x86_64_cpu cpu = {0};
        static const uint8_t left[16] = {1, 2, 3, 4, 5, 6, 7, 0, 9, 10, 11, 12, 13, 14, 15, 16};

        CHECK(code != MAP_FAILED);
        words[cursor++] = load_word(26u, offsetof(hl_native_x86_64_cpu, budget));
        memcpy(words + cursor, host, result.word_count * sizeof(uint32_t));
        cursor += result.word_count;
        words[cursor++] = load_word(16u, offsetof(hl_native_x86_64_cpu, budget));
        words[cursor++] = UINT32_C(0xcb1a0210); /* sub x16,x16,x26 */
        words[cursor++] = store_word(16u, offsetof(hl_native_x86_64_cpu, scratch));
        words[cursor++] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + cursor * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[0], left, sizeof left);
        memcpy(&cpu.vectors[2], left, sizeof left);
        cpu.budget = UINT64_C(65536);
        cpu.flags = UINT64_C(0xad7);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(cpu.scratch[0] <= result.instruction_count);
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int pcmpistri_equal_each(void) {
    static const uint8_t controls[] = {
        0x08, 0x0a, 0x18, 0x1a, 0x28, 0x2a, 0x38, 0x3a,
        0x48, 0x4a, 0x58, 0x5a, 0x68, 0x6a, 0x78, 0x7a,
    };
    const uint64_t changed = HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF |
                             HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF | HL_X86_RFLAGS_OF;
    unsigned control;
    uint32_t maximum_words = 0;

    CHECK(pcmp_length_boundaries() == 0);

    for (control = 0; control < sizeof controls; ++control) {
        uint8_t guest[] = {0x66, 0x0f, 0x3a, 0x63, 0xc1, controls[control]};
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_x86_a64_result result;

        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof guest);
        if (provenance[0].word_end - provenance[0].word_start > maximum_words)
            maximum_words = provenance[0].word_end - provenance[0].word_start;
        CHECK(provenance[0].word_end - provenance[0].word_start <= 96u);
#if defined(__aarch64__)
        {
            long page = sysconf(_SC_PAGESIZE);
            uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            unsigned left_length;
            CHECK(code != MAP_FAILED);
            memcpy(code, host, result.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            for (left_length = 0; left_length <= 16u; ++left_length) {
                unsigned right_length;
                for (right_length = 0; right_length <= 16u; ++right_length) {
                    hl_native_x86_64_cpu cpu = {0};
                    uint8_t left[16];
                    uint8_t right[16];
                    uint64_t initial_flags = UINT64_C(0xad7) | UINT64_C(0x200);
                    uint64_t flags = initial_flags & ~changed;
                    unsigned expected;
                    unsigned expected_index;
                    unsigned index;
                    unsigned mismatch;
                    for (index = 0; index < 16u; ++index) {
                        left[index] = (uint8_t)(index + 1u);
                        right[index] = (uint8_t)(index + 1u);
                    }
                    if (left_length < 16u) left[left_length] = 0u;
                    if (right_length < 16u) right[right_length] = 0u;
                    mismatch = (left_length * 5u + right_length * 3u) & 15u;
                    while (mismatch == left_length || mismatch == right_length)
                        mismatch = (mismatch + 1u) & 15u;
                    right[mismatch] ^= UINT8_C(0x55);
                    expected = pcmp_equal_each(left, right, controls[control]);
                    expected_index = expected == 0u ? 16u :
                        (controls[control] & 0x40u) != 0u
                            ? 31u - (unsigned)__builtin_clz(expected)
                            : (unsigned)__builtin_ctz(expected);
                    if (expected != 0u) flags |= HL_X86_RFLAGS_CF;
                    if (pcmp_length(right) < 16u) flags |= HL_X86_RFLAGS_ZF;
                    if (pcmp_length(left) < 16u) flags |= HL_X86_RFLAGS_SF;
                    if ((expected & 1u) != 0u) flags |= HL_X86_RFLAGS_OF;
                    memcpy(&cpu.vectors[0], left, sizeof left);
                    memcpy(&cpu.vectors[2], right, sizeof right);
                    cpu.registers[1] = UINT64_C(0xfeedface);
                    cpu.flags = initial_flags;
                    cpu.mxcsr = UINT32_C(0x5f80);
                    hl_native_x86_64_enter(&cpu, code);
                    CHECK(cpu.registers[1] == expected_index);
                    CHECK(cpu.flags == flags);
                    CHECK(memcmp(&cpu.vectors[0], left, sizeof left) == 0);
                    CHECK(memcmp(&cpu.vectors[2], right, sizeof right) == 0);
                    CHECK(cpu.mxcsr == UINT32_C(0x5f80));
                }
            }
            if (controls[control] == 0x08u || controls[control] == 0x48u) {
                unsigned boundary;
                for (boundary = 0;
                     boundary < sizeof pcmp_boundary_cases / sizeof pcmp_boundary_cases[0];
                     ++boundary) {
                    hl_native_x86_64_cpu cpu = {0};
                    uint64_t initial_flags = UINT64_C(0xad7) | UINT64_C(0x200);
                    uint64_t flags = initial_flags & ~changed;
                    unsigned expected = pcmp_boundary_cases[boundary].expected;
                    unsigned expected_index = expected == 0u ? 16u :
                        controls[control] == 0x48u
                            ? 31u - (unsigned)__builtin_clz(expected)
                            : (unsigned)__builtin_ctz(expected);
                    if (expected != 0u) flags |= HL_X86_RFLAGS_CF;
                    if (pcmp_length(pcmp_boundary_cases[boundary].right) < 16u)
                        flags |= HL_X86_RFLAGS_ZF;
                    if (pcmp_length(pcmp_boundary_cases[boundary].left) < 16u)
                        flags |= HL_X86_RFLAGS_SF;
                    if ((expected & 1u) != 0u) flags |= HL_X86_RFLAGS_OF;
                    memcpy(&cpu.vectors[0], pcmp_boundary_cases[boundary].left, 16u);
                    memcpy(&cpu.vectors[2], pcmp_boundary_cases[boundary].right, 16u);
                    cpu.registers[1] = UINT64_C(0xfeedface);
                    cpu.flags = initial_flags;
                    hl_native_x86_64_enter(&cpu, code);
                    CHECK(cpu.registers[1] == expected_index);
                    CHECK(cpu.flags == flags);
                }
            }
            { /* Register aliasing observes one immutable operand value. */
                hl_native_x86_64_cpu cpu = {0};
                static const uint8_t value[16] = {
                    1, 2, 3, 4, 5, 6, 7, 8, 0, 10, 11, 12, 13, 14, 15, 16,
                };
                unsigned expected = pcmp_equal_each(value, value, controls[control]);
                unsigned expected_index = expected == 0u ? 16u :
                    (controls[control] & 0x40u) != 0u
                        ? 31u - (unsigned)__builtin_clz(expected)
                        : (unsigned)__builtin_ctz(expected);
                guest[4] = 0xc0;
                request = request_for(guest, sizeof guest, host, provenance);
                request.max_instructions = 1u;
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
                CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
                memcpy(code, host, result.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                memcpy(&cpu.vectors[0], value, sizeof value);
                cpu.flags = UINT64_C(0xad7);
                hl_native_x86_64_enter(&cpu, code);
                CHECK(cpu.registers[1] == expected_index);
                CHECK(memcmp(&cpu.vectors[0], value, sizeof value) == 0);
            }
            CHECK(munmap(code, (size_t)page) == 0);
        }
#endif
    }
    {
        static const uint8_t memory[] = {0x66, 0x0f, 0x3a, 0x63, 0x44, 0x8b, 0x07, 0x1a};
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(memory, sizeof memory, host, provenance);
        hl_x86_a64_result result;
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof memory);
#if defined(__aarch64__)
        {
            static const uint8_t left[16] = {
                'a', 'b', 'c', 'd', 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
            };
            static const uint8_t right[16] = {
                'a', 'b', 'x', 'd', 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
            };
            long page = sysconf(_SC_PAGESIZE);
            uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            uint8_t *pages = mmap(NULL, (size_t)page * 2u, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            uint8_t *edge;
            hl_native_x86_64_cpu cpu = {0};
            unsigned expected = pcmp_equal_each(left, right, 0x1a);
            uint64_t initial_flags = UINT64_C(0xad7) | UINT64_C(0x200);
            uint64_t expected_flags = initial_flags & ~changed;
            CHECK(code != MAP_FAILED);
            CHECK(pages != MAP_FAILED);
            CHECK(mprotect(pages + page, (size_t)page, PROT_NONE) == 0);
            edge = pages + page - 16;
            memcpy(code, host, result.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            memcpy(edge, right, sizeof right);
            memcpy(&cpu.vectors[0], left, sizeof left);
            cpu.registers[3] = UINT64_C(0x2000);
            cpu.registers[1] = 1u;
            cpu.memory_first = UINT64_C(0x200b);
            cpu.memory_last = UINT64_C(0x201b);
            cpu.memory_delta = (uint64_t)(uintptr_t)edge - UINT64_C(0x200b);
            cpu.memory_permissions = 1u;
            cpu.flags = initial_flags;
            cpu.program = UINT64_C(0x400000);
            cpu.budget = 7u;
            if (expected != 0u) expected_flags |= HL_X86_RFLAGS_CF;
            expected_flags |= HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF;
            if ((expected & 1u) != 0u) expected_flags |= HL_X86_RFLAGS_OF;
            hl_native_x86_64_enter(&cpu, code);
            CHECK(cpu.registers[1] == (unsigned)__builtin_ctz(expected));
            CHECK(cpu.flags == expected_flags);
            CHECK(memcmp(&cpu.vectors[0], left, sizeof left) == 0);
            CHECK(cpu.program == UINT64_C(0x400008));
            CHECK(cpu.budget == 7u && cpu.executed == 0u);
            cpu.registers[1] = 1u;
            cpu.flags = initial_flags;
            cpu.reason = cpu.fault_access = cpu.fault_size = 0;
            cpu.memory_last = UINT64_C(0x201a);
            cpu.program = UINT64_C(0x400000);
            cpu.scratch[0] = 0u;
            cpu.budget = 7u;
            cpu.executed = 0u;
            hl_native_x86_64_enter(&cpu, code);
            CHECK(cpu.registers[1] == 1u && cpu.flags == initial_flags);
            CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == 1u && cpu.fault_size == 16u);
            CHECK(cpu.fault_address == UINT64_C(0x200b));
            CHECK(cpu.program == UINT64_C(0x400000));
            CHECK(cpu.scratch[0] == 0u && cpu.budget == 7u && cpu.executed == 0u);
            CHECK(memcmp(&cpu.vectors[0], left, sizeof left) == 0);
            CHECK(munmap(pages, (size_t)page * 2u) == 0);
            CHECK(munmap(code, (size_t)page) == 0);
        }
#endif
    }
    {
        static const uint8_t wrong_control[] = {0x66, 0x0f, 0x3a, 0x63, 0xc1, 0x00};
        static const uint8_t wrong_prefix[] = {0x0f, 0x3a, 0x63, 0xc1, 0x08};
        static const uint8_t f2_prefix[] = {0xf2, 0x66, 0x0f, 0x3a, 0x63, 0xc1, 0x08};
        static const uint8_t f3_prefix[] = {0xf3, 0x66, 0x0f, 0x3a, 0x63, 0xc1, 0x08};
        static const uint8_t lock_prefix[] = {0xf0, 0x66, 0x0f, 0x3a, 0x63, 0xc1, 0x08};
        static const uint8_t repeated_66[] = {0x66, 0x66, 0x0f, 0x3a, 0x63, 0xc1, 0x08};
        static const uint8_t truncated[] = {0x66, 0x0f, 0x3a, 0x63, 0xc1};
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(wrong_control, sizeof wrong_control, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        request = request_for(wrong_prefix, sizeof wrong_prefix, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        request = request_for(f2_prefix, sizeof f2_prefix, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        request = request_for(f3_prefix, sizeof f3_prefix, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        request = request_for(lock_prefix, sizeof lock_prefix, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        request = request_for(repeated_66, sizeof repeated_66, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof repeated_66);
        request = request_for(truncated, sizeof truncated, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
    }
    CHECK(maximum_words <= 96u);
    return 0;
}

static int scalar_double_family(void) {
    const uint8_t guest[] = {
        0xf2, 0x0f, 0x5c, 0xc1,       /* subsd xmm0,xmm1 */
        0x66, 0x0f, 0x2f, 0xc1,       /* comisd xmm0,xmm1 */
        0xf2, 0x48, 0x0f, 0x2c, 0xc0, /* cvttsd2si rax,xmm0 */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.instruction_count == 3);
    CHECK(provenance[0].guest_size == 4 && provenance[1].guest_size == 4);
    CHECK(provenance[2].guest_size == 5);
    CHECK(provenance[0].word_end > provenance[0].word_start);
    CHECK(provenance[1].word_end > provenance[1].word_start);
    CHECK(provenance[2].word_end > provenance[2].word_start);
    return 0;
}

static int scalar_double_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const uint8_t family[] = {
        0xf2, 0x0f, 0x5c, 0xc1,       /* subsd xmm0,xmm1 */
        0x66, 0x0f, 0x2f, 0xc1,       /* comisd xmm0,xmm1 */
        0xf2, 0x48, 0x0f, 0x2c, 0xc0, /* cvttsd2si rax,xmm0 */
    };
    static const uint8_t compare[] = {0x66, 0x0f, 0x2f, 0xc1};
    static const uint8_t convert[] = {0xf2, 0x48, 0x0f, 0x2c, 0xc0};
    const uint64_t comparison = HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF |
                                HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF | HL_X86_RFLAGS_OF;
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned index;

    CHECK(code != MAP_FAILED);
    {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(family, sizeof family, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.vectors[0] = UINT64_C(0x4014000000000000); /* 5.0 */
        cpu.vectors[1] = UINT64_C(0xfeedfacedeadbeef);
        cpu.vectors[2] = UINT64_C(0x4000000000000000); /* 2.0 */
        cpu.flags = comparison;
        hl_native_x86_64_enter(&cpu, code);
        CHECK(cpu.vectors[0] == UINT64_C(0x4008000000000000)); /* 3.0 */
        CHECK(cpu.vectors[1] == UINT64_C(0xfeedfacedeadbeef));
        CHECK(cpu.registers[0] == 3 && (cpu.flags & comparison) == 0);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(compare, sizeof compare, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.vectors[0] = UINT64_C(0x7ff8000000000000); /* quiet NaN */
        cpu.vectors[2] = UINT64_C(0x3ff0000000000000); /* 1.0 */
        cpu.flags = comparison;
        hl_native_x86_64_enter(&cpu, code);
        CHECK((cpu.flags & (HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF | HL_X86_RFLAGS_ZF)) ==
              (HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF | HL_X86_RFLAGS_ZF));
        CHECK((cpu.flags & (HL_X86_RFLAGS_AF | HL_X86_RFLAGS_SF | HL_X86_RFLAGS_OF)) == 0);
        CHECK((cpu.fpsr & 1u) != 0); /* COMISD signals invalid for a quiet NaN. */
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    for (index = 0; index < 3u; ++index) {
        static const uint64_t inputs[] = {
            UINT64_C(0x400e000000000000), /* 3.75 */
            UINT64_C(0x7ff0000000000000), /* +infinity */
            UINT64_C(0x7ff8000000000000), /* quiet NaN */
        };
        static const uint64_t expected[] = {
            UINT64_C(3), UINT64_C(0x8000000000000000), UINT64_C(0x8000000000000000),
        };
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(convert, sizeof convert, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.vectors[0] = inputs[index];
        cpu.flags = UINT64_C(0xad7);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(cpu.registers[0] == expected[index] && cpu.flags == UINT64_C(0xad7));
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int floating_arithmetic(void) {
    const uint8_t family[] = {
        0x0f, 0x58, 0xc1,       /* addps xmm0,xmm1 */
        0x0f, 0x5c, 0xc2,       /* subps xmm0,xmm2 */
        0xf3, 0x0f, 0x5c, 0xc3, /* subss xmm0,xmm3 */
        0x66, 0x0f, 0x58, 0xe5, /* addpd xmm4,xmm5 */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(family, sizeof family, host, provenance);
    hl_x86_a64_result result;

    request.flags |= HL_X86_A64_CHECKPOINTS;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.instruction_count == 4);
    CHECK(provenance[0].guest_size == 3 && provenance[1].guest_size == 3);
    CHECK(provenance[2].guest_size == 4 && provenance[3].guest_size == 4);
    {
        static const struct {
            uint8_t bytes[4];
            size_t size;
        } additional[] = {
            {{0x0f, 0x59, 0xc1, 0}, 3},       /* mulps */
            {{0x66, 0x0f, 0x59, 0xc1}, 4},   /* mulpd */
            {{0xf3, 0x0f, 0x59, 0xc1}, 4},   /* mulss */
            {{0x0f, 0x5e, 0xc1, 0}, 3},      /* divps */
            {{0x66, 0x0f, 0x5e, 0xc1}, 4},   /* divpd */
            {{0xf3, 0x0f, 0x5e, 0xc1}, 4},   /* divss */
        };
        static const uint8_t scalar_double[] = {
            0xf2, 0x0f, 0x59, 0xc1, /* mulsd */
            0xf2, 0x0f, 0x5e, 0xc1, /* divsd */
        };
        uint32_t boundary_host[256] = {0};
        hl_x86_a64_provenance boundary_provenance[8] = {0};
        hl_x86_a64_result boundary_result;
        unsigned index;

        for (index = 0; index < sizeof additional / sizeof additional[0]; ++index) {
            hl_x86_a64_request admitted = request_for(additional[index].bytes,
                                                      additional[index].size,
                                                      boundary_host, boundary_provenance);
            CHECK(hl_x86_a64_emit(&admitted, &boundary_result) == HL_X86_A64_OK);
            CHECK(boundary_result.instruction_count == 1);
        }
        request = request_for(scalar_double, sizeof scalar_double,
                              boundary_host, boundary_provenance);
        CHECK(hl_x86_a64_emit(&request, &boundary_result) == HL_X86_A64_OK);
        CHECK(boundary_result.exit == HL_X86_A64_FALLTHROUGH && boundary_result.instruction_count == 2);
    }
#if defined(__aarch64__)
    {
        static const float initial[4] = {1.0f, 2.0f, 3.0f, 4.0f};
        static const float addend[4] = {5.0f, 6.0f, 7.0f, 8.0f};
        static const float factor[4] = {2.0f, 3.0f, 4.0f, 5.0f};
        static const float scalar[4] = {1.0f, 99.0f, 99.0f, 99.0f};
        static const float expected[4] = {3.0f, 5.0f, 6.0f, 7.0f};
        static const double packed[4] = {1.0, 2.0, 3.0, 4.0};
        static const double packed_expected[2] = {4.0, 6.0};
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        hl_native_x86_64_cpu cpu = {0};

        CHECK(code != MAP_FAILED);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[0], initial, sizeof initial);
        memcpy(&cpu.vectors[2], addend, sizeof addend);
        memcpy(&cpu.vectors[4], factor, sizeof factor);
        memcpy(&cpu.vectors[6], scalar, sizeof scalar);
        memcpy(&cpu.vectors[8], packed, sizeof packed);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
        CHECK(memcmp(&cpu.vectors[8], packed_expected, sizeof packed_expected) == 0);
        CHECK(munmap(code, (size_t)page) == 0);
    }
    {
        static const uint8_t generated_nan[] = {0x0f, 0x58, 0xc1}; /* addps xmm0,xmm1 */
        static const uint32_t before[4] = {0x7f800000u, 1u, 2u, 3u};
        static const uint32_t other[4] = {0xff800000u, 4u, 5u, 6u};
        uint32_t nan_host[256] = {0};
        hl_x86_a64_provenance nan_provenance[8] = {0};
        hl_x86_a64_request nan_request = request_for(generated_nan, sizeof generated_nan,
                                                     nan_host, nan_provenance);
        hl_x86_a64_result nan_result;
        hl_native_x86_64_cpu cpu = {0};
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

        CHECK(code != MAP_FAILED);
        CHECK(hl_x86_a64_emit(&nan_request, &nan_result) == HL_X86_A64_OK);
        memcpy(code, nan_host, nan_result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[nan_result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (nan_result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[0], before, sizeof before);
        memcpy(&cpu.vectors[2], other, sizeof other);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(cpu.program == UINT64_C(0x400000) && cpu.reason == HL_NATIVE_EXIT_FALLBACK);
        CHECK(memcmp(&cpu.vectors[0], before, sizeof before) == 0);
        CHECK(munmap(code, (size_t)page) == 0);
    }
    {
        static const uint8_t memory_subtract[] = {
            0x0f, 0x5c, 0x03,             /* subps xmm0,[rbx] */
            0x66, 0x0f, 0x5c, 0x4b, 0x10, /* subpd xmm1,[rbx+16] */
        };
        static const float initial[4] = {1.0f, 2.0f, 3.0f, 4.0f};
        static const float factor[4] = {2.0f, 3.0f, 4.0f, 5.0f};
        static const float expected[4] = {-1.0f, -1.0f, -1.0f, -1.0f};
        static const double initial_double[2] = {10.0, 20.0};
        static const double factor_double[2] = {3.0, 4.0};
        static const double expected_double[2] = {7.0, 16.0};
        _Alignas(16) uint8_t operands[32];
        uint32_t memory_host[256] = {0};
        hl_x86_a64_provenance memory_provenance[8] = {0};
        hl_x86_a64_request memory_request = request_for(memory_subtract, sizeof memory_subtract,
                                                        memory_host, memory_provenance);
        hl_x86_a64_result memory_result;
        hl_native_x86_64_cpu cpu = {0};
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

        CHECK(code != MAP_FAILED);
        CHECK(hl_x86_a64_emit(&memory_request, &memory_result) == HL_X86_A64_OK);
        memcpy(code, memory_host, memory_result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[memory_result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (memory_result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[0], initial, sizeof initial);
        memcpy(&cpu.vectors[2], initial_double, sizeof initial_double);
        memcpy(operands, factor, sizeof factor);
        memcpy(operands + 16, factor_double, sizeof factor_double);
        cpu.registers[3] = UINT64_C(0x2000);
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x2020);
        cpu.memory_delta = (uint64_t)(uintptr_t)operands - UINT64_C(0x2000);
        cpu.memory_permissions = 1u;
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
        CHECK(memcmp(&cpu.vectors[2], expected_double, sizeof expected_double) == 0);
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int end_branch(void) {
    const uint8_t guest[] = {
        0xf3, 0x0f, 0x1e, 0xfa, /* endbr64 */
        0xf3, 0x0f, 0x1e, 0xfb, /* endbr32 */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH);
    CHECK(result.instruction_count == 2 && result.word_count == 3);
    CHECK(result.exit_pc == UINT64_C(0x400008));
    CHECK(provenance[0].guest_pc == UINT64_C(0x400000) && provenance[0].guest_size == 4);
    CHECK(provenance[0].word_start == 0 && provenance[0].word_end == 0);
    CHECK(provenance[1].guest_pc == UINT64_C(0x400004) && provenance[1].guest_size == 4);
    CHECK(provenance[1].word_start == 0 && provenance[1].word_end == 0);
    return 0;
}

static int typed_exits(void) {
    const uint8_t branch[] = {0xeb, 0xfe};
    const uint8_t syscall[] = {0x0f, 0x05};
    const uint8_t unsupported[] = {0xcc};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(branch, sizeof branch, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_DIRECT_BRANCH);
    CHECK(result.branch_target == UINT64_C(0x400000));
    CHECK(result.instruction_count == 1);
    CHECK(provenance[0].guest_size == 2);
    request = request_for(syscall, sizeof syscall, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_SYSCALL);
    CHECK(result.exit_pc == UINT64_C(0x400002));
    request = request_for(unsupported, sizeof unsupported, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(result.exit == HL_X86_A64_INTERPRETER);
    CHECK(result.exit_pc == UINT64_C(0x400000));
    return 0;
}

static int bounded_output(void) {
    const uint8_t guest[] = {0x48, 0xb8, 1, 0, 0, 0, 0, 0, 0, 0};
    uint32_t host[1] = {UINT32_C(0xfeedface)};
    hl_x86_a64_provenance provenance[8] = {{UINT64_C(0xfeedface), 7, 8, 9, 10}};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;

    memset(&result, 0xa5, sizeof result);
    request.host_capacity = 1;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
    CHECK(host[0] == UINT32_C(0xfeedface));
    CHECK(provenance[0].guest_pc == UINT64_C(0xfeedface));
    CHECK(provenance[0].guest_size == 7 && provenance[0].word_start == 8);
    CHECK(result.abi == UINT32_C(0xa5a5a5a5));
    return 0;
}

static int register_moves(void) {
    const uint8_t guest[] = {
        0x48, 0x89, 0xd8,       /* mov rax, rbx */
        0x45, 0x8b, 0xc1,       /* mov r8d, r9d */
        0x66, 0x45, 0x89, 0xca, /* mov r10w, r9w */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 3);
    CHECK(result.exit_pc == UINT64_C(0x40000a));
    CHECK(host[0] == UINT32_C(0xaa0303e0));
    CHECK(host[1] == UINT32_C(0x2a0903e8));
    CHECK(host[2] == UINT32_C(0xb3403d2a));
    CHECK(provenance[0].guest_size == 3 && provenance[0].word_end == 1);
    CHECK(provenance[1].guest_size == 3 && provenance[1].word_start == 1);
    CHECK(provenance[2].guest_size == 4 && provenance[2].word_start == 2);
    return 0;
}

static int immediate_widths(void) {
    const uint8_t guest[] = {
        0x66, 0xb8, 0x34, 0x12,                   /* mov ax, 0x1234 */
        0x4d, 0xc7, 0xc0, 0x00, 0x00, 0x00, 0x80, /* REX.R ignored; REX.B selects r8 */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 2);
    CHECK(provenance[0].guest_size == 4);
    CHECK(host[0] == UINT32_C(0xd2824690));
    CHECK(host[1] == UINT32_C(0xb3403e00));
    CHECK(provenance[1].guest_size == 7);
    CHECK(host[2] == UINT32_C(0xd2800008));
    CHECK(host[3] == UINT32_C(0xf2b00008));
    CHECK(host[4] == UINT32_C(0xf2dfffe8));
    CHECK(host[5] == UINT32_C(0xf2ffffe8));
    return 0;
}

static int exact_fallback(void) {
    const uint8_t bad_group[] = {0x48, 0xc7, 0xc8, 1, 0, 0, 0};
    const uint8_t truncated[] = {0x66, 0xb8, 0x34};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(bad_group, sizeof bad_group, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(result.exit_pc == UINT64_C(0x400000));
    request = request_for(truncated, sizeof truncated, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
    CHECK(result.exit_pc == UINT64_C(0x400000));
    return 0;
}

static int memory_load_contract(void) {
    static const struct { uint8_t bytes[5]; size_t size; } cases[] = {
        {{0x48, 0x8b, 0x03, 0, 0}, 3},       /* mov rax,[rbx] */
        {{0x8b, 0x4b, 0x01, 0, 0}, 3},       /* mov ecx,[rbx+1] */
        {{0x66, 0x8b, 0x53, 0x02, 0}, 4},    /* mov dx,[rbx+2] */
        {{0x8a, 0x63, 0x03, 0, 0}, 3},       /* mov ah,[rbx+3] */
        {{0x44, 0x8a, 0x43, 0x04, 0}, 4},    /* mov r8b,[rbx+4] */
    };
    size_t index;

    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size,
                                                  host, provenance);
        hl_x86_a64_result result;

        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.instruction_count == 1);
        CHECK(provenance[0].guest_size == cases[index].size);
        CHECK(provenance[0].word_end > provenance[0].word_start);
        request.host_capacity = result.word_count - 1u;
        memset(host, 0xa5, sizeof host);
        memset(provenance, 0xa5, sizeof provenance);
        memset(&result, 0xa5, sizeof result);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
        CHECK(host[0] == UINT32_C(0xa5a5a5a5));
        CHECK(provenance[0].guest_pc == UINT64_C(0xa5a5a5a5a5a5a5a5));
    }
    return 0;
}

static int memory_load_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const struct {
        uint8_t bytes[5];
        size_t size;
        unsigned destination;
        uint64_t initial;
        uint64_t expected;
    } cases[] = {
        {{0x48, 0x8b, 0x03, 0, 0}, 3, 0, UINT64_MAX, UINT64_C(0x8877665544332211)},
        {{0x8b, 0x4b, 0x01, 0, 0}, 3, 1, UINT64_MAX, UINT64_C(0x55443322)},
        {{0x66, 0x8b, 0x53, 0x02, 0}, 4, 2, UINT64_C(0xaaaabbbbccccdddd),
         UINT64_C(0xaaaabbbbcccc4433)},
        {{0x8a, 0x63, 0x03, 0, 0}, 3, 0, UINT64_C(0xaaaabbbbccccdddd),
         UINT64_C(0xaaaabbbbcccc44dd)},
        {{0x44, 0x8a, 0x43, 0x04, 0}, 4, 8, UINT64_C(0xaaaabbbbccccdddd),
         UINT64_C(0xaaaabbbbccccdd55)},
    };
    const uint8_t data[] = {0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t index;

    CHECK(code != MAP_FAILED);
    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size,
                                                  host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};

        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[3] = UINT64_C(0x1000);
        cpu.registers[cases[index].destination] = cases[index].initial;
        cpu.memory_first = UINT64_C(0x1000);
        cpu.memory_last = UINT64_C(0x1008);
        cpu.memory_delta = (uint64_t)(uintptr_t)data - UINT64_C(0x1000);
        cpu.memory_permissions = 1;
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[cases[index].destination] == cases[index].expected);
        CHECK(cpu.fault_access == 0 && cpu.reason == 0);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    {
        const uint8_t guest[] = {0x48, 0x8b, 0x03};
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};

        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = UINT64_C(0xfeedfacecafebeef);
        cpu.registers[3] = UINT64_C(0x1007);
        cpu.flags = UINT64_C(0xad7);
        cpu.memory_first = UINT64_C(0x1000);
        cpu.memory_last = UINT64_C(0x1008);
        cpu.memory_delta = (uint64_t)(uintptr_t)data - UINT64_C(0x1000);
        cpu.memory_permissions = 1;
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[0] == UINT64_C(0xfeedfacecafebeef));
        CHECK(cpu.flags == UINT64_C(0xad7));
        CHECK(cpu.fault_address == UINT64_C(0x1007));
        CHECK(cpu.fault_access == 1 && cpu.fault_size == 8);
        CHECK(cpu.program == UINT64_C(0x400000) && cpu.reason == HL_NATIVE_EXIT_FALLBACK);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int prefix_order(void) {
    const uint8_t before[] = {0x66, 0x48, 0xb8, 1, 0, 0, 0, 0, 0, 0, 0};
    const uint8_t after[] = {0x48, 0x66, 0xb8, 1, 0};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(before, sizeof before, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == 11);
    request = request_for(after, sizeof after, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(result.exit == HL_X86_A64_INTERPRETER && result.exit_pc == UINT64_C(0x400000));
    CHECK(result.instruction_count == 0);
    return 0;
}

static int decode_bounds(void) {
    const uint8_t wide_c7[] = {0x66, 0x48, 0xc7, 0xc0, 0x78, 0x56, 0x34, 0x12};
    const uint8_t overlong[] = {
        0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
        0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x48, 0x90,
    };
    const uint8_t overlong_mov[] = {
        0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
        0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x48, 0xb8,
        1, 0, 0, 0, 0, 0, 0, 0,
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(wide_c7, sizeof wide_c7, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == 8);
    CHECK(host[0] == UINT32_C(0xd28acf00));
    CHECK(host[1] == UINT32_C(0xf2a24680));
    request = request_for(overlong, sizeof overlong, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(result.exit == HL_X86_A64_INTERPRETER && result.exit_pc == UINT64_C(0x400000));
    CHECK(result.instruction_count == 0);
    request = request_for(overlong_mov, sizeof overlong_mov, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(result.exit_pc == UINT64_C(0x400000) && result.instruction_count == 0);
    return 0;
}

static int conditional_control(void) {
    const uint8_t equal[] = {0x74, 0x02};
    const uint8_t below_near[] = {0x0f, 0x82, 0xfa, 0xff, 0xff, 0xff};
    const uint8_t parity[] = {0x7a, 0};
    const uint8_t truncated[] = {0x0f, 0x85, 1, 0};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(equal, sizeof equal, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_CONDITIONAL_BRANCH);
    CHECK(result.exit_pc == UINT64_C(0x400002) && result.branch_target == UINT64_C(0x400004));
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == 2);
    CHECK(host[0] == UINT32_C(0xf9404794));
    CHECK(host[14] == UINT32_C(0xd51b4215));
    CHECK((host[19] & UINT32_C(0x0000f000)) == 0);
    CHECK(result.word_count == 39);

    memset(host, 0, sizeof host);
    request.host_capacity = 39;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    memset(host, 0xa5, sizeof host);
    memset(provenance, 0xa5, sizeof provenance);
    memset(&result, 0xa5, sizeof result);
    request.host_capacity = 38;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
    CHECK(provenance[0].guest_pc == UINT64_C(0xa5a5a5a5a5a5a5a5));
    CHECK(result.abi == UINT32_C(0xa5a5a5a5));

    memset(host, 0, sizeof host);
    memset(provenance, 0, sizeof provenance);
    request = request_for(below_near, sizeof below_near, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit_pc == UINT64_C(0x400006) && result.branch_target == UINT64_C(0x400000));
    CHECK((host[19] & UINT32_C(0x0000f000)) == UINT32_C(0x00003000));

    request = request_for(parity, sizeof parity, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_CONDITIONAL_BRANCH && result.exit_pc == UINT64_C(0x400002));
    CHECK(result.word_count == 9 && (host[7] & UINT32_C(0x0000f000)) == UINT32_C(0x00001000));
    request = request_for(truncated, sizeof truncated, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
    CHECK(result.exit_pc == UINT64_C(0x400000));
    return 0;
}

static int reference_condition(unsigned condition, uint64_t flags) {
    int carry = (flags >> 0) & 1u;
    int parity = (flags >> 2) & 1u;
    int zero = (flags >> 6) & 1u;
    int sign = (flags >> 7) & 1u;
    int overflow = (flags >> 11) & 1u;
    int base[] = {overflow, carry, zero, carry || zero, sign, parity, sign != overflow,
                  zero || sign != overflow};

    return base[(condition >> 1) & 7u] ^ (condition & 1u);
}

static int flags_contract(void) {
    const uint64_t lanes[] = {HL_X86_RFLAGS_CF, HL_X86_RFLAGS_PF, HL_X86_RFLAGS_AF,
                              HL_X86_RFLAGS_ZF, HL_X86_RFLAGS_SF, HL_X86_RFLAGS_OF};
    unsigned combination;

    for (combination = 0; combination < 64u; ++combination) {
        uint64_t flags = UINT64_C(0x55aa000000000202);
        uint64_t roundtrip;
        uint32_t nzcv;
        unsigned condition;
        unsigned lane;

        for (lane = 0; lane < 6u; ++lane) {
            flags &= ~lanes[lane];
            if ((combination & (1u << lane)) != 0) flags |= lanes[lane];
        }
        nzcv = hl_x86_rflags_to_nzcv(flags);
        CHECK(((nzcv >> 31) & 1u) == ((flags >> 7) & 1u));
        CHECK(((nzcv >> 30) & 1u) == ((flags >> 6) & 1u));
        CHECK(((nzcv >> 29) & 1u) != ((flags >> 0) & 1u));
        CHECK(((nzcv >> 28) & 1u) == ((flags >> 11) & 1u));
        CHECK((nzcv & UINT32_C(0x0fffffff)) == 0);
        roundtrip = hl_x86_nzcv_to_rflags(nzcv, flags);
        CHECK(roundtrip == flags);
        CHECK((roundtrip & (HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF)) ==
              (flags & (HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF)));
        for (condition = 0; condition < 16u; ++condition)
            CHECK(hl_x86_condition_holds((uint8_t)condition, flags) == reference_condition(condition, flags));
    }
    return 0;
}

static int condition_frontend_contract(void) {
    static const uint8_t arm[16] = {6, 7, 3, 2, 0, 1, 9, 8, 4, 5, 1, 0, 11, 10, 13, 12};
    unsigned condition;

    for (condition = 0; condition < 16u; ++condition) {
        uint8_t guest[] = {(uint8_t)(0x70u | condition), 0};
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_x86_a64_result result;

        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.exit == HL_X86_A64_CONDITIONAL_BRANCH && result.instruction_count == 1);
        CHECK(result.word_count == (condition == 0xau || condition == 0xbu ? 9u : 39u));
        CHECK(provenance[0].guest_pc == UINT64_C(0x400000) && provenance[0].guest_size == 2);
        CHECK(provenance[0].word_start == 0 && provenance[0].word_end == 0);
        CHECK(((host[condition == 0xau || condition == 0xbu ? 7u : 19u] >> 12) & 0xfu) == arm[condition]);
    }
    return 0;
}

static int cmov_contract(void) {
    static const struct { uint8_t bytes[5]; size_t size; } cases[] = {
        {{0x48, 0x0f, 0x45, 0xc1, 0}, 4}, /* cmovne rax,rcx */
        {{0x0f, 0x45, 0xc1, 0, 0}, 3},   /* cmovne eax,ecx */
        {{0x66, 0x0f, 0x45, 0xc1, 0}, 4}, /* cmovne ax,cx */
        {{0x48, 0x0f, 0x45, 0x03, 0}, 4}, /* cmovne rax,[rbx] */
    };
    size_t index;

    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size,
                                                  host, provenance);
        hl_x86_a64_result result;

        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.instruction_count == 1);
        CHECK(provenance[0].guest_size == cases[index].size && provenance[0].word_end > 0);
        request.host_capacity = result.word_count - 1u;
        memset(host, 0xa5, sizeof host);
        memset(provenance, 0xa5, sizeof provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
        CHECK(host[0] == UINT32_C(0xa5a5a5a5));
        CHECK(provenance[0].guest_pc == UINT64_C(0xa5a5a5a5a5a5a5a5));
    }
    return 0;
}

static int cmov_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned condition;

    CHECK(code != MAP_FAILED);
    for (condition = 0; condition < 16u; ++condition) {
        unsigned combination;
        for (combination = 0; combination < 64u; ++combination) {
            const uint64_t lanes[] = {HL_X86_RFLAGS_CF, HL_X86_RFLAGS_PF, HL_X86_RFLAGS_AF,
                                      HL_X86_RFLAGS_ZF, HL_X86_RFLAGS_SF, HL_X86_RFLAGS_OF};
            uint8_t guest[] = {0x48, 0x0f, (uint8_t)(0x40u | condition), 0xc1};
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[8] = {0};
            hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
            hl_x86_a64_result emitted;
            hl_native_x86_64_cpu cpu = {0};
            unsigned lane;

            for (lane = 0; lane < 6u; ++lane)
                if ((combination & (1u << lane)) != 0) cpu.flags |= lanes[lane];
            cpu.flags |= UINT64_C(0x202);
            cpu.registers[0] = UINT64_C(0x1111222233334444);
            cpu.registers[1] = UINT64_C(0xaaaabbbbccccdddd);
            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            uint64_t flags = cpu.flags;
            hl_x86_test_enter(&cpu, code);
            CHECK(cpu.registers[0] == (reference_condition(condition, flags) ?
                                           UINT64_C(0xaaaabbbbccccdddd) :
                                           UINT64_C(0x1111222233334444)));
            CHECK(cpu.flags == flags);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
    }
    {
        static const struct {
            uint8_t bytes[4];
            size_t size;
            uint64_t flags;
            uint64_t expected;
        } cases[] = {
            {{0x0f, 0x45, 0xc1, 0}, 3, HL_X86_RFLAGS_ZF, UINT64_C(0x33334444)},
            {{0x66, 0x0f, 0x45, 0xc1}, 4, 0, UINT64_C(0x111122223333dddd)},
            {{0x66, 0x0f, 0x45, 0xc1}, 4, HL_X86_RFLAGS_ZF, UINT64_C(0x1111222233334444)},
        };
        size_t index;

        for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[8] = {0};
            hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size,
                                                      host, provenance);
            hl_x86_a64_result emitted;
            hl_native_x86_64_cpu cpu = {0};

            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            cpu.flags = cases[index].flags | UINT64_C(0x202);
            cpu.registers[0] = UINT64_C(0x1111222233334444);
            cpu.registers[1] = UINT64_C(0xaaaabbbbccccdddd);
            hl_x86_test_enter(&cpu, code);
            CHECK(cpu.registers[0] == cases[index].expected);
            CHECK(cpu.flags == (cases[index].flags | UINT64_C(0x202)));
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
    }
    {
        const uint8_t guest[] = {0x48, 0x0f, 0x45, 0x03};
        const uint64_t source = UINT64_C(0xaaaabbbbccccdddd);
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};

        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = UINT64_C(0x1111222233334444);
        cpu.registers[3] = UINT64_C(0x2000);
        cpu.flags = HL_X86_RFLAGS_ZF | UINT64_C(0x202);
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x2008);
        cpu.memory_delta = (uint64_t)(uintptr_t)&source - UINT64_C(0x2000);
        cpu.memory_permissions = 1;
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[0] == UINT64_C(0x1111222233334444));
        CHECK(cpu.flags == (HL_X86_RFLAGS_ZF | UINT64_C(0x202)));
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);

        memset(&cpu, 0, sizeof cpu);
        cpu.registers[0] = UINT64_C(0x1111222233334444);
        cpu.registers[3] = UINT64_C(0x2008);
        cpu.flags = HL_X86_RFLAGS_ZF | UINT64_C(0x202);
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x2008);
        cpu.memory_delta = (uint64_t)(uintptr_t)&source - UINT64_C(0x2000);
        cpu.memory_permissions = 1;
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[0] == UINT64_C(0x1111222233334444));
        CHECK(cpu.fault_address == UINT64_C(0x2008));
        CHECK(cpu.fault_access == 1 && cpu.fault_size == 8);
        CHECK(cpu.program == UINT64_C(0x400000) && cpu.reason == HL_NATIVE_EXIT_FALLBACK);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int flags_abi_fallback(void) {
    static const uint8_t fallback[][4] = {
        {0x66, 0x29, 0xd8, 0}, /* sub ax, bx */
    };
    const size_t lengths[] = {3};
    const uint8_t memory[] = {0x48, 0x01, 0x18};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_result result;
    size_t index;

    for (index = 0; index < sizeof fallback / sizeof fallback[0]; ++index) {
        hl_x86_a64_request request = request_for(fallback[index], lengths[index], host, provenance);

        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_FLAGS_ABI_REQUIRED);
        CHECK(result.exit == HL_X86_A64_INTERPRETER && result.exit_pc == UINT64_C(0x400000));
        CHECK(result.instruction_count == 0 && result.provenance_count == 0);
    }
    {
        hl_x86_a64_request request = request_for(memory, sizeof memory, host, provenance);

        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && result.provenance_count == 1);
    }
    return 0;
}

static int register_alu(void) {
    static const uint8_t forms[][4] = {
        {0x48, 0x01, 0xd8, 0}, /* add rax, rbx */
        {0x09, 0xc8, 0, 0},    /* or eax, ecx */
        {0x48, 0x11, 0xd8, 0}, /* adc rax, rbx */
        {0x48, 0x19, 0xd8, 0}, /* sbb rax, rbx */
        {0x21, 0xc8, 0, 0},    /* and eax, ecx */
        {0x48, 0x29, 0xd8, 0}, /* sub rax, rbx */
        {0x31, 0xc8, 0, 0},    /* xor eax, ecx */
        {0x48, 0x39, 0xd8, 0}, /* cmp rax, rbx */
        {0x4d, 0x03, 0xc1, 0}, /* add r8, r9 (reversed direction) */
    };
    const size_t lengths[] = {3, 2, 3, 3, 2, 3, 2, 3, 3};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    size_t index;

    for (index = 0; index < sizeof forms / sizeof forms[0]; ++index) {
        hl_x86_a64_request request = request_for(forms[index], lengths[index], host, provenance);
        hl_x86_a64_result result;

        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
        CHECK(provenance[0].guest_pc == UINT64_C(0x400000));
        CHECK(provenance[0].guest_size == lengths[index] && provenance[0].word_start == 0);
        CHECK(provenance[0].word_end > 30 && provenance[0].word_end < result.word_count);
    }
    {
        const uint8_t add[] = {0x48, 0x01, 0xd8};
        hl_x86_a64_request request = request_for(add, sizeof add, host, provenance);
        hl_x86_a64_result result;

        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        request.host_capacity = result.word_count - 1u;
        memset(host, 0xa5, sizeof host);
        memset(provenance, 0xa5, sizeof provenance);
        memset(&result, 0xa5, sizeof result);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
        CHECK(host[0] == UINT32_C(0xa5a5a5a5));
        CHECK(provenance[0].guest_pc == UINT64_C(0xa5a5a5a5a5a5a5a5));
        CHECK(result.abi == UINT32_C(0xa5a5a5a5));
    }
    return 0;
}

static uint64_t parity_flag(uint64_t value) {
    return __builtin_parity((unsigned)(uint8_t)value) == 0 ? HL_X86_RFLAGS_PF : 0;
}

static void alu_reference(unsigned kind, unsigned bits, uint64_t left, uint64_t right,
                          uint64_t prior, uint64_t *result, uint64_t *flags) {
    uint64_t mask = bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    uint64_t carry_in = (prior & HL_X86_RFLAGS_CF) != 0;
    uint64_t a = left & mask;
    uint64_t b = right & mask;
    uint64_t value;
    int carry;
    int overflow;
    int logical = kind == 1u || kind == 4u || kind == 6u;

    if (kind == 0u || kind == 2u) {
        __uint128_t sum = (__uint128_t)a + b + (kind == 2u ? carry_in : 0u);
        value = (uint64_t)sum & mask;
        carry = sum > mask;
        overflow = ((~(a ^ b) & (a ^ value)) & sign) != 0;
    } else if (kind == 3u || kind == 5u || kind == 7u) {
        uint64_t borrow = kind == 3u ? carry_in : 0u;
        value = (a - b - borrow) & mask;
        carry = (__uint128_t)a < (__uint128_t)b + borrow;
        overflow = (((a ^ b) & (a ^ value)) & sign) != 0;
    } else {
        value = kind == 1u ? a | b : kind == 4u ? a & b : a ^ b;
        carry = 0;
        overflow = 0;
    }
    *flags = prior & ~(HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF |
                       HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF | HL_X86_RFLAGS_OF);
    *flags |= parity_flag(value);
    if (carry) *flags |= HL_X86_RFLAGS_CF;
    if (value == 0) *flags |= HL_X86_RFLAGS_ZF;
    if ((value & sign) != 0) *flags |= HL_X86_RFLAGS_SF;
    if (overflow) *flags |= HL_X86_RFLAGS_OF;
    if (!logical && ((a ^ b ^ value) & 0x10u) != 0) *flags |= HL_X86_RFLAGS_AF;
    *result = bits == 32u ? (uint32_t)value : value;
}

static int incdec_differential(void) {
    static const struct {
        uint8_t bytes[4];
        size_t size;
        unsigned bits;
        unsigned kind;
        unsigned destination;
        uint64_t initial;
    } cases[] = {
        {{0xfe, 0xc4, 0, 0}, 2, 8, 0, 0, UINT64_C(0x112233445566ff88)}, /* inc ah */
        {{0x66, 0xff, 0xc9, 0}, 3, 16, 5, 1, UINT64_C(0x1122334455668000)}, /* dec cx */
        {{0x41, 0xff, 0xc0, 0}, 3, 32, 0, 8, UINT64_C(0xffffffffffffffff)}, /* inc r8d */
        {{0x48, 0xff, 0xc8, 0}, 3, 64, 5, 0, UINT64_C(0x8000000000000000)}, /* dec rax */
    };
    size_t index;

    {
        const uint8_t loop[] = {
            0x48, 0x83, 0xd0, 0x01, /* adc rax,1 */
            0x48, 0xff, 0xc9,       /* dec rcx */
            0x75, 0xf7,             /* jne loop */
        };
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(loop, sizeof loop, host, provenance);
        hl_x86_a64_result emitted;

        request.host_capacity = sizeof host / sizeof host[0];
        request.flags = HL_X86_A64_CHECKPOINTS | HL_X86_A64_CONDITIONAL_SELF_LOOP;
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        CHECK(emitted.instruction_count == 3 && emitted.exit == HL_X86_A64_CONDITIONAL_BRANCH);
        CHECK(emitted.branch_target == request.guest_pc && emitted.exit_pc == request.guest_pc + sizeof loop);
    }

    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size, host, provenance);
        hl_x86_a64_result emitted;

        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        CHECK(emitted.instruction_count == 1 && emitted.exit == HL_X86_A64_FALLTHROUGH);
        CHECK(provenance[0].guest_size == cases[index].size && provenance[0].word_end > 0);
#if defined(__aarch64__)
        {
            long page = sysconf(_SC_PAGESIZE);
            uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            hl_native_x86_64_cpu cpu = {0};
            uint64_t left = cases[index].bits == 8u && cases[index].bytes[1] == 0xc4u ?
                                cases[index].initial >> 8 & 0xffu : cases[index].initial;
            uint64_t expected_result;
            uint64_t expected_flags;
            uint64_t prior = UINT64_C(0xad7) ^ (index & 1u ? HL_X86_RFLAGS_CF : 0u);

            CHECK(code != MAP_FAILED);
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            cpu.registers[cases[index].destination] = cases[index].initial;
            cpu.flags = prior;
            alu_reference(cases[index].kind, cases[index].bits, left, 1u, prior,
                          &expected_result, &expected_flags);
            expected_flags = (expected_flags & ~HL_X86_RFLAGS_CF) | (prior & HL_X86_RFLAGS_CF);
            hl_x86_test_enter(&cpu, code);
            if (cases[index].bits == 8u && cases[index].bytes[1] == 0xc4u)
                expected_result = (cases[index].initial & ~UINT64_C(0xff00)) |
                                  (expected_result & UINT64_C(0xff)) << 8;
            else if (cases[index].bits == 16u)
                expected_result = (cases[index].initial & ~UINT64_C(0xffff)) |
                                  (expected_result & UINT64_C(0xffff));
            CHECK(cpu.registers[cases[index].destination] == expected_result);
            CHECK(cpu.flags == expected_flags);
            CHECK(munmap(code, (size_t)page) == 0);
        }
#endif
    }
    return 0;
}

static int rmw_projection_contract(void) {
    const uint8_t guest[] = {0x48, 0x01, 0x08}; /* addq rcx,(rax) */
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result emitted;

    request.host_capacity = sizeof host / sizeof host[0];
    CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
    CHECK(emitted.instruction_count == 1 && provenance[0].guest_size == sizeof guest);
#if defined(__aarch64__)
    {
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        hl_native_x86_64_cpu cpu = {0};

        CHECK(code != MAP_FAILED);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = UINT64_C(0x2000);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.program == UINT64_C(0x400000) && cpu.reason == HL_NATIVE_EXIT_FALLBACK);
        CHECK(cpu.fault_address == UINT64_C(0x2000));
        CHECK(cpu.fault_access == (HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE) && cpu.fault_size == 8);
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

#if defined(__aarch64__)
static uint32_t vector_fragment(uint32_t *host, size_t host_capacity, int write,
                                unsigned vector, unsigned width);

static int execute_fragment(const uint32_t *host, uint32_t count, hl_native_x86_64_cpu *cpu) {
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    memcpy(code, host, count * sizeof(uint32_t));
    __builtin___clear_cache((char *)code, (char *)code + count * sizeof(uint32_t));
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    hl_native_x86_64_enter(cpu, code);
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
}

static int executable_store_stops_self_loop(void) {
    const uint8_t guest[] = {
        0xc7, 0x07, 0x2a, 0, 0, 0, /* mov dword ptr [rdi],42 */
        0x48, 0xff, 0xc9,          /* dec rcx */
        0x75, 0xf5,                /* jne guest */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result emitted;
    hl_native_x86_64_cpu cpu = {0};
    uint32_t storage = 0;

    request.flags = HL_X86_A64_CHECKPOINTS | HL_X86_A64_CONDITIONAL_SELF_LOOP;
    CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
    CHECK(emitted.exit == HL_X86_A64_CONDITIONAL_BRANCH && emitted.branch_target == request.guest_pc);
    cpu.program = request.guest_pc;
    cpu.registers[1] = 10;
    cpu.registers[7] = UINT64_C(0x7000);
    cpu.memory_first = UINT64_C(0x7000);
    cpu.memory_last = UINT64_C(0x7004);
    cpu.memory_delta = (uint64_t)(uintptr_t)&storage - UINT64_C(0x7000);
    cpu.memory_permissions = 7;
    cpu.dirty_first = UINT64_MAX;
    cpu.loop_remaining = 10;
    CHECK(execute_fragment(host, emitted.word_count, &cpu) == 0);
    CHECK(storage == 42 && cpu.memory_written == 1 && (cpu.executable_written & 4u) != 0);
    CHECK(cpu.loop_completed == 1 && cpu.loop_remaining == 9);
    CHECK(cpu.program == request.guest_pc && cpu.registers[1] == 9);
    return 0;
}

static int vector_arithmetic_self_loop(void) {
    const uint8_t loop[] = {
        0x0f, 0x28, 0x04, 0x01,       /* movaps (rcx,rax),xmm0 */
        0x0f, 0x59, 0x04, 0x02,       /* mulps (rdx,rax),xmm0 */
        0x0f, 0x58, 0xc1,             /* addps xmm1,xmm0 */
        0x0f, 0x29, 0x04, 0x06,       /* movaps xmm0,(rsi,rax) */
        0x48, 0x83, 0xc0, 0x10,       /* add $16,rax */
        0x48, 0x3d, 0x00, 0x40, 0, 0, /* cmp $16384,rax */
        0x75, 0xe5,                   /* jne loop */
    };
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(loop, sizeof loop, host, provenance);
    hl_x86_a64_result emitted;

    request.host_capacity = sizeof host / sizeof host[0];
    request.flags = HL_X86_A64_CHECKPOINTS | HL_X86_A64_CONDITIONAL_SELF_LOOP |
                    HL_X86_A64_LIVE_CHAIN;
    CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
    CHECK(emitted.instruction_count == 7);
    CHECK(emitted.source_end == request.guest_pc + sizeof loop);
    CHECK(emitted.exit == HL_X86_A64_CONDITIONAL_BRANCH);
    CHECK(emitted.branch_target == request.guest_pc);
    return 0;
}

static int executable_self_loop_preserves_indirect_target(void) {
    const uint8_t loop[] = {
        0xc7, 0x07, 0x2a, 0, 0, 0, /* mov dword ptr [rdi],42 */
        0x48, 0xff, 0xc9,          /* dec rcx */
        0x75, 0xf5,                /* jne loop */
    };
    const uint8_t call[] = {0xff, 0xd3}; /* call rbx */
    uint32_t loop_host[256] = {0};
    uint32_t call_host[256] = {0};
    hl_x86_a64_provenance loop_map[8] = {0};
    hl_x86_a64_provenance call_map[8] = {0};
    hl_x86_a64_request loop_request = request_for(loop, sizeof loop, loop_host, loop_map);
    hl_x86_a64_request call_request = request_for(call, sizeof call, call_host, call_map);
    hl_x86_a64_result loop_result;
    hl_x86_a64_result call_result;
    hl_native_x86_64_cpu cpu = {0};
    uint64_t return_address = 0;
    uint32_t executable = 0;
    const uint64_t target = UINT64_C(0x778899aabbccddee);

    loop_request.flags = HL_X86_A64_CHECKPOINTS | HL_X86_A64_CONDITIONAL_SELF_LOOP;
    CHECK(hl_x86_a64_emit(&loop_request, &loop_result) == HL_X86_A64_OK);
    CHECK(loop_result.exit == HL_X86_A64_CONDITIONAL_BRANCH &&
          loop_result.branch_target == loop_request.guest_pc);
    cpu.program = loop_request.guest_pc;
    cpu.registers[1] = 1;
    cpu.registers[3] = target;
    cpu.registers[7] = UINT64_C(0x7000);
    cpu.memory_first = UINT64_C(0x7000);
    cpu.memory_last = UINT64_C(0x7004);
    cpu.memory_delta = (uint64_t)(uintptr_t)&executable - UINT64_C(0x7000);
    cpu.memory_permissions = 7;
    cpu.dirty_first = UINT64_MAX;
    cpu.loop_remaining = 1;
    CHECK(execute_fragment(loop_host, loop_result.word_count, &cpu) == 0);
    CHECK(cpu.program == loop_request.guest_pc + sizeof loop && cpu.registers[1] == 0);
    CHECK(cpu.registers[3] == target && (cpu.executable_written & 4u) != 0);

    call_request.guest_pc = cpu.program;
    CHECK(hl_x86_a64_emit(&call_request, &call_result) == HL_X86_A64_OK);
    CHECK(call_result.exit == HL_X86_A64_DYNAMIC_BRANCH);
    call_host[call_result.word_count] = UINT32_C(0xd65f03c0);
    cpu.executable_written = 0;
    cpu.memory_first = UINT64_C(0x9000);
    cpu.memory_last = UINT64_C(0x9008);
    cpu.memory_delta = (uint64_t)(uintptr_t)&return_address - UINT64_C(0x9000);
    cpu.memory_permissions = 3;
    cpu.registers[4] = UINT64_C(0x9008);
    CHECK(execute_fragment(call_host, call_result.word_count + 1u, &cpu) == 0);
    CHECK(cpu.program == target && cpu.registers[3] == target);
    CHECK(cpu.registers[4] == UINT64_C(0x9000));
    CHECK(return_address == call_request.guest_pc + sizeof call);
    return 0;
}

static void publish_read_view(hl_native_x86_64_cpu *cpu, uint64_t token,
                              uint64_t incarnation, uint64_t count, uint64_t first,
                              uint64_t last, uint64_t delta, uint64_t permissions) {
    cpu->read_token = token;
    cpu->read_incarnation = incarnation;
    cpu->read_count = count;
    cpu->read_views[0][0] = first;
    cpu->read_views[0][1] = last;
    cpu->read_views[0][2] = delta;
    cpu->read_views[0][3] = permissions;
}

static uint32_t scalar_read_fragment(uint32_t *host) {
    instruction item;
    uint32_t cursor = 0;
    memset(&item, 0, sizeof item);
    item.pc = UINT64_C(0x45670000);
    item.operation = OP_LOAD;
    item.destination = 0;
    item.width = 8;
    item.address_base = 3;
    item.address_index = UINT8_MAX;
    hl_x86_emit_load(host, &cursor, &item);
    CHECK(cursor == hl_x86_load_words(&item));
    host[cursor++] = store_word(0, offsetof(hl_native_x86_64_cpu, registers));
    host[cursor++] = UINT32_C(0xd65f03c0);
    return cursor;
}

static uint32_t scalar_write_fragment(uint32_t *host) {
    instruction item;
    uint32_t cursor = 0;
    memset(&item, 0, sizeof item);
    item.pc = UINT64_C(0x45680000);
    item.operation = OP_STORE;
    item.source = 1;
    item.width = 8;
    item.address_base = 3;
    item.address_index = UINT8_MAX;
    hl_x86_emit_store(host, &cursor, &item);
    CHECK(cursor == hl_x86_store_words(&item));
    host[cursor++] = UINT32_C(0xd65f03c0);
    return cursor;
}

/* The trampoline hoists the token/incarnation/count proof out of every access,
 * so the write cache must still refuse an untrusted table on the same terms. */
static int write_cache_contract(void) {
    _Alignas(16) uint64_t storage[4] = {0, 0, 0, 0};
    uint64_t delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x1000);
    uint32_t host[256] = {0};
    uint32_t count = scalar_write_fragment(host);
    hl_native_x86_64_cpu cpu;

    memset(&cpu, 0, sizeof cpu);
    cpu.registers[3] = UINT64_C(0x1000);
    cpu.registers[1] = UINT64_C(0x0123456789abcdef);
    cpu.dirty_first = UINT64_MAX;
    publish_read_view(&cpu, 7, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 3);
    CHECK(execute_fragment(host, count, &cpu) == 0);
    CHECK(storage[0] == UINT64_C(0x0123456789abcdef) && cpu.reason == 0);
    CHECK(cpu.memory_first == UINT64_C(0x1000) && cpu.memory_last == UINT64_C(0x1020));
    CHECK(cpu.dirty_first == UINT64_C(0x1000) && cpu.dirty_last == UINT64_C(0x1008));
    CHECK(cpu.memory_written == 1 && (cpu.executable_written & 4u) == 0);

#define EXPECT_WRITE_FALLBACK()                                                                                        \
    do {                                                                                                               \
        storage[0] = 0;                                                                                                \
        cpu.registers[3] = UINT64_C(0x1000);                                                                           \
        cpu.memory_first = cpu.memory_last = cpu.memory_delta = cpu.memory_permissions = 0;                            \
        cpu.dirty_first = UINT64_MAX;                                                                                  \
        cpu.dirty_last = cpu.dirty_count = cpu.memory_written = 0;                                                     \
        cpu.reason = cpu.fault_access = cpu.fault_size = cpu.fault_address = 0;                                        \
        CHECK(execute_fragment(host, count, &cpu) == 0);                                                               \
        CHECK(storage[0] == 0 && cpu.memory_written == 0);                                                             \
        CHECK(cpu.memory_first == 0 && cpu.memory_last == 0 && cpu.dirty_count == 0);                                  \
        CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == 2 && cpu.fault_size == 8);                  \
        CHECK(cpu.fault_address == UINT64_C(0x1000) && cpu.program == UINT64_C(0x45680000));                           \
    } while (0)

    publish_read_view(&cpu, 7, 7, 5, UINT64_C(0x1000), UINT64_C(0x1020), delta, 3);
    EXPECT_WRITE_FALLBACK(); /* a corrupt count cannot install an unowned entry */
    publish_read_view(&cpu, 0, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 3);
    EXPECT_WRITE_FALLBACK(); /* token-last publication is incomplete */
    publish_read_view(&cpu, 8, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 3);
    EXPECT_WRITE_FALLBACK(); /* stale incarnation */
    publish_read_view(&cpu, 7, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    EXPECT_WRITE_FALLBACK(); /* read-only view is never selected for a store */
#undef EXPECT_WRITE_FALLBACK

    /* An executable view still latches the sticky bit through the cached path. */
    memset(&cpu, 0, sizeof cpu);
    cpu.registers[3] = UINT64_C(0x1000);
    cpu.registers[1] = UINT64_C(0xfeedfacecafebeef);
    cpu.dirty_first = UINT64_MAX;
    publish_read_view(&cpu, 9, 9, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 7);
    CHECK(execute_fragment(host, count, &cpu) == 0);
    CHECK(storage[0] == UINT64_C(0xfeedfacecafebeef) && cpu.reason == 0);
    CHECK((cpu.executable_written & 4u) != 0);
    return 0;
}

static uint32_t scalar_cmpxchg_fragment(uint32_t *host) {
    instruction item;
    uint32_t cursor = 0;
    memset(&item, 0, sizeof item);
    item.pc = UINT64_C(0x45690000);
    item.operation = OP_CMPXCHG;
    item.source = 1;
    item.width = 8;
    item.address_base = 3;
    item.address_index = UINT8_MAX;
    hl_x86_emit_cmpxchg(host, &cursor, &item);
    CHECK(cursor == hl_x86_cmpxchg_words(&item));
    host[cursor++] = UINT32_C(0xd65f03c0);
    return cursor;
}

/* A store that selects a different published view must archive the outgoing
 * view's exact dirty interval before any projection state changes: append a
 * record the first time an owner is seen, merge into that record afterwards,
 * and re-arm the empty sentinel each time.  Nothing else exercises the archive,
 * append and merge blocks of the write cache. */
static int dirty_journal_contract(void) {
    _Alignas(16) uint64_t low[8] = {0};
    _Alignas(16) uint64_t high[8] = {0};
    uint64_t low_delta = (uint64_t)(uintptr_t)low - UINT64_C(0x1000);
    uint64_t high_delta = (uint64_t)(uintptr_t)high - UINT64_C(0x2000);
    uint32_t host[256] = {0};
    uint32_t count = scalar_write_fragment(host);
    hl_native_x86_64_cpu cpu;

    memset(&cpu, 0, sizeof cpu);
    cpu.registers[1] = UINT64_C(0x0123456789abcdef);
    cpu.dirty_first = UINT64_MAX;
    cpu.read_token = 21;
    cpu.read_incarnation = 21;
    cpu.read_count = 2;
    cpu.read_views[0][0] = UINT64_C(0x1000);
    cpu.read_views[0][1] = UINT64_C(0x1020);
    cpu.read_views[0][2] = low_delta;
    cpu.read_views[0][3] = 3;
    cpu.read_views[1][0] = UINT64_C(0x2000);
    cpu.read_views[1][1] = UINT64_C(0x2020);
    cpu.read_views[1][2] = high_delta;
    cpu.read_views[1][3] = 3;

    /* First store installs the low view and opens an exact interval. */
    cpu.registers[3] = UINT64_C(0x1000);
    CHECK(execute_fragment(host, count, &cpu) == 0);
    CHECK(cpu.reason == 0 && low[0] == UINT64_C(0x0123456789abcdef));
    CHECK(cpu.memory_first == UINT64_C(0x1000) && cpu.dirty_count == 0);
    CHECK(cpu.dirty_first == UINT64_C(0x1000) && cpu.dirty_last == UINT64_C(0x1008));

    /* A second store inside the already-active view must not archive anything:
     * it widens the open interval and leaves the journal alone.  This is the
     * common case, and mis-detecting it would archive on every store. */
    cpu.registers[3] = UINT64_C(0x1008);
    CHECK(execute_fragment(host, count, &cpu) == 0);
    CHECK(cpu.reason == 0 && low[1] == UINT64_C(0x0123456789abcdef));
    CHECK(cpu.dirty_count == 0); /* nothing was archived */
    CHECK(cpu.dirty_first == UINT64_C(0x1000) && cpu.dirty_last == UINT64_C(0x1010));
    CHECK(cpu.memory_first == UINT64_C(0x1000) && cpu.memory_last == UINT64_C(0x1020));

    /* Crossing to the high view archives the low view's interval verbatim. */
    cpu.registers[3] = UINT64_C(0x2000);
    CHECK(execute_fragment(host, count, &cpu) == 0);
    CHECK(cpu.reason == 0 && high[0] == UINT64_C(0x0123456789abcdef));
    CHECK(cpu.dirty_count == 1);
    CHECK(cpu.dirty_records[0][0] == UINT64_C(0x1000) && cpu.dirty_records[0][1] == UINT64_C(0x1020));
    CHECK(cpu.dirty_records[0][2] == UINT64_C(0x1000) && cpu.dirty_records[0][3] == UINT64_C(0x1010));
    CHECK(cpu.memory_first == UINT64_C(0x2000) && cpu.dirty_view_first == UINT64_C(0x2000));
    CHECK(cpu.dirty_first == UINT64_C(0x2000) && cpu.dirty_last == UINT64_C(0x2008));

    /* An unseen owner appends rather than merging into the existing record. */
    cpu.registers[3] = UINT64_C(0x1010);
    CHECK(execute_fragment(host, count, &cpu) == 0);
    CHECK(cpu.reason == 0 && low[2] == UINT64_C(0x0123456789abcdef));
    CHECK(cpu.dirty_count == 2);
    CHECK(cpu.dirty_records[1][0] == UINT64_C(0x2000) && cpu.dirty_records[1][1] == UINT64_C(0x2020));
    CHECK(cpu.dirty_records[1][2] == UINT64_C(0x2000) && cpu.dirty_records[1][3] == UINT64_C(0x2008));
    CHECK(cpu.dirty_first == UINT64_C(0x1010) && cpu.dirty_last == UINT64_C(0x1018));

    /* Returning to a known owner widens its record instead of appending. */
    cpu.registers[3] = UINT64_C(0x2010);
    CHECK(execute_fragment(host, count, &cpu) == 0);
    CHECK(cpu.reason == 0 && high[2] == UINT64_C(0x0123456789abcdef));
    CHECK(cpu.dirty_count == 2);
    CHECK(cpu.dirty_records[0][0] == UINT64_C(0x1000) && cpu.dirty_records[0][1] == UINT64_C(0x1020));
    CHECK(cpu.dirty_records[0][2] == UINT64_C(0x1000) && cpu.dirty_records[0][3] == UINT64_C(0x1018));
    CHECK(cpu.dirty_first == UINT64_C(0x2010) && cpu.dirty_last == UINT64_C(0x2018));

    /* A CMPXCHG whose comparison fails publishes no interval, so the sentinel
     * the archive re-armed is what C observes.  This is the only reachable
     * shape where that value is not immediately overwritten by the dirty
     * publication, and it must remain exactly UINT64_MAX. */
    {
        uint32_t swap[512] = {0};
        uint32_t swap_count = scalar_cmpxchg_fragment(swap);

        CHECK(swap_count != 0);
        memset(&cpu, 0, sizeof cpu);
        cpu.dirty_first = UINT64_MAX;
        cpu.read_token = 22;
        cpu.read_incarnation = 22;
        cpu.read_count = 2;
        cpu.read_views[0][0] = UINT64_C(0x1000);
        cpu.read_views[0][1] = UINT64_C(0x1020);
        cpu.read_views[0][2] = low_delta;
        cpu.read_views[0][3] = 3;
        cpu.read_views[1][0] = UINT64_C(0x2000);
        cpu.read_views[1][1] = UINT64_C(0x2020);
        cpu.read_views[1][2] = high_delta;
        cpu.read_views[1][3] = 3;

        /* Open an interval on the low view. */
        cpu.registers[1] = UINT64_C(0x0123456789abcdef);
        cpu.registers[3] = UINT64_C(0x1000);
        CHECK(execute_fragment(host, count, &cpu) == 0);
        CHECK(cpu.reason == 0 && cpu.dirty_first == UINT64_C(0x1000));

        /* Cross to the high view with a comparison that cannot succeed. */
        high[0] = UINT64_C(0x1111111111111111);
        cpu.registers[3] = UINT64_C(0x2000);
        cpu.registers[0] = UINT64_C(0xdeadbeefdeadbeef);
        cpu.registers[1] = UINT64_C(0x5555555555555555);
        CHECK(execute_fragment(swap, swap_count, &cpu) == 0);
        CHECK(cpu.reason == 0);
        CHECK(high[0] == UINT64_C(0x1111111111111111)); /* no architectural mutation */
        CHECK(cpu.memory_first == UINT64_C(0x2000));    /* the high view was installed */
        CHECK(cpu.dirty_count == 1);                    /* the low interval was archived */
        CHECK(cpu.dirty_records[0][0] == UINT64_C(0x1000) &&
              cpu.dirty_records[0][2] == UINT64_C(0x1000) &&
              cpu.dirty_records[0][3] == UINT64_C(0x1008));
        /* No interval was published, so the re-armed sentinel is what C sees. */
        CHECK(cpu.dirty_first == UINT64_MAX && cpu.dirty_last == 0);
    }
    return 0;
}

static int read_cache_contract(void) {
    _Alignas(16) uint64_t storage[4] = {UINT64_C(0x1122334455667788), UINT64_C(0x99aabbccddeeff00)};
    uint64_t delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x1000);
    uint32_t scalar[256] = {0};
    uint32_t vector[256] = {0};
    uint32_t control[256] = {0};
    uint32_t scalar_count = scalar_read_fragment(scalar);
    uint32_t vector_count =
        vector_fragment(vector, sizeof vector / sizeof vector[0], 0, 9u, 16u);
    hl_native_x86_64_cpu cpu;
    instruction item;
    uint32_t control_count = 0;

    memset(&cpu, 0, sizeof cpu);
    cpu.registers[3] = UINT64_C(0x1000);
    publish_read_view(&cpu, 7, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    CHECK(execute_fragment(scalar, scalar_count, &cpu) == 0);
    CHECK(cpu.registers[0] == storage[0] && cpu.reason == 0 && cpu.fault_access == 0);

#define EXPECT_SCALAR_FALLBACK()                                                                                       \
    do {                                                                                                               \
        cpu.registers[0] = UINT64_C(0xfeedface);                                                                       \
        cpu.reason = cpu.fault_access = cpu.fault_size = 0;                                                           \
        CHECK(execute_fragment(scalar, scalar_count, &cpu) == 0);                                                     \
        CHECK(cpu.registers[0] == UINT64_C(0xfeedface));                                                              \
        CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == 1 && cpu.fault_size == 8);                 \
    } while (0)

    publish_read_view(&cpu, 7, 7, 0, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    EXPECT_SCALAR_FALLBACK(); /* miss */
    publish_read_view(&cpu, 7, 7, 5, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    EXPECT_SCALAR_FALLBACK(); /* corrupt count cannot expose unowned entries */
    publish_read_view(&cpu, 8, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    EXPECT_SCALAR_FALLBACK(); /* stale/wrong incarnation */
    publish_read_view(&cpu, 0, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    EXPECT_SCALAR_FALLBACK(); /* token-last publication is incomplete */
    publish_read_view(&cpu, 7, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 2);
    EXPECT_SCALAR_FALLBACK(); /* write-only view */
    publish_read_view(&cpu, 7, 7, 1, 0, UINT64_MAX, delta, 1);
    cpu.registers[3] = UINT64_MAX - 3u;
    EXPECT_SCALAR_FALLBACK(); /* address plus width overflows */
    publish_read_view(&cpu, 7, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    cpu.registers[3] = UINT64_C(0x101c);
    EXPECT_SCALAR_FALLBACK(); /* access crosses the cached view */
#undef EXPECT_SCALAR_FALLBACK

    /* A retired token or a view that loses read must fall through to the latched
     * window, which stays authoritative for both success and fault delivery. */
    cpu.registers[3] = UINT64_C(0x1000);
    cpu.memory_first = UINT64_C(0x1000);
    cpu.memory_last = UINT64_C(0x1020);
    cpu.memory_delta = delta;
    cpu.memory_permissions = 1;
    publish_read_view(&cpu, 0, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    cpu.registers[0] = UINT64_C(0xfeedface);
    cpu.reason = cpu.fault_access = cpu.fault_size = 0;
    CHECK(execute_fragment(scalar, scalar_count, &cpu) == 0);
    CHECK(cpu.registers[0] == storage[0] && cpu.reason == 0 && cpu.fault_access == 0);

    publish_read_view(&cpu, 7, 7, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 0);
    cpu.registers[0] = UINT64_C(0xfeedface);
    cpu.reason = cpu.fault_access = cpu.fault_size = 0;
    CHECK(execute_fragment(scalar, scalar_count, &cpu) == 0);
    CHECK(cpu.registers[0] == storage[0] && cpu.reason == 0);

    cpu.memory_permissions = 2; /* the latched window loses read as the view did */
    cpu.registers[0] = UINT64_C(0xfeedface);
    cpu.reason = cpu.fault_access = cpu.fault_size = cpu.fault_address = 0;
    CHECK(execute_fragment(scalar, scalar_count, &cpu) == 0);
    CHECK(cpu.registers[0] == UINT64_C(0xfeedface));
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == 1 && cpu.fault_size == 8);
    CHECK(cpu.fault_address == UINT64_C(0x1000) && cpu.program == UINT64_C(0x45670000));

    memset(&cpu, 0, sizeof cpu);
    cpu.registers[3] = UINT64_C(0x1000);
    publish_read_view(&cpu, 11, 11, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    CHECK(execute_fragment(vector, vector_count, &cpu) == 0);
    CHECK(memcmp(&cpu.vectors[18], storage, 16) == 0 && cpu.reason == 0);

    memset(&item, 0, sizeof item);
    item.pc = UINT64_C(0x45671000);
    item.operation = OP_RETURN;
    item.width = 8;
    item.address_index = UINT8_MAX;
    hl_x86_emit_control(control, &control_count, &item);
    control[control_count++] = UINT32_C(0xd65f03c0);
    memset(&cpu, 0, sizeof cpu);
    cpu.registers[4] = UINT64_C(0x1000);
    publish_read_view(&cpu, 13, 13, 1, UINT64_C(0x1000), UINT64_C(0x1020), delta, 1);
    CHECK(execute_fragment(control, control_count, &cpu) == 0);
    CHECK(cpu.program == storage[0] && cpu.registers[4] == UINT64_C(0x1008) && cpu.reason == 0);
    return 0;
}
#else
static int read_cache_contract(void) { return 0; }
static int write_cache_contract(void) { return 0; }
static int dirty_journal_contract(void) { return 0; }
#endif

static int cumulative_checkpoint_contract(void) {
    const uint8_t first[] = {0xb8, 1, 0, 0, 0, 0x90}; /* mov eax,1; nop */
    const uint8_t second[] = {0x83, 0xc0, 1};          /* add eax,1 */
    uint32_t host[2][256] = {{0}};
    hl_x86_a64_provenance provenance[2][8] = {{{0}}};
    hl_x86_a64_result emitted[2];
    const uint8_t *guest[2] = {first, second};
    const size_t size[2] = {sizeof first, sizeof second};

    for (unsigned index = 0; index < 2; ++index) {
        hl_x86_a64_request request = request_for(guest[index], size[index], host[index], provenance[index]);
        request.host_capacity = sizeof host[index] / sizeof host[index][0];
        request.flags = HL_X86_A64_CHECKPOINTS;
        CHECK(hl_x86_a64_emit(&request, &emitted[index]) == HL_X86_A64_OK);
    }
    CHECK(emitted[0].instruction_count == 2 && emitted[1].instruction_count == 1);
#if defined(__aarch64__)
    {
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page * 2u, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        hl_native_x86_64_cpu cpu = {.scratch = {5}};

        CHECK(code != MAP_FAILED);
        for (unsigned index = 0; index < 2; ++index) {
            uint32_t *destination = (uint32_t *)(code + (size_t)page * index);
            memcpy(destination, host[index], emitted[index].word_count * sizeof(uint32_t));
            destination[emitted[index].word_count] = UINT32_C(0xd65f03c0);
        }
        __builtin___clear_cache((char *)code, (char *)code + (size_t)page * 2u);
        CHECK(mprotect(code, (size_t)page * 2u, PROT_READ | PROT_EXEC) == 0);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.scratch[0] == 7 && cpu.registers[0] == 1);
        hl_x86_test_enter(&cpu, code + page);
        CHECK(cpu.scratch[0] == 8 && cpu.registers[0] == 2);
        CHECK(munmap(code, (size_t)page * 2u) == 0);
    }
#endif
    return 0;
}

static int byte_flags_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const struct { uint8_t bytes[4]; size_t size; uint64_t value; unsigned kind; } cases[] = {
        {{0x80, 0xfc, 0x80, 0}, 3, UINT64_C(0x8000), 7u}, /* cmp ah, 0x80 */
        {{0x40, 0x80, 0xfc, 0x80}, 4, UINT64_C(0x80), 7u}, /* cmp spl, 0x80 */
        {{0x84, 0xe5, 0, 0}, 2, UINT64_C(0x8080), 4u}, /* test ch, ah */
        {{0x40, 0x84, 0xe5, 0}, 3, UINT64_C(0x80), 4u}, /* test bpl, spl */
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t index;

    CHECK(code != MAP_FAILED);
    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};
        uint64_t ignored;
        uint64_t expected;
        uint64_t left = index < 2u ? 0x80u : 0x80u;
        uint64_t right = 0x80u;

        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[index == 2u ? 1u : index == 3u ? 5u : index == 1u ? 4u : 0u] = cases[index].value;
        if (index == 2u) cpu.registers[0] = cases[index].value;
        if (index == 3u) cpu.registers[4] = cases[index].value;
        cpu.flags = UINT64_C(0xad7);
        alu_reference(cases[index].kind, 8u, left, right, cpu.flags, &ignored, &expected);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.flags == expected);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int byte_alu_frontend_contract(void) {
    static const uint8_t bases[] = {0x00, 0x08, 0x10, 0x18, 0x20, 0x28, 0x30, 0x38};
    uint32_t host[256];
    hl_x86_a64_provenance provenance[8];
    size_t operation;

    for (operation = 0; operation < sizeof bases / sizeof bases[0]; ++operation) {
        /* Both directions, register and memory, including legacy high bytes. */
        const uint8_t forms[][4] = {
            {bases[operation], 0xe5, 0, 0},             /* r/m8=ch, r8=ah */
            {(uint8_t)(bases[operation] + 2u), 0xe5, 0, 0}, /* r8=ah, r/m8=ch */
            {bases[operation], 0x23, 0, 0},             /* [rbx], ah */
            {(uint8_t)(bases[operation] + 2u), 0x23, 0, 0}, /* ah, [rbx] */
            {0x45, bases[operation], 0xc1, 0},          /* r9b, r8b */
            {0x40, bases[operation], 0xe5, 0},          /* bpl, spl */
        };
        const size_t sizes[] = {2, 2, 2, 2, 3, 3};
        size_t form;

        for (form = 0; form < sizeof forms / sizeof forms[0]; ++form) {
            hl_x86_a64_request request;
            hl_x86_a64_result emitted;

            memset(host, 0, sizeof host);
            memset(provenance, 0, sizeof provenance);
            request = request_for(forms[form], sizes[form], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            CHECK(emitted.instruction_count == 1 && emitted.exit == HL_X86_A64_FALLTHROUGH);
            CHECK(provenance[0].guest_pc == request.guest_pc);
            CHECK(provenance[0].guest_size == sizes[form]);
            CHECK(provenance[0].word_start == 0 && provenance[0].word_end > 0);
            CHECK(provenance[0].word_end <= emitted.word_count);
#if defined(__aarch64__)
            if (form < 2u) {
                long page = sysconf(_SC_PAGESIZE);
                uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
                hl_native_x86_64_cpu cpu = {0};
                uint64_t result;
                uint64_t flags;
                uint64_t prior = UINT64_C(0xad7) ^ (operation & 1u ? HL_X86_RFLAGS_CF : 0u);

                CHECK(code != MAP_FAILED);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[0] = UINT64_C(0x112233445566817f); /* ah=0x81 */
                cpu.registers[1] = UINT64_C(0x8877665544337f22); /* ch=0x7f */
                cpu.flags = prior;
                alu_reference((unsigned)operation, 8u, form == 0u ? 0x7fu : 0x81u,
                              form == 0u ? 0x81u : 0x7fu, prior, &result, &flags);
                hl_x86_test_enter(&cpu, code);
                if (operation != 7u) {
                    uint64_t actual = form == 0u ? cpu.registers[1] >> 8 : cpu.registers[0] >> 8;
                    CHECK((actual & 0xffu) == result);
                    CHECK((form == 0u ? cpu.registers[1] & ~UINT64_C(0xff00) :
                                        cpu.registers[0] & ~UINT64_C(0xff00)) ==
                          (form == 0u ? UINT64_C(0x8877665544330022) :
                                       UINT64_C(0x112233445566007f)));
                }
                CHECK(cpu.flags == flags);
                CHECK(munmap(code, (size_t)page) == 0);
            }
#endif
            request.host_capacity = 1u;
            memset(host, 0xa5, sizeof host);
            memset(&emitted, 0xa5, sizeof emitted);
            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_CAPACITY);
            CHECK(host[0] == UINT32_C(0xa5a5a5a5));
        }
    }
    return 0;
}

static int byte_alu_memory_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const uint8_t bases[] = {0x00, 0x08, 0x10, 0x18, 0x20, 0x28, 0x30, 0x38};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t kind;

    CHECK(code != MAP_FAILED);
    for (kind = 0; kind < sizeof bases / sizeof bases[0]; ++kind) {
        unsigned rex_low;
        unsigned memory_source;
        for (rex_low = 0; rex_low < 2u; ++rex_low) {
            for (memory_source = 0; memory_source < 2u; ++memory_source) {
                uint8_t guest[3] = {0};
                size_t size = 0;
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result emitted;
                hl_x86_a64_request request;
                hl_native_x86_64_cpu cpu = {0};
                uint8_t memory = 0x7f;
                uint64_t result;
                uint64_t flags;
                uint64_t prior = UINT64_C(0xad6) | ((kind == 2u || kind == 3u) ? 1u : 0u);
                unsigned reg = rex_low ? 4u : 0u;

                if (rex_low) guest[size++] = 0x40;
                guest[size++] = (uint8_t)(bases[kind] + (memory_source ? 2u : 0u));
                guest[size++] = 0x23; /* byte register 4 and [rbx] */
                request = request_for(guest, size, host, provenance);
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[3] = 0x2000;
                cpu.registers[reg] = rex_low ? UINT64_C(0x1122334455667781) :
                                               UINT64_C(0x1122334455668177);
                cpu.memory_first = 0x2000;
                cpu.memory_last = 0x2001;
                cpu.memory_delta = (uint64_t)(uintptr_t)&memory - 0x2000;
                cpu.memory_permissions = 7;
                cpu.dirty_first = UINT64_MAX;
                cpu.flags = prior;
                alu_reference((unsigned)kind, 8u, memory_source ? 0x81u : 0x7fu,
                              memory_source ? 0x7fu : 0x81u, prior, &result, &flags);
                hl_x86_test_enter(&cpu, code);
                CHECK(cpu.flags == flags);
                if (memory_source) {
                    uint64_t actual = rex_low ? cpu.registers[4] : cpu.registers[0] >> 8;
                    if (kind != 7u) CHECK((actual & 0xffu) == result);
                    CHECK(cpu.registers[reg] ==
                          (kind == 7u ? (rex_low ? UINT64_C(0x1122334455667781) :
                                                  UINT64_C(0x1122334455668177)) :
                           rex_low ? UINT64_C(0x1122334455667700) | result :
                                     UINT64_C(0x1122334455660077) | result << 8));
                    CHECK(memory == 0x7f && cpu.memory_written == 0);
                    CHECK(cpu.dirty_first == UINT64_MAX && cpu.executable_written == 0);
                } else if (kind == 7u) {
                    CHECK(memory == 0x7f && cpu.memory_written == 0);
                    CHECK(cpu.dirty_first == UINT64_MAX && cpu.executable_written == 0);
                } else {
                    CHECK(memory == (uint8_t)result && cpu.memory_written == 1);
                    CHECK(cpu.dirty_first == 0x2000 && cpu.dirty_last == 0x2001);
                    CHECK((cpu.executable_written & 4u) != 0u);
                }
                if (!memory_source)
                    CHECK(cpu.registers[reg] == (rex_low ? UINT64_C(0x1122334455667781) :
                                                           UINT64_C(0x1122334455668177)));
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    /* A denied write and an out-of-range read both exit before any architectural mutation. */
    {
        static const uint8_t denied[] = {0x00, 0x23}; /* add ah to byte [rbx] */
        static const uint8_t missing[] = {0x02, 0x23}; /* add byte [rbx] to ah */
        const uint8_t *forms[] = {denied, missing};
        unsigned fault;
        for (fault = 0; fault < 2u; ++fault) {
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[8] = {0};
            hl_x86_a64_result emitted;
            hl_x86_a64_request request = request_for(forms[fault], 2, host, provenance);
            hl_native_x86_64_cpu cpu = {0};
            uint8_t memory = 0x7f;
            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            cpu.registers[0] = UINT64_C(0x1122334455668177);
            cpu.registers[3] = 0x2000;
            cpu.memory_first = fault ? 0x2001 : 0x2000;
            cpu.memory_last = fault ? 0x2002 : 0x2001;
            cpu.memory_delta = (uint64_t)(uintptr_t)&memory - 0x2000;
            cpu.memory_permissions = fault ? 3u : 1u;
            cpu.dirty_first = UINT64_MAX;
            cpu.flags = UINT64_C(0xad7);
            hl_x86_test_enter(&cpu, code);
            CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK);
            CHECK(cpu.fault_access == (fault ? HL_NATIVE_ACCESS_READ :
                                               HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE));
            CHECK(cpu.fault_size == 1 && cpu.fault_address == 0x2000);
            CHECK(memory == 0x7f && cpu.registers[0] == UINT64_C(0x1122334455668177));
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.memory_written == 0);
            CHECK(cpu.dirty_first == UINT64_MAX && cpu.executable_written == 0);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int vector_immediate_staging_contract(void) {
    instruction shape = {0};
    instruction item = {0};
    uint32_t words[256] = {0};
    uint32_t cursor = 0;

    hl_x86_prepare_vector_immediate(&shape, 0xf1, 5u, 0x83, 1u);
    CHECK(shape.vector_immediate_form == VECTOR_IMMEDIATE_RM_DESTRUCTIVE);
    CHECK(shape.vector_subopcode == 6u && shape.destination == 9u);
    CHECK(shape.vector_immediate == 0x83);
    hl_x86_prepare_vector_immediate(&shape, 0xd1, 4u, 7u, 0u);
    CHECK(shape.vector_immediate_form == VECTOR_IMMEDIATE_REG_DESTINATION);
    CHECK(shape.vector_subopcode == 2u && shape.destination == 10u);

    {
        static const uint8_t unsupported[][5] = {
            {0x0f, 0x71, 0xd1, 1, 0},       /* bare MMX shift */
            {0x66, 0x0f, 0xc4, 0xc1, 0},   /* future XMM PINSRW */
        };
        static const size_t sizes[] = {4, 5};
        unsigned index;
        for (index = 0; index < 2u; ++index) {
            uint32_t host[64] = {0};
            hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(unsupported[index], sizes[index], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
            CHECK(result.exit == HL_X86_A64_INTERPRETER && result.instruction_count == 0);
            CHECK(result.exit_pc == request.guest_pc);
        }
    }

    item.pc = UINT64_C(0x400000);
    item.operation = OP_VECTOR;
    item.vector_kind = VECTOR_INSERT_WORD;
    item.vector_immediate_form = VECTOR_IMMEDIATE_REG_DESTINATION;
    item.vector_immediate = 7u;
    item.vector_memory_width = 2u;
    item.width = 16u;
    item.destination = 0u;
    item.source = 16u;
    item.memory_operand = 1u;
    item.address_base = 3u;
    item.address_index = UINT8_MAX;
    CHECK(hl_x86_vector_words(&item) < sizeof words / sizeof words[0]);
    hl_x86_emit_vector(words, &cursor, &item);
    CHECK(cursor == hl_x86_vector_words(&item));
#if defined(__aarch64__)
    {
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *data = mmap(NULL, (size_t)page * 2u, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        hl_native_x86_64_cpu cpu = {0};
        CHECK(data != MAP_FAILED && code != MAP_FAILED);
        data[page - 2] = 0x34;
        data[page - 1] = 0x12;
        CHECK(mprotect(data + page, (size_t)page, PROT_NONE) == 0);
        memcpy(code, words, cursor * sizeof(uint32_t));
        ((uint32_t *)code)[cursor] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (cursor + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[3] = 0x2000;
        cpu.memory_first = 0x2000;
        cpu.memory_last = 0x2002;
        cpu.memory_delta = (uint64_t)(uintptr_t)(data + page - 2) - 0x2000;
        cpu.memory_permissions = 1u;
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.reason == 0 && cpu.fault_access == 0); /* exact LDRH did not cross the guard page */
        memset(&cpu, 0, sizeof cpu);
        cpu.registers[3] = 0x2000;
        cpu.memory_first = 0x2000;
        cpu.memory_last = 0x2001;
        cpu.memory_delta = (uint64_t)(uintptr_t)(data + page - 2) - 0x2000;
        cpu.memory_permissions = 1u;
        cpu.vectors[0] = UINT64_C(0xfeedfacecafebeef);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == HL_NATIVE_ACCESS_READ);
        CHECK(cpu.fault_address == 0x2000 && cpu.fault_size == 2u);
        CHECK(cpu.vectors[0] == UINT64_C(0xfeedfacecafebeef) && cpu.memory_written == 0);
        CHECK(munmap(code, (size_t)page) == 0);
        CHECK(munmap(data, (size_t)page * 2u) == 0);
    }
#endif
    return 0;
}

static int shuffle_family_contract(void) {
    static const uint8_t forms[][6] = {
        {0x66, 0x0f, 0x70, 0xc1, 0x1b, 0}, /* pshufd xmm0,xmm1,0x1b */
        {0xf2, 0x0f, 0x70, 0xc1, 0x1b, 0}, /* pshuflw */
        {0xf3, 0x0f, 0x70, 0xc1, 0x1b, 0}, /* pshufhw */
        {0x0f, 0xc6, 0xc1, 0x4e, 0, 0},    /* shufps */
        {0x66, 0x0f, 0xc6, 0xc1, 0x01, 0}, /* shufpd */
        {0x66, 0x0f, 0x70, 0x03, 0x39, 0}, /* pshufd xmm0,[rbx],0x39 */
        {0x0f, 0xc6, 0x43, 0x10, 0xe4, 0}, /* shufps xmm0,[rbx+16],0xe4 */
    };
    static const size_t sizes[] = {5, 5, 5, 4, 5, 5, 5};
    unsigned index;
    for (index = 0; index < sizeof forms / sizeof forms[0]; ++index) {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(forms[index], sizes[index], host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.instruction_count == 1);
        CHECK(provenance[0].guest_size == sizes[index]);
        CHECK(provenance[0].word_end > provenance[0].word_start);
    }
    {
        static const uint8_t truncated[][4] = {
            {0x66, 0x0f, 0x70, 0xc1}, {0x0f, 0xc6, 0xc1, 0},
        };
        static const size_t sizes_short[] = {4, 3};
        for (index = 0; index < 2u; ++index) {
            uint32_t host[32] = {0};
            hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(truncated[index], sizes_short[index], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
            CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
        }
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0x66, 0x0f, 0x70, 0xc1, 0x1b};
        static const uint32_t source[] = {0x11223344, 0x55667788, 0x99aabbcc, 0xddeeff00};
        static const uint32_t expected[] = {0xddeeff00, 0x99aabbcc, 0x55667788, 0x11223344};
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_native_x86_64_cpu cpu = {0};
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[2], source, sizeof source);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
        CHECK(memcmp(&cpu.vectors[2], source, sizeof source) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_shuffle_family_contract(void) {
    static const uint8_t forms[][6] = {
        {0xc5, 0xf9, 0x70, 0xc1, 0x1b, 0}, /* vpshufd xmm0,xmm1,imm */
        {0xc5, 0xfd, 0x70, 0xc1, 0x1b, 0}, /* vpshufd ymm0,ymm1,imm */
        {0xc5, 0xfb, 0x70, 0xc1, 0x39, 0}, /* vpshuflw */
        {0xc5, 0xfe, 0x70, 0xc1, 0x39, 0}, /* vpshufhw ymm */
        {0xc5, 0xe8, 0xc6, 0xc1, 0x4e, 0}, /* vshufps xmm0,xmm2,xmm1,imm */
        {0xc5, 0xed, 0xc6, 0xc1, 0x4e, 0}, /* vshufpd ymm0,ymm2,ymm1,imm */
        {0xc4, 0xe1, 0xf9, 0x70, 0xc1, 0x1b}, /* C4 WIG vpshufd */
        {0xc5, 0xfd, 0x70, 0x03, 0x1b, 0}, /* vpshufd ymm0,[rbx],imm */
    };
    static const size_t sizes[] = {5, 5, 5, 5, 5, 5, 6, 5};
    static const uint8_t invalid[][6] = {
        {0xc5, 0xf1, 0x70, 0xc1, 0, 0}, /* reserved vvvv */
        {0xc5, 0xf8, 0x70, 0xc1, 0, 0}, /* invalid pp */
        {0xc5, 0xea, 0xc6, 0xc1, 0, 0}, /* invalid pp */
        {0xc4, 0xe2, 0x79, 0x70, 0xc1, 0}, /* invalid map */
    };
    unsigned index;
    for (index = 0; index < sizeof forms / sizeof forms[0]; ++index) {
        uint32_t host[512] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(forms[index], sizes[index], host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
        CHECK(provenance[0].guest_size == sizes[index]);
    }
    for (index = 0; index < sizeof invalid / sizeof invalid[0]; ++index) {
        uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(invalid[index],
                                                  index == 3u ? 6u : 5u, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
    }
    return 0;
}

static int vex_signed_dword_to_float_contract(void) {
    static const uint8_t forms[][6] = {
        {0xc5, 0xf8, 0x5b, 0xc1, 0, 0},       /* vcvtdq2ps xmm0,xmm1 */
        {0xc5, 0xfc, 0x5b, 0xc1, 0, 0},       /* vcvtdq2ps ymm0,ymm1 */
        {0xc4, 0xe1, 0xf8, 0x5b, 0xc1, 0},    /* C4 WIG */
        {0xc5, 0xfc, 0x5b, 0x03, 0, 0},       /* vcvtdq2ps ymm0,[rbx] */
        {0xc5, 0xf9, 0x5b, 0xc1, 0, 0},       /* vcvtps2dq xmm0,xmm1 */
        {0xc5, 0xfd, 0x5b, 0xc1, 0, 0},       /* vcvtps2dq ymm0,ymm1 */
        {0xc4, 0xe1, 0x7a, 0x5b, 0xc1, 0},    /* C4 WIG vcvttps2dq */
        {0xc5, 0xfe, 0x5b, 0x03, 0, 0},       /* vcvttps2dq ymm0,[rbx] */
    };
    static const size_t sizes[] = {4, 4, 5, 4, 4, 4, 5, 4};
    static const uint8_t invalid[][6] = {
        {0xc5, 0xe8, 0x5b, 0xc1, 0, 0},       /* reserved vvvv */
        {0xc5, 0xfb, 0x5b, 0xc1, 0, 0},       /* reserved F2 pp */
        {0xc4, 0xe2, 0x78, 0x5b, 0xc1, 0},    /* invalid map */
    };
    unsigned index;
    for (index = 0; index < sizeof forms / sizeof forms[0]; ++index) {
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(forms[index], sizes[index], host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
        CHECK(provenance[0].guest_size == sizes[index]);
    }
    for (index = 0; index < sizeof invalid / sizeof invalid[0]; ++index) {
        uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(invalid[index], index == 2u ? 6u : 4u,
                                                  host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0xc5, 0xfc, 0x5b, 0xc1};
        static const int32_t source[] = {0, 1, -1, 16777217, -16777217, INT32_MAX, INT32_MIN, 42};
        static const uint32_t expected[] = {
            0, 0x3f800000, 0xbf800000, 0x4b800000,
            0xcb800000, 0x4f000000, 0xcf000000, 0x42280000,
        };
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_native_x86_64_cpu cpu = {0};
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[2], source, 16); memcpy(&cpu.vector_upper[2], source + 4, 16);
        cpu.flags = UINT64_C(0xad7);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[0], expected + 4, 16) == 0);
        CHECK(memcmp(&cpu.vectors[2], source, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[2], source + 4, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && (cpu.fpsr & UINT64_C(0x10)) != 0);
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_packed_compare_contract(void) {
    static const uint8_t forms[][6] = {
        {0xc5, 0xe9, 0x74, 0xc1, 0, 0}, {0xc5, 0xed, 0x75, 0xc1, 0, 0},
        {0xc5, 0xe9, 0x76, 0x03, 0, 0}, {0xc5, 0xed, 0x64, 0xc1, 0, 0},
        {0xc5, 0xe9, 0x65, 0xc1, 0, 0}, {0xc5, 0xed, 0x66, 0x03, 0, 0},
        {0xc4, 0xe2, 0x69, 0x29, 0xc1, 0}, {0xc4, 0xe2, 0x6d, 0x29, 0x03, 0},
        {0xc4, 0xe2, 0x69, 0x37, 0xc1, 0}, {0xc4, 0xe2, 0x6d, 0x37, 0x03, 0},
    };
    static const size_t sizes[] = {4, 4, 4, 4, 4, 4, 5, 5, 5, 5};
    static const uint8_t invalid[][6] = {
        {0xc5, 0xe8, 0x74, 0xc1, 0, 0},
        {0xc4, 0xe3, 0x69, 0x29, 0xc1, 0},
        {0xc4, 0xe2, 0x6a, 0x37, 0xc1, 0},
    };
    unsigned index;
    for (index = 0; index < sizeof forms / sizeof forms[0]; ++index) {
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(forms[index], sizes[index], host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
        CHECK(provenance[0].guest_size == sizes[index]);
    }
    for (index = 0; index < sizeof invalid / sizeof invalid[0]; ++index) {
        uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(invalid[index], index == 0u ? 4u : 5u,
                                                  host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0xc5, 0xed, 0x76, 0x03};
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0}; uint8_t data[32] = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[3] = 0x4000; cpu.memory_first = 0x4000; cpu.memory_last = 0x401f;
        cpu.memory_delta = (uint64_t)(uintptr_t)data - 0x4000; cpu.memory_permissions = 1u;
        cpu.vectors[0] = UINT64_C(0xfeedfacecafebeef);
        cpu.vector_upper[0] = UINT64_C(0x0123456789abcdef);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == HL_NATIVE_ACCESS_READ);
        CHECK(cpu.fault_address == 0x4000 && cpu.fault_size == 32u);
        CHECK(cpu.vectors[0] == UINT64_C(0xfeedfacecafebeef));
        CHECK(cpu.vector_upper[0] == UINT64_C(0x0123456789abcdef));
        CHECK(munmap(code, (size_t)page) == 0);
    }
    {
        static const uint8_t guest[] = {0xc4, 0xe2, 0x69, 0x29, 0xc1};
        uint32_t host[128] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.vectors[4] = 7; cpu.vectors[5] = 8; cpu.vectors[2] = 7; cpu.vectors[3] = 9;
        cpu.vector_upper[0] = UINT64_MAX; cpu.vector_upper[1] = UINT64_MAX;
        hl_native_x86_64_enter(&cpu, code);
        CHECK(cpu.vectors[0] == UINT64_MAX && cpu.vectors[1] == 0);
        CHECK(cpu.vector_upper[0] == 0 && cpu.vector_upper[1] == 0);
        CHECK(cpu.vectors[4] == 7 && cpu.vectors[5] == 8 && cpu.vectors[2] == 7 && cpu.vectors[3] == 9);
        CHECK(munmap(code, (size_t)page) == 0);
    }
    {
        static const uint8_t guest[] = {0xc5, 0xed, 0x66, 0xc1};
        static const int32_t left[8] = {INT32_MIN, -1, 0, 1, 7, 9, INT32_MAX, -8};
        static const int32_t right[8] = {-1, -1, -1, 2, 6, 10, INT32_MIN, -9};
        static const uint32_t expected[8] = {0, 0, UINT32_MAX, 0, UINT32_MAX, 0, UINT32_MAX, UINT32_MAX};
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[4], left, 16); memcpy(&cpu.vector_upper[4], left + 4, 16);
        memcpy(&cpu.vectors[2], right, 16); memcpy(&cpu.vector_upper[2], right + 4, 16);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[0], expected + 4, 16) == 0);
        CHECK(memcmp(&cpu.vectors[4], left, 16) == 0 && memcmp(&cpu.vectors[2], right, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_packed_sign_absolute_contract(void) {
    static const uint8_t opcodes[] = {0x08, 0x09, 0x0a, 0x1c, 0x1d, 0x1e};
    unsigned operation;

    for (operation = 0; operation < sizeof opcodes; ++operation) {
        unsigned wide;
        for (wide = 0; wide < 2u; ++wide) {
            unsigned memory;
            for (memory = 0; memory < 2u; ++memory) {
                uint8_t prefix = operation < 3u ? (wide != 0u ? 0x6du : 0x69u) :
                                                  (wide != 0u ? 0x7du : 0x79u);
                uint8_t guest[] = {0xc4, 0xe2, prefix, opcodes[operation],
                                   memory != 0u ? 0x0bu : 0xcbu};
                uint32_t host[256]; uint32_t exact[256]; uint32_t short_host[256];
                hl_x86_a64_provenance provenance[2]; hl_x86_a64_provenance exact_provenance[2];
                hl_x86_a64_provenance short_provenance[2];
                hl_x86_a64_result emitted; hl_x86_a64_result exact_result; hl_x86_a64_result short_result;
                hl_x86_a64_request request;
                memset(host, 0xa5, sizeof host); memset(provenance, 0x5a, sizeof provenance);
                request = request_for(guest, sizeof guest, host, provenance);
                request.max_instructions = 1u;
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                CHECK(emitted.exit == HL_X86_A64_FALLTHROUGH && emitted.instruction_count == 1u);
                CHECK(provenance[0].guest_size == sizeof guest && provenance[0].word_end > provenance[0].word_start);
                {
                    uint32_t capacity;
                    for (capacity = 0u; capacity < sizeof exact / sizeof exact[0]; ++capacity) {
                        memset(exact, 0xa5, sizeof exact);
                        memset(exact_provenance, 0x5a, sizeof exact_provenance);
                        memset(&exact_result, 0x3c, sizeof exact_result);
                        request.host_words = exact; request.host_capacity = capacity;
                        request.provenance = exact_provenance;
                        if (hl_x86_a64_emit(&request, &exact_result) == HL_X86_A64_OK) break;
                        { uint32_t sentinel[256]; hl_x86_a64_provenance ps[2]; hl_x86_a64_result rs;
                          memset(sentinel, 0xa5, sizeof sentinel); memset(ps, 0x5a, sizeof ps); memset(&rs, 0x3c, sizeof rs);
                          CHECK(memcmp(exact, sentinel, sizeof sentinel) == 0);
                          CHECK(memcmp(exact_provenance, ps, sizeof ps) == 0);
                          CHECK(memcmp(&exact_result, &rs, sizeof rs) == 0); }
                    }
                    CHECK(capacity < sizeof exact / sizeof exact[0] && capacity > 0u);
                    CHECK(exact_result.instruction_count == 1u && exact_result.exit == HL_X86_A64_FALLTHROUGH);
                    request.host_capacity = capacity - 1u;
                }
                memset(short_host, 0xa5, sizeof short_host); memset(short_provenance, 0x5a, sizeof short_provenance);
                memset(&short_result, 0x3c, sizeof short_result);
                request.host_words = short_host;
                request.provenance = short_provenance;
                CHECK(hl_x86_a64_emit(&request, &short_result) == HL_X86_A64_CAPACITY);
                { uint32_t sentinel[256]; hl_x86_a64_provenance ps[2]; hl_x86_a64_result rs;
                  memset(sentinel, 0xa5, sizeof sentinel); memset(ps, 0x5a, sizeof ps); memset(&rs, 0x3c, sizeof rs);
                  CHECK(memcmp(short_host, sentinel, sizeof sentinel) == 0);
                  CHECK(memcmp(short_provenance, ps, sizeof ps) == 0);
                  CHECK(memcmp(&short_result, &rs, sizeof rs) == 0); }
            }
        }
    }
    {
        static const uint8_t invalid[][5] = {
            {0xc4, 0xe2, 0x6c, 0x08, 0xcb}, /* pp != 66 */
            {0xc4, 0xe1, 0x69, 0x08, 0xcb}, /* wrong map */
            {0xc4, 0xe2, 0x71, 0x1c, 0xcb}, /* vpabs reserved vvvv != 1111 */
        };
        unsigned index;
        for (index = 0; index < sizeof invalid / sizeof invalid[0]; ++index) {
            uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(invalid[index], sizeof invalid[index], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
            CHECK(result.exit == HL_X86_A64_INTERPRETER && result.instruction_count == 0u);
        }
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0xc4, 0xe2, 0x6d, 0x0a, 0xc1};
        static const int32_t values[8] = {INT32_MIN, -9, -1, 0, 1, 7, INT32_MAX, 31};
        static const int32_t controls[8] = {-1, 0, 1, INT32_MIN, INT32_MAX, -7, 0, 1};
        static const int32_t expected[8] = {INT32_MIN, 0, -1, 0, 1, -7, 0, 31};
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[4], values, 16); memcpy(&cpu.vector_upper[4], values + 4, 16);
        memcpy(&cpu.vectors[2], controls, 16); memcpy(&cpu.vector_upper[2], controls + 4, 16);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[0], expected + 4, 16) == 0);
        CHECK(memcmp(&cpu.vectors[4], values, 16) == 0 && memcmp(&cpu.vectors[2], controls, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
    {
        static const uint8_t guest[] = {0xc4, 0xe2, 0x79, 0x1c, 0xc0};
        static const int8_t input[16] = {INT8_MIN, -127, -2, -1, 0, 1, 2, 127,
                                         -9, 9, -64, 64, -3, 3, -100, 100};
        static const uint8_t expected[16] = {128, 127, 2, 1, 0, 1, 2, 127,
                                             9, 9, 64, 64, 3, 3, 100, 100};
        uint32_t host[128] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[0], input, sizeof input);
        cpu.vector_upper[0] = UINT64_MAX; cpu.vector_upper[1] = UINT64_MAX;
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, sizeof expected) == 0);
        CHECK(cpu.vector_upper[0] == 0 && cpu.vector_upper[1] == 0);
        CHECK(munmap(code, (size_t)page) == 0);
    }
    {
        static const uint8_t guest[] = {0xc4, 0xe2, 0x7d, 0x1e, 0x03};
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0}; uint8_t data[32] = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[3] = 0x4000; cpu.memory_first = 0x4000; cpu.memory_last = 0x401f;
        cpu.memory_delta = (uint64_t)(uintptr_t)data - 0x4000; cpu.memory_permissions = 1u;
        cpu.vectors[0] = UINT64_C(0xfeedfacecafebeef);
        cpu.vector_upper[0] = UINT64_C(0x0123456789abcdef);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.fault_access == HL_NATIVE_ACCESS_READ);
        CHECK(cpu.fault_address == 0x4000 && cpu.fault_size == 32u);
        CHECK(cpu.vectors[0] == UINT64_C(0xfeedfacecafebeef));
        CHECK(cpu.vector_upper[0] == UINT64_C(0x0123456789abcdef));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_packed_extrema_contract(void) {
    static const uint8_t map_one[] = {0xda, 0xde, 0xea, 0xee};
    static const uint8_t map_two[] = {0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f};
    unsigned wide, memory, index;
    for (wide = 0; wide < 2; ++wide) for (memory = 0; memory < 2; ++memory) {
        for (index = 0; index < sizeof map_one; ++index) {
            uint8_t guest[] = {0xc5, (uint8_t)(0xe9u | wide * 4u), map_one[index],
                               (uint8_t)(memory != 0u ? 0x03u : 0xc1u)};
            uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0}; hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
        }
        for (index = 0; index < sizeof map_two; ++index) {
            uint8_t guest[] = {0xc4, 0xe2, (uint8_t)(0x69u | wide * 4u), map_two[index],
                               (uint8_t)(memory != 0u ? 0x03u : 0xc1u)};
            uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0}; hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
        }
    }
    return 0;
}

static int vex_map2_multiply_contract(void) {
    static const uint8_t forms[][5] = {
        {0xc4, 0xe2, 0x69, 0x28, 0xc1}, /* vpmuldq xmm0,xmm2,xmm1 */
        {0xc4, 0xe2, 0x69, 0x28, 0x03}, /* vpmuldq xmm0,xmm2,[rbx] */
        {0xc4, 0xe2, 0x6d, 0x28, 0xc1}, /* vpmuldq ymm0,ymm2,ymm1 */
        {0xc4, 0xe2, 0x6d, 0x28, 0x03}, /* vpmuldq ymm0,ymm2,[rbx] */
        {0xc4, 0xe2, 0xe9, 0x40, 0xc1}, /* WIG vpmulld xmm0,xmm2,xmm1 */
        {0xc4, 0xe2, 0xe9, 0x40, 0x03}, /* WIG vpmulld xmm0,xmm2,[rbx] */
        {0xc4, 0xe2, 0x7d, 0x40, 0xc1}, /* vpmulld ymm0,ymm0,ymm1; dst aliases vvvv */
        {0xc4, 0xe2, 0x75, 0x40, 0xc0}, /* vpmulld ymm0,ymm1,ymm0; dst aliases rm */
    };
    static const uint8_t invalid[][5] = {
        {0xc4, 0xe1, 0x69, 0x28, 0xc1}, /* wrong map */
        {0xc4, 0xe2, 0x68, 0x28, 0xc1}, /* wrong pp */
        {0xc5, 0xe9, 0x28, 0xc1, 0},    /* C5 cannot select map two */
    };
    unsigned index;
    for (index = 0; index < sizeof forms / sizeof forms[0]; ++index) {
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(forms[index], sizeof forms[index], host, provenance);
        request.max_instructions = 1u;
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
        {
            uint32_t exact[256]; uint32_t short_output[256];
            hl_x86_a64_result exact_result; hl_x86_a64_result untouched;
            memset(exact, 0xa5, sizeof exact);
            request.host_words = exact; request.host_capacity = result.word_count;
            CHECK(hl_x86_a64_emit(&request, &exact_result) == HL_X86_A64_OK);
            CHECK(exact_result.word_count == result.word_count);
            CHECK(memcmp(exact, host, result.word_count * sizeof host[0]) == 0);
            memset(short_output, 0xa5, sizeof short_output);
            memset(&untouched, 0xa5, sizeof untouched);
            {
                hl_x86_a64_provenance short_provenance[2];
                hl_x86_a64_request short_request =
                    request_for(forms[index], sizeof forms[index], short_output, short_provenance);
                short_request.max_instructions = 1u;
                short_request.host_capacity = (index == 1u || index == 3u || index == 5u) ?
                                                  1u : result.word_count - 1u;
                CHECK(hl_x86_a64_emit(&short_request, &untouched) == HL_X86_A64_CAPACITY);
            }
            CHECK(short_output[0] == UINT32_C(0xa5a5a5a5));
            CHECK(untouched.abi == UINT32_C(0xa5a5a5a5));
        }
    }
    for (index = 0; index < sizeof invalid / sizeof invalid[0]; ++index) {
        uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        size_t size = index == 2u ? 4u : 5u;
        hl_x86_a64_request request = request_for(invalid[index], size, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0xc4, 0xe2, 0x6d, 0x28, 0xc1};
        static const int32_t left[8] = {-3, 99, INT32_MIN, 88, 7, 77, -9, 66};
        static const int32_t right[8] = {5, 55, -1, 44, -11, 33, INT32_MIN, 22};
        static const int64_t expected[4] = {-15, INT64_C(2147483648), -77,
                                             INT64_C(19327352832)};
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[4], left, 16); memcpy(&cpu.vector_upper[4], left + 4, 16);
        memcpy(&cpu.vectors[2], right, 16); memcpy(&cpu.vector_upper[2], right + 4, 16);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[0], expected + 2, 16) == 0);
        CHECK(memcmp(&cpu.vectors[4], left, 16) == 0 && memcmp(&cpu.vectors[2], right, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
    {
        static const uint8_t guest[] = {0xc4, 0xe2, 0x7d, 0x40, 0xc1};
        static const uint32_t left[8] = {UINT32_MAX, 3, 0x80000000, 7, 9, 11, 13, 15};
        static const uint32_t right[8] = {2, UINT32_MAX, 3, 0x40000000, 5, 7, 9, 11};
        static const uint32_t expected[8] = {UINT32_MAX - 1u, UINT32_MAX - 2u, 0x80000000, 0xc0000000,
                                              45, 77, 117, 165};
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[0], left, 16); memcpy(&cpu.vector_upper[0], left + 4, 16);
        memcpy(&cpu.vectors[2], right, 16); memcpy(&cpu.vector_upper[2], right + 4, 16);
        cpu.flags = UINT64_C(0xad7);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[0], expected + 4, 16) == 0);
        CHECK(memcmp(&cpu.vectors[2], right, 16) == 0 && cpu.flags == UINT64_C(0xad7));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_packed_unpack_contract(void) {
    static const uint8_t opcodes[] = {0x60, 0x61, 0x62, 0x6c, 0x68, 0x69, 0x6a, 0x6d};
    unsigned operation;
    unsigned wide;

    for (operation = 0; operation < sizeof opcodes / sizeof opcodes[0]; ++operation) {
        for (wide = 0; wide < 2u; ++wide) {
            unsigned memory;
            for (memory = 0; memory < 2u; ++memory) {
                uint8_t guest[] = {0xc5, (uint8_t)(wide != 0u ? 0xe5u : 0xe1u),
                                   opcodes[operation], (uint8_t)(memory != 0u ? 0x03u : 0xc1u)};
                uint32_t host[160] = {0};
                hl_x86_a64_provenance provenance[2] = {0};
                hl_x86_a64_result result;
                hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
                CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
                CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
                CHECK(provenance[0].guest_size == sizeof guest && result.word_count > 0u);
                request.host_capacity = result.word_count;
                CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
                memset(host, 0xa5, sizeof host);
                request.host_capacity = result.word_count - 1u;
                CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
                CHECK(host[0] == UINT32_C(0xa5a5a5a5));
            }
        }
    }
    {
        static const uint8_t memory[] = {0xc5, 0xe5, 0x62, 0x03};
        static const uint8_t extended[] = {0xc4, 0x61, 0x65, 0x6d, 0xc1};
        const uint8_t *forms[] = {memory, extended};
        const size_t sizes[] = {sizeof memory, sizeof extended};
        for (operation = 0; operation < 2u; ++operation) {
            uint32_t host[160] = {0}; hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(forms[operation], sizes[operation], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        }
    }
    {
        static const uint8_t invalid[][5] = {
            {0xc5, 0xe4, 0x60, 0xc1, 0},
            {0xc4, 0xe2, 0x65, 0x60, 0xc1},
        };
        static const size_t sizes[] = {4, 5};
        for (operation = 0; operation < 2u; ++operation) {
            uint32_t host[32] = {0}; hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(invalid[operation], sizes[operation], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
            CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
        }
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0xc5, 0xe5, 0x69, 0xc1};
        static const uint16_t first[] = {0,1,2,3,4,5,6,7, 8,9,10,11,12,13,14,15};
        static const uint16_t second[] = {100,101,102,103,104,105,106,107,
                                          108,109,110,111,112,113,114,115};
        static const uint16_t expected[] = {4,104,5,105,6,106,7,107,
                                             12,112,13,113,14,114,15,115};
        uint32_t host[128] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_native_x86_64_cpu cpu = {0}; long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[6], first, 16); memcpy(&cpu.vector_upper[6], first + 8, 16);
        memcpy(&cpu.vectors[2], second, 16); memcpy(&cpu.vector_upper[2], second + 8, 16);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[0], expected + 8, 16) == 0);
        CHECK(memcmp(&cpu.vectors[6], first, 16) == 0 &&
              memcmp(&cpu.vector_upper[6], first + 8, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_horizontal_add_sub_contract(void) {
    static const uint8_t opcodes[] = {0x01, 0x02, 0x03, 0x05, 0x06, 0x07};
    unsigned wide, memory, index;
    for (wide = 0; wide < 2; ++wide) for (memory = 0; memory < 2; ++memory)
        for (index = 0; index < sizeof opcodes; ++index) {
            uint8_t guest[] = {0xc4, 0xe2, (uint8_t)(0x69u | wide * 4u), opcodes[index],
                               (uint8_t)(memory != 0u ? 0x03u : 0xc1u)};
            uint32_t host[256] = {0}, exact[256], short_output[256], short_expected[256];
            hl_x86_a64_provenance provenance[2] = {0}, exact_provenance[2] = {0};
            hl_x86_a64_provenance short_provenance[2], short_provenance_expected[2];
            hl_x86_a64_result result, exact_result, untouched, untouched_expected;
            hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
            request.max_instructions = 1u;
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
            memset(exact, 0xa5, sizeof exact);
            request.host_words = exact; request.provenance = exact_provenance;
            request.host_capacity = result.word_count;
            CHECK(hl_x86_a64_emit(&request, &exact_result) == HL_X86_A64_OK);
            CHECK(exact_result.word_count == result.word_count);
            CHECK(memcmp(exact, host, result.word_count * sizeof host[0]) == 0);
            CHECK(memcmp(exact_provenance, provenance, sizeof provenance) == 0);
            memset(short_output, 0xa5, sizeof short_output);
            memcpy(short_expected, short_output, sizeof short_output);
            memset(short_provenance, 0xa5, sizeof short_provenance);
            memcpy(short_provenance_expected, short_provenance, sizeof short_provenance);
            memset(&untouched, 0xa5, sizeof untouched);
            untouched_expected = untouched;
            request.host_words = short_output; request.provenance = short_provenance;
            request.host_capacity = result.word_count - 1u;
            CHECK(hl_x86_a64_emit(&request, &untouched) == HL_X86_A64_CAPACITY);
            CHECK(memcmp(short_output, short_expected, sizeof short_output) == 0);
            CHECK(memcmp(short_provenance, short_provenance_expected, sizeof short_provenance) == 0);
            CHECK(memcmp(&untouched, &untouched_expected, sizeof untouched) == 0);
        }
    {
        static const uint8_t aliases[][5] = {
            {0xc4, 0xe2, 0x79, 0x01, 0xc1}, /* dst aliases vvvv */
            {0xc4, 0xe2, 0x71, 0x07, 0xc0}, /* dst aliases r/m */
        };
        for (index = 0; index < sizeof aliases / sizeof aliases[0]; ++index) {
            uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0}; hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(aliases[index], sizeof aliases[index], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        }
    }
    {
        static const uint8_t invalid[][5] = {
            {0xc4, 0xe1, 0x69, 0x01, 0xc1}, /* wrong map */
            {0xc4, 0xe2, 0x68, 0x01, 0xc1}, /* wrong mandatory prefix */
            {0xc4, 0xe2, 0x69, 0x04, 0xc1}, /* opcode belongs to another family */
        };
        for (index = 0; index < sizeof invalid / sizeof invalid[0]; ++index) {
            uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0}; hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(invalid[index], sizeof invalid[index], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
            CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
        }
    }
#if defined(__aarch64__)
    for (index = 0; index < sizeof opcodes; ++index) {
        uint8_t guest[] = {0xc4, 0xe2, 0x6d, opcodes[index], 0xc1};
        uint8_t left[32], right[32], expected[32];
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE); unsigned lane, pair;
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED);
        for (lane = 0; lane < 32; lane += 16) {
            if (opcodes[index] == 0x02u || opcodes[index] == 0x06u) {
                static const int32_t a[4] = {INT32_MAX, 1, INT32_MIN, -1};
                static const int32_t b[4] = {7, 11, -13, 17};
                int32_t output[4]; memcpy(left + lane, a, 16); memcpy(right + lane, b, 16);
                for (pair = 0; pair < 2; ++pair) {
                    uint32_t av = (uint32_t)a[2 * pair], aw = (uint32_t)a[2 * pair + 1];
                    uint32_t bv = (uint32_t)b[2 * pair], bw = (uint32_t)b[2 * pair + 1];
                    output[pair] = (int32_t)(opcodes[index] == 0x06u ? av - aw : av + aw);
                    output[pair + 2] = (int32_t)(opcodes[index] == 0x06u ? bv - bw : bv + bw);
                }
                memcpy(expected + lane, output, 16);
            } else {
                static const int16_t a[8] = {INT16_MAX, 1, INT16_MIN, -1, 100, 30, -100, 30};
                static const int16_t b[8] = {20000, 20000, -20000, -20000, 9, 4, -9, -4};
                int16_t output[8]; memcpy(left + lane, a, 16); memcpy(right + lane, b, 16);
                for (pair = 0; pair < 4; ++pair) {
                    int av = opcodes[index] >= 0x05u ? a[2 * pair] - a[2 * pair + 1] :
                                                       a[2 * pair] + a[2 * pair + 1];
                    int bv = opcodes[index] >= 0x05u ? b[2 * pair] - b[2 * pair + 1] :
                                                       b[2 * pair] + b[2 * pair + 1];
                    if (opcodes[index] == 0x03u || opcodes[index] == 0x07u) {
                        av = av < INT16_MIN ? INT16_MIN : av > INT16_MAX ? INT16_MAX : av;
                        bv = bv < INT16_MIN ? INT16_MIN : bv > INT16_MAX ? INT16_MAX : bv;
                    }
                    output[pair] = (int16_t)av; output[pair + 4] = (int16_t)bv;
                }
                memcpy(expected + lane, output, 16);
            }
        }
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[4], left, 16); memcpy(&cpu.vector_upper[4], left + 16, 16);
        memcpy(&cpu.vectors[2], right, 16); memcpy(&cpu.vector_upper[2], right + 16, 16);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[0], expected + 16, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_packed_add_subtract_contract(void) {
    static const uint8_t opcodes[] = {0xfc, 0xfd, 0xfe, 0xd4, 0xf8, 0xf9, 0xfa, 0xfb};
    unsigned operation;
    unsigned wide;

    for (operation = 0; operation < sizeof opcodes / sizeof opcodes[0]; ++operation) {
        for (wide = 0; wide < 2u; ++wide) {
            uint8_t guest[] = {0xc5, (uint8_t)(wide != 0u ? 0xe5u : 0xe1u),
                               opcodes[operation], 0xc1};
            uint32_t host[64] = {0};
            hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
            CHECK(provenance[0].guest_size == sizeof guest);
        }
    }
    {
        static const uint8_t forms[][6] = {
            {0xc4, 0x61, 0x65, 0xfe, 0xc1, 0}, /* WIG, extended destination, YMM */
            {0xc5, 0xe5, 0xfe, 0x03, 0, 0},    /* YMM memory source */
        };
        static const size_t sizes[] = {5, 4};
        for (operation = 0; operation < 2u; ++operation) {
            uint32_t host[128] = {0}; hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(forms[operation], sizes[operation], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
        }
    }
    {
        static const uint8_t invalid[][6] = {
            {0xc5, 0xe4, 0xfe, 0xc1, 0, 0},       /* wrong pp */
            {0xc4, 0xe2, 0x65, 0xfe, 0xc1, 0},    /* wrong map */
        };
        static const size_t sizes[] = {4, 5};
        for (operation = 0; operation < 2u; ++operation) {
            uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(invalid[operation], sizes[operation], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
            CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
        }
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0xc5, 0xe5, 0xfe, 0xc1}; /* vpaddd ymm0,ymm3,ymm1 */
        static const uint32_t left[] = {0, 1, UINT32_MAX, 0x7fffffff,
                                        5, 0x80000000, 100, UINT32_MAX};
        static const uint32_t right[] = {1, UINT32_MAX, 2, 1, 7, 0x80000000, UINT32_MAX, 1};
        static const uint32_t expected[] = {1, 0, 1, 0x80000000, 12, 0, 99, 0};
        uint32_t host[128] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_native_x86_64_cpu cpu = {0};
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[6], left, 16); memcpy(&cpu.vector_upper[6], left + 4, 16);
        memcpy(&cpu.vectors[2], right, 16); memcpy(&cpu.vector_upper[2], right + 4, 16);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[0], expected + 4, 16) == 0);
        CHECK(memcmp(&cpu.vectors[6], left, 16) == 0 &&
              memcmp(&cpu.vector_upper[6], left + 4, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_packed_average_sad_contract(void) {
    static const uint8_t opcodes[] = {0xe0, 0xe3, 0xf6};
    unsigned operation, wide, memory;
    for (operation = 0; operation < sizeof opcodes; ++operation) {
        for (wide = 0; wide < 2u; ++wide) for (memory = 0; memory < 2u; ++memory) {
            uint8_t guest[] = {0xc5, (uint8_t)(wide != 0u ? 0xe5u : 0xe1u),
                               opcodes[operation], (uint8_t)(memory != 0u ? 0x03u : 0xc1u)};
            uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
            CHECK(provenance[0].guest_size == sizeof guest && provenance[0].word_end != 0u);
            {
                size_t capacity;
                uint32_t required = 0u;
                for (capacity = 1u; capacity <= 256u; ++capacity) {
                    request.host_capacity = capacity;
                    if (hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK &&
                        result.instruction_count == 1u) { required = (uint32_t)capacity; break; }
                }
                CHECK(required != 0u);
                request.host_capacity = required;
                CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK &&
                      result.instruction_count == 1u);
                request.host_capacity = required - 1u;
                memset(host, 0xa5, sizeof host); memset(provenance, 0xa5, sizeof provenance);
                {
                    hl_x86_a64_status status = hl_x86_a64_emit(&request, &result);
                    CHECK(status == HL_X86_A64_CAPACITY || result.instruction_count == 0u);
                    if (status == HL_X86_A64_CAPACITY) CHECK(host[0] == UINT32_C(0xa5a5a5a5));
                }
            }
        }
    }
    {
        static const uint8_t alternatives[][5] = {
            {0xc4, 0xe1, 0x69, 0xe3, 0xc1}, /* C4 W=0 VPAVGW xmm0,xmm2,xmm1 */
            {0xc4, 0xe1, 0xed, 0xf6, 0x03}, /* C4 W=1 VPSADBW ymm0,ymm2,[rbx] */
        };
        for (operation = 0; operation < sizeof alternatives / sizeof alternatives[0]; ++operation) {
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(alternatives[operation],
                                                      sizeof alternatives[operation], host, provenance);
            size_t capacity;
            uint32_t required = 0u;
            for (capacity = 1u; capacity <= 256u; ++capacity) {
                request.host_capacity = capacity;
                if (hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK &&
                    result.instruction_count == 1u) { required = (uint32_t)capacity; break; }
            }
            CHECK(required != 0u && provenance[0].guest_size == sizeof alternatives[operation]);
            request.host_capacity = required - 1u;
            memset(host, 0xa5, sizeof host);
            memset(provenance, 0xa5, sizeof provenance);
            {
                hl_x86_a64_status status = hl_x86_a64_emit(&request, &result);
                CHECK(status == HL_X86_A64_CAPACITY || result.instruction_count == 0u);
                if (status == HL_X86_A64_CAPACITY) {
                    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
                    CHECK(provenance[0].guest_pc == UINT64_C(0xa5a5a5a5a5a5a5a5));
                }
            }
        }
    }
    {
        static const uint8_t invalid[][5] = {
            {0xc5, 0xe0, 0xe0, 0xc1, 0}, /* wrong pp */
            {0xc4, 0xe2, 0x61, 0xf6, 0xc1}, /* wrong map */
        };
        static const size_t sizes[] = {4, 5};
        for (operation = 0; operation < 2u; ++operation) {
            uint32_t host[32] = {0}; hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(invalid[operation], sizes[operation], host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
            CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
        }
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {
            0xc5, 0xe5, 0xe0, 0xc1, /* vpavgb ymm0,ymm3,ymm1 */
            0xc5, 0xed, 0xf6, 0xc9, /* vpsadbw ymm1,ymm2,ymm1 (destination/source alias) */
        };
        uint8_t a[32], b[32], average[32], sad[32] = {0};
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[3] = {0};
        hl_x86_a64_result emitted; hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_native_x86_64_cpu cpu = {0}; long page = sysconf(_SC_PAGESIZE); unsigned i, q;
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED);
        for (i = 0; i < 32u; ++i) {
            a[i] = (uint8_t)(i * 9u); b[i] = (uint8_t)(255u - i * 5u);
            average[i] = (uint8_t)(((unsigned)a[i] + (unsigned)b[i] + 1u) >> 1);
        }
        for (q = 0; q < 32u; q += 8u) {
            uint16_t sum = 0;
            for (i = 0; i < 8u; ++i)
                sum = (uint16_t)(sum + (a[q + i] > b[q + i] ?
                       a[q + i] - b[q + i] : b[q + i] - a[q + i]));
            memcpy(sad + q, &sum, sizeof sum);
        }
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * 4u);
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[6], a, 16); memcpy(&cpu.vector_upper[6], a + 16, 16);
        memcpy(&cpu.vectors[2], b, 16); memcpy(&cpu.vector_upper[2], b + 16, 16);
        memcpy(&cpu.vectors[4], a, 16); memcpy(&cpu.vector_upper[4], a + 16, 16);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[0], average, 16) == 0 &&
              memcmp(&cpu.vector_upper[0], average + 16, 16) == 0);
        CHECK(memcmp(&cpu.vectors[2], sad, 16) == 0 &&
              memcmp(&cpu.vector_upper[2], sad + 16, 16) == 0);
        CHECK(memcmp(&cpu.vectors[6], a, 16) == 0 && memcmp(&cpu.vectors[4], a, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int vex_packed_saturating_contract(void) {
    static const uint8_t opcodes[] = {0x63, 0x67, 0x6b, 0x2b};
    unsigned operation;
    unsigned wide;

    for (operation = 0; operation < sizeof opcodes / sizeof opcodes[0]; ++operation) {
        for (wide = 0; wide < 2u; ++wide) {
            uint8_t guest[] = {0xc4, (uint8_t)(operation == 3u ? 0xe2u : 0xe1u),
                               (uint8_t)(wide != 0u ? 0x6du : 0x69u),
                               opcodes[operation], (uint8_t)(wide != 0u ? 0x03u : 0xc3u)};
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
            CHECK(result.instruction_count == 1 && result.exit == HL_X86_A64_FALLTHROUGH);
            CHECK(provenance[0].guest_size == sizeof guest);
            {
                uint32_t required = result.word_count;
                uint32_t exact_host[256] = {0};
                uint32_t short_host[256];
                hl_x86_a64_provenance exact_provenance[2] = {0};
                hl_x86_a64_provenance short_provenance[2];
                hl_x86_a64_result exact_result;
                hl_x86_a64_result short_result;
                hl_x86_a64_request exact = request_for(guest, sizeof guest, exact_host,
                                                        exact_provenance);
                hl_x86_a64_request short_request = request_for(guest, sizeof guest, short_host,
                                                                short_provenance);
                unsigned word;

                exact.host_capacity = required;
                CHECK(hl_x86_a64_emit(&exact, &exact_result) == HL_X86_A64_OK);
                CHECK(exact_result.word_count == required);
                memset(short_host, 0xa5, sizeof short_host);
                memset(short_provenance, 0xa5, sizeof short_provenance);
                short_request.host_capacity = required - 1u;
                CHECK(hl_x86_a64_emit(&short_request, &short_result) == HL_X86_A64_CAPACITY);
                for (word = 0; word < sizeof short_host / sizeof short_host[0]; ++word)
                    CHECK(short_host[word] == UINT32_C(0xa5a5a5a5));
            }
        }
    }
    {
        static const uint8_t c5[] = {0xc5, 0xe9, 0x63, 0xc3};
        uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(c5, sizeof c5, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof c5);
    }
    {
        static const uint8_t invalid[][5] = {
            {0xc4, 0xe1, 0x68, 0x63, 0xc3}, /* wrong pp */
            {0xc4, 0xe2, 0x69, 0x63, 0xc3}, /* wrong map */
            {0xc4, 0xe1, 0x69, 0x2b, 0xc3}, /* PACKUSDW needs map two */
        };
        for (operation = 0; operation < sizeof invalid / sizeof invalid[0]; ++operation) {
            uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0};
            hl_x86_a64_result result;
            hl_x86_a64_request request = request_for(invalid[operation], sizeof invalid[operation],
                                                      host, provenance);
            CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
            CHECK(result.instruction_count == 0 && result.exit_pc == request.guest_pc);
        }
    }
#if defined(__aarch64__)
    {
        static const uint8_t guest[] = {0xc4, 0xe1, 0x6d, 0x63, 0xd3};
        static const int16_t first[16] = {
            INT16_MIN, -129, -128, -1, 0, 1, 127, 128,
            255, 256, INT16_MAX, -255, -2, 2, 126, 129,
        };
        static const int16_t second[16] = {
            327, -327, 42, -42, 127, 128, -128, -129,
            3, 4, 5, 6, 300, -300, INT16_MIN, INT16_MAX,
        };
        static const int8_t expected[32] = {
            -128, -128, -128, -1, 0, 1, 127, 127,
            127, -128, 42, -42, 127, 127, -128, -128,
            127, 127, 127, -128, -2, 2, 126, 127,
            3, 4, 5, 6, 127, -128, -128, 127,
        };
        uint32_t host[128] = {0}; hl_x86_a64_provenance provenance[2] = {0};
        hl_x86_a64_result result; hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(code != MAP_FAILED && hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(&cpu.vectors[4], first, 16); memcpy(&cpu.vector_upper[4], first + 8, 16);
        memcpy(&cpu.vectors[6], second, 16); memcpy(&cpu.vector_upper[6], second + 8, 16);
        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
        hl_native_x86_64_enter(&cpu, code);
        CHECK(memcmp(&cpu.vectors[4], expected, 16) == 0);
        CHECK(memcmp(&cpu.vector_upper[4], expected + 16, 16) == 0);
        CHECK(memcmp(&cpu.vectors[6], second, 16) == 0 &&
              memcmp(&cpu.vector_upper[6], second + 8, 16) == 0);
        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static int alu_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const uint8_t opcodes[] = {0x01, 0x09, 0x11, 0x19, 0x21, 0x29, 0x31, 0x39};
    static const uint64_t values[][2] = {
        {0, 0}, {1, 1}, {UINT64_MAX, 1}, {UINT64_C(0x7fffffffffffffff), 1},
        {UINT64_C(0x80000000), UINT64_C(0xffffffff)}, {UINT64_C(0xf), 1},
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t operation;
    size_t sample;

    CHECK(code != MAP_FAILED);
    for (operation = 0; operation < sizeof opcodes / sizeof opcodes[0]; ++operation) {
        for (sample = 0; sample < sizeof values / sizeof values[0]; ++sample) {
            unsigned wide;
            for (wide = 0; wide < 2u; ++wide) {
                uint8_t guest[] = {(uint8_t)(wide ? 0x48 : opcodes[operation]),
                                   (uint8_t)(wide ? opcodes[operation] : 0xc8),
                                   (uint8_t)(wide ? 0xc8 : 0)};
                size_t guest_size = wide ? 3u : 2u;
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_request request = request_for(guest, guest_size, host, provenance);
                hl_x86_a64_result emitted;
                hl_native_x86_64_cpu cpu = {0};
                uint64_t expected_result;
                uint64_t expected_flags;
                unsigned kind = (opcodes[operation] >> 3) & 7u;

                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[0] = values[sample][0];
                cpu.registers[1] = values[sample][1];
                cpu.flags = UINT64_C(0x202) | (sample & 1u ? HL_X86_RFLAGS_CF : 0);
                alu_reference(kind, wide ? 64u : 32u, cpu.registers[0], cpu.registers[1],
                              cpu.flags, &expected_result, &expected_flags);
                hl_x86_test_enter(&cpu, code);
                if (kind != 7u) CHECK(cpu.registers[0] == expected_result);
                CHECK(cpu.flags == expected_flags);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int immediate_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const unsigned bits[] = {8u, 16u, 32u, 64u};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned kind;
    unsigned wide;
    unsigned form;
    size_t width;

    CHECK(code != MAP_FAILED);
    for (kind = 0; kind < 8u; ++kind) {
        for (wide = 0; wide < 2u; ++wide) {
            for (form = 0; form < 3u; ++form) {
                uint8_t guest[8] = {0};
                size_t size = 0;
                uint64_t right;
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result emitted;
                hl_native_x86_64_cpu cpu = {0};
                uint64_t expected_result;
                uint64_t expected_flags;
                hl_x86_a64_request request;

                if (wide) guest[size++] = 0x48;
                if (form == 0u) {
                    guest[size++] = (uint8_t)(0x05u | kind << 3);
                } else {
                    guest[size++] = form == 1u ? 0x81 : 0x83;
                    guest[size++] = (uint8_t)(0xc0u | kind << 3);
                }
                if (form == 2u) {
                    guest[size++] = 0x80;
                    right = UINT64_MAX - 127u;
                } else {
                    guest[size++] = 0;
                    guest[size++] = 0;
                    guest[size++] = 0;
                    guest[size++] = 0x80;
                    right = wide ? UINT64_C(0xffffffff80000000) : UINT64_C(0x80000000);
                }
                request = request_for(guest, size, host, provenance);
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                CHECK(provenance[0].guest_size == size && provenance[0].word_start == 0);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[0] = wide ? UINT64_C(0x7ffffffffffffff0) : UINT64_C(0xfffffff0);
                cpu.flags = UINT64_C(0x203);
                alu_reference(kind, wide ? 64u : 32u, cpu.registers[0], right, cpu.flags,
                              &expected_result, &expected_flags);
                hl_x86_test_enter(&cpu, code);
                if (kind != 7u) CHECK(cpu.registers[0] == expected_result);
                CHECK(cpu.flags == expected_flags);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    for (width = 0; width < sizeof bits / sizeof bits[0]; ++width) {
        uint8_t guest[8] = {0};
        size_t size = 0;
        size_t immediate_size = bits[width] == 8u ? 1u : bits[width] == 16u ? 2u : 4u;
        uint64_t immediate = bits[width] == 8u ? UINT64_C(0x81) :
                             bits[width] == 16u ? UINT64_C(0x8001) : UINT64_C(0x80000001);
        uint64_t right = bits[width] == 64u ? UINT64_C(0xffffffff80000001) : immediate;
        uint64_t initial = UINT64_C(0x1122334455667ff0);
        uint64_t ignored;
        uint64_t expected_flags;
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};
        hl_x86_a64_request request;
        size_t byte;
        if (bits[width] == 16u) guest[size++] = 0x66;
        if (bits[width] == 64u) guest[size++] = 0x48;
        guest[size++] = bits[width] == 8u ? 0xa8 : 0xa9;
        for (byte = 0; byte < immediate_size; ++byte)
            guest[size++] = (uint8_t)(immediate >> (byte * 8u));
        request = request_for(guest, size, host, provenance);
        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = initial;
        cpu.flags = UINT64_C(0xad7);
        alu_reference(4u, bits[width], initial, right, cpu.flags, &ignored, &expected_flags);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[0] == initial);
        CHECK(cpu.flags == expected_flags);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int accumulator_immediate_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const unsigned bits[] = {8u, 16u, 32u, 64u};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned kind;
    size_t width;

    CHECK(code != MAP_FAILED);
    for (kind = 0; kind < 8u; ++kind) {
        for (width = 0; width < sizeof bits / sizeof bits[0]; ++width) {
            uint8_t guest[8] = {0};
            size_t size = 0;
            size_t immediate_size = bits[width] == 8u ? 1u : bits[width] == 16u ? 2u : 4u;
            uint64_t immediate = bits[width] == 8u ? UINT64_C(0x81) :
                                 bits[width] == 16u ? UINT64_C(0x8001) : UINT64_C(0x80000001);
            uint64_t right = bits[width] == 64u ? UINT64_C(0xffffffff80000001) : immediate;
            uint64_t initial = UINT64_C(0x1122334455667ff0);
            uint64_t result;
            uint64_t expected_flags;
            uint64_t expected_register;
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[8] = {0};
            hl_x86_a64_result emitted;
            hl_native_x86_64_cpu cpu = {0};
            hl_x86_a64_request request;
            size_t byte;

            if (bits[width] == 16u) guest[size++] = 0x66;
            if (bits[width] == 64u) guest[size++] = 0x48;
            guest[size++] = (uint8_t)((kind << 3) | (bits[width] == 8u ? 4u : 5u));
            for (byte = 0; byte < immediate_size; ++byte)
                guest[size++] = (uint8_t)(immediate >> (byte * 8u));
            request = request_for(guest, size, host, provenance);
            request.host_capacity = sizeof host / sizeof host[0];
            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            CHECK(provenance[0].guest_size == size);
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            cpu.registers[0] = initial;
            cpu.flags = UINT64_C(0x203);
            alu_reference(kind, bits[width], initial, right, cpu.flags, &result, &expected_flags);
            if (bits[width] < 32u)
                expected_register = (initial & ~(bits[width] == 8u ? UINT64_C(0xff) : UINT64_C(0xffff))) |
                                    result;
            else
                expected_register = result;
            hl_x86_test_enter(&cpu, code);
            if (kind != 7u) CHECK(cpu.registers[0] == expected_register);
            else CHECK(cpu.registers[0] == initial);
            CHECK(cpu.flags == expected_flags);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int long_accumulator_immediate_chain(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint8_t guest[3 + 40 * 6] = {0x48, 0x89, 0xf8};
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[HL_X86_A64_MAX_INSTRUCTIONS] = {0};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page * 2u, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint64_t expected = 29862;
    size_t cursor = 3;
    unsigned index;

    CHECK(code != MAP_FAILED);
    for (index = 1; index <= 29; ++index) {
        uint32_t immediate = index * 3u;
        guest[cursor++] = 0x48;
        guest[cursor++] = 0x05;
        memcpy(guest + cursor, &immediate, sizeof immediate);
        cursor += sizeof immediate;
        expected += immediate;
    }
    hl_x86_a64_request request = request_for(guest, cursor, host, provenance);
    hl_x86_a64_result emitted;
    hl_native_x86_64_cpu cpu = {0};
    request.host_capacity = sizeof host / sizeof host[0];
    request.provenance_capacity = HL_X86_A64_MAX_INSTRUCTIONS;
    CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
    CHECK(emitted.instruction_count == 30);
    memcpy(code, host, emitted.word_count * sizeof(uint32_t));
    ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
    __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
    CHECK(mprotect(code, (size_t)page * 2u, PROT_READ | PROT_EXEC) == 0);
    cpu.registers[7] = 29862;
    hl_x86_test_enter(&cpu, code);
    CHECK(cpu.registers[0] == expected);
    CHECK(munmap(code, (size_t)page * 2u) == 0);
    return 0;
#endif
}

static int immediate_contract(void) {
    const uint8_t high[] = {0x49, 0x83, 0xc0, 0x80};
    const uint8_t byte_low[] = {0x80, 0xf8, 0x80};
    const uint8_t byte_high[] = {0x80, 0xfc, 0x80};
    const uint8_t byte_rex[] = {0x40, 0x80, 0xfc, 0x80};
    const uint8_t byte_write[] = {0x80, 0xc0, 1};
    const uint8_t narrow[] = {0x66, 0x83, 0xc0, 1};
    const uint8_t memory[] = {0x48, 0x83, 0x00, 1};
    const uint8_t truncated[] = {0x48, 0x81, 0xc0, 1, 2};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(high, sizeof high, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(provenance[0].guest_size == sizeof high && provenance[0].word_start == 0);
    CHECK(host[0] == UINT32_C(0xaa0803f0));
    request = request_for(byte_low, sizeof byte_low, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof byte_low);
    request = request_for(byte_high, sizeof byte_high, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof byte_high);
    request = request_for(byte_rex, sizeof byte_rex, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof byte_rex);
    request = request_for(byte_write, sizeof byte_write, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(high, sizeof high, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request.host_capacity = result.word_count - 1u;
    memset(host, 0xa5, sizeof host);
    memset(provenance, 0xa5, sizeof provenance);
    memset(&result, 0xa5, sizeof result);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
    CHECK(provenance[0].guest_pc == UINT64_C(0xa5a5a5a5a5a5a5a5));
    request = request_for(narrow, sizeof narrow, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(memory, sizeof memory, host, provenance);
    request.host_capacity = sizeof host / sizeof host[0];
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(truncated, sizeof truncated, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
    return 0;
}

static int immediate_memory_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const unsigned bits[] = {8u, 16u, 32u, 64u};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned kind;
    size_t width;
    unsigned short_form;

    CHECK(code != MAP_FAILED);
    for (kind = 0; kind < 8u; ++kind) {
        for (width = 0; width < sizeof bits / sizeof bits[0]; ++width) {
            for (short_form = 0; short_form < (bits[width] == 8u ? 1u : 2u); ++short_form) {
                uint8_t guest[12] = {0};
                size_t size = 0;
                size_t immediate_size;
                uint64_t immediate;
                uint64_t right;
                uint64_t initial = UINT64_C(0x8877665544332211);
                uint64_t backing = initial;
                uint64_t expected_result;
                uint64_t expected_flags;
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result emitted;
                hl_native_x86_64_cpu cpu = {0};
                hl_x86_a64_request request;
                size_t byte;

                if (bits[width] == 16u) guest[size++] = 0x66;
                if (bits[width] == 64u) guest[size++] = 0x48;
                guest[size++] = bits[width] == 8u ? 0x80u : short_form != 0u ? 0x83u : 0x81u;
                guest[size++] = (uint8_t)(kind << 3); /* [rax] */
                immediate_size = bits[width] == 8u || short_form != 0u ? 1u :
                                 bits[width] == 16u ? 2u : 4u;
                immediate = immediate_size == 1u ? UINT64_C(0x81) :
                            immediate_size == 2u ? UINT64_C(0x8001) : UINT64_C(0x80000001);
                for (byte = 0; byte < immediate_size; ++byte)
                    guest[size++] = (uint8_t)(immediate >> (byte * 8u));
                right = short_form != 0u ? UINT64_C(0xffffffffffffff81) :
                        bits[width] == 64u ? UINT64_C(0xffffffff80000001) : immediate;
                request = request_for(guest, size, host, provenance);
                request.host_capacity = sizeof host / sizeof host[0];
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                CHECK(provenance[0].guest_size == size);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[0] = UINT64_C(0x2000);
                cpu.flags = UINT64_C(0x203);
                cpu.memory_first = UINT64_C(0x2000);
                cpu.memory_last = UINT64_C(0x2008);
                cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
                cpu.memory_permissions = kind == 7u ? 1u : 3u;
                cpu.dirty_first = UINT64_MAX;
                alu_reference(kind, bits[width], initial, right, cpu.flags,
                              &expected_result, &expected_flags);
                hl_x86_test_enter(&cpu, code);
                if (kind != 7u) {
                    uint64_t mask = bits[width] == 64u ? UINT64_MAX :
                                    (UINT64_C(1) << bits[width]) - 1u;
                    CHECK(backing == ((initial & ~mask) | expected_result));
                    CHECK(cpu.memory_written == 1u);
                    CHECK(cpu.dirty_view_first == UINT64_C(0x2000));
                    CHECK(cpu.dirty_view_last == UINT64_C(0x2008));
                    CHECK(cpu.dirty_first == UINT64_C(0x2000));
                    CHECK(cpu.dirty_last == UINT64_C(0x2000) + bits[width] / 8u);
                } else {
                    CHECK(backing == initial && cpu.memory_written == 0u);
                    CHECK(cpu.dirty_first == UINT64_MAX && cpu.dirty_count == 0u);
                }
                CHECK(cpu.flags == expected_flags && cpu.reason == 0);
                if (kind != 7u) {
                    backing = initial;
                    memset(&cpu, 0, sizeof cpu);
                    cpu.registers[0] = UINT64_C(0x2000);
                    cpu.flags = UINT64_C(0x203);
                    cpu.memory_first = UINT64_C(0x2000);
                    cpu.memory_last = UINT64_C(0x2008);
                    cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
                    cpu.memory_permissions = 1u;
                    cpu.dirty_first = UINT64_MAX;
                    hl_x86_test_enter(&cpu, code);
                    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK &&
                          cpu.fault_access == (HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE));
                    CHECK(cpu.flags == UINT64_C(0x203) && backing == initial);
                    CHECK(cpu.memory_written == 0u && cpu.dirty_first == UINT64_MAX);
                }
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int test_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const unsigned bits[] = {16u, 32u, 64u};
    static const uint64_t values[][2] = {
        {0, UINT64_MAX}, {UINT64_MAX, UINT64_C(0x80000000)},
        {UINT64_C(0x8000000000000000), UINT64_C(0xfffffffffffffff0)},
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned form;
    size_t width;
    unsigned sample;

    CHECK(code != MAP_FAILED);
    for (form = 0; form < 3u; ++form) {
        for (width = 0; width < sizeof bits / sizeof bits[0]; ++width) {
            for (sample = 0; sample < sizeof values / sizeof values[0]; ++sample) {
                uint8_t guest[8] = {0};
                size_t size = 0;
                uint64_t right = values[sample][1];
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result emitted;
                hl_native_x86_64_cpu cpu = {0};
                uint64_t ignored;
                uint64_t expected_flags;
                hl_x86_a64_request request;

                if (bits[width] == 16u) guest[size++] = 0x66;
                if (bits[width] == 64u) guest[size++] = 0x48;
                if (form == 0u) {
                    guest[size++] = 0x85;
                    guest[size++] = 0xc8;
                } else {
                    guest[size++] = form == 1u ? 0xa9 : 0xf7;
                    if (form == 2u) guest[size++] = 0xc0;
                    guest[size++] = 0;
                    if (bits[width] == 16u) {
                        guest[size++] = 0x80;
                    } else {
                        guest[size++] = 0;
                        guest[size++] = 0;
                        guest[size++] = 0x80;
                    }
                    right = bits[width] == 16u ? UINT64_C(0x8000) :
                            bits[width] == 64u ? UINT64_C(0xffffffff80000000) : UINT64_C(0x80000000);
                }
                request = request_for(guest, size, host, provenance);
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[0] = values[sample][0];
                cpu.registers[1] = values[sample][1];
                cpu.flags = UINT64_C(0xad7);
                alu_reference(4u, bits[width], cpu.registers[0], right, cpu.flags,
                              &ignored, &expected_flags);
                hl_x86_test_enter(&cpu, code);
                CHECK(cpu.registers[0] == values[sample][0]);
                CHECK(cpu.flags == expected_flags);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    {
        static const struct {
            uint8_t guest[5];
            uint8_t size;
            uint8_t width;
            uint8_t source;
            uint8_t high;
        } cases[] = {
            {{0x84, 0x5c, 0x24, 0x18}, 4, 8, 3, 0}, /* test [rsp+24],bl */
            {{0x84, 0x64, 0x24, 0x18}, 4, 8, 0, 1}, /* test [rsp+24],ah */
            {{0x84, 0x6c, 0x24, 0x18}, 4, 8, 1, 1}, /* test [rsp+24],ch */
            {{0x84, 0x74, 0x24, 0x18}, 4, 8, 2, 1}, /* test [rsp+24],dh */
            {{0x84, 0x7c, 0x24, 0x18}, 4, 8, 3, 1}, /* test [rsp+24],bh */
            {{0x40, 0x84, 0x6c, 0x24, 0x18}, 5, 8, 5, 0}, /* test [rsp+24],bpl */
            {{0x44, 0x84, 0x5c, 0x24, 0x18}, 5, 8, 11, 0}, /* test [rsp+24],r11b */
            {{0x66, 0x85, 0x5c, 0x24, 0x18}, 5, 16, 3, 0}, /* test [rsp+24],bx */
            {{0x85, 0x5c, 0x24, 0x18}, 4, 32, 3, 0}, /* test [rsp+24],ebx */
            {{0x48, 0x85, 0x5c, 0x24, 0x18}, 5, 64, 3, 0}, /* test [rsp+24],rbx */
            {{0x44, 0x85, 0x5c, 0x24, 0x18}, 5, 32, 11, 0}, /* test [rsp+24],r11d */
        };
        size_t test_case;
        unsigned nonzero;

        for (test_case = 0; test_case < sizeof cases / sizeof cases[0]; ++test_case) {
            for (nonzero = 0; nonzero < 2u; ++nonzero) {
                uint8_t backing[32] = {0};
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result emitted;
                hl_native_x86_64_cpu cpu = {0};
                hl_x86_a64_request request =
                    request_for(cases[test_case].guest, cases[test_case].size, host, provenance);
                uint64_t source = nonzero != 0u ? 1u : 2u;
                uint64_t ignored;
                uint64_t expected_flags;

                backing[24] = 1;
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[cases[test_case].source] =
                    cases[test_case].high != 0u ? source << 8 : source;
                cpu.registers[4] = UINT64_C(0x2000);
                cpu.flags = UINT64_C(0xad7);
                cpu.memory_first = UINT64_C(0x2000);
                cpu.memory_last = UINT64_C(0x2020);
                cpu.memory_delta = (uint64_t)(uintptr_t)backing - UINT64_C(0x2000);
                cpu.memory_permissions = HL_NATIVE_ACCESS_READ;
                alu_reference(4u, cases[test_case].width, 1u, source, cpu.flags,
                              &ignored, &expected_flags);
                hl_x86_test_enter(&cpu, code);
                CHECK(cpu.flags == expected_flags && cpu.reason == 0u);
                CHECK(cpu.registers[cases[test_case].source] ==
                      (cases[test_case].high != 0u ? source << 8 : source));
                CHECK(backing[24] == 1 && cpu.memory_written == 0u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int group_test_memory_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const unsigned bits[] = {8u, 16u, 32u, 64u};
    static const uint64_t values[] = {
        UINT64_C(0), UINT64_C(0x8877665544332211), UINT64_MAX,
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned extension;
    size_t width;
    size_t sample;

    CHECK(code != MAP_FAILED);
    for (extension = 0; extension < 2u; ++extension) {
        for (width = 0; width < sizeof bits / sizeof bits[0]; ++width) {
            for (sample = 0; sample < sizeof values / sizeof values[0]; ++sample) {
            uint8_t guest[12] = {0};
            size_t size = 0;
            size_t immediate_size = bits[width] == 8u ? 1u : bits[width] == 16u ? 2u : 4u;
            uint64_t immediate = bits[width] == 8u ? UINT64_C(0x81) :
                                 bits[width] == 16u ? UINT64_C(0x8001) : UINT64_C(0x80000001);
            uint64_t right = bits[width] == 64u ? UINT64_C(0xffffffff80000001) : immediate;
            uint64_t backing[2] = {UINT64_C(0xfeedfacecafebeef), values[sample]};
            uint64_t ignored;
            uint64_t expected_flags;
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[8] = {0};
            hl_x86_a64_result emitted;
            hl_native_x86_64_cpu cpu = {0};
            hl_x86_a64_request request;
            size_t byte;

            if (bits[width] == 16u) guest[size++] = 0x66u;
            if (bits[width] == 64u) guest[size++] = 0x48u;
            guest[size++] = bits[width] == 8u ? 0xf6u : 0xf7u;
            /* TEST [rax+8], immediate: displacement and operand must stay distinct. */
            guest[size++] = (uint8_t)(0x40u | extension << 3);
            guest[size++] = 8u;
            for (byte = 0; byte < immediate_size; ++byte)
                guest[size++] = (uint8_t)(immediate >> (byte * 8u));
            request = request_for(guest, size, host, provenance);
            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            CHECK(provenance[0].guest_size == size);
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            cpu.registers[0] = UINT64_C(0x2000);
            cpu.flags = UINT64_C(0xad7);
            cpu.memory_first = UINT64_C(0x2000);
            cpu.memory_last = UINT64_C(0x2010);
            cpu.memory_delta = (uint64_t)(uintptr_t)&backing[0] - UINT64_C(0x2000);
            cpu.memory_permissions = 1u;
            cpu.dirty_first = UINT64_MAX;
            alu_reference(4u, bits[width], values[sample], right, cpu.flags,
                          &ignored, &expected_flags);
            hl_x86_test_enter(&cpu, code);
            CHECK(cpu.flags == expected_flags && cpu.reason == 0u);
            CHECK(backing[0] == UINT64_C(0xfeedfacecafebeef) && backing[1] == values[sample]);
            CHECK(cpu.memory_written == 0u && cpu.dirty_first == UINT64_MAX);

            memset(&cpu, 0, sizeof cpu);
            cpu.registers[0] = UINT64_C(0x3000);
            cpu.flags = UINT64_C(0xad7);
            cpu.memory_first = UINT64_C(0x2000);
            cpu.memory_last = UINT64_C(0x2010);
            cpu.memory_permissions = 1u;
            cpu.dirty_first = UINT64_MAX;
            hl_x86_test_enter(&cpu, code);
            CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK);
            CHECK(cpu.fault_address == UINT64_C(0x3008));
            CHECK(cpu.fault_access == HL_NATIVE_ACCESS_READ && cpu.fault_size == bits[width] / 8u);
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.memory_written == 0u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int test_contract(void) {
    const uint8_t high[] = {0x4d, 0x85, 0xc1};
    const uint8_t byte_low[] = {0x84, 0xc1};
    const uint8_t byte_high[] = {0x84, 0xe5};
    const uint8_t byte_rex[] = {0x40, 0x84, 0xe5};
    const uint8_t narrow[] = {0x66, 0x85, 0xc0};
    const uint8_t memory[] = {0x48, 0xf7, 0x00, 1, 0, 0, 0};
    const uint8_t group[] = {0x48, 0xf7, 0xc8, 1, 0, 0, 0};
    const uint8_t truncated[] = {0x48, 0xa9, 1, 2};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(high, sizeof high, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(provenance[0].guest_size == sizeof high && host[0] == UINT32_C(0xaa0903f0));
    request = request_for(byte_low, sizeof byte_low, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof byte_low);
    request = request_for(byte_high, sizeof byte_high, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof byte_high);
    request = request_for(byte_rex, sizeof byte_rex, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && provenance[0].guest_size == sizeof byte_rex);
    request = request_for(high, sizeof high, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request.host_capacity = result.word_count - 1u;
    memset(host, 0xa5, sizeof host);
    memset(provenance, 0xa5, sizeof provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
    request = request_for(narrow, sizeof narrow, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(memory, sizeof memory, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(group, sizeof group, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(truncated, sizeof truncated, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
    return 0;
}

static int group3_contract(void) {
    unsigned width;
    unsigned extension;
    unsigned memory;

    for (width = 0; width < 4u; ++width) {
        for (extension = 0; extension < 4u; ++extension) {
            for (memory = 0; memory < 2u; ++memory) {
                uint8_t guest[16] = {0};
                size_t size = 0;
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result result;

                if (width == 1u) guest[size++] = 0x66u;
                if (width == 3u) guest[size++] = 0x48u;
                guest[size++] = width == 0u ? 0xf6u : 0xf7u;
                guest[size++] = (uint8_t)((memory == 0u ? 0xc0u : 0x00u) | extension << 3);
                if (extension < 2u) {
                    unsigned immediate = width == 0u ? 1u : width == 1u ? 2u : 4u;
                    size += immediate;
                }
                hl_x86_a64_request request = request_for(guest, size, host, provenance);
                CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
                CHECK(result.instruction_count == 1u && provenance[0].guest_size == size);
            }
        }
    }
    {
        const uint8_t lock_memory[] = {0xf0u, 0xf7u, 0x10u};
        const uint8_t lock_register[] = {0xf0u, 0xf7u, 0xd0u};
        const uint8_t lock_test[] = {0xf0u, 0xf7u, 0x00u, 1u, 0u, 0u, 0u};
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_result result;
        hl_x86_a64_request request = request_for(lock_memory, sizeof lock_memory, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        request = request_for(lock_register, sizeof lock_register, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
        request = request_for(lock_test, sizeof lock_test, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    }
    return 0;
}

static void rotate_reference(unsigned kind, unsigned bits, uint64_t value, unsigned count,
                             uint64_t prior, uint64_t *result, uint64_t *flags) {
    uint64_t mask = bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    unsigned raw = count & (bits == 64u ? 63u : 31u);
    unsigned effective = raw;
    uint64_t carry = (prior & HL_X86_RFLAGS_CF) != 0u;
    unsigned step;
    value &= mask;
    if ((kind == 2u || kind == 3u) && bits < 32u) effective %= bits + 1u;
    *result = value;
    *flags = prior;
    if (effective == 0u) return;
    for (step = 0; step < effective; ++step) {
        uint64_t next;
        if (kind == 0u || kind == 2u) {
            next = (value >> (bits - 1u)) & 1u;
            value = ((value << 1u) | (kind == 0u ? next : carry)) & mask;
        } else {
            next = value & 1u;
            value = (value >> 1u) | ((kind == 1u ? next : carry) << (bits - 1u));
        }
        carry = next;
    }
    *flags = (prior & ~HL_X86_RFLAGS_CF) | (carry != 0u ? HL_X86_RFLAGS_CF : 0u);
    if (raw == 1u) {
        uint64_t overflow = kind == 0u || kind == 2u ? ((value >> (bits - 1u)) ^ carry) & 1u :
                            ((value >> (bits - 1u)) ^ (value >> (bits - 2u))) & 1u;
        *flags = (*flags & ~HL_X86_RFLAGS_OF) | (overflow != 0u ? HL_X86_RFLAGS_OF : 0u);
    }
    *result = value;
}

static int rotate_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const unsigned bits[] = {8u, 16u, 32u, 64u};
    static const unsigned counts[] = {0u, 1u, 2u, 8u, 9u, 17u, 31u, 63u};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned kind;
    unsigned form;
    unsigned memory;
    size_t width;
    CHECK(code != MAP_FAILED);
    for (kind = 0; kind < 4u; ++kind) for (form = 0; form < 3u; ++form)
        for (memory = 0; memory < 2u; ++memory) for (width = 0; width < 4u; ++width) {
            size_t samples = form == 1u ? 1u : sizeof counts / sizeof counts[0];
            size_t sample;
            for (sample = 0; sample < samples; ++sample) {
                unsigned count = form == 1u ? 1u : counts[sample];
                uint8_t guest[8] = {0};
                size_t size = 0;
                uint8_t opcode;
                uint64_t initial = UINT64_C(0x887766554433aa81);
                uint64_t expected_value;
                uint64_t expected_flags;
                uint64_t expected_register;
                uint64_t expected_memory;
                uint64_t backing = initial;
                uint32_t host[512] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result emitted;
                hl_native_x86_64_cpu cpu = {0};
                hl_x86_a64_request request;
                if (bits[width] == 16u) guest[size++] = 0x66;
                if (bits[width] == 64u) guest[size++] = 0x48;
                opcode = form == 0u ? (bits[width] == 8u ? 0xc0u : 0xc1u) :
                         form == 1u ? (bits[width] == 8u ? 0xd0u : 0xd1u) :
                                      (bits[width] == 8u ? 0xd2u : 0xd3u);
                guest[size++] = opcode;
                guest[size++] = (uint8_t)((kind << 3) | (memory ? 3u : 0xc0u));
                if (form == 0u) guest[size++] = (uint8_t)count;
                request = request_for(guest, size, host, provenance);
                request.host_capacity = sizeof host / sizeof host[0];
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                request.host_capacity = emitted.word_count;
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[0] = initial;
                cpu.registers[1] = count;
                cpu.registers[3] = UINT64_C(0x2000);
                cpu.flags = UINT64_C(0xad7);
                cpu.memory_first = UINT64_C(0x2000);
                cpu.memory_last = UINT64_C(0x2008);
                cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
                cpu.memory_permissions = 3;
                rotate_reference(kind, bits[width], initial, count, cpu.flags,
                                 &expected_value, &expected_flags);
                if ((count & (bits[width] == 64u ? 63u : 31u)) == 0u ||
                    ((kind == 2u || kind == 3u) && bits[width] < 32u &&
                     (count & 31u) % (bits[width] + 1u) == 0u))
                    expected_register = initial;
                else expected_register = bits[width] == 8u ? (initial & ~UINT64_C(0xff)) | expected_value :
                                    bits[width] == 16u ? (initial & ~UINT64_C(0xffff)) | expected_value :
                                    bits[width] == 32u ? (uint32_t)expected_value : expected_value;
                expected_memory = bits[width] == 8u ? (initial & ~UINT64_C(0xff)) | expected_value :
                                  bits[width] == 16u ? (initial & ~UINT64_C(0xffff)) | expected_value :
                                  bits[width] == 32u ? (initial & ~UINT64_C(0xffffffff)) | expected_value :
                                                       expected_value;
                hl_x86_test_enter(&cpu, code);
                if (memory) CHECK(backing == expected_memory);
                else CHECK(cpu.registers[0] == expected_register);
                CHECK(cpu.flags == expected_flags && cpu.reason == 0);
                CHECK(cpu.memory_written == (memory && (count & (bits[width] == 64u ? 63u : 31u)) != 0u &&
                                              !((kind == 2u || kind == 3u) && bits[width] < 32u &&
                                                (count & 31u) % (bits[width] + 1u) == 0u)));
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static void shift_reference(unsigned kind, unsigned bits, uint64_t value, unsigned count,
                            uint64_t prior, uint64_t *result, uint64_t *flags) {
    uint64_t mask = bits == 64u ? UINT64_MAX : UINT64_C(0xffffffff);
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    uint64_t input = value & mask;
    uint64_t carry;

    count &= bits == 64u ? 63u : 31u;
    if (count == 0u) {
        *result = bits == 32u ? (uint32_t)input : input;
        *flags = prior;
        return;
    }
    carry = kind == 4u ? input >> (bits - count) & 1u : input >> (count - 1u) & 1u;
    if (kind == 4u) *result = input << count & mask;
    else if (kind == 5u) *result = input >> count;
    else *result = (uint64_t)(((int64_t)(input << (64u - bits)) >> (64u - bits)) >> count) & mask;
    if (bits == 32u) *result = (uint32_t)*result;
    *flags = prior & ~(HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF | HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF |
                       (count == 1u ? HL_X86_RFLAGS_OF : 0));
    if (carry) *flags |= HL_X86_RFLAGS_CF;
    *flags |= parity_flag(*result);
    if (*result == 0) *flags |= HL_X86_RFLAGS_ZF;
    if ((*result & sign) != 0) *flags |= HL_X86_RFLAGS_SF;
    if (count == 1u) {
        int overflow = kind == 4u ? ((*result & sign) != 0) != carry :
                       kind == 5u ? (input & sign) != 0 : 0;
        if (overflow) *flags |= HL_X86_RFLAGS_OF;
    }
}

static int shift_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const uint8_t kinds[] = {4, 5, 7};
    static const uint8_t counts[] = {0, 1, 2, 31, 63};
    static const uint64_t values[] = {UINT64_C(0x80000001), UINT64_C(0x8000000000000011), UINT64_MAX};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned operation;
    unsigned wide;
    unsigned sample;
    unsigned count_index;

    CHECK(code != MAP_FAILED);
    for (operation = 0; operation < 3u; ++operation) for (wide = 0; wide < 2u; ++wide)
        for (sample = 0; sample < 3u; ++sample) for (count_index = 0; count_index < 5u; ++count_index) {
            uint8_t guest[4] = {0};
            size_t size = 0;
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[8] = {0};
            hl_x86_a64_result emitted;
            hl_native_x86_64_cpu cpu = {0};
            uint64_t expected_result;
            uint64_t expected_flags;
            hl_x86_a64_request request;

            if (wide) guest[size++] = 0x48;
            guest[size++] = counts[count_index] == 1u ? 0xd1 : 0xc1;
            guest[size++] = (uint8_t)(0xc0u | kinds[operation] << 3);
            if (counts[count_index] != 1u) guest[size++] = counts[count_index];
            request = request_for(guest, size, host, provenance);
            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            cpu.registers[0] = values[sample];
            cpu.flags = UINT64_C(0xad7);
            shift_reference(kinds[operation], wide ? 64u : 32u, cpu.registers[0], counts[count_index],
                            cpu.flags, &expected_result, &expected_flags);
            hl_x86_test_enter(&cpu, code);
            CHECK(cpu.registers[0] == expected_result && cpu.flags == expected_flags);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
    {
        const uint8_t guest[] = {0x48, 0xd3, 0xe1}; /* shl rcx, cl */
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};
        uint64_t expected_result;
        uint64_t expected_flags;

        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[1] = UINT64_C(0x8000000000000001);
        cpu.flags = UINT64_C(0xad7);
        shift_reference(4u, 64u, cpu.registers[1], (unsigned)cpu.registers[1], cpu.flags,
                        &expected_result, &expected_flags);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[1] == expected_result && cpu.flags == expected_flags);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int shift_cl_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const uint8_t kinds[] = {4, 5, 7};
    static const uint8_t counts[] = {0, 1, 2, 31, 63};
    static const uint64_t values[] = {UINT64_C(0x80000001), UINT64_C(0x8000000000000011), UINT64_MAX};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned operation;
    unsigned wide;
    unsigned sample;
    unsigned count_index;

    CHECK(code != MAP_FAILED);
    for (operation = 0; operation < 3u; ++operation) for (wide = 0; wide < 2u; ++wide)
        for (sample = 0; sample < 3u; ++sample) for (count_index = 0; count_index < 5u; ++count_index) {
            uint8_t guest[] = {(uint8_t)(wide ? 0x48 : 0xd3),
                               (uint8_t)(wide ? 0xd3 : 0xc0u | kinds[operation] << 3),
                               (uint8_t)(wide ? 0xc0u | kinds[operation] << 3 : 0)};
            size_t size = wide ? 3u : 2u;
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[8] = {0};
            hl_x86_a64_request request = request_for(guest, size, host, provenance);
            hl_x86_a64_result emitted;
            hl_native_x86_64_cpu cpu = {0};
            uint64_t expected_result;
            uint64_t expected_flags;

            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            cpu.registers[0] = values[sample];
            cpu.registers[1] = counts[count_index];
            cpu.flags = UINT64_C(0xad7);
            shift_reference(kinds[operation], wide ? 64u : 32u, cpu.registers[0], counts[count_index],
                            cpu.flags, &expected_result, &expected_flags);
            hl_x86_test_enter(&cpu, code);
            CHECK(cpu.registers[0] == expected_result && cpu.flags == expected_flags);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int shift_contract(void) {
    const uint8_t high[] = {0x49, 0xc1, 0xe0, 2};
    const uint8_t narrow[] = {0x66, 0xd1, 0xe0};
    const uint8_t memory[] = {0x48, 0xc1, 0x20, 1};
    const uint8_t rotate[] = {0x48, 0xc1, 0xc0, 1};
    const uint8_t variable[] = {0x48, 0xd3, 0xe0};
    const uint8_t variable_high[] = {0x49, 0xd3, 0xe0};
    const uint8_t truncated[] = {0x48, 0xc1, 0xe0};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(high, sizeof high, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(provenance[0].guest_size == sizeof high && provenance[0].word_start == 0);
    CHECK(provenance[0].word_end > 20 && host[0] == UINT32_C(0xaa0803f0));
    request.host_capacity = result.word_count - 1u;
    memset(host, 0xa5, sizeof host);
    memset(provenance, 0xa5, sizeof provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
    request = request_for(narrow, sizeof narrow, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_FLAGS_ABI_REQUIRED);
    request = request_for(memory, sizeof memory, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    request = request_for(rotate, sizeof rotate, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(variable, sizeof variable, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(variable_high, sizeof variable_high, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(provenance[0].guest_size == sizeof variable_high && host[0] == UINT32_C(0xaa0803f0));
    request.host_capacity = result.word_count - 1u;
    memset(host, 0xa5, sizeof host);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
    request = request_for(truncated, sizeof truncated, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
    return 0;
}

static int address_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const struct {
        uint8_t bytes[8];
        uint8_t size;
        uint8_t destination;
        uint8_t kind;
    } cases[] = {
        {{0x48, 0x8d, 0x43, 0x20}, 4, 0, 0},
        {{0x48, 0x8d, 0x44, 0x8b, 0xf0}, 5, 0, 1},
        {{0x48, 0x8d, 0x05, 0x10, 0, 0, 0}, 7, 0, 2},
        {{0x48, 0x8d, 0x04, 0x8d, 0, 0, 0x20, 0}, 8, 0, 3},
        {{0x4f, 0x8d, 0x44, 0xcc, 0x7f}, 5, 8, 4},
        {{0x48, 0x8d, 0x5c, 0x5b, 0x08}, 5, 3, 5},
        {{0x8d, 0x43, 0xff}, 3, 0, 6},
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t index;

    CHECK(code != MAP_FAILED);
    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};
        uint64_t expected;

        cpu.registers[1] = 7;
        cpu.registers[3] = UINT64_C(0x1234567890);
        cpu.registers[9] = 11;
        cpu.registers[12] = UINT64_C(0x1000);
        cpu.flags = UINT64_C(0xad7);
        if (cases[index].kind == 0u) expected = cpu.registers[3] + 0x20;
        else if (cases[index].kind == 1u) expected = cpu.registers[3] + cpu.registers[1] * 4 - 16;
        else if (cases[index].kind == 2u) expected = request.guest_pc + cases[index].size + 0x10;
        else if (cases[index].kind == 3u) expected = cpu.registers[1] * 4 + UINT64_C(0x200000);
        else if (cases[index].kind == 4u) expected = cpu.registers[12] + cpu.registers[9] * 8 + 0x7f;
        else if (cases[index].kind == 5u) expected = cpu.registers[3] * 3 + 8;
        else expected = (uint32_t)(cpu.registers[3] - 1);
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        CHECK(provenance[0].guest_size == cases[index].size && provenance[0].word_start == 0);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[cases[index].destination] == expected);
        CHECK(cpu.flags == UINT64_C(0xad7));
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int address_contract(void) {
    const uint8_t address32[] = {0x67, 0x48, 0x8d, 0x03};
    const uint8_t narrow[] = {0x66, 0x8d, 0x03};
    const uint8_t register_form[] = {0x48, 0x8d, 0xc3};
    const uint8_t truncated[] = {0x48, 0x8d, 0x44};
    const uint8_t valid[] = {0x48, 0x8d, 0x44, 0x8b, 0x10};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(valid, sizeof valid, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(provenance[0].guest_size == sizeof valid && provenance[0].word_end > 0);
    request.host_capacity = result.word_count - 1u;
    memset(host, 0xa5, sizeof host);
    memset(provenance, 0xa5, sizeof provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
    request = request_for(address32, sizeof address32, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    request = request_for(narrow, sizeof narrow, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    request = request_for(register_form, sizeof register_form, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    request = request_for(truncated, sizeof truncated, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
    return 0;
}

static int register_extensions(void) {
    const uint8_t guest[] = {
        0x0f, 0xc8,                         /* bswap eax */
        0x49, 0x0f, 0xc9,                   /* bswap r9 */
        0x0f, 0xb6, 0xc4,                   /* movzx eax, ah */
        0x40, 0x0f, 0xb6, 0xc4,             /* movzx eax, spl */
        0x4d, 0x0f, 0xbf, 0xca,             /* movsx r9, r10w */
        0x66, 0x0f, 0xb6, 0xc1,             /* movzx ax, cl */
        0x63, 0xc1,                         /* mov eax, ecx */
        0x48, 0x63, 0xc1,                   /* movsxd rax, ecx */
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(guest, sizeof guest, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 8 && result.exit_pc == UINT64_C(0x400019));
    CHECK(host[0] == UINT32_C(0x5ac00800));
    CHECK(host[1] == UINT32_C(0xdac00d29));
    CHECK(host[2] == UINT32_C(0x53083c10));
    CHECK(host[3] == UINT32_C(0x12001e00));
    CHECK(host[4] == UINT32_C(0x12001c80));
    CHECK(host[5] == UINT32_C(0x93403d49));
    CHECK(host[6] == UINT32_C(0x12001c30));
    CHECK(host[7] == UINT32_C(0xb3403e00));
    CHECK(host[8] == UINT32_C(0x2a0103e0));
    CHECK(host[9] == UINT32_C(0x93407c20));
    CHECK(provenance[2].guest_size == 3 && provenance[3].guest_size == 4);
    CHECK(provenance[5].guest_size == 4 && provenance[7].guest_size == 3);
    return 0;
}

static int extension_fallback(void) {
    const uint8_t movzx_memory[] = {0x0f, 0xb6, 0x00};
    const uint8_t movsxd_memory[] = {0x48, 0x63, 0x00};
    const uint8_t truncated[] = {0x4d, 0x0f, 0xbf};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(movzx_memory, sizeof movzx_memory, host, provenance);
    hl_x86_a64_result result;

    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.exit_pc == UINT64_C(0x400003));
    request = request_for(movsxd_memory, sizeof movsxd_memory, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.exit_pc == UINT64_C(0x400003));
    request = request_for(truncated, sizeof truncated, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_TRUNCATED);
    CHECK(result.exit_pc == UINT64_C(0x400000));
    return 0;
}

static int memory_extension_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const struct {
        uint8_t bytes[5];
        uint8_t size;
        uint64_t input;
        uint64_t initial;
        uint64_t expected;
    } cases[] = {
        {{0x44, 0x0f, 0xb6, 0x08}, 4, UINT64_C(0x80), 0, UINT64_C(0x80)},
        {{0x4c, 0x0f, 0xb6, 0x08}, 4, UINT64_C(0x80), 0, UINT64_C(0x80)},
        {{0x4c, 0x0f, 0xbe, 0x08}, 4, UINT64_C(0x80), 0, UINT64_C(0xffffffffffffff80)},
        {{0x44, 0x0f, 0xb7, 0x08}, 4, UINT64_C(0x8001), 0, UINT64_C(0x8001)},
        {{0x4c, 0x0f, 0xbf, 0x08}, 4, UINT64_C(0x8001), 0, UINT64_C(0xffffffffffff8001)},
        {{0x66, 0x44, 0x0f, 0xbf, 0x08}, 5, UINT64_C(0x8001), UINT64_C(0x1122334455667788),
         UINT64_C(0x1122334455668001)},
        {{0x4c, 0x63, 0x08}, 3, UINT64_C(0x80000001), 0, UINT64_C(0xffffffff80000001)},
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t index;

    CHECK(code != MAP_FAILED);
    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint64_t backing = cases[index].input;
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size, host, provenance);
        hl_x86_a64_result result;
        hl_native_x86_64_cpu cpu = {0};

        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = UINT64_C(0x2000);
        cpu.registers[9] = cases[index].initial;
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x2008);
        cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
        cpu.memory_permissions = 1;
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[9] == cases[index].expected);
        CHECK(cpu.fault_access == 0 && cpu.reason == 0);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static void imul_reference(unsigned bits, uint64_t left, uint64_t right, uint64_t prior,
                           uint64_t *result, uint64_t *flags) {
    uint64_t mask = bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    __int128 a = (__int128)(int64_t)((left & mask) << (64u - bits)) >> (64u - bits);
    __int128 b = (__int128)(int64_t)((right & mask) << (64u - bits)) >> (64u - bits);
    __int128 product = a * b;
    int64_t truncated = (int64_t)(((uint64_t)product & mask) << (64u - bits)) >> (64u - bits);
    int overflow = product != (__int128)truncated;
    *result = (uint64_t)product & mask;
    *flags = prior & ~(HL_X86_RFLAGS_CF | HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF | HL_X86_RFLAGS_OF);
    if (overflow) *flags |= HL_X86_RFLAGS_CF | HL_X86_RFLAGS_OF;
}

static void mul_reference(unsigned bits, uint64_t left, uint64_t right, uint64_t prior,
                          uint64_t *low, uint64_t *high, uint64_t *flags) {
    uint64_t mask = bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    unsigned __int128 product = (unsigned __int128)(left & mask) * (right & mask);
    *low = (uint64_t)product & mask;
    *high = (uint64_t)(product >> bits) & mask;
    *flags = prior & ~(HL_X86_RFLAGS_CF | HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF | HL_X86_RFLAGS_OF);
    if (*high != 0u) *flags |= HL_X86_RFLAGS_CF | HL_X86_RFLAGS_OF;
}

static int mul_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const unsigned bits[] = {8u, 16u, 32u, 64u};
    static const uint64_t values[] = {0u, 1u, UINT64_C(0xff), UINT64_C(0x80000001), UINT64_MAX};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned memory;
    size_t width;
    size_t value;
    CHECK(code != MAP_FAILED);
    for (memory = 0; memory < 2u; ++memory) {
        for (width = 0; width < sizeof bits / sizeof bits[0]; ++width) {
            for (value = 0; value < sizeof values / sizeof values[0]; ++value) {
                uint8_t guest[5] = {0};
                size_t size = 0;
                uint64_t left = values[(value + 2u) % (sizeof values / sizeof values[0])];
                uint64_t right = values[value];
                uint64_t backing = right;
                uint64_t low;
                uint64_t high;
                uint64_t expected_flags;
                uint64_t initial_high = UINT64_C(0x7766554433221100);
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result emitted;
                hl_native_x86_64_cpu cpu = {0};
                hl_x86_a64_request request;
                if (bits[width] == 16u) guest[size++] = 0x66u;
                guest[size++] = bits[width] == 64u ? 0x49u : 0x41u;
                guest[size++] = bits[width] == 8u ? 0xf6u : 0xf7u;
                guest[size++] = memory ? 0x21u : 0xe1u; /* /4 [r9] or /4 r9 */
                request = request_for(guest, size, host, provenance);
                request.host_capacity = sizeof host / sizeof host[0];
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                request.host_capacity = emitted.word_count;
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                CHECK(provenance[0].guest_size == size);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[0] = left;
                cpu.registers[2] = initial_high;
                cpu.registers[9] = memory ? UINT64_C(0x2000) : right;
                cpu.flags = UINT64_C(0xad7);
                cpu.memory_first = UINT64_C(0x2000);
                cpu.memory_last = UINT64_C(0x2008);
                cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
                cpu.memory_permissions = 1u;
                mul_reference(bits[width], left, right, cpu.flags, &low, &high, &expected_flags);
                hl_x86_test_enter(&cpu, code);
                if (bits[width] == 8u)
                    CHECK(cpu.registers[0] == ((left & ~UINT64_C(0xffff)) | low | high << 8));
                else if (bits[width] == 16u) {
                    CHECK(cpu.registers[0] == ((left & ~UINT64_C(0xffff)) | low));
                    CHECK(cpu.registers[2] == ((initial_high & ~UINT64_C(0xffff)) | high));
                } else {
                    CHECK(cpu.registers[0] == low && cpu.registers[2] == high);
                }
                CHECK(cpu.flags == expected_flags && cpu.reason == 0u);
                if (memory) {
                    hl_native_x86_64_cpu fault = {0};
                    fault.registers[0] = left;
                    fault.registers[2] = initial_high;
                    fault.registers[9] = UINT64_C(0x2000);
                    fault.flags = UINT64_C(0xad7);
                    fault.memory_first = UINT64_C(0x2000);
                    fault.memory_last = UINT64_C(0x2000);
                    fault.memory_permissions = 1u;
                    hl_x86_test_enter(&fault, code);
                    CHECK(fault.reason == 3u && fault.fault_access == 1u);
                    CHECK(fault.fault_address == UINT64_C(0x2000));
                    CHECK(fault.fault_size == bits[width] / 8u);
                    CHECK(fault.registers[0] == left && fault.registers[2] == initial_high);
                    CHECK(fault.flags == UINT64_C(0xad7));
                }
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    {
        const uint8_t high_byte[] = {0xf6u, 0xe4u}; /* mul ah */
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_result emitted;
        hl_x86_a64_request request = request_for(high_byte, sizeof high_byte, host, provenance);
        hl_native_x86_64_cpu cpu = {0};
        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = UINT64_C(0x1122334455660305);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[0] == UINT64_C(0x112233445566000f));
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int imul_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const unsigned bits[] = {16u, 32u, 64u};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned form;
    unsigned memory;
    size_t width;
    CHECK(code != MAP_FAILED);
    for (form = 0; form < 3u; ++form) {
        for (memory = 0; memory < 2u; ++memory) {
            for (width = 0; width < sizeof bits / sizeof bits[0]; ++width) {
                uint8_t guest[12] = {0};
                size_t size = 0;
                uint8_t rex = (uint8_t)((bits[width] == 64u ? 0x4cu : 0x44u) |
                                        (memory ? 0u : 1u));
                uint64_t source = bits[width] == 16u ? UINT64_C(0x4001) :
                                  bits[width] == 32u ? UINT64_C(0x40000001) :
                                                       UINT64_C(0x4000000000000001);
                uint64_t right = form == 0u ? source : UINT64_MAX - 2u;
                uint64_t initial = UINT64_C(0x8877665544332211);
                uint64_t product;
                uint64_t expected_flags;
                uint64_t expected;
                uint64_t backing = source;
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[8] = {0};
                hl_x86_a64_result emitted;
                hl_native_x86_64_cpu cpu = {0};
                hl_x86_a64_request request;
                if (bits[width] == 16u) guest[size++] = 0x66;
                guest[size++] = rex;
                if (form == 0u) { guest[size++] = 0x0f; guest[size++] = 0xaf; }
                else guest[size++] = form == 1u ? 0x69 : 0x6b;
                guest[size++] = memory ? 0x08 : 0xca;
                if (form == 1u) {
                    size_t count = bits[width] == 16u ? 2u : 4u;
                    size_t byte;
                    for (byte = 0; byte < count; ++byte) guest[size++] = (uint8_t)(UINT64_C(0xfffffffd) >> (byte * 8u));
                } else if (form == 2u) guest[size++] = 0xfd;
                request = request_for(guest, size, host, provenance);
                request.host_capacity = sizeof host / sizeof host[0];
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                request.host_capacity = emitted.word_count;
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                cpu.registers[0] = UINT64_C(0x2000);
                cpu.registers[9] = form == 0u ? source : initial;
                cpu.registers[10] = source;
                cpu.flags = UINT64_C(0xad7);
                cpu.memory_first = UINT64_C(0x2000);
                cpu.memory_last = UINT64_C(0x2008);
                cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
                cpu.memory_permissions = 1;
                imul_reference(bits[width], source, right, cpu.flags, &product, &expected_flags);
                expected = bits[width] == 16u ? (cpu.registers[9] & ~UINT64_C(0xffff)) | product :
                           bits[width] == 32u ? (uint32_t)product : product;
                hl_x86_test_enter(&cpu, code);
                CHECK(cpu.registers[9] == expected);
                CHECK(cpu.flags == expected_flags);
                CHECK(cpu.fault_access == 0 && cpu.reason == 0);
                if (memory) {
                    memset(&cpu, 0, sizeof cpu);
                    cpu.registers[0] = UINT64_C(0x2000);
                    cpu.registers[9] = initial;
                    cpu.memory_first = UINT64_C(0x2000);
                    cpu.memory_last = UINT64_C(0x2000);
                    cpu.memory_permissions = 1;
                    hl_x86_test_enter(&cpu, code);
                    CHECK(cpu.reason == 3 && cpu.fault_access == 1);
                    CHECK(cpu.fault_address == UINT64_C(0x2000) && cpu.fault_size == bits[width] / 8u);
                }
                CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
            }
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int multibyte_nop_contract(void) {
    static const struct { uint8_t bytes[10]; uint8_t size; } cases[] = {
        {{0x0f, 0x1f, 0x00}, 3}, {{0x0f, 0x1f, 0x44, 0x00, 0x00}, 5},
        {{0x66, 0x0f, 0x1f, 0x84, 0x00, 1, 2, 3, 4}, 9},
        {{0x67, 0x0f, 0x1f, 0x05, 1, 2, 3, 4}, 8}, {{0x0f, 0x1f, 0xc0}, 3},
    };
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    size_t index;
    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size, host, provenance);
        hl_x86_a64_result result;
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1 && provenance[0].guest_size == cases[index].size);
    }
    return 0;
}

static int control_family(void) {
    const struct { const uint8_t *bytes; size_t size; uint32_t guest_size; } cases[] = {
        {(const uint8_t[]){0xe8, 2, 0, 0, 0}, 5, 5},
        {(const uint8_t[]){0xc3}, 1, 1},
        {(const uint8_t[]){0xc2, 0x10, 0}, 3, 3},
        {(const uint8_t[]){0xff, 0xd0}, 2, 2},
        {(const uint8_t[]){0xff, 0x13}, 2, 2},
    };
    uint32_t host[512];
    hl_x86_a64_provenance provenance[8];
    size_t index;
    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        memset(host, 0, sizeof host); memset(provenance, 0, sizeof provenance);
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size, host, provenance);
        hl_x86_a64_result result;
        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        CHECK(result.exit == (index == 0 ? HL_X86_A64_DIRECT_CALL : HL_X86_A64_DYNAMIC_BRANCH) &&
              result.instruction_count == 1);
        CHECK(provenance[0].guest_size == cases[index].guest_size && provenance[0].word_end > 0);
    }
    return 0;
}

static int indirect_control_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const struct { uint8_t bytes[3]; uint8_t size; uint8_t call; uint8_t memory; } cases[] = {
        {{0x49, 0xff, 0xd0}, 3, 1, 0}, {{0xff, 0x10}, 2, 1, 1},
        {{0x3e, 0x49, 0xff}, 0, 0, 0},
        {{0x49, 0xff, 0xe0}, 3, 0, 0}, {{0xff, 0x20}, 2, 0, 1},
        {{0x67, 0xff, 0x20}, 3, 0, 1},
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t index;
    CHECK(code != MAP_FAILED);
    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint8_t notrack[] = {0x3e, 0x49, 0xff, 0xe0};
        const uint8_t *guest = cases[index].size == 0u ? notrack : cases[index].bytes;
        size_t size = cases[index].size == 0u ? sizeof notrack : cases[index].size;
        uint64_t backing[514] = {0};
        uint64_t target = UINT64_C(0x5566778899aabbcc);
        uint32_t host[512] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(guest, size, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};
        int call = cases[index].call != 0;
        int memory = cases[index].memory != 0;
        backing[0] = target;
        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = UINT64_C(0x2000);
        cpu.registers[4] = UINT64_C(0x3008);
        cpu.registers[8] = target;
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x3010);
        cpu.memory_delta = (uint64_t)(uintptr_t)backing - UINT64_C(0x2000);
        cpu.memory_permissions = 3;
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.program == target && cpu.indirect_site == UINT64_C(0x400000));
        if (call) {
            CHECK(cpu.registers[4] == UINT64_C(0x3000));
            CHECK(backing[512] == UINT64_C(0x400000) + size);
        } else CHECK(cpu.registers[4] == UINT64_C(0x3008));
        CHECK(cpu.reason == 0);
        if (call && memory) {
            memset(&cpu, 0, sizeof cpu);
            cpu.registers[0] = UINT64_C(0x2000);
            cpu.registers[4] = UINT64_C(0x3008);
            cpu.memory_first = UINT64_C(0x2000);
            cpu.memory_last = UINT64_C(0x3010);
            cpu.memory_delta = (uint64_t)(uintptr_t)backing - UINT64_C(0x2000);
            cpu.memory_permissions = 1;
            hl_x86_test_enter(&cpu, code);
            CHECK(cpu.reason == 3 && cpu.fault_access == 2);
            CHECK(cpu.registers[4] == UINT64_C(0x3008) && cpu.program == UINT64_C(0x400000));
            CHECK(cpu.indirect_site == 0);
        }
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int leave_contract(void) {
    const uint8_t leave[] = {0xc9};
    const uint8_t narrow[] = {0x66, 0xc9};
    uint32_t host[256] = {0};
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = request_for(leave, sizeof leave, host, provenance);
    hl_x86_a64_result result;

    request.host_capacity = sizeof host / sizeof host[0];
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.exit == HL_X86_A64_FALLTHROUGH && result.instruction_count == 1);
    CHECK(provenance[0].guest_size == 1 && provenance[0].word_end > 0);
    request.host_capacity = result.word_count - 1u;
    memset(host, 0xa5, sizeof host);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_CAPACITY);
    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
    request = request_for(narrow, sizeof narrow, host, provenance);
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);

#if defined(__aarch64__)
    {
        long page = sysconf(_SC_PAGESIZE);
        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        const uint64_t frame = UINT64_C(0x8877665544332211);
        hl_native_x86_64_cpu cpu = {0};

        CHECK(code != MAP_FAILED);
        request = request_for(leave, sizeof leave, host, provenance);
        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
        memcpy(code, host, result.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[4] = UINT64_C(0xdeadbeef);
        cpu.registers[5] = UINT64_C(0x2000);
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x2008);
        cpu.memory_delta = (uint64_t)(uintptr_t)&frame - UINT64_C(0x2000);
        cpu.memory_permissions = 1;
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.registers[4] == UINT64_C(0x2008));
        CHECK(cpu.registers[5] == frame);
        CHECK(cpu.fault_access == 0 && cpu.reason == 0);
        CHECK(munmap(code, (size_t)page) == 0);
    }
#endif
    return 0;
}

static uint32_t vector_fragment(uint32_t *host, size_t host_capacity, int write,
                                unsigned vector, unsigned width) {
    instruction item;
    uint32_t cursor = 0;

    memset(&item, 0, sizeof item);
    item.pc = UINT64_C(0x45678000);
    item.operation = OP_VECTOR;
    item.width = (uint8_t)width;
    item.destination = (uint8_t)vector;
    item.source = (uint8_t)vector;
    item.memory_operand = 1u;
    item.memory_write = (uint8_t)write;
    item.address_base = 3u;
    item.address_index = UINT8_MAX;
    CHECK(hl_x86_vector_words(&item) + 1u <= host_capacity);
    hl_x86_emit_vector(host, &cursor, &item);
    CHECK(cursor == hl_x86_vector_words(&item));
    host[cursor++] = UINT32_C(0xd65f03c0);
    return cursor;
}

static int vector_projection(void) {
#if !defined(__aarch64__)
    return 0;
#else
    _Alignas(16) uint8_t storage[32];
    const uint8_t value[16] = {
        0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87,
        0x98, 0xa9, 0xba, 0xcb, 0xdc, 0xed, 0xfe, 0x0f,
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint32_t host[256] = {0};
    uint32_t count;
    hl_native_x86_64_cpu cpu;
    uint8_t unchanged[16];

    CHECK(code != MAP_FAILED);
    memset(unchanged, 0x5a, sizeof unchanged);
    memset(storage, 0xa5, sizeof storage);
    memcpy(storage + 8, value, sizeof value);
    count = vector_fragment(host, sizeof host / sizeof host[0], 0, 9u, 16u);
    memcpy(code, host, count * sizeof(uint32_t));
    __builtin___clear_cache((char *)code, (char *)code + count * sizeof(uint32_t));
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);

    memset(&cpu, 0, sizeof cpu);
    memset(&cpu.vectors[18], 0x5a, 16);
    cpu.registers[3] = UINT64_C(0x1008);
    cpu.memory_first = UINT64_C(0x1000);
    cpu.memory_last = UINT64_C(0x1020);
    cpu.memory_delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x1000);
    cpu.memory_permissions = 1u;
    cpu.dirty_first = UINT64_MAX;
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(&cpu.vectors[18], value, sizeof value) == 0);
    CHECK(cpu.fault_access == 0 && cpu.reason == 0);

    memset(&cpu.vectors[18], 0x5a, 16);
    cpu.memory_last = UINT64_C(0x1010);
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(&cpu.vectors[18], unchanged, 16) == 0);
    CHECK(cpu.fault_address == UINT64_C(0x1008));
    CHECK(cpu.fault_access == 1 && cpu.fault_size == 16);
    CHECK(cpu.program == UINT64_C(0x45678000) && cpu.reason == HL_NATIVE_EXIT_FALLBACK);

    cpu.reason = cpu.fault_access = cpu.fault_size = 0;
    cpu.memory_last = UINT64_C(0x1020);
    cpu.memory_permissions = 2u;
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(&cpu.vectors[18], unchanged, 16) == 0);
    CHECK(cpu.fault_access == 1 && cpu.fault_size == 16);

    cpu.reason = cpu.fault_access = cpu.fault_size = 0;
    cpu.registers[3] = UINT64_MAX - 7u;
    cpu.memory_first = 0;
    cpu.memory_last = UINT64_MAX;
    cpu.memory_permissions = 1u;
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(&cpu.vectors[18], unchanged, 16) == 0);
    CHECK(cpu.fault_address == UINT64_MAX - 7u && cpu.fault_access == 1 && cpu.fault_size == 16);

    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    memset(host, 0, sizeof host);
    count = vector_fragment(host, sizeof host / sizeof host[0], 1, 8u, 16u);
    memcpy(code, host, count * sizeof(uint32_t));
    __builtin___clear_cache((char *)code, (char *)code + count * sizeof(uint32_t));
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    memset(&cpu, 0, sizeof cpu);
    memcpy(&cpu.vectors[16], value, sizeof value);
    cpu.registers[3] = UINT64_C(0x1008);
    cpu.memory_first = UINT64_C(0x1000);
    cpu.memory_last = UINT64_C(0x1020);
    cpu.memory_delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x1000);
    cpu.memory_permissions = 1u;
    cpu.dirty_first = UINT64_MAX;
    memset(storage + 8, 0x33, 16);
    memset(unchanged, 0x33, sizeof unchanged);
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(storage + 8, unchanged, 16) == 0);
    CHECK(cpu.fault_access == 2 && cpu.fault_size == 16 && cpu.memory_written == 0);
    CHECK(cpu.dirty_first == UINT64_MAX && cpu.dirty_count == 0u);

    cpu.reason = cpu.fault_access = cpu.fault_size = 0;
    cpu.memory_permissions = 2u;
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(storage + 8, value, 16) == 0);
    CHECK(cpu.fault_access == 0 && cpu.reason == 0 && cpu.memory_written == 1);
    CHECK(cpu.dirty_first == UINT64_C(0x1008) && cpu.dirty_last == UINT64_C(0x1018));
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int guarded_load_store_handoff(void) {
#if !defined(__aarch64__)
    return 0;
#else
    _Alignas(8) uint64_t storage[2] = {UINT64_C(0x8877665544332211), 0};
    long page = sysconf(_SC_PAGESIZE);
    uint32_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint32_t cursor = 0;
    instruction load = {.pc = UINT64_C(0x45670000), .operation = OP_LOAD, .destination = 18u,
                        .width = 8u, .load_width = 8u, .address_base = 6u,
                        .address_index = UINT8_MAX};
    instruction store = {.pc = UINT64_C(0x45670000), .operation = OP_STORE, .source = 19u,
                         .width = 8u, .address_base = 7u, .address_index = UINT8_MAX};
    hl_native_x86_64_cpu cpu = {0};
    CHECK(code != MAP_FAILED);
    hl_x86_emit_load(code, &cursor, &load);
    code[cursor++] = UINT32_C(0xaa1203f3); /* mov x19,x18 */
    hl_x86_emit_store(code, &cursor, &store);
    code[cursor++] = UINT32_C(0xd65f03c0);
    __builtin___clear_cache((char *)code, (char *)code + cursor * sizeof(uint32_t));
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    cpu.registers[6] = UINT64_C(0x8000); cpu.registers[7] = UINT64_C(0x8008);
    cpu.memory_first = UINT64_C(0x8000); cpu.memory_last = UINT64_C(0x8010);
    cpu.memory_delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x8000);
    cpu.memory_permissions = 3u; cpu.dirty_first = UINT64_MAX;
    hl_native_x86_64_enter(&cpu, code);
    CHECK(storage[1] == storage[0]);
    CHECK(cpu.fault_access == 0 && cpu.memory_written == 1);
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int vector_256_memory_owner(void) {
#if !defined(__aarch64__)
    return 0;
#else
    _Alignas(32) uint8_t storage[64];
    uint8_t expected[32], before[32];
    long page = sysconf(_SC_PAGESIZE);
    uint32_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint32_t host[384] = {0};
    hl_native_x86_64_cpu cpu = {0};
    uint32_t count;
    unsigned index;

    CHECK(code != MAP_FAILED);
    for (index = 0; index < sizeof expected; ++index) expected[index] = (uint8_t)(index * 13u + 5u);
    memcpy(storage + 7, expected, sizeof expected); /* deliberately unaligned */
    count = vector_fragment(host, sizeof host / sizeof host[0], 0, 6u, 32u);
    memcpy(code, host, count * sizeof(uint32_t));
    __builtin___clear_cache((char *)code, (char *)code + count * sizeof(uint32_t));
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    memset(&cpu.vectors[12], 0xa5, 16); memset(&cpu.vector_upper[12], 0xa5, 16);
    cpu.registers[3] = UINT64_C(0x2017); cpu.memory_first = UINT64_C(0x2000);
    cpu.memory_last = UINT64_C(0x2037);
    cpu.memory_delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x2010);
    cpu.memory_permissions = 1u; cpu.dirty_first = UINT64_MAX;
    cpu.read_incarnation = cpu.read_token = 7u; cpu.read_count = 1u;
    cpu.read_views[0][0] = UINT64_C(0x2000); cpu.read_views[0][1] = UINT64_C(0x2037);
    cpu.read_views[0][2] = cpu.memory_delta; cpu.read_views[0][3] = 1u;
    cpu.memory_first = cpu.memory_last = 0; /* success must come from the published cache view */
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(&cpu.vectors[12], expected, 16) == 0);
    CHECK(memcmp(&cpu.vector_upper[12], expected + 16, 16) == 0);

    memset(&cpu.vectors[12], 0x5a, 16); memset(&cpu.vector_upper[12], 0x5a, 16);
    memset(before, 0x5a, sizeof before); cpu.memory_last = UINT64_C(0x2027);
    cpu.memory_first = UINT64_C(0x2000); cpu.read_token = 0;
    cpu.reason = cpu.fault_access = cpu.fault_size = 0;
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(&cpu.vectors[12], before, 16) == 0);
    CHECK(memcmp(&cpu.vector_upper[12], before + 16, 16) == 0);
    CHECK(cpu.fault_access == 1u && cpu.fault_size == 32u);

    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    count = vector_fragment(host, sizeof host / sizeof host[0], 1, 6u, 32u);
    memcpy(code, host, count * sizeof(uint32_t));
    __builtin___clear_cache((char *)code, (char *)code + count * sizeof(uint32_t));
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    memcpy(&cpu.vectors[12], expected, 16); memcpy(&cpu.vector_upper[12], expected + 16, 16);
    memset(storage + 7, 0x33, 32); memcpy(before, storage + 7, 32);
    cpu.memory_permissions = 2u; cpu.memory_last = UINT64_C(0x2027);
    cpu.reason = cpu.fault_access = cpu.fault_size = cpu.memory_written = 0;
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(storage + 7, before, 32) == 0 && cpu.memory_written == 0u);
    cpu.memory_last = UINT64_C(0x2037); cpu.reason = cpu.fault_access = cpu.fault_size = 0;
    hl_native_x86_64_enter(&cpu, code);
    CHECK(memcmp(storage + 7, expected, 32) == 0 && cpu.memory_written == 1u);
    CHECK(cpu.dirty_first == UINT64_C(0x2017) && cpu.dirty_last == UINT64_C(0x2037));
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int vector_host_preservation(void) {
#if !defined(__aarch64__)
    return 0;
#else
    _Alignas(16) uint8_t expected[128];
    _Alignas(16) uint8_t observed[128];
    long page = sysconf(_SC_PAGESIZE);
    uint32_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_native_x86_64_cpu cpu = {0};
    unsigned index;

    CHECK(code != MAP_FAILED);
    code[0] = UINT32_C(0xd65f03c0);
    __builtin___clear_cache((char *)code, (char *)code + sizeof(uint32_t));
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    for (index = 0; index < sizeof expected; ++index) expected[index] = (uint8_t)(index * 29u + 7u);
    memset(observed, 0, sizeof observed);
    memset(cpu.vectors, 0xc3, sizeof cpu.vectors);
    hl_x86_test_preserved(&cpu, code, expected, observed);
    CHECK(memcmp(observed, expected, sizeof expected) == 0);
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int vector_opcode(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const uint8_t load[] = {0x44, 0x0f, 0x10, 0x0b};
    static const uint8_t store[] = {0x44, 0x0f, 0x11, 0x0b};
    static const uint8_t copy[] = {0x45, 0x0f, 0x10, 0xc1};
    static const uint8_t from32[] = {0x66, 0x44, 0x0f, 0x6e, 0xc0};
    static const uint8_t from64[] = {0x66, 0x4d, 0x0f, 0x6e, 0xc1};
    static const uint8_t to32[] = {0x66, 0x44, 0x0f, 0x7e, 0xc0};
    static const uint8_t to64[] = {0x66, 0x4d, 0x0f, 0x7e, 0xc1};
    static const uint8_t low64[] = {0xf3, 0x45, 0x0f, 0x7e, 0xc1};
    static const uint8_t low_memory[] = {0xf3, 0x44, 0x0f, 0x7e, 0x03};
    static const uint8_t low_store[] = {0x66, 0x45, 0x0f, 0xd6, 0xc1};
    static const uint8_t low_store_memory[] = {0x66, 0x44, 0x0f, 0xd6, 0x03};
    static const struct {
        const uint8_t *bytes;
        size_t size;
    } cases[] = {
        {load, sizeof load}, {store, sizeof store}, {copy, sizeof copy},
        {from32, sizeof from32}, {from64, sizeof from64}, {to32, sizeof to32},
        {to64, sizeof to64}, {low64, sizeof low64}, {low_memory, sizeof low_memory},
        {low_store, sizeof low_store}, {low_store_memory, sizeof low_store_memory},
    };
    _Alignas(16) uint8_t storage[32];
    const uint64_t low = UINT64_C(0x0123456789abcdef);
    const uint64_t high = UINT64_C(0xfedcba9876543210);
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t index;

    CHECK(code != MAP_FAILED);
    for (index = 0; index < sizeof cases / sizeof cases[0]; ++index) {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(cases[index].bytes, cases[index].size,
                                                  host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};

        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        CHECK(emitted.instruction_count == 1 && provenance[0].guest_size == cases[index].size);
        if (index == 0u) {
            uint32_t staged[128];
            hl_x86_a64_result untouched;

            memset(staged, 0xa5, sizeof staged);
            memset(&untouched, 0xa5, sizeof untouched);
            request.host_words = staged;
            request.host_capacity = emitted.word_count - 1u;
            CHECK(hl_x86_a64_emit(&request, &untouched) == HL_X86_A64_CAPACITY);
            CHECK(staged[0] == UINT32_C(0xa5a5a5a5));
            CHECK(untouched.abi == UINT32_C(0xa5a5a5a5));
        }
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        memcpy(storage, &low, 8);
        memcpy(storage + 8, &high, 8);
        cpu.registers[0] = UINT64_C(0xaaaaaaaa76543210);
        cpu.registers[1] = low;
        cpu.registers[9] = low;
        cpu.registers[3] = UINT64_C(0x2000);
        cpu.vectors[16] = low;
        cpu.vectors[17] = high;
        cpu.vectors[18] = high;
        cpu.vectors[19] = low;
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x2020);
        cpu.memory_delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x2000);
        cpu.memory_permissions = index == 1u || index == 10u ? 2u : 3u;
        cpu.dirty_first = UINT64_MAX;
        hl_native_x86_64_enter(&cpu, code);
        CHECK(cpu.reason == 0 && cpu.fault_access == 0);
        if (index == 0u)
            CHECK(cpu.vectors[18] == low && cpu.vectors[19] == high);
        else if (index == 1u) {
            CHECK(memcmp(storage, &cpu.vectors[18], 16) == 0 && cpu.memory_written == 1u);
            CHECK(cpu.dirty_view_first == UINT64_C(0x2000) &&
                  cpu.dirty_view_last == UINT64_C(0x2020));
            CHECK(cpu.dirty_first == UINT64_C(0x2000) && cpu.dirty_last == UINT64_C(0x2010));
        }
        else if (index == 2u)
            CHECK(cpu.vectors[16] == high && cpu.vectors[17] == low);
        else if (index == 3u)
            CHECK(cpu.vectors[16] == UINT64_C(0x76543210) && cpu.vectors[17] == 0);
        else if (index == 4u)
            CHECK(cpu.vectors[16] == low && cpu.vectors[17] == 0);
        else if (index == 5u)
            CHECK(cpu.registers[0] == (uint32_t)low);
        else if (index == 6u)
            CHECK(cpu.registers[9] == low);
        else if (index == 7u)
            CHECK(cpu.vectors[16] == high && cpu.vectors[17] == 0);
        else if (index == 8u)
            CHECK(cpu.vectors[16] == low && cpu.vectors[17] == 0);
        else if (index == 9u)
            CHECK(cpu.vectors[18] == low && cpu.vectors[19] == 0);
        else {
            CHECK(memcmp(storage, &cpu.vectors[16], 8) == 0);
            CHECK(cpu.memory_written == 1u);
            CHECK(cpu.dirty_first == UINT64_C(0x2000) && cpu.dirty_last == UINT64_C(0x2008));
        }
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    {
        uint32_t host[256] = {0};
        hl_x86_a64_provenance provenance[8] = {0};
        hl_x86_a64_request request = request_for(low_memory, sizeof low_memory, host, provenance);
        hl_x86_a64_result emitted;
        hl_native_x86_64_cpu cpu = {0};

        request.host_capacity = sizeof host / sizeof host[0];
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[3] = UINT64_C(0x2004);
        cpu.vectors[16] = high;
        cpu.vectors[17] = high;
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x2008);
        cpu.memory_delta = (uint64_t)(uintptr_t)storage - UINT64_C(0x2000);
        cpu.memory_permissions = 1u;
        hl_native_x86_64_enter(&cpu, code);
        CHECK(cpu.vectors[16] == high && cpu.vectors[17] == high);
        CHECK(cpu.fault_address == UINT64_C(0x2004));
        CHECK(cpu.fault_access == 1 && cpu.fault_size == 8);
        CHECK(cpu.program == UINT64_C(0x400000) && cpu.reason == HL_NATIVE_EXIT_FALLBACK);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int accumulator_sign(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const uint8_t bytes[][3] = {
        {0x66, 0x98, 0}, {0x98, 0, 0}, {0x48, 0x98},
        {0x66, 0x99, 0}, {0x99, 0, 0}, {0x48, 0x99},
    };
    static const size_t sizes[] = {2, 1, 2, 2, 1, 2};
    static const uint64_t positive[] = {0x7f, 0x7fff, UINT64_C(0x7fffffff)};
    static const uint64_t negative[] = {0x80, 0x8000, UINT64_C(0x80000000)};
    static const uint64_t dividend_positive[] = {
        0x7fff, UINT64_C(0x7fffffff), UINT64_C(0x7fffffffffffffff),
    };
    static const uint64_t dividend_negative[] = {
        0x8000, UINT64_C(0x80000000), UINT64_C(0x8000000000000000),
    };
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned operation;
    unsigned sign;

    CHECK(code != MAP_FAILED);
    for (operation = 0; operation < 6u; ++operation) {
        for (sign = 0; sign < 2u; ++sign) {
            uint32_t host[256] = {0};
            hl_x86_a64_provenance provenance[8] = {0};
            hl_x86_a64_request request = request_for(bytes[operation], sizes[operation], host, provenance);
            hl_x86_a64_result emitted;
            hl_native_x86_64_cpu cpu = {0};
            unsigned width_index = operation % 3u;
            uint64_t source = operation < 3u ?
                                  (sign == 0u ? positive[width_index] : negative[width_index]) :
                                  (sign == 0u ? dividend_positive[width_index] :
                                                dividend_negative[width_index]);
            uint64_t base = width_index == 2u ?
                                (operation < 3u ? UINT64_C(0x55aa112200000000) : 0u) :
                            width_index == 1u ? UINT64_C(0x55aa112200000000) :
                                                UINT64_C(0x55aa112233440000);
            uint64_t initial_rax = base | source;
            uint64_t initial_rdx = UINT64_C(0xa5a55a5ac3c30000);
            uint64_t expected;

            request.host_capacity = sizeof host / sizeof host[0];
            CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
            CHECK(emitted.instruction_count == 1 && provenance[0].guest_size == sizes[operation]);
            if (operation == 0u && sign == 0u) {
                uint32_t staged[128];
                hl_x86_a64_result untouched;

                memset(staged, 0xa5, sizeof staged);
                memset(&untouched, 0xa5, sizeof untouched);
                request.host_words = staged;
                request.host_capacity = emitted.word_count - 1u;
                CHECK(hl_x86_a64_emit(&request, &untouched) == HL_X86_A64_CAPACITY);
                CHECK(staged[0] == UINT32_C(0xa5a5a5a5));
                CHECK(untouched.abi == UINT32_C(0xa5a5a5a5));
            }
            memcpy(code, host, emitted.word_count * sizeof(uint32_t));
            ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
            cpu.registers[0] = initial_rax;
            cpu.registers[2] = initial_rdx;
            cpu.flags = UINT64_C(0xad7);
            hl_native_x86_64_enter(&cpu, code);
            if (operation < 3u) {
                expected = width_index == 0u ?
                               (initial_rax & ~UINT64_C(0xffff)) |
                                   (uint16_t)(int16_t)(int8_t)(uint8_t)source :
                           width_index == 1u ? (uint32_t)(int32_t)(int16_t)(uint16_t)source :
                                               (uint64_t)(int64_t)(int32_t)(uint32_t)source;
                CHECK(cpu.registers[0] == expected && cpu.registers[2] == initial_rdx);
            } else {
                uint64_t mask = sign == 0u ? 0u : UINT64_MAX;

                expected = width_index == 0u ?
                               (initial_rdx & ~UINT64_C(0xffff)) | (mask & UINT64_C(0xffff)) :
                           width_index == 1u ? (uint32_t)mask : mask;
                CHECK(cpu.registers[0] == initial_rax && cpu.registers[2] == expected);
            }
            CHECK(cpu.flags == UINT64_C(0xad7) && cpu.reason == 0 && cpu.fault_access == 0);
            CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);
    return 0;
#endif
}

static int lse_gate_contract(void) {
    const uint8_t guest[] = {0x48, 0x87, 0x10};
    uint32_t host[256];
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_result result;
    hl_x86_a64_request request;

    memset(host, 0xa5, sizeof host);
    request = request_for(guest, sizeof guest, host, provenance);
    request.flags &= ~HL_X86_A64_LSE;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_UNSUPPORTED);
    request.flags |= HL_X86_A64_LSE;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1 && result.word_count != 0);
    return 0;
}

static int memory_xchg_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const uint8_t guest[][4] = {{0x86,0x10},{0x66,0x87,0x10},{0x87,0x10},{0x48,0x87,0x10}};
    static const uint8_t size[] = {2,3,2,3}, width[] = {1,2,4,8};
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned i;
    CHECK(code != MAP_FAILED);
    for (i = 0; i < 4; ++i) {
        uint64_t memory = UINT64_C(0x8877665544332211), initial = memory;
        uint64_t replacement = UINT64_C(0xaabbccddeeff0099);
        uint64_t mask = width[i] == 8 ? UINT64_MAX : (UINT64_C(1) << (8u * width[i])) - 1u;
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[8] = {0}; hl_x86_a64_result emitted;
        hl_x86_a64_request request = request_for(guest[i], size[i], host, provenance);
        hl_native_x86_64_cpu cpu = {0}; request.host_capacity = 256;
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        CHECK(emitted.instruction_count == 1 && provenance[0].guest_size == size[i]);
        memcpy(code, host, emitted.word_count * 4u); ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.registers[0] = UINT64_C(0x2000); cpu.registers[2] = replacement;
        cpu.memory_first = UINT64_C(0x2000); cpu.memory_last = UINT64_C(0x2008);
        cpu.memory_delta = (uint64_t)(uintptr_t)&memory - UINT64_C(0x2000);
        cpu.memory_permissions = 7; cpu.dirty_first = UINT64_MAX; hl_x86_test_enter(&cpu, code);
        CHECK(memory == ((initial & ~mask) | (replacement & mask)));
        CHECK(cpu.registers[2] == (width[i] == 4 ? (uint32_t)initial : ((replacement & ~mask) | (initial & mask))));
        CHECK(cpu.memory_written == 1 && cpu.dirty_first == UINT64_C(0x2000));
        CHECK(cpu.dirty_last == UINT64_C(0x2000) + width[i]);
        CHECK(cpu.dirty_view_first == UINT64_C(0x2000) && cpu.dirty_view_last == UINT64_C(0x2008));
        CHECK((cpu.executable_written & 4u) != 0);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    }
    {
        const uint8_t high[] = {0x86,0x20}; uint64_t memory = 0x44;
        uint32_t host[256] = {0}; hl_x86_a64_provenance provenance[8] = {0}; hl_x86_a64_result emitted;
        hl_x86_a64_request request = request_for(high, sizeof high, host, provenance); hl_native_x86_64_cpu cpu = {0};
        request.host_capacity = 256; CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
        memcpy(code,host,emitted.word_count*4u); ((uint32_t*)code)[emitted.word_count]=UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char*)code,(char*)code+(emitted.word_count+1u)*4u); CHECK(mprotect(code,page,PROT_READ|PROT_EXEC)==0);
        cpu.registers[0]=UINT64_C(0x112233445566aa00); cpu.memory_first=cpu.registers[0]; cpu.memory_last=cpu.memory_first+1;
        cpu.memory_delta=(uint64_t)(uintptr_t)&memory-cpu.memory_first; cpu.memory_permissions=3; cpu.dirty_first=UINT64_MAX;
        hl_x86_test_enter(&cpu,code); CHECK(memory==0xaa && cpu.registers[0]==UINT64_C(0x1122334455664400));
        CHECK(mprotect(code,page,PROT_READ|PROT_WRITE)==0);
    }
    {
        const uint8_t wide[] = {0x48,0x87,0x10}; uint64_t memory=UINT64_C(0x8877665544332211), initial=memory;
        uint32_t host[256]={0}; hl_x86_a64_provenance provenance[8]={0}; hl_x86_a64_result emitted;
        hl_x86_a64_request request=request_for(wide,sizeof wide,host,provenance); hl_native_x86_64_cpu cpu={0};
        request.host_capacity=256; CHECK(hl_x86_a64_emit(&request,&emitted)==HL_X86_A64_OK);
        memcpy(code,host,emitted.word_count*4u); ((uint32_t*)code)[emitted.word_count]=UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char*)code,(char*)code+(emitted.word_count+1u)*4u); CHECK(mprotect(code,page,PROT_READ|PROT_EXEC)==0);
        cpu.registers[0]=0x2000; cpu.registers[2]=0x1234; cpu.memory_first=0x2000; cpu.memory_last=0x2008;
        cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2000; cpu.memory_permissions=1; cpu.dirty_first=UINT64_MAX;
        hl_x86_test_enter(&cpu,code); CHECK(cpu.reason==HL_NATIVE_EXIT_FALLBACK && cpu.fault_access==3);
        CHECK(memory==initial && cpu.registers[2]==0x1234 && cpu.memory_written==0 && cpu.dirty_first==UINT64_MAX);
        memset(&cpu,0,sizeof cpu); cpu.registers[0]=0x2001; cpu.registers[2]=0x5678; cpu.memory_first=0x2000; cpu.memory_last=0x2009;
        cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2001; cpu.memory_permissions=3; cpu.dirty_first=UINT64_MAX;
        hl_x86_test_enter(&cpu,code); CHECK(cpu.reason==HL_NATIVE_EXIT_FALLBACK && cpu.fault_access==0);
        CHECK(memory==initial && cpu.registers[2]==0x5678 && cpu.memory_written==0 && cpu.dirty_first==UINT64_MAX);
        CHECK(mprotect(code,page,PROT_READ|PROT_WRITE)==0);
    }
    CHECK(munmap(code,(size_t)page)==0); return 0;
#endif
}

static int memory_cmpxchg_differential(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint8_t guest[] = {0x48,0x0f,0xb1,0x13}; /* cmpxchgq %rdx,(%rbx) */
    long page=sysconf(_SC_PAGESIZE); uint8_t *code=mmap(NULL,(size_t)page,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);
    uint32_t host[256]={0}; hl_x86_a64_provenance provenance[8]={0}; hl_x86_a64_result emitted;
    hl_x86_a64_request request=request_for(guest,sizeof guest,host,provenance); uint64_t memory=2;
    hl_native_x86_64_cpu cpu={0}; CHECK(code!=MAP_FAILED); CHECK(hl_x86_a64_emit(&request,&emitted)==HL_X86_A64_OK);
    memcpy(code,host,emitted.word_count*4u); ((uint32_t*)code)[emitted.word_count]=UINT32_C(0xd65f03c0);
    __builtin___clear_cache((char*)code,(char*)code+(emitted.word_count+1u)*4u); CHECK(mprotect(code,page,PROT_READ|PROT_EXEC)==0);
    cpu.registers[0]=2; cpu.registers[2]=9; cpu.registers[3]=0x2000; cpu.memory_first=0x2000; cpu.memory_last=0x2008;
    cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2000; cpu.memory_permissions=7; cpu.dirty_first=UINT64_MAX;
    hl_x86_test_enter(&cpu,code); CHECK(memory==9 && cpu.registers[0]==2 && cpu.flags==0x44);
    CHECK(cpu.memory_written==1 && cpu.dirty_first==0x2000 && cpu.dirty_last==0x2008 && (cpu.executable_written&4)!=0);
    memset(&cpu,0,sizeof cpu); memory=2; cpu.registers[0]=1; cpu.registers[2]=9; cpu.registers[3]=0x2000;
    cpu.memory_first=0x2000; cpu.memory_last=0x2008; cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2000;
    cpu.memory_permissions=3; cpu.dirty_first=UINT64_MAX; hl_x86_test_enter(&cpu,code);
    CHECK(memory==2 && cpu.registers[0]==2 && cpu.flags==0x95);
    CHECK(cpu.memory_written==0 && cpu.dirty_first==UINT64_MAX && cpu.executable_written==0);
    CHECK(mprotect(code,page,PROT_READ|PROT_WRITE)==0);
    {
        static const uint8_t forms[][4]={{0x0f,0xb0,0x13,0},{0x66,0x0f,0xb1,0x13},{0x0f,0xb1,0x13,0}};
        static const uint8_t lengths[]={3,4,3}, widths[]={1,2,4}; unsigned i;
        for(i=0;i<3;i++) {
            uint64_t mask=widths[i]==4?UINT64_C(0xffffffff):(UINT64_C(1)<<(widths[i]*8))-1;
            memset(host,0,sizeof host); request=request_for(forms[i],lengths[i],host,provenance);
            CHECK(hl_x86_a64_emit(&request,&emitted)==HL_X86_A64_OK);
            memcpy(code,host,emitted.word_count*4u); ((uint32_t*)code)[emitted.word_count]=UINT32_C(0xd65f03c0);
            __builtin___clear_cache((char*)code,(char*)code+(emitted.word_count+1u)*4u); CHECK(mprotect(code,page,PROT_READ|PROT_EXEC)==0);
            memory=2; memset(&cpu,0,sizeof cpu); cpu.registers[0]=UINT64_C(0xfeedface00000002);
            cpu.registers[2]=9; cpu.registers[3]=0x2000; cpu.memory_first=0x2000; cpu.memory_last=0x2008;
            cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2000; cpu.memory_permissions=3; cpu.dirty_first=UINT64_MAX;
            hl_x86_test_enter(&cpu,code); CHECK((memory&mask)==9 && cpu.flags==0x44 && cpu.memory_written==1);
            CHECK(cpu.dirty_first==UINT64_C(0x2000) && cpu.dirty_last==UINT64_C(0x2000)+widths[i]);
            memory=2; memset(&cpu,0,sizeof cpu); cpu.registers[0]=UINT64_C(0xfeedface00000001);
            cpu.registers[2]=9; cpu.registers[3]=0x2000; cpu.memory_first=0x2000; cpu.memory_last=0x2008;
            cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2000; cpu.memory_permissions=3; cpu.dirty_first=UINT64_MAX;
            hl_x86_test_enter(&cpu,code); CHECK(memory==2 && cpu.flags==0x95 && cpu.memory_written==0);
            CHECK(cpu.dirty_first==UINT64_MAX);
            if(widths[i]==4) CHECK(cpu.registers[0]==2); else CHECK((cpu.registers[0]&mask)==2);
            CHECK(mprotect(code,page,PROT_READ|PROT_WRITE)==0);
        }
    }
    { /* AH replacement and AL accumulator are independently addressed. */
        const uint8_t high[]={0x0f,0xb0,0x23}; memset(host,0,sizeof host);
        request=request_for(high,sizeof high,host,provenance); CHECK(hl_x86_a64_emit(&request,&emitted)==HL_X86_A64_OK);
        memcpy(code,host,emitted.word_count*4u); ((uint32_t*)code)[emitted.word_count]=UINT32_C(0xd65f03c0);
        __builtin___clear_cache((char*)code,(char*)code+(emitted.word_count+1u)*4u); CHECK(mprotect(code,page,PROT_READ|PROT_EXEC)==0);
        memory=0x44; memset(&cpu,0,sizeof cpu); cpu.registers[0]=UINT64_C(0x112233445566aa44); cpu.registers[3]=0x2000;
        cpu.memory_first=0x2000; cpu.memory_last=0x2001; cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2000;
        cpu.memory_permissions=3; cpu.dirty_first=UINT64_MAX; hl_x86_test_enter(&cpu,code);
        CHECK((memory&0xff)==0xaa && cpu.registers[0]==UINT64_C(0x112233445566aa44) && cpu.flags==0x44);
        CHECK(mprotect(code,page,PROT_READ|PROT_WRITE)==0);
    }
    { /* Fault and alignment exits precede CAS and architectural register mutation. */
        memcpy(code,host,emitted.word_count*4u); CHECK(mprotect(code,page,PROT_READ|PROT_EXEC)==0);
        memory=0x44; memset(&cpu,0,sizeof cpu); cpu.registers[0]=UINT64_C(0x112233445566aa44); cpu.registers[3]=0x2000;
        cpu.memory_first=0x2000; cpu.memory_last=0x2001; cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2000;
        cpu.memory_permissions=1; cpu.dirty_first=UINT64_MAX; hl_x86_test_enter(&cpu,code);
        CHECK(memory==0x44 && cpu.registers[0]==UINT64_C(0x112233445566aa44) && cpu.memory_written==0);
        CHECK(cpu.fault_access==3 && cpu.dirty_first==UINT64_MAX);
    }
    CHECK(mprotect(code,page,PROT_READ|PROT_WRITE)==0);
    request=request_for(guest,sizeof guest,host,provenance); CHECK(hl_x86_a64_emit(&request,&emitted)==HL_X86_A64_OK);
    memcpy(code,host,emitted.word_count*4u); ((uint32_t*)code)[emitted.word_count]=UINT32_C(0xd65f03c0);
    __builtin___clear_cache((char*)code,(char*)code+(emitted.word_count+1u)*4u); CHECK(mprotect(code,page,PROT_READ|PROT_EXEC)==0);
    memory=2; memset(&cpu,0,sizeof cpu); cpu.registers[0]=2; cpu.registers[2]=9; cpu.registers[3]=0x2001;
    cpu.memory_first=0x2000; cpu.memory_last=0x2009; cpu.memory_delta=(uint64_t)(uintptr_t)&memory-0x2001;
    cpu.memory_permissions=3; cpu.dirty_first=UINT64_MAX; hl_x86_test_enter(&cpu,code);
    CHECK(cpu.reason==HL_NATIVE_EXIT_FALLBACK && cpu.fault_access==0 && memory==2 && cpu.registers[0]==2);
    CHECK(cpu.memory_written==0 && cpu.dirty_first==UINT64_MAX);
    CHECK(munmap(code,(size_t)page)==0); return 0;
#endif
}

static int vex_blend_family_contract(void) {
    static const uint8_t opcodes[] = {0x02u, 0x0cu, 0x0du, 0x0eu, 0x4au, 0x4bu, 0x4cu};
    unsigned operation;
    unsigned wide;
    unsigned memory;
    for (operation = 0u; operation < sizeof opcodes; ++operation) {
        for (wide = 0u; wide < 2u; ++wide) {
            for (memory = 0u; memory < 2u; ++memory) {
                uint8_t guest[] = {0xc4u, 0xe3u, (uint8_t)(0x69u | (wide << 2)),
                                   opcodes[operation], (uint8_t)(memory ? 0x4bu : 0xcbu),
                                   memory ? 0x10u : 0xa0u, 0xa0u};
                size_t size = memory != 0u ? 7u : 6u;
                uint32_t host[256] = {0};
                hl_x86_a64_provenance provenance[2] = {0};
                hl_x86_a64_request request = request_for(guest, size, host, provenance);
                hl_x86_a64_result emitted;
                uint32_t required;
                CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                CHECK(emitted.instruction_count == 1u && emitted.exit == HL_X86_A64_FALLTHROUGH);
                CHECK(provenance[0].guest_size == size && provenance[0].word_end > provenance[0].word_start);
                if (memory <= 1u) {
                    for (required = 0u; required < 256u; ++required) {
                        request.host_capacity = required;
                        if (hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK) break;
                    }
                    CHECK(required < 256u);
                    memset(host, 0xa5, sizeof host); memset(provenance, 0xa5, sizeof provenance);
                    request.host_capacity = required - 1u;
                    CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_CAPACITY);
                    CHECK(host[0] == UINT32_C(0xa5a5a5a5));
                    CHECK(provenance[0].guest_pc == UINT64_C(0xa5a5a5a5a5a5a5a5));
#if defined(__aarch64__)
                    {
                        long page = sysconf(_SC_PAGESIZE);
                        uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
                        hl_native_x86_64_cpu cpu = {0};
                        hl_native_x86_64_cpu initial;
                        uint8_t left[32], right[32], mask[32], expected[32];
                        unsigned byte;
                        unsigned lane = opcodes[operation] == 0x0eu ? 2u :
                                        (opcodes[operation] == 0x0du || opcodes[operation] == 0x4bu) ? 8u :
                                        opcodes[operation] == 0x4cu ? 1u : 4u;
                        CHECK(code != MAP_FAILED);
                        for (byte = 0u; byte < 32u; ++byte) {
                            left[byte] = (uint8_t)(0x10u + byte);
                            right[byte] = (uint8_t)(0xe0u - byte);
                            mask[byte] = (uint8_t)(((byte / lane) & 1u) != 0u ? 0x80u : 0x7fu);
                        }
                        for (byte = 0u; byte < (wide != 0u ? 32u : 16u); byte += lane) {
                            unsigned element = byte / lane;
                            unsigned selected;
                            if (opcodes[operation] >= 0x4au)
                                selected = (mask[byte + lane - 1u] & 0x80u) != 0u;
                            else {
                                unsigned bit = opcodes[operation] == 0x0eu ? element & 7u : element;
                                selected = (0xa0u >> bit) & 1u;
                            }
                            memcpy(expected + byte, selected != 0u ? right + byte : left + byte, lane);
                        }
                        memcpy(&cpu.vectors[4], left, 16u); memcpy(&cpu.vector_upper[4], left + 16u, 16u);
                        memcpy(&cpu.vectors[6], right, 16u); memcpy(&cpu.vector_upper[6], right + 16u, 16u);
                        memcpy(&cpu.vectors[20], mask, 16u); memcpy(&cpu.vector_upper[20], mask + 16u, 16u);
                        memset(&cpu.vector_upper[2], 0x5au, 16u);
                        if (memory != 0u) {
                            cpu.registers[3] = UINT64_C(0x3ff0);
                            cpu.memory_first = UINT64_C(0x4000);
                            cpu.memory_last = wide != 0u ? UINT64_C(0x4020) : UINT64_C(0x4010);
                            cpu.memory_delta = (uint64_t)(uintptr_t)right - UINT64_C(0x4000);
                            cpu.memory_permissions = 1u;
                        }
                        cpu.flags = UINT64_C(0xad7); cpu.mxcsr = UINT32_C(0x5f80);
                        initial = cpu;
                        request.host_capacity = sizeof host / sizeof host[0];
                        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_OK);
                        memcpy(code, host, emitted.word_count * sizeof(uint32_t));
                        ((uint32_t *)code)[emitted.word_count] = UINT32_C(0xd65f03c0);
                        __builtin___clear_cache((char *)code, (char *)code + (emitted.word_count + 1u) * 4u);
                        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
                        hl_native_x86_64_enter(&cpu, code);
                        if (memcmp(&cpu.vectors[2], expected, 16u) != 0) {
                            fprintf(stderr, "blend semantic op=%02x wide=%u mem=%u reason=%llu fault=%llx got=%02x exp=%02x\n", opcodes[operation], wide, memory, (unsigned long long)cpu.reason, (unsigned long long)cpu.fault_address, ((uint8_t *)&cpu.vectors[2])[0], expected[0]);
                            return 1;
                        }
                        if (wide != 0u) CHECK(memcmp(&cpu.vector_upper[2], expected + 16u, 16u) == 0);
                        else { uint8_t zero[16] = {0}; CHECK(memcmp(&cpu.vector_upper[2], zero, 16u) == 0); }
                        CHECK(memcmp(&cpu.vectors[4], left, 16u) == 0 && memcmp(&cpu.vectors[6], right, 16u) == 0);
                        CHECK(cpu.flags == UINT64_C(0xad7) && cpu.mxcsr == UINT32_C(0x5f80));
                        if (memory != 0u) {
                            hl_native_x86_64_cpu fault = initial;
                            fault.memory_last--;
                            hl_native_x86_64_enter(&fault, code);
                            CHECK(fault.reason == 3u && fault.fault_address == UINT64_C(0x4000));
                            CHECK(memcmp(&fault.vectors[2], &initial.vectors[2], 16u) == 0);
                            CHECK(memcmp(&fault.vector_upper[2], &initial.vector_upper[2], 16u) == 0);
                        }
                        CHECK(munmap(code, (size_t)page) == 0);
                    }
#endif
                }
            }
        }
    }
    { /* W1 is outside the retained W0 family; a missing immediate is truncated. */
        static const uint8_t invalid[] = {0xc4u, 0xe3u, 0xe9u, 0x0eu, 0xcbu, 0xffu};
        static const uint8_t truncated[] = {0xc4u, 0xe3u, 0x69u, 0x4cu, 0xcbu};
        uint32_t host[64] = {0}; hl_x86_a64_provenance provenance[2] = {0}; hl_x86_a64_result emitted;
        hl_x86_a64_request request = request_for(invalid, sizeof invalid, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_UNSUPPORTED);
        request = request_for(truncated, sizeof truncated, host, provenance);
        CHECK(hl_x86_a64_emit(&request, &emitted) == HL_X86_A64_TRUNCATED);
    }
    return 0;
}

int main(void) {
    int status = incdec_differential();
    if (status != 0) return status;
    status = lse_gate_contract();
    if (status != 0) return status;
    status = memory_xchg_differential();
    if (status != 0) return status;
    status = memory_cmpxchg_differential();
    if (status != 0) return status;
#if defined(__aarch64__)
    status = executable_store_stops_self_loop();
    if (status != 0) return status;
    status = vector_arithmetic_self_loop();
    if (status != 0) return status;
    status = executable_self_loop_preserves_indirect_target();
    if (status != 0) return status;
#endif
    status = rmw_projection_contract();
    if (status != 0) return status;
    status = read_cache_contract();
    if (status != 0) return status;
    status = write_cache_contract();
    if (status != 0) return status;
    status = dirty_journal_contract();
    if (status != 0) return status;
    status = cumulative_checkpoint_contract();
    if (status != 0) return status;
#if defined(HL_X86_INCDEC_TEST_ONLY)
    return 0;
#endif
    status = accumulator_sign();
    if (status != 0) return status;
    status = vector_projection();
    if (status != 0) return status;
    status = vector_256_memory_owner();
    if (status != 0) return status;
    status = guarded_load_store_handoff();
    if (status != 0) return status;
    status = vector_host_preservation();
    if (status != 0) return status;
    status = vector_opcode();
    if (status != 0) return status;
    status = straight_line();
    if (status != 0) return status;
    status = scalar_double_family();
    if (status != 0) return status;
    status = scalar_double_differential();
    if (status != 0) return status;
    status = floating_arithmetic();
    if (status != 0) return status;
    status = packed_integer_arithmetic();
    if (status != 0) return status;
    status = packed_integer_multiply();
    if (status != 0) return status;
    status = pcmpistri_equal_each();
    if (status != 0) return status;
    status = pcmpistri_preserves_budget();
    if (status != 0) return status;
    status = end_branch();
    if (status != 0) return status;
    status = typed_exits();
    if (status != 0) return status;
    status = bounded_output();
    if (status != 0) return status;
    status = register_moves();
    if (status != 0) return status;
    status = immediate_widths();
    if (status != 0) return status;
    status = exact_fallback();
    if (status != 0) return status;
    status = memory_load_contract();
    if (status != 0) return status;
    status = memory_load_differential();
    if (status != 0) return status;
    status = prefix_order();
    if (status != 0) return status;
    status = decode_bounds();
    if (status != 0) return status;
    status = conditional_control();
    if (status != 0) return status;
    status = flags_contract();
    if (status != 0) return status;
    status = condition_frontend_contract();
    if (status != 0) return status;
    status = cmov_contract();
    if (status != 0) return status;
    status = cmov_differential();
    if (status != 0) return status;
    status = flags_abi_fallback();
    if (status != 0) return status;
    status = register_alu();
    if (status != 0) return status;
    status = mul_differential();
    if (status != 0) return status;
    status = alu_differential();
    if (status != 0) return status;
    status = byte_flags_differential();
    if (status != 0) return status;
    status = byte_alu_frontend_contract();
    if (status != 0) return status;
    status = byte_alu_memory_differential();
    if (status != 0) return status;
    status = vector_immediate_staging_contract();
    if (status != 0) return status;
    status = shuffle_family_contract();
    if (status != 0) return status;
    status = vex_shuffle_family_contract();
    if (status != 0) return status;
    status = vex_signed_dword_to_float_contract();
    if (status != 0) return status;
    status = vex_packed_compare_contract();
    if (status != 0) return status;
    status = vex_packed_sign_absolute_contract();
    if (status != 0) return status;
    status = vex_packed_extrema_contract();
    if (status != 0) return status;
    status = vex_map2_multiply_contract();
    if (status != 0) return status;
    status = vex_horizontal_add_sub_contract();
    if (status != 0) return status;
    status = vex_packed_unpack_contract();
    if (status != 0) return status;
    status = vex_packed_add_subtract_contract();
    if (status != 0) return status;
    status = vex_packed_average_sad_contract();
    if (status != 0) return status;
    status = vex_packed_saturating_contract();
    if (status != 0) return status;
    status = vex_blend_family_contract();
    if (status != 0) return status;
    status = immediate_differential();
    if (status != 0) return status;
    status = accumulator_immediate_differential();
    if (status != 0) return status;
    status = long_accumulator_immediate_chain();
    if (status != 0) return status;
    status = immediate_contract();
    if (status != 0) return status;
    status = immediate_memory_differential();
    if (status != 0) return status;
    status = test_differential();
    if (status != 0) return status;
    status = group_test_memory_differential();
    if (status != 0) return status;
    status = test_contract();
    if (status != 0) return status;
    status = group3_contract();
    if (status != 0) return status;
    status = rotate_differential();
    if (status != 0) return status;
    status = shift_differential();
    if (status != 0) return status;
    status = shift_cl_differential();
    if (status != 0) return status;
    status = shift_contract();
    if (status != 0) return status;
    status = address_differential();
    if (status != 0) return status;
    status = address_contract();
    if (status != 0) return status;
    status = control_family();
    if (status != 0) return status;
    status = indirect_control_differential();
    if (status != 0) return status;
    status = leave_contract();
    if (status != 0) return status;
    status = register_extensions();
    if (status != 0) return status;
    status = extension_fallback();
    if (status != 0) return status;
    status = memory_extension_differential();
    if (status != 0) return status;
    status = imul_differential();
    if (status != 0) return status;
    status = multibyte_nop_contract();
    if (status != 0) return status;
    return 0;
}
