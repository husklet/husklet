/*
 * The process group: five callbacks over CreateProcess, and the child-side
 * bootstrap that turns a fresh image into a call of the caller's entry point.
 *
 * The shape of this file rests on one fact about what these callbacks are for.
 * "fork" names three unrelated things in this tree, and spawn_cloned /
 * spawn_prepared are the *launch* one: a fresh engine that cold-loads a guest.
 * At the moment they are called there is no guest, no translated code and no
 * worker thread, so nothing warm has to survive. What the child actually needs
 * is a process of its own, a fixed set of handles, and a waitable identity --
 * and CreateProcess plus explicit handle inheritance supplies all three without
 * an address-space clone. (An address-space clone is a real requirement for
 * guest fork(2), which is a different problem and is not implemented here.)
 *
 * Four decisions follow from that, and each has a cost stated where it is paid:
 *
 *   1. The child is a re-execution of *our own image*, discovered with
 *      GetModuleFileNameW. The entry point is carried as an offset from the
 *      module base rather than as an absolute address, so the child recomputes
 *      it against its own base and nothing depends on the two processes being
 *      loaded at the same address.
 *   2. The launch record travels in a pagefile-backed section whose handle is
 *      inherited, and the *numeric handle value* is what the environment
 *      variable carries. Inherited handles occupy the same numeric slot in the
 *      child, which makes a handle value a usable transfer channel; the
 *      environment is used only for that one small integer, because it is the
 *      one channel that exists before the child has run a single instruction.
 *   3. Inheritance is by PROC_THREAD_ATTRIBUTE_HANDLE_LIST, never by "every
 *      inheritable handle". The list form is exact: a handle the parent forgot
 *      to think about cannot leak into the child, and a handle on the list that
 *      is not marked inheritable fails the spawn loudly instead of silently
 *      arriving absent.
 *   4. The child is created suspended so that the parent can stamp the child's
 *      own process id into the record before it runs. The child refuses a
 *      record not addressed to it, which is what keeps a stale environment
 *      variable inherited by some grandchild from hijacking an unrelated
 *      process.
 *
 * The one thing CreateProcess cannot do that fork does: entry_context is a bare
 * pointer, and a fresh process does not have the parent's heap. A context that
 * points at committed parent memory is therefore refused at spawn time with a
 * typed HL_STATUS_NOT_SUPPORTED -- in the parent, before a child exists --
 * rather than handed to a child that would fault on the first dereference. A
 * context that is not an address at all (the scalar-in-a-pointer form) crosses
 * intact, and so does NULL.
 *
 * A caller whose context IS a graph of parent pointers therefore has to hand the
 * child BYTES, and the launch channel below is how it does so. Two additions,
 * both riding the mechanism that already exists:
 *
 *   - a byte payload, copied into the same inherited section the launch record
 *     travels in and left mapped in the child for the life of the process. The
 *     caller serialises its context, publishes the bytes, spawns with a NULL
 *     entry_context, and rebuilds on the far side. Nothing here interprets the
 *     bytes; the producer and the consumer are the same image, which is what
 *     lets the encoding stay private to the caller.
 *   - a shared mapping, handed over as a SECTION HANDLE on the inheritance list.
 *     CreateProcess inherits handles, not views, so a parent's mapped address
 *     means nothing to a child; the handle value crosses (an inherited handle
 *     keeps its number) and the child maps a view of its own from it. This is
 *     what lets a child write into a page the parent reads after the wait.
 *
 * Both are published immediately before the spawn and consumed by it, on the
 * thread that spawns, so two concurrent spawns cannot take each other's payload.
 */
#include "internal.h"

#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include "../process.h"
#include "launch.h"

/*
 * The child looks for this one variable, consumes it, and deletes it from its
 * own environment before running anything, so no descendant ever sees it.
 */
#define HL_WINDOWS_SPAWN_VARIABLE L"HL_HOST_WINDOWS_SPAWN"
#define HL_WINDOWS_SPAWN_VARIABLE_LENGTH 21u
#define HL_WINDOWS_SPAWN_MAGIC UINT64_C(0x484c53504157314e)

/*
 * Process exit codes this backend mints itself, encoding a Linux signal number
 * in the low byte. 0xE0000000 is the customer-defined NTSTATUS space (severity
 * "error" plus the customer bit), which the system is guaranteed never to use,
 * so these can never be confused with a real exception status or with a value a
 * program returned from main.
 */
#define HL_WINDOWS_EXIT_SIGNAL_BASE 0xE0484C00u
/* A child whose launch record was present but unusable. Deliberately not in the
 * signal space above: it is a spawn failure, not a death. */
#define HL_WINDOWS_EXIT_BOOTSTRAP 255u

/*
 * The launch record. Laid out with explicit widths and no pointers because it is
 * read by a different process; nothing in it is an address in the parent's
 * address space except entry_context, which the spawn path has already proved is
 * not one.
 */
typedef struct hl_windows_spawn_record {
    uint64_t magic;
    uint64_t nonce; /* must equal the value the environment variable carries */
    uint64_t entry_offset;
    uint64_t entry_context;
    uint64_t image_size; /* the parent's SizeOfImage, checked against the child's */
    /* Caller payload, stored immediately after this record inside the same
     * section. Zero when the caller published none. */
    uint64_t payload_size;
    /* A section the child re-maps for itself, carried as a handle VALUE for the
     * same reason the launch section's handle is: an inherited handle occupies
     * the same numeric slot in the child. Zero means none was handed over. */
    uint64_t shared_section;
    uint64_t shared_size;
    uint32_t record_size;
    uint32_t child_id; /* stamped after CreateProcess, before ResumeThread */
    volatile LONG claimed;
    uint32_t reserved;
} hl_windows_spawn_record;

/* The launch channel's two halves. The parent's is per-thread and lives exactly
 * from publish() to the spawn that consumes it; the child's is process-wide and
 * is written once, before anything else in this process has run. */
typedef struct hl_windows_launch_request {
    const void *bytes; /* borrowed: the publisher owns them across the spawn */
    size_t size;
    hl_host_handle shared;
} hl_windows_launch_request;

static _Thread_local hl_windows_launch_request hl_windows_launch_pending;

static const void *hl_windows_launch_bytes;
static size_t hl_windows_launch_bytes_size;
static void *hl_windows_launch_shared_view;
static size_t hl_windows_launch_shared_view_size;

/* --- small string helpers ---------------------------------------------------
 * Hand-rolled rather than taken from a library. wsprintfW would do the
 * formatting, but it lives in USER32, which pulls in IMM32, whose thread-detach
 * handler is not safe in every process shape this engine creates -- so the whole
 * backend stays clear of that import and pays ten lines for it here. */

static size_t hl_windows_wide_length(const WCHAR *text) {
    size_t length = 0;
    while (text[length] != L'\0')
        length++;
    return length;
}

static uint32_t hl_windows_format_hex(uint64_t value, WCHAR *out) {
    WCHAR digits[16];
    uint32_t length = 0;
    uint32_t index;
    do {
        const uint32_t nibble = (uint32_t)(value & UINT64_C(0xF));
        digits[length++] = (WCHAR)(nibble < 10u ? (uint32_t)L'0' + nibble : ((uint32_t)L'a' + nibble) - 10u);
        value >>= 4;
    } while (value != 0);
    for (index = 0; index < length; ++index)
        out[index] = digits[length - 1u - index];
    return length;
}

/* Returns 0 on a malformed or empty run of digits; *cursor is left on the first
 * character that is not a hex digit. */
static int hl_windows_parse_hex(const WCHAR *text, uint32_t *cursor, uint64_t *out) {
    uint64_t value = 0;
    uint32_t digits = 0;
    for (;;) {
        const WCHAR character = text[*cursor];
        uint32_t nibble;
        if (character >= L'0' && character <= L'9')
            nibble = (uint32_t)(character - L'0');
        else if (character >= L'a' && character <= L'f')
            nibble = (uint32_t)(character - L'a') + 10u;
        else
            break;
        if (digits >= 16u) return 0;
        value = (value << 4) | (uint64_t)nibble;
        digits++;
        (*cursor)++;
    }
    if (digits == 0) return 0;
    *out = value;
    return 1;
}

/* --- the image ---------------------------------------------------------------
 * SizeOfImage is read out of the loaded PE headers rather than queried, because
 * the only thing it is used for is bounding an offset that must land inside this
 * same module. Both sides read it the same way, so a mismatch means the child is
 * not the image the parent thought it launched. */
static uint64_t hl_windows_image_size(const void *base) {
    const IMAGE_DOS_HEADER *dos = base;
    const IMAGE_NT_HEADERS *headers;
    if (base == NULL || dos->e_magic != IMAGE_DOS_SIGNATURE || dos->e_lfanew <= 0) return 0;
    headers = (const IMAGE_NT_HEADERS *)(const void *)((const char *)base + dos->e_lfanew);
    if (headers->Signature != IMAGE_NT_SIGNATURE) return 0;
    return (uint64_t)headers->OptionalHeader.SizeOfImage;
}

/* --- the child side ----------------------------------------------------------
 *
 * A constructor rather than an exported function the runner has to call: the
 * child is our own image re-executed, and its main() knows nothing about having
 * been spawned. Running before main and never returning to it is what makes the
 * mechanism invisible to every consumer of this archive that does not spawn.
 *
 * Everything here is a refusal by default. The variable is deleted first, so any
 * process this one goes on to create is unaffected no matter which branch is
 * taken; a record not addressed to this process id is left alone and startup
 * continues normally; a record addressed to this process id that does not check
 * out exits immediately, because that state cannot be recovered from and a
 * silent fall-through to main would be reported to the parent as a plausible
 * exit code.
 */
static void hl_windows_process_bootstrap(void) __attribute__((constructor));

static void hl_windows_process_bootstrap(void) {
    WCHAR text[64];
    hl_windows_spawn_record *record;
    hl_host_process_entry entry;
    HANDLE section;
    uint64_t handle_value = 0;
    uint64_t nonce = 0;
    uint64_t entry_offset;
    uint64_t entry_context;
    uint64_t payload_size;
    uint64_t shared_section;
    uint64_t shared_size;
    uint32_t cursor = 0;
    int32_t code;
    const DWORD length =
        GetEnvironmentVariableW(HL_WINDOWS_SPAWN_VARIABLE, text, (DWORD)(sizeof(text) / sizeof(*text)));
    if (length == 0 || length >= sizeof(text) / sizeof(*text)) return;
    (void)SetEnvironmentVariableW(HL_WINDOWS_SPAWN_VARIABLE, NULL);
    if (!hl_windows_parse_hex(text, &cursor, &handle_value) || text[cursor] != L'.') return;
    cursor++;
    if (!hl_windows_parse_hex(text, &cursor, &nonce) || text[cursor] != L'\0') return;

    /* The handle value is the parent's, and it is meaningful here only because
     * an inherited handle keeps its number. Every field below is checked before
     * anything is called, so a number that is not our section fails a compare
     * instead of being trusted. */
    section = (HANDLE)(uintptr_t)handle_value;
    /* The whole section, not just the record: the caller's payload lives behind
     * it and stays mapped for the life of this process. */
    record = MapViewOfFile(section, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0);
    if (record == NULL) ExitProcess(HL_WINDOWS_EXIT_BOOTSTRAP);
    if (record->magic != HL_WINDOWS_SPAWN_MAGIC || record->record_size != (uint32_t)sizeof(*record) ||
        record->nonce != nonce) {
        (void)UnmapViewOfFile(record);
        ExitProcess(HL_WINDOWS_EXIT_BOOTSTRAP);
    }
    /* Addressed to a different process: this is the stale-inheritance case, and
     * the right answer is to leave the process alone. */
    if (record->child_id != GetCurrentProcessId()) {
        (void)UnmapViewOfFile(record);
        return;
    }
    if (InterlockedCompareExchange(&record->claimed, 1, 0) != 0 ||
        record->image_size != hl_windows_image_size(GetModuleHandleW(NULL)) ||
        record->entry_offset >= record->image_size) {
        (void)UnmapViewOfFile(record);
        ExitProcess(HL_WINDOWS_EXIT_BOOTSTRAP);
    }
    entry_offset = record->entry_offset;
    entry_context = record->entry_context;
    payload_size = record->payload_size;
    shared_section = record->shared_section;
    shared_size = record->shared_size;
    /* Closing the section handle does not unmap the view, so the payload survives
     * while the one reference this process no longer needs is released. */
    if (payload_size != 0) {
        hl_windows_launch_bytes = (const char *)(const void *)record + sizeof(*record);
        hl_windows_launch_bytes_size = (size_t)payload_size;
    } else {
        (void)UnmapViewOfFile(record);
    }
    (void)CloseHandle(section);

    /* A view of the parent's shared page, mapped wherever this process has room:
     * the parent's address for it means nothing here, which is exactly why the
     * handle rather than the address is what crossed. */
    if (shared_section != 0) {
        const HANDLE shared = (HANDLE)(uintptr_t)shared_section;
        void *view = MapViewOfFile(shared, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0);
        if (view == NULL) ExitProcess(HL_WINDOWS_EXIT_BOOTSTRAP);
        hl_windows_launch_shared_view = view;
        hl_windows_launch_shared_view_size = (size_t)shared_size;
        (void)CloseHandle(shared);
    }

    /* The parent created this process in its own group so that a later
     * terminate() can direct a console control event at it alone. A new group
     * starts with Ctrl+C *ignored*, which would make that event arrive and do
     * nothing, so the default disposition is restored here. */
    (void)SetConsoleCtrlHandler(NULL, FALSE);

    entry = (hl_host_process_entry)(uintptr_t)((uintptr_t)GetModuleHandleW(NULL) + (uintptr_t)entry_offset);
    code = entry((void *)(uintptr_t)entry_context);
    /* Masked to eight bits because that is what _exit() gives the POSIX backends,
     * and a guest exit status is a byte on every host. */
    ExitProcess((UINT)((uint32_t)code & 0xFFu));
}

const void *hl_host_windows_launch_payload(size_t *out_size) {
    if (out_size != NULL) *out_size = hl_windows_launch_bytes_size;
    return hl_windows_launch_bytes;
}

void *hl_host_windows_launch_shared(size_t *out_size) {
    if (out_size != NULL) *out_size = hl_windows_launch_shared_view_size;
    return hl_windows_launch_shared_view;
}

/* --- the parent side ---------------------------------------------------------
 *
 * Everything one spawn allocates, so that the twelve failure exits below have
 * one place to unwind through instead of twelve copies of the same six frees.
 */
typedef struct hl_windows_spawn_state {
    HANDLE section;
    HANDLE shared; /* inheritable duplicate of the caller's shared section */
    hl_windows_spawn_record *record;
    WCHAR *image;
    WCHAR *command;
    WCHAR *environment;
    LPPROC_THREAD_ATTRIBUTE_LIST attributes;
    /* the launch section, the optional shared section, then the duplicated
     * standard streams -- streams last, which is what lets the release below
     * name them by counting back from the end */
    HANDLE inherited[5];
    uint32_t inherited_count;
    uint32_t stream_count; /* how many of inherited[] are streams, i.e. owned dups */
} hl_windows_spawn_state;

static void hl_windows_spawn_release(hl_windows_spawn_state *state) {
    uint32_t index;
    for (index = 0; index < state->stream_count; ++index)
        (void)CloseHandle(state->inherited[state->inherited_count - state->stream_count + index]);
    if (state->attributes != NULL) {
        DeleteProcThreadAttributeList(state->attributes);
        free(state->attributes);
    }
    if (state->record != NULL) (void)UnmapViewOfFile(state->record);
    if (state->shared != NULL) (void)CloseHandle(state->shared);
    if (state->section != NULL) (void)CloseHandle(state->section);
    free(state->environment);
    free(state->command);
    free(state->image);
    memset(state, 0, sizeof(*state));
}

hl_status hl_host_windows_launch_publish(const void *bytes, size_t size, hl_host_handle shared) {
    if ((bytes == NULL) != (size == 0)) return HL_STATUS_INVALID_ARGUMENT;
    hl_windows_launch_pending.bytes = bytes;
    hl_windows_launch_pending.size = size;
    hl_windows_launch_pending.shared = shared;
    return HL_STATUS_OK;
}

/*
 * The caller's shared mapping, duplicated inheritable.
 *
 * Duplicating rather than flipping HANDLE_FLAG_INHERIT on the memory group's own
 * handle, for the reason the standard streams are duplicated: the group owns that
 * handle and every other CreateProcess in this process would see the change.
 * Only a mapping the memory group created SHARED has a section at all -- a
 * private one is committed pages with no object behind them -- so a request to
 * hand over a private mapping is refused rather than silently handed over empty.
 */
static hl_host_result hl_windows_spawn_share(hl_host_windows *host, hl_windows_spawn_state *state,
                                             hl_host_handle mapping) {
    const HANDLE self = GetCurrentProcess();
    hl_windows_handle_entry *entry;
    HANDLE source = NULL;
    uint64_t size = 0;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, mapping, HL_WINDOWS_HANDLE_MAPPING);
    if (entry != NULL) {
        source = entry->section;
        size = entry->size;
    }
    hl_windows_unlock(host);
    if (entry == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (source == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    if (!DuplicateHandle(self, source, self, &state->shared, 0, TRUE, DUPLICATE_SAME_ACCESS))
        return hl_windows_last_error_result();
    state->record->shared_section = (uint64_t)(uintptr_t)state->shared;
    state->record->shared_size = size;
    state->inherited[state->inherited_count++] = state->shared;
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* GetModuleFileNameW has no "how long is it" form: it truncates and reports the
 * buffer size it filled, so the only way to know it fit is to see a shorter
 * answer than the buffer. */
static WCHAR *hl_windows_module_path(void) {
    uint32_t capacity = MAX_PATH;
    for (;;) {
        WCHAR *buffer = calloc(capacity, sizeof(*buffer));
        DWORD length;
        if (buffer == NULL) return NULL;
        length = GetModuleFileNameW(NULL, buffer, capacity);
        if (length != 0 && length < capacity) return buffer;
        free(buffer);
        if (capacity >= (1u << 16)) return NULL;
        capacity *= 2u;
    }
}

static int hl_windows_is_spawn_variable(const WCHAR *text) {
    static const WCHAR name[] = HL_WINDOWS_SPAWN_VARIABLE;
    uint32_t index;
    for (index = 0; index < HL_WINDOWS_SPAWN_VARIABLE_LENGTH; ++index)
        if (text[index] != name[index]) return 0;
    return text[HL_WINDOWS_SPAWN_VARIABLE_LENGTH] == L'=';
}

/*
 * The child's environment is built explicitly rather than by setting the
 * variable on ourselves and passing NULL. Two reasons, both real: a process-wide
 * SetEnvironmentVariableW would be visible to every other thread and to any
 * concurrent spawn, and it would leave the marker behind on the parent if the
 * spawn failed between the set and the clear.
 *
 * Any pre-existing marker is dropped on the way through, so a nested engine
 * never carries its parent's record forward.
 */
static WCHAR *hl_windows_child_environment(const WCHAR *addition) {
    const size_t addition_length = hl_windows_wide_length(addition);
    WCHAR *source = GetEnvironmentStringsW();
    WCHAR *block;
    const WCHAR *cursor;
    size_t total = addition_length + 2u; /* the addition's NUL, and the block's */
    size_t offset = 0;
    if (source == NULL) return NULL;
    for (cursor = source; *cursor != L'\0'; cursor += hl_windows_wide_length(cursor) + 1u)
        if (!hl_windows_is_spawn_variable(cursor)) total += hl_windows_wide_length(cursor) + 1u;
    block = calloc(total, sizeof(*block));
    if (block != NULL) {
        for (cursor = source; *cursor != L'\0'; cursor += hl_windows_wide_length(cursor) + 1u) {
            const size_t length = hl_windows_wide_length(cursor) + 1u;
            if (hl_windows_is_spawn_variable(cursor)) continue;
            memcpy(&block[offset], cursor, length * sizeof(*block));
            offset += length;
        }
        memcpy(&block[offset], addition, (addition_length + 1u) * sizeof(*block));
    }
    (void)FreeEnvironmentStringsW(source);
    return block;
}

/*
 * The standard streams, duplicated inheritable. A handle already marked
 * inheritable would work as it stands, but duplicating unconditionally keeps the
 * parent's own handle flags untouched -- flipping HANDLE_FLAG_INHERIT on
 * GetStdHandle(STD_ERROR_HANDLE) would change how every *other* CreateProcess in
 * this process behaves, which is not this function's decision to make.
 *
 * All three or none: STARTF_USESTDHANDLES is all-or-nothing, and a child given
 * two of three streams is a worse outcome than one that inherits the parent's
 * defaults.
 */
static int hl_windows_duplicate_streams(hl_windows_spawn_state *state, STARTUPINFOW *startup) {
    static const DWORD identifiers[3] = {STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE};
    HANDLE copies[3] = {NULL, NULL, NULL};
    const HANDLE self = GetCurrentProcess();
    uint32_t index;
    for (index = 0; index < 3u; ++index) {
        const HANDLE source = GetStdHandle(identifiers[index]);
        if (source == NULL || source == INVALID_HANDLE_VALUE ||
            !DuplicateHandle(self, source, self, &copies[index], 0, TRUE, DUPLICATE_SAME_ACCESS)) {
            uint32_t undo;
            for (undo = 0; undo < index; ++undo)
                (void)CloseHandle(copies[undo]);
            return 0;
        }
    }
    for (index = 0; index < 3u; ++index)
        state->inherited[state->inherited_count++] = copies[index];
    state->stream_count = 3u;
    startup->dwFlags |= STARTF_USESTDHANDLES;
    startup->hStdInput = copies[0];
    startup->hStdOutput = copies[1];
    startup->hStdError = copies[2];
    return 1;
}

/*
 * Is entry_context something the child can dereference?
 *
 * The child has none of the parent's private memory, so the answer is yes only
 * when the value is not an address at all. VirtualQuery is the exact test: a
 * committed page is one the parent can read and the child cannot, and anything
 * else -- free, reserved-but-uncommitted, or outside the address space entirely
 * -- is a scalar that a caller stuffed into a pointer, which crosses by value.
 *
 * Refusing here, in the parent, is the whole point. The alternative is a child
 * that faults on its first dereference and a wait() that reports a segmentation
 * fault the caller has no way to attribute.
 */
static int hl_windows_context_crosses(void *entry_context) {
    MEMORY_BASIC_INFORMATION information;
    if (entry_context == NULL) return 1;
    if (VirtualQuery(entry_context, &information, sizeof(information)) == 0) return 1;
    return information.State != MEM_COMMIT;
}

static uint64_t hl_windows_spawn_nonce(void) {
    static volatile LONG64 counter;
    LARGE_INTEGER now;
    if (!QueryPerformanceCounter(&now)) now.QuadPart = 0;
    return ((uint64_t)GetCurrentProcessId() << 32) ^ (uint64_t)now.QuadPart ^
           (uint64_t)InterlockedIncrement64(&counter) * UINT64_C(0x9E3779B97F4A7C15);
}

static hl_host_result hl_windows_process_launch(hl_host_windows *host, hl_host_process_entry entry,
                                                void *entry_context) {
    hl_windows_spawn_state state;
    STARTUPINFOEXW startup;
    PROCESS_INFORMATION process;
    SECURITY_ATTRIBUTES security;
    WCHAR variable[HL_WINDOWS_SPAWN_VARIABLE_LENGTH + 36u];
    hl_host_result result;
    hl_windows_handle_entry *slot;
    SIZE_T attribute_size = 0;
    const void *base = GetModuleHandleW(NULL);
    const uint64_t image_size = hl_windows_image_size(base);
    const uint64_t entry_offset = (uint64_t)((uintptr_t)entry - (uintptr_t)base);
    /* Taken and cleared before the first failure exit: a publication belongs to
     * exactly one spawn, and a spawn that fails must not leave it for the next. */
    const hl_windows_launch_request request = hl_windows_launch_pending;
    uint64_t section_size;
    uint32_t length;
    size_t command_size;

    memset(&hl_windows_launch_pending, 0, sizeof(hl_windows_launch_pending));
    if (entry == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* An entry point outside this image cannot be named by an offset from its
     * base, and this backend has no other way to name code to a fresh process. */
    if (image_size == 0 || entry_offset >= image_size) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    if (!hl_windows_context_crosses(entry_context)) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    /* The section is one 64-bit object and its size is a DWORD pair below, so a
     * payload that cannot be described that way is refused rather than truncated. */
    if (request.size > UINT64_C(0xFFFFFFFF)) return hl_windows_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    section_size = (uint64_t)sizeof(*state.record) + (uint64_t)request.size;

    memset(&state, 0, sizeof(state));
    memset(&startup, 0, sizeof(startup));
    memset(&process, 0, sizeof(process));
    startup.StartupInfo.cb = (DWORD)sizeof(startup);

    security.nLength = (DWORD)sizeof(security);
    security.lpSecurityDescriptor = NULL;
    security.bInheritHandle = TRUE;
    state.section = CreateFileMappingW(INVALID_HANDLE_VALUE, &security, PAGE_READWRITE, (DWORD)(section_size >> 32),
                                       (DWORD)(section_size & UINT64_C(0xFFFFFFFF)), NULL);
    if (state.section == NULL) {
        result = hl_windows_last_error_result();
        hl_windows_spawn_release(&state);
        return result;
    }
    state.record = MapViewOfFile(state.section, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, (SIZE_T)section_size);
    state.image = hl_windows_module_path();
    if (state.record == NULL || state.image == NULL) {
        result =
            state.record == NULL ? hl_windows_last_error_result() : hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        hl_windows_spawn_release(&state);
        return result;
    }
    memset(state.record, 0, sizeof(*state.record));
    state.record->magic = HL_WINDOWS_SPAWN_MAGIC;
    state.record->nonce = hl_windows_spawn_nonce();
    state.record->entry_offset = entry_offset;
    state.record->entry_context = (uint64_t)(uintptr_t)entry_context;
    state.record->image_size = image_size;
    state.record->payload_size = (uint64_t)request.size;
    state.record->record_size = (uint32_t)sizeof(*state.record);
    /* Copied, not referenced: the publisher owns its bytes only across this call,
     * and after this memcpy the child's copy is the only one it depends on. */
    if (request.size != 0) memcpy((char *)(void *)state.record + sizeof(*state.record), request.bytes, request.size);

    /* CreateProcessW may write to lpCommandLine, so it gets its own copy even
     * though the two strings are identical here. argv[0] is the image path,
     * which is what a re-executed engine would see anyway. */
    command_size = hl_windows_wide_length(state.image) + 1u;
    state.command = calloc(command_size, sizeof(*state.command));
    memcpy(&variable[0], HL_WINDOWS_SPAWN_VARIABLE, HL_WINDOWS_SPAWN_VARIABLE_LENGTH * sizeof(*variable));
    length = HL_WINDOWS_SPAWN_VARIABLE_LENGTH;
    variable[length++] = L'=';
    length += hl_windows_format_hex((uint64_t)(uintptr_t)state.section, &variable[length]);
    variable[length++] = L'.';
    length += hl_windows_format_hex(state.record->nonce, &variable[length]);
    variable[length] = L'\0';
    if (state.command != NULL) {
        memcpy(state.command, state.image, command_size * sizeof(*state.command));
        state.environment = hl_windows_child_environment(variable);
    }
    if (state.command == NULL || state.environment == NULL) {
        hl_windows_spawn_release(&state);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }

    state.inherited[state.inherited_count++] = state.section;
    if (request.shared != HL_HOST_HANDLE_INVALID) {
        result = hl_windows_spawn_share(host, &state, request.shared);
        if (result.status != HL_STATUS_OK) {
            hl_windows_spawn_release(&state);
            return result;
        }
    }
    (void)hl_windows_duplicate_streams(&state, &startup.StartupInfo);

    /* The documented two-call form: the first call always fails, and its purpose
     * is to report the size. */
    if (InitializeProcThreadAttributeList(NULL, 1, 0, &attribute_size) || GetLastError() != ERROR_INSUFFICIENT_BUFFER) {
        result = hl_windows_last_error_result();
        hl_windows_spawn_release(&state);
        return result;
    }
    state.attributes = malloc(attribute_size);
    if (state.attributes == NULL) {
        hl_windows_spawn_release(&state);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    if (!InitializeProcThreadAttributeList(state.attributes, 1, 0, &attribute_size)) {
        result = hl_windows_last_error_result();
        free(state.attributes);
        state.attributes = NULL;
        hl_windows_spawn_release(&state);
        return result;
    }
    if (!UpdateProcThreadAttribute(state.attributes, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, state.inherited,
                                   (SIZE_T)state.inherited_count * sizeof(state.inherited[0]), NULL, NULL)) {
        result = hl_windows_last_error_result();
        hl_windows_spawn_release(&state);
        return result;
    }
    startup.lpAttributeList = state.attributes;

    /*
     * bInheritHandles is TRUE *and* the list is present: the flag alone inherits
     * every inheritable handle, and the list alone does nothing without the flag.
     * Together they mean "these and only these".
     *
     * CREATE_NEW_PROCESS_GROUP is what makes terminate(INTERRUPT) addressable --
     * a console control event names a process group, and without a group of its
     * own the child could only be reached by an event that also hit this process.
     * The cost is that the child starts with Ctrl+C ignored, which the child-side
     * bootstrap undoes.
     */
    if (!CreateProcessW(state.image, state.command, NULL, NULL, TRUE,
                        CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NEW_PROCESS_GROUP |
                            EXTENDED_STARTUPINFO_PRESENT,
                        state.environment, NULL, &startup.StartupInfo, &process)) {
        result = hl_windows_last_error_result();
        hl_windows_spawn_release(&state);
        return result;
    }
    /* Stamped while the child is still suspended, so it is visible to the first
     * instruction the child runs and to nothing before it. */
    state.record->child_id = process.dwProcessId;

    result = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_PROCESS);
    if (result.status == HL_STATUS_OK && ResumeThread(process.hThread) == (DWORD)-1)
        result = hl_windows_last_error_result();
    (void)CloseHandle(process.hThread);
    if (result.status != HL_STATUS_OK) {
        (void)TerminateProcess(process.hProcess, HL_WINDOWS_EXIT_SIGNAL_BASE + 9u);
        (void)CloseHandle(process.hProcess);
        hl_windows_spawn_release(&state);
        return result;
    }
    hl_windows_lock(host);
    slot = hl_windows_lookup_locked(host, result.value, HL_WINDOWS_HANDLE_PROCESS);
    if (slot != NULL) {
        slot->object = process.hProcess;
        slot->process_id = process.dwProcessId;
    }
    hl_windows_unlock(host);
    if (slot == NULL) {
        (void)TerminateProcess(process.hProcess, HL_WINDOWS_EXIT_SIGNAL_BASE + 9u);
        (void)CloseHandle(process.hProcess);
        hl_windows_spawn_release(&state);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    /* The parent's section handle and view go here rather than at close: the
     * child holds an inherited reference of its own, so the object outlives this
     * release, and holding a second handle per live child for no reason would be
     * a leak with a long half-life. */
    hl_windows_spawn_release(&state);
    return result;
}

/* --- exit status -------------------------------------------------------------
 *
 * Windows reports an unhandled exception as the process exit code, so the exit
 * code is the only channel a signal-shaped death can arrive on. Only codes that
 * are unambiguously an unhandled exception status are read that way; anything
 * else is reported as an exit code, because a program is free to return any
 * 32-bit value from main and guessing would corrupt the ordinary case.
 */
static int hl_windows_exit_signal(DWORD code, uint64_t *signal_number) {
    if (code > HL_WINDOWS_EXIT_SIGNAL_BASE && code <= HL_WINDOWS_EXIT_SIGNAL_BASE + 64u) {
        *signal_number = code - HL_WINDOWS_EXIT_SIGNAL_BASE;
        return 1;
    }
    switch (code) {
    case 0xC0000005u: /* ACCESS_VIOLATION */
    case 0xC000008Cu: /* ARRAY_BOUNDS_EXCEEDED */
    case 0xC00000FDu: /* STACK_OVERFLOW */ *signal_number = 11u; return 1;
    case 0x80000002u: /* DATATYPE_MISALIGNMENT */
    case 0xC0000006u: /* IN_PAGE_ERROR */ *signal_number = 7u; return 1;
    case 0x80000003u: /* BREAKPOINT */
    case 0x80000004u: /* SINGLE_STEP */ *signal_number = 5u; return 1;
    case 0xC000001Du: /* ILLEGAL_INSTRUCTION */
    case 0xC0000025u: /* NONCONTINUABLE_EXCEPTION */
    case 0xC0000026u: /* INVALID_DISPOSITION */
    case 0xC0000096u: /* PRIVILEGED_INSTRUCTION */ *signal_number = 4u; return 1;
    case 0xC000008Du: /* FLOAT_DENORMAL_OPERAND */
    case 0xC000008Eu: /* FLOAT_DIVIDE_BY_ZERO */
    case 0xC000008Fu: /* FLOAT_INEXACT_RESULT */
    case 0xC0000090u: /* FLOAT_INVALID_OPERATION */
    case 0xC0000091u: /* FLOAT_OVERFLOW */
    case 0xC0000092u: /* FLOAT_STACK_CHECK */
    case 0xC0000093u: /* FLOAT_UNDERFLOW */
    case 0xC0000094u: /* INTEGER_DIVIDE_BY_ZERO */
    case 0xC0000095u: /* INTEGER_OVERFLOW */ *signal_number = 8u; return 1;
    case 0x40010005u: /* DBG_CONTROL_C */
    case 0xC000013Au: /* CONTROL_C_EXIT */ *signal_number = 2u; return 1;
    case 0xC0000409u: /* STACK_BUFFER_OVERRUN */
    case 0xC0000602u: /* FAIL_FAST_EXCEPTION */ *signal_number = 6u; return 1;
    default: return 0;
    }
}

static hl_host_result hl_windows_process_retained(const hl_windows_handle_entry *entry) {
    return hl_windows_result(HL_STATUS_OK, entry->process_exit_value, entry->process_exit_kind);
}

static uint64_t hl_windows_monotonic_now(hl_host_windows *host) {
    const hl_host_result now = hl_windows_clock_services.monotonic_ns(host);
    return now.status == HL_STATUS_OK ? now.value : 0;
}

/*
 * deadline_ns is absolute on the host monotonic clock, so it is converted to a
 * relative millisecond timeout on each pass and the loop re-checks the clock
 * after every WAIT_TIMEOUT. Rounding up and re-checking is what keeps the call
 * from returning before the deadline: a bare (deadline - now) / 1e6 would
 * truncate and could report WOULD_BLOCK up to a millisecond early.
 */
static DWORD hl_windows_process_wait_for(hl_host_windows *host, HANDLE object, uint64_t deadline_ns) {
    for (;;) {
        DWORD timeout = INFINITE;
        DWORD waited;
        if (deadline_ns != HL_HOST_DEADLINE_INFINITE) {
            const uint64_t now = hl_windows_monotonic_now(host);
            const uint64_t remaining = now >= deadline_ns ? 0u : (deadline_ns - now + 999999u) / 1000000u;
            timeout = remaining >= (uint64_t)INFINITE ? INFINITE - 1u : (DWORD)remaining;
        }
        waited = WaitForSingleObject(object, timeout);
        if (waited != WAIT_TIMEOUT || deadline_ns == HL_HOST_DEADLINE_INFINITE) return waited;
        if (deadline_ns == 0 || hl_windows_monotonic_now(host) >= deadline_ns) return WAIT_TIMEOUT;
    }
}

static hl_host_result hl_windows_process_wait(void *context, hl_host_handle handle, uint64_t deadline_ns) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    HANDLE object;
    DWORD waited;
    DWORD code = 0;
    hl_host_result result;

    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_PROCESS);
    if (entry == NULL || host->destroying) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if ((entry->process_state & HL_WINDOWS_PROCESS_REAPED) != 0) {
        result = hl_windows_process_retained(entry);
        hl_windows_unlock(host);
        return result;
    }
    /* The waiter count is what close() refuses on, so the native handle can be
     * used outside the lock without a duplicate: no close can retire it while
     * this count is non-zero. */
    entry->process_waiters++;
    object = entry->object;
    hl_windows_unlock(host);

    waited = hl_windows_process_wait_for(host, object, deadline_ns);
    if (waited == WAIT_OBJECT_0 && !GetExitCodeProcess(object, &code)) waited = WAIT_FAILED;

    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_PROCESS);
    if (entry != NULL) {
        if (entry->process_waiters != 0) entry->process_waiters--;
        /* First waiter home records the completion; every later one -- and every
         * concurrent one that woke on the same exit -- reads that record, so all
         * of them return the identical value and kind. */
        if (waited == WAIT_OBJECT_0 && (entry->process_state & HL_WINDOWS_PROCESS_REAPED) == 0) {
            uint64_t signal_number = 0;
            if (hl_windows_exit_signal(code, &signal_number)) {
                entry->process_exit_kind = HL_HOST_PROCESS_EXIT_SIGNAL;
                entry->process_exit_value = signal_number;
            } else {
                entry->process_exit_kind = HL_HOST_PROCESS_EXIT_CODE;
                entry->process_exit_value = code;
            }
            entry->process_state |= HL_WINDOWS_PROCESS_REAPED;
        }
    }
    if (waited == WAIT_OBJECT_0 && entry != NULL) result = hl_windows_process_retained(entry);
    hl_windows_unlock(host);

    if (waited == WAIT_OBJECT_0) return entry != NULL ? result : hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (waited == WAIT_TIMEOUT) return hl_windows_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    return hl_windows_last_error_result();
}

/*
 * What Windows can express, and what it cannot.
 *
 * INTERRUPT is a console control event, which is a genuine equivalent: the child
 * is asked to stop, it can install a handler and decline, and if it does not it
 * dies with CONTROL_C_EXIT -- which wait() reports as SIGINT. FORCE is
 * TerminateProcess, which is exactly SIGKILL's contract: immediate, unmaskable,
 * no handler runs.
 *
 * The signal form is accepted for those same two numbers and refused for every
 * other one, with HL_STATUS_NOT_SUPPORTED. That refusal is the honest answer.
 * TerminateProcess is not SIGTERM: SIGTERM is a request a process may catch,
 * block, or use to run its shutdown path, and reporting "delivered" for a call
 * that instead destroyed the process would be a lie the caller cannot detect.
 * Windows has no mechanism that delivers an arbitrary catchable signal to
 * another process, so there is nothing to map it onto.
 */
static hl_host_result hl_windows_process_terminate(void *context, hl_host_handle handle, uint32_t reason) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    HANDLE object;
    DWORD process_id;
    int force;
    if (reason != HL_HOST_PROCESS_TERMINATE_INTERRUPT && reason != HL_HOST_PROCESS_TERMINATE_FORCE &&
        (reason <= HL_HOST_PROCESS_TERMINATE_SIGNAL || reason > HL_HOST_PROCESS_TERMINATE_SIGNAL + 64u))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (reason == HL_HOST_PROCESS_TERMINATE_FORCE || reason == HL_HOST_PROCESS_TERMINATE_SIGNAL + 9u)
        force = 1;
    else if (reason == HL_HOST_PROCESS_TERMINATE_INTERRUPT || reason == HL_HOST_PROCESS_TERMINATE_SIGNAL + 2u)
        force = 0;
    else
        return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, reason - HL_HOST_PROCESS_TERMINATE_SIGNAL);

    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_PROCESS);
    if (entry == NULL || host->destroying || (entry->process_state & HL_WINDOWS_PROCESS_REAPED) != 0) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    /* Held across the call rather than borrowed: TerminateProcess on a stale
     * handle would be a signal delivered to whatever process id got reused, and
     * close() is the only thing this lock excludes. */
    object = entry->object;
    process_id = entry->process_id;
    if (force) {
        if (!TerminateProcess(object, HL_WINDOWS_EXIT_SIGNAL_BASE + 9u)) {
            hl_windows_unlock(host);
            return hl_windows_last_error_result();
        }
    } else if (!GenerateConsoleCtrlEvent(CTRL_C_EVENT, process_id)) {
        hl_windows_unlock(host);
        return hl_windows_last_error_result();
    }
    hl_windows_unlock(host);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/*
 * Retiring the slot is also what releases the retained completion, so close is
 * refused until there is one and until no waiter is still inside wait(). That is
 * the same rule the POSIX backends apply, and for the same reason: a close that
 * ran first would leave a concurrent waiter holding a handle to a reaped process
 * with nowhere to record its answer.
 */
static hl_host_result hl_windows_process_close(void *context, hl_host_handle handle) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    HANDLE object;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_PROCESS);
    if (entry == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (host->destroying || (entry->process_state & HL_WINDOWS_PROCESS_REAPED) == 0 || entry->process_waiters != 0) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_BUSY, 0, 0);
    }
    object = entry->object;
    hl_windows_clear_entry_locked(entry);
    hl_windows_unlock(host);
    if (object != NULL && !CloseHandle(object)) return hl_windows_last_error_result();
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/*
 * spawn_cloned takes the host's fork bracket itself; spawn_prepared is called
 * with that bracket already held by the caller and is required to complete it.
 * On this host the bracket is the sync registry's lock and there is nothing to
 * quiesce -- a fresh process inherits no locks -- but it is still taken and
 * released honestly, because a caller that used it to serialise something of its
 * own is entitled to that serialisation.
 *
 * A bracket that fails to complete after the child already exists is settled by
 * retiring the child: the caller is being told the spawn failed, so it will
 * never wait on the handle, and a process nobody will reap is worse than a
 * process that never ran.
 */
static hl_host_result hl_windows_process_settle(hl_host_windows *host, hl_host_result spawned,
                                                hl_host_result completed) {
    if (spawned.status != HL_STATUS_OK) return spawned;
    if (completed.status == HL_STATUS_OK) return spawned;
    (void)hl_windows_process_terminate(host, spawned.value, HL_HOST_PROCESS_TERMINATE_FORCE);
    (void)hl_windows_process_wait(host, spawned.value, HL_HOST_DEADLINE_INFINITE);
    (void)hl_windows_process_close(host, spawned.value);
    return completed;
}

static hl_host_result hl_windows_process_spawn_cloned(void *context, hl_host_process_entry entry, void *entry_context) {
    hl_host_windows *host = context;
    const hl_host_result armed = hl_host_sync_fork_prepare(host->sync);
    hl_host_result spawned;
    if (armed.status != HL_STATUS_OK) return armed;
    spawned = hl_windows_process_launch(host, entry, entry_context);
    return hl_windows_process_settle(host, spawned, hl_host_sync_fork_complete(host->sync));
}

static hl_host_result hl_windows_process_spawn_prepared(void *context, hl_host_process_entry entry,
                                                        void *entry_context) {
    hl_host_windows *host = context;
    const hl_host_result spawned = hl_windows_process_launch(host, entry, entry_context);
    return hl_windows_process_settle(host, spawned, hl_host_sync_fork_complete(host->sync));
}

const hl_host_process_services hl_windows_process_services = {.abi = HL_HOST_PROCESS_ABI,
                                                              .size = sizeof(hl_host_process_services),
                                                              .spawn_cloned = hl_windows_process_spawn_cloned,
                                                              .wait = hl_windows_process_wait,
                                                              .terminate = hl_windows_process_terminate,
                                                              .close = hl_windows_process_close,
                                                              .spawn_prepared = hl_windows_process_spawn_prepared};

/*
 * A pidfd-shaped watch on another process.
 *
 * REFUSAL, and the reason is the readiness boundary rather than the watch. The
 * contract is a close-on-exec DESCRIPTOR that becomes persistently readable when
 * the process exits -- Linux's pidfd_open(2). Windows has the watchable object
 * (a process HANDLE is signalled on exit, permanently, which is the same
 * edge-then-level shape) but no way to present it as a descriptor that joins the
 * engine's readiness set, because that set is a mixed one and this host has no
 * single call that waits on a mixed set. The same absence host_poll.h records.
 *
 * The typed process group already exposes wait-for-exit over the handle it
 * created, so a caller that wants a child's status has a supported path; what is
 * refused here is specifically the poll-alongside-everything-else form.
 */
int hl_host_process_open(pid_t pid) {
    (void)pid;
    errno = ENOSYS;
    return -1;
}

/* ==========================================================================
 * Guest fork(2).
 *
 * The file header above says the launch callbacks are not an address-space
 * clone and that guest fork(2) is a different problem. This is that problem,
 * solved here because it is the same namespace -- a child process and a way to
 * wait for it -- and because putting it anywhere else would mean a second
 * process table.
 *
 * The primitive is ntdll's RtlCloneUserProcess, which is the NT kernel's own
 * fork: it duplicates the address space copy-on-write and returns twice, once
 * in the parent with a handle and a client id, once on the calling thread of a
 * brand new process with STATUS_PROCESS_CLONED. Only the calling thread is
 * carried over, which is exactly fork(2)'s rule, so the engine's existing
 * fork-child repair hooks -- written for that rule on the POSIX hosts -- apply
 * here unchanged.
 *
 * Four properties this depends on, each measured against this engine's real
 * process shape (a live dual-alias JIT arena, shared ledger sections, peer
 * threads holding locks, __thread storage, a dirty private heap) before a line
 * of this was written:
 *
 *   - every region lands at a byte-identical virtual address in the child, so
 *     the engine's pointer globals -- which are copied verbatim -- stay valid;
 *   - pagefile-backed section views (CreateFileMappingW + MapViewOfFile3, which
 *     is how every shared ledger arena and the JIT arena are built here) survive
 *     the clone GENUINELY SHARED, in both directions;
 *   - an inherited handle keeps its numeric value, so no handle fixup is
 *     needed; a non-inheritable one leaves an EMPTY slot rather than a stale
 *     object;
 *   - threads created inside the clone work, provided the image does not import
 *     USER32 (IMM32's thread-detach handler access-violates in a clone). That is
 *     why the import gate exists and why this file hand-rolls the string helpers
 *     above rather than calling wsprintfW.
 *
 * The function is resolved by name rather than imported. ntdll is permitted to
 * this image, but RtlCloneUserProcess is in no import library the toolchain
 * ships, and GetProcAddress on an already-resident module costs one lookup once.
 */

#define HL_WINDOWS_CLONE_INHERIT_HANDLES 0x00000002u
#define HL_WINDOWS_STATUS_PROCESS_CLONED ((LONG)0x00000129)

/* CLIENT_ID and RTL_USER_PROCESS_INFORMATION, spelled locally: winternl.h does
 * not declare the second at all, and owning the declaration keeps the layout
 * visible at the one place it matters. A ULONG followed by pointers, so the
 * natural 64-bit padding is exactly what ntdll writes. */
typedef struct hl_windows_client_id {
    HANDLE unique_process;
    HANDLE unique_thread;
} hl_windows_client_id;

typedef struct hl_windows_clone_information {
    ULONG length;
    HANDLE process;
    HANDLE thread;
    hl_windows_client_id client;
    /* SECTION_IMAGE_INFORMATION. Never read here, but ntdll fills it, so the
     * space has to exist and has to be large enough: the 64-bit layout is 0x48
     * bytes and this is 128. */
    ULONG_PTR image_information[16];
} hl_windows_clone_information;

typedef LONG(NTAPI *hl_windows_clone_process_fn)(ULONG, void *, void *, HANDLE, hl_windows_clone_information *);

/*
 * The child table. A fixed array rather than a growing one for two reasons that
 * both come from the clone: it must be usable from a child that has just
 * appeared with no allocator invariant re-established, and it must not add dirty
 * pages to the address space -- untouched BSS costs nothing to clone, and clone
 * cost is linear in dirty bytes.
 *
 * Capacity is a real limit: a guest holding this many unreaped children gets
 * EAGAIN from fork, which is the errno Linux gives when it hits RLIMIT_NPROC.
 */
#define HL_WINDOWS_CHILD_CAPACITY 1024u

typedef struct hl_windows_child {
    DWORD id;
    HANDLE process;
    unsigned char used;
} hl_windows_child;

static hl_windows_child hl_windows_children[HL_WINDOWS_CHILD_CAPACITY];
static SRWLOCK hl_windows_children_lock = SRWLOCK_INIT;
/* Rotates the scan start so a guest with more than MAXIMUM_WAIT_OBJECTS live
 * children cannot starve the ones past the first 64. */
static unsigned hl_windows_child_cursor;

static hl_windows_clone_process_fn hl_windows_clone_process(void) {
    static hl_windows_clone_process_fn resolved;
    HMODULE ntdll;
    if (resolved != NULL) return resolved;
    /* ntdll is mapped into every process before any user code runs, so this is a
     * lookup and never a load -- which matters, because LoadLibrary does not
     * work inside a clone. */
    ntdll = GetModuleHandleW(L"ntdll.dll");
    if (ntdll == NULL) return NULL;
    resolved = (hl_windows_clone_process_fn)(void *)GetProcAddress(ntdll, "RtlCloneUserProcess");
    return resolved;
}

/*
 * The child's first act. No lock is taken and none can be: a peer thread of the
 * parent may have held this lock at the clone instant and no thread survives to
 * release it, so the only correct move is to overwrite it. That is safe for the
 * same reason it is necessary -- a clone carries exactly one thread, so nothing
 * else in this process can be looking at the table.
 *
 * The entries are dropped rather than closed. A process handle is created
 * non-inheritable, so in the child the numeric slot is EMPTY, not a live
 * duplicate; CloseHandle on it would at best fail and at worst close whatever a
 * later CreateFileW happened to be given that value. Dropping is also the right
 * SEMANTICS: a fork child does not inherit its parent's children, and a wait in
 * the child must answer ECHILD for them.
 */
void hl_host_windows_fork_child_reset(void) {
    unsigned index;
    InitializeSRWLock(&hl_windows_children_lock);
    for (index = 0; index < HL_WINDOWS_CHILD_CAPACITY; index++) {
        hl_windows_children[index].used = 0;
        hl_windows_children[index].process = NULL;
        hl_windows_children[index].id = 0;
    }
    hl_windows_child_cursor = 0;
}

int hl_host_windows_fork(void) {
    const hl_windows_clone_process_fn clone = hl_windows_clone_process();
    hl_windows_clone_information information;
    unsigned slot;
    LONG status;

    if (clone == NULL) {
        errno = ENOSYS;
        return -1;
    }
    /* The bookkeeping slot is claimed BEFORE the clone. Claiming it afterwards
     * would mean discovering the table was full with a child already running,
     * and the only way out of that is to kill a process the caller never learned
     * about. The child inherits the claim and immediately discards the whole
     * table, so the reservation costs it nothing. */
    AcquireSRWLockExclusive(&hl_windows_children_lock);
    for (slot = 0; slot < HL_WINDOWS_CHILD_CAPACITY; slot++)
        if (!hl_windows_children[slot].used) break;
    if (slot == HL_WINDOWS_CHILD_CAPACITY) {
        ReleaseSRWLockExclusive(&hl_windows_children_lock);
        errno = EAGAIN;
        return -1;
    }
    hl_windows_children[slot].used = 1;
    hl_windows_children[slot].process = NULL;
    hl_windows_children[slot].id = 0;
    ReleaseSRWLockExclusive(&hl_windows_children_lock);

    memset(&information, 0, sizeof information);
    /* No NO_SYNCHRONIZE: the default takes the loader, PEB, heap and TLS locks
     * around the clone, which is what leaves the child's ntdll state consistent
     * when a peer thread was inside one of them. It is measurably cheaper too. */
    status = clone(HL_WINDOWS_CLONE_INHERIT_HANDLES, NULL, NULL, NULL, &information);

    if (status == HL_WINDOWS_STATUS_PROCESS_CLONED) {
        hl_host_windows_fork_child_reset();
        return 0;
    }
    if (status < 0) {
        AcquireSRWLockExclusive(&hl_windows_children_lock);
        hl_windows_children[slot].used = 0;
        ReleaseSRWLockExclusive(&hl_windows_children_lock);
        /* Every documented failure of this call is a resource one, and EAGAIN is
         * what fork(2) reports for all of them. */
        errno = EAGAIN;
        return -1;
    }
    AcquireSRWLockExclusive(&hl_windows_children_lock);
    hl_windows_children[slot].id = (DWORD)(ULONG_PTR)information.client.unique_process;
    hl_windows_children[slot].process = information.process;
    ReleaseSRWLockExclusive(&hl_windows_children_lock);
    /* The initial thread handle is not kept: the process handle is what carries
     * exit and it is what a wait needs. */
    if (information.thread != NULL) (void)CloseHandle(information.thread);
    return (int)(DWORD)(ULONG_PTR)information.client.unique_process;
}

/*
 * A Windows exit code is a bare DWORD -- no signal, no stop, no core bit -- so
 * the Linux status word is built here rather than decoded. hl_windows_exit_signal
 * is the same decoder the typed process group uses, so a child killed through
 * terminate() and a child reaped through the wait below agree about what
 * happened.
 *
 * The core bit is deliberately NOT set: whether a Linux death dumps core is a
 * function of the GUEST's RLIMIT_CORE, and the ABI layer's wait4 synthesizes it
 * from exactly that after this returns. Setting it here would be guessing over
 * the top of an answer that is already correct.
 */
static int hl_windows_wait_status(DWORD code) {
    uint64_t signal_number = 0;
    if (hl_windows_exit_signal(code, &signal_number)) return (int)(signal_number & 0x7fu);
    return (int)((code & 0xffu) << 8);
}

/* Collect the exit of one slot whose process object is already signalled. Called
 * with the lock held, and re-validates the slot because two guest threads can
 * observe the same child exit while only one of them may reap it.
 *
 * `release` is WNOWAIT inverted: a peeking wait reads the status and leaves the
 * entry, so the same child can be reaped again afterwards. Windows makes that
 * free -- the process object stays signalled and its exit code stays readable
 * for as long as a handle is held -- which is exactly the zombie that WNOWAIT
 * asks not to be released. */
static int hl_windows_reap_locked(unsigned slot, DWORD expected_id, int release, int *status) {
    HANDLE object;
    DWORD code = 0;
    if (!hl_windows_children[slot].used || hl_windows_children[slot].id != expected_id) return 0;
    object = hl_windows_children[slot].process;
    if (!GetExitCodeProcess(object, &code)) code = HL_WINDOWS_EXIT_BOOTSTRAP;
    if (release) {
        hl_windows_children[slot].used = 0;
        hl_windows_children[slot].process = NULL;
        hl_windows_children[slot].id = 0;
        (void)CloseHandle(object);
    }
    *status = hl_windows_wait_status(code);
    return 1;
}

/* How long a blocking wait sits on one snapshot of the child set before taking
 * another. It is not a latency floor: a child that exits signals its process
 * object and wakes the wait immediately. It bounds only how long a wait can miss
 * a child a PEER guest thread forked after the snapshot was taken, and how long
 * a child past the first MAXIMUM_WAIT_OBJECTS goes unpolled. */
#define HL_WINDOWS_WAIT_SLICE_MS 100u

/* The option word is the GUEST's, i.e. Linux's, spelled here rather than taken
 * from a header: the ABI layer forwards its own translated option bits straight
 * through, and this backend must read them with Linux's numbering. Only WNOHANG
 * is actionable. WUNTRACED and WCONTINUED are accepted and never satisfied
 * because a cloned child cannot be stopped or continued on this host, so there
 * is no such event for a wait to report. */
#define HL_WINDOWS_WNOHANG 0x00000001
/* WNOWAIT: report the child without releasing it, so a later wait sees it again. */
#define HL_WINDOWS_WNOWAIT 0x01000000

int hl_host_windows_waitpid(pid_t pid, int *status, int options) {
    /*
     * Negative and zero pids name process GROUPS, and this host's group model is
     * degenerate rather than absent: setpgid is a refusal here, so no guest
     * process ever leaves the group it was launched in and every group the guest
     * believes it created contains exactly its leader. Read that way both forms
     * have an exact answer rather than an approximation.
     *
     *   pid <  -1  -- "any child in group -pid". That group is the single
     *                 process -pid, so this is a wait for exactly that child.
     *   pid == 0   -- "any child in MY group". Every child is still in it,
     *                 because none of them could leave it, so this is "any".
     */
    const int any = pid == 0 || pid == -1;
    const pid_t target = pid < -1 ? -pid : pid;
    for (;;) {
        HANDLE waitset[MAXIMUM_WAIT_OBJECTS];
        unsigned slots[MAXIMUM_WAIT_OBJECTS];
        DWORD identifiers[MAXIMUM_WAIT_OBJECTS];
        DWORD count = 0;
        unsigned candidates = 0;
        unsigned step;
        unsigned start;
        DWORD waited;
        int reaped_status = 0;

        AcquireSRWLockExclusive(&hl_windows_children_lock);
        start = hl_windows_child_cursor;
        for (step = 0; step < HL_WINDOWS_CHILD_CAPACITY; step++) {
            const unsigned index = (start + step) % HL_WINDOWS_CHILD_CAPACITY;
            if (!hl_windows_children[index].used) continue;
            if (!any && hl_windows_children[index].id != (DWORD)target) continue;
            candidates++;
            if (count < MAXIMUM_WAIT_OBJECTS) {
                waitset[count] = hl_windows_children[index].process;
                slots[count] = index;
                identifiers[count] = hl_windows_children[index].id;
                count++;
            }
        }
        hl_windows_child_cursor = (start + 1u) % HL_WINDOWS_CHILD_CAPACITY;
        ReleaseSRWLockExclusive(&hl_windows_children_lock);

        if (candidates == 0) {
            /* The load-bearing answer: "you have no such child". A shell or a
             * runtime treats it as authoritative and stops reaping. */
            errno = ECHILD;
            return -1;
        }
        /* A zero timeout first, unconditionally, so an already-exited child is
         * collected without a trip through the scheduler and so WNOHANG costs
         * exactly one call. */
        waited = WaitForMultipleObjects(count, waitset, FALSE, 0);
        if (waited == WAIT_TIMEOUT) {
            if (options & HL_WINDOWS_WNOHANG) return 0;
            waited = WaitForMultipleObjects(count, waitset, FALSE, HL_WINDOWS_WAIT_SLICE_MS);
            if (waited == WAIT_TIMEOUT) continue;
        }
        if (waited >= WAIT_OBJECT_0 && waited < WAIT_OBJECT_0 + count) {
            const DWORD index = waited - WAIT_OBJECT_0;
            int taken;
            AcquireSRWLockExclusive(&hl_windows_children_lock);
            taken = hl_windows_reap_locked(slots[index], identifiers[index], (options & HL_WINDOWS_WNOWAIT) == 0,
                                           &reaped_status);
            ReleaseSRWLockExclusive(&hl_windows_children_lock);
            if (!taken) continue; /* a peer guest thread reaped it first */
            if (status != NULL) *status = reaped_status;
            return (int)identifiers[index];
        }
        /* WAIT_FAILED. The only route here is a handle that is no longer a
         * process, which this table cannot produce, so report it as the absence
         * it effectively is rather than spinning on it. */
        errno = ECHILD;
        return -1;
    }
}

/*
 * kill(2), honestly partial.
 *
 * Signal 0 is the whole liveness half of kill and it is REAL here: the container
 * registry's membership checks, its /proc enumeration and its stale-marker
 * pruning are all kill(pid, 0) probes, and they were the visible cost of the
 * previous whole-file refusal -- every one of them read "dead" for a live
 * process. It answers for any host pid, not only this process's children,
 * because that is what those probes ask.
 *
 * SIGKILL is REAL because TerminateProcess is genuinely SIGKILL: immediate,
 * unmaskable, no handler, and the exit code it mints round-trips back through
 * the wait above as WIFSIGNALED(SIGKILL).
 *
 * Every other signal is REFUSED, which is the same judgement the previous whole
 * refusal made and for the same reason: this host cannot deliver a catchable
 * signal to another process, and terminating a process that may have installed a
 * handler would report a death the guest asked to be able to prevent. A caller
 * that gets ENOSYS can tell which half it got; one that gets a fabricated kill
 * cannot.
 */
int hl_host_windows_kill(pid_t pid, int signo) {
    HANDLE object;
    unsigned index;
    int owned = 0;
    /*
     * kill(-pgid, sig) resolves to the single process -pgid, for the reason the
     * wait above spells out: setpgid is a refusal on this host, so a guest
     * process group is exactly its leader and signalling the group is
     * signalling that one process. Delivering to it is therefore complete, not
     * partial -- and the alternative, the ESRCH this used to return, is what
     * left a parent tearing down a child's private group blocked forever in the
     * wait that follows.
     *
     * kill(0, sig) and kill(-1, sig) do NOT arrive here: the ABI layer routes
     * both through the container registry, which is what keeps a broadcast
     * inside this container instead of reaching every process the user owns.
     */
    if (pid < -1) pid = -pid;
    if (pid <= 0) {
        errno = ESRCH;
        return -1;
    }
    if (signo != 0 && signo != 9) {
        errno = ENOSYS;
        return -1;
    }
    if ((DWORD)pid == GetCurrentProcessId()) {
        if (signo == 0) return 0;
        errno = ENOSYS; /* self-signalling belongs to the guest signal machinery */
        return -1;
    }
    AcquireSRWLockExclusive(&hl_windows_children_lock);
    for (index = 0; index < HL_WINDOWS_CHILD_CAPACITY; index++) {
        if (hl_windows_children[index].used && hl_windows_children[index].id == (DWORD)pid) {
            owned = 1;
            break;
        }
    }
    ReleaseSRWLockExclusive(&hl_windows_children_lock);
    /* An unreaped child of ours is alive-or-zombie either way, and a zombie
     * answers signal 0 on Linux, so the table is consulted before the OS is: a
     * child that has exited but not been waited for still has a pid here. */
    if (owned && signo == 0) return 0;

    object = OpenProcess(signo == 0 ? PROCESS_QUERY_LIMITED_INFORMATION : PROCESS_TERMINATE, FALSE, (DWORD)pid);
    if (object == NULL) {
        const DWORD error = GetLastError();
        errno = (error == ERROR_ACCESS_DENIED) ? EPERM : ESRCH;
        return -1;
    }
    if (signo == 0) {
        DWORD code = 0;
        const BOOL queried = GetExitCodeProcess(object, &code);
        (void)CloseHandle(object);
        if (queried && code != STILL_ACTIVE) {
            /* Exited and not ours to reap: the pid names nothing a signal could
             * reach, which is ESRCH rather than a live process. */
            errno = ESRCH;
            return -1;
        }
        return 0;
    }
    if (!TerminateProcess(object, HL_WINDOWS_EXIT_SIGNAL_BASE + 9u)) {
        const DWORD error = GetLastError();
        (void)CloseHandle(object);
        errno = (error == ERROR_ACCESS_DENIED) ? EPERM : ESRCH;
        return -1;
    }
    (void)CloseHandle(object);
    return 0;
}

/*
 * getppid(2). NtQueryInformationProcess(ProcessBasicInformation) carries the
 * creating process id, which is what Windows holds instead of a parent link. It
 * is not maintained: it names the process that created this one whether or not
 * that process still exists. Linux re-parents an orphan onto init and reports 1;
 * this reports the original id. The difference is visible only to an orphan, and
 * the truth this host holds beats a fabricated 1 for every process.
 */
int hl_host_windows_parent_pid(void) {
    typedef LONG(NTAPI * hl_windows_query_process_fn)(HANDLE, ULONG, void *, ULONG, ULONG *);
    static hl_windows_query_process_fn resolved;
    PROCESS_BASIC_INFORMATION basic;
    ULONG written = 0;
    if (resolved == NULL) {
        HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
        if (ntdll == NULL) return -1;
        resolved = (hl_windows_query_process_fn)(void *)GetProcAddress(ntdll, "NtQueryInformationProcess");
        if (resolved == NULL) return -1;
    }
    memset(&basic, 0, sizeof basic);
    if (resolved(GetCurrentProcess(), 0u /* ProcessBasicInformation */, &basic, (ULONG)sizeof basic, &written) < 0)
        return -1;
    return (int)(DWORD)(ULONG_PTR)basic.InheritedFromUniqueProcessId;
}
