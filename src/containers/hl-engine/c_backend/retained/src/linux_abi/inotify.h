#ifndef HL_LINUX_INOTIFY_H
#define HL_LINUX_INOTIFY_H

#include "hl/linux_abi.h"

enum { HL_LINUX_OBJECT_INOTIFY = 0x696e6f74u };

enum {
    HL_LINUX_IN_ACCESS = 0x00000001u,
    HL_LINUX_IN_MODIFY = 0x00000002u,
    HL_LINUX_IN_ATTRIB = 0x00000004u,
    HL_LINUX_IN_CLOSE_WRITE = 0x00000008u,
    HL_LINUX_IN_CLOSE_NOWRITE = 0x00000010u,
    HL_LINUX_IN_OPEN = 0x00000020u,
    HL_LINUX_IN_MOVED_FROM = 0x00000040u,
    HL_LINUX_IN_MOVED_TO = 0x00000080u,
    HL_LINUX_IN_CREATE = 0x00000100u,
    HL_LINUX_IN_DELETE = 0x00000200u,
    HL_LINUX_IN_DELETE_SELF = 0x00000400u,
    HL_LINUX_IN_MOVE_SELF = 0x00000800u,
    HL_LINUX_IN_UNMOUNT = 0x00002000u,
    HL_LINUX_IN_Q_OVERFLOW = 0x00004000u,
    HL_LINUX_IN_IGNORED = 0x00008000u,
    HL_LINUX_IN_ONLYDIR = 0x01000000u,
    HL_LINUX_IN_DONT_FOLLOW = 0x02000000u,
    HL_LINUX_IN_EXCL_UNLINK = 0x04000000u,
    HL_LINUX_IN_MASK_CREATE = 0x10000000u,
    HL_LINUX_IN_MASK_ADD = 0x20000000u,
    HL_LINUX_IN_ISDIR = 0x40000000u
};

#define HL_LINUX_IN_ONESHOT UINT32_C(0x80000000)

typedef struct hl_linux_inotify_provider_event {
    uint64_t token;
    uint32_t mask;
    uint32_t cookie;
    const char *name;
    size_t name_size;
} hl_linux_inotify_provider_event;

/* The provider owns host observation. It never exposes a host descriptor to the guest. */
typedef struct hl_linux_inotify_provider_ops {
    hl_status (*add)(void *context, const char *path, size_t path_size, uint64_t token, uint32_t mask);
    hl_status (*modify)(void *context, uint64_t token, uint32_t mask);
    hl_status (*remove)(void *context, uint64_t token);
    hl_status (*drain)(void *context, hl_linux_inotify_provider_event *events, uint32_t capacity, uint32_t *out_count);
    /* Blocks until an event is available or returns INTERRUPTED. */
    hl_status (*wait)(void *context);
    hl_host_result (*wait_handle)(void *context);
    uint32_t (*readiness)(void *context);
    /* Optional fallback for hosts that cannot supply a wait_handle. */
    hl_status (*subscribe)(void *context, void (*notify)(void *, uint64_t), void *observer, uint64_t token);
    void (*unsubscribe)(void *context, void *observer, uint64_t token);
    hl_status (*clone)(void *context, void **child_context);
    hl_status (*close)(void *context);
} hl_linux_inotify_provider_ops;

int64_t hl_linux_inotify_create(hl_linux_abi *linux_abi, const hl_linux_inotify_provider_ops *provider,
                                void *provider_context, uint32_t descriptor_flags, uint32_t status_flags);
int64_t hl_linux_inotify_create_at(hl_linux_abi *linux_abi, hl_linux_fd requested,
                                   const hl_linux_inotify_provider_ops *provider, void *provider_context,
                                   uint32_t descriptor_flags, uint32_t status_flags);
int64_t hl_linux_inotify_add(hl_linux_abi *linux_abi, hl_linux_fd fd, const char *path, size_t path_size,
                             uint32_t mask);
int64_t hl_linux_inotify_remove(hl_linux_abi *linux_abi, hl_linux_fd fd, int32_t watch);
hl_status hl_linux_inotify_export(hl_linux_abi *linux_abi, hl_linux_fd fd, void *buffer, size_t capacity,
                                  size_t *out_size);
int64_t hl_linux_inotify_import_at(hl_linux_abi *linux_abi, hl_linux_fd requested,
                                   const hl_linux_inotify_provider_ops *provider, void *provider_context,
                                   uint32_t descriptor_flags, uint32_t status_flags, const void *buffer, size_t size);

#endif
