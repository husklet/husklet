#include "bus.h"

void hl_guest_bus_init(hl_guest_bus *b, const hl_guest_bus_ops *o, void *c) {
    b->query = 0;
    b->ops = o;
    b->context = c;
    atomic_init(&b->generation, 0);
    atomic_init(&b->enabled, 0);
}

void hl_guest_bus_changed(hl_guest_bus *b, uint64_t g, int active) {
    uint64_t seen = atomic_load_explicit(&b->generation, memory_order_acquire);
    while (g > seen && !atomic_compare_exchange_weak_explicit(&b->generation, &seen, g, memory_order_acq_rel,
                                                              memory_order_acquire)) {}
    if (g < seen) return;
    if (active && !atomic_exchange_explicit(&b->enabled, 1, memory_order_acq_rel) && b->ops && b->ops->activate)
        (void)b->ops->activate(b->context);
}

void hl_guest_bus_bind(hl_guest_bus *b, hl_guest_bus_query q, int a, uint64_t g) {
    b->query = q;
    hl_guest_bus_changed(b, g, a);
}

int hl_guest_bus_active(const hl_guest_bus *b) {
    return atomic_load_explicit(&b->enabled, memory_order_acquire);
}

uint64_t hl_guest_bus_fault(const hl_guest_bus *b, uint64_t a, uint64_t s) {
    return b->query ? b->query(a, s) : 0;
}

void hl_guest_bus_begin(hl_guest_bus *b) {
    if (b->ops && b->ops->begin) b->ops->begin(b->context);
}

void hl_guest_bus_end(hl_guest_bus *b) {
    if (b->ops && b->ops->end) b->ops->end(b->context);
}
