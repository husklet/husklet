#include "placement.h"

void *hl_elf_place_image(hl_elf_mapper mapper, void *context, void *fixed_address, size_t length, int *fixed_failed) {
    void *mapped;
    if (fixed_failed != NULL) *fixed_failed = 0;
    if (mapper == NULL || length == 0) return NULL;
    if (fixed_address == NULL) return mapper(context, NULL, length, HL_ELF_MAP_FLEXIBLE);
    mapped = mapper(context, fixed_address, length, HL_ELF_MAP_FIXED);
    if (mapped != NULL) return mapped;
    if (fixed_failed != NULL) *fixed_failed = 1;
    return mapper(context, NULL, length, HL_ELF_MAP_FLEXIBLE);
}
