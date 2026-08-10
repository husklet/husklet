#ifndef HL_LINUX_ABI_H
#define HL_LINUX_ABI_H

#include "hl/base.h"
#include "hl/host_services.h"

#include <stdatomic.h>

HL_EXTERN_C_BEGIN

#define HL_LINUX_ABI_VERSION 4u
#define HL_LINUX_FD_LIMIT 65536u
#define HL_LINUX_OFD_LIMIT 65536u

typedef uint32_t hl_linux_fd;
typedef uint32_t hl_linux_ofd;

/* Linux guest errno numbers returned by this library as negative syscall results. */
typedef enum hl_linux_errno {
    HL_LINUX_ENOENT = 2,
    HL_LINUX_EINTR = 4,
    HL_LINUX_EIO = 5,
    HL_LINUX_EBADF = 9,
    HL_LINUX_EAGAIN = 11,
    HL_LINUX_ENXIO = 6,
    HL_LINUX_ENOMEM = 12,
    HL_LINUX_EACCES = 13,
    HL_LINUX_EFAULT = 14,
    HL_LINUX_EBUSY = 16,
    HL_LINUX_EEXIST = 17,
    HL_LINUX_ENOTDIR = 20,
    HL_LINUX_EISDIR = 21,
    HL_LINUX_EINVAL = 22,
    HL_LINUX_ENFILE = 23,
    HL_LINUX_EMFILE = 24,
    HL_LINUX_EFBIG = 27,
    HL_LINUX_ENOSPC = 28,
    HL_LINUX_ESPIPE = 29,
    HL_LINUX_EPIPE = 32,
    HL_LINUX_EROFS = 30,
    HL_LINUX_ENAMETOOLONG = 36,
    HL_LINUX_ENOSYS = 38,
    HL_LINUX_ELOOP = 40,
    HL_LINUX_ENOTEMPTY = 39,
    HL_LINUX_EXDEV = 18,
    HL_LINUX_EDQUOT = 122,
    HL_LINUX_ETIMEDOUT = 110,
    HL_LINUX_ECONNREFUSED = 111,
    HL_LINUX_ECONNRESET = 104,
    HL_LINUX_ENETUNREACH = 101,
    HL_LINUX_EADDRINUSE = 98,
    HL_LINUX_EOVERFLOW = 75
} hl_linux_errno;

enum { HL_LINUX_PATH_MAX = 4096 };

enum { HL_LINUX_IOV_MAX = 1024 };

enum {
    HL_LINUX_O_ACCMODE = 00000003u,
    HL_LINUX_O_RDONLY = 00000000u,
    HL_LINUX_O_WRONLY = 00000001u,
    HL_LINUX_O_RDWR = 00000002u,
    HL_LINUX_O_CREAT = 00000100u,
    HL_LINUX_O_EXCL = 00000200u,
    HL_LINUX_O_TRUNC = 00001000u,
    HL_LINUX_O_APPEND = 00002000u,
    HL_LINUX_O_NONBLOCK = 00004000u,
    HL_LINUX_O_DIRECT = 00040000u,
    HL_LINUX_O_NOFOLLOW = 00400000u,
    HL_LINUX_O_DIRECTORY = 00200000u,
    HL_LINUX_O_PATH = 010000000u,
    HL_LINUX_O_CLOEXEC = 02000000u,
    HL_LINUX_FD_CLOEXEC = 1u
};

enum {
    HL_LINUX_SEEK_SET = 0,
    HL_LINUX_SEEK_CUR = 1,
    HL_LINUX_SEEK_END = 2,
    HL_LINUX_SEEK_DATA = 3,
    HL_LINUX_SEEK_HOLE = 4,
    HL_LINUX_F_DUPFD = 0,
    HL_LINUX_F_GETFD = 1,
    HL_LINUX_F_SETFD = 2,
    HL_LINUX_F_GETFL = 3,
    HL_LINUX_F_SETFL = 4,
    HL_LINUX_F_DUPFD_CLOEXEC = 1030
};

enum {
    HL_LINUX_S_IFIFO = 0010000u,
    HL_LINUX_S_IFCHR = 0020000u,
    HL_LINUX_S_IFDIR = 0040000u,
    HL_LINUX_S_IFBLK = 0060000u,
    HL_LINUX_S_IFREG = 0100000u,
    HL_LINUX_S_IFLNK = 0120000u,
    HL_LINUX_S_IFSOCK = 0140000u
};

#define HL_LINUX_AT_FDCWD (-100)

typedef struct hl_linux_ofd_entry {
    /* Host object owned by this OFD. Closed when the final descriptor is closed. */
    hl_host_handle host_handle;
    /* Linux open-file-description offset, shared by dup'd descriptors. */
    uint64_t offset;
    /* Open status flags belong to the OFD; descriptor flags do not. */
    uint32_t status_flags;
    uint32_t references;
    /* Operations pin the entry while table_lock is dropped around host calls. */
    uint32_t active_operations;
    /* Final close claimed this slot; allocation cannot recycle it yet. */
    uint32_t closing;
    uint32_t generation;
    uint32_t kind;
    /* Stable Linux open-file-description identity: preserved by dup/fork, renewed on allocation. */
    uint64_t flock_token;
    /* Serializes the shared offset and final close for this OFD only. */
    hl_host_handle io_mutex;
    /* Private typed-object adapter. NULL denotes the ordinary host-file adapter. */
    const struct hl_linux_object_ops *object_ops;
    void *object_context;
} hl_linux_ofd_entry;

typedef struct hl_linux_fd_entry {
    /* Index zero means unused; live descriptors refer to one shared OFD. */
    hl_linux_ofd ofd;
    /* Per-descriptor flags such as FD_CLOEXEC. */
    uint32_t descriptor_flags;
    uint32_t generation;
} hl_linux_fd_entry;

typedef struct hl_linux_fd_reservation {
    hl_linux_fd fd;
    uint32_t generation;
} hl_linux_fd_reservation;

/* Copyable descriptor metadata captured atomically under table ownership. */
typedef struct hl_linux_fd_snapshot {
    hl_linux_fd fd;
    hl_linux_ofd ofd;
    hl_host_handle host_handle;
    uint64_t offset;
    uint32_t status_flags;
    uint32_t descriptor_flags;
    uint32_t descriptor_generation;
    uint32_t ofd_generation;
    uint32_t descriptor_references;
    uint32_t kind;
    uint64_t flock_token;
} hl_linux_fd_snapshot;

/* Host-neutral values which the syscall marshaller can encode for either Linux guest ISA. */
typedef struct hl_linux_file_status {
    uint64_t device;
    uint64_t object;
    uint64_t size;
    uint64_t blocks_512;
    uint64_t modified_ns;
    uint64_t accessed_ns;
    uint64_t changed_ns;
    uint64_t created_ns;
    uint64_t special_device;
    uint64_t link_count;
    uint32_t mode;
    uint32_t user;
    uint32_t group;
} hl_linux_file_status;

typedef struct hl_linux_fork_record {
    uint32_t ofd;
    uint32_t generation;
    hl_host_handle parent_handle;
    hl_host_handle child_handle;
    hl_host_handle child_mutex;
    const struct hl_linux_object_ops *object_ops;
    void *parent_context;
    void *child_context;
    uint32_t snapshot_pin;
} hl_linux_fork_record;

typedef struct hl_linux_fork_plan {
    HL_ABI_HEADER;
    hl_linux_fork_record *records;
    uint32_t capacity;
    uint32_t count;
    uint32_t armed;
    uint32_t host_completed;
} hl_linux_fork_plan;

enum { HL_LINUX_STAT_AARCH64_SIZE = 128, HL_LINUX_STAT_X86_64_SIZE = 144 };

typedef struct hl_linux_abi {
    HL_ABI_HEADER;
    const hl_host_services *host;
    hl_linux_fd_entry *fds;
    uint32_t fd_capacity;
    hl_linux_ofd_entry *ofds;
    uint32_t ofd_capacity;
    /* Monotonic high-water mark: one past the highest OFD index ever allocated (0 when none). Every live
     * OFD index is < ofd_watermark, so fork_prepare's live-OFD snapshot scans [1, ofd_watermark) instead of
     * the whole ofd_capacity table -- a fork-heavy guest whose live descriptor set is tiny no longer pays a
     * full 65536-slot scan per fork. Maintained solely in hl_linux_find_ofd (the one allocation choke point);
     * never decreases, so a since-closed slot only makes the bound a harmless over-estimate. */
    uint32_t ofd_watermark;
    /* Count of fds currently in the transient RESERVED state (reserve_at .. commit/cancel). fork_prepare must
     * fail BUSY while any descriptor install is mid-flight; this replaces a full fd_capacity scan for RESERVED
     * with an O(1) check. Maintained under table_lock at the three reserve/cancel/commit transitions. */
    uint32_t reserved_fds;
    /* Private, dynamically sized virtual-mapping ledger owned by this instance. */
    void *vma_state;
    /*
     * Serializes only descriptor lookup and OFD lifetime counters. Host calls
     * never hold it; each OFD has independent I/O ownership instead.
     */
    atomic_flag table_lock;
} hl_linux_abi;

/*
 * Each live OFD owns one host mutex; unused capacity owns no host resources.
 * An initialized hl_linux_abi must not be copied. Host service callbacks must not re-enter the same
 * hl_linux_abi instance. Contending operations use host synchronization;
 * unrelated OFDs never wait for one another's host I/O.
 */

HL_API hl_status hl_linux_abi_init(hl_linux_abi *linux_abi, const hl_host_services *host, hl_linux_fd_entry *fd_storage,
                                   uint32_t fd_capacity, hl_linux_ofd_entry *ofd_storage, uint32_t ofd_capacity);
/*
 * Requires every descriptor to be closed and releases the per-OFD host mutexes.
 * The owner must externally exclude every concurrent call on this instance from
 * destroy's entry onward; after success the instance may only be initialized.
 */
HL_API hl_status hl_linux_abi_destroy(hl_linux_abi *linux_abi);
/* Caller must quiesce every concurrent operation on linux_abi until parent/child completion. */
HL_API hl_status hl_linux_abi_fork_prepare(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan);
/* Marks that process.spawn_prepared already completed the inherited host fork bracket. */
HL_API hl_status hl_linux_abi_fork_host_completed(hl_linux_fork_plan *plan);
HL_API hl_status hl_linux_abi_fork_parent(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan);
HL_API hl_status hl_linux_abi_fork_child(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan);
/*
 * Spawn an entry in a host-cloned process while preserving this Linux fd table.
 * The operation owns the fork plan and completes the host fork bracket in both
 * branches. On success, out_process is an opaque host process handle.
 */
HL_API hl_status hl_linux_abi_spawn(hl_linux_abi *linux_abi, hl_host_process_entry entry, void *entry_context,
                                    hl_host_handle *out_process);
HL_API hl_status hl_linux_fd_install(hl_linux_abi *linux_abi, hl_host_handle host_handle, uint32_t status_flags,
                                     uint32_t descriptor_flags, hl_linux_fd *out_fd);
/* Installs only at the requested vacant guest descriptor; never exposes or duplicates a native descriptor. */
HL_API hl_status hl_linux_fd_install_at(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_host_handle host_handle,
                                        uint32_t status_flags, uint32_t descriptor_flags);
HL_API hl_status hl_linux_fd_reserve_at(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_fd_reservation *reservation);
HL_API hl_status hl_linux_fd_cancel(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation);
HL_API hl_status hl_linux_fd_dup(hl_linux_abi *linux_abi, hl_linux_fd source, uint32_t descriptor_flags,
                                 hl_linux_fd *out_fd);
HL_API hl_status hl_linux_fd_close(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_host_handle *last_host_handle);
/*
 * Apply Linux close-on-exec policy to one descriptor. The descriptor table is
 * authoritative: out_closed is one only when this call removed fd. A final OFD
 * reference also closes its host file handle through the host-service seam.
 */
HL_API hl_status hl_linux_fd_exec(hl_linux_abi *linux_abi, hl_linux_fd fd, uint32_t *out_closed);
typedef void (*hl_linux_fd_exec_callback)(void *context, hl_linux_fd fd);
/*
 * Remove every close-on-exec descriptor without scanning through host-native fd numbers.
 * The table lock is released before host close and callback; failures do not stop later removals.
 */
HL_API hl_status hl_linux_fd_exec_all(hl_linux_abi *linux_abi, hl_linux_fd_exec_callback callback, void *context,
                                      uint32_t *out_closed);
/* Quiescent-state diagnostic for descriptor/OFD ownership invariants. */
HL_API hl_status hl_linux_abi_validate_fds(const hl_linux_abi *linux_abi);
/* Returns a stable value snapshot; internal entries and mutex-bearing OFDs never escape. */
HL_API hl_status hl_linux_fd_snapshot_get(const hl_linux_abi *linux_abi, hl_linux_fd fd,
                                          hl_linux_fd_snapshot *snapshot);
/*
 * Map an ordinary typed file while its open-file-description is pinned. Closing
 * the guest descriptor concurrently cannot retire the opaque host file until
 * map_file has returned. The returned mapping owns its independent host handle.
 */
HL_API int64_t hl_linux_map_file(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t address, uint64_t offset,
                                 uint64_t size, uint32_t protection, uint32_t flags, hl_host_file_mapping *mapping);

/*
 * Host-agnostic Linux file-I/O syscall semantics.
 *
 * Buffers are already validated/translated guest memory owned by the caller. The
 * library resolves the guest fd to its OFD, calls only hl_host_file_services,
 * translates hl_status to a negative Linux errno, and applies Linux offset rules.
 * read() advances the shared OFD offset only on success; pread64() never changes it.
 */
HL_API int64_t hl_linux_read(hl_linux_abi *linux_abi, hl_linux_fd fd, void *buffer, size_t size);
HL_API int64_t hl_linux_pread64(hl_linux_abi *linux_abi, hl_linux_fd fd, void *buffer, size_t size, uint64_t offset);
HL_API int64_t hl_linux_write(hl_linux_abi *linux_abi, hl_linux_fd fd, const void *buffer, size_t size);
HL_API int64_t hl_linux_pwrite64(hl_linux_abi *linux_abi, hl_linux_fd fd, const void *buffer, size_t size,
                                 uint64_t offset);
HL_API int64_t hl_linux_readv(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count);
HL_API int64_t hl_linux_writev(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count);
HL_API int64_t hl_linux_preadv(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count,
                               uint64_t offset);
HL_API int64_t hl_linux_pwritev(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count,
                                uint64_t offset);
HL_API int64_t hl_linux_ftruncate(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t size);
HL_API int64_t hl_linux_fsync(hl_linux_abi *linux_abi, hl_linux_fd fd);
HL_API int64_t hl_linux_sync_range(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t offset, uint64_t size,
                                   uint32_t flags);
HL_API int64_t hl_linux_sync_filesystem(hl_linux_abi *linux_abi, hl_linux_fd fd);
HL_API int64_t hl_linux_fdatasync(hl_linux_abi *linux_abi, hl_linux_fd fd);
/* path is translated guest memory; mode is used only with O_CREAT. */
HL_API int64_t hl_linux_openat(hl_linux_abi *linux_abi, int32_t directory_fd, const char *path, size_t path_size,
                               uint32_t flags, uint32_t mode);
/* Opens through directory_fd and publishes only at the requested vacant descriptor. */
HL_API int64_t hl_linux_openat_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                        int32_t directory_fd, const char *path, size_t path_size, uint32_t flags,
                                        uint32_t mode);
/* Opens relative to an opaque host directory without exposing or adopting its native descriptor. */
HL_API int64_t hl_linux_openat_handle_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                               hl_host_handle directory, const char *path, size_t path_size,
                                               uint32_t flags, uint32_t mode);
/* Publish an already opened opaque host file at a reserved guest descriptor.
 * Ownership transfers only on success. */
HL_API int64_t hl_linux_file_adopt_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                            hl_host_handle file, uint32_t flags);
/* close() invalidates this descriptor even if the host reports a late close error. */
HL_API int64_t hl_linux_close(hl_linux_abi *linux_abi, hl_linux_fd fd);
/* dup and supported fcntl commands operate only on the guest fd/OFD tables. */
HL_API int64_t hl_linux_dup(hl_linux_abi *linux_abi, hl_linux_fd fd);
HL_API int64_t hl_linux_dup2(hl_linux_abi *linux_abi, hl_linux_fd source, hl_linux_fd target);
/* flags accepts only Linux O_CLOEXEC; source == target is EINVAL as on Linux. */
HL_API int64_t hl_linux_dup3(hl_linux_abi *linux_abi, hl_linux_fd source, hl_linux_fd target, uint32_t flags);
HL_API int64_t hl_linux_fcntl(hl_linux_abi *linux_abi, hl_linux_fd fd, int32_t command, uint64_t argument);
/* lseek changes the shared OFD offset. SEEK_END and fstat use normalized host metadata. */
HL_API int64_t hl_linux_lseek(hl_linux_abi *linux_abi, hl_linux_fd fd, int64_t offset, int32_t whence);
HL_API int64_t hl_linux_fstat(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_file_status *output);

/* Encode host-neutral metadata into exact little-endian Linux stat records; unavailable fields are zero. */
HL_API int64_t hl_linux_stat_aarch64(const hl_linux_file_status *status, void *output, size_t output_size);
HL_API int64_t hl_linux_stat_x86_64(const hl_linux_file_status *status, void *output, size_t output_size);

HL_EXTERN_C_END

#endif
