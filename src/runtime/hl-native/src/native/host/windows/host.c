/*
 * The Windows host backend: handle table, error mapping, log, sync and the
 * constructor that assembles hl_host_services.
 *
 * Advertised: MEMORY | CLOCK | LOG | SYNC | CODE_MAPPING | FILE | PROCESS |
 * COUNTER | EVENT | EVENT_TIMER | STREAM | NETWORK.
 * Omitting a bit is safe by construction -- hl_host_services_validate inspects a
 * group's pointers only when its bit is set -- and the contract's rule is that a
 * capability is advertised only when the whole group is real. transfer, network,
 * shared_memory, directory, watch, terminal and posix_attachment are therefore
 * absent rather than stubbed: a half-populated table would pass validation and
 * fail at run time.
 *
 * NETWORK is advertised at ABI 2, all twenty callbacks over Winsock, with three
 * typed refusals among them and each argued at its site in socket.c: a named
 * local datagram socket (no Windows AF_UNIX datagram exists, and framing a
 * socket a third party may connect to is not ours to do), ancillary data (no
 * carrier at all, and a send that dropped it would report a success the peer
 * never sees), and wait_handle (the sockets ARE waitable, but the event group
 * does not accept them yet, so a yes would send a caller into a pollset that
 * never wakes). ws2_32 is resolved by name on first use rather than imported --
 * see socket.c's header for why the import table is the constraint.
 *
 * COUNTER, EVENT and STREAM are advertised because all ten, seven and eight of
 * their callbacks respectively are implemented over real Windows objects, with
 * no typed refusal among them. EVENT_TIMER is a separate bit over the same
 * group and is advertised on its own footing: arm_timer and disarm_timer are
 * real waitable timers, and the group's records carry the caller's token and
 * HL_HOST_READY_TIMER exactly, which is what a POSIX guest timer is drained by.
 *
 * FILE is advertised because all 41 of its callbacks are implemented, including
 * the two -- make_fifo and set_owner -- that report a typed NOT_SUPPORTED. That
 * is a complete group answering honestly, not a gap: a filesystem FIFO and a
 * guest uid have no Windows spelling at all, and file.c says why at each.
 *
 * SYNC is complete at its current ABI on the same footing. The mutexes and the
 * fork trio are real; the parking trio is wired to the shared registry, whose
 * Windows arm reports a typed NOT_SUPPORTED for the wait itself because the
 * process-shared scope has no address-keyed primitive here. Leaving those three
 * pointers NULL instead would not have been the honest option -- it fails
 * validation for the whole group, which takes the mutexes down with it.
 *
 * PROCESS is advertised on the same footing: all five callbacks are real over
 * CreateProcess, and the two inputs that have no Windows meaning -- an entry
 * context that is a pointer into the parent's private memory, and a request to
 * deliver an arbitrary catchable signal -- are refused with a typed
 * NOT_SUPPORTED rather than approximated. process.c says why at each.
 */
#include "internal.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <io.h>
#include <fcntl.h>

/* --- results and error mapping --------------------------------------------- */

hl_host_result hl_windows_result(hl_status status, uint64_t value, uint64_t detail) {
    return (hl_host_result){(int32_t)status, HL_WINDOWS_DETAIL_WIN32, value, detail};
}

/* Win32 error -> hl_status. The native number never escapes as a status; it is
 * carried only in the opaque detail field, tagged with its domain. */
hl_status hl_windows_status_from_error(DWORD error) {
    switch (error) {
    case ERROR_SUCCESS: return HL_STATUS_OK;
    case ERROR_INVALID_PARAMETER:
    case ERROR_INVALID_ADDRESS:
    case ERROR_INVALID_HANDLE:
    case ERROR_NEGATIVE_SEEK: return HL_STATUS_INVALID_ARGUMENT;
    case ERROR_NOT_ENOUGH_MEMORY:
    case ERROR_OUTOFMEMORY:
    case ERROR_COMMITMENT_LIMIT: return HL_STATUS_OUT_OF_MEMORY;
    case ERROR_TOO_MANY_OPEN_FILES: return HL_STATUS_PROCESS_LIMIT;
    case ERROR_NO_MORE_FILES: return HL_STATUS_RESOURCE_LIMIT;
    case ERROR_FILE_NOT_FOUND:
    case ERROR_PATH_NOT_FOUND:
    case ERROR_INVALID_DRIVE: return HL_STATUS_NOT_FOUND;
    case ERROR_FILE_EXISTS:
    case ERROR_ALREADY_EXISTS: return HL_STATUS_ALREADY_EXISTS;
    case ERROR_ACCESS_DENIED:
    case ERROR_PRIVILEGE_NOT_HELD: return HL_STATUS_PERMISSION_DENIED;
    case ERROR_NO_SYSTEM_RESOURCES:
    case ERROR_WORKING_SET_QUOTA: return HL_STATUS_WOULD_BLOCK;
    case ERROR_OPERATION_ABORTED: return HL_STATUS_INTERRUPTED;
    case ERROR_CALL_NOT_IMPLEMENTED:
    case ERROR_NOT_SUPPORTED:
    case ERROR_INVALID_FUNCTION: return HL_STATUS_NOT_SUPPORTED;
    case ERROR_BUSY:
    case ERROR_SHARING_VIOLATION:
    case ERROR_LOCK_VIOLATION: return HL_STATUS_BUSY;
    case ERROR_DIRECTORY: return HL_STATUS_NOT_DIRECTORY;
    case ERROR_DIRECTORY_NOT_SUPPORTED: return HL_STATUS_IS_DIRECTORY;
    case ERROR_FILENAME_EXCED_RANGE: return HL_STATUS_NAME_TOO_LONG;
    case ERROR_CANT_RESOLVE_FILENAME: return HL_STATUS_SYMLINK_LOOP;
    case ERROR_WRITE_PROTECT: return HL_STATUS_READ_ONLY;
    case ERROR_BROKEN_PIPE:
    case ERROR_NO_DATA:
    case ERROR_PIPE_NOT_CONNECTED: return HL_STATUS_DISCONNECTED;
    case ERROR_NOT_SAME_DEVICE: return HL_STATUS_CROSS_DEVICE;
    case ERROR_DIR_NOT_EMPTY: return HL_STATUS_NOT_EMPTY;
    case ERROR_DISK_FULL:
    case ERROR_HANDLE_DISK_FULL: return HL_STATUS_NO_SPACE;
    case ERROR_FILE_TOO_LARGE: return HL_STATUS_FILE_TOO_LARGE;
    case ERROR_TIMEOUT:
    case WAIT_TIMEOUT: return HL_STATUS_TIMED_OUT;
    default: return HL_STATUS_IO;
    }
}

hl_host_result hl_windows_last_error_result(void) {
    const DWORD error = GetLastError();
    return hl_windows_result(hl_windows_status_from_error(error), 0, (uint64_t)error);
}

/* --- the host lock ---------------------------------------------------------- */

void hl_windows_lock(hl_host_windows *host) {
    AcquireSRWLockExclusive(&host->lock);
}

void hl_windows_unlock(hl_host_windows *host) {
    ReleaseSRWLockExclusive(&host->lock);
}

/* --- the handle table ------------------------------------------------------- */

/*
 * A handle is (generation << 32) | (index + 1), with the kind stored in the slot
 * and checked on every lookup. Three rejections fall out of that: zero is never
 * a handle, a reused slot fails the generation compare, and a handle presented
 * to the wrong group fails the kind compare. The table grows by doubling and
 * never shrinks, so an index stays valid for the life of the host.
 *
 * There is no descriptor-band scheme here of the kind the POSIX backends need.
 * That design rests on four premises Windows breaks: descriptors are small dense
 * integers, RLIMIT_NOFILE bounds them, F_DUPFD_CLOEXEC can relocate one above a
 * chosen floor, and fork inherits both the descriptors and a shared page holding
 * the table. A Windows HANDLE is opaque, sparse and not relocatable, and a guest
 * can never see one at all.
 */
hl_host_handle hl_windows_encode_handle(uint32_t index, uint32_t generation) {
    return ((uint64_t)generation << 32) | (uint64_t)(index + 1u);
}

hl_windows_handle_entry *hl_windows_lookup_locked(hl_host_windows *host, hl_host_handle handle,
                                                  hl_windows_handle_kind kind) {
    const uint32_t low = (uint32_t)handle;
    const uint32_t generation = (uint32_t)(handle >> 32);
    hl_windows_handle_entry *entry;
    uint32_t index;
    if (low == 0) return NULL;
    index = low - 1u;
    if (index >= host->handle_capacity) return NULL;
    entry = &host->handles[index];
    if (entry->generation != generation || entry->kind != (uint16_t)kind) return NULL;
    return entry;
}

void hl_windows_clear_entry_locked(hl_windows_handle_entry *entry) {
    const uint32_t generation = entry->generation;
    memset(entry, 0, sizeof(*entry));
    entry->generation = generation;
}

hl_host_result hl_windows_allocate_handle(hl_host_windows *host, hl_windows_handle_kind kind) {
    hl_host_handle handle = 0;
    uint32_t index;
    hl_windows_lock(host);
    if (host->destroying) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    for (index = 0; index < host->handle_capacity; ++index) {
        hl_windows_handle_entry *entry = &host->handles[index];
        if (entry->kind != HL_WINDOWS_HANDLE_NONE) continue;
        entry->generation++;
        if (entry->generation == 0) entry->generation = 1;
        entry->kind = (uint16_t)kind;
        handle = hl_windows_encode_handle(index, entry->generation);
        break;
    }
    if (handle == 0) {
        const uint32_t capacity =
            host->handle_capacity > (UINT32_MAX - 1u) / 2u ? UINT32_MAX - 1u : host->handle_capacity * 2u;
        hl_windows_handle_entry *grown =
            capacity > host->handle_capacity ? realloc(host->handles, (size_t)capacity * sizeof(*grown)) : NULL;
        if (grown != NULL) {
            hl_windows_handle_entry *entry;
            memset(&grown[host->handle_capacity], 0, (size_t)(capacity - host->handle_capacity) * sizeof(*grown));
            index = host->handle_capacity;
            host->handles = grown;
            host->handle_capacity = capacity;
            entry = &host->handles[index];
            entry->generation = 1;
            entry->kind = (uint16_t)kind;
            handle = hl_windows_encode_handle(index, entry->generation);
        }
    }
    hl_windows_unlock(host);
    if (handle == 0) return hl_windows_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    return hl_windows_result(HL_STATUS_OK, handle, 0);
}

/* --- log -------------------------------------------------------------------- */

static void hl_windows_log(void *context, uint32_t event, const char *message, size_t message_size) {
    HANDLE stream = GetStdHandle(STD_ERROR_HANDLE);
    size_t written = 0;
    (void)context;
    (void)event;
    if (stream == INVALID_HANDLE_VALUE || stream == NULL) return;
    while (written < message_size) {
        const size_t remaining = message_size - written;
        DWORD chunk = remaining > (size_t)MAXDWORD ? MAXDWORD : (DWORD)remaining;
        DWORD produced = 0;
        /* No EINTR on Windows, so a short write is the only reason to loop. */
        if (!WriteFile(stream, message + written, chunk, &produced, NULL) || produced == 0) break;
        written += produced;
    }
}

/* --- sync ------------------------------------------------------------------- */

static hl_host_result hl_windows_mutex_create(void *context) {
    hl_host_windows *host = context;
    return hl_host_sync_mutex_create(host->sync);
}

static hl_host_result hl_windows_mutex_lock(void *context, hl_host_handle handle) {
    hl_host_windows *host = context;
    return hl_host_sync_mutex_lock(host->sync, handle);
}

static hl_host_result hl_windows_mutex_unlock(void *context, hl_host_handle handle) {
    hl_host_windows *host = context;
    return hl_host_sync_mutex_unlock(host->sync, handle);
}

static hl_host_result hl_windows_mutex_close(void *context, hl_host_handle handle) {
    hl_host_windows *host = context;
    return hl_host_sync_mutex_close(host->sync, handle);
}

/*
 * The fork bracket. There is no fork on this host, so there is nothing to
 * quiesce and nothing to repair -- but hl_host_services_validate requires all
 * three entry points non-NULL whenever SYNC is advertised, and SYNC is
 * mandatory. The bracket is still taken honestly rather than returned OK
 * blindly: prepare acquires the mutex registry's lock and the two completions
 * release it, so a caller that brackets some other operation with it gets the
 * mutual exclusion it asked for.
 */
static hl_host_result hl_windows_fork_prepare(void *context) {
    hl_host_windows *host = context;
    return hl_host_sync_fork_prepare(host->sync);
}

static hl_host_result hl_windows_fork_complete(void *context) {
    hl_host_windows *host = context;
    return hl_host_sync_fork_complete(host->sync);
}

/* The parking trio is the shared registry's, exactly as on the other backends:
 * the waiter table, the interruption record and the deadline arithmetic are
 * host-independent, and only the address-keyed wait underneath differs. */
static hl_host_result hl_windows_park(void *context, uint64_t waiter, uint32_t scope, uint64_t key, const void *address,
                                      uint64_t expected, uint32_t compare_size, uint64_t deadline_ns) {
    hl_host_windows *host = context;
    return hl_host_sync_park(host->sync, waiter, scope, key, address, expected, compare_size, deadline_ns);
}

static hl_host_result hl_windows_unpark(void *context, uint32_t scope, uint64_t key, const void *address,
                                        uint32_t count) {
    hl_host_windows *host = context;
    return hl_host_sync_unpark(host->sync, scope, key, address, count);
}

static hl_host_result hl_windows_interrupt_park(void *context, uint64_t waiter) {
    hl_host_windows *host = context;
    return hl_host_sync_interrupt_park(host->sync, waiter);
}

/* --- construction ----------------------------------------------------------- */

static int hl_windows_resolve_kernelbase(hl_windows_kernelbase *api) {
    HMODULE module = GetModuleHandleW(L"KernelBase.dll");
    if (module == NULL) module = LoadLibraryW(L"KernelBase.dll");
    if (module == NULL) return 0;
    /* The casts through a function-pointer-shaped intermediate are what ISO C
     * allows: GetProcAddress returns FARPROC and the conversion is only valid
     * back to a function pointer type. */
    api->virtual_alloc2 = (PVOID(WINAPI *)(HANDLE, PVOID, SIZE_T, ULONG, ULONG, MEM_EXTENDED_PARAMETER *, ULONG))(
        void (*)(void))GetProcAddress(module, "VirtualAlloc2");
    api->map_view_of_file3 =
        (PVOID(WINAPI *)(HANDLE, HANDLE, PVOID, ULONG64, SIZE_T, ULONG, ULONG, MEM_EXTENDED_PARAMETER *, ULONG))(
            void (*)(void))GetProcAddress(module, "MapViewOfFile3");
    api->unmap_view_of_file2 =
        (BOOL(WINAPI *)(HANDLE, PVOID, ULONG))(void (*)(void))GetProcAddress(module, "UnmapViewOfFile2");
    api->query_unbiased_interrupt_time_precise =
        (VOID(WINAPI *)(PULONGLONG))(void (*)(void))GetProcAddress(module, "QueryUnbiasedInterruptTimePrecise");
    api->query_unbiased_interrupt_time =
        (BOOLEAN(WINAPI *)(PULONGLONG))(void (*)(void))GetProcAddress(module, "QueryUnbiasedInterruptTime");
    /* The three placeholder entry points are load-bearing: without them
     * unmap_range cannot carve a hole and map_file cannot land at a fixed
     * address, so there is no honest degraded mode to fall back to. */
    return api->virtual_alloc2 != NULL && api->map_view_of_file3 != NULL && api->unmap_view_of_file2 != NULL;
}

hl_status hl_host_windows_create(hl_host_windows **out_host, hl_host_services *out_services) {
    static const hl_host_log_services log = {HL_HOST_LOG_ABI, sizeof(log), hl_windows_log};
    static const hl_host_sync_services sync = {
        HL_HOST_SYNC_ABI,         sizeof(sync),           hl_windows_mutex_create, hl_windows_mutex_lock,
        hl_windows_mutex_unlock,  hl_windows_mutex_close, hl_windows_fork_prepare, hl_windows_fork_complete,
        hl_windows_fork_complete, hl_windows_park,        hl_windows_unpark,       hl_windows_interrupt_park};
    SYSTEM_INFO info;
    hl_host_windows *host;
    if (out_host == NULL || out_services == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *out_host = NULL;
    memset(out_services, 0, sizeof(*out_services));

    /* The memory group encodes 4 KiB pages and a 64 KiB reservation granularity
     * as constants because both are baked into the placeholder call sequences.
     * A machine that disagrees would silently mis-split every mapping, so it is
     * refused here instead. */
    GetSystemInfo(&info);
    if ((uint64_t)info.dwPageSize != HL_WINDOWS_PAGE_SIZE ||
        (uint64_t)info.dwAllocationGranularity != HL_WINDOWS_ALLOCATION_GRANULARITY)
        return HL_STATUS_NOT_SUPPORTED;

    host = calloc(1, sizeof(*host));
    if (host == NULL) return HL_STATUS_OUT_OF_MEMORY;
    if (!hl_windows_resolve_kernelbase(&host->api)) {
        free(host);
        return HL_STATUS_NOT_SUPPORTED;
    }
    /* Every export the file group binds has shipped in ntdll since NT 4, so a
     * failure here means the process is running against something that is not
     * Windows rather than an older Windows -- refuse rather than degrade. */
    if (!hl_windows_resolve_ntdll(&host->nt)) {
        free(host);
        return HL_STATUS_NOT_SUPPORTED;
    }
    host->handles = calloc(HL_WINDOWS_HANDLE_CAPACITY, sizeof(*host->handles));
    if (host->handles == NULL) {
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    host->handle_capacity = HL_WINDOWS_HANDLE_CAPACITY;
    InitializeSRWLock(&host->lock);
    if (hl_host_sync_registry_create(&host->sync) != HL_STATUS_OK) {
        free(host->handles);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    /* Diagnostics are UTF-8 byte spans. Without this a console interprets them
     * in its OEM code page and every non-ASCII message is mojibake. */
    (void)SetConsoleOutputCP(CP_UTF8);

    /* The three inherited descriptors are put into BINARY mode, once, here.
     *
     * This CRT defaults them to text mode, which rewrites every '\n' the process
     * writes into "\r\n" and strips '\r' on the way in. For a program that emits
     * text that is a convenience; for this one it is corruption. Everything that
     * crosses these descriptors is a byte span a GUEST produced or expects --
     * a guest's write(2) is a byte count it will compare against the return
     * value, and a translated executable, a tar stream or a checkpoint image
     * passing through stdout must arrive byte-identical. Measured: a guest
     * write(1, "hi\n", 3) reached the console as three bytes plus an injected
     * carriage return, and reported 3, so the guest could not even detect it.
     *
     * Done at host creation rather than per write because the mode is a property
     * of the descriptor, not of a call, and because every later writer -- the
     * engine's own diagnostics included -- wants the same answer. The engine's
     * diagnostics are LF-terminated after this, which every Windows console and
     * every tool that reads a pipe handles. */
    (void)_setmode(_fileno(stdin), _O_BINARY);
    (void)_setmode(_fileno(stdout), _O_BINARY);
    (void)_setmode(_fileno(stderr), _O_BINARY);

    out_services->abi = HL_HOST_SERVICES_ABI;
    out_services->size = sizeof(*out_services);
    out_services->capabilities = HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_LOG | HL_HOST_CAP_SYNC |
                                 HL_HOST_CAP_CODE_MAPPING | HL_HOST_CAP_FILE | HL_HOST_CAP_PROCESS |
                                 HL_HOST_CAP_COUNTER | HL_HOST_CAP_EVENT | HL_HOST_CAP_EVENT_TIMER |
                                 HL_HOST_CAP_STREAM | HL_HOST_CAP_NETWORK;
    out_services->context = host;
    out_services->memory = &hl_windows_memory_services;
    out_services->clock = &hl_windows_clock_services;
    out_services->log = &log;
    out_services->sync = &sync;
    out_services->file = &hl_windows_file_services;
    out_services->process = &hl_windows_process_services;
    out_services->counter = &hl_windows_counter_services;
    out_services->event = &hl_windows_event_services;
    out_services->stream = &hl_windows_stream_services;
    out_services->network = &hl_windows_network_services;
    *out_host = host;
    return HL_STATUS_OK;
}

void hl_host_windows_destroy(hl_host_windows *host) {
    uint32_t index;
    if (host == NULL) return;
    /* Subscriptions first, and outside the host lock, because retiring one
     * blocks until its thread-pool callback has finished and takes the host
     * lock to find the next -- neither of which can happen with the table
     * pinned. Nothing new can be subscribed: the caller is destroying. */
    hl_windows_counter_retire_all_subscriptions(host);
    hl_windows_lock(host);
    host->destroying = 1;
    for (index = 0; index < host->handle_capacity; ++index) {
        hl_windows_handle_entry *entry = &host->handles[index];
        if (entry->kind == HL_WINDOWS_HANDLE_MAPPING)
            hl_windows_memory_destroy_entry(host, entry);
        else if (entry->kind == HL_WINDOWS_HANDLE_COUNTER)
            hl_windows_counter_destroy_entry(entry);
        else if (entry->kind == HL_WINDOWS_HANDLE_STREAM)
            hl_windows_stream_destroy_entry(entry);
        else if (entry->kind == HL_WINDOWS_HANDLE_POLLSET)
            hl_windows_event_destroy_entry(entry);
        else if (entry->kind == HL_WINDOWS_HANDLE_SOCKET)
            hl_windows_socket_destroy_entry(entry);
        else if (entry->kind != HL_WINDOWS_HANDLE_NONE && entry->object != NULL)
            CloseHandle(entry->object);
    }
    hl_windows_unlock(host);
    hl_host_sync_registry_destroy(host->sync);
    free(host->handles);
    free(host);
}
