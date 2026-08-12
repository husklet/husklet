#include "../cache/cache.h"
#include "../src/arch/aarch64/atomic.h"
#include "../src/arch/aarch64/memory.h"
#include "../src/arch/aarch64/trace.h"

#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "a64-provenance:%d: %s\n", __LINE__, #x); return 1; } } while (0)

typedef struct provenance_case {
    uint32_t word, access, width, sites;
    int64_t displacement[HL_A64_MEMORY_SITE_MAX];
} provenance_case;

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const provenance_case cases[] = {
        {0xb9000462u, HL_NATIVE_ACCESS_WRITE, 4, 1, {0}},
        {0xf81f8ffeu, HL_NATIVE_ACCESS_WRITE, 8, 1, {0}},
        {0xf84087feu, HL_NATIVE_ACCESS_READ, 8, 1, {0}},
        {0xf8627820u, HL_NATIVE_ACCESS_READ, 8, 1, {0}}, /* LSL #3 */
        {0xf8624820u, HL_NATIVE_ACCESS_READ, 8, 1, {0}}, /* UXTW */
        {0xf862c820u, HL_NATIVE_ACCESS_READ, 8, 1, {0}}, /* SXTW */
        {0x58000000u, HL_NATIVE_ACCESS_READ, 8, 1, {0}},
        {0x3dc00020u, HL_NATIVE_ACCESS_READ, 16, 1, {0}},
        {0xa9bf7bfdu, HL_NATIVE_ACCESS_WRITE, 16, 1, {0}},
        {0x88df7c22u, HL_NATIVE_ACCESS_READ, 4, 1, {0}},
        {0x4c408020u, HL_NATIVE_ACCESS_READ, 32, 1, {0}},
        {0xd50b7421u, HL_NATIVE_ACCESS_WRITE, 16, 4, {0, 16, 32, 48}},
    };
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 16;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    for (size_t current = 0; current < sizeof(cases) / sizeof(cases[0]); ++current) {
        const uint32_t words[] = {cases[current].word, 0xd4000001u};
        const uint64_t guest = UINT64_C(0xa000) + current * 8;
        const hl_a64_source_span span = {guest, (const uint8_t *)words, sizeof(words), 11, 12};
        const hl_a64_source source = {&span, 1, 11, 12};
        hl_a64_trace_result trace;
        uint32_t found = 0;
        CHECK(hl_a64_trace_build(&source, guest, 2, code, capacity, &trace));
        CHECK(trace.provenance_count <= HL_NATIVE_PROVENANCE_MAX);
        for (uint32_t index = 0; index < trace.provenance_count; ++index) {
            const hl_native_provenance *record = &trace.provenance[index];
            CHECK(record->code_offset <= trace.code_size);
            CHECK(record->code_size <= trace.code_size - record->code_offset);
            if (record->access == HL_NATIVE_ACCESS_UNKNOWN) {
                CHECK(record->width == 0 && record->address.kind == HL_NATIVE_ADDRESS_NONE);
                continue;
            }
            CHECK(found < cases[current].sites && record->guest == guest);
            CHECK(record->code_size == sizeof(uint32_t));
            CHECK(record->access == cases[current].access && record->width == cases[current].width);
            CHECK(record->address.kind == HL_NATIVE_ADDRESS_BASE && record->address.bits == 64);
            CHECK(record->address.base == 16 && record->address.index == 0);
            CHECK(record->address.displacement == cases[current].displacement[found]);
            uint64_t registers[32] = {0}, reconstructed = 0;
            registers[16] = UINT64_C(0x12345000);
            CHECK(hl_native_address_reconstruct(&record->address, registers, 32, &reconstructed));
            CHECK(reconstructed == registers[16] + (uint64_t)cases[current].displacement[found]);
            ++found;
        }
        CHECK(found == cases[current].sites);
    }
    uint32_t bounded[HL_A64_TRACE_MAX_WORDS];
    for (size_t index = 0; index < HL_A64_TRACE_MAX_WORDS; ++index) bounded[index] = 0xf9400020u;
    const hl_a64_source_span bounded_span = {
        0xac00, (const uint8_t *)bounded, sizeof(bounded), 11, 12};
    const hl_a64_source bounded_source = {&bounded_span, 1, 11, 12};
    hl_a64_trace_result bounded_trace;
    CHECK(hl_a64_trace_build(&bounded_source, 0xac00, HL_A64_TRACE_MAX_WORDS,
                             code, capacity, &bounded_trace));
    CHECK(bounded_trace.provenance_count == HL_NATIVE_PROVENANCE_MAX);
    for (size_t index = 0; index < 13; ++index) bounded[index] = 0xd50b7421u;
    CHECK(!hl_a64_trace_build(&bounded_source, 0xac00, 13, code, capacity, &bounded_trace));
    CHECK(bounded_trace.provenance_count <= HL_NATIVE_PROVENANCE_MAX);
    /* A lowered LSE word publishes exactly one write access for its own
     * four-byte host opcode. */
    if (hl_a64_atomic_host_supports()) {
        static const uint32_t lse = 0xf8200022u; /* ldadd x0,x2,[x1] */
        const hl_a64_source_span span = {0xb000, (const uint8_t *)&lse, 4, 11, 12};
        const hl_a64_source source = {&span, 1, 11, 12};
        hl_a64_trace_result trace;
        uint32_t found = 0;
        CHECK(hl_a64_trace_build(&source, 0xb000, 1, code, capacity, &trace));
        CHECK(trace.provenance_count <= HL_NATIVE_PROVENANCE_MAX);
        for (uint32_t index = 0; index < trace.provenance_count; ++index) {
            const hl_native_provenance *record = &trace.provenance[index];
            if (record->access == HL_NATIVE_ACCESS_UNKNOWN) continue;
            CHECK(record->access == HL_NATIVE_ACCESS_WRITE && record->width == 8);
            CHECK(record->code_size == sizeof(uint32_t) && record->guest == 0xb000);
            CHECK(record->address.kind == HL_NATIVE_ADDRESS_BASE);
            CHECK(record->address.base == 16 && record->address.displacement == 0);
            ++found;
        }
        CHECK(found == 1);
    }
    /* Exclusive and CASP stay fallback-only until monitor state is ported. */
    static const uint32_t pending[] = {0xc85f7c20u, 0x48207c82u};
    for (size_t index = 0; index < sizeof(pending) / sizeof(pending[0]); ++index) {
        const hl_a64_source_span span = {0xb000, (const uint8_t *)&pending[index], 4, 11, 12};
        const hl_a64_source source = {&span, 1, 11, 12};
        hl_a64_trace_result trace;
        CHECK(!hl_a64_trace_build(&source, 0xb000, 1, code, capacity, &trace));
        CHECK(trace.provenance_count == 0);
    }
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
