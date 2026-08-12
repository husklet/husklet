#include "../include/executor.h"
#include "../src/arch/x86_64/projection.h"

#include <stdio.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_projection:%d: %s\n", __LINE__, #value); return 1; } } while (0)

int main(void) {
    uint8_t storage[32] = {0};
    hl_native_projection_view views[] = {
        {.guest_first = 0x1000, .guest_last = 0x1010,
         .host_first = (uint64_t)(uintptr_t)storage, .mapping_incarnation = 7,
         .permissions = 7, .write_policy = HL_NATIVE_WRITE_EXACT, .write_index = 0},
        {.guest_first = 0x1010, .guest_last = 0x1020,
         .host_first = (uint64_t)(uintptr_t)(storage + 16), .mapping_incarnation = 7,
         .permissions = 3, .write_policy = HL_NATIVE_WRITE_EXACT, .write_index = 1},
    };
    hl_native_projection projection = {.views = views, .count = 2,
                                       .mapping_incarnation = 7, .active = 0};
    hl_native_x86_64_cpu cpu = {0};
    cpu.dirty_first = UINT64_MAX;
    CHECK(hl_x86_projection_validate(&projection));
    /* Host-contiguous views with different guest permissions must not share a
       cached write window: projection_written owns the permissions of that
       window, and otherwise a later RW-alias store inherits an RX/RWX bit. */
    CHECK(!hl_x86_projection_resolve(&projection, &cpu, 0x1008, 16, 2));
    CHECK(cpu.fault_address == 0x1010 && cpu.fault_access == 2 && cpu.fault_size == 8);
    CHECK(hl_x86_projection_resolve(&projection, &cpu, 0x1008, 8, 2));
    CHECK(cpu.memory_first == 0x1000 && cpu.memory_last == 0x1010);
    CHECK(cpu.memory_delta == (uint64_t)(uintptr_t)storage - 0x1000);
    CHECK(hl_x86_projection_written(&cpu, 0x1008, 8));
    CHECK(cpu.memory_written == 1 && cpu.fault_access == 0);
    CHECK((cpu.executable_written & 4) != 0);
    CHECK(cpu.dirty_view_first == 0x1000 && cpu.dirty_view_last == 0x1010);
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
    cpu.memory_first = 0;
    cpu.memory_last = 0;
    cpu.dirty_count = 0;
    cpu.dirty_overflow = 0;
    cpu.dirty_first = UINT64_MAX;
    cpu.dirty_last = 0;
    cpu.executable_written = 0;
    CHECK(hl_x86_projection_resolve(&projection, &cpu, 0x1018, 1, 2));
    CHECK(hl_x86_projection_written(&cpu, 0x1018, 1));
    CHECK(cpu.memory_written == 1 && (cpu.executable_written & 4) == 0);
    CHECK(hl_x86_projection_resolve(&projection, &cpu, 0x1010, 1, 2));
    CHECK(hl_x86_projection_written(&cpu, 0x1010, 1));
    CHECK((cpu.executable_written & 4) == 0);
    cpu.dirty_count = 0;
    cpu.dirty_overflow = 0;
    cpu.dirty_first = UINT64_MAX;
    cpu.dirty_last = 0;
    cpu.memory_first = 0;
    cpu.memory_last = 0;
    for (unsigned index = 0; index < 18; ++index) {
        uint64_t address = (index & 1u) == 0u ? 0x1000 : 0x1018;
        CHECK(hl_x86_projection_resolve(&projection, &cpu, address, 1, 2));
        CHECK(hl_x86_projection_written(&cpu, address, 1));
    }
    CHECK(cpu.dirty_count == 2 && cpu.dirty_overflow == 0);
    cpu.dirty_count = HL_X86_DIRTY_CAPACITY;
    cpu.dirty_overflow = 0;
    for (unsigned index = 0; index < HL_X86_DIRTY_CAPACITY; ++index) {
        cpu.dirty_records[index][0] = 0x2000;
        cpu.dirty_records[index][1] = 0x3000;
        cpu.dirty_records[index][2] = 0x2000 + 2u * index;
        cpu.dirty_records[index][3] = 0x2001 + 2u * index;
    }
    cpu.dirty_view_first = 0x1000;
    cpu.dirty_view_last = 0x1010;
    cpu.dirty_first = 0x1000;
    cpu.dirty_last = 0x1001;
    cpu.memory_first = 0x1010;
    cpu.memory_last = 0x1020;
    CHECK(hl_x86_projection_resolve(&projection, &cpu, 0x1000, 1, 2));
    CHECK(cpu.dirty_count == HL_X86_DIRTY_CAPACITY && cpu.dirty_overflow == 1);
    return 0;
}
