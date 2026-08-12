#include "affinity.h"

#include <stdio.h>
#include <string.h>

void hl_linux_affinity_online(uint8_t *mask, size_t size, int online_cpus) {
    memset(mask, 0, size);
    for (int cpu = 0; cpu < online_cpus; cpu++)
        if ((size_t)(cpu / 8) < size) mask[cpu / 8] |= (uint8_t)(1u << (cpu % 8));
}

const uint8_t *hl_linux_affinity_get(struct hl_linux_affinity *affinity, int online_cpus) {
    if (!affinity->set) {
        hl_linux_affinity_online(affinity->mask, sizeof affinity->mask, online_cpus);
        affinity->set = 1;
    }
    return affinity->mask;
}

int hl_linux_affinity_set(struct hl_linux_affinity *affinity, const uint8_t *mask, size_t size, int online_cpus) {
    if (size > sizeof affinity->mask) size = sizeof affinity->mask;
    // Linux clears the mask then intersects it with cpu_active_mask; a request that selects no online CPU --
    // including a zero-length mask, which clears to empty -- is rejected with -EINVAL. Return 0 in that case.
    uint8_t online[HL_LINUX_AFFINITY_BYTES];
    uint8_t wanted[HL_LINUX_AFFINITY_BYTES];
    int any = 0;
    hl_linux_affinity_online(online, sizeof online, online_cpus);
    for (size_t i = 0; i < size; i++) {
        wanted[i] = (mask ? mask[i] : 0u) & online[i];
        if (wanted[i]) any = 1;
    }
    if (!any) return 0;
    memset(affinity->mask, 0, sizeof affinity->mask);
    memcpy(affinity->mask, wanted, size);
    affinity->set = 1;
    return 1;
}

unsigned hl_linux_affinity_first(struct hl_linux_affinity *affinity, int online_cpus) {
    const uint8_t *mask = hl_linux_affinity_get(affinity, online_cpus);
    for (size_t i = 0; i < sizeof affinity->mask; i++)
        if (mask[i])
            for (int bit = 0; bit < 8; bit++)
                if (mask[i] & (1u << bit)) return (unsigned)(i * 8 + (size_t)bit);
    return 0;
}

void hl_linux_affinity_range(char *buffer, size_t size, int online_cpus) {
    if (online_cpus <= 1)
        snprintf(buffer, size, "0\n");
    else
        snprintf(buffer, size, "0-%d\n", online_cpus - 1);
}
