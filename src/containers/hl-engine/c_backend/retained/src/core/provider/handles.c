#include "handles.h"

#include <errno.h>
#include <string.h>

#define HL_PROVIDER_HANDLE_TAG UINT64_C(0x4850000000000000)
#define HL_PROVIDER_HANDLE_TAG_MASK UINT64_C(0xffff000000000000)

static hl_host_handle encode(uint32_t slot, uint16_t generation) {
    return HL_PROVIDER_HANDLE_TAG | (uint64_t)generation << 32 | ((uint64_t)slot + 1);
}

static int decode(hl_host_handle handle, uint32_t *slot, uint16_t *generation) {
    uint64_t raw;
    if ((handle & HL_PROVIDER_HANDLE_TAG_MASK) != HL_PROVIDER_HANDLE_TAG) return -EBADF;
    raw = handle & UINT64_C(0xffffffff);
    if (raw == 0 || raw > HL_PROVIDER_HANDLE_MAX) return -EBADF;
    *slot = (uint32_t)(raw - 1);
    *generation = (uint16_t)(handle >> 32);
    if (*generation == 0) return -EBADF;
    return 0;
}

static hl_provider_handle_slot *lookup(hl_provider_handles *handles, hl_host_handle handle) {
    uint32_t slot;
    uint16_t generation;
    if (handles == NULL || decode(handle, &slot, &generation) != 0 || !handles->slots[slot].live ||
        handles->slots[slot].generation != generation)
        return NULL;
    return &handles->slots[slot];
}

int hl_provider_handle_is(hl_host_handle handle) {
    uint32_t slot;
    uint16_t generation;
    return decode(handle, &slot, &generation) == 0;
}

int hl_provider_handle_open(hl_provider_handles *handles, uint64_t remote, hl_host_handle *out_handle) {
    uint32_t index;
    if (handles == NULL || remote == 0 || out_handle == NULL) return -EINVAL;
    for (index = 0; index < HL_PROVIDER_HANDLE_MAX; ++index) {
        hl_provider_handle_slot *slot = &handles->slots[index];
        if (slot->live) continue;
        slot->generation++;
        if (slot->generation == 0) slot->generation = 1;
        slot->remote = remote;
        slot->references = 1;
        slot->live = 1;
        *out_handle = encode(index, slot->generation);
        return 0;
    }
    return -EMFILE;
}

int hl_provider_handle_get(const hl_provider_handles *handles, hl_host_handle handle, uint64_t *out_remote) {
    hl_provider_handle_slot *slot;
    if (out_remote == NULL) return -EINVAL;
    slot = lookup((hl_provider_handles *)(uintptr_t)handles, handle);
    if (slot == NULL) return -EBADF;
    *out_remote = slot->remote;
    return 0;
}

int hl_provider_handle_retain(hl_provider_handles *handles, hl_host_handle handle) {
    hl_provider_handle_slot *slot = lookup(handles, handle);
    if (slot == NULL) return -EBADF;
    if (slot->references == UINT32_MAX) return -EMFILE;
    slot->references++;
    return 0;
}

int hl_provider_handle_release(hl_provider_handles *handles, hl_host_handle handle, uint64_t *out_remote) {
    hl_provider_handle_slot *slot = lookup(handles, handle);
    if (slot == NULL || out_remote == NULL) return -EBADF;
    if (--slot->references != 0) return 0;
    *out_remote = slot->remote;
    slot->remote = 0;
    slot->live = 0;
    return 1;
}

void hl_provider_handles_revoke(hl_provider_handles *handles) {
    uint32_t index;
    if (handles == NULL) return;
    for (index = 0; index < HL_PROVIDER_HANDLE_MAX; ++index) {
        handles->slots[index].remote = 0;
        handles->slots[index].references = 0;
        handles->slots[index].live = 0;
    }
}
