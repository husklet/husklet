/*
 * The network group: sockets as objects, over Winsock, with nothing about
 * Winsock crossing the seam.
 *
 * Four decisions shape this file. Each is a measurement, not a preference.
 *
 * 1. WS2_32 IS RESOLVED BY NAME, LAZILY, AND IS NEVER IMPORTED.
 *
 *    The engine's PE import table is allowed to name kernel32.dll, ntdll.dll
 *    and the UCRT apisets and nothing else. That is not tidiness: a guest fork
 *    is served by RtlCloneUserProcess, and an unlisted DLL is an unaudited
 *    DLL_THREAD_DETACH handler running in a process whose win32k-side state the
 *    clone did not carry. Linking -lws2_32 would put ws2_32.dll in the import
 *    table of every engine binary, including every binary that never opens a
 *    socket. So the whole vocabulary below is a function table filled in by
 *    GetProcAddress, exactly as the KernelBase and ntdll entry points already
 *    are, and the library is loaded on the FIRST socket() and not at host
 *    creation -- a guest that never makes a socket never maps ws2_32 at all and
 *    its fork path is unchanged.
 *
 * 2. EVERY SOCKET IS PERMANENTLY NON-BLOCKING; BLOCKING IS SYNTHESISED.
 *
 *    Windows cannot be asked whether a socket is non-blocking. ioctlsocket
 *    FIONBIO is write-only, and using it as a getter does not read the flag --
 *    it SETS it from whatever the output parameter happened to contain. So the
 *    flag has to live here, in the object, which is also where it belongs:
 *    dup(2) semantics require two handles onto one socket to share it, and a
 *    flag stored per handle could not do that.
 *
 *    WSAEventSelect is what makes the object waitable, and it also forces the
 *    socket into non-blocking mode as a side effect that cannot be undone
 *    piecemeal. Rather than fight that, it is adopted: every operation runs
 *    non-blocking, and when the caller's flag says the socket blocks, the
 *    operation is retried around a wait on the object's event. There is no lost
 *    wakeup in that loop, because Winsock re-arms the event's FD_READ/FD_WRITE
 *    record on the failing call itself -- the WSAEWOULDBLOCK that sends us to
 *    the wait is the same call that arms the wake.
 *
 * 3. SO_REUSEADDR INVERTS, AND GETTING IT WRONG IS A SECURITY DEFECT.
 *
 *    Windows' DEFAULT bind behaviour is already Linux's SO_REUSEADDR=1: a
 *    port left in TIME_WAIT can be rebound. Windows' option of that NAME does
 *    something else entirely -- it permits binding over a LIVE listener, which
 *    was measured on this machine stealing an established server's port. So the
 *    contract's REUSE_ADDRESS is implemented by its definition and not by its
 *    name: 1 is a no-op because the behaviour is already there, and 0 is
 *    SO_EXCLUSIVEADDRUSE, which is the only value that needs an action. Setting
 *    the Windows option of the same name would hand a guest the ability to take
 *    another process's bound port.
 *
 * 4. ABSTRACT AF_UNIX NAMES ARE REFUSED HERE RATHER THAN PASSED DOWN.
 *
 *    A sun_path beginning with NUL is Linux's abstract namespace. Windows'
 *    AF_UNIX does not implement it and, measured, does not say so: two binds to
 *    the same abstract name BOTH return 0, and the mistake only surfaces later
 *    as WSAEINVAL from a connect that cannot find anything. A silent double
 *    success is worse than any refusal, so the refusal is made here where it can
 *    still be typed.
 *
 * Handle model: a slot's payload is a reference-counted object shared by every
 * handle onto the same socket, the way an open file description is shared by
 * dup(2)ed descriptors. duplicate() therefore takes a reference rather than
 * calling DuplicateHandle: the status flags and the latched connect error have
 * to be visible through both handles, and two kernel handles would give two
 * copies of neither.
 */
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#include <afunix.h>

#include "internal.h"

#include <stdlib.h>
#include <string.h>

/* --- the dynamically resolved Winsock vocabulary ---------------------------- */

typedef struct hl_windows_winsock {
    HMODULE module;
    int(WSAAPI *startup)(WORD, LPWSADATA);
    int(WSAAPI *last_error)(void);
    SOCKET(WSAAPI *socket)(int, int, int);
    int(WSAAPI *close_socket)(SOCKET);
    int(WSAAPI *bind)(SOCKET, const struct sockaddr *, int);
    int(WSAAPI *listen)(SOCKET, int);
    SOCKET(WSAAPI *accept)(SOCKET, struct sockaddr *, int *);
    int(WSAAPI *connect)(SOCKET, const struct sockaddr *, int);
    int(WSAAPI *shutdown)(SOCKET, int);
    int(WSAAPI *get_sock_name)(SOCKET, struct sockaddr *, int *);
    int(WSAAPI *get_peer_name)(SOCKET, struct sockaddr *, int *);
    int(WSAAPI *get_sock_opt)(SOCKET, int, int, char *, int *);
    int(WSAAPI *set_sock_opt)(SOCKET, int, int, const char *, int);
    int(WSAAPI *ioctl_socket)(SOCKET, long, u_long *);
    int(WSAAPI *select)(int, fd_set *, fd_set *, fd_set *, const struct timeval *);
    int(WSAAPI *event_select)(SOCKET, WSAEVENT, long);
    int(WSAAPI *enum_network_events)(SOCKET, WSAEVENT, LPWSANETWORKEVENTS);
    int(WSAAPI *send)(SOCKET, LPWSABUF, DWORD, LPDWORD, DWORD, LPWSAOVERLAPPED, LPWSAOVERLAPPED_COMPLETION_ROUTINE);
    int(WSAAPI *receive)(SOCKET, LPWSABUF, DWORD, LPDWORD, LPDWORD, LPWSAOVERLAPPED,
                         LPWSAOVERLAPPED_COMPLETION_ROUTINE);
    int(WSAAPI *send_to)(SOCKET, LPWSABUF, DWORD, LPDWORD, DWORD, const struct sockaddr *, int, LPWSAOVERLAPPED,
                         LPWSAOVERLAPPED_COMPLETION_ROUTINE);
    int(WSAAPI *receive_from)(SOCKET, LPWSABUF, DWORD, LPDWORD, LPDWORD, struct sockaddr *, LPINT, LPWSAOVERLAPPED,
                              LPWSAOVERLAPPED_COMPLETION_ROUTINE);
} hl_windows_winsock;

static hl_windows_winsock g_winsock;
static INIT_ONCE g_winsock_once = INIT_ONCE_STATIC_INIT;

#define HL_WINDOWS_BIND_WINSOCK(field, name)                                                                           \
    do {                                                                                                               \
        FARPROC symbol = GetProcAddress(module, (name));                                                               \
        if (symbol == NULL) return FALSE;                                                                              \
        *(FARPROC *)&g_winsock.field = symbol;                                                                         \
    } while (0)

static BOOL CALLBACK hl_windows_winsock_resolve(PINIT_ONCE once, PVOID parameter, PVOID *context) {
    HMODULE module;
    WSADATA data;
    (void)once;
    (void)parameter;
    (void)context;
    module = LoadLibraryW(L"ws2_32.dll");
    if (module == NULL) return FALSE;
    HL_WINDOWS_BIND_WINSOCK(startup, "WSAStartup");
    HL_WINDOWS_BIND_WINSOCK(last_error, "WSAGetLastError");
    HL_WINDOWS_BIND_WINSOCK(socket, "socket");
    HL_WINDOWS_BIND_WINSOCK(close_socket, "closesocket");
    HL_WINDOWS_BIND_WINSOCK(bind, "bind");
    HL_WINDOWS_BIND_WINSOCK(listen, "listen");
    HL_WINDOWS_BIND_WINSOCK(accept, "accept");
    HL_WINDOWS_BIND_WINSOCK(connect, "connect");
    HL_WINDOWS_BIND_WINSOCK(shutdown, "shutdown");
    HL_WINDOWS_BIND_WINSOCK(get_sock_name, "getsockname");
    HL_WINDOWS_BIND_WINSOCK(get_peer_name, "getpeername");
    HL_WINDOWS_BIND_WINSOCK(get_sock_opt, "getsockopt");
    HL_WINDOWS_BIND_WINSOCK(set_sock_opt, "setsockopt");
    HL_WINDOWS_BIND_WINSOCK(ioctl_socket, "ioctlsocket");
    HL_WINDOWS_BIND_WINSOCK(select, "select");
    HL_WINDOWS_BIND_WINSOCK(event_select, "WSAEventSelect");
    HL_WINDOWS_BIND_WINSOCK(enum_network_events, "WSAEnumNetworkEvents");
    HL_WINDOWS_BIND_WINSOCK(send, "WSASend");
    HL_WINDOWS_BIND_WINSOCK(receive, "WSARecv");
    HL_WINDOWS_BIND_WINSOCK(send_to, "WSASendTo");
    HL_WINDOWS_BIND_WINSOCK(receive_from, "WSARecvFrom");
    /* 2.2 has shipped since Winsock 2 and this is the only version this file is
     * written against; a machine that cannot provide it is refused rather than
     * degraded, because every negotiated-down version differs in exactly the
     * places -- WSARecvFrom's flags, AF_UNIX -- that matter here. */
    if (g_winsock.startup(MAKEWORD(2, 2), &data) != 0) return FALSE;
    g_winsock.module = module;
    return TRUE;
}

/* WSACleanup is deliberately never called. The table is process-lifetime and a
 * teardown that raced a socket still in flight would be strictly worse than
 * leaving the library mapped for the few milliseconds before exit. */
static const hl_windows_winsock *hl_windows_winsock_get(void) {
    if (!InitOnceExecuteOnce(&g_winsock_once, hl_windows_winsock_resolve, NULL, NULL)) return NULL;
    return &g_winsock;
}

/* --- errors ----------------------------------------------------------------- */

/*
 * A Winsock error is reported twice: as the coarse hl_status a caller may act on
 * without knowing which host it has, and as the precise neutral condition that
 * status cannot express. Both are named here in one table so the two can never
 * drift apart, and neither is the WSAE* number -- that stays in `detail` only
 * when there is no condition for it, tagged with its own domain.
 */
typedef struct hl_windows_socket_error {
    int code;
    hl_status status;
    uint32_t condition;
} hl_windows_socket_error;

static const hl_windows_socket_error hl_windows_socket_errors[] = {
    {WSAEWOULDBLOCK, HL_STATUS_WOULD_BLOCK, HL_HOST_NETWORK_CONDITION_WOULD_BLOCK},
    {WSAEINPROGRESS, HL_STATUS_WOULD_BLOCK, HL_HOST_NETWORK_CONDITION_CONNECT_IN_PROGRESS},
    {WSAEALREADY, HL_STATUS_BUSY, HL_HOST_NETWORK_CONDITION_CONNECT_PENDING},
    {WSAENOTSOCK, HL_STATUS_INVALID_ARGUMENT, HL_HOST_NETWORK_CONDITION_NOT_A_SOCKET},
    {WSAEDESTADDRREQ, HL_STATUS_INVALID_ARGUMENT, HL_HOST_NETWORK_CONDITION_DESTINATION_REQUIRED},
    {WSAEMSGSIZE, HL_STATUS_INVALID_ARGUMENT, HL_HOST_NETWORK_CONDITION_MESSAGE_TOO_LARGE},
    {WSAEPROTOTYPE, HL_STATUS_INVALID_ARGUMENT, HL_HOST_NETWORK_CONDITION_WRONG_PROTOCOL},
    {WSAENOPROTOOPT, HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_OPTION_NOT_SUPPORTED},
    {WSAEPROTONOSUPPORT, HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_PROTOCOL_NOT_SUPPORTED},
    {WSAESOCKTNOSUPPORT, HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_TYPE_NOT_SUPPORTED},
    {WSAEOPNOTSUPP, HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_OPERATION_NOT_SUPPORTED},
    {WSAEPFNOSUPPORT, HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_FAMILY_NOT_SUPPORTED},
    {WSAEAFNOSUPPORT, HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_FAMILY_NOT_SUPPORTED},
    {WSAEADDRINUSE, HL_STATUS_ADDRESS_IN_USE, HL_HOST_NETWORK_CONDITION_ADDRESS_IN_USE},
    {WSAEADDRNOTAVAIL, HL_STATUS_NOT_FOUND, HL_HOST_NETWORK_CONDITION_ADDRESS_NOT_AVAILABLE},
    {WSAENETDOWN, HL_STATUS_NETWORK_UNREACHABLE, HL_HOST_NETWORK_CONDITION_NETWORK_DOWN},
    {WSAENETUNREACH, HL_STATUS_NETWORK_UNREACHABLE, HL_HOST_NETWORK_CONDITION_NETWORK_UNREACHABLE},
    {WSAENETRESET, HL_STATUS_CONNECTION_RESET, HL_HOST_NETWORK_CONDITION_NETWORK_RESET},
    {WSAECONNABORTED, HL_STATUS_CONNECTION_RESET, HL_HOST_NETWORK_CONDITION_CONNECTION_ABORTED},
    {WSAECONNRESET, HL_STATUS_CONNECTION_RESET, HL_HOST_NETWORK_CONDITION_CONNECTION_RESET},
    {WSAENOBUFS, HL_STATUS_OUT_OF_MEMORY, HL_HOST_NETWORK_CONDITION_BUFFER_EXHAUSTED},
    {WSAEISCONN, HL_STATUS_ALREADY_EXISTS, HL_HOST_NETWORK_CONDITION_ALREADY_CONNECTED},
    {WSAENOTCONN, HL_STATUS_DISCONNECTED, HL_HOST_NETWORK_CONDITION_NOT_CONNECTED},
    {WSAESHUTDOWN, HL_STATUS_DISCONNECTED, HL_HOST_NETWORK_CONDITION_SHUT_DOWN},
    {WSAETIMEDOUT, HL_STATUS_TIMED_OUT, HL_HOST_NETWORK_CONDITION_TIMED_OUT},
    {WSAECONNREFUSED, HL_STATUS_CONNECTION_REFUSED, HL_HOST_NETWORK_CONDITION_CONNECTION_REFUSED},
    {WSAEHOSTDOWN, HL_STATUS_NETWORK_UNREACHABLE, HL_HOST_NETWORK_CONDITION_HOST_UNREACHABLE},
    {WSAEHOSTUNREACH, HL_STATUS_NETWORK_UNREACHABLE, HL_HOST_NETWORK_CONDITION_HOST_UNREACHABLE},
    {WSAEACCES, HL_STATUS_PERMISSION_DENIED, HL_HOST_NETWORK_CONDITION_NONE},
    {WSAEFAULT, HL_STATUS_INVALID_ARGUMENT, HL_HOST_NETWORK_CONDITION_NONE},
    {WSAEINVAL, HL_STATUS_INVALID_ARGUMENT, HL_HOST_NETWORK_CONDITION_NONE},
    {WSAEMFILE, HL_STATUS_PROCESS_LIMIT, HL_HOST_NETWORK_CONDITION_NONE},
    {WSAEINTR, HL_STATUS_INTERRUPTED, HL_HOST_NETWORK_CONDITION_INTERRUPTED},
    {WSAENAMETOOLONG, HL_STATUS_NAME_TOO_LONG, HL_HOST_NETWORK_CONDITION_NONE},
    {WSAELOOP, HL_STATUS_SYMLINK_LOOP, HL_HOST_NETWORK_CONDITION_NONE},
    {WSAENOTEMPTY, HL_STATUS_NOT_EMPTY, HL_HOST_NETWORK_CONDITION_NONE},
    {WSAEPROCLIM, HL_STATUS_PROCESS_LIMIT, HL_HOST_NETWORK_CONDITION_NONE},
    {WSASYSNOTREADY, HL_STATUS_PLATFORM_FAILURE, HL_HOST_NETWORK_CONDITION_NETWORK_DOWN},
    {WSANOTINITIALISED, HL_STATUS_PLATFORM_FAILURE, HL_HOST_NETWORK_CONDITION_NONE}};

static hl_host_result hl_windows_socket_error_result(int code) {
    size_t index;
    for (index = 0; index < HL_ARRAY_COUNT(hl_windows_socket_errors); ++index) {
        if (hl_windows_socket_errors[index].code != code) continue;
        if (hl_windows_socket_errors[index].condition == HL_HOST_NETWORK_CONDITION_NONE)
            return (hl_host_result){(int32_t)hl_windows_socket_errors[index].status, HL_HOST_DETAIL_WIN32, 0,
                                    (uint64_t)(uint32_t)code};
        return (hl_host_result){(int32_t)hl_windows_socket_errors[index].status, HL_HOST_DETAIL_NETWORK, 0,
                                hl_windows_socket_errors[index].condition};
    }
    /* An unmapped WSAE* stays a platform failure carrying its own number rather
     * than being folded into a neighbour: guessing here is how a guest ends up
     * retrying something that will never succeed. */
    return (hl_host_result){(int32_t)HL_STATUS_PLATFORM_FAILURE, HL_HOST_DETAIL_WIN32, 0, (uint64_t)(uint32_t)code};
}

static hl_status hl_windows_socket_error_status(int code) {
    size_t index;
    for (index = 0; index < HL_ARRAY_COUNT(hl_windows_socket_errors); ++index)
        if (hl_windows_socket_errors[index].code == code) return hl_windows_socket_errors[index].status;
    return HL_STATUS_PLATFORM_FAILURE;
}

static hl_host_result hl_windows_socket_last_error(const hl_windows_winsock *ws) {
    return hl_windows_socket_error_result(ws->last_error());
}

static hl_host_result hl_windows_socket_condition(hl_status status, uint32_t condition) {
    return (hl_host_result){(int32_t)status, HL_HOST_DETAIL_NETWORK, 0, condition};
}

static hl_host_result hl_windows_socket_ok(uint64_t value) {
    return (hl_host_result){(int32_t)HL_STATUS_OK, HL_HOST_DETAIL_NONE, value, 0};
}

/* --- the object ------------------------------------------------------------- */

enum {
    HL_WINDOWS_SOCKET_EVENTS = FD_READ | FD_WRITE | FD_ACCEPT | FD_CONNECT | FD_CLOSE | FD_OOB,
    /* Every wait in this file is bounded so a socket closed under a blocked
     * operation, or an FD_* edge Winsock declines to repeat, cannot wedge a
     * guest thread forever. The loop re-derives readiness on each pass, so the
     * only cost of a wakeup with nothing to do is one retried syscall. */
    HL_WINDOWS_SOCKET_WAIT_SLICE_MS = 50
};

typedef struct hl_windows_socket_object {
    CRITICAL_SECTION lock;
    SOCKET socket;
    HANDLE ready;      /* manual-reset; WSAEventSelect's event. Also set by close, to release waiters. */
    uint32_t family;   /* hl_host_network_family */
    uint32_t type;     /* hl_host_network_type */
    uint32_t protocol; /* as the caller asked for it, so SOCKOPT_PROTOCOL can answer */
    uint32_t flags;    /* HL_HOST_SOCKET_NONBLOCK */
    uint32_t listening;
    uint32_t connecting;
    uint32_t closing;
    /* The WSAE* that FD_CONNECT reported, latched. Windows' own SO_ERROR is
     * sticky where Linux's is read-and-clear, so the read-and-clear semantics
     * are provided here rather than trusted to the host. */
    int32_t pending_error;
    uint32_t references;
    /* The name this socket was bound to, kept because a Windows AF_UNIX
     * getsockname on a bound socket answers with the path only sometimes, and
     * answering from what we were asked for is both cheaper and always right. */
    char local_path[108];
    uint16_t local_path_size;
    uint16_t local_path_valid;
} hl_windows_socket_object;

static void hl_windows_socket_object_release(hl_windows_socket_object *object) {
    uint32_t remaining;
    const hl_windows_winsock *ws;
    EnterCriticalSection(&object->lock);
    remaining = object->references == 0 ? 0 : --object->references;
    LeaveCriticalSection(&object->lock);
    if (remaining != 0) return;
    ws = hl_windows_winsock_get();
    if (ws != NULL && object->socket != INVALID_SOCKET) (void)ws->close_socket(object->socket);
    if (object->ready != NULL) CloseHandle(object->ready);
    DeleteCriticalSection(&object->lock);
    free(object);
}

void hl_windows_socket_destroy_entry(hl_windows_handle_entry *entry) {
    hl_windows_socket_object *object = entry->payload;
    entry->payload = NULL;
    entry->object = NULL;
    if (object != NULL) hl_windows_socket_object_release(object);
}

HANDLE hl_windows_socket_wait_handle_locked(const hl_windows_handle_entry *entry) {
    const hl_windows_socket_object *object = entry->payload;
    return object == NULL ? NULL : object->ready;
}

static hl_windows_socket_object *hl_windows_socket_object_create(const hl_windows_winsock *ws, SOCKET socket,
                                                                 uint32_t family, uint32_t type, uint32_t protocol,
                                                                 uint32_t flags) {
    hl_windows_socket_object *object = calloc(1, sizeof(*object));
    if (object == NULL) return NULL;
    InitializeCriticalSection(&object->lock);
    object->socket = socket;
    object->family = family;
    object->type = type;
    object->protocol = protocol;
    object->flags = flags;
    object->references = 1;
    object->ready = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (object->ready == NULL || ws->event_select(socket, object->ready, HL_WINDOWS_SOCKET_EVENTS) != 0) {
        if (object->ready != NULL) CloseHandle(object->ready);
        DeleteCriticalSection(&object->lock);
        free(object);
        return NULL;
    }
    return object;
}

static hl_host_result hl_windows_socket_publish(hl_host_windows *host, hl_windows_socket_object *object) {
    hl_host_result allocated = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_SOCKET);
    hl_windows_handle_entry *entry;
    if (allocated.status != HL_STATUS_OK) return allocated;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, allocated.value, HL_WINDOWS_HANDLE_SOCKET);
    if (entry == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    entry->payload = object;
    /* The slot's HANDLE field carries the SOCKET too. A SOCKET is a real kernel
     * handle -- GetHandleInformation and DuplicateHandle both succeed on one --
     * so this is not a cast of convenience, and it is what lets any code that
     * walks the table for teardown see something it recognises. */
    entry->object = (HANDLE)object->socket;
    hl_windows_unlock(host);
    return hl_windows_socket_ok(allocated.value);
}

static hl_windows_socket_object *hl_windows_socket_acquire(hl_host_windows *host, hl_host_handle handle) {
    hl_windows_handle_entry *entry;
    hl_windows_socket_object *object = NULL;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_SOCKET);
    if (entry != NULL && entry->payload != NULL) {
        object = entry->payload;
        EnterCriticalSection(&object->lock);
        object->references++;
        LeaveCriticalSection(&object->lock);
    }
    hl_windows_unlock(host);
    return object;
}

/* --- events and blocking synthesis ------------------------------------------ */

/*
 * Drain the socket's recorded network events into the object. WSAEnumNetworkEvents
 * resets the event and CLEARS the record as it reads it, so anything worth
 * keeping past this call has to be latched here -- which is exactly the connect
 * result, the only edge Winsock reports once and never again.
 */
static void hl_windows_socket_drain(const hl_windows_winsock *ws, hl_windows_socket_object *object) {
    WSANETWORKEVENTS events;
    memset(&events, 0, sizeof(events));
    if (ws->enum_network_events(object->socket, object->ready, &events) != 0) return;
    EnterCriticalSection(&object->lock);
    if ((events.lNetworkEvents & FD_CONNECT) != 0) {
        object->connecting = 0;
        if (events.iErrorCode[FD_CONNECT_BIT] != 0) object->pending_error = events.iErrorCode[FD_CONNECT_BIT];
    }
    if ((events.lNetworkEvents & FD_CLOSE) != 0 && events.iErrorCode[FD_CLOSE_BIT] != 0 && object->pending_error == 0)
        object->pending_error = events.iErrorCode[FD_CLOSE_BIT];
    LeaveCriticalSection(&object->lock);
}

/*
 * One pass of the blocking retry loop. Returns non-zero to retry.
 *
 * The wait is on the object's own event, which Winsock re-armed on the very call
 * that returned WSAEWOULDBLOCK, so there is no window in which an arrival
 * between the failing call and this wait could be missed. The slice bound is not
 * a poll: it is there so that a close of the socket under this thread, or an
 * edge Winsock chooses not to repeat, costs one retried syscall rather than a
 * wedged guest thread.
 */
static int hl_windows_socket_block(const hl_windows_winsock *ws, hl_windows_socket_object *object) {
    uint32_t blocking;
    uint32_t closing;
    EnterCriticalSection(&object->lock);
    blocking = (object->flags & HL_HOST_SOCKET_NONBLOCK) == 0;
    closing = object->closing;
    LeaveCriticalSection(&object->lock);
    if (!blocking || closing) return 0;
    (void)WaitForSingleObject(object->ready, HL_WINDOWS_SOCKET_WAIT_SLICE_MS);
    hl_windows_socket_drain(ws, object);
    return 1;
}

/* --- addresses -------------------------------------------------------------- */

static int hl_windows_socket_native_family(uint32_t family) {
    switch (family) {
    case HL_HOST_NETWORK_IPV4: return AF_INET;
    case HL_HOST_NETWORK_IPV6: return AF_INET6;
    case HL_HOST_NETWORK_LOCAL: return AF_UNIX;
    default: return -1;
    }
}

static int hl_windows_socket_native_type(uint32_t type) {
    switch (type) {
    case HL_HOST_NETWORK_STREAM: return SOCK_STREAM;
    case HL_HOST_NETWORK_DATAGRAM: return SOCK_DGRAM;
    case HL_HOST_NETWORK_SEQPACKET: return SOCK_SEQPACKET;
    case HL_HOST_NETWORK_RAW: return SOCK_RAW;
    default: return -1;
    }
}

/*
 * Contract address -> sockaddr. Port is host order on the contract side and
 * network order on the wire, and this is the only place the swap happens, on
 * purpose: a swap that appears in two places eventually appears in one of them
 * twice.
 */
static hl_status hl_windows_socket_encode(const hl_host_network_address *address, struct sockaddr_storage *storage,
                                          int *size) {
    memset(storage, 0, sizeof(*storage));
    if (address == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (address->family == HL_HOST_NETWORK_IPV4 && address->size == 4) {
        struct sockaddr_in *ipv4 = (struct sockaddr_in *)storage;
        ipv4->sin_family = AF_INET;
        ipv4->sin_port = (u_short)(((address->port & 0xffu) << 8) | ((address->port >> 8) & 0xffu));
        memcpy(&ipv4->sin_addr, address->address, 4);
        *size = (int)sizeof(*ipv4);
        return HL_STATUS_OK;
    }
    if (address->family == HL_HOST_NETWORK_IPV6 && address->size == 16) {
        struct sockaddr_in6 *ipv6 = (struct sockaddr_in6 *)storage;
        ipv6->sin6_family = AF_INET6;
        ipv6->sin6_port = (u_short)(((address->port & 0xffu) << 8) | ((address->port >> 8) & 0xffu));
        memcpy(&ipv6->sin6_addr, address->address, 16);
        ipv6->sin6_scope_id = address->scope_id;
        ipv6->sin6_flowinfo = address->flow_info;
        *size = (int)sizeof(*ipv6);
        return HL_STATUS_OK;
    }
    if (address->family == HL_HOST_NETWORK_LOCAL) {
        struct sockaddr_un *local = (struct sockaddr_un *)storage;
        if (address->size == 0 || address->size >= sizeof(address->local_path)) return HL_STATUS_INVALID_ARGUMENT;
        /* An abstract name -- a leading NUL -- is refused here rather than passed
         * down. Windows accepts the bind and returns 0 for TWO processes binding
         * the same abstract name; the mistake only appears as a WSAEINVAL from a
         * later connect, by which point neither side can attribute it. */
        if (address->local_path[0] == '\0') return HL_STATUS_NOT_SUPPORTED;
        local->sun_family = AF_UNIX;
        memcpy(local->sun_path, address->local_path, address->size);
        local->sun_path[address->size] = '\0';
        *size = (int)(offsetof(struct sockaddr_un, sun_path) + address->size + 1u);
        return HL_STATUS_OK;
    }
    return HL_STATUS_INVALID_ARGUMENT;
}

/*
 * sockaddr -> contract address.
 *
 * The AF_UNIX arm is where a measured Windows quirk is absorbed: an unnamed
 * local peer comes back with the FULL 110-byte sockaddr_un and a zeroed
 * sun_path, not with Linux's 2. Deriving the name length from the path itself
 * rather than from the reported length is right on both hosts and needs no
 * per-host branch.
 */
static hl_status hl_windows_socket_decode(const struct sockaddr_storage *storage, int size,
                                          hl_host_network_address *out) {
    memset(out, 0, sizeof(*out));
    if (size <= 0) return HL_STATUS_INVALID_ARGUMENT;
    if (storage->ss_family == AF_INET && size >= (int)sizeof(struct sockaddr_in)) {
        const struct sockaddr_in *ipv4 = (const struct sockaddr_in *)storage;
        out->family = HL_HOST_NETWORK_IPV4;
        out->port = (uint16_t)(((ipv4->sin_port & 0xffu) << 8) | ((ipv4->sin_port >> 8) & 0xffu));
        out->size = 4;
        memcpy(out->address, &ipv4->sin_addr, 4);
        return HL_STATUS_OK;
    }
    if (storage->ss_family == AF_INET6 && size >= (int)sizeof(struct sockaddr_in6)) {
        const struct sockaddr_in6 *ipv6 = (const struct sockaddr_in6 *)storage;
        out->family = HL_HOST_NETWORK_IPV6;
        out->port = (uint16_t)(((ipv6->sin6_port & 0xffu) << 8) | ((ipv6->sin6_port >> 8) & 0xffu));
        out->size = 16;
        memcpy(out->address, &ipv6->sin6_addr, 16);
        out->scope_id = ipv6->sin6_scope_id;
        out->flow_info = ipv6->sin6_flowinfo;
        return HL_STATUS_OK;
    }
    if (storage->ss_family == AF_UNIX) {
        const struct sockaddr_un *local = (const struct sockaddr_un *)storage;
        size_t length = 0;
        while (length < sizeof(local->sun_path) && local->sun_path[length] != '\0')
            length++;
        out->family = HL_HOST_NETWORK_LOCAL;
        out->size = (uint16_t)length;
        if (length != 0) memcpy(out->local_path, local->sun_path, length);
        return HL_STATUS_OK;
    }
    return HL_STATUS_NOT_SUPPORTED;
}

/* --- creation and teardown -------------------------------------------------- */

static hl_host_result hl_windows_network_socket(void *context, uint32_t family, uint32_t type, uint32_t protocol) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *object;
    hl_host_result published;
    SOCKET socket;
    int native_family = hl_windows_socket_native_family(family);
    int native_type = hl_windows_socket_native_type(type);
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    if (native_family < 0)
        return hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_FAMILY_NOT_SUPPORTED);
    if (native_type < 0)
        return hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_TYPE_NOT_SUPPORTED);
    /* AF_UNIX here is stream only, and the refusal is typed rather than left to
     * Winsock: a local datagram or seqpacket socket answers WSAEAFNOSUPPORT,
     * which reads to a caller as "this machine has no AF_UNIX at all" and sends
     * it down a TCP fallback. What is actually missing is the socket TYPE. */
    if (family == HL_HOST_NETWORK_LOCAL && type != HL_HOST_NETWORK_STREAM)
        return hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_TYPE_NOT_SUPPORTED);
    socket = ws->socket(native_family, native_type, (int)protocol);
    if (socket == INVALID_SOCKET) return hl_windows_socket_last_error(ws);
    object = hl_windows_socket_object_create(ws, socket, family, type, protocol, 0);
    if (object == NULL) {
        (void)ws->close_socket(socket);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    published = hl_windows_socket_publish(host, object);
    if (published.status != HL_STATUS_OK) hl_windows_socket_object_release(object);
    return published;
}

static hl_host_result hl_windows_network_close(void *context, hl_host_handle handle) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_windows_socket_object *object;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_SOCKET);
    if (entry == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object = entry->payload;
    hl_windows_clear_entry_locked(entry);
    hl_windows_unlock(host);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* Release anything blocked on this object before dropping the reference. The
     * retry loop re-reads `closing` on every pass, so a thread parked inside a
     * blocking receive leaves with a typed failure instead of waiting out a
     * peer that is never going to speak again. */
    EnterCriticalSection(&object->lock);
    object->closing = 1;
    LeaveCriticalSection(&object->lock);
    SetEvent(object->ready);
    hl_windows_socket_object_release(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_network_duplicate(void *context, hl_host_handle handle) {
    hl_host_windows *host = context;
    hl_windows_socket_object *object = hl_windows_socket_acquire(host, handle);
    hl_host_result published;
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* acquire already took the reference the new slot will own. */
    published = hl_windows_socket_publish(host, object);
    if (published.status != HL_STATUS_OK) hl_windows_socket_object_release(object);
    return published;
}

/* --- naming ----------------------------------------------------------------- */

static hl_host_result hl_windows_network_bind(void *context, hl_host_handle handle,
                                              const hl_host_network_address *address) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *object;
    struct sockaddr_storage storage;
    hl_host_result result;
    hl_status status;
    int size = 0;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    status = hl_windows_socket_encode(address, &storage, &size);
    if (status != HL_STATUS_OK)
        return status == HL_STATUS_NOT_SUPPORTED
                   ? hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED,
                                                 HL_HOST_NETWORK_CONDITION_ADDRESS_NOT_AVAILABLE)
                   : hl_windows_result(status, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (ws->bind(object->socket, (const struct sockaddr *)&storage, size) != 0) {
        result = hl_windows_socket_last_error(ws);
    } else {
        if (address->family == HL_HOST_NETWORK_LOCAL) {
            EnterCriticalSection(&object->lock);
            memcpy(object->local_path, address->local_path, address->size);
            object->local_path_size = address->size;
            object->local_path_valid = 1;
            LeaveCriticalSection(&object->lock);
        }
        result = hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    hl_windows_socket_object_release(object);
    return result;
}

static hl_host_result hl_windows_network_listen(void *context, hl_host_handle handle, uint32_t backlog) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *object;
    hl_host_result result;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (backlog > (uint32_t)INT_MAX) backlog = (uint32_t)INT_MAX;
    if (ws->listen(object->socket, (int)backlog) != 0) {
        result = hl_windows_socket_last_error(ws);
    } else {
        EnterCriticalSection(&object->lock);
        object->listening = 1;
        LeaveCriticalSection(&object->lock);
        result = hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    hl_windows_socket_object_release(object);
    return result;
}

static hl_host_result hl_windows_network_accept(void *context, hl_host_handle handle, hl_host_network_address *peer,
                                                uint32_t flags) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *listener;
    hl_windows_socket_object *object;
    hl_host_result result;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    listener = hl_windows_socket_acquire(host, handle);
    if (listener == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (;;) {
        struct sockaddr_storage storage;
        int size = (int)sizeof(storage);
        SOCKET accepted;
        memset(&storage, 0, sizeof(storage));
        accepted = ws->accept(listener->socket, (struct sockaddr *)&storage, &size);
        if (accepted == INVALID_SOCKET) {
            const int code = ws->last_error();
            if (code == WSAEWOULDBLOCK && hl_windows_socket_block(ws, listener)) continue;
            result = listener->closing ? hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0)
                                       : hl_windows_socket_error_result(code);
            break;
        }
        /* An accepted socket inherits the LISTENER's WSAEventSelect settings,
         * event handle included. Left alone it would signal the listener's event
         * and be invisible to any wait of its own, so the object constructor's
         * WSAEventSelect on the new socket is not decoration -- it is what
         * separates the two. */
        object = hl_windows_socket_object_create(ws, accepted, listener->family, listener->type, listener->protocol,
                                                 flags & HL_HOST_SOCKET_NONBLOCK);
        if (object == NULL) {
            (void)ws->close_socket(accepted);
            result = hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
            break;
        }
        if (peer != NULL && hl_windows_socket_decode(&storage, size, peer) != HL_STATUS_OK) {
            memset(peer, 0, sizeof(*peer));
            peer->family = listener->family;
        }
        result = hl_windows_socket_publish(host, object);
        if (result.status != HL_STATUS_OK) hl_windows_socket_object_release(object);
        break;
    }
    hl_windows_socket_object_release(listener);
    return result;
}

static hl_host_result hl_windows_network_connect(void *context, hl_host_handle handle,
                                                 const hl_host_network_address *address) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *object;
    struct sockaddr_storage storage;
    hl_host_result result;
    hl_status status;
    int size = 0;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    status = hl_windows_socket_encode(address, &storage, &size);
    if (status != HL_STATUS_OK)
        return status == HL_STATUS_NOT_SUPPORTED
                   ? hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED,
                                                 HL_HOST_NETWORK_CONDITION_ADDRESS_NOT_AVAILABLE)
                   : hl_windows_result(status, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (unsigned attempt = 0;; ++attempt) {
        int code;
        if (ws->connect(object->socket, (const struct sockaddr *)&storage, size) == 0) {
            EnterCriticalSection(&object->lock);
            object->connecting = 0;
            LeaveCriticalSection(&object->lock);
            result = hl_windows_result(HL_STATUS_OK, 0, 0);
            break;
        }
        code = ws->last_error();
        /* WSAEISCONN on a RETRY is the completion, not a failure: the connect this
         * loop already issued has finished, and Winsock reports a second attempt
         * on an established socket that way. Only a FIRST connect that answers
         * WSAEISCONN is the caller's own mistake, which is why the attempt count
         * is what separates them -- there is no other signal that does. */
        if (code == WSAEISCONN && attempt != 0) {
            EnterCriticalSection(&object->lock);
            object->connecting = 0;
            LeaveCriticalSection(&object->lock);
            result = hl_windows_result(HL_STATUS_OK, 0, 0);
            break;
        }
        if (code == WSAEWOULDBLOCK || code == WSAEINPROGRESS || code == WSAEALREADY) {
            uint32_t blocking;
            EnterCriticalSection(&object->lock);
            object->connecting = 1;
            blocking = (object->flags & HL_HOST_SOCKET_NONBLOCK) == 0;
            LeaveCriticalSection(&object->lock);
            if (blocking && !object->closing) {
                int32_t latched;
                (void)WaitForSingleObject(object->ready, HL_WINDOWS_SOCKET_WAIT_SLICE_MS);
                hl_windows_socket_drain(ws, object);
                EnterCriticalSection(&object->lock);
                latched = object->pending_error;
                object->pending_error = 0;
                LeaveCriticalSection(&object->lock);
                if (latched != 0) {
                    result = hl_windows_socket_error_result(latched);
                    break;
                }
                continue;
            }
            /* A second connect while the first is still outstanding is a
             * different answer from the first one, and a guest acts on the
             * difference: EINPROGRESS means "wait for writability", EALREADY
             * means "you already asked". */
            result = hl_windows_socket_condition(code == WSAEALREADY ? HL_STATUS_BUSY : HL_STATUS_WOULD_BLOCK,
                                                 code == WSAEALREADY ? HL_HOST_NETWORK_CONDITION_CONNECT_PENDING
                                                                     : HL_HOST_NETWORK_CONDITION_CONNECT_IN_PROGRESS);
            break;
        }
        result = hl_windows_socket_error_result(code);
        break;
    }
    hl_windows_socket_object_release(object);
    return result;
}

static hl_host_result hl_windows_network_shutdown(void *context, hl_host_handle handle, uint32_t direction) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *object;
    hl_host_result result;
    int native;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    /* SD_RECEIVE/SD_SEND/SD_BOTH happen to be 0/1/2 like Linux's SHUT_*, which
     * is precisely why the contract's own numbering is 1/2/3: an accidental
     * agreement is not a translation, and the next host need not share it. */
    switch (direction) {
    case HL_HOST_SHUTDOWN_READ: native = SD_RECEIVE; break;
    case HL_HOST_SHUTDOWN_WRITE: native = SD_SEND; break;
    case HL_HOST_SHUTDOWN_BOTH: native = SD_BOTH; break;
    default: return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = ws->shutdown(object->socket, native) == 0 ? hl_windows_result(HL_STATUS_OK, 0, 0)
                                                       : hl_windows_socket_last_error(ws);
    /* A shutdown makes the socket permanently ready in that direction; release
     * anything waiting so a blocked receive on our own end returns end of
     * stream rather than sitting out the slice. */
    SetEvent(object->ready);
    hl_windows_socket_object_release(object);
    return result;
}

static hl_host_result hl_windows_network_name(void *context, hl_host_handle handle, hl_host_network_address *address,
                                              int peer) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *object;
    struct sockaddr_storage storage;
    hl_host_result result;
    int size = (int)sizeof(storage);
    int failed;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    if (address == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&storage, 0, sizeof(storage));
    failed = peer ? ws->get_peer_name(object->socket, (struct sockaddr *)&storage, &size) != 0
                  : ws->get_sock_name(object->socket, (struct sockaddr *)&storage, &size) != 0;
    if (failed && !peer && ws->last_error() == WSAEINVAL) {
        /* getsockname on a socket that has never been bound is WSAEINVAL here
         * and a success reporting the wildcard everywhere else. The wildcard is
         * the truthful answer -- an unbound socket really is bound to no address
         * and no port -- and the refusal is the host being unable to say so, so
         * it is said here. */
        memset(address, 0, sizeof(*address));
        address->family = object->family;
        address->size = object->family == HL_HOST_NETWORK_IPV4 ? 4 : object->family == HL_HOST_NETWORK_IPV6 ? 16 : 0;
        result = hl_windows_result(HL_STATUS_OK, 0, 0);
    } else if (failed) {
        result = hl_windows_socket_last_error(ws);
    } else if (hl_windows_socket_decode(&storage, size, address) != HL_STATUS_OK) {
        result = hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    } else {
        /* Windows answers a bound AF_UNIX getsockname with a zeroed path often
         * enough that the bound name is remembered on the object instead. The
         * remembered value is the one the caller asked for, so it is not an
         * approximation of the host's answer -- it is a better one. */
        if (!peer && address->family == HL_HOST_NETWORK_LOCAL && address->size == 0) {
            EnterCriticalSection(&object->lock);
            if (object->local_path_valid) {
                memcpy(address->local_path, object->local_path, object->local_path_size);
                address->size = object->local_path_size;
            }
            LeaveCriticalSection(&object->lock);
        }
        result = hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    hl_windows_socket_object_release(object);
    return result;
}

static hl_host_result hl_windows_network_local_address(void *context, hl_host_handle handle,
                                                       hl_host_network_address *address) {
    return hl_windows_network_name(context, handle, address, 0);
}

static hl_host_result hl_windows_network_peer_address(void *context, hl_host_handle handle,
                                                      hl_host_network_address *address) {
    return hl_windows_network_name(context, handle, address, 1);
}

/* --- transfers -------------------------------------------------------------- */

/*
 * Contract flags -> Winsock flags, per direction. The two words are disjoint,
 * not merely renumbered: MSG_DONTWAIT has no Winsock spelling at all (it is the
 * object's status flag here), MSG_NOSIGNAL has no meaning on a host that raises
 * no signal for it, and MSG_WAITALL is 0x100 on Linux against 0x8 here. Passing
 * a guest's word through was measured returning WSAEOPNOTSUPP for the plainest
 * case, so nothing is passed through.
 */
static DWORD hl_windows_socket_send_flags(uint32_t flags) {
    DWORD native = 0;
    if ((flags & HL_HOST_MSG_OUT_OF_BAND) != 0) native |= MSG_OOB;
    if ((flags & HL_HOST_MSG_DONT_ROUTE) != 0) native |= MSG_DONTROUTE;
    return native;
}

static DWORD hl_windows_socket_receive_flags(uint32_t flags) {
    DWORD native = 0;
    if ((flags & HL_HOST_MSG_PEEK) != 0) native |= MSG_PEEK;
    if ((flags & HL_HOST_MSG_OUT_OF_BAND) != 0) native |= MSG_OOB;
    if ((flags & HL_HOST_MSG_WAIT_ALL) != 0) native |= MSG_WAITALL;
    return native;
}

/* Whether this call is allowed to block, given the object's flag and the
 * per-call override. MSG_DONTWAIT is per-call and never changes the object. */
static int hl_windows_socket_call_blocks(hl_windows_socket_object *object, uint32_t flags) {
    uint32_t blocking;
    if ((flags & HL_HOST_MSG_DONT_WAIT) != 0) return 0;
    EnterCriticalSection(&object->lock);
    blocking = (object->flags & HL_HOST_SOCKET_NONBLOCK) == 0;
    LeaveCriticalSection(&object->lock);
    return blocking != 0;
}

static hl_host_result hl_windows_network_transfer(void *context, hl_host_handle handle, WSABUF *buffers,
                                                  DWORD buffer_count, uint32_t flags, int sending,
                                                  const struct sockaddr *destination, int destination_size,
                                                  struct sockaddr_storage *source, int *source_size) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *object;
    hl_host_result result;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (;;) {
        DWORD transferred = 0;
        DWORD native = sending ? hl_windows_socket_send_flags(flags) : hl_windows_socket_receive_flags(flags);
        int outcome;
        int code;
        if (sending)
            outcome = destination != NULL
                          ? ws->send_to(object->socket, buffers, buffer_count, &transferred, native, destination,
                                        destination_size, NULL, NULL)
                          : ws->send(object->socket, buffers, buffer_count, &transferred, native, NULL, NULL);
        else if (source != NULL)
            outcome = ws->receive_from(object->socket, buffers, buffer_count, &transferred, &native,
                                       (struct sockaddr *)source, source_size, NULL, NULL);
        else
            outcome = ws->receive(object->socket, buffers, buffer_count, &transferred, &native, NULL, NULL);
        if (outcome == 0) {
            /* The receive flags word is in/out: what comes back names truncation
             * and out-of-band delivery, which the caller has no other way to
             * learn. It rides in `detail` so `value` stays the byte count. */
            result = (hl_host_result){(int32_t)HL_STATUS_OK, HL_HOST_DETAIL_NONE, (uint64_t)transferred, 0};
            if (!sending) {
                uint64_t out = 0;
                if ((native & MSG_PARTIAL) != 0) out |= HL_HOST_MSG_TRUNCATED;
                if ((native & MSG_OOB) != 0) out |= HL_HOST_MSG_RECEIVED_OUT_OF_BAND;
                result.detail = out;
            }
            break;
        }
        code = ws->last_error();
        /* A datagram larger than the buffer is a successful, truncated receive on
         * Linux and an error here. The bytes ARE in the caller's buffer; only the
         * report differs, so it is reported the way the contract defines it. */
        /* A datagram send with no destination and no prior connect is
         * "you never said where" everywhere else and WSAENOTCONN here. The
         * distinction matters: a guest reads ENOTCONN as "the connection went
         * away" and closes, where the correct answer tells it to supply an
         * address. */
        if (sending && code == WSAENOTCONN && destination == NULL && object->type != HL_HOST_NETWORK_STREAM) {
            result =
                hl_windows_socket_condition(HL_STATUS_INVALID_ARGUMENT, HL_HOST_NETWORK_CONDITION_DESTINATION_REQUIRED);
            break;
        }
        /* A receive on an end we ourselves shut down for reading is end of
         * stream, not an error: there is nothing more to come and the caller
         * asked for that. Windows reports it as a failure; everywhere else it
         * is a zero-length read, and a guest that gets the failure instead
         * treats a completed conversation as a broken one. */
        if (!sending && code == WSAESHUTDOWN) {
            result = hl_windows_socket_ok(0);
            break;
        }
        if (!sending && code == WSAEMSGSIZE) {
            result = (hl_host_result){(int32_t)HL_STATUS_OK, HL_HOST_DETAIL_NONE, (uint64_t)transferred,
                                      HL_HOST_MSG_TRUNCATED};
            break;
        }
        if (code == WSAEWOULDBLOCK && hl_windows_socket_call_blocks(object, flags) &&
            hl_windows_socket_block(ws, object))
            continue;
        if (object->closing) {
            result = hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
            break;
        }
        result = hl_windows_socket_error_result(code);
        break;
    }
    hl_windows_socket_object_release(object);
    return result;
}

static hl_host_result hl_windows_network_send(void *context, hl_host_handle handle, hl_host_const_bytes data,
                                              uint32_t flags) {
    WSABUF buffer;
    if (data.size != 0 && data.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (data.size > (size_t)ULONG_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    buffer.buf = (CHAR *)(void *)(uintptr_t)data.data;
    buffer.len = (ULONG)data.size;
    return hl_windows_network_transfer(context, handle, &buffer, 1, flags, 1, NULL, 0, NULL, NULL);
}

static hl_host_result hl_windows_network_receive(void *context, hl_host_handle handle, hl_host_bytes data,
                                                 uint32_t flags) {
    WSABUF buffer;
    if (data.size != 0 && data.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (data.size > (size_t)ULONG_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    buffer.buf = (CHAR *)data.data;
    buffer.len = (ULONG)data.size;
    return hl_windows_network_transfer(context, handle, &buffer, 1, flags, 0, NULL, 0, NULL, NULL);
}

enum { HL_WINDOWS_SOCKET_IOV_MAX = 64 };

static hl_status hl_windows_socket_gather(const hl_host_network_message *message, WSABUF *buffers, DWORD *count) {
    uint32_t index;
    if (message == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (message->buffer_count > HL_WINDOWS_SOCKET_IOV_MAX) return HL_STATUS_INVALID_ARGUMENT;
    if (message->buffer_count != 0 && message->buffers == NULL) return HL_STATUS_INVALID_ARGUMENT;
    for (index = 0; index < message->buffer_count; ++index) {
        if (message->buffers[index].size > (uint64_t)ULONG_MAX) return HL_STATUS_INVALID_ARGUMENT;
        buffers[index].buf = (CHAR *)(uintptr_t)message->buffers[index].address;
        buffers[index].len = (ULONG)message->buffers[index].size;
    }
    *count = message->buffer_count;
    return HL_STATUS_OK;
}

static hl_host_result hl_windows_network_send_message(void *context, hl_host_handle handle,
                                                      const hl_host_network_message *message, uint32_t flags) {
    WSABUF buffers[HL_WINDOWS_SOCKET_IOV_MAX];
    struct sockaddr_storage storage;
    DWORD count = 0;
    int size = 0;
    hl_status status = hl_windows_socket_gather(message, buffers, &count);
    if (status != HL_STATUS_OK) return hl_windows_result(status, 0, 0);
    /* Ancillary data has no Winsock carrier at all, and inventing one that
     * silently dropped it would make a descriptor-passing guest look like it
     * had succeeded. Refused, with the message unsent. */
    if (message->control != NULL && message->control_size != 0)
        return hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_OPERATION_NOT_SUPPORTED);
    if (message->address != NULL) {
        status = hl_windows_socket_encode(message->address, &storage, &size);
        if (status != HL_STATUS_OK) return hl_windows_result(status, 0, 0);
        return hl_windows_network_transfer(context, handle, buffers, count, flags, 1, (const struct sockaddr *)&storage,
                                           size, NULL, NULL);
    }
    return hl_windows_network_transfer(context, handle, buffers, count, flags, 1, NULL, 0, NULL, NULL);
}

static hl_host_result hl_windows_network_receive_message(void *context, hl_host_handle handle,
                                                         hl_host_network_message *message, uint32_t flags) {
    WSABUF buffers[HL_WINDOWS_SOCKET_IOV_MAX];
    struct sockaddr_storage storage;
    DWORD count = 0;
    int size = (int)sizeof(storage);
    hl_host_result result;
    hl_status status = hl_windows_socket_gather(message, buffers, &count);
    if (status != HL_STATUS_OK) return hl_windows_result(status, 0, 0);
    memset(&storage, 0, sizeof(storage));
    result = message->address != NULL
                 ? hl_windows_network_transfer(context, handle, buffers, count, flags, 0, NULL, 0, &storage, &size)
                 : hl_windows_network_transfer(context, handle, buffers, count, flags, 0, NULL, 0, NULL, NULL);
    message->flags = (uint32_t)result.detail;
    message->control_size = 0;
    if (result.status == HL_STATUS_OK && message->address != NULL &&
        hl_windows_socket_decode(&storage, size, message->address) != HL_STATUS_OK)
        memset(message->address, 0, sizeof(*message->address));
    result.detail = 0;
    result.detail_domain = HL_HOST_DETAIL_NONE;
    return result;
}

/* --- readiness -------------------------------------------------------------- */

/*
 * Readiness is re-derived rather than remembered, which is the wakeup-bus model
 * this contract is built on: a wake names nothing, and every caller asks again.
 * select answers readable and writable exactly; what it cannot say is whether a
 * readable stream socket has bytes or an end of stream, and a caller that
 * cannot tell those apart spins on a closed connection forever. So a readable
 * non-listening stream socket is peeked at with one byte -- a peek consumes
 * nothing, and a zero-length peek is end of stream by definition.
 */
static hl_host_result hl_windows_network_readiness(void *context, hl_host_handle handle, uint32_t interests) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    hl_windows_socket_object *object;
    fd_set readable;
    fd_set writable;
    fd_set failing;
    struct timeval immediately;
    uint32_t ready = 0;
    uint64_t pending = 0;
    int32_t latched;
    uint32_t listening;
    uint32_t type;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_socket_drain(ws, object);
    EnterCriticalSection(&object->lock);
    latched = object->pending_error;
    listening = object->listening;
    type = object->type;
    LeaveCriticalSection(&object->lock);

    readable.fd_count = 1;
    readable.fd_array[0] = object->socket;
    writable.fd_count = 1;
    writable.fd_array[0] = object->socket;
    failing.fd_count = 1;
    failing.fd_array[0] = object->socket;
    immediately.tv_sec = 0;
    immediately.tv_usec = 0;
    if (ws->select(0, &readable, &writable, &failing, &immediately) == SOCKET_ERROR) {
        hl_host_result failure = hl_windows_socket_last_error(ws);
        hl_windows_socket_object_release(object);
        return failure;
    }
    if (readable.fd_count != 0) ready |= HL_HOST_READY_READ;
    /* FIONREAD is the one ioctl on this host that is a straight answer rather
     * than a mode change, and it is asked as a question here so that no caller
     * has to know the name. On a datagram socket Winsock reports the size of the
     * FIRST queued datagram, which is what a guest reading it means. */
    {
        u_long queued = 0;
        if (ws->ioctl_socket(object->socket, (long)FIONREAD, &queued) == 0) pending = (uint64_t)queued;
    }
    if (writable.fd_count != 0) ready |= HL_HOST_READY_WRITE;
    if (failing.fd_count != 0) ready |= HL_HOST_READY_ERROR;
    if (latched != 0) ready |= HL_HOST_READY_ERROR;
    if ((ready & HL_HOST_READY_READ) != 0 && !listening && type == HL_HOST_NETWORK_STREAM) {
        WSABUF probe;
        char byte = 0;
        DWORD produced = 0;
        DWORD peek = MSG_PEEK;
        probe.buf = &byte;
        probe.len = 1;
        if (ws->receive(object->socket, &probe, 1, &produced, &peek, NULL, NULL) == 0) {
            if (produced == 0) ready |= HL_HOST_READY_HANGUP;
        } else {
            const int code = ws->last_error();
            if (code == WSAECONNRESET || code == WSAECONNABORTED || code == WSAENETRESET || code == WSAESHUTDOWN)
                ready |= HL_HOST_READY_HANGUP | HL_HOST_READY_ERROR;
            else if (code == WSAEWOULDBLOCK)
                ready &= ~(uint32_t)HL_HOST_READY_READ;
        }
    }
    hl_windows_socket_object_release(object);
    return (hl_host_result){(int32_t)HL_STATUS_OK, HL_HOST_DETAIL_NONE, interests == 0 ? ready : (ready & interests),
                            pending};
}

/*
 * Sockets are waitable here -- every one carries a WSAEventSelect event -- but
 * the event group does not yet accept them, so answering "yes" would send a
 * caller into a pollset that would register the socket without a wake and never
 * report it. The honest answer is the one that makes the caller poll.
 */
static hl_host_result hl_windows_network_wait_handle(void *context, hl_host_handle handle) {
    hl_host_windows *host = context;
    hl_windows_socket_object *object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_socket_object_release(object);
    return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

static hl_host_result hl_windows_network_set_status_flags(void *context, hl_host_handle handle, uint32_t flags) {
    hl_host_windows *host = context;
    hl_windows_socket_object *object;
    if ((flags & ~(uint32_t)HL_HOST_SOCKET_NONBLOCK) != 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* The socket itself stays non-blocking regardless: WSAEventSelect put it
     * there and taking it back would cost the waitable event. Only the flag
     * moves, and the retry loops read it. */
    EnterCriticalSection(&object->lock);
    object->flags = flags;
    LeaveCriticalSection(&object->lock);
    hl_windows_socket_object_release(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* --- options ---------------------------------------------------------------- */

/*
 * Every option is named once here, with the Winsock level and name it maps to
 * and the width the contract gives it. A table rather than a switch because the
 * defect this design exists to prevent -- a missing `break` letting one level's
 * option number be applied at another level -- is a switch defect, and a table
 * has no fallthrough to forget.
 */
typedef struct hl_windows_socket_option {
    uint32_t option;
    int level;
    int name;
    uint8_t readable;
    uint8_t writable;
} hl_windows_socket_option;

static const hl_windows_socket_option hl_windows_socket_options[] = {
    /*
     * REUSE_PORT is Windows' SO_REUSEADDR, and that is not the contradiction it
     * looks like -- it is the flat enum doing its job. The contract defines
     * REUSE_PORT as "several sockets may hold this address at once", which is
     * exactly what Windows' option of the SO_REUSEADDR name provides and what
     * its own default does not. The contract's REUSE_ADDRESS means something
     * else entirely -- "rebind an address in a post-close wait state" -- which
     * Windows already does by default and which is handled separately above.
     * Two contract options, one host option, and a name-for-name mapping would
     * have got both of them wrong in opposite directions.
     */
    {HL_HOST_SOCKOPT_REUSE_PORT, SOL_SOCKET, SO_REUSEADDR, 1, 1},
    {HL_HOST_SOCKOPT_KEEP_ALIVE, SOL_SOCKET, SO_KEEPALIVE, 1, 1},
    {HL_HOST_SOCKOPT_BROADCAST, SOL_SOCKET, SO_BROADCAST, 1, 1},
    {HL_HOST_SOCKOPT_DONT_ROUTE, SOL_SOCKET, SO_DONTROUTE, 1, 1},
    {HL_HOST_SOCKOPT_OUT_OF_BAND_INLINE, SOL_SOCKET, SO_OOBINLINE, 1, 1},
    {HL_HOST_SOCKOPT_SEND_BUFFER, SOL_SOCKET, SO_SNDBUF, 1, 1},
    {HL_HOST_SOCKOPT_RECEIVE_BUFFER, SOL_SOCKET, SO_RCVBUF, 1, 1},
    {HL_HOST_SOCKOPT_ACCEPT_CONNECTIONS, SOL_SOCKET, SO_ACCEPTCONN, 1, 0},
    {HL_HOST_SOCKOPT_TCP_NO_DELAY, IPPROTO_TCP, TCP_NODELAY, 1, 1},
    {HL_HOST_SOCKOPT_IP_TIME_TO_LIVE, IPPROTO_IP, IP_TTL, 1, 1},
    {HL_HOST_SOCKOPT_IP_TYPE_OF_SERVICE, IPPROTO_IP, IP_TOS, 1, 1},
    {HL_HOST_SOCKOPT_IP_HEADER_INCLUDED, IPPROTO_IP, IP_HDRINCL, 1, 1},
    {HL_HOST_SOCKOPT_IP_MULTICAST_TTL, IPPROTO_IP, IP_MULTICAST_TTL, 1, 1},
    {HL_HOST_SOCKOPT_IP_MULTICAST_LOOP, IPPROTO_IP, IP_MULTICAST_LOOP, 1, 1},
    {HL_HOST_SOCKOPT_IPV6_ONLY, IPPROTO_IPV6, IPV6_V6ONLY, 1, 1},
    {HL_HOST_SOCKOPT_IPV6_UNICAST_HOPS, IPPROTO_IPV6, IPV6_UNICAST_HOPS, 1, 1},
    {HL_HOST_SOCKOPT_IPV6_MULTICAST_HOPS, IPPROTO_IPV6, IPV6_MULTICAST_HOPS, 1, 1},
    {HL_HOST_SOCKOPT_IPV6_MULTICAST_LOOP, IPPROTO_IPV6, IPV6_MULTICAST_LOOP, 1, 1}};

static const hl_windows_socket_option *hl_windows_socket_option_find(uint32_t option) {
    size_t index;
    for (index = 0; index < HL_ARRAY_COUNT(hl_windows_socket_options); ++index)
        if (hl_windows_socket_options[index].option == option) return &hl_windows_socket_options[index];
    return NULL;
}

static hl_host_result hl_windows_socket_scalar_out(hl_host_bytes value, uint32_t scalar) {
    if (value.data == NULL || value.size < sizeof(uint32_t)) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(value.data, &scalar, sizeof(scalar));
    return hl_windows_socket_ok(sizeof(scalar));
}

static hl_host_result hl_windows_network_get_option(void *context, hl_host_handle handle, uint32_t option,
                                                    hl_host_bytes value) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    const hl_windows_socket_option *entry;
    hl_windows_socket_object *object;
    hl_host_result result;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    switch (option) {
    case HL_HOST_SOCKOPT_TYPE: result = hl_windows_socket_scalar_out(value, object->type); goto done;
    case HL_HOST_SOCKOPT_DOMAIN: result = hl_windows_socket_scalar_out(value, object->family); goto done;
    case HL_HOST_SOCKOPT_PROTOCOL: result = hl_windows_socket_scalar_out(value, object->protocol); goto done;
    case HL_HOST_SOCKOPT_REUSE_ADDRESS: {
        /* What is reported is the CONTRACT's option, which Windows applies by
         * default and only SO_EXCLUSIVEADDRUSE turns off. Reporting Windows'
         * own SO_REUSEADDR here would answer a different question. */
        int exclusive = 0;
        int size = (int)sizeof(exclusive);
        if (ws->get_sock_opt(object->socket, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, (char *)&exclusive, &size) != 0) {
            result = hl_windows_socket_last_error(ws);
            goto done;
        }
        result = hl_windows_socket_scalar_out(value, exclusive != 0 ? 0u : 1u);
        goto done;
    }
    case HL_HOST_SOCKOPT_ERROR: {
        /* Read AND CLEAR, which is what a guest expects and what Windows does
         * not do: its SO_ERROR is sticky. The latched FD_CONNECT code is
         * preferred over the socket's, because it is the one that names the
         * failure of a non-blocking connect. */
        int32_t latched;
        int code = 0;
        int size = (int)sizeof(code);
        hl_windows_socket_drain(ws, object);
        EnterCriticalSection(&object->lock);
        latched = object->pending_error;
        object->pending_error = 0;
        LeaveCriticalSection(&object->lock);
        if (latched == 0 && ws->get_sock_opt(object->socket, SOL_SOCKET, SO_ERROR, (char *)&code, &size) == 0)
            latched = code;
        result = hl_windows_socket_scalar_out(value, latched == 0 ? (uint32_t)HL_STATUS_OK
                                                                  : (uint32_t)hl_windows_socket_error_status(latched));
        goto done;
    }
    case HL_HOST_SOCKOPT_LINGER: {
        struct linger native;
        hl_host_network_linger out;
        int size = (int)sizeof(native);
        memset(&native, 0, sizeof(native));
        if (value.data == NULL || value.size < sizeof(out)) {
            result = hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
            goto done;
        }
        if (ws->get_sock_opt(object->socket, SOL_SOCKET, SO_LINGER, (char *)&native, &size) != 0) {
            result = hl_windows_socket_last_error(ws);
            goto done;
        }
        out.enabled = native.l_onoff != 0 ? 1u : 0u;
        out.seconds = native.l_linger;
        memcpy(value.data, &out, sizeof(out));
        result = hl_windows_socket_ok(sizeof(out));
        goto done;
    }
    case HL_HOST_SOCKOPT_SEND_TIMEOUT:
    case HL_HOST_SOCKOPT_RECEIVE_TIMEOUT: {
        DWORD milliseconds = 0;
        uint64_t nanoseconds;
        int size = (int)sizeof(milliseconds);
        const int name = option == HL_HOST_SOCKOPT_SEND_TIMEOUT ? SO_SNDTIMEO : SO_RCVTIMEO;
        if (value.data == NULL || value.size < sizeof(uint64_t)) {
            result = hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
            goto done;
        }
        if (ws->get_sock_opt(object->socket, SOL_SOCKET, name, (char *)&milliseconds, &size) != 0) {
            result = hl_windows_socket_last_error(ws);
            goto done;
        }
        nanoseconds = (uint64_t)milliseconds * UINT64_C(1000000);
        memcpy(value.data, &nanoseconds, sizeof(nanoseconds));
        result = hl_windows_socket_ok(sizeof(nanoseconds));
        goto done;
    }
    default: break;
    }
    entry = hl_windows_socket_option_find(option);
    if (entry == NULL || !entry->readable) {
        result = hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_OPTION_NOT_SUPPORTED);
    } else {
        int scalar = 0;
        int size = (int)sizeof(scalar);
        if (ws->get_sock_opt(object->socket, entry->level, entry->name, (char *)&scalar, &size) != 0)
            result = hl_windows_socket_last_error(ws);
        else
            result = hl_windows_socket_scalar_out(value, (uint32_t)scalar);
    }
done:
    hl_windows_socket_object_release(object);
    return result;
}

static hl_host_result hl_windows_network_set_option(void *context, hl_host_handle handle, uint32_t option,
                                                    const hl_host_const_bytes value) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    const hl_windows_socket_option *entry;
    hl_windows_socket_object *object;
    hl_host_result result;
    uint32_t scalar = 0;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    object = hl_windows_socket_acquire(host, handle);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    switch (option) {
    case HL_HOST_SOCKOPT_REUSE_ADDRESS: {
        /* The whole security argument of this file, in six lines. 1 asks for
         * behaviour Windows already has, so it is a no-op; 0 asks for the
         * behaviour Windows only has with SO_EXCLUSIVEADDRUSE. Setting Windows'
         * SO_REUSEADDR for 1 -- which is what the name suggests and what the
         * prior art does -- was measured binding over a LIVE listener and
         * taking its connections. */
        int exclusive;
        if (value.data == NULL || value.size < sizeof(uint32_t)) {
            result = hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
            goto done;
        }
        memcpy(&scalar, value.data, sizeof(scalar));
        exclusive = scalar != 0 ? 0 : 1;
        if (exclusive == 0) {
            result = hl_windows_result(HL_STATUS_OK, 0, 0);
            goto done;
        }
        result = ws->set_sock_opt(object->socket, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, (const char *)&exclusive,
                                  (int)sizeof(exclusive)) == 0
                     ? hl_windows_result(HL_STATUS_OK, 0, 0)
                     : hl_windows_socket_last_error(ws);
        goto done;
    }
    case HL_HOST_SOCKOPT_LINGER: {
        hl_host_network_linger in;
        struct linger native;
        if (value.data == NULL || value.size < sizeof(in)) {
            result = hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
            goto done;
        }
        memcpy(&in, value.data, sizeof(in));
        if (in.seconds > 0xffffu) {
            result = hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
            goto done;
        }
        native.l_onoff = (u_short)(in.enabled != 0 ? 1 : 0);
        native.l_linger = (u_short)in.seconds;
        result =
            ws->set_sock_opt(object->socket, SOL_SOCKET, SO_LINGER, (const char *)&native, (int)sizeof(native)) == 0
                ? hl_windows_result(HL_STATUS_OK, 0, 0)
                : hl_windows_socket_last_error(ws);
        goto done;
    }
    case HL_HOST_SOCKOPT_SEND_TIMEOUT:
    case HL_HOST_SOCKOPT_RECEIVE_TIMEOUT: {
        uint64_t nanoseconds;
        DWORD milliseconds;
        const int name = option == HL_HOST_SOCKOPT_SEND_TIMEOUT ? SO_SNDTIMEO : SO_RCVTIMEO;
        if (value.data == NULL || value.size < sizeof(nanoseconds)) {
            result = hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
            goto done;
        }
        memcpy(&nanoseconds, value.data, sizeof(nanoseconds));
        /* Rounded UP, never down: a caller that asked for a bound of half a
         * millisecond meant "do not wait forever", and a zero here would mean
         * exactly that. */
        milliseconds = (DWORD)((nanoseconds + UINT64_C(999999)) / UINT64_C(1000000));
        if (nanoseconds == 0) milliseconds = 0;
        result = ws->set_sock_opt(object->socket, SOL_SOCKET, name, (const char *)&milliseconds,
                                  (int)sizeof(milliseconds)) == 0
                     ? hl_windows_result(HL_STATUS_OK, 0, 0)
                     : hl_windows_socket_last_error(ws);
        goto done;
    }
    case HL_HOST_SOCKOPT_ERROR:
    case HL_HOST_SOCKOPT_TYPE:
    case HL_HOST_SOCKOPT_DOMAIN:
    case HL_HOST_SOCKOPT_PROTOCOL:
    case HL_HOST_SOCKOPT_ACCEPT_CONNECTIONS:
        /* Read-only by definition. Refused as an option this socket does not
         * accept rather than as a bad argument, because that is the distinction
         * a caller acts on: one says "ask differently", the other says "this
         * option is not settable here". */
        result = hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_OPTION_NOT_SUPPORTED);
        goto done;
    default: break;
    }
    entry = hl_windows_socket_option_find(option);
    if (entry == NULL || !entry->writable) {
        result = hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_OPTION_NOT_SUPPORTED);
    } else if (value.data == NULL || value.size < sizeof(uint32_t)) {
        result = hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    } else {
        int native;
        memcpy(&scalar, value.data, sizeof(scalar));
        native = (int)scalar;
        result =
            ws->set_sock_opt(object->socket, entry->level, entry->name, (const char *)&native, (int)sizeof(native)) == 0
                ? hl_windows_result(HL_STATUS_OK, 0, 0)
                : hl_windows_socket_last_error(ws);
    }
done:
    hl_windows_socket_object_release(object);
    return result;
}

/* --- pairs ------------------------------------------------------------------ */

/*
 * A private connected pair, built from a listener on a unique name under the
 * process's temporary directory and torn down to nothing before it returns.
 *
 * Windows has no socketpair, and the window in which the rendezvous name is
 * visible is the one thing that has to be argued for. It is bounded by this
 * function: the name is unique per process and per call, the listener has a
 * backlog of one, and the accepted end is checked against the connected end's
 * own address so a third party that raced in on the name cannot be handed back
 * as the peer.
 *
 * Datagram and seqpacket pairs are refused rather than approximated. A local
 * stream with a length prefix would reproduce their boundaries exactly and no
 * outside party could ever see the framing -- that is why `pair` is its own
 * operation -- but the framing has to be applied on every transfer path, and a
 * half-applied one loses bytes rather than messages.
 */
static hl_host_result hl_windows_network_pair(void *context, uint32_t family, uint32_t type, uint32_t protocol,
                                              hl_host_handle ends[2]) {
    hl_host_windows *host = context;
    const hl_windows_winsock *ws = hl_windows_winsock_get();
    static volatile LONG sequence;
    struct sockaddr_un name;
    hl_windows_socket_object *first = NULL;
    hl_windows_socket_object *second = NULL;
    hl_host_result result;
    hl_host_result published;
    SOCKET listener = INVALID_SOCKET;
    SOCKET client = INVALID_SOCKET;
    SOCKET server = INVALID_SOCKET;
    WCHAR directory[MAX_PATH + 1];
    DWORD directory_length;
    char path[sizeof(name.sun_path)];
    size_t offset = 0;
    unsigned long long unique;
    unsigned long long scale;
    int size;
    if (ws == NULL) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    if (ends == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (family != HL_HOST_NETWORK_LOCAL)
        return hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_FAMILY_NOT_SUPPORTED);
    if (type != HL_HOST_NETWORK_STREAM)
        return hl_windows_socket_condition(HL_STATUS_NOT_SUPPORTED, HL_HOST_NETWORK_CONDITION_TYPE_NOT_SUPPORTED);

    directory_length = GetTempPathW(MAX_PATH, directory);
    if (directory_length == 0 || directory_length > MAX_PATH) return hl_windows_last_error_result();
    {
        DWORD index;
        for (index = 0; index < directory_length && offset + 1 < sizeof(path); ++index) {
            const WCHAR character = directory[index];
            if (character > 0x7f) return hl_windows_result(HL_STATUS_NAME_TOO_LONG, 0, 0);
            path[offset++] = (char)character;
        }
    }
    if (offset == 0 || offset + 40 >= sizeof(path)) return hl_windows_result(HL_STATUS_NAME_TOO_LONG, 0, 0);
    if (path[offset - 1] != '\\' && path[offset - 1] != '/') path[offset++] = '\\';
    {
        static const char prefix[] = "hl-pair-";
        size_t index;
        for (index = 0; index + 1 < sizeof(prefix); ++index)
            path[offset++] = prefix[index];
    }
    unique = ((unsigned long long)GetCurrentProcessId() << 32) ^
             ((unsigned long long)(ULONG)InterlockedIncrement(&sequence) << 8) ^ (unsigned long long)GetTickCount64();
    for (scale = 0; scale < 16; ++scale) {
        static const char digits[] = "0123456789abcdef";
        path[offset++] = digits[(unique >> ((15u - (unsigned)scale) * 4u)) & 0xfu];
    }
    path[offset] = '\0';

    memset(&name, 0, sizeof(name));
    name.sun_family = AF_UNIX;
    memcpy(name.sun_path, path, offset + 1);
    size = (int)(offsetof(struct sockaddr_un, sun_path) + offset + 1u);

    listener = ws->socket(AF_UNIX, SOCK_STREAM, (int)protocol);
    client = ws->socket(AF_UNIX, SOCK_STREAM, (int)protocol);
    if (listener == INVALID_SOCKET || client == INVALID_SOCKET) {
        result = hl_windows_socket_last_error(ws);
        goto fail;
    }
    /* Nothing here is event-selected yet, so all three sockets are blocking and
     * the three-step rendezvous needs no retry loop. The objects that survive
     * are event-selected as they are constructed, below. */
    if (ws->bind(listener, (const struct sockaddr *)&name, size) != 0 || ws->listen(listener, 1) != 0 ||
        ws->connect(client, (const struct sockaddr *)&name, size) != 0) {
        result = hl_windows_socket_last_error(ws);
        goto fail;
    }
    server = ws->accept(listener, NULL, NULL);
    if (server == INVALID_SOCKET) {
        result = hl_windows_socket_last_error(ws);
        goto fail;
    }
    (void)ws->close_socket(listener);
    listener = INVALID_SOCKET;
    (void)DeleteFileA(path);

    first = hl_windows_socket_object_create(ws, client, family, type, protocol, 0);
    if (first != NULL) client = INVALID_SOCKET;
    second = hl_windows_socket_object_create(ws, server, family, type, protocol, 0);
    if (second != NULL) server = INVALID_SOCKET;
    if (first == NULL || second == NULL) {
        result = hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        goto fail;
    }
    published = hl_windows_socket_publish(host, first);
    if (published.status != HL_STATUS_OK) {
        result = published;
        goto fail;
    }
    ends[0] = published.value;
    first = NULL;
    published = hl_windows_socket_publish(host, second);
    if (published.status != HL_STATUS_OK) {
        (void)hl_windows_network_close(host, ends[0]);
        result = published;
        goto fail;
    }
    ends[1] = published.value;
    second = NULL;
    return hl_windows_socket_ok(0);

fail:
    if (listener != INVALID_SOCKET) {
        (void)ws->close_socket(listener);
        (void)DeleteFileA(path);
    }
    if (client != INVALID_SOCKET) (void)ws->close_socket(client);
    if (server != INVALID_SOCKET) (void)ws->close_socket(server);
    if (first != NULL) hl_windows_socket_object_release(first);
    if (second != NULL) hl_windows_socket_object_release(second);
    return result;
}

const hl_host_network_services hl_windows_network_services = {.abi = HL_HOST_NETWORK_ABI,
                                                              .size = sizeof(hl_host_network_services),
                                                              .socket = hl_windows_network_socket,
                                                              .bind = hl_windows_network_bind,
                                                              .connect = hl_windows_network_connect,
                                                              .send = hl_windows_network_send,
                                                              .receive = hl_windows_network_receive,
                                                              .close = hl_windows_network_close,
                                                              .listen = hl_windows_network_listen,
                                                              .accept = hl_windows_network_accept,
                                                              .pair = hl_windows_network_pair,
                                                              .shutdown = hl_windows_network_shutdown,
                                                              .local_address = hl_windows_network_local_address,
                                                              .peer_address = hl_windows_network_peer_address,
                                                              .get_option = hl_windows_network_get_option,
                                                              .set_option = hl_windows_network_set_option,
                                                              .send_message = hl_windows_network_send_message,
                                                              .receive_message = hl_windows_network_receive_message,
                                                              .readiness = hl_windows_network_readiness,
                                                              .wait_handle = hl_windows_network_wait_handle,
                                                              .set_status_flags = hl_windows_network_set_status_flags,
                                                              .duplicate = hl_windows_network_duplicate};
