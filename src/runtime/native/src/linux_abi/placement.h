#ifndef HL_LINUX_ABI_PLACEMENT_H
#define HL_LINUX_ABI_PLACEMENT_H

#include <stddef.h>
#include <stdint.h>

enum { HL_ELF_MAP_FLEXIBLE = 0, HL_ELF_MAP_FIXED = 1 };

typedef void *(*hl_elf_mapper)(void *context, void *address, size_t length, uint32_t placement);

/*
 * Try the deterministic ELF image address when one is requested. If the host
 * rejects it, preserve execution by retrying at a host-selected address and
 * report that the resulting image is not safe for fixed-address cache reuse.
 * A mapper reports failure with NULL.
 */
void *hl_elf_place_image(hl_elf_mapper mapper, void *context, void *fixed_address, size_t length, int *fixed_failed);

#endif
