/*
 * The NT path layer under the file group: ntdll binding, NTSTATUS mapping, the
 * UTF-8/UTF-16 boundary, and the pinned-root resolver.
 *
 * Three measured facts shape everything here, and each was checked on this host
 * rather than inferred from documentation:
 *
 *   1. The NT parser does not understand "." or "..". Both come back
 *      STATUS_OBJECT_NAME_INVALID from a relative NtCreateFile, so dot
 *      resolution is this file's job and not the kernel's. An *empty* relative
 *      name does work, and reopens the directory the handle already names --
 *      that is the "." this layer uses internally.
 *   2. "/" is not a separator in an NT name; it is an ordinary character, and a
 *      name containing one is rejected outright. Guest paths therefore have to
 *      be split here, component by component, which is the same loop the
 *      beneath-a-root containment needs anyway.
 *   3. A raw ':' in a relative NT name does *not* fail. "a:b" creates a file
 *      named "a" carrying an alternate data stream named "b" -- silently, with
 *      STATUS_SUCCESS. That is worse than an error, and it is why the
 *      illegal-character remap below is mandatory rather than cosmetic.
 *
 * Reserved DOS device names are deliberately not special-cased: CON, AUX, COM1
 * and CONOUT$ were all created and reopened as ordinary files through this
 * layer. The Win32 path parser is what reserves them, and nothing here goes
 * through it. Do not re-add a workaround.
 */
#include "internal.h"

#include <stdlib.h>
#include <string.h>

/* NTSTATUS values, spelled locally. ntstatus.h collides with the STATUS_*
 * subset winbase.h already defines unless WIN32_NO_STATUS is arranged before
 * <windows.h>, which internal.h owns; a dozen constants are not worth that. */
#define HL_NT_SUCCESS ((NTSTATUS)0x00000000L)
#define HL_NT_BUFFER_OVERFLOW ((NTSTATUS)0x80000005L)
#define HL_NT_NO_MORE_FILES ((NTSTATUS)0x80000006L)
#define HL_NT_END_OF_FILE ((NTSTATUS)0xC0000011L)
#define HL_NT_INFO_LENGTH_MISMATCH ((NTSTATUS)0xC0000004L)
#define HL_NT_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#define HL_NT_INVALID_HANDLE ((NTSTATUS)0xC0000008L)
#define HL_NT_INVALID_DEVICE_REQUEST ((NTSTATUS)0xC0000010L)
#define HL_NT_INVALID_INFO_CLASS ((NTSTATUS)0xC0000003L)
#define HL_NT_NO_MEMORY ((NTSTATUS)0xC0000017L)
#define HL_NT_ACCESS_DENIED ((NTSTATUS)0xC0000022L)
#define HL_NT_BUFFER_TOO_SMALL ((NTSTATUS)0xC0000023L)
#define HL_NT_OBJECT_NAME_INVALID ((NTSTATUS)0xC0000033L)
#define HL_NT_OBJECT_NAME_NOT_FOUND ((NTSTATUS)0xC0000034L)
#define HL_NT_OBJECT_NAME_COLLISION ((NTSTATUS)0xC0000035L)
#define HL_NT_OBJECT_PATH_INVALID ((NTSTATUS)0xC0000039L)
#define HL_NT_OBJECT_PATH_NOT_FOUND ((NTSTATUS)0xC000003AL)
#define HL_NT_OBJECT_PATH_SYNTAX_BAD ((NTSTATUS)0xC000003BL)
#define HL_NT_SHARING_VIOLATION ((NTSTATUS)0xC0000043L)
#define HL_NT_DELETE_PENDING ((NTSTATUS)0xC0000056L)
#define HL_NT_PRIVILEGE_NOT_HELD ((NTSTATUS)0xC0000061L)
#define HL_NT_DISK_FULL ((NTSTATUS)0xC000007FL)
#define HL_NT_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009AL)
#define HL_NT_MEDIA_WRITE_PROTECTED ((NTSTATUS)0xC00000A2L)
#define HL_NT_FILE_IS_A_DIRECTORY ((NTSTATUS)0xC00000BAL)
#define HL_NT_NOT_SAME_DEVICE ((NTSTATUS)0xC00000D4L)
#define HL_NT_DIRECTORY_NOT_EMPTY ((NTSTATUS)0xC0000101L)
#define HL_NT_NOT_A_DIRECTORY ((NTSTATUS)0xC0000103L)
#define HL_NT_NAME_TOO_LONG ((NTSTATUS)0xC0000106L)
#define HL_NT_CANNOT_DELETE ((NTSTATUS)0xC0000121L)
#define HL_NT_CANCELLED ((NTSTATUS)0xC0000120L)
#define HL_NT_FILE_CLOSED ((NTSTATUS)0xC0000128L)
#define HL_NT_NOT_A_REPARSE_POINT ((NTSTATUS)0xC0000275L)
#define HL_NT_REPARSE_POINT_NOT_RESOLVED ((NTSTATUS)0xC0000280L)
#define HL_NT_QUOTA_EXCEEDED ((NTSTATUS)0xC0000044L)
#define HL_NT_TOO_MANY_OPENED_FILES ((NTSTATUS)0xC000011FL)

/* Information classes younger than mingw-w64's FILE_INFORMATION_CLASS enum. */
#define HL_NT_FILE_DISPOSITION_INFORMATION_EX ((FILE_INFORMATION_CLASS)64)
#define HL_NT_FILE_RENAME_INFORMATION_EX ((FILE_INFORMATION_CLASS)65)

/* --- ntdll binding --------------------------------------------------------- */

int hl_windows_resolve_ntdll(hl_windows_ntdll *nt) {
    HMODULE module = GetModuleHandleW(L"ntdll.dll");
    if (module == NULL) module = LoadLibraryW(L"ntdll.dll");
    if (module == NULL) return 0;
    /* Same cast shape as hl_windows_resolve_kernelbase: FARPROC converts back
     * only to a function pointer type, so the hop through one is what ISO C
     * permits. */
#define HL_WINDOWS_BIND(field, name) *(void (**)(void)) & nt->field = (void (*)(void))GetProcAddress(module, name)
    HL_WINDOWS_BIND(create_file, "NtCreateFile");
    HL_WINDOWS_BIND(read_file, "NtReadFile");
    HL_WINDOWS_BIND(write_file, "NtWriteFile");
    HL_WINDOWS_BIND(query_information_file, "NtQueryInformationFile");
    HL_WINDOWS_BIND(set_information_file, "NtSetInformationFile");
    HL_WINDOWS_BIND(query_directory_file, "NtQueryDirectoryFile");
    HL_WINDOWS_BIND(query_volume_information_file, "NtQueryVolumeInformationFile");
    HL_WINDOWS_BIND(flush_buffers_file, "NtFlushBuffersFile");
    HL_WINDOWS_BIND(query_security_object, "NtQuerySecurityObject");
    HL_WINDOWS_BIND(open_process_token, "NtOpenProcessToken");
    HL_WINDOWS_BIND(query_information_token, "NtQueryInformationToken");
    HL_WINDOWS_BIND(status_to_dos_error, "RtlNtStatusToDosError");
#undef HL_WINDOWS_BIND
    return nt->create_file != NULL && nt->read_file != NULL && nt->write_file != NULL &&
           nt->query_information_file != NULL && nt->set_information_file != NULL && nt->query_directory_file != NULL &&
           nt->query_volume_information_file != NULL && nt->flush_buffers_file != NULL &&
           nt->query_security_object != NULL && nt->open_process_token != NULL && nt->query_information_token != NULL &&
           nt->status_to_dos_error != NULL;
}

/* --- status mapping -------------------------------------------------------- */

/*
 * NTSTATUS is strictly finer than the Win32 error it collapses into, so the
 * cases that matter are named here and anything else is handed to
 * RtlNtStatusToDosError and the existing Win32 table. Two examples of why the
 * explicit arm earns its place: STATUS_DELETE_PENDING becomes ERROR_ACCESS_DENIED
 * once through the DOS mapping and would report PERMISSION_DENIED instead of
 * BUSY, and STATUS_OBJECT_PATH_NOT_FOUND becomes ERROR_PATH_NOT_FOUND, which is
 * right, but only by accident of the direction we happen to want.
 */
hl_status hl_windows_status_from_ntstatus(const hl_host_windows *host, NTSTATUS status) {
    switch (status) {
    case HL_NT_SUCCESS: return HL_STATUS_OK;
    case HL_NT_END_OF_FILE: return HL_STATUS_OK; /* a zero-length read, not an error */
    case HL_NT_INVALID_PARAMETER:
    case HL_NT_INVALID_HANDLE:
    case HL_NT_OBJECT_NAME_INVALID:
    case HL_NT_OBJECT_PATH_SYNTAX_BAD:
    case HL_NT_FILE_CLOSED:
    case HL_NT_INFO_LENGTH_MISMATCH:
    case HL_NT_NOT_A_REPARSE_POINT: return HL_STATUS_INVALID_ARGUMENT;
    case HL_NT_INVALID_DEVICE_REQUEST:
    case HL_NT_INVALID_INFO_CLASS: return HL_STATUS_NOT_SUPPORTED;
    case HL_NT_NO_MEMORY:
    case HL_NT_INSUFFICIENT_RESOURCES: return HL_STATUS_OUT_OF_MEMORY;
    case HL_NT_BUFFER_TOO_SMALL:
    case HL_NT_BUFFER_OVERFLOW: return HL_STATUS_RESOURCE_LIMIT;
    case HL_NT_TOO_MANY_OPENED_FILES: return HL_STATUS_PROCESS_LIMIT;
    case HL_NT_OBJECT_NAME_NOT_FOUND:
    case HL_NT_OBJECT_PATH_NOT_FOUND:
    case HL_NT_OBJECT_PATH_INVALID: return HL_STATUS_NOT_FOUND;
    case HL_NT_OBJECT_NAME_COLLISION: return HL_STATUS_ALREADY_EXISTS;
    case HL_NT_ACCESS_DENIED:
    case HL_NT_CANNOT_DELETE:
    case HL_NT_PRIVILEGE_NOT_HELD: return HL_STATUS_PERMISSION_DENIED;
    case HL_NT_SHARING_VIOLATION:
    case HL_NT_DELETE_PENDING: return HL_STATUS_BUSY;
    case HL_NT_FILE_IS_A_DIRECTORY: return HL_STATUS_IS_DIRECTORY;
    case HL_NT_NOT_A_DIRECTORY: return HL_STATUS_NOT_DIRECTORY;
    case HL_NT_DIRECTORY_NOT_EMPTY: return HL_STATUS_NOT_EMPTY;
    case HL_NT_NAME_TOO_LONG: return HL_STATUS_NAME_TOO_LONG;
    case HL_NT_REPARSE_POINT_NOT_RESOLVED: return HL_STATUS_SYMLINK_LOOP;
    case HL_NT_DISK_FULL: return HL_STATUS_NO_SPACE;
    case HL_NT_QUOTA_EXCEEDED: return HL_STATUS_QUOTA;
    case HL_NT_MEDIA_WRITE_PROTECTED: return HL_STATUS_READ_ONLY;
    case HL_NT_NOT_SAME_DEVICE: return HL_STATUS_CROSS_DEVICE;
    case HL_NT_CANCELLED: return HL_STATUS_INTERRUPTED;
    default: break;
    }
    return hl_windows_status_from_error(host->nt.status_to_dos_error(status));
}

hl_host_result hl_windows_nt_result(const hl_host_windows *host, NTSTATUS status) {
    hl_host_result result;
    result.status = (int32_t)hl_windows_status_from_ntstatus(host, status);
    result.detail_domain = HL_WINDOWS_DETAIL_NT;
    result.value = 0;
    result.detail = (uint64_t)(uint32_t)status;
    return result;
}

/* --- the UTF-8 / UTF-16 boundary ------------------------------------------- */

/*
 * Policy, stated once: every path crossing hl_host_file_services is a UTF-8 byte
 * span without a terminator, and every path inside Windows is UTF-16. The
 * conversion happens here and nowhere else, and a byte span that is not valid
 * UTF-8 is rejected rather than silently transcoded -- a Linux guest may hold
 * arbitrary bytes in a filename, but there is no lossless place to put them in
 * a UTF-16 namespace, and inventing one would make the mapping non-invertible.
 *
 * Seven characters are legal in a Linux filename and illegal in a Windows one
 * (: * ? < > | "), and an eighth, backslash, is a separator down here and an
 * ordinary character up there. All eight are carried as 0xF000|c -- the scheme
 * Cygwin uses, chosen for exactly this reason: the range is Unicode private
 * use, NTFS stores it without complaint, and the mapping is a bijection, so a
 * name survives create, enumerate and reopen unchanged.
 */
static int hl_windows_illegal(WCHAR c) {
    return c == L':' || c == L'*' || c == L'?' || c == L'<' || c == L'>' || c == L'|' || c == L'"' || c == L'\\';
}

hl_status hl_windows_wide_from_utf8(const char *bytes, size_t size, int escape, WCHAR *out, uint32_t capacity,
                                    uint32_t *out_length) {
    int produced;
    uint32_t index;
    if (bytes == NULL || capacity == 0) return HL_STATUS_INVALID_ARGUMENT;
    if (size == 0) {
        out[0] = 0;
        *out_length = 0;
        return HL_STATUS_OK;
    }
    if (size > (size_t)INT_MAX || size >= capacity) return HL_STATUS_NAME_TOO_LONG;
    if (memchr(bytes, '\0', size) != NULL) return HL_STATUS_INVALID_ARGUMENT;
    produced = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, bytes, (int)size, out, (int)(capacity - 1u));
    if (produced <= 0)
        return GetLastError() == ERROR_INSUFFICIENT_BUFFER ? HL_STATUS_NAME_TOO_LONG : HL_STATUS_INVALID_ARGUMENT;
    if (escape)
        for (index = 0; index < (uint32_t)produced; ++index)
            if (hl_windows_illegal(out[index])) out[index] = (WCHAR)(0xF000u | out[index]);
    out[produced] = 0;
    *out_length = (uint32_t)produced;
    return HL_STATUS_OK;
}

hl_status hl_windows_utf8_from_wide(const WCHAR *wide, uint32_t length, char *out, size_t capacity, size_t *out_size) {
    WCHAR stack[512];
    WCHAR *plain = stack;
    int produced;
    uint32_t index;
    if (wide == NULL || out_size == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (length == 0) {
        *out_size = 0;
        return HL_STATUS_OK;
    }
    if (length > (uint32_t)INT_MAX || capacity > (size_t)INT_MAX) return HL_STATUS_INVALID_ARGUMENT;
    if (length > sizeof(stack) / sizeof(stack[0])) {
        plain = malloc((size_t)length * sizeof(*plain));
        if (plain == NULL) return HL_STATUS_OUT_OF_MEMORY;
    }
    /* The reverse of the remap. Unescaping the whole private-use plane block
     * rather than only the eight characters keeps this a pure inverse: nothing
     * else may legally reach here in 0xF000..0xF0FF, and a name that did would
     * have been unreachable through the forward direction anyway. */
    for (index = 0; index < length; ++index)
        plain[index] = (wide[index] & 0xFF00u) == 0xF000u ? (WCHAR)(wide[index] & 0x00FFu) : wide[index];
    produced =
        WideCharToMultiByte(CP_UTF8, 0, plain, (int)length, capacity == 0 ? NULL : out, (int)capacity, NULL, NULL);
    if (plain != stack) free(plain);
    if (produced <= 0) {
        if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) return HL_STATUS_INVALID_ARGUMENT;
        /* Report what the caller would have needed, the way the POSIX backends
         * report a truncating readlink. */
        produced = WideCharToMultiByte(CP_UTF8, 0, wide, (int)length, NULL, 0, NULL, NULL);
        *out_size = produced > 0 ? (size_t)produced : 0;
        return HL_STATUS_RESOURCE_LIMIT;
    }
    *out_size = (size_t)produced;
    return HL_STATUS_OK;
}

/* --- the relative open primitive ------------------------------------------- */

/*
 * Every open in this backend lands here, and every one of them passes
 * FILE_SHARE_DELETE. That is not politeness: POSIX unlink-while-open is only
 * available on Windows when *no* opener has excluded delete sharing, and the
 * single reason it can be guaranteed here is that this function is the only door
 * into the filesystem for the whole group.
 */
NTSTATUS hl_windows_open_child(const hl_windows_ntdll *nt, HANDLE parent, const WCHAR *name, uint32_t length,
                               ACCESS_MASK access, ULONG attributes, ULONG disposition, ULONG options, HANDLE *out) {
    UNICODE_STRING native;
    OBJECT_ATTRIBUTES object;
    IO_STATUS_BLOCK status_block;
    *out = NULL;
    if (length > 0x7FFEu) return HL_NT_NAME_TOO_LONG;
    native.Buffer = (PWSTR)(size_t)name;
    native.Length = (USHORT)(length * 2u);
    native.MaximumLength = (USHORT)(native.Length + 2u);
    /* OBJ_CASE_INSENSITIVE matches how every Windows volume is actually
     * configured; a case-sensitive guest namespace is a Linux-front concern and
     * cannot be conjured from a case-insensitive volume by asking politely. */
    InitializeObjectAttributes(&object, &native, OBJ_CASE_INSENSITIVE, parent, NULL);
    return nt->create_file(out, access | SYNCHRONIZE, &object, &status_block, NULL, attributes,
                           FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, disposition,
                           options | FILE_SYNCHRONOUS_IO_NONALERT, NULL, 0);
}

/* --- guest symlinks as a file format --------------------------------------- */

/*
 * Native symlink creation fails ERROR_PRIVILEGE_NOT_HELD on this host even with
 * SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE, and developer mode is not
 * something a guest can be asked to enable. Guest symlinks are therefore a
 * tagged file payload -- the approach Cygwin has shipped for two decades -- and
 * the resolver below is what makes them behave like links.
 *
 * Layout: the magic, then the target as raw UTF-8, no terminator. The file
 * carries FILE_ATTRIBUTE_SYSTEM so the cheap filter (attribute plus a plausible
 * size) rejects ordinary files without reading them. The payload is UTF-8 rather
 * than Cygwin's UTF-16, because the target arrives and leaves as UTF-8 bytes and
 * a round trip through UTF-16 would not be lossless for a target that is not
 * valid UTF-8.
 */
const char hl_windows_symlink_magic[HL_WINDOWS_SYMLINK_MAGIC_SIZE] = {'!', '<', 's', 'y',    'm',    'l',   'i',
                                                                      'n', 'k', '>', '\xEF', '\xBB', '\xBF'};

int hl_windows_symlink_candidate(uint32_t attributes, uint64_t size) {
    return (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0 && (attributes & FILE_ATTRIBUTE_SYSTEM) != 0 &&
           size >= (uint64_t)HL_WINDOWS_SYMLINK_MAGIC_SIZE && size <= UINT64_C(65536);
}

/*
 * Read the payload of an already-open candidate. The handle needs FILE_READ_DATA;
 * a candidate that cannot be read is reported as "not a symlink" rather than as
 * an error, because an unreadable ordinary system file must not break a walk
 * that was never going to touch it.
 */
int hl_windows_symlink_read(const hl_windows_ntdll *nt, HANDLE file, char *target, uint32_t capacity,
                            uint32_t *out_size) {
    IO_STATUS_BLOCK status_block;
    LARGE_INTEGER offset;
    char header[HL_WINDOWS_SYMLINK_MAGIC_SIZE];
    NTSTATUS status;
    offset.QuadPart = 0;
    status = nt->read_file(file, NULL, NULL, NULL, &status_block, header, (ULONG)sizeof(header), &offset, NULL);
    if (status != HL_NT_SUCCESS || status_block.Information != sizeof(header)) return 0;
    if (memcmp(header, hl_windows_symlink_magic, sizeof(header)) != 0) return 0;
    if (target == NULL) return 1;
    offset.QuadPart = (LONGLONG)sizeof(header);
    status = nt->read_file(file, NULL, NULL, NULL, &status_block, target, capacity, &offset, NULL);
    if (status != HL_NT_SUCCESS && status != HL_NT_END_OF_FILE) return 0;
    *out_size = (uint32_t)status_block.Information;
    return 1;
}

/* --- absolute names -------------------------------------------------------- */

/*
 * A guest path is relative to a directory handle, which is the only spelling
 * this backend really wants. Two absolute spellings still have to be accepted:
 * a leading '/', because guests produce them, and a native "C:\..." or "\\?\...",
 * because file.path() hands one back and callers feed it in again.
 *
 * A leading '/' resolves against the volume root of the process working
 * directory. That is a placeholder policy and it is stated rather than hidden:
 * deciding what the guest's "/" *is* belongs to the Linux front end, which
 * expresses it by passing a pinned directory handle instead.
 */
static int hl_windows_native_prefix(const char *path, size_t size) {
    if (size >= 4 && path[0] == '\\' && path[1] == '\\' && (path[2] == '?' || path[2] == '.') && path[3] == '\\')
        return 1;
    if (size >= 2 && path[1] == ':' && ((path[0] >= 'a' && path[0] <= 'z') || (path[0] >= 'A' && path[0] <= 'Z')))
        return 1;
    return 0;
}

/*
 * Open the NT object directory a native or rooted-absolute path starts from,
 * and report how many leading bytes it consumed.
 */
static hl_host_result hl_windows_open_absolute_root(hl_host_windows *host, const char *path, size_t size,
                                                    size_t *consumed, HANDLE *out) {
    WCHAR native[16 + MAX_PATH];
    HANDLE root = NULL;
    NTSTATUS status;
    uint32_t length;
    *out = NULL;
    if (hl_windows_native_prefix(path, size)) {
        /* "\\?\X:\rest" and "X:\rest" both become the NT "\??\X:\" device
         * link, which is the same object the Win32 layer would have reached
         * and is where trailing dots and spaces stop being stripped. */
        size_t at = path[0] == '\\' ? 4u : 0u;
        size_t start = at;
        while (at < size && path[at] != '\\' && path[at] != '/')
            ++at;
        if (at - start == 0 || at - start > MAX_PATH) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memcpy(native, L"\\??\\", 4 * sizeof(WCHAR));
        {
            uint32_t produced = 0;
            hl_status converted = hl_windows_wide_from_utf8(
                path + start, at - start, 0, native + 4, (uint32_t)(sizeof(native) / sizeof(*native)) - 6u, &produced);
            if (converted != HL_STATUS_OK) return hl_windows_result(converted, 0, 0);
            length = produced + 4u;
        }
        native[length++] = L'\\';
        native[length] = 0;
        *consumed = at;
    } else {
        /* A leading '/': the volume of the process working directory. */
        WCHAR working[MAX_PATH];
        DWORD produced = GetCurrentDirectoryW(MAX_PATH, working);
        if (produced == 0 || produced >= MAX_PATH || working[1] != L':')
            return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
        memcpy(native, L"\\??\\", 4 * sizeof(WCHAR));
        native[4] = working[0];
        native[5] = L':';
        native[6] = L'\\';
        native[7] = 0;
        length = 7;
        *consumed = 0;
    }
    status = hl_windows_open_child(&host->nt, NULL, native, length,
                                   FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES, 0, FILE_OPEN,
                                   FILE_DIRECTORY_FILE | FILE_OPEN_FOR_BACKUP_INTENT, &root);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    *out = root;
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

hl_host_result hl_windows_open_working_directory(hl_host_windows *host, HANDLE *out) {
    WCHAR working[MAX_PATH + 8];
    DWORD produced;
    NTSTATUS status;
    memcpy(working, L"\\??\\", 4 * sizeof(WCHAR));
    produced = GetCurrentDirectoryW(MAX_PATH, working + 4);
    if (produced == 0 || produced >= MAX_PATH) return hl_windows_last_error_result();
    status = hl_windows_open_child(&host->nt, NULL, working, produced + 4u,
                                   FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES, 0, FILE_OPEN,
                                   FILE_DIRECTORY_FILE | FILE_OPEN_FOR_BACKUP_INTENT, out);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

/*
 * Open the parent of a directory handle. Needed only by the escape-permitting
 * walk, and it is the one place a path string is unavoidable: ".." is not a name
 * the NT parser resolves, so the handle's own path is fetched and truncated
 * instead. A handle already at a volume root reports itself.
 */
static hl_host_result hl_windows_open_parent(hl_host_windows *host, HANDLE directory, HANDLE *out) {
    WCHAR path[8 + 1024];
    DWORD produced;
    uint32_t at;
    NTSTATUS status;
    memcpy(path, L"\\??\\", 4 * sizeof(WCHAR));
    produced = GetFinalPathNameByHandleW(directory, path + 4, 1024, FILE_NAME_NORMALIZED | VOLUME_NAME_NT);
    if (produced == 0 || produced >= 1024) return hl_windows_last_error_result();
    /* VOLUME_NAME_NT already yields a "\Device\..." name, which is absolute in
     * the NT namespace; the "\??\" head is only reserved storage in that case. */
    at = produced;
    while (at > 0 && path[4 + at - 1] != L'\\')
        --at;
    if (at <= 1) {
        at = produced;
    } else {
        --at; /* drop the separator, unless that would empty the name */
        if (at == 0) at = 1;
    }
    status =
        hl_windows_open_child(&host->nt, NULL, path + 4, at, FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES,
                              0, FILE_OPEN, FILE_DIRECTORY_FILE | FILE_OPEN_FOR_BACKUP_INTENT, out);
    return status == HL_NT_SUCCESS ? hl_windows_result(HL_STATUS_OK, 0, 0) : hl_windows_nt_result(host, status);
}

/* --- the resolver ---------------------------------------------------------- */

typedef struct hl_windows_stack {
    HANDLE inline_storage[16];
    HANDLE *entries;
    uint32_t count;
    uint32_t capacity;
} hl_windows_stack;

static void hl_windows_stack_init(hl_windows_stack *stack) {
    stack->entries = stack->inline_storage;
    stack->count = 0;
    stack->capacity = 16;
}

static void hl_windows_stack_destroy(hl_windows_stack *stack) {
    while (stack->count != 0)
        CloseHandle(stack->entries[--stack->count]);
    if (stack->entries != stack->inline_storage) free(stack->entries);
}

static int hl_windows_stack_push(hl_windows_stack *stack, HANDLE value) {
    if (stack->count == stack->capacity) {
        uint32_t capacity = stack->capacity * 2u;
        HANDLE *grown;
        if (capacity > 4096u) return 0;
        grown = stack->entries == stack->inline_storage ? malloc((size_t)capacity * sizeof(*grown))
                                                        : realloc(stack->entries, (size_t)capacity * sizeof(*grown));
        if (grown == NULL) return 0;
        if (stack->entries == stack->inline_storage)
            memcpy(grown, stack->inline_storage, sizeof(stack->inline_storage));
        stack->entries = grown;
        stack->capacity = capacity;
    }
    stack->entries[stack->count++] = value;
    return 1;
}

static HANDLE hl_windows_duplicate(HANDLE value) {
    HANDLE copy = NULL;
    if (!DuplicateHandle(GetCurrentProcess(), value, GetCurrentProcess(), &copy, 0, FALSE, DUPLICATE_SAME_ACCESS))
        return NULL;
    return copy;
}

/* Splice a symlink target in front of whatever is left to resolve. */
static hl_status hl_windows_join(WCHAR *pending, uint32_t capacity, const char *target, uint32_t target_size,
                                 const WCHAR *rest, uint32_t rest_length) {
    WCHAR converted[HL_WINDOWS_PATH_MAX];
    uint32_t length = 0;
    hl_status status = hl_windows_wide_from_utf8(target, target_size, 1, converted,
                                                 (uint32_t)(sizeof(converted) / sizeof(*converted)), &length);
    uint32_t total;
    if (status != HL_STATUS_OK) return status;
    /* The escape pass above turned the target's backslashes into 0xF05C, which
     * is right for a name and wrong for a separator -- but a guest symlink
     * target has no backslash separators to begin with, so nothing is lost and
     * a literal backslash in a target stays literal. */
    total = length + (rest_length != 0 ? rest_length + 1u : 0u);
    if (total >= capacity) return HL_STATUS_NAME_TOO_LONG;
    memcpy(pending, converted, (size_t)length * sizeof(WCHAR));
    if (rest_length != 0) {
        pending[length] = L'/';
        memcpy(pending + length + 1u, rest, (size_t)rest_length * sizeof(WCHAR));
    }
    pending[total] = 0;
    return HL_STATUS_OK;
}

/*
 * Resolve `path` beneath `root`, returning the directory that owns the final
 * component plus the component itself. A zero-length leaf means "the object the
 * parent handle already names", which is this layer's spelling of ".".
 *
 * Containment, when HL_WINDOWS_RESOLVE_ESCAPE is absent, rests on two
 * properties and nothing else: ".." never pops past the pinned root, and every
 * component is opened FILE_OPEN_REPARSE_POINT, so no native junction or symlink
 * is ever traversed. The consequence is deliberate -- a directory reached
 * through a real NTFS junction is not reachable beneath a pinned root at all,
 * and reports NOT_DIRECTORY rather than silently leaving the subtree.
 */
hl_host_result hl_windows_resolve_path(hl_host_windows *host, HANDLE root, const char *path, size_t path_size,
                                       uint32_t policy, hl_windows_resolution *out) {
    hl_windows_stack stack;
    WCHAR pending[HL_WINDOWS_PATH_MAX];
    WCHAR scratch[HL_WINDOWS_PATH_MAX];
    HANDLE absolute_root = NULL;
    hl_host_result result = hl_windows_result(HL_STATUS_OK, 0, 0);
    const int escape = (policy & HL_WINDOWS_RESOLVE_ESCAPE) != 0;
    const int missing_ok = (policy & HL_HOST_RESOLVE_ALLOW_MISSING) != 0;
    uint32_t length = 0;
    uint32_t at = 0;
    uint32_t links = 0;
    int native = 0;
    hl_status converted;

    memset(out, 0, sizeof(*out));
    out->parent = NULL;
    if (path == NULL || path_size == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);

    /* An absolute spelling replaces the pinned root outright. Callers that must
     * not allow this (open_beneath) reject the leading separator themselves. */
    if (path[0] == '/' || hl_windows_native_prefix(path, path_size)) {
        size_t consumed = 0;
        native = hl_windows_native_prefix(path, path_size);
        result = hl_windows_open_absolute_root(host, path, path_size, &consumed, &absolute_root);
        if (result.status != HL_STATUS_OK) return result;
        path += consumed;
        path_size -= consumed;
        root = absolute_root;
    }
    converted = hl_windows_wide_from_utf8(path, path_size, !native, pending,
                                          (uint32_t)(sizeof(pending) / sizeof(*pending)), &length);
    if (converted != HL_STATUS_OK) {
        if (absolute_root != NULL) CloseHandle(absolute_root);
        return hl_windows_result(converted, 0, 0);
    }
    /* In a native spelling both separators are real; in a guest spelling only
     * '/' is, and a backslash has already become 0xF05C above. */
    if (native)
        for (at = 0; at < length; ++at)
            if (pending[at] == L'\\') pending[at] = L'/';

    hl_windows_stack_init(&stack);
    {
        HANDLE base = hl_windows_duplicate(root);
        if (base == NULL || !hl_windows_stack_push(&stack, base)) {
            if (base != NULL) CloseHandle(base);
            if (absolute_root != NULL) CloseHandle(absolute_root);
            hl_windows_stack_destroy(&stack);
            return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        }
    }
    if (absolute_root != NULL) {
        CloseHandle(absolute_root);
        absolute_root = NULL;
    }

    at = 0;
    for (;;) {
        HANDLE current = stack.entries[stack.count - 1u];
        uint32_t start;
        uint32_t end;
        uint32_t rest;
        HANDLE child = NULL;
        NTSTATUS status;
        FILE_NETWORK_OPEN_INFORMATION information;
        IO_STATUS_BLOCK status_block;

        while (at < length && pending[at] == L'/')
            ++at;
        if (at >= length) { /* the path named the directory itself */
            out->parent = current;
            stack.count--;
            out->leaf_length = 0;
            out->leaf[0] = 0;
            break;
        }
        start = at;
        while (at < length && pending[at] != L'/')
            ++at;
        end = at;
        while (at < length && pending[at] == L'/')
            ++at;
        rest = at;

        if (end - start == 1u && pending[start] == L'.') continue;
        if (end - start == 2u && pending[start] == L'.' && pending[start + 1u] == L'.') {
            if (stack.count > 1u) {
                CloseHandle(stack.entries[--stack.count]);
            } else if (escape) {
                HANDLE parent = NULL;
                result = hl_windows_open_parent(host, current, &parent);
                if (result.status != HL_STATUS_OK) goto finish;
                CloseHandle(stack.entries[0]);
                stack.entries[0] = parent;
            }
            /* Without ESCAPE, ".." at the pinned root is a no-op, which is what
               makes an escape attempt harmless instead of an error. */
            continue;
        }
        if (end - start > HL_WINDOWS_NAME_MAX) {
            result = hl_windows_result(HL_STATUS_NAME_TOO_LONG, 0, 0);
            goto finish;
        }

        /* One open serves both jobs: FILE_READ_DATA lets the symlink payload be
         * sniffed and FILE_TRAVERSE lets the handle be used as a parent. */
        status = hl_windows_open_child(&host->nt, current, pending + start, end - start,
                                       FILE_READ_DATA | FILE_READ_ATTRIBUTES | FILE_TRAVERSE, 0, FILE_OPEN,
                                       FILE_OPEN_REPARSE_POINT, &child);
        if (status == HL_NT_ACCESS_DENIED)
            status = hl_windows_open_child(&host->nt, current, pending + start, end - start,
                                           FILE_READ_ATTRIBUTES | FILE_TRAVERSE, 0, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
                                           &child);
        if (status != HL_NT_SUCCESS) {
            if (missing_ok && rest >= length &&
                (status == HL_NT_OBJECT_NAME_NOT_FOUND || status == HL_NT_OBJECT_PATH_NOT_FOUND)) {
                out->parent = current;
                stack.count--;
                out->leaf_length = end - start;
                memcpy(out->leaf, pending + start, (size_t)(end - start) * sizeof(WCHAR));
                out->leaf[end - start] = 0;
                break;
            }
            result = hl_windows_nt_result(host, status);
            goto finish;
        }
        memset(&information, 0, sizeof(information));
        status = host->nt.query_information_file(child, &status_block, &information, (ULONG)sizeof(information),
                                                 FileNetworkOpenInformation);
        if (status != HL_NT_SUCCESS) {
            CloseHandle(child);
            result = hl_windows_nt_result(host, status);
            goto finish;
        }
        if (hl_windows_symlink_candidate((uint32_t)information.FileAttributes,
                                         (uint64_t)information.EndOfFile.QuadPart)) {
            char target[HL_WINDOWS_PATH_MAX];
            uint32_t target_size = 0;
            if (hl_windows_symlink_read(&host->nt, child, target, (uint32_t)sizeof(target), &target_size)) {
                CloseHandle(child);
                if ((policy & HL_HOST_RESOLVE_NO_SYMLINKS) != 0) {
                    result = hl_windows_result(HL_STATUS_SYMLINK_LOOP, 0, 0);
                    goto finish;
                }
                if (rest >= length && (policy & HL_HOST_RESOLVE_NOFOLLOW_FINAL) != 0) {
                    out->parent = current;
                    stack.count--;
                    out->leaf_length = end - start;
                    memcpy(out->leaf, pending + start, (size_t)(end - start) * sizeof(WCHAR));
                    out->leaf[end - start] = 0;
                    break;
                }
                if (target_size == 0 || ++links > 40u) {
                    result = hl_windows_result(target_size == 0 ? HL_STATUS_NOT_FOUND : HL_STATUS_SYMLINK_LOOP, 0, 0);
                    goto finish;
                }
                memcpy(scratch, pending + rest, (size_t)(length - rest) * sizeof(WCHAR));
                converted = hl_windows_join(pending, (uint32_t)(sizeof(pending) / sizeof(*pending)), target,
                                            target_size, scratch, length - rest);
                if (converted != HL_STATUS_OK) {
                    result = hl_windows_result(converted, 0, 0);
                    goto finish;
                }
                /* An absolute target restarts at the pinned root rather than at
                 * a host root, exactly as the POSIX resolver treats it. */
                if (target[0] == '/')
                    while (stack.count > 1u)
                        CloseHandle(stack.entries[--stack.count]);
                at = 0;
                length = (uint32_t)wcslen(pending);
                continue;
            }
        }
        if (rest >= length) {
            CloseHandle(child);
            out->parent = current;
            stack.count--;
            out->leaf_length = end - start;
            memcpy(out->leaf, pending + start, (size_t)(end - start) * sizeof(WCHAR));
            out->leaf[end - start] = 0;
            break;
        }
        if ((information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
            CloseHandle(child);
            result = hl_windows_result(HL_STATUS_NOT_DIRECTORY, 0, 0);
            goto finish;
        }
        if (!hl_windows_stack_push(&stack, child)) {
            CloseHandle(child);
            result = hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
            goto finish;
        }
    }

finish:
    hl_windows_stack_destroy(&stack);
    if (result.status != HL_STATUS_OK && out->parent != NULL) {
        CloseHandle(out->parent);
        out->parent = NULL;
    }
    return result;
}
