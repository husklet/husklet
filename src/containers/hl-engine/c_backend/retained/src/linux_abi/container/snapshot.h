#ifndef HL_LINUX_ABI_CONTAINER_SNAPSHOT_H
#define HL_LINUX_ABI_CONTAINER_SNAPSHOT_H

#include <stdint.h>

#define HL_LINUX_SNAPSHOT_BASE UINT64_C(0x50000000000)

typedef struct hl_linux_snapshot {
    uint64_t next;
    uint32_t enabled;
} hl_linux_snapshot;

void hl_linux_snapshot_init(hl_linux_snapshot *snapshot);
void hl_linux_snapshot_enable(hl_linux_snapshot *snapshot);
int hl_linux_snapshot_enabled(const hl_linux_snapshot *snapshot);
uint64_t hl_linux_snapshot_reserve(hl_linux_snapshot *snapshot, uint64_t length);
void hl_linux_snapshot_advance(hl_linux_snapshot *snapshot, uint64_t end);

#endif
