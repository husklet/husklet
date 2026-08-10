#ifndef HL_LINUX_ABI_SYSCALL_MISC_H
#define HL_LINUX_ABI_SYSCALL_MISC_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

typedef void (*hl_linux_misc_random_fn)(void *context, void *output, size_t size);
typedef ssize_t (*hl_linux_misc_copy_from_fn)(void *context, void *destination, uint64_t source, size_t size);
typedef ssize_t (*hl_linux_misc_copy_to_fn)(void *context, uint64_t destination, const void *source, size_t size);

typedef struct hl_linux_misc_context {
    char *hostname;
    size_t hostname_capacity;
    uint64_t memory_limit;
    uint64_t memory_used;
    // Host-backend memory snapshot + uptime, so sysinfo(2) agrees with /proc/meminfo and /proc/uptime (the
    // same source vfs.c host_mem/host_btime feed those files from). Zero means "host read failed" -> fall back.
    uint64_t host_memory_total; // bytes; used as sysinfo totalram when there is no cgroup memory limit
    uint64_t host_memory_free;  // bytes; sysinfo freeram when unconstrained
    uint64_t uptime_seconds;    // monotonic seconds since boot (matches /proc/uptime)
    uint64_t loads[3];          // 1-, 5-, 15-minute load, already scaled by 1<<16 (SI_LOAD_SHIFT)
    uint32_t process_count;     // sysinfo procs
    const char *machine;
    hl_linux_misc_copy_from_fn copy_from;
    hl_linux_misc_copy_to_fn copy_to;
    hl_linux_misc_random_fn random;
    void *callback_context;
} hl_linux_misc_context;

int hl_linux_misc_dispatch(hl_linux_misc_context *context, uint64_t number, const uint64_t arguments[6],
                           int64_t *guest_result);

#endif
