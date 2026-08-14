#ifndef HL_ENGINE_EXECUTABLE_AUTHORITY_H
#define HL_ENGINE_EXECUTABLE_AUTHORITY_H

#include <stdint.h>
#include "hl/host_services.h"

/* Host-neutral identity and guest-visible DAC metadata for an immutable image
 * whose descriptor cannot cross the process-launch boundary. */
typedef struct hl_executable_authority {
    uint64_t stable_device;
    uint64_t stable_object;
    uint32_t user;
    uint32_t group;
    uint32_t mode;
    uint32_t execute_authorized;
    uint32_t ready;
} hl_executable_authority;

static inline int hl_executable_authority_from_metadata(const hl_host_file_metadata *metadata,
                                                        uint32_t execute_authorized,
                                                        hl_executable_authority *authority) {
    if (metadata == NULL || authority == NULL || !execute_authorized || metadata->type != HL_HOST_FILE_TYPE_REGULAR ||
        metadata->stable_device == 0 || metadata->stable_object == 0)
        return 0;
    authority->stable_device = metadata->stable_device;
    authority->stable_object = metadata->stable_object;
    authority->user = metadata->user;
    authority->group = metadata->group;
    authority->mode = metadata->permissions;
    authority->execute_authorized = execute_authorized;
    authority->ready = 1;
    return 1;
}

static inline uint32_t hl_executable_authority_guest_mode(const hl_executable_authority *authority) {
    return authority->mode | (authority->execute_authorized ? 0111u : 0u);
}

#endif
