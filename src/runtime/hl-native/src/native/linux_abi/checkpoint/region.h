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
    // An ANONYMOUS MAP_SHARED region. Linux implements one as an unnamed shmem inode, so it has a
    // real object identity every sharer agrees on (see ckpt_anon_shared_object), but it has no
    // guest descriptor and therefore no `fds` record to seed a restore from. Restore materialises
    // it as one namespace-named object shared by every member instead of a per-process seed --
    // without this flag the region is indistinguishable from a mapping whose descriptor was closed,
    // and each member silently gets a PRIVATE copy of what the guest believes is shared memory.
    uint32_t backing_anon_shared;
    uint32_t reserved;
};

#define CKPT_REGION_VERSION 2

static inline int ckpt_region_valid(const struct ckpt_region *region) {
    return region != 0 && region->format_version == CKPT_REGION_VERSION && region->logical <= 1 &&
           region->backing_shared <= 1 && region->backing_emulated <= 1 && region->backing_anon_shared <= 1 &&
           region->reserved == 0 &&
           // An anonymous shared backing is shared by definition, is never the emulated-refresh
           // form, and is never a logical VMA: reject an image that claims otherwise rather than
           // restoring it down one of the other two paths.
           (!region->backing_anon_shared ||
            (region->backing_object != 0 && region->backing_shared && !region->backing_emulated && !region->logical));
}

#endif
