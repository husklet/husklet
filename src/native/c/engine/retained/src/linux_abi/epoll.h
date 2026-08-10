#ifndef HL_LINUX_EPOLL_H
#define HL_LINUX_EPOLL_H

#include "object.h"

enum { HL_LINUX_EPOLL_ADD = 1, HL_LINUX_EPOLL_DELETE = 2, HL_LINUX_EPOLL_MODIFY = 3 };

#define HL_LINUX_EPOLL_EDGE (UINT32_C(1) << 30)
#define HL_LINUX_EPOLL_ONESHOT (UINT32_C(1) << 31)

typedef struct hl_linux_epoll_event {
    uint32_t readiness;
    uint64_t data;
} hl_linux_epoll_event;

int64_t hl_linux_epoll_create(hl_linux_abi *linux_abi, uint32_t descriptor_flags);
int64_t hl_linux_epoll_control(hl_linux_abi *linux_abi, hl_linux_fd epoll_fd, uint32_t operation, hl_linux_fd target_fd,
                               uint32_t interests, uint64_t data);
int64_t hl_linux_epoll_wait(hl_linux_abi *linux_abi, hl_linux_fd epoll_fd, hl_linux_epoll_event *events,
                            uint32_t capacity, uint64_t deadline_ns);

#endif
