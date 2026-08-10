#ifndef HL_CORE_BUS_H
#define HL_CORE_BUS_H
#include <stdatomic.h>
#include <stdint.h>
typedef uint64_t (*hl_guest_bus_query)(uint64_t, uint64_t);
typedef hl_guest_bus_query jit_guest_bus_query;

typedef struct hl_guest_bus_ops {
    int (*activate)(void *);
    void (*begin)(void *);
    void (*end)(void *);
} hl_guest_bus_ops;

typedef struct hl_guest_bus {
    hl_guest_bus_query query;
    const hl_guest_bus_ops *ops;
    void *context;
    _Atomic uint64_t generation;
    _Atomic int enabled;
} hl_guest_bus;

void hl_guest_bus_init(hl_guest_bus *, const hl_guest_bus_ops *, void *);
void hl_guest_bus_bind(hl_guest_bus *, hl_guest_bus_query, int, uint64_t);
void hl_guest_bus_changed(hl_guest_bus *, uint64_t, int);
int hl_guest_bus_active(const hl_guest_bus *);
uint64_t hl_guest_bus_fault(const hl_guest_bus *, uint64_t, uint64_t);
void hl_guest_bus_begin(hl_guest_bus *);
void hl_guest_bus_end(hl_guest_bus *);
void jit_guest_bus_bind(hl_guest_bus_query, int, uint64_t);
void jit_guest_bus_changed(void *, uint64_t, int);
void jit_guest_bus_transition_begin(void *);
void jit_guest_bus_transition_end(void *);
int jit_guest_bus_active(void);
uint64_t jit_guest_bus_fault(uint64_t, uint64_t);
#endif
