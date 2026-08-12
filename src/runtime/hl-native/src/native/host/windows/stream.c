/*
 * The stream group: an opaque byte stream pair with readiness, non-blocking
 * status flags and a bounded move.
 *
 * The pipe is a NAMED pipe with a unique, single-instance name, not a
 * CreatePipe anonymous pipe, and that is the one decision the rest of this file
 * follows from. An anonymous pipe handle cannot be opened FILE_FLAG_OVERLAPPED,
 * and without overlapped I/O there is no way to learn that data has ARRIVED --
 * only to ask whether it is already there. A pollset needs a handle that
 * becomes signalled, so the read end is created overlapped and carries a
 * standing zero-byte read:
 *
 *   ReadFile(pipe, buffer, 0, NULL, overlapped)
 *
 * A zero-byte read on a byte-mode pipe consumes nothing and completes when the
 * pipe has data, so its OVERLAPPED event is signalled exactly when the stream
 * is readable. That event is what event.control registers, and it is re-armed
 * after every read, which is what makes it level-triggered rather than a single
 * edge: re-arming a pipe that still holds data completes immediately and leaves
 * the event set.
 *
 * The write end is deliberately NOT overlapped. Nothing needs it to be: write
 * readiness is a quota question, which NtQueryInformationFile answers exactly
 * and without blocking, and a synchronous handle keeps ordinary writes to one
 * system call. The cost is stated where it is paid, in event.control: a
 * registered write end contributes no waitable handle, so a pollset is not
 * woken by a full pipe draining. Callers above this seam re-derive readiness on
 * every wake rather than trusting the wake to name the object, so what they
 * lose is a wake they did not need to be correct.
 *
 * Two handles onto one endpoint (pipe_pair + duplicate) alias one object with a
 * reference count, the way dup(2) aliases one open file description. That is
 * what makes the non-blocking bit set through one handle visible through the
 * other, which is the observable behaviour of the flag living in the open file
 * description rather than in the descriptor.
 */
#include "internal.h"

#include <stdlib.h>
#include <string.h>

/*
 * NtQueryInformationFile's pipe-local class. Declared here because the mingw-w64
 * winternl.h ships neither the structure nor the class number, and this is the
 * only query that answers "how many bytes will a write accept right now"
 * without attempting the write.
 */
#define HL_WINDOWS_FILE_PIPE_LOCAL_INFORMATION 24

typedef struct hl_windows_pipe_local_information {
    ULONG named_pipe_type;
    ULONG named_pipe_configuration;
    ULONG maximum_instances;
    ULONG current_instances;
    ULONG inbound_quota;
    ULONG read_data_available;
    ULONG outbound_quota;
    ULONG write_quota_available;
    ULONG named_pipe_state;
    ULONG named_pipe_end;
} hl_windows_pipe_local_information;

#define HL_WINDOWS_PIPE_BUFFER 65536u

typedef struct hl_windows_stream_object {
    CRITICAL_SECTION lock;
    HANDLE pipe;
    HANDLE ready;        /* manual-reset; the standing zero-byte read's event. Read end only. */
    OVERLAPPED *arm;     /* the standing zero-byte read. Heap, because the kernel writes it. */
    unsigned char probe; /* the zero-byte read's (unused) buffer; never dereferenced by the kernel */
    uint32_t writing;
    uint32_t flags; /* HL_HOST_STREAM_NONBLOCK */
    uint32_t references;
} hl_windows_stream_object;

/* --- lifetime --------------------------------------------------------------- */

static void hl_windows_stream_object_release(hl_windows_stream_object *object) {
    uint32_t remaining;
    EnterCriticalSection(&object->lock);
    remaining = object->references == 0 ? 0 : --object->references;
    LeaveCriticalSection(&object->lock);
    if (remaining != 0) return;
    /* The standing read has to be cancelled and OBSERVED before the OVERLAPPED
     * is freed: the kernel writes the completion into that memory, and a cancel
     * that has not been waited for is not a cancel that has finished. */
    if (object->arm != NULL && object->pipe != NULL) {
        DWORD ignored = 0;
        (void)CancelIoEx(object->pipe, object->arm);
        (void)GetOverlappedResult(object->pipe, object->arm, &ignored, TRUE);
    }
    if (object->pipe != NULL) CloseHandle(object->pipe);
    if (object->ready != NULL) CloseHandle(object->ready);
    free(object->arm);
    DeleteCriticalSection(&object->lock);
    free(object);
}

void hl_windows_stream_destroy_entry(hl_windows_handle_entry *entry) {
    hl_windows_stream_object *object = entry->payload;
    entry->payload = NULL;
    if (object != NULL) hl_windows_stream_object_release(object);
}

HANDLE hl_windows_stream_wait_handle_locked(const hl_windows_handle_entry *entry) {
    const hl_windows_stream_object *object = entry->payload;
    return object == NULL ? NULL : object->ready;
}

/* --- the standing zero-byte read -------------------------------------------- */

/* Re-issue the standing read if the previous one has completed. Object lock
 * held. Nothing to do while it is still pending: a pending read IS the armed
 * state, and re-issuing would only add a second outstanding operation. */
static void hl_windows_stream_arm_locked(hl_windows_stream_object *object) {
    if (object->arm == NULL || object->pipe == NULL) return;
    if (!HasOverlappedIoCompleted(object->arm)) return;
    ResetEvent(object->ready);
    object->arm->Internal = 0;
    object->arm->InternalHigh = 0;
    object->arm->Offset = 0;
    object->arm->OffsetHigh = 0;
    if (!ReadFile(object->pipe, &object->probe, 0, NULL, object->arm) && GetLastError() != ERROR_IO_PENDING) {
        /* A broken pipe is permanently readable -- a read of it returns end of
         * file immediately -- so leaving the event set is the correct level. */
        SetEvent(object->ready);
    }
}

/* --- handle resolution ------------------------------------------------------ */

static hl_windows_stream_object *hl_windows_stream_acquire(hl_host_windows *host, hl_host_handle handle) {
    hl_windows_handle_entry *entry;
    hl_windows_stream_object *object = NULL;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_STREAM);
    if (entry != NULL && entry->payload != NULL) {
        object = entry->payload;
        EnterCriticalSection(&object->lock);
        object->references++;
        LeaveCriticalSection(&object->lock);
    }
    hl_windows_unlock(host);
    return object;
}

/* --- creation --------------------------------------------------------------- */

/* Append an unsigned decimal to a wide buffer. Deliberately hand-written: the
 * import gate forbids wsprintfW, and the CRT's wide printf is a heavier
 * dependency than four lines of arithmetic. */
static uint32_t hl_windows_stream_append_number(WCHAR *out, uint32_t offset, uint64_t value) {
    WCHAR digits[24];
    uint32_t count = 0;
    do {
        digits[count++] = (WCHAR)(L'0' + (WCHAR)(value % 10u));
        value /= 10u;
    } while (value != 0 && count < (uint32_t)(sizeof(digits) / sizeof(digits[0])));
    while (count != 0)
        out[offset++] = digits[--count];
    return offset;
}

static void hl_windows_stream_name(WCHAR *out, uint32_t capacity, uint64_t sequence) {
    static const WCHAR prefix[] = L"\\\\.\\pipe\\hl-host-";
    uint32_t offset = 0;
    (void)capacity;
    while (prefix[offset] != L'\0') {
        out[offset] = prefix[offset];
        offset++;
    }
    offset = hl_windows_stream_append_number(out, offset, (uint64_t)GetCurrentProcessId());
    out[offset++] = L'-';
    offset = hl_windows_stream_append_number(out, offset, sequence);
    out[offset] = L'\0';
}

static hl_windows_stream_object *hl_windows_stream_object_create(HANDLE pipe, uint32_t writing, uint32_t flags) {
    hl_windows_stream_object *object = calloc(1, sizeof(*object));
    if (object == NULL) return NULL;
    InitializeCriticalSection(&object->lock);
    object->pipe = pipe;
    object->writing = writing;
    object->flags = flags;
    object->references = 1;
    if (writing == 0) {
        object->ready = CreateEventW(NULL, TRUE, FALSE, NULL);
        object->arm = calloc(1, sizeof(*object->arm));
        if (object->ready == NULL || object->arm == NULL) {
            object->pipe = NULL; /* the caller still owns it on this path */
            hl_windows_stream_object_release(object);
            return NULL;
        }
        object->arm->hEvent = object->ready;
        /* Mark the standing read complete so the first arm actually issues it. */
        object->arm->Internal = 0;
        EnterCriticalSection(&object->lock);
        hl_windows_stream_arm_locked(object);
        LeaveCriticalSection(&object->lock);
    }
    return object;
}

static hl_host_result hl_windows_stream_publish(hl_host_windows *host, hl_windows_stream_object *object) {
    hl_windows_handle_entry *entry;
    hl_host_result allocated = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_STREAM);
    if (allocated.status != HL_STATUS_OK) return allocated;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, allocated.value, HL_WINDOWS_HANDLE_STREAM);
    entry->payload = object;
    hl_windows_unlock(host);
    return allocated;
}

static hl_host_result hl_windows_stream_pipe_pair(void *context, uint32_t flags) {
    static volatile LONG64 sequence;
    hl_host_windows *host = context;
    WCHAR name[64];
    HANDLE reader;
    HANDLE writer;
    hl_windows_stream_object *read_object;
    hl_windows_stream_object *write_object;
    hl_host_result first;
    hl_host_result second;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_stream_name(name, (uint32_t)(sizeof(name) / sizeof(name[0])),
                           (uint64_t)InterlockedIncrement64(&sequence));
    /* FIRST_PIPE_INSTANCE plus a name no other process can predict-and-win is
     * what makes this pair private: a second server on the same name fails
     * rather than silently becoming an alternative endpoint. */
    reader = CreateNamedPipeW(name, PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                              PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, HL_WINDOWS_PIPE_BUFFER,
                              HL_WINDOWS_PIPE_BUFFER, 0, NULL);
    if (reader == INVALID_HANDLE_VALUE) return hl_windows_last_error_result();
    writer = CreateFileW(name, GENERIC_WRITE, 0, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    if (writer == INVALID_HANDLE_VALUE) {
        hl_host_result failure = hl_windows_last_error_result();
        CloseHandle(reader);
        return failure;
    }
    read_object = hl_windows_stream_object_create(reader, 0, flags);
    write_object = read_object == NULL ? NULL : hl_windows_stream_object_create(writer, 1, flags);
    if (read_object == NULL || write_object == NULL) {
        if (read_object != NULL) hl_windows_stream_object_release(read_object);
        CloseHandle(reader);
        CloseHandle(writer);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    first = hl_windows_stream_publish(host, read_object);
    if (first.status != HL_STATUS_OK) {
        hl_windows_stream_object_release(read_object);
        hl_windows_stream_object_release(write_object);
        return first;
    }
    second = hl_windows_stream_publish(host, write_object);
    if (second.status != HL_STATUS_OK) {
        hl_windows_stream_object_release(write_object);
        return second;
    }
    first.detail = second.value;
    return first;
}

/* --- transfer --------------------------------------------------------------- */

/* Bytes a read would return right now, and whether the peer is gone. */
static hl_status hl_windows_stream_available(hl_windows_stream_object *object, uint32_t *available, int *broken) {
    DWORD ready = 0;
    *available = 0;
    *broken = 0;
    if (PeekNamedPipe(object->pipe, NULL, 0, NULL, &ready, NULL)) {
        *available = (uint32_t)ready;
        return HL_STATUS_OK;
    }
    {
        const DWORD error = GetLastError();
        if (error == ERROR_BROKEN_PIPE || error == ERROR_PIPE_NOT_CONNECTED || error == ERROR_NO_DATA) {
            *broken = 1;
            return HL_STATUS_OK;
        }
        return hl_windows_status_from_error(error);
    }
}

static hl_status hl_windows_stream_write_quota(const hl_host_windows *host, hl_windows_stream_object *object,
                                               uint32_t *quota, int *broken) {
    hl_windows_pipe_local_information information;
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    *quota = 0;
    *broken = 0;
    memset(&information, 0, sizeof(information));
    memset(&status_block, 0, sizeof(status_block));
    status = host->nt.query_information_file(object->pipe, &status_block, &information, (ULONG)sizeof(information),
                                             (FILE_INFORMATION_CLASS)HL_WINDOWS_FILE_PIPE_LOCAL_INFORMATION);
    if (status >= 0) {
        *quota = (uint32_t)information.write_quota_available;
        return HL_STATUS_OK;
    }
    {
        const hl_status mapped = hl_windows_status_from_ntstatus(host, status);
        if (mapped == HL_STATUS_DISCONNECTED) {
            *broken = 1;
            return HL_STATUS_OK;
        }
        return mapped;
    }
}

static hl_host_result hl_windows_stream_read(void *context, hl_host_handle stream, hl_host_bytes output) {
    hl_host_windows *host = context;
    hl_windows_stream_object *object;
    hl_host_result result;
    uint32_t available = 0;
    int broken = 0;
    hl_status status;
    DWORD chunk;
    DWORD produced = 0;
    if (output.size != 0 && output.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_stream_acquire(host, stream);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (object->writing) {
        hl_windows_stream_object_release(object);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (output.size == 0) {
        hl_windows_stream_object_release(object);
        return hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    status = hl_windows_stream_available(object, &available, &broken);
    if (status != HL_STATUS_OK) {
        hl_windows_stream_object_release(object);
        return hl_windows_result(status, 0, 0);
    }
    /* A pipe whose writer is gone reads as end of file, not as an error: that
     * is what read(2) reports and what every caller above this seam expects. */
    if (broken) {
        hl_windows_stream_object_release(object);
        return hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    EnterCriticalSection(&object->lock);
    if (available == 0 && (object->flags & HL_HOST_STREAM_NONBLOCK) != 0) {
        LeaveCriticalSection(&object->lock);
        hl_windows_stream_object_release(object);
        return hl_windows_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    }
    LeaveCriticalSection(&object->lock);
    chunk = output.size > (size_t)MAXDWORD ? MAXDWORD : (DWORD)output.size;
    {
        OVERLAPPED overlapped;
        memset(&overlapped, 0, sizeof(overlapped));
        overlapped.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
        if (overlapped.hEvent == NULL) {
            hl_host_result failure = hl_windows_last_error_result();
            hl_windows_stream_object_release(object);
            return failure;
        }
        if (!ReadFile(object->pipe, output.data, chunk, NULL, &overlapped) && GetLastError() == ERROR_IO_PENDING)
            (void)GetOverlappedResult(object->pipe, &overlapped, &produced, TRUE);
        else
            (void)GetOverlappedResult(object->pipe, &overlapped, &produced, FALSE);
        {
            const DWORD error = GetLastError();
            if (produced == 0 && error != ERROR_SUCCESS && error != ERROR_BROKEN_PIPE && error != ERROR_HANDLE_EOF &&
                error != ERROR_MORE_DATA)
                result = hl_windows_result(hl_windows_status_from_error(error), 0, error);
            else
                result = hl_windows_result(HL_STATUS_OK, produced, 0);
        }
        CloseHandle(overlapped.hEvent);
    }
    EnterCriticalSection(&object->lock);
    hl_windows_stream_arm_locked(object);
    LeaveCriticalSection(&object->lock);
    hl_windows_stream_object_release(object);
    return result;
}

static hl_host_result hl_windows_stream_write(void *context, hl_host_handle stream, hl_host_const_bytes input) {
    hl_host_windows *host = context;
    hl_windows_stream_object *object;
    DWORD chunk;
    DWORD consumed = 0;
    if (input.size != 0 && input.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_stream_acquire(host, stream);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!object->writing) {
        hl_windows_stream_object_release(object);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (input.size == 0) {
        hl_windows_stream_object_release(object);
        return hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    chunk = input.size > (size_t)MAXDWORD ? MAXDWORD : (DWORD)input.size;
    EnterCriticalSection(&object->lock);
    if ((object->flags & HL_HOST_STREAM_NONBLOCK) != 0) {
        uint32_t quota = 0;
        int broken = 0;
        const hl_status status = hl_windows_stream_write_quota(host, object, &quota, &broken);
        LeaveCriticalSection(&object->lock);
        if (status != HL_STATUS_OK) {
            hl_windows_stream_object_release(object);
            return hl_windows_result(status, 0, 0);
        }
        if (broken) {
            hl_windows_stream_object_release(object);
            return hl_windows_result(HL_STATUS_DISCONNECTED, 0, 0);
        }
        if (quota == 0) {
            hl_windows_stream_object_release(object);
            return hl_windows_result(HL_STATUS_WOULD_BLOCK, 0, 0);
        }
        /* Capping to the quota is what makes the write not block. A short
         * write is the POSIX answer for a non-blocking pipe with room for some
         * but not all of the request, so it is reported rather than refused. */
        if ((uint32_t)chunk > quota) chunk = (DWORD)quota;
    } else
        LeaveCriticalSection(&object->lock);
    if (!WriteFile(object->pipe, input.data, chunk, &consumed, NULL)) {
        hl_host_result failure = hl_windows_last_error_result();
        hl_windows_stream_object_release(object);
        return failure;
    }
    hl_windows_stream_object_release(object);
    return hl_windows_result(HL_STATUS_OK, consumed, 0);
}

static hl_host_result hl_windows_stream_duplicate(void *context, hl_host_handle stream) {
    hl_host_windows *host = context;
    hl_windows_stream_object *object = hl_windows_stream_acquire(host, stream);
    hl_host_result published;
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    published = hl_windows_stream_publish(host, object);
    if (published.status != HL_STATUS_OK) hl_windows_stream_object_release(object);
    return published;
}

static hl_host_result hl_windows_stream_close(void *context, hl_host_handle stream) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_windows_stream_object *object;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, stream, HL_WINDOWS_HANDLE_STREAM);
    object = entry == NULL ? NULL : entry->payload;
    if (entry != NULL) hl_windows_clear_entry_locked(entry);
    hl_windows_unlock(host);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_stream_object_release(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_stream_set_status_flags(void *context, hl_host_handle stream, uint32_t flags) {
    hl_windows_stream_object *object;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_stream_acquire(context, stream);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    EnterCriticalSection(&object->lock);
    object->flags = flags;
    LeaveCriticalSection(&object->lock);
    hl_windows_stream_object_release(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_stream_readiness(void *context, hl_host_handle stream, uint32_t interests) {
    hl_host_windows *host = context;
    hl_windows_stream_object *object = hl_windows_stream_acquire(host, stream);
    uint32_t ready = 0;
    hl_status status;
    int broken = 0;
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (object->writing) {
        uint32_t quota = 0;
        status = hl_windows_stream_write_quota(host, object, &quota, &broken);
        if (status == HL_STATUS_OK) {
            if (broken)
                ready = HL_HOST_READY_HANGUP | HL_HOST_READY_ERROR;
            else if (quota != 0)
                ready = HL_HOST_READY_WRITE;
        }
    } else {
        uint32_t available = 0;
        status = hl_windows_stream_available(object, &available, &broken);
        if (status == HL_STATUS_OK) {
            /* A hung-up read end is readable: the read returns end of file
             * immediately. Reporting only HANGUP would make a poller that asked
             * for READ block forever on a pipe that will never block again. */
            if (broken)
                ready = HL_HOST_READY_READ | HL_HOST_READY_HANGUP;
            else if (available != 0)
                ready = HL_HOST_READY_READ;
        }
    }
    hl_windows_stream_object_release(object);
    if (status != HL_STATUS_OK) return hl_windows_result(status, 0, 0);
    return hl_windows_result(HL_STATUS_OK, ready & interests, 0);
}

/* --- move ------------------------------------------------------------------- */

/*
 * There is no Windows splice, so this is a bounce through a bounded buffer --
 * and the contract's hard rule, that no byte is consumed which the destination
 * did not accept, is what dictates its shape rather than the buffer size.
 *
 * A file endpoint is always POSITIONED, so reading from it consumes nothing: it
 * is safe to read first and report only what the destination took. A stream
 * endpoint has no offset, so a read DOES consume; for those the size is clamped
 * first to what the source has and then to what the destination will accept
 * without blocking, so the write that follows cannot come up short.
 */
#define HL_WINDOWS_STREAM_MOVE_CHUNK 65536u

typedef struct hl_windows_move_endpoint {
    hl_windows_stream_object *stream; /* NULL for a file endpoint */
    int is_file;
} hl_windows_move_endpoint;

static int hl_windows_move_resolve(hl_host_windows *host, hl_host_handle handle, hl_windows_move_endpoint *out) {
    const hl_windows_handle_entry *entry;
    out->stream = hl_windows_stream_acquire(host, handle);
    out->is_file = 0;
    if (out->stream != NULL) return 1;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_FILE);
    out->is_file = entry != NULL;
    hl_windows_unlock(host);
    return out->is_file;
}

static void hl_windows_move_release(hl_windows_move_endpoint *endpoint) {
    if (endpoint->stream != NULL) hl_windows_stream_object_release(endpoint->stream);
    endpoint->stream = NULL;
}

static hl_host_result hl_windows_stream_move(void *context, hl_host_handle source, uint64_t source_offset,
                                             hl_host_handle destination, uint64_t destination_offset, uint64_t size,
                                             uint32_t flags) {
    const uint32_t allowed = HL_HOST_STREAM_SOURCE_POSITIONED | HL_HOST_STREAM_DESTINATION_POSITIONED;
    hl_host_windows *host = context;
    hl_windows_move_endpoint input;
    hl_windows_move_endpoint output;
    unsigned char buffer[HL_WINDOWS_STREAM_MOVE_CHUNK];
    hl_host_result result;
    DWORD chunk;
    DWORD produced = 0;
    DWORD consumed = 0;
    if ((flags & ~allowed) != 0 || source_offset > INT64_MAX || destination_offset > INT64_MAX)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_move_resolve(host, source, &input)) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!hl_windows_move_resolve(host, destination, &output)) {
        hl_windows_move_release(&input);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    /* File endpoints must be POSITIONED and file-to-file is rejected outright,
     * both straight from the contract: this operation never races or mutates a
     * file's shared position. */
    if ((input.is_file && output.is_file) || (input.is_file != ((flags & HL_HOST_STREAM_SOURCE_POSITIONED) != 0)) ||
        (output.is_file != ((flags & HL_HOST_STREAM_DESTINATION_POSITIONED) != 0)) ||
        (!input.is_file && input.stream->writing) || (!output.is_file && !output.stream->writing)) {
        hl_windows_move_release(&input);
        hl_windows_move_release(&output);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (size == 0) {
        hl_windows_move_release(&input);
        hl_windows_move_release(&output);
        return hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    chunk = size > (uint64_t)sizeof(buffer) ? (DWORD)sizeof(buffer) : (DWORD)size;
    if (!input.is_file) {
        uint32_t available = 0;
        int broken = 0;
        const hl_status status = hl_windows_stream_available(input.stream, &available, &broken);
        if (status != HL_STATUS_OK || broken || available == 0) {
            hl_windows_move_release(&input);
            hl_windows_move_release(&output);
            if (status != HL_STATUS_OK) return hl_windows_result(status, 0, 0);
            return hl_windows_result(HL_STATUS_OK, 0, 0);
        }
        if (available < (uint32_t)chunk) chunk = (DWORD)available;
    }
    if (!output.is_file) {
        uint32_t quota = 0;
        int broken = 0;
        const hl_status status = hl_windows_stream_write_quota(host, output.stream, &quota, &broken);
        if (status != HL_STATUS_OK || broken) {
            hl_windows_move_release(&input);
            hl_windows_move_release(&output);
            return hl_windows_result(status != HL_STATUS_OK ? status : HL_STATUS_DISCONNECTED, 0, 0);
        }
        if (quota == 0) {
            hl_windows_move_release(&input);
            hl_windows_move_release(&output);
            return hl_windows_result(HL_STATUS_WOULD_BLOCK, 0, 0);
        }
        if (quota < (uint32_t)chunk) chunk = (DWORD)quota;
    }
    /* A file endpoint goes through the file group's own positioned calls rather
     * than through a raw ReadFile with an OVERLAPPED offset. On a synchronous
     * handle that raw form ALSO advances the shared file position, which is
     * exactly the mutation this operation is specified never to perform. */
    if (input.is_file) {
        const hl_host_bytes span = {buffer, (size_t)chunk};
        const hl_host_result read = hl_windows_file_services.read_at(host, source, source_offset, span);
        if (read.status != HL_STATUS_OK) {
            hl_windows_move_release(&input);
            hl_windows_move_release(&output);
            return read;
        }
        produced = (DWORD)read.value;
    } else {
        OVERLAPPED overlapped;
        memset(&overlapped, 0, sizeof(overlapped));
        overlapped.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
        if (overlapped.hEvent == NULL) {
            hl_host_result failure = hl_windows_last_error_result();
            hl_windows_move_release(&input);
            hl_windows_move_release(&output);
            return failure;
        }
        if (!ReadFile(input.stream->pipe, buffer, chunk, &produced, &overlapped) && GetLastError() == ERROR_IO_PENDING)
            (void)GetOverlappedResult(input.stream->pipe, &overlapped, &produced, TRUE);
        CloseHandle(overlapped.hEvent);
        EnterCriticalSection(&input.stream->lock);
        hl_windows_stream_arm_locked(input.stream);
        LeaveCriticalSection(&input.stream->lock);
    }
    if (produced == 0) {
        hl_windows_move_release(&input);
        hl_windows_move_release(&output);
        return hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    if (output.is_file) {
        const hl_host_const_bytes span = {buffer, (size_t)produced};
        const hl_host_result written = hl_windows_file_services.write_at(host, destination, destination_offset, span);
        if (written.status != HL_STATUS_OK) {
            hl_windows_move_release(&input);
            hl_windows_move_release(&output);
            return written;
        }
        consumed = (DWORD)written.value;
    } else if (!WriteFile(output.stream->pipe, buffer, produced, &consumed, NULL))
        consumed = 0;
    result = hl_windows_result(HL_STATUS_OK, consumed, 0);
    hl_windows_move_release(&input);
    hl_windows_move_release(&output);
    return result;
}

const hl_host_stream_services hl_windows_stream_services = {.abi = HL_HOST_STREAM_ABI,
                                                            .size = sizeof(hl_host_stream_services),
                                                            .pipe_pair = hl_windows_stream_pipe_pair,
                                                            .read = hl_windows_stream_read,
                                                            .write = hl_windows_stream_write,
                                                            .duplicate = hl_windows_stream_duplicate,
                                                            .close = hl_windows_stream_close,
                                                            .set_status_flags = hl_windows_stream_set_status_flags,
                                                            .readiness = hl_windows_stream_readiness,
                                                            .move = hl_windows_stream_move};
