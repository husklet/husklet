#ifndef HL_LINUX_SHARED_H
#define HL_LINUX_SHARED_H

#include "hl/host_services.h"

#include <stdint.h>

hl_status hl_linux_shared_create(const hl_host_services *host, uint64_t size, void **output);
hl_status hl_linux_memory_create(const hl_host_services *host, uint64_t size, uint32_t flags, void **output);

#endif
