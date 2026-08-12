#include "xattr.h"

#include <stdint.h>
#include <string.h>

#define HL_XATTR_CACHE_SIZE 4096u

typedef struct hl_xattr_cache_entry {
    uint64_t device;
    uint64_t inode;
    uint32_t generation;
} hl_xattr_cache_entry;

static hl_xattr_cache_entry entries[HL_XATTR_CACHE_SIZE];
static uint32_t generation = 1;

static uint32_t cache_slot(uint64_t device, uint64_t inode) {
    uint64_t hash = (inode * UINT64_C(0x9E3779B97F4A7C15)) ^ (device * UINT64_C(0xC2B2AE3D27D4EB4F));
    return (uint32_t)(hash >> 33) & (HL_XATTR_CACHE_SIZE - 1u);
}

int hl_xattr_cache_is_negative(uint64_t device, uint64_t inode) {
    if (inode == 0) return 0;
    const hl_xattr_cache_entry *entry = &entries[cache_slot(device, inode)];
    return entry->generation == generation && entry->device == device && entry->inode == inode;
}

void hl_xattr_cache_record_negative(uint64_t device, uint64_t inode) {
    if (inode == 0) return;
    hl_xattr_cache_entry *entry = &entries[cache_slot(device, inode)];
    entry->device = device;
    entry->inode = inode;
    entry->generation = generation;
}

void hl_xattr_cache_invalidate(void) {
    generation++;
    if (generation == 0) {
        generation = 1;
        memset(entries, 0, sizeof entries);
    }
}
