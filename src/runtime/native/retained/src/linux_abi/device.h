#ifndef HL_LINUX_DEVICE_H
#define HL_LINUX_DEVICE_H

#include <stdint.h>

uint32_t hl_linux_device_major(uint64_t device);
uint32_t hl_linux_device_minor(uint64_t device);
uint64_t hl_linux_device_make(uint32_t major, uint32_t minor);

#endif
