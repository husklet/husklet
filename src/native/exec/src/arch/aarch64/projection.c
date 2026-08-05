#include "projection.h"

#define HL_A64_DIRTY_CAPACITY 16u

static void flush_dirty(hl_native_aarch64_cpu *cpu) {
    uint64_t *record;
    if (cpu->dirty_first == UINT64_MAX) return;
    if (cpu->dirty_count >= HL_A64_DIRTY_CAPACITY) {
        cpu->dirty_overflow = 1;
    } else {
        record = cpu->dirty_records[cpu->dirty_count++];
        record[0] = cpu->dirty_view_first;
        record[1] = cpu->dirty_view_last;
        record[2] = cpu->dirty_first;
        record[3] = cpu->dirty_last;
    }
    cpu->dirty_first = UINT64_MAX;
    cpu->dirty_last = 0;
}

int hl_a64_projection_validate(const hl_a64_projection *projection) {
    uint64_t previous = 0;
    if (projection == NULL || projection->views == NULL || projection->count == 0 ||
        projection->active >= projection->count ||
        projection->count > HL_A64_PROJECTION_MAX_VIEWS)
        return 0;
    for (size_t index = 0; index < projection->count; index++) {
        const hl_a64_view *view = &projection->views[index];
        if (view->guest_last <= view->guest_first || view->write_policy != HL_NATIVE_WRITE_EXACT ||
            view->mapping_incarnation != projection->mapping_incarnation ||
            view->permissions == 0 || (view->permissions & ~7u) != 0 ||
            view->host_first > UINT64_MAX - (view->guest_last - view->guest_first) ||
            (index != 0 && view->guest_first < previous))
            return 0;
        previous = view->guest_last;
    }
    return 1;
}

static const hl_a64_view *containing(const hl_a64_projection *projection, uint64_t address) {
    for (size_t index = 0; index < projection->count; index++) {
        const hl_a64_view *view = &projection->views[index];
        if (address >= view->guest_first && address < view->guest_last) return view;
        if (view->guest_first > address) break;
    }
    return NULL;
}

int hl_a64_projection_resolve(const hl_a64_projection *projection, hl_native_aarch64_cpu *cpu,
                              uint64_t address, uint64_t size, uint32_t required) {
    uint64_t cursor = address;
    uint64_t last;
    uint64_t first;
    uint64_t delta;
    uint32_t permissions;
    const hl_a64_view *view;
    if (cpu != NULL) {
        cpu->memory_write_policy = 0;
        cpu->memory_write_index = 0;
    }
    if (cpu == NULL || !hl_a64_projection_validate(projection) || size == 0 || address > UINT64_MAX - size ||
        required == 0 || (required & ~7u) != 0) {
        if (cpu != NULL) {
            cpu->fault_address = address;
            cpu->fault_access = required;
        }
        return 0;
    }
    last = address + size;
    view = containing(projection, cursor);
    if (view == NULL || (view->permissions & required) != required) goto fault;
    first = view->guest_first;
    permissions = view->permissions;
    delta = view->host_first - view->guest_first;
    while (cursor < last) {
        view = containing(projection, cursor);
        if (view == NULL || (view->permissions & required) != required ||
            view->host_first - view->guest_first != delta)
            goto fault;
        cursor = view->guest_last < last ? view->guest_last : last;
    }
    if (cpu->memory_first != first || cpu->memory_last != cursor) flush_dirty(cpu);
    cpu->memory_first = first;
    cpu->memory_last = cursor;
    cpu->memory_delta = delta;
    cpu->memory_permissions = permissions;
    cpu->memory_write_policy = view->write_policy;
    cpu->memory_write_index = view->write_index;
    cpu->fault_address = 0;
    cpu->fault_access = 0;
    cpu->fault_size = 0;
    return 1;

fault:
    cpu->fault_address = cursor;
    cpu->fault_access = required;
    return 0;
}
