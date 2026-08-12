#ifndef HL_HOST_WINDOWS_INTERNAL_H
#define HL_HOST_WINDOWS_INTERNAL_H

#include "hl/windows.h"
#include "../sync.h"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <winternl.h>

/* Reservation granularity, not page size. SYSTEM_INFO.dwAllocationGranularity is
 * 64 KiB on every shipping x86-64 Windows and the value is baked into the ABI of
 * the placeholder calls; the backend still reads it back at create time and
 * refuses to run if the machine disagrees. */
#define HL_WINDOWS_ALLOCATION_GRANULARITY UINT64_C(65536)
#define HL_WINDOWS_PAGE_SIZE UINT64_C(4096)
#define HL_WINDOWS_HANDLE_CAPACITY 4096u

/* hl_host_result.detail_domain. 1 is errno (the POSIX backends), 0 is "no
 * domain" (src/host/sync.c); 2 and 3 are Win32 and NTSTATUS respectively, so a
 * reader can tell which numbering a detail came from. The file group is the
 * first user of domain 3: every path operation goes through NtCreateFile, whose
 * status codes are strictly finer than the Win32 errors they collapse into. */
enum { HL_WINDOWS_DETAIL_WIN32 = 2u, HL_WINDOWS_DETAIL_NT = 3u };

typedef enum hl_windows_handle_kind {
    HL_WINDOWS_HANDLE_NONE = 0,
    HL_WINDOWS_HANDLE_MAPPING = 1,
    HL_WINDOWS_HANDLE_FILE = 2,
    HL_WINDOWS_HANDLE_SOCKET = 3,
    HL_WINDOWS_HANDLE_POLLSET = 4,
    HL_WINDOWS_HANDLE_SHARED_MEMORY = 5,
    HL_WINDOWS_HANDLE_PROCESS = 6,
    HL_WINDOWS_HANDLE_COUNTER = 7,
    HL_WINDOWS_HANDLE_TRANSFER = 8,
    HL_WINDOWS_HANDLE_DIRECTORY = 9,
    HL_WINDOWS_HANDLE_WATCH = 10,
    HL_WINDOWS_HANDLE_STREAM = 11,
    /* A counter subscription. It is a handle kind rather than a private table
     * because counter.unsubscribe is handed an opaque hl_host_handle and has to
     * resolve it with the same generation and kind checks every other handle
     * gets; a second numbering would be a second way to get that wrong. */
    HL_WINDOWS_HANDLE_SUBSCRIPTION = 12
} hl_windows_handle_kind;

/*
 * One record per NT allocation inside a mapping, kept in ascending offset order
 * and always 1:1 with a distinct AllocationBase. That correspondence is the
 * invariant the whole memory group rests on: every placeholder-replacing call
 * demands the *exact* bounds of one placeholder, so a region record is what
 * lets the backend name those bounds later. A range with no record is address
 * space the process no longer owns.
 */
typedef enum hl_windows_region_state {
    HL_WINDOWS_REGION_COMMIT = 1, /* private committed pages (VirtualAlloc2) */
    HL_WINDOWS_REGION_VIEW = 2    /* a section view (MapViewOfFile3) */
} hl_windows_region_state;

typedef struct hl_windows_region {
    uint64_t offset; /* from the mapping base */
    uint64_t size;
    uint64_t section_offset; /* byte offset into the section, VIEW only */
    uint32_t state;
    uint32_t protection; /* HL_HOST_MEMORY_* as requested, not the Win32 spelling */
} hl_windows_region;

/* entry->file_state, valid on HL_WINDOWS_HANDLE_FILE slots only. */
enum {
    /* The object came out of the filesystem namespace, so path() and the
     * metadata queries mean something. Duplicated standard streams do not set
     * it: a console or pipe has no path and no stable identity. */
    HL_WINDOWS_FILE_PATH_BACKED = 1u << 0,
    HL_WINDOWS_FILE_DIRECTORY = 1u << 1
};

/* entry->process_state, valid on HL_WINDOWS_HANDLE_PROCESS slots only. */
enum {
    /* A wait has observed the child exit and the exit fields below are final. */
    HL_WINDOWS_PROCESS_REAPED = 1u << 0
};

typedef struct hl_windows_handle_entry {
    uint32_t generation;
    uint16_t kind;
    uint16_t reserved;
    HANDLE object;       /* file, socket, process, ... for non-mapping kinds */
    HANDLE section;      /* section backing this mapping's views, NULL if private */
    HANDLE section_file; /* file the section was created over, borrowed not owned */
    uint32_t section_owned;
    void *address;
    void *executable_address; /* second alias of a dual-alias code mapping */
    uint64_t size;
    hl_windows_region *regions;
    uint32_t region_count;
    uint32_t region_capacity;
    uint32_t file_access; /* HL_HOST_FILE_* exactly as the caller asked for them */
    uint32_t file_state;  /* HL_WINDOWS_FILE_* */
    /* Directory enumeration has no byte offset on Windows -- NtQueryDirectoryFile
     * carries its cursor inside the file object. This counter only fills in
     * hl_host_file_entry.next_offset with something monotonic per handle. */
    uint64_t file_cursor;
    /* HL_WINDOWS_HANDLE_PROCESS only. The completion is retained on the slot
     * once a wait has observed it and is released only by close, which is what
     * lets repeated and concurrent waiters read one identical answer. The id is
     * kept alongside the handle because a console control event names a process
     * group by id, not by handle. */
    uint32_t process_id;
    uint32_t process_state; /* HL_WINDOWS_PROCESS_* */
    uint32_t process_waiters;
    uint32_t process_exit_kind; /* hl_host_process_exit_kind */
    uint64_t process_exit_value;
    /* Group-private record for the kinds whose state does not fit a HANDLE: a
     * counter object, a stream object, a pollset or a subscription. Those four
     * groups share an object between several handles (duplicate aliases rather
     * than copies), so the slot points at the object instead of owning it, and
     * the object carries its own reference count. */
    void *payload;
    /* HL_HOST_TRANSFER_* rights, on HL_WINDOWS_HANDLE_COUNTER slots only. */
    uint32_t rights;
} hl_windows_handle_entry;

/*
 * VirtualAlloc2, MapViewOfFile3 and UnmapViewOfFile2 are exported by
 * KernelBase.dll and *not* by kernel32.dll, and mingw-w64's import libraries
 * carry no thunk for any of them: taking their address links only against
 * -lmincore, which would force a link flag on every consumer of this archive.
 * They are resolved by name instead, once, at create time. Same story for
 * QueryUnbiasedInterruptTimePrecise.
 */
typedef struct hl_windows_kernelbase {
    PVOID(WINAPI *virtual_alloc2)(HANDLE, PVOID, SIZE_T, ULONG, ULONG, MEM_EXTENDED_PARAMETER *, ULONG);
    PVOID(WINAPI *map_view_of_file3)
    (HANDLE, HANDLE, PVOID, ULONG64, SIZE_T, ULONG, ULONG, MEM_EXTENDED_PARAMETER *, ULONG);
    BOOL(WINAPI *unmap_view_of_file2)(HANDLE, PVOID, ULONG);
    VOID(WINAPI *query_unbiased_interrupt_time_precise)(PULONGLONG);
    BOOLEAN(WINAPI *query_unbiased_interrupt_time)(PULONGLONG);
} hl_windows_kernelbase;

/*
 * The native NT file interface, resolved by name for the same reason the
 * KernelBase entry points above are: mingw-w64 ships no import thunk for most of
 * it, and taking the addresses statically would force -lntdll on every consumer
 * of this archive.
 *
 * The file group is built on NtCreateFile rather than CreateFileW throughout,
 * and that is not a preference. OBJECT_ATTRIBUTES.RootDirectory is the only
 * Windows primitive that opens a name *relative to an open directory*, which is
 * what openat, resolve_beneath and open_beneath all require; it also sidesteps
 * MAX_PATH entirely, because a relative NT name is never joined into a
 * Win32-parsed path.
 */
typedef struct hl_windows_ntdll {
    NTSTATUS(NTAPI *create_file)
    (PHANDLE, ACCESS_MASK, POBJECT_ATTRIBUTES, PIO_STATUS_BLOCK, PLARGE_INTEGER, ULONG, ULONG, ULONG, ULONG, PVOID,
     ULONG);
    NTSTATUS(NTAPI *read_file)
    (HANDLE, HANDLE, PIO_APC_ROUTINE, PVOID, PIO_STATUS_BLOCK, PVOID, ULONG, PLARGE_INTEGER, PULONG);
    NTSTATUS(NTAPI *write_file)
    (HANDLE, HANDLE, PIO_APC_ROUTINE, PVOID, PIO_STATUS_BLOCK, const void *, ULONG, PLARGE_INTEGER, PULONG);
    NTSTATUS(NTAPI *query_information_file)(HANDLE, PIO_STATUS_BLOCK, PVOID, ULONG, FILE_INFORMATION_CLASS);
    NTSTATUS(NTAPI *set_information_file)(HANDLE, PIO_STATUS_BLOCK, PVOID, ULONG, FILE_INFORMATION_CLASS);
    NTSTATUS(NTAPI *query_directory_file)
    (HANDLE, HANDLE, PIO_APC_ROUTINE, PVOID, PIO_STATUS_BLOCK, PVOID, ULONG, FILE_INFORMATION_CLASS, BOOLEAN,
     PUNICODE_STRING, BOOLEAN);
    NTSTATUS(NTAPI *query_volume_information_file)(HANDLE, PIO_STATUS_BLOCK, PVOID, ULONG, FS_INFORMATION_CLASS);
    NTSTATUS(NTAPI *flush_buffers_file)(HANDLE, PIO_STATUS_BLOCK);
    NTSTATUS(NTAPI *query_security_object)(HANDLE, SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, ULONG, PULONG);
    NTSTATUS(NTAPI *open_process_token)(HANDLE, ACCESS_MASK, PHANDLE);
    NTSTATUS(NTAPI *query_information_token)(HANDLE, ULONG, PVOID, ULONG, PULONG);
    ULONG(NTAPI *status_to_dos_error)(NTSTATUS);
} hl_windows_ntdll;

struct hl_host_windows {
    SRWLOCK lock;
    hl_windows_kernelbase api;
    hl_windows_ntdll nt;
    uint32_t destroying;
    hl_host_sync_registry *sync;
    hl_windows_handle_entry *handles;
    uint32_t handle_capacity;
};

/* --- shared plumbing (host.c) --------------------------------------------- */

hl_host_result hl_windows_result(hl_status status, uint64_t value, uint64_t detail);
hl_status hl_windows_status_from_error(DWORD error);
hl_host_result hl_windows_last_error_result(void);

void hl_windows_lock(hl_host_windows *host);
void hl_windows_unlock(hl_host_windows *host);

hl_host_handle hl_windows_encode_handle(uint32_t index, uint32_t generation);
hl_windows_handle_entry *hl_windows_lookup_locked(hl_host_windows *host, hl_host_handle handle,
                                                  hl_windows_handle_kind kind);
/* Reserves a slot for `kind` and returns its opaque handle in `value`. The
 * caller fills the payload under the host lock once the native object exists. */
hl_host_result hl_windows_allocate_handle(hl_host_windows *host, hl_windows_handle_kind kind);
void hl_windows_clear_entry_locked(hl_windows_handle_entry *entry);

/* --- the NT path layer (ntpath.c) ----------------------------------------- */

enum {
    HL_WINDOWS_NAME_MAX = 255,  /* NTFS MaximumComponentNameLength, read back at run time */
    HL_WINDOWS_PATH_MAX = 4096, /* in WCHARs; a bound on this layer, not on NT */
    HL_WINDOWS_SYMLINK_MAGIC_SIZE = 13
};

/* A resolver policy bit private to this backend, above the public
 * HL_HOST_RESOLVE_* range: openat semantics, where ".." may leave the directory
 * the caller pinned. resolve_beneath and open_beneath never set it. */
enum { HL_WINDOWS_RESOLVE_ESCAPE = 1u << 8 };

typedef struct hl_windows_resolution {
    HANDLE parent;                       /* owned by the caller on success */
    WCHAR leaf[HL_WINDOWS_NAME_MAX + 1]; /* NT spelling; length 0 means "parent itself" */
    uint32_t leaf_length;
} hl_windows_resolution;

int hl_windows_resolve_ntdll(hl_windows_ntdll *nt);
hl_status hl_windows_status_from_ntstatus(const hl_host_windows *host, NTSTATUS status);
hl_host_result hl_windows_nt_result(const hl_host_windows *host, NTSTATUS status);

hl_status hl_windows_wide_from_utf8(const char *bytes, size_t size, int escape, WCHAR *out, uint32_t capacity,
                                    uint32_t *out_length);
hl_status hl_windows_utf8_from_wide(const WCHAR *wide, uint32_t length, char *out, size_t capacity, size_t *out_size);

NTSTATUS hl_windows_open_child(const hl_windows_ntdll *nt, HANDLE parent, const WCHAR *name, uint32_t length,
                               ACCESS_MASK access, ULONG attributes, ULONG disposition, ULONG options, HANDLE *out);
hl_host_result hl_windows_open_working_directory(hl_host_windows *host, HANDLE *out);
hl_host_result hl_windows_resolve_path(hl_host_windows *host, HANDLE root, const char *path, size_t path_size,
                                       uint32_t policy, hl_windows_resolution *out);

extern const char hl_windows_symlink_magic[HL_WINDOWS_SYMLINK_MAGIC_SIZE];
int hl_windows_symlink_candidate(uint32_t attributes, uint64_t size);
int hl_windows_symlink_read(const hl_windows_ntdll *nt, HANDLE file, char *target, uint32_t capacity,
                            uint32_t *out_size);

/* --- groups --------------------------------------------------------------- */

extern const hl_host_memory_services hl_windows_memory_services;
extern const hl_host_clock_services hl_windows_clock_services;
extern const hl_host_file_services hl_windows_file_services;
extern const hl_host_process_services hl_windows_process_services;
extern const hl_host_counter_services hl_windows_counter_services;
extern const hl_host_event_services hl_windows_event_services;
extern const hl_host_stream_services hl_windows_stream_services;
extern const hl_host_network_services hl_windows_network_services;

/* Tear down every mapping a destroyed host still owns. */
void hl_windows_memory_destroy_entry(hl_host_windows *host, hl_windows_handle_entry *entry);
/* Same, for the three groups whose slots own a reference-counted payload. Each
 * is called under the host lock with the host already marked destroying. */
void hl_windows_counter_destroy_entry(hl_windows_handle_entry *entry);
void hl_windows_stream_destroy_entry(hl_windows_handle_entry *entry);
void hl_windows_event_destroy_entry(hl_windows_handle_entry *entry);
/* Same again for a socket slot. A socket is closed through closesocket and not
 * CloseHandle -- the two are not interchangeable even though a SOCKET is a real
 * kernel handle, because only the former runs the protocol's own teardown. */
void hl_windows_socket_destroy_entry(hl_windows_handle_entry *entry);

/*
 * The borrowed handle a pollset waits on for this object, or NULL when the
 * object has no waitable form. Called with the host lock held; the handle stays
 * owned by the object and must not be closed by the pollset.
 *
 * This is what keeps event.control typed. The alternative -- handing the pollset
 * whatever HANDLE the slot happens to hold -- would register a pipe or a file
 * handle whose signalled state means "the last synchronous I/O on this handle
 * finished", which is not readiness and would wake the pollset at the wrong
 * times forever.
 */
HANDLE hl_windows_counter_wait_handle_locked(const hl_windows_handle_entry *entry);
HANDLE hl_windows_stream_wait_handle_locked(const hl_windows_handle_entry *entry);
HANDLE hl_windows_socket_wait_handle_locked(const hl_windows_handle_entry *entry);
/* Retire every live subscription attached to a counter handle, or -- for the
 * teardown form -- every live subscription this host owns. Both quiesce each
 * callback before returning and must be called without the host lock held. */
void hl_windows_counter_retire_subscriptions(hl_host_windows *host, hl_host_handle counter);
void hl_windows_counter_retire_all_subscriptions(hl_host_windows *host);

#endif
