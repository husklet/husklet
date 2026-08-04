#include "../src/arch/x86_64/frontend.h"
#include "../include/cpu.h"
#include "../include/executor.h"

#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#if defined(__aarch64__)
extern void hl_x86_test_enter(hl_native_x86_64_cpu *, void *);
#endif

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "packed_shift:%d: %s\n", __LINE__, #x); return __LINE__; } } while (0)

static hl_x86_a64_status translate(const uint8_t *guest, size_t size,
                                   uint32_t *host, hl_x86_a64_result *result) {
    hl_x86_a64_provenance provenance[8] = {0};
    hl_x86_a64_request request = {0};
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = UINT64_C(0x400000);
    request.guest_bytes = guest;
    request.guest_size = size;
    request.max_instructions = 1u;
    request.host_words = host;
    request.host_capacity = 2048u;
    request.provenance = provenance;
    request.provenance_capacity = 8u;
    request.flags = HL_X86_A64_LSE;
    return hl_x86_a64_emit(&request, result);
}

static int decode_family(void) {
    static const uint8_t immediate_ops[] = {0x71, 0x72, 0x73};
    static const uint8_t subops[] = {2, 4, 6};
    static const uint8_t variable_ops[] = {0xd1, 0xd2, 0xd3, 0xe1, 0xe2, 0xf1, 0xf2, 0xf3};
    uint32_t host[2048];
    hl_x86_a64_result result;
    for (size_t op = 0; op < sizeof immediate_ops; ++op) {
        for (size_t sub = 0; sub < sizeof subops; ++sub) {
            uint8_t code[] = {0x66, 0x0f, immediate_ops[op],
                              (uint8_t)(0xc0u | subops[sub] << 3), 0xff};
            int valid = !(immediate_ops[op] == 0x73 && subops[sub] == 4);
            CHECK((translate(code, sizeof code, host, &result) == HL_X86_A64_OK) == valid);
        }
    }
    for (unsigned sub = 3; sub <= 7; sub += 4) {
        uint8_t code[] = {0x66, 0x0f, 0x73, (uint8_t)(0xc0u | sub << 3), 16};
        CHECK(translate(code, sizeof code, host, &result) == HL_X86_A64_OK);
    }
    for (size_t op = 0; op < sizeof variable_ops; ++op) {
        uint8_t reg[] = {0x66, 0x0f, variable_ops[op], 0xc1};
        uint8_t mem[] = {0x66, 0x44, 0x0f, variable_ops[op], 0x44, 0x88, 0x10};
        CHECK(translate(reg, sizeof reg, host, &result) == HL_X86_A64_OK);
        CHECK(translate(mem, sizeof mem, host, &result) == HL_X86_A64_OK);
    }
    {
        const uint8_t bad_memory_immediate[] = {0x66, 0x0f, 0x71, 0x10, 1};
        const uint8_t missing_immediate[] = {0x66, 0x0f, 0x72, 0xd0};
        CHECK(translate(bad_memory_immediate, sizeof bad_memory_immediate, host, &result) == HL_X86_A64_UNSUPPORTED);
        CHECK(translate(missing_immediate, sizeof missing_immediate, host, &result) == HL_X86_A64_TRUNCATED);
    }
    return 0;
}

static int execute_semantics(void) {
#if defined(__aarch64__)
    static const uint8_t forms[][5] = {
        {0x66, 0x0f, 0x71, 0xd0, 16}, /* psrlw xmm0,16 -> zero */
        {0x66, 0x0f, 0x72, 0xe0, 40}, /* psrad xmm0,40 -> sign fill */
        {0x66, 0x0f, 0x73, 0xf0, 8},  /* psllq xmm0,8 */
        {0x66, 0x0f, 0x73, 0xd8, 3},  /* psrldq xmm0,3 */
        {0x66, 0x0f, 0x73, 0xf8, 16}, /* pslldq xmm0,16 -> zero */
    };
    static const uint64_t expected[][2] = {
        {0, 0},
        {0, UINT64_MAX},
        {UINT64_C(0x2233445566778800), UINT64_C(0xaabbccddeeff0000)},
        {UINT64_C(0xeeff001122334455), UINT64_C(0x00000099aabbccdd)},
        {0, 0},
    };
    long page = sysconf(_SC_PAGESIZE);
    void *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    for (size_t form = 0; form < sizeof forms / sizeof forms[0]; ++form) {
        uint32_t host[2048] = {0};
        hl_x86_a64_result result;
        hl_native_x86_64_cpu cpu = {0};
        CHECK(translate(forms[form], sizeof forms[form], host, &result) == HL_X86_A64_OK);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        ((uint32_t *)code)[0] = UINT32_C(0x3dc00000) |
                                ((uint32_t)(offsetof(hl_native_x86_64_cpu, vectors) / 16u) << 10) |
                                (28u << 5); /* ldr q0,[x28,#vectors] */
        memcpy((uint32_t *)code + 1u, host, result.word_count * sizeof host[0]);
        ((uint32_t *)code)[result.word_count + 1u] = UINT32_C(0x3d800000) |
                                                       ((uint32_t)(offsetof(hl_native_x86_64_cpu, vectors) / 16u) << 10) |
                                                       (28u << 5); /* str q0,[x28,#vectors] */
        ((uint32_t *)code)[result.word_count + 2u] = UINT32_C(0xd65f03c0);
        __builtin___clear_cache(code, (char *)code + (result.word_count + 3u) * sizeof host[0]);
        CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
        cpu.vectors[0] = UINT64_C(0x1122334455667788);
        cpu.vectors[1] = UINT64_C(0x99aabbccddeeff00);
        cpu.flags = UINT64_C(0xad7);
        hl_x86_test_enter(&cpu, code);
        if (cpu.vectors[0] != expected[form][0] || cpu.vectors[1] != expected[form][1]) {
            fprintf(stderr, "packed_shift form %zu: %016llx %016llx\n", form,
                    (unsigned long long)cpu.vectors[0], (unsigned long long)cpu.vectors[1]);
            return __LINE__;
        }
        CHECK(cpu.flags == UINT64_C(0xad7));
    }
    CHECK(munmap(code, (size_t)page) == 0);
#endif
    return 0;
}

int main(void) {
    int result = decode_family();
    return result != 0 ? result : execute_semantics();
}
