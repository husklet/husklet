#include "../include/executor.h"
#include "../src/arch/x86_64/projection.h"

#include <stdio.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_projection:%d: %s\n", __LINE__, #value); return 1; } } while (0)

int main(void) {
    uint8_t storage[32] = {0};
    hl_native_projection_view views[] = {
        {0x1000, 0x1010, (uint64_t)(uintptr_t)storage, 7, 7, 0},
        {0x1010, 0x1020, (uint64_t)(uintptr_t)(storage + 16), 7, 3, 0},
    };
    hl_native_projection projection = {views, 2, 7};
    hl_native_x86_64_cpu cpu = {0};
    cpu.dirty_first = UINT64_MAX;
    CHECK(hl_x86_projection_validate(&projection));
    CHECK(hl_x86_projection_resolve(&projection, &cpu, 0x1008, 16, 2));
    CHECK(cpu.memory_first == 0x1000 && cpu.memory_last == 0x1018);
    CHECK(cpu.memory_delta == (uint64_t)(uintptr_t)storage - 0x1000);
    CHECK(hl_x86_projection_written(&cpu, 0x1008, 8));
    CHECK(cpu.memory_written == 1 && cpu.fault_access == 0);
    CHECK((cpu.executable_written & 4) != 0);
    CHECK(cpu.dirty_view_first == 0x1000 && cpu.dirty_view_last == 0x1018);
    CHECK(cpu.dirty_first == 0x1008 && cpu.dirty_last == 0x1010);
    CHECK(!hl_x86_projection_written(&cpu, 0x1018, 1));
    CHECK(cpu.fault_address == 0x1018 && cpu.fault_access == 2 && cpu.fault_size == 1);
    views[1].host_first++;
    CHECK(!hl_x86_projection_resolve(&projection, &cpu, 0x1008, 16, 2));
    CHECK(cpu.fault_address == 0x1010 && cpu.fault_access == 2 && cpu.fault_size == 8);
    views[1].host_first--;
    views[1].permissions = 1;
    CHECK(!hl_x86_projection_resolve(&projection, &cpu, 0x1008, 16, 2));
    CHECK(cpu.fault_address == 0x1010);
    CHECK(!hl_x86_projection_resolve(&projection, &cpu, UINT64_MAX, 2, 1));
    CHECK(cpu.fault_address == UINT64_MAX && cpu.fault_size == 2);
    views[1].permissions = 3;
    CHECK(hl_x86_projection_resolve(&projection, &cpu, 0x1018, 1, 2));
    CHECK(hl_x86_projection_written(&cpu, 0x1018, 1));
    CHECK(cpu.memory_written == 1 && (cpu.executable_written & 4) != 0);
    CHECK(cpu.dirty_count == 1);
    CHECK(cpu.dirty_records[0][0] == 0x1000 && cpu.dirty_records[0][1] == 0x1018);
    CHECK(cpu.dirty_records[0][2] == 0x1008 && cpu.dirty_records[0][3] == 0x1010);
    for (unsigned index = 0; index < 18; ++index) {
        uint64_t address = (index & 1u) == 0u ? 0x1000 : 0x1018;
        CHECK(hl_x86_projection_resolve(&projection, &cpu, address, 1, 2));
        CHECK(hl_x86_projection_written(&cpu, address, 1));
    }
    CHECK(cpu.dirty_count == 16 && cpu.dirty_overflow == 1);
    return 0;
}
