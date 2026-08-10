#ifndef HL_LINUX_OBJECT_H
#define HL_LINUX_OBJECT_H

#include "hl/linux_abi.h"

enum {
    HL_LINUX_READY_READ = 1u << 0,
    HL_LINUX_READY_WRITE = 1u << 1,
    HL_LINUX_READY_PRIORITY = 1u << 2,
    HL_LINUX_READY_ERROR = 1u << 3,
    HL_LINUX_READY_HANGUP = 1u << 4
};

typedef struct hl_linux_object_ops {
    /* clone may run while this OFD has an active operation.  Set only when
       clone snapshots immutable state or uses the object's own synchronization. */
    uint32_t fork_while_active_safe;
    int64_t (*read)(void *context, void *buffer, size_t size);
    int64_t (*write)(void *context, const void *buffer, size_t size);
    int64_t (*status)(void *context, hl_linux_file_status *status);
    int64_t (*set_status_flags)(void *context, uint32_t flags);
    uint32_t (*readiness)(void *context, uint32_t interests);
    /* Optional borrowed host object used for event-driven readiness. Never a guest/native descriptor. */
    hl_host_result (*wait_handle)(void *context);
    /* subscribe never calls notify inline; unsubscribe quiesces the token before returning. */
    hl_status (*subscribe)(void *context, void (*notify)(void *observer, uint64_t token), void *observer,
                           uint64_t token);
    void (*unsubscribe)(void *context, void *observer, uint64_t token);
    /* Descriptor retirement notification; must not block. Used to interrupt object waiters. */
    void (*retire)(void *context);
    hl_status (*clone)(void *context, void **child_context);
    /* Before returning, close synchronously quiesces and forgets every subscription callback. */
    hl_status (*close)(void *context);
} hl_linux_object_ops;

typedef struct hl_linux_object_pin {
    hl_linux_abi *linux_abi;
    hl_linux_ofd ofd;
    uint32_t generation;
    const hl_linux_object_ops *ops;
    void *context;
} hl_linux_object_pin;

hl_status hl_linux_object_install(hl_linux_abi *linux_abi, const hl_linux_object_ops *ops, void *context, uint32_t kind,
                                  uint32_t status_flags, uint32_t descriptor_flags, hl_linux_fd *out_fd);
hl_status hl_linux_object_install_at(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_linux_object_ops *ops,
                                     void *context, uint32_t kind, uint32_t status_flags, uint32_t descriptor_flags);
hl_status hl_linux_object_pin_fd(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_object_pin *pin);
hl_status hl_linux_object_pin_ofd(hl_linux_abi *linux_abi, hl_linux_ofd ofd, uint32_t generation,
                                  hl_linux_object_pin *pin);
void hl_linux_object_unpin(hl_linux_object_pin *pin);
hl_status hl_linux_object_unlock(hl_linux_object_pin *pin);
hl_status hl_linux_object_relock(hl_linux_object_pin *pin);
void hl_linux_object_abandon(hl_linux_object_pin *pin);
int hl_linux_object_retired(hl_linux_object_pin *pin);
uint32_t hl_linux_object_ready(hl_linux_object_pin *pin, uint32_t interests);

typedef struct hl_linux_poll_entry {
    hl_linux_fd fd;
    uint32_t interests;
    uint32_t readiness;
} hl_linux_poll_entry;

int64_t hl_linux_object_poll(hl_linux_abi *linux_abi, hl_linux_poll_entry *entries, uint32_t count,
                             uint64_t deadline_ns);

#endif
