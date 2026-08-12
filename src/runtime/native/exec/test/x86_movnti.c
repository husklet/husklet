#include "../src/arch/x86_64/frontend.h"
#include "../include/cpu.h"
#include "../include/executor.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(expression) do { if (!(expression)) { \
    fprintf(stderr, "x86_movnti:%d: %s\n", __LINE__, #expression); return __LINE__; \
} } while (0)

#if defined(__aarch64__)
extern void hl_x86_test_enter(hl_native_x86_64_cpu *, void *);
#endif

static hl_x86_a64_status emit(const uint8_t *guest, size_t size, uint32_t *host,
                              hl_x86_a64_result *result) {
    hl_x86_a64_provenance provenance[2] = {0};
    hl_x86_a64_request request;
    memset(&request, 0, sizeof request);
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = UINT64_C(0x400000);
    request.guest_bytes = guest;
    request.guest_size = size;
    request.max_instructions = 1u;
    request.host_words = host;
    request.host_capacity = 512u;
    request.provenance = provenance;
    request.provenance_capacity = 2u;
    request.flags = HL_X86_A64_LSE;
    return hl_x86_a64_emit(&request, result);
}

static int decode_contract(void) {
    static const uint8_t dword[] = {0x0fu, 0xc3u, 0x18u};
    static const uint8_t word[] = {0x66u, 0x0fu, 0xc3u, 0x5cu, 0x24u, 0x08u};
    static const uint8_t qword[] = {0x4du, 0x0fu, 0xc3u, 0x48u, 0x10u};
    static const uint8_t reg[] = {0x0fu, 0xc3u, 0xd8u};
    static const uint8_t lock[] = {0xf0u, 0x0fu, 0xc3u, 0x18u};
    static const uint8_t rep[] = {0xf3u, 0x0fu, 0xc3u, 0x18u};
    static const uint8_t repne[] = {0xf2u, 0x0fu, 0xc3u, 0x18u};
    static const uint8_t address32[] = {0x67u, 0x0fu, 0xc3u, 0x18u};
    static const uint8_t rip_relative[] = {0x0fu, 0xc3u, 0x1du, 0u, 0u, 0u, 0u};
    static const uint8_t fs_relative[] = {0x64u, 0x0fu, 0xc3u, 0x18u};
    static const uint8_t gs_relative[] = {0x65u, 0x0fu, 0xc3u, 0x18u};
    static const uint8_t short_form[] = {0x0fu, 0xc3u};
    const uint8_t *valid[] = {dword, word, qword, rep, repne, address32,
                              rip_relative, fs_relative, gs_relative};
    const size_t sizes[] = {sizeof dword, sizeof word, sizeof qword, sizeof rep,
                            sizeof repne, sizeof address32, sizeof rip_relative,
                            sizeof fs_relative, sizeof gs_relative};
    uint32_t host[512];
    hl_x86_a64_result result;
    unsigned index;
    for (index = 0u; index < sizeof valid / sizeof valid[0]; ++index) {
        CHECK(emit(valid[index], sizes[index], host, &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count == 1u);
        CHECK(result.exit_pc == UINT64_C(0x400000) + sizes[index]);
    }
    CHECK(emit(reg, sizeof reg, host, &result) == HL_X86_A64_UNSUPPORTED);
    /* Retained C reaches MOVNTI after recording LOCK but does not consult it.
     * Native keeps the architectural #UD policy at the common prefix gate. */
    CHECK(emit(lock, sizeof lock, host, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(emit(short_form, sizeof short_form, host, &result) == HL_X86_A64_TRUNCATED);
    return 0;
}

static int execution_contract(void) {
#if defined(__aarch64__)
    static const uint8_t qword[] = {0x48u, 0x0fu, 0xc3u, 0x18u};
    static const uint8_t dword[] = {0x0fu, 0xc3u, 0x18u};
    const uint8_t *forms[] = {qword, dword};
    const size_t sizes[] = {sizeof qword, sizeof dword};
    uint32_t host[512] = {0};
    hl_x86_a64_result result;
    long page = sysconf(_SC_PAGESIZE);
    void *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned fault;
    unsigned form;
    CHECK(code != MAP_FAILED);
    for (form = 0u; form < 2u; ++form) {
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    CHECK(emit(forms[form], sizes[form], host, &result) == HL_X86_A64_OK);
    memcpy(code, host, result.word_count * sizeof host[0]);
    ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
    __builtin___clear_cache(code, (char *)code + (result.word_count + 1u) * sizeof host[0]);
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);
    for (fault = 0u; fault < 2u; ++fault) {
        hl_native_x86_64_cpu cpu = {0};
        uint64_t backing = UINT64_C(0xfeedfacecafebeef);
        cpu.registers[0] = UINT64_C(0x2000);
        cpu.registers[3] = UINT64_C(0x1122334455667788);
        cpu.memory_first = UINT64_C(0x2000);
        cpu.memory_last = UINT64_C(0x2000) + (form == 0u ? 8u : 4u);
        cpu.memory_delta = (uint64_t)(uintptr_t)&backing - UINT64_C(0x2000);
        cpu.memory_permissions = fault ? 1u : 7u;
        cpu.dirty_first = UINT64_MAX;
        cpu.flags = UINT64_C(0xad7);
        hl_x86_test_enter(&cpu, code);
        CHECK(cpu.flags == UINT64_C(0xad7));
        CHECK(cpu.registers[3] == UINT64_C(0x1122334455667788));
        if (!fault) {
            CHECK(backing == (form == 0u ? UINT64_C(0x1122334455667788) :
                                           UINT64_C(0xfeedface55667788)));
            CHECK(cpu.memory_written == 1u);
            CHECK(cpu.dirty_first == UINT64_C(0x2000));
            CHECK(cpu.dirty_last == UINT64_C(0x2000) + (form == 0u ? 8u : 4u));
            CHECK((cpu.executable_written & 4u) != 0u);
        } else {
            CHECK(backing == UINT64_C(0xfeedfacecafebeef));
            CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK);
            CHECK(cpu.fault_access == HL_NATIVE_ACCESS_WRITE);
            CHECK(cpu.fault_size == (form == 0u ? 8u : 4u));
            CHECK(cpu.memory_written == 0u && cpu.dirty_first == UINT64_MAX);
        }
    }
    }
    CHECK(munmap(code, (size_t)page) == 0);
#endif
    return 0;
}

int main(void) {
    int status = decode_contract();
    if (status != 0) return status;
    return execution_contract();
}
