#include "projection.h"

static void flush_dirty(hl_native_x86_64_cpu *cpu) {
    uint64_t *record;
    if (cpu->dirty_first == UINT64_MAX) return;
    for (uint64_t index = 0; index < cpu->dirty_count; ++index) {
        record = cpu->dirty_records[index];
        if (record[0] != cpu->dirty_view_first || record[1] != cpu->dirty_view_last ||
            record[2] > cpu->dirty_last || record[3] < cpu->dirty_first)
            continue;
        if (cpu->dirty_first < record[2]) record[2] = cpu->dirty_first;
        if (cpu->dirty_last > record[3]) record[3] = cpu->dirty_last;
        cpu->dirty_first = UINT64_MAX;
        cpu->dirty_last = 0;
        return;
    }
    if (cpu->dirty_count >= HL_X86_DIRTY_CAPACITY) {
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

int hl_x86_projection_switch_writable(const hl_native_x86_64_cpu *cpu,
                                      uint64_t memory_first, uint64_t memory_last) {
    if (cpu == NULL || memory_first >= memory_last) return 0;
    if (cpu->dirty_first == UINT64_MAX ||
        (cpu->memory_first == memory_first && cpu->memory_last == memory_last) ||
        cpu->dirty_count < HL_X86_DIRTY_CAPACITY)
        return 1;
    for (uint64_t index = 0; index < cpu->dirty_count; ++index) {
        const uint64_t *record = cpu->dirty_records[index];
        if (record[0] == cpu->dirty_view_first && record[1] == cpu->dirty_view_last &&
            record[2] <= cpu->dirty_last && record[3] >= cpu->dirty_first)
            return 1;
    }
    return 0;
}

static const hl_native_projection_view *containing(const hl_native_projection *projection,
                                                   uint64_t address) {
    size_t index;
    for (index = 0; index < projection->count; ++index) {
        const hl_native_projection_view *view = &projection->views[index];
        if (address >= view->guest_first && address < view->guest_last) return view;
        if (view->guest_first > address) break;
    }
    return NULL;
}

int hl_x86_projection_validate(const hl_native_projection *projection) {
    uint64_t previous = 0;
    size_t index;
    if (projection == NULL || projection->views == NULL || projection->count == 0 ||
        projection->active >= projection->count ||
        projection->count > HL_X86_PROJECTION_MAX_VIEWS)
        return 0;
    for (index = 0; index < projection->count; ++index) {
        const hl_native_projection_view *view = &projection->views[index];
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

int hl_x86_projection_resolve(const hl_native_projection *projection, hl_native_x86_64_cpu *cpu,
                              uint64_t address, uint64_t size, uint32_t required) {
    const hl_native_projection_view *view;
    uint64_t cursor = address;
    uint64_t last;
    uint64_t delta;
    uint64_t first;
    uint32_t permissions;
    if (cpu == NULL || !hl_x86_projection_validate(projection) || size == 0 ||
        address > UINT64_MAX - size || required == 0 || (required & ~7u) != 0) {
        if (cpu != NULL) {
            cpu->fault_address = address;
            cpu->fault_access = required;
            cpu->fault_size = size;
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
            view->permissions != permissions ||
            view->host_first - view->guest_first != delta)
            goto fault;
        cursor = view->guest_last < last ? view->guest_last : last;
    }
    if (cpu->memory_first != first || cpu->memory_last != cursor) flush_dirty(cpu);
    cpu->memory_first = first;
    cpu->memory_last = cursor;
    cpu->memory_delta = delta;
    cpu->memory_permissions = permissions;
    cpu->fault_address = 0;
    cpu->fault_access = 0;
    cpu->fault_size = 0;
    return 1;

fault:
    cpu->fault_address = cursor;
    cpu->fault_access = required;
    cpu->fault_size = last - cursor;
    return 0;
}

int hl_x86_projection_written(hl_native_x86_64_cpu *cpu, uint64_t address, uint64_t size) {
    if (cpu == NULL || size == 0 || address < cpu->memory_first || address > UINT64_MAX - size ||
        address + size > cpu->memory_last || (cpu->memory_permissions & 2u) == 0) {
        if (cpu != NULL) {
            cpu->fault_address = address;
            cpu->fault_access = 2;
            cpu->fault_size = size;
        }
        return 0;
    }
    cpu->memory_written = 1;
    cpu->executable_written |= cpu->memory_permissions;
    if (cpu->dirty_first == UINT64_MAX || address < cpu->dirty_first) cpu->dirty_first = address;
    if (address + size > cpu->dirty_last) cpu->dirty_last = address + size;
    cpu->dirty_view_first = cpu->memory_first;
    cpu->dirty_view_last = cpu->memory_last;
    return 1;
}
