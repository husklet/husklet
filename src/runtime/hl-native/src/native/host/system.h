#ifndef HL_HOST_SYSTEM_H
#define HL_HOST_SYSTEM_H

/* Native descriptors kept disjoint from the guest-visible interval. */
#define HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM 4096u
#define HL_HOST_GUEST_DESCRIPTOR_MINIMUM 20480u

void hl_host_private_init(void);

#include <stddef.h>
#include <stdint.h>

typedef struct hl_host_cpu_ticks {
    uint64_t user;
    uint64_t nice;
    uint64_t system;
    uint64_t idle;
} hl_host_cpu_ticks;

typedef struct hl_host_system_info {
    uint64_t boot_time_seconds;
    uint64_t memory_total;
    uint64_t memory_free;
    uint64_t memory_available;
    uint64_t memory_cached;
    uint32_t online_cpus;
    uint32_t reported_cores;
    hl_host_cpu_ticks aggregate;
} hl_host_system_info;

typedef struct hl_host_process_info {
    int64_t parent_pid;
    int64_t process_group;
    int64_t session;
    int64_t terminal_device;
    int64_t foreground_group;
    uint64_t resident_bytes;
    uint64_t virtual_bytes;
    uint64_t user_time_ns;
    uint64_t system_time_ns;
    uint64_t start_time_seconds;
    uint64_t start_time_ns;
    uint32_t threads;
    char state;
    char name[32];
} hl_host_process_info;

enum {
    HL_HOST_FD_OTHER = 0,
    HL_HOST_FD_FILE = 1,
    HL_HOST_FD_PIPE = 2,
    HL_HOST_FD_SOCKET = 3,
};

enum { HL_HOST_PROCESS_FD_ENGINE_PRIVATE = 1u << 0 };

typedef struct hl_host_process_fd {
    int32_t descriptor;
    uint32_t kind;
    uint32_t flags;
    uint32_t reserved;
    uint64_t stable_device;
    uint64_t stable_object;
} hl_host_process_fd;

typedef struct hl_host_process_peer {
    int64_t identity;
} hl_host_process_peer;

typedef struct hl_host_process_resource_snapshot {
    uint64_t nofile_current;
    uint64_t nofile_maximum;
    uint64_t nproc_current;
    uint64_t nproc_maximum;
    int32_t nofile_status;
    int32_t nproc_status;
    int32_t open_descriptors;
    int32_t threads;
    int32_t caller_children;
    int32_t children_truncated;
} hl_host_process_resource_snapshot;

/* Snapshot host-wide values and up to core_capacity per-core counters. */
int hl_host_system_read(hl_host_system_info *info, hl_host_cpu_ticks *cores, size_t core_capacity);

/* Snapshot one live native process. Returns zero when the pid is absent or inaccessible. */
int hl_host_process_read(int64_t pid, hl_host_process_info *info);

/* Resolve only a process's start-time identity token, writing it to start_time_ns and returning
 * non-zero on success. Equivalent to hl_host_process_read()'s start_time_ns field, but the calling
 * process's own token is memoized: it is immutable for the process's lifetime and the memo is retired by
 * a fork epoch, so a forked child re-reads rather than answering with its parent's token. Peer pids are always observed
 * fresh, because a remembered start time for a recycled peer pid would defeat the very reuse check the
 * token exists to perform. */
int hl_host_process_start_time_ns(int64_t pid, uint64_t *start_time_ns);

/* This process's own pid paired with its own start-time token, in one call and -- after the first --
 * with no host syscall at all. Both fields come from a memo retired by a fork epoch that a
 * pthread_atfork() child handler bumps, so a forked child never reports its parent's identity. Use
 * this instead of getpid() + hl_host_process_start_time_ns(getpid(), ...) wherever the caller wants
 * its OWN identity; it is the pair every engine-private descriptor row and fdvis owner stamp is keyed
 * on. Returns non-zero on success. */
int hl_host_process_self_identity(int64_t *pid, uint64_t *start_time_ns);

/* Retire the self-identity memo. Registered as a pthread_atfork() child handler, and called again
 * explicitly from the engine's own post-fork child hook. Async-signal-safe: one relaxed increment. */
void hl_host_process_identity_after_fork(void);

/* Capture real host limits and topology below guest ABI emulation. */
int hl_host_process_resource_read(hl_host_process_resource_snapshot *snapshot);

/* Enumerate descriptor numbers. kind may remain OTHER until fd_read; count includes truncated entries. */
int hl_host_process_fds(int64_t pid, hl_host_process_fd *entries, size_t capacity, size_t *count);
/* Collect every descriptor in ONE enumeration pass, allocating the listing for the caller (free() it).
 * The size-then-list pair that callers used to write costs TWO passes, and on Linux one pass over
 * /proc/<pid>/fd is O(fd-TABLE SIZE) in the kernel -- 1.14 ms on a table the engine-private descriptor
 * band has expanded to 65536 slots, whatever the handful of descriptors actually open. Sizing separately
 * therefore doubled the most expensive call in execve. Growing the buffer inside the pass also removes
 * the truncation race the two-call form had to fall back on: no descriptor can be opened between a
 * sizing call and a listing call that no longer exist. Returns zero when enumeration is unavailable, in
 * which case the caller must keep its bounded linear-scan fallback. */
int hl_host_process_fds_collect(int64_t pid, hl_host_process_fd **entries, size_t *count);
int hl_host_process_fd_private_add(int descriptor);
/* Takes ownership of descriptor on success and returns its relocated engine-private number; leaves the
 * input open and returns a negative errno on failure. */
int hl_host_process_fd_private_adopt(int descriptor);
typedef struct hl_host_process_fd_private_plan hl_host_process_fd_private_plan;
/* Duplicate the explicitly retained child channels above the guest interval before fork. The child then
 * closes the entire low native interval, giving typed guest descriptors an exec-like empty namespace. */
int hl_host_process_fd_private_plan_prepare(int minimum, const int *descriptors, size_t descriptor_count,
                                            hl_host_process_fd_private_plan **plan);
int hl_host_process_fd_private_plan_descriptor(const hl_host_process_fd_private_plan *plan, int descriptor);
int hl_host_process_fd_private_plan_child(const hl_host_process_fd_private_plan *plan);
int hl_host_process_fd_private_plan_release(hl_host_process_fd_private_plan **plan);
int hl_host_process_fd_private_floor(void);
void hl_host_process_fd_private_remove(int descriptor);
int hl_host_process_fd_private_is(int64_t pid, uint64_t start_ns, int descriptor);
int hl_host_process_fd_private_current(int descriptor);
size_t hl_host_process_fd_private_count_current(void);
int hl_host_process_fd_private_fork_prepare(void);
int hl_host_process_fd_private_fork_complete(int child);
void hl_host_process_fd_private_cleanup(void);
#if defined(HL_NATIVE_TEST_HOOKS) && !defined(_WIN32)
/* Forks while a sibling thread holds the private-fd fork lock and drives the child down the
   locking path, bounded so an inherited-locked mutex reports instead of hanging. */
int hl_c_backend_private_fork_lock_test(uint32_t scenario);
#endif

/* Query one open descriptor and, for files, copy its native absolute path without a trailing NUL. */
int hl_host_process_fd_read(int64_t pid, int32_t descriptor, hl_host_process_fd *entry, char *path,
                            size_t path_capacity, size_t *path_size);

/* Enumerate other instances of this executable in the current process session. count includes truncation. */
int hl_host_process_peers(hl_host_process_peer *entries, size_t capacity, size_t *count);

/* Interrupt one enumerated peer so it can reach an engine safepoint. */
int hl_host_process_interrupt(hl_host_process_peer peer);

#endif
