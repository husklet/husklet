#ifndef HL_LINUX_ABI_CHECKPOINT_REGION_H
#define HL_LINUX_ABI_CHECKPOINT_REGION_H

#include <stdint.h>

struct ckpt_region {
    uint64_t addr, len, glen;
    int32_t prot;
    int32_t is_gna;
    uint64_t npages;
    uint64_t backing_object;
    uint64_t backing_offset;
    uint32_t backing_shared;
    uint32_t backing_emulated;
    uint32_t format_version;
    uint32_t logical;
};

#define CKPT_REGION_VERSION 1

static inline int ckpt_region_valid(const struct ckpt_region *region) {
    return region != 0 && region->format_version == CKPT_REGION_VERSION && region->logical <= 1 &&
           region->backing_shared <= 1 && region->backing_emulated <= 1;
}

#endif
