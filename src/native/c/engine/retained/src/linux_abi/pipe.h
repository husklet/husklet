#ifndef HL_LINUX_PIPE_H
#define HL_LINUX_PIPE_H

#include "object.h"

enum { HL_LINUX_OBJECT_PIPE = 0x70697065u };

int64_t hl_linux_pipe_create(hl_linux_abi *linux_abi, uint32_t status_flags, uint32_t descriptor_flags,
                             hl_linux_fd output[2]);

#endif
