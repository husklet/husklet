#include "device.h"

uint32_t hl_linux_device_major(uint64_t device) {
    return (uint32_t)(((device >> 8) & 0xfffu) | ((uint32_t)(device >> 32) & ~0xfffu));
}

uint32_t hl_linux_device_minor(uint64_t device) {
    return (uint32_t)((device & 0xffu) | ((uint32_t)(device >> 12) & ~0xffu));
}

uint64_t hl_linux_device_make(uint32_t major, uint32_t minor) {
    return ((uint64_t)(major & 0xfffu) << 8) | (minor & 0xffu) | ((uint64_t)(minor & ~0xffu) << 12) |
           ((uint64_t)(major & ~0xfffu) << 32);
}
