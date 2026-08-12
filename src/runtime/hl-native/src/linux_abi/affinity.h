#ifndef HL_LINUX_AFFINITY_H
#define HL_LINUX_AFFINITY_H

#include <stddef.h>
#include <stdint.h>

#define HL_LINUX_AFFINITY_BYTES 128

struct hl_linux_affinity {
    uint8_t mask[HL_LINUX_AFFINITY_BYTES];
    int set;
};

void hl_linux_affinity_online(uint8_t *mask, size_t size, int online_cpus);
const uint8_t *hl_linux_affinity_get(struct hl_linux_affinity *affinity, int online_cpus);
int hl_linux_affinity_set(struct hl_linux_affinity *affinity, const uint8_t *mask, size_t size, int online_cpus);
unsigned hl_linux_affinity_first(struct hl_linux_affinity *affinity, int online_cpus);
void hl_linux_affinity_range(char *buffer, size_t size, int online_cpus);

#endif
