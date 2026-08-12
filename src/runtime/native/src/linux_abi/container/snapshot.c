#include "snapshot.h"

#include <stddef.h>

void hl_linux_snapshot_init(hl_linux_snapshot *snapshot) {
    if (snapshot == NULL) return;
    snapshot->next = HL_LINUX_SNAPSHOT_BASE;
    snapshot->enabled = 0;
}

void hl_linux_snapshot_enable(hl_linux_snapshot *snapshot) {
    if (snapshot == NULL) return;
    if (snapshot->next == 0) snapshot->next = HL_LINUX_SNAPSHOT_BASE;
    snapshot->enabled = 1;
}

int hl_linux_snapshot_enabled(const hl_linux_snapshot *snapshot) {
    return snapshot != NULL && snapshot->enabled != 0;
}

uint64_t hl_linux_snapshot_reserve(hl_linux_snapshot *snapshot, uint64_t length) {
    uint64_t address;
    uint64_t step;

    if (!hl_linux_snapshot_enabled(snapshot) || length == 0 || length > UINT64_MAX - UINT64_C(0xffff)) return 0;
    step = (length + UINT64_C(0xffff)) & ~UINT64_C(0xffff);
    if (snapshot->next > UINT64_MAX - step - UINT64_C(0x100000)) return 0;
    address = snapshot->next;
    snapshot->next += step + UINT64_C(0x100000);
    return address;
}

void hl_linux_snapshot_advance(hl_linux_snapshot *snapshot, uint64_t end) {
    uint64_t aligned;

    if (snapshot == NULL || end <= snapshot->next || end > UINT64_MAX - UINT64_C(0xfffff)) return;
    aligned = (end + UINT64_C(0xfffff)) & ~UINT64_C(0xfffff);
    snapshot->next = aligned;
}
