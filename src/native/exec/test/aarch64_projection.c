#include "../src/arch/aarch64/projection.h"

#include <stdio.h>
#include <string.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "projection:%d: %s\n", __LINE__, #x); return 1; } } while (0)

int main(void) {
    const hl_a64_view views[] = {
        {0x1000, 0x2000, 0x100000, 9, HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE, 0},
        {0x2000, 0x3000, 0x101000, 9, HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE, 0},
        {0x4000, 0x5000, 0x200000, 9, HL_A64_PERMISSION_READ, 0},
    };
    const hl_a64_projection projection = {views, 3, 9};
    hl_native_aarch64_cpu cpu;
    memset(&cpu, 0, sizeof(cpu));
    CHECK(hl_a64_projection_validate(&projection));
    CHECK(hl_a64_projection_resolve(&projection, &cpu, 0x1ff8, 16, HL_A64_PERMISSION_WRITE));
    CHECK(cpu.memory_first == 0x1000 && cpu.memory_last == 0x2008);
    CHECK(cpu.memory_delta == 0xff000 && cpu.memory_permissions == 3);
    uint64_t first = cpu.memory_first, last = cpu.memory_last, delta = cpu.memory_delta;
    CHECK(!hl_a64_projection_resolve(&projection, &cpu, 0x3ff8, 16, HL_A64_PERMISSION_READ));
    CHECK(cpu.fault_address == 0x3ff8 && cpu.fault_access == HL_A64_PERMISSION_READ);
    CHECK(cpu.memory_first == first && cpu.memory_last == last && cpu.memory_delta == delta);
    CHECK(!hl_a64_projection_resolve(&projection, &cpu, 0x4000, 8, HL_A64_PERMISSION_WRITE));
    CHECK(cpu.fault_address == 0x4000 && cpu.memory_first == first);
    CHECK(hl_a64_projection_resolve(&projection, &cpu, 0x4001, 7, HL_A64_PERMISSION_READ));
    CHECK(cpu.memory_first == 0x4000 && cpu.memory_last == 0x4008);
    CHECK(!hl_a64_projection_resolve(&projection, &cpu, UINT64_MAX - 1, 4, HL_A64_PERMISSION_READ));
    CHECK(cpu.fault_address == UINT64_MAX - 1);
    hl_a64_projection stale = projection;
    stale.mapping_incarnation++;
    CHECK(!hl_a64_projection_validate(&stale));
    return 0;
}
