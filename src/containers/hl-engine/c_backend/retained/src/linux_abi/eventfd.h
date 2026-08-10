#ifndef HL_LINUX_EVENTFD_H
#define HL_LINUX_EVENTFD_H

#include "object.h"

enum { HL_LINUX_OBJECT_EVENTFD = 3u, HL_LINUX_EVENTFD_SEMAPHORE = 1u, HL_LINUX_EVENTFD_NONBLOCK = 1u << 1 };

int64_t hl_linux_eventfd_create(hl_linux_abi *linux_abi, uint64_t initial, uint32_t flags, uint32_t descriptor_flags);
int64_t hl_linux_eventfd_create_at(hl_linux_abi *linux_abi, hl_linux_fd requested, uint64_t initial, uint32_t flags,
                                   uint32_t descriptor_flags);
hl_status hl_linux_eventfd_wait_handle(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_host_handle *handle);
int64_t hl_linux_eventfd_send(hl_linux_abi *linux_abi, hl_host_handle endpoint, hl_linux_fd fd,
                              hl_host_const_bytes payload, uint32_t rights);
int64_t hl_linux_eventfd_receive(hl_linux_abi *linux_abi, hl_host_handle endpoint, hl_host_bytes payload,
                                 uint32_t descriptor_flags, hl_linux_fd *fd);

#endif
