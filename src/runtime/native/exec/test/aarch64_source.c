#include "../src/arch/aarch64/source.h"

#include <stdio.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "source:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static int crossing(void) {
    const uint8_t first[] = {0x20, 0x00, 0x80, 0xd2};
    const uint8_t second[] = {0x00, 0x08, 0x00, 0x91};
    const hl_a64_source_span spans[] = {
        {0x4ffc, first, sizeof(first), 7, 11},
        {0x5000, second, sizeof(second), 7, 11},
    };
    const hl_a64_source source = {spans, 2, 7, 11};
    hl_a64_fetch_result result;
    CHECK(hl_a64_source_fetch(&source, 0x4ffe, 2, &result) == 0); /* unaligned PC */
    CHECK(hl_a64_source_fetch(&source, 0x4ffc, 2, &result));
    CHECK(result.words[0] == 0xd2800020u && result.words[1] == 0x91000800u);
    CHECK(result.source_last == 0x5004 && result.fault_pc == 0);
    return 0;
}

static int boundary_and_fault(void) {
    const uint8_t left[] = {0x20, 0x00};
    const uint8_t right[] = {0x80, 0xd2, 0x00, 0x08};
    const hl_a64_source_span spans[] = {
        {0x6000, left, sizeof(left), 9, 4},
        {0x6002, right, sizeof(right), 9, 4},
    };
    hl_a64_source source = {spans, 2, 9, 4};
    hl_a64_fetch_result result;
    CHECK(hl_a64_source_fetch(&source, 0x6000, 1, &result));
    CHECK(result.words[0] == 0xd2800020u);
    CHECK(!hl_a64_source_fetch(&source, 0x6000, 2, &result));
    CHECK(result.count == 1 && result.fault_pc == 0x6004);
    source.instruction_epoch++;
    CHECK(!hl_a64_source_fetch(&source, 0x6000, 1, &result));
    CHECK(result.count == 0 && result.fault_pc == 0x6000);
    return 0;
}

int main(void) { return crossing() || boundary_and_fault(); }
