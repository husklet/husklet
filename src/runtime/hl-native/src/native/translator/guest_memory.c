#include "guest_memory.h"

static const hl_guest_memory_ops *g_ops;
const _Atomic uint64_t *hl_guest_memory_generation;

void hl_guest_memory_bind(const hl_guest_memory_ops *ops) {
    g_ops = ops;
    /* Resolved once here, not per fetch: the accessor behind it is one-time
       initialised and the counter's address never moves. */
    hl_guest_memory_generation = ops != NULL && ops->exec_generation != NULL ? ops->exec_generation() : NULL;
}

int hl_guest_memory_resolve_exec(uint64_t guest, size_t length, const void **host, size_t *contiguous) {
    if (g_ops == NULL || g_ops->resolve_exec == NULL) return 0;
    return g_ops->resolve_exec(guest, length, host, contiguous);
}

/*
 * Fallback when a bound ops table predates exec_span: claim only what
 * resolve_exec actually proved, so the caller's memo cannot outlive one fetch.
 * Unbound is the flat address space, where one span covers everything.
 */
int hl_guest_memory_resolve_exec_span(uint64_t guest, size_t length, uint64_t *generation, uint64_t *first,
                                      uint64_t *last, uint64_t *delta) {
    if (g_ops == NULL || g_ops->resolve_exec == NULL) {
        *generation = 0;
        *first = 0;
        *last = UINT64_MAX;
        *delta = 0;
        return 0;
    }
    if (g_ops->exec_span != NULL) return g_ops->exec_span(guest, generation, first, last, delta);
    const void *host = NULL;
    size_t contiguous = 0;
    int resolution = g_ops->resolve_exec(guest, length, &host, &contiguous);
    if (resolution < 0) return -1;
    *generation = 0;
    *first = guest;
    *last = guest + (resolution > 0 ? contiguous : (length != 0 ? length : 1));
    *delta = resolution > 0 ? (uint64_t)(uintptr_t)host - guest : 0;
    return resolution;
}

int hl_guest_memory_read(uint64_t guest, void *destination, size_t length) {
    if (g_ops == NULL || g_ops->read == NULL) return 0;
    return g_ops->read(guest, destination, length);
}

int hl_guest_memory_write(uint64_t guest, const void *source, size_t length) {
    if (g_ops == NULL || g_ops->write == NULL) return 0;
    return g_ops->write(guest, source, length);
}

int hl_guest_memory_pin_data(uint64_t guest, size_t length, hl_guest_memory_access access, hl_guest_memory_pin *pin) {
    if (pin == NULL || length == 0 || guest > UINT64_MAX - length) return -1;
    *pin = (hl_guest_memory_pin){0};
    if (g_ops != NULL && g_ops->pin != NULL) return g_ops->pin(guest, length, access, pin);
    pin->host = (void *)(uintptr_t)guest;
    pin->contiguous = length;
    return 0;
}

void hl_guest_memory_unpin_data(hl_guest_memory_pin *pin) {
    if (pin == NULL) return;
    if (g_ops != NULL && g_ops->unpin != NULL) g_ops->unpin(pin);
    *pin = (hl_guest_memory_pin){0};
}

int hl_guest_memory_indirect(void) {
    return g_ops != NULL && g_ops->indirect != NULL && g_ops->indirect();
}

uint64_t hl_guest_memory_host_pointer(uint64_t guest) {
    return g_ops != NULL && g_ops->host_pointer != NULL ? g_ops->host_pointer(guest) : guest;
}
