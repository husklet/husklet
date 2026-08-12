/*
 * The file group: 41 callbacks over NtCreateFile and the pinned-root resolver
 * in ntpath.c.
 *
 * The shape of this file follows four decisions, all of them measured on this
 * host before they were coded:
 *
 *   1. Every open goes through hl_windows_open_child, and therefore through
 *      FILE_SHARE_DELETE. POSIX unlink-while-open exists on Windows only when no
 *      opener has excluded delete sharing, and this group being the single door
 *      into the filesystem is what makes that enforceable rather than hoped for.
 *   2. Deletion is FILE_DISPOSITION_POSIX_SEMANTICS on a handle opened purely to
 *      delete, and that handle is closed immediately. The name vanishes when the
 *      *deleting* handle closes, not when the request is made -- so holding it
 *      open would leave the name in a delete-pending state that rejects both
 *      reopen and recreate, which is not what unlink(2) does.
 *   3. Appends use FILE_APPEND_DATA with the FILE_WRITE_TO_END_OF_FILE offset.
 *      Two handles interleaving appends produced exactly the concatenation of
 *      their writes, so the atomicity the contract demands is the kernel's and
 *      needs no re-open indirection of the kind the Linux backend carries.
 *   4. File objects are synchronous (FILE_SYNCHRONOUS_IO_NONALERT), so the
 *      kernel owns the file position and DuplicateHandle shares it -- which is
 *      what makes clone_for_fork behave like dup(2). The cost is that an
 *      explicit-offset NtReadFile *also* moves that position (measured: reading
 *      3 bytes at offset 0 left the position at 3), so every positioned call
 *      brackets itself with a save and restore under the host lock.
 *
 * Guest ownership (uid/gid) and guest permission bits are not stored. Windows
 * has SIDs and ACLs, there is no total mapping between the two models, and
 * inventing one here would put a policy decision in the wrong layer -- so the
 * metadata below synthesises mode bits from the read-only attribute and reports
 * owner 0, and set_owner accepts only the "change nothing" encoding.
 */
#include "internal.h"

#include <winioctl.h>

#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

#define HL_NT_SUCCESS ((NTSTATUS)0x00000000L)
#define HL_NT_NO_MORE_FILES ((NTSTATUS)0x80000006L)
#define HL_NT_BUFFER_OVERFLOW ((NTSTATUS)0x80000005L)
#define HL_NT_END_OF_FILE ((NTSTATUS)0xC0000011L)
#define HL_NT_ACCESS_DENIED ((NTSTATUS)0xC0000022L)
#define HL_NT_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#define HL_NT_INVALID_INFO_CLASS ((NTSTATUS)0xC0000003L)
#define HL_NT_INVALID_DEVICE_REQUEST ((NTSTATUS)0xC0000010L)
#define HL_NT_NOT_IMPLEMENTED ((NTSTATUS)0xC0000002L)
#define HL_NT_OBJECT_NAME_NOT_FOUND ((NTSTATUS)0xC0000034L)
#define HL_NT_INFO_LENGTH_MISMATCH ((NTSTATUS)0xC0000004L)
#define HL_NT_BUFFER_TOO_SMALL ((NTSTATUS)0xC0000023L)

#define HL_NT_FILE_DISPOSITION_INFORMATION_EX ((FILE_INFORMATION_CLASS)64)
#define HL_NT_FILE_RENAME_INFORMATION_EX ((FILE_INFORMATION_CLASS)65)

enum {
    HL_NT_DISPOSITION_DELETE = 0x1u,
    HL_NT_DISPOSITION_POSIX = 0x2u,
    HL_NT_DISPOSITION_IGNORE_READONLY = 0x10u,
    HL_NT_RENAME_REPLACE_IF_EXISTS = 0x1u,
    HL_NT_RENAME_POSIX = 0x2u
};

/* 1601-01-01 to 1970-01-01 in 100 ns ticks: the whole of the FILETIME/POSIX
 * epoch difference, and the only place it appears. */
#define HL_WINDOWS_EPOCH_TICKS INT64_C(116444736000000000)
/* One transfer's ceiling. Linux caps read(2)/write(2) at 0x7ffff000 and returns
 * a short count; NtReadFile takes a ULONG, so the same cap is honest here. */
#define HL_WINDOWS_IO_MAX UINT64_C(0x7ffff000)

/* --- small shared helpers --------------------------------------------------- */

static uint64_t hl_windows_time_to_ns(LARGE_INTEGER value) {
    int64_t ticks = value.QuadPart - HL_WINDOWS_EPOCH_TICKS;
    return ticks <= 0 ? 0u : (uint64_t)ticks * 100u;
}

static LARGE_INTEGER hl_windows_time_from_parts(int64_t seconds, uint32_t nanoseconds) {
    LARGE_INTEGER value;
    value.QuadPart = seconds * INT64_C(10000000) + (int64_t)(nanoseconds / 100u) + HL_WINDOWS_EPOCH_TICKS;
    return value;
}

/*
 * Take the native handle out of a file slot. The handle is used after the lock
 * is dropped, which is the same trade the POSIX backends make with their
 * descriptors: serialising every read behind one host lock would be worse than
 * the residual close-during-use race, and the race is the caller's to avoid.
 */
static int hl_windows_file_borrow(hl_host_windows *host, hl_host_handle file, HANDLE *object, uint32_t *access,
                                  uint32_t *state) {
    hl_windows_handle_entry *entry;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, file, HL_WINDOWS_HANDLE_FILE);
    if (entry != NULL) {
        if (object != NULL) *object = entry->object;
        if (access != NULL) *access = entry->file_access;
        if (state != NULL) *state = entry->file_state;
    }
    hl_windows_unlock(host);
    return entry != NULL;
}

/* An owned directory handle for a path operation's anchor. HL_HOST_HANDLE_CWD
 * opens the process working directory; anything else is duplicated, so the
 * resolver cannot be left holding a handle a concurrent close retired. */
static hl_host_result hl_windows_directory_for(hl_host_windows *host, hl_host_handle directory, HANDLE *out) {
    HANDLE object = NULL;
    HANDLE copy = NULL;
    *out = NULL;
    if (directory == HL_HOST_HANDLE_CWD) return hl_windows_open_working_directory(host, out);
    if (!hl_windows_file_borrow(host, directory, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!DuplicateHandle(GetCurrentProcess(), object, GetCurrentProcess(), &copy, 0, FALSE, DUPLICATE_SAME_ACCESS))
        return hl_windows_last_error_result();
    *out = copy;
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_file_register(hl_host_windows *host, HANDLE object, uint32_t access, uint32_t state) {
    hl_host_result registered = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_FILE);
    hl_windows_handle_entry *entry;
    if (registered.status != HL_STATUS_OK) return registered;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, registered.value, HL_WINDOWS_HANDLE_FILE);
    if (entry == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    entry->object = object;
    entry->file_access = access;
    entry->file_state = state;
    entry->file_cursor = 0;
    hl_windows_unlock(host);
    return registered;
}

static void hl_windows_file_release(hl_host_windows *host, hl_host_handle handle) {
    hl_windows_handle_entry *entry;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_FILE);
    if (entry != NULL) {
        if (entry->object != NULL) CloseHandle(entry->object);
        hl_windows_clear_entry_locked(entry);
    }
    hl_windows_unlock(host);
}

/*
 * HL_HOST_FILE_* to an NT access mask. FILE_TRAVERSE and FILE_READ_ATTRIBUTES
 * are unconditional: the first is what lets a directory handle serve as an
 * OBJECT_ATTRIBUTES root (the *at-family dirfd of this backend) and the second
 * is what makes PATH_ONLY handles answer metadata, which is the whole point of
 * O_PATH.
 */
static ACCESS_MASK hl_windows_desired_access(uint32_t access) {
    ACCESS_MASK mask = FILE_READ_ATTRIBUTES | FILE_TRAVERSE;
    /* PATH_ONLY still asks for FILE_READ_DATA, and this is not a widening of
     * O_PATH for its own sake: on Windows "is this a symlink" is a question
     * about file *contents*, so a handle with no read access cannot answer the
     * one metadata field that matters most. The open falls back to the bare
     * mask when a file denies reads, so nothing that used to open still fails. */
    if ((access & HL_HOST_FILE_PATH_ONLY) != 0) return mask | FILE_READ_DATA;
    if ((access & HL_HOST_FILE_READ) != 0) mask |= FILE_READ_DATA | FILE_READ_EA;
    if ((access & HL_HOST_FILE_WRITE) != 0) mask |= FILE_WRITE_DATA | FILE_WRITE_EA | FILE_WRITE_ATTRIBUTES;
    if ((access & HL_HOST_FILE_APPEND) != 0) mask |= FILE_APPEND_DATA | FILE_WRITE_ATTRIBUTES;
    if ((access & HL_HOST_FILE_DIRECTORY) != 0) mask |= FILE_LIST_DIRECTORY;
    return mask;
}

static ULONG hl_windows_disposition(uint32_t creation) {
    const int create = (creation & HL_HOST_FILE_CREATE) != 0;
    const int exclusive = (creation & HL_HOST_FILE_EXCLUSIVE) != 0;
    const int truncate = (creation & HL_HOST_FILE_TRUNCATE) != 0;
    if (create && exclusive) return FILE_CREATE;
    if (create && truncate) return FILE_OVERWRITE_IF;
    if (create) return FILE_OPEN_IF;
    if (truncate) return FILE_OVERWRITE;
    return FILE_OPEN;
}

static ULONG hl_windows_open_options(uint32_t access, uint32_t creation) {
    ULONG options = 0;
    if ((access & HL_HOST_FILE_DIRECTORY) != 0)
        options |= FILE_DIRECTORY_FILE;
    else if (creation != 0 || (access & (HL_HOST_FILE_WRITE | HL_HOST_FILE_APPEND)) != 0)
        /* Linux refuses write access to a directory with EISDIR and refuses to
         * create over one; FILE_NON_DIRECTORY_FILE is how NT says the same. */
        options |= FILE_NON_DIRECTORY_FILE;
    if ((access & HL_HOST_FILE_NOFOLLOW) != 0) options |= FILE_OPEN_REPARSE_POINT;
    return options;
}

/* --- metadata --------------------------------------------------------------- */

static uint32_t hl_windows_permissions_for(uint32_t attributes, int directory) {
    /* Synthesised, and deliberately coarse. The read-only attribute is the only
     * Windows fact that carries over; execute is meaningless for a Windows file
     * and is reported on directories, where it means "searchable", and on
     * symlinks, which Linux always reports as 0777. */
    const uint32_t writable = (attributes & FILE_ATTRIBUTE_READONLY) != 0 ? 0u : 0222u;
    return directory ? 0555u | writable : 0444u | writable;
}

static hl_host_result hl_windows_metadata_for(hl_host_windows *host, HANDLE object, uint32_t state,
                                              hl_host_file_metadata *output) {
    unsigned char raw[sizeof(FILE_ALL_INFORMATION) + 1024];
    unsigned char volume_raw[sizeof(FILE_FS_VOLUME_INFORMATION) + 256];
    FILE_ALL_INFORMATION *all = (FILE_ALL_INFORMATION *)(void *)raw;
    FILE_FS_VOLUME_INFORMATION *volume = (FILE_FS_VOLUME_INFORMATION *)(void *)volume_raw;
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    uint32_t attributes;
    int directory;
    memset(raw, 0, sizeof(raw));
    memset(volume_raw, 0, sizeof(volume_raw));
    status = host->nt.query_information_file(object, &status_block, all, (ULONG)sizeof(raw), FileAllInformation);
    /* The trailing name is variable length, so a short buffer is expected and
     * only the fixed head is consumed. */
    if (status != HL_NT_SUCCESS && status != HL_NT_BUFFER_OVERFLOW) return hl_windows_nt_result(host, status);
    attributes = (uint32_t)all->BasicInformation.FileAttributes;
    directory = all->StandardInformation.Directory != 0;
    output->size = (uint64_t)all->StandardInformation.EndOfFile.QuadPart;
    output->allocated_size = (uint64_t)all->StandardInformation.AllocationSize.QuadPart;
    output->modified_ns = hl_windows_time_to_ns(all->BasicInformation.LastWriteTime);
    output->accessed_ns = hl_windows_time_to_ns(all->BasicInformation.LastAccessTime);
    output->changed_ns = hl_windows_time_to_ns(all->BasicInformation.ChangeTime);
    output->created_ns = hl_windows_time_to_ns(all->BasicInformation.CreationTime);
    output->link_count = all->StandardInformation.NumberOfLinks;
    /* The full 64-bit NTFS file reference, not a fold of it: the low 48 bits are
     * the MFT record and the high 16 the reuse sequence, and dropping either
     * half turns two distinct files into one identity. Measured stable across
     * reopen and rename, and shared by hard links. */
    output->stable_object = (uint64_t)all->InternalInformation.IndexNumber.QuadPart;
    output->permissions = hl_windows_permissions_for(attributes, directory);
    output->user = 0;
    output->group = 0;
    output->device = 0;
    if (host->nt.query_volume_information_file(object, &status_block, volume, (ULONG)sizeof(volume_raw),
                                               FileFsVolumeInformation) == HL_NT_SUCCESS)
        output->stable_device = volume->VolumeSerialNumber;

    if (directory) {
        output->type = HL_HOST_FILE_TYPE_DIRECTORY;
    } else if ((state & HL_WINDOWS_FILE_PATH_BACKED) == 0) {
        /* A duplicated standard stream has no filesystem identity; report what
         * the object actually is so a guest fstat can tell a console from a
         * pipe from a redirected file. */
        const DWORD kind = GetFileType(object);
        output->type = kind == FILE_TYPE_CHAR   ? (uint32_t)HL_HOST_FILE_TYPE_CHARACTER
                       : kind == FILE_TYPE_PIPE ? (uint32_t)HL_HOST_FILE_TYPE_FIFO
                       : kind == FILE_TYPE_DISK ? (uint32_t)HL_HOST_FILE_TYPE_REGULAR
                                                : (uint32_t)HL_HOST_FILE_TYPE_UNKNOWN;
    } else if (hl_windows_symlink_candidate(attributes, output->size) &&
               hl_windows_symlink_read(&host->nt, object, NULL, 0, NULL)) {
        output->type = HL_HOST_FILE_TYPE_SYMLINK;
        /* Linux reports a symlink's size as the length of its target. */
        output->size -= (uint64_t)HL_WINDOWS_SYMLINK_MAGIC_SIZE;
        output->permissions = 0777u;
    } else {
        output->type = HL_HOST_FILE_TYPE_REGULAR;
    }
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_file_metadata(void *context, hl_host_handle file, hl_host_file_metadata *output) {
    hl_host_windows *host = context;
    HANDLE object = NULL;
    uint32_t state = 0;
    if (output == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(output, 0, sizeof(*output));
    if (!hl_windows_file_borrow(host, file, &object, NULL, &state))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_metadata_for(host, object, state, output);
}

/* --- open ------------------------------------------------------------------- */

static hl_host_result hl_windows_open_resolved(hl_host_windows *host, const hl_windows_resolution *resolved,
                                               uint32_t access, uint32_t creation, uint32_t permissions) {
    HANDLE object = NULL;
    /* Windows has no mode bits at create time. The only guest bit with a native
     * counterpart is write permission, which becomes the read-only attribute;
     * the rest are the Linux front end's to virtualise. */
    const ULONG attributes =
        (permissions & 0222u) == 0u ? (ULONG)FILE_ATTRIBUTE_READONLY : (ULONG)FILE_ATTRIBUTE_NORMAL;
    NTSTATUS status = hl_windows_open_child(
        &host->nt, resolved->parent, resolved->leaf, resolved->leaf_length, hl_windows_desired_access(access),
        attributes, hl_windows_disposition(creation), hl_windows_open_options(access, creation), &object);
    hl_host_result result;
    uint32_t state = HL_WINDOWS_FILE_PATH_BACKED;
    if (status == HL_NT_ACCESS_DENIED && (access & HL_HOST_FILE_PATH_ONLY) != 0)
        status = hl_windows_open_child(
            &host->nt, resolved->parent, resolved->leaf, resolved->leaf_length, FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
            attributes, hl_windows_disposition(creation), hl_windows_open_options(access, creation), &object);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    if ((access & HL_HOST_FILE_DIRECTORY) != 0) state |= HL_WINDOWS_FILE_DIRECTORY;
    result = hl_windows_file_register(host, object, access, state);
    if (result.status != HL_STATUS_OK) CloseHandle(object);
    return result;
}

static hl_host_result hl_windows_file_open(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                           uint32_t access, uint32_t creation, uint32_t permissions) {
    hl_host_windows *host = context;
    hl_windows_resolution resolved;
    hl_host_result result;
    HANDLE root = NULL;
    uint32_t policy = HL_WINDOWS_RESOLVE_ESCAPE | HL_HOST_RESOLVE_ALLOW_MISSING;
    if (path == NULL || path_size == 0 || (permissions & ~07777u) != 0 ||
        (access & ~(uint32_t)(HL_HOST_FILE_READ | HL_HOST_FILE_WRITE | HL_HOST_FILE_APPEND | HL_HOST_FILE_DIRECTORY |
                              HL_HOST_FILE_NONBLOCK | HL_HOST_FILE_NOFOLLOW | HL_HOST_FILE_PATH_ONLY)) != 0 ||
        (creation & ~(uint32_t)(HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE | HL_HOST_FILE_TRUNCATE)) != 0 ||
        (access & (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE | HL_HOST_FILE_APPEND | HL_HOST_FILE_PATH_ONLY)) == 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* O_NOFOLLOW, and O_CREAT|O_EXCL, both stop at the link node itself. */
    if ((access & HL_HOST_FILE_NOFOLLOW) != 0 ||
        (creation & (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE)) == (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE))
        policy |= HL_HOST_RESOLVE_NOFOLLOW_FINAL;
    result = hl_windows_directory_for(host, directory, &root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(host, root, path, path_size, policy, &resolved);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_open_resolved(host, &resolved, access, creation, permissions);
    CloseHandle(resolved.parent);
    return result;
}

static hl_host_result hl_windows_file_open_beneath(void *context, hl_host_handle root, const char *path,
                                                   size_t path_size, uint32_t access, uint32_t creation,
                                                   uint32_t permissions, uint32_t policy) {
    hl_host_windows *host = context;
    hl_windows_resolution resolved;
    hl_host_result result;
    HANDLE anchor = NULL;
    if (path == NULL || path_size == 0 || path[0] == '/' || path[0] == '\\' || memchr(path, '\0', path_size) != NULL ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS)) != 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* A drive-qualified spelling is an absolute path wearing a relative shape,
     * and resolve_path would honour it. Beneath-a-root must not. */
    if (path_size >= 2 && path[1] == ':') return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    policy |= HL_HOST_RESOLVE_ALLOW_MISSING;
    if ((creation & (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE)) == (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE))
        policy |= HL_HOST_RESOLVE_NOFOLLOW_FINAL;
    result = hl_windows_directory_for(host, root, &anchor);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(host, anchor, path, path_size, policy, &resolved);
    CloseHandle(anchor);
    if (result.status != HL_STATUS_OK) return result;
    /* The final open is always NOFOLLOW even when the walk followed links: the
     * walk already resolved them, and re-following here would reopen the race
     * the containment exists to close. */
    result = hl_windows_open_resolved(host, &resolved, access | HL_HOST_FILE_NOFOLLOW, creation, permissions);
    CloseHandle(resolved.parent);
    return result;
}

static hl_host_result hl_windows_file_resolve_beneath(void *context, hl_host_handle root, const char *path,
                                                      size_t path_size, uint32_t policy,
                                                      hl_host_file_resolution *output) {
    hl_host_windows *host = context;
    hl_windows_resolution resolved;
    hl_host_result result;
    hl_host_result parent_handle;
    hl_host_result target_handle = hl_windows_result(HL_STATUS_OK, HL_HOST_HANDLE_INVALID, 0);
    HANDLE anchor = NULL;
    HANDLE target = NULL;
    size_t final_size = 0;
    if (output == NULL || path == NULL || path_size == 0 || path[0] == '/' || path[0] == '\\' ||
        memchr(path, '\0', path_size) != NULL ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS |
                              HL_HOST_RESOLVE_ALLOW_MISSING)) != 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (path_size >= 2 && path[1] == ':') return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_windows_directory_for(host, root, &anchor);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(host, anchor, path, path_size, policy, &resolved);
    CloseHandle(anchor);
    if (result.status != HL_STATUS_OK) return result;

    memset(output, 0, sizeof(*output));
    if (resolved.leaf_length == 0) {
        /* The path named the pinned directory itself. */
        output->final[0] = '.';
        final_size = 1;
    } else {
        const hl_status converted = hl_windows_utf8_from_wide(resolved.leaf, resolved.leaf_length, output->final,
                                                              sizeof(output->final) - 1u, &final_size);
        if (converted != HL_STATUS_OK) {
            CloseHandle(resolved.parent);
            return hl_windows_result(converted, 0, 0);
        }
    }
    output->final[final_size] = '\0';
    output->final_size = final_size;

    /* Resolution must not participate in special-file I/O, so the target is
     * pinned for metadata only -- FILE_READ_ATTRIBUTES is this backend's O_PATH. */
    if ((policy & HL_HOST_RESOLVE_ALLOW_MISSING) == 0) {
        const NTSTATUS status = hl_windows_open_child(&host->nt, resolved.parent, resolved.leaf, resolved.leaf_length,
                                                      FILE_READ_ATTRIBUTES | FILE_READ_DATA | FILE_TRAVERSE, 0,
                                                      FILE_OPEN, FILE_OPEN_REPARSE_POINT, &target);
        if (status != HL_NT_SUCCESS) {
            CloseHandle(resolved.parent);
            return hl_windows_nt_result(host, status);
        }
    }
    parent_handle = hl_windows_file_register(host, resolved.parent, HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_DIRECTORY,
                                             HL_WINDOWS_FILE_PATH_BACKED | HL_WINDOWS_FILE_DIRECTORY);
    if (parent_handle.status != HL_STATUS_OK) {
        CloseHandle(resolved.parent);
        if (target != NULL) CloseHandle(target);
        return parent_handle;
    }
    if (target != NULL) {
        target_handle = hl_windows_file_register(host, target, HL_HOST_FILE_PATH_ONLY, HL_WINDOWS_FILE_PATH_BACKED);
        if (target_handle.status != HL_STATUS_OK) {
            CloseHandle(target);
            hl_windows_file_release(host, parent_handle.value);
            return target_handle;
        }
    }
    output->parent = parent_handle.value;
    output->target = target_handle.value;
    if (target != NULL) {
        hl_host_file_metadata metadata;
        memset(&metadata, 0, sizeof(metadata));
        if (hl_windows_metadata_for(host, target, HL_WINDOWS_FILE_PATH_BACKED, &metadata).status == HL_STATUS_OK)
            output->target_type = metadata.type;
    }
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* --- reads and writes ------------------------------------------------------- */

static ULONG hl_windows_transfer_size(uint64_t size) {
    return size > HL_WINDOWS_IO_MAX ? (ULONG)HL_WINDOWS_IO_MAX : (ULONG)size;
}

/*
 * A positioned transfer. The save/restore bracket is what makes this pread and
 * pwrite rather than "seek then read": on a synchronous file object NtReadFile
 * updates CurrentByteOffset even when an explicit offset is supplied. It is
 * taken under the host lock so that two positioned calls on one handle cannot
 * interleave their save and restore; sequential read/write take no lock at all,
 * because the kernel already serialises those on the file object.
 */
static hl_host_result hl_windows_file_positioned(hl_host_windows *host, hl_host_handle file, uint64_t offset,
                                                 void *buffer, uint64_t size, int writing) {
    hl_windows_handle_entry *entry;
    FILE_POSITION_INFORMATION saved;
    IO_STATUS_BLOCK status_block;
    LARGE_INTEGER where;
    NTSTATUS status;
    HANDLE object;
    uint64_t moved;
    int restore;
    if (offset > (uint64_t)INT64_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    where.QuadPart = (LONGLONG)offset;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, file, HL_WINDOWS_HANDLE_FILE);
    if (entry == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object = entry->object;
    restore = host->nt.query_information_file(object, &status_block, &saved, (ULONG)sizeof(saved),
                                              FilePositionInformation) == HL_NT_SUCCESS;
    status = writing ? host->nt.write_file(object, NULL, NULL, NULL, &status_block, buffer,
                                           hl_windows_transfer_size(size), &where, NULL)
                     : host->nt.read_file(object, NULL, NULL, NULL, &status_block, buffer,
                                          hl_windows_transfer_size(size), &where, NULL);
    /* Read the transferred count before the restore reuses the status block. */
    moved = (uint64_t)status_block.Information;
    if (restore) {
        IO_STATUS_BLOCK restored;
        (void)host->nt.set_information_file(object, &restored, &saved, (ULONG)sizeof(saved), FilePositionInformation);
    }
    hl_windows_unlock(host);
    if (status == HL_NT_END_OF_FILE) return hl_windows_result(HL_STATUS_OK, 0, 0);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    return hl_windows_result(HL_STATUS_OK, moved, 0);
}

static hl_host_result hl_windows_file_read_at(void *context, hl_host_handle file, uint64_t offset,
                                              hl_host_bytes output) {
    if (output.size != 0 && output.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_file_positioned(context, file, offset, output.data, output.size, 0);
}

static hl_host_result hl_windows_file_write_at(void *context, hl_host_handle file, uint64_t offset,
                                               hl_host_const_bytes input) {
    if (input.size != 0 && input.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* The cast drops const only to reach one call site; NtWriteFile does not
     * write through it. */
    return hl_windows_file_positioned(context, file, offset, (void *)(size_t)input.data, input.size, 1);
}

/* A sequential transfer against the file object's own position. */
static hl_host_result hl_windows_file_sequential(hl_host_windows *host, hl_host_handle file, void *buffer,
                                                 uint64_t size, int writing) {
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    HANDLE object = NULL;
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    status = writing ? host->nt.write_file(object, NULL, NULL, NULL, &status_block, buffer,
                                           hl_windows_transfer_size(size), NULL, NULL)
                     : host->nt.read_file(object, NULL, NULL, NULL, &status_block, buffer,
                                          hl_windows_transfer_size(size), NULL, NULL);
    if (status == HL_NT_END_OF_FILE) return hl_windows_result(HL_STATUS_OK, 0, 0);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    return hl_windows_result(HL_STATUS_OK, (uint64_t)status_block.Information, 0);
}

static hl_host_result hl_windows_file_read(void *context, hl_host_handle file, void *output, uint64_t output_size) {
    if (output_size != 0 && output == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_file_sequential(context, file, output, output_size, 0);
}

static hl_host_result hl_windows_file_write(void *context, hl_host_handle file, const void *input,
                                            uint64_t input_size) {
    if (input_size != 0 && input == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_file_sequential(context, file, (void *)(size_t)input, input_size, 1);
}

/*
 * One indivisible append. The offset is the NT "write to end of file" sentinel,
 * and the handle was opened with FILE_APPEND_DATA, which is what makes the
 * seek-to-end and the write a single filesystem operation. Two handles
 * appending concurrently were measured to concatenate cleanly.
 */
static hl_host_result hl_windows_append_bytes(hl_host_windows *host, hl_host_handle file, const void *data,
                                              uint64_t size) {
    IO_STATUS_BLOCK status_block;
    LARGE_INTEGER where;
    NTSTATUS status;
    HANDLE object = NULL;
    uint32_t access = 0;
    if (!hl_windows_file_borrow(host, file, &object, &access, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((access & HL_HOST_FILE_APPEND) == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    where.HighPart = -1;
    where.LowPart = 0xFFFFFFFFu; /* FILE_WRITE_TO_END_OF_FILE */
    status = host->nt.write_file(object, NULL, NULL, NULL, &status_block, data, hl_windows_transfer_size(size), &where,
                                 NULL);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    return hl_windows_result(HL_STATUS_OK, (uint64_t)status_block.Information, 0);
}

static hl_host_result hl_windows_file_append(void *context, hl_host_handle file, hl_host_const_bytes input) {
    if (input.size != 0 && input.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_append_bytes(context, file, input.data, input.size);
}

/* --- vectored transfers ----------------------------------------------------- */

static hl_status hl_windows_vectors_valid(const hl_host_iovec *vectors, uint32_t count, uint64_t *out_total) {
    uint64_t total = 0;
    uint32_t index;
    if (count > (uint32_t)HL_HOST_FILE_IOV_MAX) return HL_STATUS_INVALID_ARGUMENT;
    if (count != 0 && vectors == NULL) return HL_STATUS_INVALID_ARGUMENT;
    for (index = 0; index < count; ++index) {
        if (vectors[index].size != 0 && vectors[index].address == 0) return HL_STATUS_INVALID_ARGUMENT;
        if (vectors[index].size > UINT64_MAX - total) return HL_STATUS_INVALID_ARGUMENT;
        total += vectors[index].size;
    }
    *out_total = total;
    return HL_STATUS_OK;
}

/*
 * Windows has no scatter/gather equivalent of readv(2) for ordinary files --
 * ReadFileScatter demands page-aligned, unbuffered, whole-page segments -- so
 * these iterate. The loop stops on the first short transfer, which is what a
 * caller must already tolerate from readv on a POSIX host.
 */
static hl_host_result hl_windows_file_vector(hl_host_windows *host, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count, uint64_t offset, int positioned, int writing) {
    uint64_t total = 0;
    uint64_t moved = 0;
    uint32_t index;
    const hl_status valid = hl_windows_vectors_valid(vectors, count, &total);
    if (valid != HL_STATUS_OK) return hl_windows_result(valid, 0, 0);
    for (index = 0; index < count; ++index) {
        void *buffer = (void *)(size_t)vectors[index].address;
        hl_host_result step;
        if (vectors[index].size == 0) continue;
        step = positioned ? hl_windows_file_positioned(host, file, offset + moved, buffer, vectors[index].size, writing)
                          : hl_windows_file_sequential(host, file, buffer, vectors[index].size, writing);
        if (step.status != HL_STATUS_OK) return moved != 0 ? hl_windows_result(HL_STATUS_OK, moved, 0) : step;
        moved += step.value;
        if (step.value < vectors[index].size) break;
    }
    return hl_windows_result(HL_STATUS_OK, moved, 0);
}

static hl_host_result hl_windows_file_readv(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                            uint32_t count) {
    return hl_windows_file_vector(context, file, vectors, count, 0, 0, 0);
}

static hl_host_result hl_windows_file_writev(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count) {
    return hl_windows_file_vector(context, file, vectors, count, 0, 0, 1);
}

static hl_host_result hl_windows_file_readv_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                               uint32_t count, uint64_t offset) {
    return hl_windows_file_vector(context, file, vectors, count, offset, 1, 0);
}

static hl_host_result hl_windows_file_writev_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                                uint32_t count, uint64_t offset) {
    return hl_windows_file_vector(context, file, vectors, count, offset, 1, 1);
}

/* An append must stay indivisible, so the vectors are gathered into one buffer
 * and written once rather than appended in sequence. */
static hl_host_result hl_windows_file_appendv(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                              uint32_t count) {
    hl_host_windows *host = context;
    uint64_t total = 0;
    uint64_t at = 0;
    uint32_t index;
    unsigned char *gathered;
    hl_host_result result;
    const hl_status valid = hl_windows_vectors_valid(vectors, count, &total);
    if (valid != HL_STATUS_OK) return hl_windows_result(valid, 0, 0);
    if (total == 0) return hl_windows_append_bytes(host, file, "", 0);
    if (total > HL_WINDOWS_IO_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    gathered = malloc((size_t)total);
    if (gathered == NULL) return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    for (index = 0; index < count; ++index) {
        memcpy(gathered + at, (const void *)(size_t)vectors[index].address, (size_t)vectors[index].size);
        at += vectors[index].size;
    }
    result = hl_windows_append_bytes(host, file, gathered, total);
    free(gathered);
    return result;
}

/* --- position --------------------------------------------------------------- */

/*
 * SEEK_DATA and SEEK_HOLE over FSCTL_QUERY_ALLOCATED_RANGES. NTFS reports the
 * allocated extents of a sparse file, which is exactly the information the two
 * modes need; a file that was never made sparse reports one range covering its
 * data, so the answers degrade to "all data, one hole at the end", which is what
 * a filesystem without hole tracking is required to say.
 */
static hl_host_result hl_windows_seek_extent(hl_host_windows *host, HANDLE object, uint64_t offset, uint64_t size,
                                             int want_hole, uint64_t *out) {
    FILE_ALLOCATED_RANGE_BUFFER query;
    FILE_ALLOCATED_RANGE_BUFFER ranges[64];
    DWORD produced = 0;
    DWORD index;
    (void)host;
    query.FileOffset.QuadPart = (LONGLONG)offset;
    query.Length.QuadPart = (LONGLONG)(size - offset);
    if (!DeviceIoControl(object, FSCTL_QUERY_ALLOCATED_RANGES, &query, (DWORD)sizeof(query), ranges,
                         (DWORD)sizeof(ranges), &produced, NULL) &&
        GetLastError() != ERROR_MORE_DATA)
        return hl_windows_last_error_result();
    produced /= (DWORD)sizeof(ranges[0]);
    for (index = 0; index < produced; ++index) {
        const uint64_t start = (uint64_t)ranges[index].FileOffset.QuadPart;
        const uint64_t end = start + (uint64_t)ranges[index].Length.QuadPart;
        if (end <= offset) continue;
        if (want_hole) {
            if (offset < start) break; /* already inside a hole */
            offset = end;
            continue;
        }
        *out = offset > start ? offset : start;
        return hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    if (want_hole) {
        *out = offset < size ? offset : size;
        return hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    /* No data at or after the offset: lseek(2) reports ENXIO for that. */
    return hl_windows_result(HL_STATUS_NOT_FOUND, 0, 0);
}

static hl_host_result hl_windows_file_seek(void *context, hl_host_handle file, int64_t offset, uint32_t whence) {
    hl_host_windows *host = context;
    FILE_POSITION_INFORMATION position;
    FILE_STANDARD_INFORMATION standard;
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    HANDLE object = NULL;
    uint64_t base = 0;
    uint64_t target;
    if (whence > HL_HOST_FILE_SEEK_HOLE) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    status = host->nt.query_information_file(object, &status_block, &standard, (ULONG)sizeof(standard),
                                             FileStandardInformation);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    if (whence == HL_HOST_FILE_SEEK_CUR) {
        status = host->nt.query_information_file(object, &status_block, &position, (ULONG)sizeof(position),
                                                 FilePositionInformation);
        if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
        base = (uint64_t)position.CurrentByteOffset.QuadPart;
    } else if (whence == HL_HOST_FILE_SEEK_END) {
        base = (uint64_t)standard.EndOfFile.QuadPart;
    }
    if (whence == HL_HOST_FILE_SEEK_DATA || whence == HL_HOST_FILE_SEEK_HOLE) {
        hl_host_result found;
        if (offset < 0 || (uint64_t)offset >= (uint64_t)standard.EndOfFile.QuadPart)
            return hl_windows_result(HL_STATUS_NOT_FOUND, 0, 0);
        found = hl_windows_seek_extent(host, object, (uint64_t)offset, (uint64_t)standard.EndOfFile.QuadPart,
                                       whence == HL_HOST_FILE_SEEK_HOLE, &target);
        if (found.status != HL_STATUS_OK) return found;
    } else {
        if (offset < 0 && (uint64_t)(-offset) > base) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        target = offset < 0 ? base - (uint64_t)(-offset) : base + (uint64_t)offset;
        if (target > (uint64_t)INT64_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    position.CurrentByteOffset.QuadPart = (LONGLONG)target;
    status = host->nt.set_information_file(object, &status_block, &position, (ULONG)sizeof(position),
                                           FilePositionInformation);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    return hl_windows_result(HL_STATUS_OK, target, 0);
}

/* --- size, durability ------------------------------------------------------- */

typedef struct hl_windows_offset_information {
    LARGE_INTEGER value;
} hl_windows_offset_information;

static hl_host_result hl_windows_file_truncate(void *context, hl_host_handle file, uint64_t size) {
    hl_host_windows *host = context;
    hl_windows_offset_information end;
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    HANDLE object = NULL;
    if (size > (uint64_t)INT64_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    end.value.QuadPart = (LONGLONG)size;
    status = host->nt.set_information_file(object, &status_block, &end, (ULONG)sizeof(end), FileEndOfFileInformation);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

static hl_host_result hl_windows_flush(hl_host_windows *host, hl_host_handle file) {
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    HANDLE object = NULL;
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    status = host->nt.flush_buffers_file(object, &status_block);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

static hl_host_result hl_windows_file_sync(void *context, hl_host_handle file) {
    return hl_windows_flush(context, file);
}

/* Windows draws no line between metadata and data durability: NtFlushBuffersFile
 * commits both. Reporting NOT_SUPPORTED for fdatasync would be worse than
 * over-delivering on it. */
static hl_host_result hl_windows_file_data_sync(void *context, hl_host_handle file) {
    return hl_windows_flush(context, file);
}

/* sync_file_range(2) has no NT counterpart; there is no range-scoped flush at
 * all. Flushing the whole object satisfies every ordering the flags can ask for,
 * so the range is honoured by being exceeded. */
static hl_host_result hl_windows_file_sync_range(void *context, hl_host_handle file, uint64_t offset, uint64_t size,
                                                 uint32_t flags) {
    if ((flags & ~(uint32_t)(HL_HOST_FILE_SYNC_WAIT_BEFORE | HL_HOST_FILE_SYNC_WRITE | HL_HOST_FILE_SYNC_WAIT_AFTER)) !=
            0 ||
        offset > (uint64_t)INT64_MAX || size > (uint64_t)INT64_MAX || offset > (uint64_t)INT64_MAX - size)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_flush(context, file);
}

/*
 * syncfs(2) flushes every dirty object on the filesystem. The NT equivalent is a
 * flush of the volume handle, which requires opening \\.\X: -- an operation
 * reserved to administrators. This flushes the named object instead: a strict
 * subset of what was asked, reported honestly here rather than as a success that
 * covers a whole volume it never touched.
 */
static hl_host_result hl_windows_file_sync_filesystem(void *context, hl_host_handle file) {
    return hl_windows_flush(context, file);
}

/* --- namespace mutation ----------------------------------------------------- */

/* FILE_RENAME_INFORMATION and FILE_LINK_INFORMATION share this shape. The older
 * spelling puts a BOOLEAN ReplaceIfExists where flags sits; on x86-64 the
 * padding before RootDirectory makes the two layouts identical, so one struct
 * serves both the Ex and the legacy information class. */
typedef struct hl_windows_link_information {
    ULONG flags;
    HANDLE root;
    ULONG name_length;
    WCHAR name[HL_WINDOWS_NAME_MAX + 1];
} hl_windows_link_information;

static ULONG hl_windows_link_size(const hl_windows_link_information *information) {
    return (ULONG)(offsetof(hl_windows_link_information, name) + (size_t)information->name_length);
}

/*
 * unlink(2): open the victim purely to delete it, request POSIX-semantics
 * disposition, and close at once. The name disappears when this handle closes,
 * while every other handle on the object keeps working -- measured, and the
 * reason the handle is not held.
 */
static hl_host_result hl_windows_remove(hl_host_windows *host, hl_host_handle directory, const char *path,
                                        size_t path_size, int as_directory) {
    hl_windows_resolution resolved;
    hl_host_result result;
    HANDLE root = NULL;
    HANDLE victim = NULL;
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    ULONG flags;
    if (path == NULL || path_size == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_windows_directory_for(host, directory, &root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(
        host, root, path, path_size,
        HL_WINDOWS_RESOLVE_ESCAPE | HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_ALLOW_MISSING, &resolved);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) return result;
    if (resolved.leaf_length == 0) {
        /* "." and "/" name the directory itself; POSIX answers EINVAL/EBUSY. */
        CloseHandle(resolved.parent);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = hl_windows_open_child(
        &host->nt, resolved.parent, resolved.leaf, resolved.leaf_length, DELETE, 0, FILE_OPEN,
        FILE_OPEN_REPARSE_POINT | (as_directory ? (ULONG)FILE_DIRECTORY_FILE : (ULONG)FILE_NON_DIRECTORY_FILE),
        &victim);
    CloseHandle(resolved.parent);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    flags = HL_NT_DISPOSITION_DELETE | HL_NT_DISPOSITION_POSIX | HL_NT_DISPOSITION_IGNORE_READONLY;
    status = host->nt.set_information_file(victim, &status_block, &flags, (ULONG)sizeof(flags),
                                           HL_NT_FILE_DISPOSITION_INFORMATION_EX);
    if (status != HL_NT_SUCCESS) {
        /* Pre-1709 kernels and non-NTFS volumes have no Ex class. The legacy
         * disposition still unlinks, just not until the last handle closes. */
        BOOLEAN legacy = TRUE;
        status = host->nt.set_information_file(victim, &status_block, &legacy, (ULONG)sizeof(legacy),
                                               FileDispositionInformation);
    }
    CloseHandle(victim);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

static hl_host_result hl_windows_file_unlink(void *context, hl_host_handle directory, const char *path,
                                             size_t path_size) {
    return hl_windows_remove(context, directory, path, path_size, 0);
}

static hl_host_result hl_windows_file_remove_directory(void *context, hl_host_handle directory, const char *path,
                                                       size_t path_size) {
    return hl_windows_remove(context, directory, path, path_size, 1);
}

static hl_host_result hl_windows_file_rename(void *context, hl_host_handle old_directory, const char *old_path,
                                             size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                             size_t new_path_size) {
    hl_host_windows *host = context;
    hl_windows_resolution source;
    hl_windows_resolution destination;
    hl_windows_link_information information;
    hl_host_result result;
    IO_STATUS_BLOCK status_block;
    HANDLE root = NULL;
    HANDLE object = NULL;
    NTSTATUS status;
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_windows_directory_for(host, old_directory, &root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(host, root, old_path, old_path_size,
                                     HL_WINDOWS_RESOLVE_ESCAPE | HL_HOST_RESOLVE_NOFOLLOW_FINAL, &source);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_directory_for(host, new_directory, &root);
    if (result.status != HL_STATUS_OK) {
        CloseHandle(source.parent);
        return result;
    }
    result = hl_windows_resolve_path(
        host, root, new_path, new_path_size,
        HL_WINDOWS_RESOLVE_ESCAPE | HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_ALLOW_MISSING, &destination);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) {
        CloseHandle(source.parent);
        return result;
    }
    if (source.leaf_length == 0 || destination.leaf_length == 0) {
        CloseHandle(source.parent);
        CloseHandle(destination.parent);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = hl_windows_open_child(&host->nt, source.parent, source.leaf, source.leaf_length, DELETE, 0, FILE_OPEN,
                                   FILE_OPEN_REPARSE_POINT, &object);
    CloseHandle(source.parent);
    if (status != HL_NT_SUCCESS) {
        CloseHandle(destination.parent);
        return hl_windows_nt_result(host, status);
    }
    memset(&information, 0, sizeof(information));
    information.root = destination.parent;
    information.name_length = destination.leaf_length * 2u;
    memcpy(information.name, destination.leaf, (size_t)destination.leaf_length * sizeof(WCHAR));
    /* POSIX semantics is what makes replacing an *open* destination work, which
     * rename(2) requires and the legacy class refuses. */
    information.flags = HL_NT_RENAME_REPLACE_IF_EXISTS | HL_NT_RENAME_POSIX;
    status = host->nt.set_information_file(object, &status_block, &information, hl_windows_link_size(&information),
                                           HL_NT_FILE_RENAME_INFORMATION_EX);
    if (status != HL_NT_SUCCESS) {
        information.flags = HL_NT_RENAME_REPLACE_IF_EXISTS;
        status = host->nt.set_information_file(object, &status_block, &information, hl_windows_link_size(&information),
                                               FileRenameInformation);
    }
    CloseHandle(object);
    CloseHandle(destination.parent);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

static hl_host_result hl_windows_file_make_directory(void *context, hl_host_handle directory, const char *path,
                                                     size_t path_size, uint32_t permissions) {
    hl_host_windows *host = context;
    hl_windows_resolution resolved;
    hl_host_result result;
    HANDLE root = NULL;
    HANDLE object = NULL;
    NTSTATUS status;
    if (path == NULL || path_size == 0 || (permissions & ~07777u) != 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_windows_directory_for(host, directory, &root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(
        host, root, path, path_size,
        HL_WINDOWS_RESOLVE_ESCAPE | HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_ALLOW_MISSING, &resolved);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) return result;
    if (resolved.leaf_length == 0) {
        CloseHandle(resolved.parent);
        return hl_windows_result(HL_STATUS_ALREADY_EXISTS, 0, 0);
    }
    status = hl_windows_open_child(&host->nt, resolved.parent, resolved.leaf, resolved.leaf_length,
                                   FILE_LIST_DIRECTORY | FILE_TRAVERSE, FILE_ATTRIBUTE_NORMAL, FILE_CREATE,
                                   FILE_DIRECTORY_FILE, &object);
    CloseHandle(resolved.parent);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    CloseHandle(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_file_make_symlink(void *context, const char *target, size_t target_size,
                                                   hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_windows *host = context;
    hl_windows_resolution resolved;
    hl_host_result result;
    IO_STATUS_BLOCK status_block;
    LARGE_INTEGER where;
    HANDLE root = NULL;
    HANDLE object = NULL;
    NTSTATUS status;
    if (target == NULL || path == NULL || target_size == 0 || path_size == 0 ||
        target_size > (size_t)HL_WINDOWS_PATH_MAX || memchr(target, '\0', target_size) != NULL)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_windows_directory_for(host, directory, &root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(
        host, root, path, path_size,
        HL_WINDOWS_RESOLVE_ESCAPE | HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_ALLOW_MISSING, &resolved);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) return result;
    if (resolved.leaf_length == 0) {
        CloseHandle(resolved.parent);
        return hl_windows_result(HL_STATUS_ALREADY_EXISTS, 0, 0);
    }
    /* FILE_ATTRIBUTE_SYSTEM is the cheap half of the symlink test: it lets the
     * resolver skip the content read for every ordinary file it walks past. */
    status = hl_windows_open_child(&host->nt, resolved.parent, resolved.leaf, resolved.leaf_length,
                                   FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES, FILE_ATTRIBUTE_SYSTEM, FILE_CREATE,
                                   FILE_NON_DIRECTORY_FILE, &object);
    CloseHandle(resolved.parent);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    where.QuadPart = 0;
    status = host->nt.write_file(object, NULL, NULL, NULL, &status_block, hl_windows_symlink_magic,
                                 (ULONG)HL_WINDOWS_SYMLINK_MAGIC_SIZE, &where, NULL);
    if (status == HL_NT_SUCCESS) {
        where.QuadPart = HL_WINDOWS_SYMLINK_MAGIC_SIZE;
        status = host->nt.write_file(object, NULL, NULL, NULL, &status_block, target, (ULONG)target_size, &where, NULL);
    }
    CloseHandle(object);
    if (status != HL_NT_SUCCESS) {
        (void)hl_windows_remove(host, directory, path, path_size, 0);
        return hl_windows_nt_result(host, status);
    }
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_file_make_link(void *context, hl_host_handle old_directory, const char *old_path,
                                                size_t old_path_size, hl_host_handle new_directory,
                                                const char *new_path, size_t new_path_size, uint32_t flags) {
    hl_host_windows *host = context;
    hl_windows_resolution source;
    hl_windows_resolution destination;
    hl_windows_link_information information;
    hl_host_result result;
    IO_STATUS_BLOCK status_block;
    HANDLE root = NULL;
    HANDLE object = NULL;
    NTSTATUS status;
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 || (flags & ~1u) != 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_windows_directory_for(host, old_directory, &root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(
        host, root, old_path, old_path_size,
        HL_WINDOWS_RESOLVE_ESCAPE | ((flags & 1u) != 0 ? 0u : (uint32_t)HL_HOST_RESOLVE_NOFOLLOW_FINAL), &source);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_directory_for(host, new_directory, &root);
    if (result.status != HL_STATUS_OK) {
        CloseHandle(source.parent);
        return result;
    }
    result = hl_windows_resolve_path(
        host, root, new_path, new_path_size,
        HL_WINDOWS_RESOLVE_ESCAPE | HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_ALLOW_MISSING, &destination);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) {
        CloseHandle(source.parent);
        return result;
    }
    if (source.leaf_length == 0 || destination.leaf_length == 0) {
        CloseHandle(source.parent);
        CloseHandle(destination.parent);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = hl_windows_open_child(&host->nt, source.parent, source.leaf, source.leaf_length, FILE_READ_ATTRIBUTES, 0,
                                   FILE_OPEN, FILE_OPEN_REPARSE_POINT | FILE_NON_DIRECTORY_FILE, &object);
    CloseHandle(source.parent);
    if (status != HL_NT_SUCCESS) {
        CloseHandle(destination.parent);
        return hl_windows_nt_result(host, status);
    }
    memset(&information, 0, sizeof(information));
    information.flags = 0; /* link(2) never replaces an existing name */
    information.root = destination.parent;
    information.name_length = destination.leaf_length * 2u;
    memcpy(information.name, destination.leaf, (size_t)destination.leaf_length * sizeof(WCHAR));
    status = host->nt.set_information_file(object, &status_block, &information, hl_windows_link_size(&information),
                                           FileLinkInformation);
    CloseHandle(object);
    CloseHandle(destination.parent);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

/*
 * A FIFO cannot exist at a filesystem path on Windows. Named pipes live in a
 * separate object namespace reached only through \\.\pipe\, they are not
 * directory entries, and nothing in the filesystem can be made to behave like
 * one. This is a genuine absence, not an unimplemented case: emulating it would
 * mean creating an ordinary file that lies about its type to every stat.
 */
static hl_host_result hl_windows_file_make_fifo(void *context, hl_host_handle directory, const char *path,
                                                size_t path_size, uint32_t permissions) {
    (void)context;
    (void)directory;
    (void)path;
    (void)path_size;
    (void)permissions;
    return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

/* --- links ------------------------------------------------------------------ */

static hl_host_result hl_windows_file_readlink(void *context, hl_host_handle file, hl_host_bytes output) {
    hl_host_windows *host = context;
    char target[HL_WINDOWS_PATH_MAX];
    uint32_t target_size = 0;
    HANDLE object = NULL;
    uint32_t state = 0;
    if (output.size != 0 && output.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_file_borrow(host, file, &object, NULL, &state))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((state & HL_WINDOWS_FILE_PATH_BACKED) == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_symlink_read(&host->nt, object, target, (uint32_t)sizeof(target), &target_size))
        /* readlink(2) on a non-symlink is EINVAL, not ENOENT. */
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((uint64_t)target_size > output.size) return hl_windows_result(HL_STATUS_RESOURCE_LIMIT, target_size, 0);
    if (target_size != 0) memcpy(output.data, target, target_size);
    return hl_windows_result(HL_STATUS_OK, target_size, 0);
}

/*
 * The native absolute path of the object, as "\\?\C:\...". Two hazards live at
 * this call site and neither can be fixed from here. It is a Windows path, so a
 * caller that expects a Linux one will mis-handle it; and the "\\?\" prefix is
 * kept deliberately, because a name ending in a dot or a space is reachable only
 * through it -- Win32 path parsing strips those, the NT parser preserves them,
 * and dropping the prefix would hand back a path that no longer opens the file.
 */
static hl_host_result hl_windows_file_path(void *context, hl_host_handle file, hl_host_bytes output) {
    hl_host_windows *host = context;
    WCHAR path[HL_WINDOWS_PATH_MAX];
    DWORD produced;
    size_t size = 0;
    hl_status converted;
    HANDLE object = NULL;
    uint32_t state = 0;
    if (output.size != 0 && output.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_file_borrow(host, file, &object, NULL, &state))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((state & HL_WINDOWS_FILE_PATH_BACKED) == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    produced = GetFinalPathNameByHandleW(object, path, (DWORD)(sizeof(path) / sizeof(*path)),
                                         FILE_NAME_NORMALIZED | VOLUME_NAME_DOS);
    if (produced == 0 || produced >= sizeof(path) / sizeof(*path)) return hl_windows_last_error_result();
    converted = hl_windows_utf8_from_wide(path, produced, output.data, output.size, &size);
    if (converted != HL_STATUS_OK) return hl_windows_result(converted, size, 0);
    return hl_windows_result(HL_STATUS_OK, size, 0);
}

/* --- attributes ------------------------------------------------------------- */

/*
 * Windows ownership is a SID and a security descriptor; a guest uid is a small
 * integer with no total mapping onto one. Rather than pick a fiction, only the
 * POSIX "change nothing" encoding is accepted -- which is what chown(-1,-1) and
 * the post-create ownership pass actually send. Real guest ownership is
 * virtualised by the Linux front end, which is where the uid namespace lives.
 */
static hl_host_result hl_windows_file_set_owner(void *context, hl_host_handle file, uint32_t uid, uint32_t gid) {
    hl_host_windows *host = context;
    if (!hl_windows_file_borrow(host, file, NULL, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (uid == UINT32_MAX && gid == UINT32_MAX) return hl_windows_result(HL_STATUS_OK, 0, 0);
    return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

static hl_host_result hl_windows_file_set_permissions(void *context, hl_host_handle file, uint32_t permissions) {
    hl_host_windows *host = context;
    FILE_BASIC_INFORMATION basic;
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    HANDLE object = NULL;
    ULONG attributes;
    if ((permissions & ~07777u) != 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&basic, 0, sizeof(basic));
    status = host->nt.query_information_file(object, &status_block, &basic, (ULONG)sizeof(basic), FileBasicInformation);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    /* Only the write bits survive the crossing. Setting the read-only attribute
     * is the whole of what chmod can mean on a Windows volume without an ACL
     * rewrite, and rewriting ACLs from guest mode bits would invent a mapping. */
    attributes = basic.FileAttributes & ~(ULONG)FILE_ATTRIBUTE_READONLY;
    if ((permissions & 0222u) == 0u) attributes |= FILE_ATTRIBUTE_READONLY;
    if (attributes == 0) attributes = FILE_ATTRIBUTE_NORMAL;
    memset(&basic, 0, sizeof(basic));
    basic.FileAttributes = attributes;
    status = host->nt.set_information_file(object, &status_block, &basic, (ULONG)sizeof(basic), FileBasicInformation);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

static hl_host_result hl_windows_file_set_times(void *context, hl_host_handle file, const hl_host_file_time times[2]) {
    hl_host_windows *host = context;
    FILE_BASIC_INFORMATION basic;
    IO_STATUS_BLOCK status_block;
    LARGE_INTEGER now;
    NTSTATUS status;
    HANDLE object = NULL;
    int index;
    if (times == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&basic, 0, sizeof(basic));
    GetSystemTimeAsFileTime((FILETIME *)(void *)&now);
    for (index = 0; index < 2; ++index) {
        LARGE_INTEGER value;
        /* Zero is NT's "leave this stamp alone", which is exactly UTIME_OMIT;
         * the two models agree here without translation. */
        value.QuadPart = 0;
        if (times[index].mode == HL_HOST_FILE_TIME_NOW)
            value = now;
        else if (times[index].mode == HL_HOST_FILE_TIME_EXPLICIT && times[index].nanoseconds < 1000000000u)
            value = hl_windows_time_from_parts(times[index].seconds, times[index].nanoseconds);
        else if (times[index].mode != HL_HOST_FILE_TIME_OMIT)
            return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        if (index == 0)
            basic.LastAccessTime = value;
        else
            basic.LastWriteTime = value;
    }
    status = host->nt.set_information_file(object, &status_block, &basic, (ULONG)sizeof(basic), FileBasicInformation);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

static hl_host_result hl_windows_file_allocate_range(void *context, hl_host_handle file, uint32_t mode, uint64_t offset,
                                                     uint64_t size) {
    const uint32_t allowed =
        HL_HOST_FILE_ALLOC_KEEP_SIZE | HL_HOST_FILE_ALLOC_PUNCH_HOLE | HL_HOST_FILE_ALLOC_ZERO_RANGE;
    hl_host_windows *host = context;
    FILE_STANDARD_INFORMATION standard;
    hl_windows_offset_information extent;
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    HANDLE object = NULL;
    DWORD produced = 0;
    if (size == 0 || offset > (uint64_t)INT64_MAX || size > (uint64_t)INT64_MAX || offset > (uint64_t)INT64_MAX - size)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* COLLAPSE_RANGE, INSERT_RANGE and UNSHARE_RANGE move or split file extents
     * in place. NTFS exposes no operation that does either, and no sequence of
     * copies would be atomic with respect to a concurrent reader, so they are
     * refused rather than approximated. */
    if ((mode & ~allowed) != 0) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    status = host->nt.query_information_file(object, &status_block, &standard, (ULONG)sizeof(standard),
                                             FileStandardInformation);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);

    if ((mode & (HL_HOST_FILE_ALLOC_PUNCH_HOLE | HL_HOST_FILE_ALLOC_ZERO_RANGE)) != 0) {
        FILE_ZERO_DATA_INFORMATION zero;
        if ((mode & HL_HOST_FILE_ALLOC_PUNCH_HOLE) != 0) {
            /* fallocate demands KEEP_SIZE with PUNCH_HOLE, and the hole is only
             * a hole once the file is sparse. */
            if ((mode & HL_HOST_FILE_ALLOC_KEEP_SIZE) == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
            if (!DeviceIoControl(object, FSCTL_SET_SPARSE, NULL, 0, NULL, 0, &produced, NULL))
                return hl_windows_last_error_result();
        }
        zero.FileOffset.QuadPart = (LONGLONG)offset;
        zero.BeyondFinalZero.QuadPart = (LONGLONG)(offset + size);
        if (!DeviceIoControl(object, FSCTL_SET_ZERO_DATA, &zero, (DWORD)sizeof(zero), NULL, 0, &produced, NULL))
            return hl_windows_last_error_result();
    } else {
        extent.value.QuadPart = (LONGLONG)(offset + size);
        if ((uint64_t)standard.AllocationSize.QuadPart < offset + size) {
            status = host->nt.set_information_file(object, &status_block, &extent, (ULONG)sizeof(extent),
                                                   FileAllocationInformation);
            if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
        }
    }
    if ((mode & HL_HOST_FILE_ALLOC_KEEP_SIZE) == 0 && (uint64_t)standard.EndOfFile.QuadPart < offset + size) {
        extent.value.QuadPart = (LONGLONG)(offset + size);
        status = host->nt.set_information_file(object, &status_block, &extent, (ULONG)sizeof(extent),
                                               FileEndOfFileInformation);
        if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    }
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_file_filesystem_metadata(void *context, hl_host_handle file,
                                                          hl_host_filesystem_metadata *output) {
    hl_host_windows *host = context;
    unsigned char attribute_raw[sizeof(FILE_FS_ATTRIBUTE_INFORMATION) + 256];
    unsigned char volume_raw[sizeof(FILE_FS_VOLUME_INFORMATION) + 256];
    FILE_FS_FULL_SIZE_INFORMATION full;
    FILE_FS_ATTRIBUTE_INFORMATION *attributes = (FILE_FS_ATTRIBUTE_INFORMATION *)(void *)attribute_raw;
    FILE_FS_VOLUME_INFORMATION *volume = (FILE_FS_VOLUME_INFORMATION *)(void *)volume_raw;
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    HANDLE object = NULL;
    if (output == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(output, 0, sizeof(*output));
    memset(attribute_raw, 0, sizeof(attribute_raw));
    memset(volume_raw, 0, sizeof(volume_raw));
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    status = host->nt.query_volume_information_file(object, &status_block, &full, (ULONG)sizeof(full),
                                                    FileFsFullSizeInformation);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    output->block_size = (uint64_t)full.BytesPerSector * full.SectorsPerAllocationUnit;
    output->fragment_size = output->block_size;
    output->blocks = (uint64_t)full.TotalAllocationUnits.QuadPart;
    output->blocks_free = (uint64_t)full.ActualAvailableAllocationUnits.QuadPart;
    output->blocks_available = (uint64_t)full.CallerAvailableAllocationUnits.QuadPart;
    /* NTFS grows its MFT on demand, so there is no inode budget to report; statfs
     * on a Linux NTFS mount says the same zero. */
    output->files = 0;
    output->files_free = 0;
    output->name_max = HL_WINDOWS_NAME_MAX;
    if (host->nt.query_volume_information_file(object, &status_block, attributes, (ULONG)sizeof(attribute_raw),
                                               FileFsAttributeInformation) == HL_NT_SUCCESS) {
        output->name_max = attributes->MaximumComponentNameLength;
        if ((attributes->FileSystemAttributes & 0x00080000u /* FILE_READ_ONLY_VOLUME */) != 0) output->flags = 1;
    }
    if (host->nt.query_volume_information_file(object, &status_block, volume, (ULONG)sizeof(volume_raw),
                                               FileFsVolumeInformation) == HL_NT_SUCCESS)
        output->filesystem_id[0] = volume->VolumeSerialNumber;
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* --- directory enumeration -------------------------------------------------- */

/*
 * NtQueryDirectoryFile keeps its cursor inside the file object, so successive
 * calls on one handle continue where the last stopped -- which is the shared
 * open-file-description cursor the contract asks for, without any bookkeeping
 * here. Entries are pulled one at a time (ReturnSingleEntry) so that neither the
 * caller's entry budget nor its byte budget can be overrun by a kernel that
 * filled a buffer with more records than were asked for: an entry consumed from
 * the cursor and then dropped would be an entry lost for good.
 */
static hl_host_result hl_windows_file_read_directory(void *context, hl_host_handle file, hl_host_file_entry *entries,
                                                     uint32_t entry_capacity, uint32_t byte_capacity) {
    hl_host_windows *host = context;
    unsigned char raw[sizeof(FILE_ID_FULL_DIR_INFORMATION) + 2u * (HL_WINDOWS_NAME_MAX + 1u)];
    FILE_ID_FULL_DIR_INFORMATION *record = (FILE_ID_FULL_DIR_INFORMATION *)(void *)raw;
    IO_STATUS_BLOCK status_block;
    hl_windows_handle_entry *slot;
    HANDLE object;
    uint32_t produced = 0;
    uint32_t bytes = 0;
    if (entries == NULL || entry_capacity == 0 || byte_capacity < 24u || byte_capacity > (UINT32_C(1) << 20))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_lock(host);
    slot = hl_windows_lookup_locked(host, file, HL_WINDOWS_HANDLE_FILE);
    if (slot == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object = slot->object;
    while (produced < entry_capacity) {
        size_t name_size = 0;
        uint32_t attributes;
        hl_status converted;
        const NTSTATUS status =
            host->nt.query_directory_file(object, NULL, NULL, NULL, &status_block, raw, (ULONG)sizeof(raw),
                                          FileIdFullDirectoryInformation, TRUE, NULL, FALSE);
        if (status == HL_NT_NO_MORE_FILES) break;
        if (status != HL_NT_SUCCESS) {
            hl_windows_unlock(host);
            return produced != 0 ? hl_windows_result(HL_STATUS_OK, produced, bytes)
                                 : hl_windows_nt_result(host, status);
        }
        converted = hl_windows_utf8_from_wide(record->FileName, record->FileNameLength / 2u, entries[produced].name,
                                              sizeof(entries[produced].name) - 1u, &name_size);
        if (converted != HL_STATUS_OK) {
            hl_windows_unlock(host);
            return hl_windows_result(converted, produced, bytes);
        }
        if (bytes + name_size + 24u > byte_capacity && produced != 0) {
            /* The record has already left the cursor, so stopping here would
             * lose it; it is reported and the byte budget is exceeded by one
             * entry rather than an entry going missing. */
            bytes = byte_capacity;
        }
        entries[produced].name[name_size] = '\0';
        entries[produced].name_size = (uint32_t)name_size;
        entries[produced].object = (uint64_t)record->FileId.QuadPart;
        entries[produced].next_offset = ++slot->file_cursor;
        attributes = (uint32_t)record->FileAttributes;
        if ((attributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
            entries[produced].type = HL_HOST_DIRECTORY_TYPE_DIRECTORY;
        } else if (hl_windows_symlink_candidate(attributes, (uint64_t)record->EndOfFile.QuadPart)) {
            /* A candidate has to be opened to be sure; system-attributed files
             * are rare enough that paying for the check keeps d_type honest
             * instead of pushing a stat back onto every caller. */
            HANDLE probe = NULL;
            entries[produced].type = HL_HOST_DIRECTORY_TYPE_REGULAR;
            if (hl_windows_open_child(&host->nt, object, record->FileName, record->FileNameLength / 2u, FILE_READ_DATA,
                                      0, FILE_OPEN, FILE_OPEN_REPARSE_POINT, &probe) == HL_NT_SUCCESS) {
                if (hl_windows_symlink_read(&host->nt, probe, NULL, 0, NULL))
                    entries[produced].type = HL_HOST_DIRECTORY_TYPE_LINK;
                CloseHandle(probe);
            }
        } else {
            entries[produced].type = HL_HOST_DIRECTORY_TYPE_REGULAR;
        }
        produced++;
        if (bytes >= byte_capacity) break;
        bytes += (uint32_t)name_size + 24u;
    }
    hl_windows_unlock(host);
    return hl_windows_result(HL_STATUS_OK, produced, bytes);
}

/* --- handles ---------------------------------------------------------------- */

static hl_host_result hl_windows_file_clone_for_fork(void *context, hl_host_handle file) {
    hl_host_windows *host = context;
    hl_host_result result;
    HANDLE object = NULL;
    HANDLE copy = NULL;
    uint32_t access = 0;
    uint32_t state = 0;
    if (!hl_windows_file_borrow(host, file, &object, &access, &state))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* DuplicateHandle aliases the file object itself, so the clone shares the
     * position and the append state -- the properties dup(2) is defined by. */
    if (!DuplicateHandle(GetCurrentProcess(), object, GetCurrentProcess(), &copy, 0, FALSE, DUPLICATE_SAME_ACCESS))
        return hl_windows_last_error_result();
    result = hl_windows_file_register(host, copy, access, state);
    if (result.status != HL_STATUS_OK) CloseHandle(copy);
    return result;
}

static hl_host_result hl_windows_file_standard_stream(void *context, uint32_t stream) {
    hl_host_windows *host = context;
    hl_host_result result;
    HANDLE object;
    HANDLE copy = NULL;
    uint32_t access;
    if (stream > HL_HOST_STANDARD_ERROR) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = GetStdHandle(stream == HL_HOST_STANDARD_INPUT    ? STD_INPUT_HANDLE
                          : stream == HL_HOST_STANDARD_OUTPUT ? STD_OUTPUT_HANDLE
                                                              : STD_ERROR_HANDLE);
    if (object == NULL || object == INVALID_HANDLE_VALUE) return hl_windows_result(HL_STATUS_NOT_FOUND, 0, 0);
    /* Windows keeps no queryable access mode on a handle -- there is no F_GETFL
     * -- so the direction is taken from which stream was asked for, which is the
     * only fact available. Append state is not knowable and is not claimed. */
    access = stream == HL_HOST_STANDARD_INPUT ? (uint32_t)HL_HOST_FILE_READ : (uint32_t)HL_HOST_FILE_WRITE;
    if (!DuplicateHandle(GetCurrentProcess(), object, GetCurrentProcess(), &copy, 0, FALSE, DUPLICATE_SAME_ACCESS))
        return hl_windows_last_error_result();
    result = hl_windows_file_register(host, copy, access, 0);
    if (result.status != HL_STATUS_OK) {
        CloseHandle(copy);
        return result;
    }
    result.detail = access;
    return result;
}

static hl_host_result hl_windows_file_close(void *context, hl_host_handle file) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    HANDLE object = NULL;
    int found;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, file, HL_WINDOWS_HANDLE_FILE);
    found = entry != NULL;
    if (found) {
        object = entry->object;
        hl_windows_clear_entry_locked(entry);
    }
    hl_windows_unlock(host);
    /* The slot pointer is not consulted after the unlock: a concurrent grow
     * reallocates the table, and the answer to "was it there" is already held. */
    if (!found) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (object != NULL && !CloseHandle(object)) return hl_windows_last_error_result();
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* --- host-private validation ------------------------------------------------ */

/*
 * The POSIX backends answer "is this cache input private?" with a uid compare
 * and a mode test. Neither exists here, so the equivalent question is asked of
 * the security descriptor: the object must be owned by this process's token
 * user, and no access-allowed entry may grant write to anyone but the owner,
 * LocalSystem or the local Administrators group.
 *
 * The descriptor is parsed in place rather than through advapi32's accessors,
 * for the same reason ntdll is bound by name: a static archive that pulls in a
 * second import library pushes a link flag onto everything downstream.
 */
enum {
    HL_WINDOWS_WRITE_ACCESS = FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_WRITE_EA | FILE_WRITE_ATTRIBUTES | WRITE_DAC |
                              WRITE_OWNER | DELETE | GENERIC_WRITE | GENERIC_ALL
};

static uint32_t hl_windows_sid_size(const void *sid) {
    const unsigned char *bytes = sid;
    return 8u + 4u * bytes[1];
}

static int hl_windows_sid_equal(const void *left, const void *right) {
    const uint32_t size = hl_windows_sid_size(left);
    return size == hl_windows_sid_size(right) && memcmp(left, right, size) == 0;
}

/* S-1-5-18 (LocalSystem) and S-1-5-32-544 (Administrators), spelled as bytes. */
static int hl_windows_sid_privileged(const void *sid) {
    static const unsigned char system_sid[12] = {1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0};
    static const unsigned char administrators_sid[16] = {1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0};
    return hl_windows_sid_equal(sid, system_sid) || hl_windows_sid_equal(sid, administrators_sid);
}

static hl_host_result hl_windows_validate_private(hl_host_windows *host, hl_host_handle file, int want_directory) {
    unsigned char descriptor_raw[4096];
    unsigned char token_raw[256];
    SECURITY_DESCRIPTOR_RELATIVE *descriptor = (SECURITY_DESCRIPTOR_RELATIVE *)(void *)descriptor_raw;
    hl_host_file_metadata metadata;
    hl_host_result result;
    HANDLE object = NULL;
    HANDLE readable = NULL;
    HANDLE token = NULL;
    ULONG produced = 0;
    NTSTATUS status;
    const ACL *acl;
    const unsigned char *owner;
    const unsigned char *user;
    uint32_t index;
    uint32_t at;
    uint32_t state = 0;
    if (!hl_windows_file_borrow(host, file, &object, NULL, &state))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((state & HL_WINDOWS_FILE_PATH_BACKED) == 0) return hl_windows_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    memset(&metadata, 0, sizeof(metadata));
    result = hl_windows_metadata_for(host, object, state, &metadata);
    if (result.status != HL_STATUS_OK) return result;
    if (metadata.type != (want_directory ? (uint32_t)HL_HOST_FILE_TYPE_DIRECTORY : (uint32_t)HL_HOST_FILE_TYPE_REGULAR))
        return hl_windows_result(HL_STATUS_PERMISSION_DENIED, 0, 0);

    /* READ_CONTROL was not part of the original open, and an empty relative name
     * reopens the object the handle already names -- no path, no race. */
    status = hl_windows_open_child(&host->nt, object, L"", 0, READ_CONTROL, 0, FILE_OPEN, 0, &readable);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    status = host->nt.query_security_object(readable, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                                            descriptor_raw, (ULONG)sizeof(descriptor_raw), &produced);
    CloseHandle(readable);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    if (descriptor->Owner == 0 || descriptor->Dacl == 0)
        /* No owner, or a NULL DACL, which grants everyone everything. */
        return hl_windows_result(HL_STATUS_PERMISSION_DENIED, 0, 0);

    status = host->nt.open_process_token(GetCurrentProcess(), TOKEN_QUERY, &token);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    status = host->nt.query_information_token(token, 1 /* TokenUser */, token_raw, (ULONG)sizeof(token_raw), &produced);
    CloseHandle(token);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    user = (const unsigned char *)((const TOKEN_USER *)(const void *)token_raw)->User.Sid;
    owner = descriptor_raw + descriptor->Owner;
    if (!hl_windows_sid_equal(owner, user)) return hl_windows_result(HL_STATUS_PERMISSION_DENIED, 0, 0);

    acl = (const ACL *)(const void *)(descriptor_raw + descriptor->Dacl);
    at = sizeof(ACL);
    for (index = 0; index < acl->AceCount; ++index) {
        const ACE_HEADER *header = (const ACE_HEADER *)(const void *)((const unsigned char *)acl + at);
        const ACCESS_ALLOWED_ACE *ace = (const ACCESS_ALLOWED_ACE *)(const void *)header;
        const unsigned char *sid = (const unsigned char *)(const void *)&ace->SidStart;
        if (at + sizeof(ACE_HEADER) > acl->AclSize || header->AceSize == 0) break;
        at += header->AceSize;
        if (header->AceType != ACCESS_ALLOWED_ACE_TYPE) continue;
        if ((ace->Mask & (ACCESS_MASK)HL_WINDOWS_WRITE_ACCESS) == 0) continue;
        if (hl_windows_sid_equal(sid, owner) || hl_windows_sid_privileged(sid)) continue;
        return hl_windows_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_file_validate_private_regular(void *context, hl_host_handle file) {
    return hl_windows_validate_private(context, file, 0);
}

static hl_host_result hl_windows_file_validate_private_directory(void *context, hl_host_handle directory) {
    return hl_windows_validate_private(context, directory, 1);
}

/*
 * Publish a complete file through a unique temporary and one atomic rename. The
 * rename carries POSIX semantics, so a reader holding the old file open keeps
 * reading it while new openers see the new one -- the property that makes this
 * safe to run against a live cache.
 */
static hl_host_result hl_windows_file_store_private_atomic(void *context, hl_host_handle directory, const char *path,
                                                           size_t path_size, hl_host_const_bytes input,
                                                           uint32_t permissions) {
    static volatile LONG sequence;
    hl_host_windows *host = context;
    hl_windows_resolution resolved;
    hl_windows_link_information information;
    hl_host_result result;
    IO_STATUS_BLOCK status_block;
    LARGE_INTEGER where;
    WCHAR temporary[HL_WINDOWS_NAME_MAX + 1];
    HANDLE root = NULL;
    HANDLE object = NULL;
    NTSTATUS status = HL_NT_SUCCESS;
    uint64_t at = 0;
    unsigned attempt;
    int length;
    if (path == NULL || path_size == 0 || (permissions & ~07777u) != 0 || (input.size != 0 && input.data == NULL) ||
        input.size > HL_WINDOWS_IO_MAX)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_windows_directory_for(host, directory, &root);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_windows_resolve_path(
        host, root, path, path_size,
        HL_WINDOWS_RESOLVE_ESCAPE | HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_ALLOW_MISSING, &resolved);
    CloseHandle(root);
    if (result.status != HL_STATUS_OK) return result;
    if (resolved.leaf_length == 0 || resolved.leaf_length > HL_WINDOWS_NAME_MAX - 32) {
        CloseHandle(resolved.parent);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    for (attempt = 0; attempt < 16u; ++attempt) {
        length = swprintf(temporary, sizeof(temporary) / sizeof(*temporary), L"%.*s.hl-%lx-%lx.tmp",
                          (int)resolved.leaf_length, resolved.leaf, (unsigned long)GetCurrentProcessId(),
                          (unsigned long)InterlockedIncrement(&sequence));
        if (length <= 0) break;
        status = hl_windows_open_child(
            &host->nt, resolved.parent, temporary, (uint32_t)length, FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | DELETE,
            (permissions & 0222u) == 0u ? (ULONG)FILE_ATTRIBUTE_READONLY : (ULONG)FILE_ATTRIBUTE_NORMAL, FILE_CREATE,
            FILE_NON_DIRECTORY_FILE, &object);
        if (status == HL_NT_SUCCESS) break;
    }
    if (status != HL_NT_SUCCESS || object == NULL) {
        CloseHandle(resolved.parent);
        return hl_windows_nt_result(host, status);
    }
    while (at < input.size && status == HL_NT_SUCCESS) {
        where.QuadPart = (LONGLONG)at;
        status = host->nt.write_file(object, NULL, NULL, NULL, &status_block, (const unsigned char *)input.data + at,
                                     hl_windows_transfer_size(input.size - at), &where, NULL);
        at += status_block.Information;
        if (status_block.Information == 0) break;
    }
    if (status == HL_NT_SUCCESS && at == input.size) status = host->nt.flush_buffers_file(object, &status_block);
    if (status == HL_NT_SUCCESS && at == input.size) {
        memset(&information, 0, sizeof(information));
        information.flags = HL_NT_RENAME_REPLACE_IF_EXISTS | HL_NT_RENAME_POSIX;
        information.root = resolved.parent;
        information.name_length = resolved.leaf_length * 2u;
        memcpy(information.name, resolved.leaf, (size_t)resolved.leaf_length * sizeof(WCHAR));
        status = host->nt.set_information_file(object, &status_block, &information, hl_windows_link_size(&information),
                                               HL_NT_FILE_RENAME_INFORMATION_EX);
        if (status != HL_NT_SUCCESS) {
            information.flags = HL_NT_RENAME_REPLACE_IF_EXISTS;
            status = host->nt.set_information_file(object, &status_block, &information,
                                                   hl_windows_link_size(&information), FileRenameInformation);
        }
    }
    if (status != HL_NT_SUCCESS || at != input.size) {
        ULONG flags = HL_NT_DISPOSITION_DELETE | HL_NT_DISPOSITION_POSIX | HL_NT_DISPOSITION_IGNORE_READONLY;
        (void)host->nt.set_information_file(object, &status_block, &flags, (ULONG)sizeof(flags),
                                            HL_NT_FILE_DISPOSITION_INFORMATION_EX);
    }
    CloseHandle(object);
    CloseHandle(resolved.parent);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    return at == input.size ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_result(HL_STATUS_IO, 0, 0);
}

/* --- the table -------------------------------------------------------------- */

const hl_host_file_services hl_windows_file_services = {HL_HOST_FILE_ABI,
                                                        sizeof(hl_windows_file_services),
                                                        hl_windows_file_open,
                                                        hl_windows_file_read_at,
                                                        hl_windows_file_write_at,
                                                        hl_windows_file_append,
                                                        hl_windows_file_metadata,
                                                        hl_windows_file_close,
                                                        hl_windows_file_read,
                                                        hl_windows_file_write,
                                                        hl_windows_file_clone_for_fork,
                                                        hl_windows_file_seek,
                                                        hl_windows_file_readv,
                                                        hl_windows_file_writev,
                                                        hl_windows_file_readv_at,
                                                        hl_windows_file_writev_at,
                                                        hl_windows_file_appendv,
                                                        hl_windows_file_truncate,
                                                        hl_windows_file_sync,
                                                        hl_windows_file_data_sync,
                                                        hl_windows_file_rename,
                                                        hl_windows_file_unlink,
                                                        hl_windows_file_path,
                                                        hl_windows_file_standard_stream,
                                                        hl_windows_file_readlink,
                                                        hl_windows_file_set_owner,
                                                        hl_windows_file_resolve_beneath,
                                                        hl_windows_file_sync_range,
                                                        hl_windows_file_sync_filesystem,
                                                        hl_windows_file_open_beneath,
                                                        hl_windows_file_allocate_range,
                                                        hl_windows_file_filesystem_metadata,
                                                        hl_windows_file_set_permissions,
                                                        hl_windows_file_set_times,
                                                        hl_windows_file_read_directory,
                                                        hl_windows_file_make_directory,
                                                        hl_windows_file_make_symlink,
                                                        hl_windows_file_make_link,
                                                        hl_windows_file_make_fifo,
                                                        hl_windows_file_validate_private_regular,
                                                        hl_windows_file_store_private_atomic,
                                                        hl_windows_file_validate_private_directory,
                                                        hl_windows_file_remove_directory};
