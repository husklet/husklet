#ifndef HL_CORE_PROVIDER_HANDLES_H
#define HL_CORE_PROVIDER_HANDLES_H

#include "hl/host_services.h"

#include <stdint.h>

enum { HL_PROVIDER_HANDLE_MAX = 1024 };

typedef struct hl_provider_handle_slot {
    uint64_t remote;
    uint32_t references;
    uint16_t generation;
    uint16_t live;
} hl_provider_handle_slot;

typedef struct hl_provider_handles {
    hl_provider_handle_slot slots[HL_PROVIDER_HANDLE_MAX];
} hl_provider_handles;

int hl_provider_handle_is(hl_host_handle handle);
int hl_provider_handle_open(hl_provider_handles *handles, uint64_t remote, hl_host_handle *out_handle);
int hl_provider_handle_get(const hl_provider_handles *handles, hl_host_handle handle, uint64_t *out_remote);
int hl_provider_handle_retain(hl_provider_handles *handles, hl_host_handle handle);
/* Returns one and the remote id for the final reference, zero for a retained reference, or negative errno. */
int hl_provider_handle_release(hl_provider_handles *handles, hl_host_handle handle, uint64_t *out_remote);
void hl_provider_handles_revoke(hl_provider_handles *handles);

#endif
