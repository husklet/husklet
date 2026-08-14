#ifndef HL_ENGINE_EXECUTABLE_AUTHORITY_H
#define HL_ENGINE_EXECUTABLE_AUTHORITY_H

#include <stdint.h>

/* Host-neutral identity and guest-visible DAC metadata for an immutable image
 * whose descriptor cannot cross the process-launch boundary. */
typedef struct hl_executable_authority {
    uint64_t stable_device;
    uint64_t stable_object;
    uint32_t user;
    uint32_t group;
    uint32_t mode;
    uint32_t ready;
} hl_executable_authority;

#endif
