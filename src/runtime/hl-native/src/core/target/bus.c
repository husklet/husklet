#include "bus.h"

void hl_target_bus_init(hl_target_bus *bus, const hl_guest_bus_ops *operations, void *context) {
    bus->operations = *operations;
    hl_guest_bus_init(&bus->guest, &bus->operations, context);
}

void hl_target_bus_bind(hl_target_bus *bus, hl_guest_bus_query query, int active, uint64_t generation) {
    hl_guest_bus_bind(&bus->guest, query, active, generation);
}

void hl_target_bus_changed(hl_target_bus *bus, uint64_t generation, int active) {
    hl_guest_bus_changed(&bus->guest, generation, active);
}

int hl_target_bus_active(const hl_target_bus *bus) {
    return hl_guest_bus_active(&bus->guest);
}

uint64_t hl_target_bus_fault(const hl_target_bus *bus, uint64_t address, uint64_t size) {
    return hl_guest_bus_fault(&bus->guest, address, size);
}

void hl_target_bus_begin(hl_target_bus *bus) {
    hl_guest_bus_begin(&bus->guest);
}

void hl_target_bus_end(hl_target_bus *bus) {
    hl_guest_bus_end(&bus->guest);
}
