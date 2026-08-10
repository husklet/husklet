#include "shared.h"

#include <stdint.h>
#include <string.h>

hl_status hl_linux_memory_create(const hl_host_services *host, uint64_t size, uint32_t flags, void **output) {
    hl_host_memory_mapping mapping = {HL_HOST_MEMORY_MAPPING_ABI, sizeof(mapping), HL_HOST_HANDLE_INVALID, 0, 0, 0};
    hl_host_result mapped;
    hl_host_result discarded;
    if (output == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *output = NULL;
    if (host == NULL || host->memory == NULL || host->memory->map_anonymous == NULL || host->memory->discard == NULL ||
        host->memory->release == NULL || size == 0 || size > SIZE_MAX)
        return HL_STATUS_INVALID_ARGUMENT;
    if (flags != HL_HOST_MEMORY_SHARED && flags != HL_HOST_MEMORY_PRIVATE) return HL_STATUS_INVALID_ARGUMENT;
    mapped = host->memory->map_anonymous(host->context, 0, size, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, flags,
                                         &mapping);
    if (mapped.status != HL_STATUS_OK || mapping.handle == HL_HOST_HANDLE_INVALID || mapping.address == 0 ||
        mapping.address > UINTPTR_MAX || mapping.mapped_size < size) {
        if (mapping.handle != HL_HOST_HANDLE_INVALID) (void)host->memory->release(host->context, mapping.handle);
        return mapped.status == HL_STATUS_OK ? HL_STATUS_PLATFORM_FAILURE : (hl_status)mapped.status;
    }
    // Freshly created backing (memfd for SHARED, MAP_ANONYMOUS for PRIVATE) is guaranteed
    // zero-filled by the kernel, and the discard() below immediately drops any resident pages
    // (MADV_DONTNEED) so first access re-faults zero on demand. An eager memset here only faults
    // in the whole arena (e.g. 6MB / 1536 pages for the fdvis table) to write zeros that are both
    // redundant and instantly discarded -- pure startup page-fault cost. Rely on the kernel zero.
    discarded = host->memory->discard(host->context, mapping.handle);
    if (discarded.status != HL_STATUS_OK) {
        (void)host->memory->release(host->context, mapping.handle);
        return (hl_status)discarded.status;
    }
    *output = (void *)(uintptr_t)mapping.address;
    return HL_STATUS_OK;
}

hl_status hl_linux_shared_create(const hl_host_services *host, uint64_t size, void **output) {
    return hl_linux_memory_create(host, size, HL_HOST_MEMORY_SHARED, output);
}
