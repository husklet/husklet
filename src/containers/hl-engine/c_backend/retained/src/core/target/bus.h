#ifndef HL_CORE_TARGET_BUS_H
#define HL_CORE_TARGET_BUS_H

#include "../bus.h"

typedef struct hl_target_bus {
    hl_guest_bus guest;
    hl_guest_bus_ops operations;
} hl_target_bus;

void hl_target_bus_init(hl_target_bus *, const hl_guest_bus_ops *, void *);
void hl_target_bus_bind(hl_target_bus *, hl_guest_bus_query, int, uint64_t);
void hl_target_bus_changed(hl_target_bus *, uint64_t, int);
int hl_target_bus_active(const hl_target_bus *);
uint64_t hl_target_bus_fault(const hl_target_bus *, uint64_t, uint64_t);
void hl_target_bus_begin(hl_target_bus *);
void hl_target_bus_end(hl_target_bus *);

#endif
