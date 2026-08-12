#ifndef HL_LINUX_ABI_HOST_SOCKET_H
#define HL_LINUX_ABI_HOST_SOCKET_H

enum { HL_LINUX_SOCK_NONBLOCK = 0x800, HL_LINUX_SOCK_CLOEXEC = 0x80000 };

/*
 * The BSD sockets vocabulary for this layer: <sys/socket.h>, <netinet/in.h>,
 * <netinet/tcp.h>, <sys/un.h>, <arpa/inet.h> and <netdb.h> behind one door.
 * Same construction and the same REAL/SHAPE/REFUSAL labelling as host_mman.h;
 * see that file's header for why this vocabulary lives in src/linux_abi rather
 * than in src/host/native_compat.h.
 *
 * Six system headers collapse into one file because on Windows they are one
 * decision rather than six.  There is exactly one place the whole vocabulary
 * could come from -- <winsock2.h> and its satellites -- and this arm
 * deliberately does NOT include it.  That is the load-bearing choice here, so
 * it gets the argument it deserves:
 *
 *   - Winsock's constants COLLIDE NUMERICALLY with Linux's.  The socket types
 *     happen to agree and almost nothing else does.  AF_INET6 is 10 on Linux
 *     and 23 on Winsock.  SOL_SOCKET is 1 on Linux and 0xffff on Winsock, and
 *     every SO_* underneath it is a different number: SO_REUSEADDR 2 against
 *     0x0004, SO_TYPE 3 against 0x1008, SO_ERROR 4 against 0x1007, SO_LINGER 13
 *     against 0x0080.  MSG_WAITALL is 0x100 on Linux and 0x8 on Winsock.
 *     SHUT_RD/SHUT_WR/SHUT_RDWR are spelled SD_RECEIVE/SD_SEND/SD_BOTH there
 *     and do not exist under their POSIX names.  Even where the level agrees --
 *     IPPROTO_IPV6 is 41 on both -- the options beneath it do not: IPV6_V6ONLY
 *     is 26 on Linux and 27 on Windows.
 *
 *     That matters because this layer translates a GUEST value into a HOST
 *     value BY IDENTITY in many places.  syscall/net.c's ip_opt_l2m() and
 *     ip6_opt_l2m() return their argument unchanged under __linux__,
 *     container/netns.c's option and message-flag tables collapse to no-ops
 *     when the two sides agree, and dgram_addr_peek() hands SOL_SOCKET/SO_TYPE
 *     straight through.  Every one of those sites is correct today precisely
 *     because the host constant IS the Linux constant.  Winsock's numbers would
 *     leave all of them compiling and quietly asking the host a different
 *     question than the guest asked -- the failure mode with no diagnostic.
 *
 *   - Winsock also brings its own `struct sockaddr` (length-prefixed like the
 *     BSD one), its own `fd_set`, its own `timeval`, and a `SOCKET` that is a
 *     pointer-sized HANDLE rather than an int.  This translation unit's job is
 *     marshalling the GUEST's structures of those same names.  native_compat.h
 *     already declines exactly this collision for the sake of four byte swaps;
 *     the argument only gets stronger for the structs.
 *
 * So every constant below is the LINUX value and every structure below is the
 * LINUX layout -- not as an approximation of Windows, but because these numbers
 * and offsets ARE the guest ABI written down, and the guest ABI is the same on
 * every host.  Nothing here is Windows' opinion about sockets.
 *
 * Three kinds of entry, labelled at each one:
 *
 *   REAL     -- does what its name says, here and now.  The <arpa/inet.h> text
 *               conversions, which are arithmetic over a caller's own buffer,
 *               and -- since the network group reached ABI 2 -- everything that
 *               takes or produces a descriptor.
 *   SHAPE    -- a constant, type or macro with no behaviour of its own.
 *   REFUSAL  -- returns failure with a specific errno and never a quiet
 *               success.  Only <netdb.h> remains, because name resolution is a
 *               different currency and no host group carries it yet.
 *
 * What used to make the descriptor calls refusals was not a missing Windows
 * primitive -- Windows plainly has sockets -- but that a guest fd number named
 * no host object here.  That is what the table below supplies: a socket
 * descriptor is a real UCRT descriptor number, reserved so that it shares the
 * one namespace every other descriptor on this host comes out of, with the
 * opaque hl_host_handle filed beside it.  Everything else in this arm is the
 * translation between the Linux vocabulary above and the host contract's
 * neutral one.
 *

 * htons/ntohs/htonl/ntohl are NOT declared here.  src/host/native_compat.h
 * already supplies them on this arm as inline byte swaps, guarded on the
 * Winsock include markers, for exactly the reason above.
 */

#if !defined(_WIN32)

#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/un.h>
#include <arpa/inet.h>
#include <netdb.h>
#include <fcntl.h>
#include <unistd.h>

/*
 * Two names the syscall layer uses on every host, so that a caller does not
 * have to ask which host it is on to spell an operation.
 *
 * hl_linux_socket_is() asks whether this descriptor is a socket THIS LAYER
 * created and owns a side record for. Here the answer is always no, and that is
 * the truth rather than a stub: a socket on a POSIX host is a kernel descriptor
 * like every other, so there is no side record and every consumer's ambient
 * path is already the correct one. A caller reads a 0 as "carry on", never as
 * "not a socket" -- getsockopt(SO_TYPE) is still the way to ask that.
 */
static inline int hl_linux_socket_is(int descriptor) {
    (void)descriptor;
    return 0;
}

/* Apply the SOCK_CLOEXEC/SOCK_NONBLOCK bits a creation call carried. Two fcntls
   here; on a host with no queryable descriptor status it is a record update. */
static inline int hl_linux_socket_apply_type_flags(int descriptor, int type) {
    if ((type & HL_LINUX_SOCK_CLOEXEC) != 0) (void)fcntl(descriptor, F_SETFD, FD_CLOEXEC);
    if ((type & HL_LINUX_SOCK_NONBLOCK) != 0) (void)fcntl(descriptor, F_SETFL, O_NONBLOCK);
    return 0;
}

#else /* Windows */

#include "host_uio.h" /* struct iovec -- msghdr's gather list is one */
#include "fdhandle.h" /* hl_fdhandle_host(): the bound hl_host_services this file's REALs go through */

#include <errno.h>
#include <fcntl.h>
#include <io.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>

/* Guest-visible Linux socket vocabulary and address text codecs. */
#include "socket_guest.h"

/* =========================================================================
 * REAL -- everything that starts from, or produces, a descriptor.
 *
 * All of it goes through hl_host_network_services, the neutral socket contract,
 * and none of it goes through <winsock2.h>.  That is the same decision the
 * header note makes for the constants, applied to the calls: the numbers below
 * are the guest's, the numbers the host group speaks are nobody's, and the two
 * are joined by the translation functions in this section rather than by a cast.
 *
 * THE DESCRIPTOR.  A socket here is a real UCRT descriptor number, obtained by
 * opening the null device and immediately filing the socket's opaque handle
 * beside it.  The wasted null-device descriptor is the point rather than a
 * cost: the guest's descriptor numbering on this host IS the UCRT's, so files,
 * pipes and sockets have to come out of ONE allocator or two of them will
 * eventually hand out the same number and the second one to do so will silently
 * take over the first's meaning.  Reserving through the same allocator makes
 * lowest-free, dup2 targets and RLIMIT_NOFILE accounting agree for free.
 *
 * WHY THIS TABLE IS NOT fdhandle.c's.  That table is the FILE group's: its
 * release() closes through file->close and its clone() goes through
 * clone_for_fork, neither of which is the right operation for a socket, and its
 * state word has no field naming which group owns the binding.  Filing a socket
 * there would make close(2) call the wrong group's close.  A socket needs the
 * family, type and protocol it was created with anyway -- SO_TYPE and SO_DOMAIN
 * are answered from them, and net.c's zero-length-datagram workaround branches
 * on SO_TYPE -- so the record is a different shape as well as a different owner.
 * ========================================================================= */

#define HL_LINUX_SOCKET_MAX 8192

enum { HL_LINUX_SOCKET_CLOEXEC = 1u << 0, HL_LINUX_SOCKET_NONBLOCK = 1u << 1, HL_LINUX_SOCKET_LISTENING = 1u << 2 };

typedef struct hl_linux_socket_slot {
    /* The handle is the liveness flag: a zero here is a free slot, and it is
     * written last on publish and first on release so a reader that sees a
     * handle has already seen the record that goes with it. */
    _Atomic uint_least64_t handle;
    _Atomic uint_least32_t flags;
    uint16_t family; /* guest AF_*, as the guest asked for it */
    uint16_t type;   /* guest SOCK_*, masked to SOCK_TYPE_MASK */
    int32_t protocol;
} hl_linux_socket_slot;

static hl_linux_socket_slot hl_linux_socket_table[HL_LINUX_SOCKET_MAX];

static inline const hl_host_services *hl_linux_socket_services(void) {
    const hl_host_services *host = hl_fdhandle_host();
    if (host == NULL || (host->capabilities & HL_HOST_CAP_NETWORK) == 0) return NULL;
    /* ABI 1 carries six callbacks and none of the ones below; a provider that
     * stops there is not a socket layer, it is the stub that preceded one. */
    if (host->network == NULL || host->network->abi < 2u) return NULL;
    return host;
}

static inline int hl_linux_socket_slot_of(int descriptor, hl_host_handle *out) {
    uint_least64_t handle;
    if (descriptor < 0 || descriptor >= HL_LINUX_SOCKET_MAX) return 0;
    handle = atomic_load(&hl_linux_socket_table[descriptor].handle);
    if (handle == (uint_least64_t)HL_HOST_HANDLE_INVALID) return 0;
    if (out != NULL) *out = (hl_host_handle)handle;
    return 1;
}

/* Whether this descriptor names a socket. The one question every consumer
 * outside this file asks; a 0 means "take the ambient path", never "bad fd". */
static inline int hl_linux_socket_is(int descriptor) {
    return hl_linux_socket_slot_of(descriptor, NULL);
}

/*
 * A host result -> errno, in the UCRT's numbering, because that is what the
 * dispatch boundary translates from (src/linux_abi/errno.c).
 *
 * The neutral condition is consulted FIRST and the coarse status only as a
 * fallback. That ordering is the whole reason the condition exists: hl_status
 * has one HL_STATUS_WOULD_BLOCK, and a guest needs it to arrive as EAGAIN after
 * a recv, as EINPROGRESS after a first connect and as EALREADY after a second.
 * A guest that cannot tell those apart either spins or gives up.
 */
static inline int hl_linux_socket_condition_errno(uint32_t condition) {
    switch (condition) {
    case HL_HOST_NETWORK_CONDITION_WOULD_BLOCK: return EWOULDBLOCK;
    case HL_HOST_NETWORK_CONDITION_CONNECT_IN_PROGRESS: return EINPROGRESS;
    case HL_HOST_NETWORK_CONDITION_CONNECT_PENDING: return EALREADY;
    case HL_HOST_NETWORK_CONDITION_ALREADY_CONNECTED: return EISCONN;
    case HL_HOST_NETWORK_CONDITION_NOT_CONNECTED: return ENOTCONN;
    case HL_HOST_NETWORK_CONDITION_ADDRESS_IN_USE: return EADDRINUSE;
    case HL_HOST_NETWORK_CONDITION_ADDRESS_NOT_AVAILABLE: return EADDRNOTAVAIL;
    case HL_HOST_NETWORK_CONDITION_CONNECTION_REFUSED: return ECONNREFUSED;
    case HL_HOST_NETWORK_CONDITION_CONNECTION_RESET: return ECONNRESET;
    case HL_HOST_NETWORK_CONDITION_CONNECTION_ABORTED: return ECONNABORTED;
    case HL_HOST_NETWORK_CONDITION_DESTINATION_REQUIRED: return EDESTADDRREQ;
    case HL_HOST_NETWORK_CONDITION_MESSAGE_TOO_LARGE: return EMSGSIZE;
    case HL_HOST_NETWORK_CONDITION_FAMILY_NOT_SUPPORTED: return EAFNOSUPPORT;
    case HL_HOST_NETWORK_CONDITION_PROTOCOL_NOT_SUPPORTED: return EPROTONOSUPPORT;
    case HL_HOST_NETWORK_CONDITION_TYPE_NOT_SUPPORTED: return EPROTONOSUPPORT;
    case HL_HOST_NETWORK_CONDITION_OPTION_NOT_SUPPORTED: return ENOPROTOOPT;
    case HL_HOST_NETWORK_CONDITION_WRONG_PROTOCOL: return EPROTOTYPE;
    case HL_HOST_NETWORK_CONDITION_NOT_A_SOCKET: return ENOTSOCK;
    case HL_HOST_NETWORK_CONDITION_HOST_UNREACHABLE: return EHOSTUNREACH;
    case HL_HOST_NETWORK_CONDITION_NETWORK_UNREACHABLE: return ENETUNREACH;
    case HL_HOST_NETWORK_CONDITION_NETWORK_DOWN: return ENETDOWN;
    case HL_HOST_NETWORK_CONDITION_NETWORK_RESET: return ENETRESET;
    case HL_HOST_NETWORK_CONDITION_BUFFER_EXHAUSTED: return ENOBUFS;
    case HL_HOST_NETWORK_CONDITION_SHUT_DOWN: return EPIPE;
    case HL_HOST_NETWORK_CONDITION_BROKEN_PIPE: return EPIPE;
    case HL_HOST_NETWORK_CONDITION_OPERATION_NOT_SUPPORTED: return EOPNOTSUPP;
    case HL_HOST_NETWORK_CONDITION_TIMED_OUT: return ETIMEDOUT;
    case HL_HOST_NETWORK_CONDITION_INTERRUPTED: return EINTR;
    default: return 0;
    }
}

static inline int hl_linux_socket_fail(hl_host_result result) {
    int code = 0;
    if (result.detail_domain == HL_HOST_DETAIL_NETWORK) code = hl_linux_socket_condition_errno((uint32_t)result.detail);
    if (code == 0) code = hl_fdhandle_errno(result.status);
    errno = code;
    return -1;
}

/* ---- the descriptor reservation ---------------------------------------- */

static inline void hl_linux_socket_forget(int descriptor) {
    if (descriptor < 0 || descriptor >= HL_LINUX_SOCKET_MAX) return;
    atomic_store(&hl_linux_socket_table[descriptor].handle, (uint_least64_t)HL_HOST_HANDLE_INVALID);
    atomic_store(&hl_linux_socket_table[descriptor].flags, 0u);
}

/*
 * Reserve a descriptor number from the UCRT's own allocator. "NUL" is the null
 * device on this host; the descriptor is never read or written, only held, so
 * that the number cannot be handed to anyone else while a socket answers to it.
 *
 * A number that comes back already carrying a slot is a stale binding whose
 * descriptor was closed by something that did not know it was a socket. It is
 * dropped rather than trusted: the allocator has just told us the old owner is
 * gone, and that is a stronger statement than anything the stale record says.
 */
static inline int hl_linux_socket_reserve(void) {
    int descriptor = _open("NUL", _O_RDONLY | _O_BINARY);
    if (descriptor < 0) return -1;
    if (descriptor >= HL_LINUX_SOCKET_MAX) {
        _close(descriptor);
        errno = EMFILE;
        return -1;
    }
    hl_linux_socket_forget(descriptor);
    return descriptor;
}

static inline void hl_linux_socket_publish(int descriptor, hl_host_handle handle, int family, int type, int protocol,
                                           uint32_t flags) {
    hl_linux_socket_slot *slot = &hl_linux_socket_table[descriptor];
    slot->family = (uint16_t)family;
    slot->type = (uint16_t)(type & SOCK_TYPE_MASK);
    slot->protocol = (int32_t)protocol;
    atomic_store(&slot->flags, (uint_least32_t)flags);
    atomic_store(&slot->handle, (uint_least64_t)handle);
}

/* ---- constant translation ----------------------------------------------- */

static inline int hl_linux_socket_family_to_host(int family, uint32_t *out) {
    switch (family) {
    case AF_INET: *out = HL_HOST_NETWORK_IPV4; return 1;
    case AF_INET6: *out = HL_HOST_NETWORK_IPV6; return 1;
    case AF_UNIX: *out = HL_HOST_NETWORK_LOCAL; return 1;
    default: return 0;
    }
}

static inline int hl_linux_socket_family_from_host(uint32_t family) {
    switch (family) {
    case HL_HOST_NETWORK_IPV4: return AF_INET;
    case HL_HOST_NETWORK_IPV6: return AF_INET6;
    case HL_HOST_NETWORK_LOCAL: return AF_UNIX;
    default: return AF_UNSPEC;
    }
}

static inline int hl_linux_socket_type_to_host(int type, uint32_t *out) {
    switch (type & SOCK_TYPE_MASK) {
    case SOCK_STREAM: *out = HL_HOST_NETWORK_STREAM; return 1;
    case SOCK_DGRAM: *out = HL_HOST_NETWORK_DATAGRAM; return 1;
    case SOCK_SEQPACKET: *out = HL_HOST_NETWORK_SEQPACKET; return 1;
    case SOCK_RAW: *out = HL_HOST_NETWORK_RAW; return 1;
    default: return 0;
    }
}

/*
 * Guest MSG_* -> contract MSG_*. Never a pass-through, and the reason is
 * measured rather than theoretical: the guest's MSG_DONTWAIT is 0x40, which a
 * Winsock send rejects outright with WSAEOPNOTSUPP, and the guest's MSG_WAITALL
 * is 0x100 against that host's 0x8. Bits with no contract meaning are dropped
 * rather than refused, which matches what a Linux kernel does with the ones it
 * does not implement for a given socket.
 */
static inline uint32_t hl_linux_socket_message_flags(int flags) {
    uint32_t out = 0;
    if ((flags & MSG_PEEK) != 0) out |= HL_HOST_MSG_PEEK;
    if ((flags & MSG_OOB) != 0) out |= HL_HOST_MSG_OUT_OF_BAND;
    if ((flags & MSG_DONTWAIT) != 0) out |= HL_HOST_MSG_DONT_WAIT;
    if ((flags & MSG_WAITALL) != 0) out |= HL_HOST_MSG_WAIT_ALL;
    if ((flags & MSG_DONTROUTE) != 0) out |= HL_HOST_MSG_DONT_ROUTE;
    if ((flags & MSG_NOSIGNAL) != 0) out |= HL_HOST_MSG_NO_SIGNAL;
    if ((flags & MSG_EOR) != 0) out |= HL_HOST_MSG_END_OF_RECORD;
    if ((flags & MSG_MORE) != 0) out |= HL_HOST_MSG_MORE;
    return out;
}

static inline int hl_linux_socket_message_flags_back(uint32_t flags) {
    int out = 0;
    if ((flags & HL_HOST_MSG_TRUNCATED) != 0) out |= MSG_TRUNC;
    if ((flags & HL_HOST_MSG_CONTROL_TRUNCATED) != 0) out |= MSG_CTRUNC;
    if ((flags & HL_HOST_MSG_RECEIVED_OUT_OF_BAND) != 0) out |= MSG_OOB;
    if ((flags & HL_HOST_MSG_RECEIVED_END_OF_RECORD) != 0) out |= MSG_EOR;
    return out;
}

/*
 * (level, name) -> the flat neutral option, and zero for anything this seam
 * does not carry.
 *
 * The flattening is the point and it is worth restating where the translation
 * happens rather than only where the enum is declared: the pair (1, 2) is
 * SO_REUSEADDR to a Linux guest, SO_ACCEPTCONN to a Windows host and IP_TTL at
 * another level entirely, and an interface that carried the pair across would
 * let a single missing `break` in some later switch apply one of those three in
 * place of another -- silently, because setting an option produces no
 * observable result. After this function there is no level left to get wrong.
 */
static inline uint32_t hl_linux_socket_option(int level, int name) {
    if (level == SOL_SOCKET) switch (name) {
        case SO_REUSEADDR: return HL_HOST_SOCKOPT_REUSE_ADDRESS;
        case SO_REUSEPORT: return HL_HOST_SOCKOPT_REUSE_PORT;
        case SO_KEEPALIVE: return HL_HOST_SOCKOPT_KEEP_ALIVE;
        case SO_BROADCAST: return HL_HOST_SOCKOPT_BROADCAST;
        case SO_DONTROUTE: return HL_HOST_SOCKOPT_DONT_ROUTE;
        case SO_OOBINLINE: return HL_HOST_SOCKOPT_OUT_OF_BAND_INLINE;
        case SO_SNDBUF: return HL_HOST_SOCKOPT_SEND_BUFFER;
        case SO_RCVBUF: return HL_HOST_SOCKOPT_RECEIVE_BUFFER;
        case SO_SNDBUFFORCE: return HL_HOST_SOCKOPT_SEND_BUFFER;
        case SO_RCVBUFFORCE: return HL_HOST_SOCKOPT_RECEIVE_BUFFER;
        case SO_SNDLOWAT: return HL_HOST_SOCKOPT_SEND_LOW_WATER;
        case SO_RCVLOWAT: return HL_HOST_SOCKOPT_RECEIVE_LOW_WATER;
        case SO_SNDTIMEO: return HL_HOST_SOCKOPT_SEND_TIMEOUT;
        case SO_RCVTIMEO: return HL_HOST_SOCKOPT_RECEIVE_TIMEOUT;
        case SO_LINGER: return HL_HOST_SOCKOPT_LINGER;
        case SO_ERROR: return HL_HOST_SOCKOPT_ERROR;
        case SO_TYPE: return HL_HOST_SOCKOPT_TYPE;
        case SO_PROTOCOL: return HL_HOST_SOCKOPT_PROTOCOL;
        case SO_DOMAIN: return HL_HOST_SOCKOPT_DOMAIN;
        case SO_ACCEPTCONN: return HL_HOST_SOCKOPT_ACCEPT_CONNECTIONS;
        case SO_PEERCRED: return HL_HOST_SOCKOPT_PEER_CREDENTIALS;
        case SO_PASSCRED: return HL_HOST_SOCKOPT_PASS_CREDENTIALS;
        default: return 0;
        }
    if (level == SOL_TCP) switch (name) {
        case TCP_NODELAY: return HL_HOST_SOCKOPT_TCP_NO_DELAY;
        case TCP_KEEPIDLE: return HL_HOST_SOCKOPT_TCP_KEEP_IDLE;
        case TCP_KEEPINTVL: return HL_HOST_SOCKOPT_TCP_KEEP_INTERVAL;
        case TCP_KEEPCNT: return HL_HOST_SOCKOPT_TCP_KEEP_COUNT;
        case TCP_MAXSEG: return HL_HOST_SOCKOPT_TCP_MAX_SEGMENT;
        case TCP_CORK: return HL_HOST_SOCKOPT_TCP_CORK;
        case TCP_QUICKACK: return HL_HOST_SOCKOPT_TCP_QUICK_ACK;
        case TCP_USER_TIMEOUT: return HL_HOST_SOCKOPT_TCP_USER_TIMEOUT;
        default: return 0;
        }
    if (level == SOL_IP) switch (name) {
        case IP_TTL: return HL_HOST_SOCKOPT_IP_TIME_TO_LIVE;
        case IP_TOS: return HL_HOST_SOCKOPT_IP_TYPE_OF_SERVICE;
        case IP_HDRINCL: return HL_HOST_SOCKOPT_IP_HEADER_INCLUDED;
        case IP_MULTICAST_TTL: return HL_HOST_SOCKOPT_IP_MULTICAST_TTL;
        case IP_MULTICAST_LOOP: return HL_HOST_SOCKOPT_IP_MULTICAST_LOOP;
        case IP_PKTINFO: return HL_HOST_SOCKOPT_IP_PACKET_INFO;
        default: return 0;
        }
    if (level == SOL_IPV6) switch (name) {
        case IPV6_V6ONLY: return HL_HOST_SOCKOPT_IPV6_ONLY;
        case IPV6_UNICAST_HOPS: return HL_HOST_SOCKOPT_IPV6_UNICAST_HOPS;
        case IPV6_MULTICAST_HOPS: return HL_HOST_SOCKOPT_IPV6_MULTICAST_HOPS;
        case IPV6_MULTICAST_LOOP: return HL_HOST_SOCKOPT_IPV6_MULTICAST_LOOP;
        case IPV6_RECVPKTINFO: return HL_HOST_SOCKOPT_IPV6_PACKET_INFO;
        default: return 0;
        }
    return 0;
}

/* ---- addresses ----------------------------------------------------------- */

/* Local, because <arpa/inet.h>'s swaps are supplied by native_compat.h, which
 * is included after this header in the unity translation unit. */
static inline uint16_t hl_linux_socket_swap16(uint16_t value) {
    return (uint16_t)(((value & 0x00ffu) << 8) | ((value & 0xff00u) >> 8));
}

static inline int hl_linux_socket_address_to_host(const struct sockaddr *address, socklen_t length,
                                                  hl_host_network_address *out) {
    memset(out, 0, sizeof(*out));
    if (address == NULL || length < (socklen_t)sizeof(sa_family_t)) {
        errno = EINVAL;
        return -1;
    }
    if (address->sa_family == AF_INET && length >= (socklen_t)sizeof(struct sockaddr_in)) {
        const struct sockaddr_in *ipv4 = (const struct sockaddr_in *)address;
        out->family = HL_HOST_NETWORK_IPV4;
        out->port = hl_linux_socket_swap16(ipv4->sin_port);
        out->size = 4;
        memcpy(out->address, &ipv4->sin_addr, 4);
        return 0;
    }
    if (address->sa_family == AF_INET6 && length >= (socklen_t)sizeof(struct sockaddr_in6)) {
        const struct sockaddr_in6 *ipv6 = (const struct sockaddr_in6 *)address;
        out->family = HL_HOST_NETWORK_IPV6;
        out->port = hl_linux_socket_swap16(ipv6->sin6_port);
        out->size = 16;
        memcpy(out->address, &ipv6->sin6_addr, 16);
        out->scope_id = ipv6->sin6_scope_id;
        out->flow_info = ipv6->sin6_flowinfo;
        return 0;
    }
    if (address->sa_family == AF_UNIX) {
        const struct sockaddr_un *local = (const struct sockaddr_un *)address;
        size_t capacity;
        size_t size = 0;
        if (length <= (socklen_t)offsetof(struct sockaddr_un, sun_path)) {
            /* An unnamed local address: legal, and the caller must be able to
             * express it, so this is a success with a zero-length name. */
            out->family = HL_HOST_NETWORK_LOCAL;
            return 0;
        }
        capacity = (size_t)length - offsetof(struct sockaddr_un, sun_path);
        if (capacity > sizeof(local->sun_path)) capacity = sizeof(local->sun_path);
        while (size < capacity && local->sun_path[size] != '\0')
            size++;
        if (size >= sizeof(out->local_path)) {
            errno = ENAMETOOLONG;
            return -1;
        }
        out->family = HL_HOST_NETWORK_LOCAL;
        out->size = (uint16_t)size;
        if (size != 0) memcpy(out->local_path, local->sun_path, size);
        /* A leading NUL is Linux's abstract namespace; it survives this
         * translation as a zero-length name plus the first byte, which is not
         * something the contract can carry, so it is named here and refused by
         * the provider rather than being flattened into "unnamed". */
        if (capacity != 0 && local->sun_path[0] == '\0' && length > (socklen_t)offsetof(struct sockaddr_un, sun_path)) {
            out->size = 1;
            out->local_path[0] = '\0';
        }
        return 0;
    }
    errno = EAFNOSUPPORT;
    return -1;
}

/*
 * Contract address -> sockaddr, returning the FULL length the address needs
 * even when the caller's buffer is shorter. That is getsockname(2)'s contract --
 * a short buffer is truncated and the real length is reported -- and a function
 * that returned the copied length instead would make every truncation invisible.
 */
static inline socklen_t hl_linux_socket_address_from_host(const hl_host_network_address *in, struct sockaddr *out,
                                                          socklen_t capacity) {
    struct sockaddr_storage storage;
    socklen_t length;
    memset(&storage, 0, sizeof(storage));
    if (in->family == HL_HOST_NETWORK_IPV4) {
        struct sockaddr_in *ipv4 = (struct sockaddr_in *)&storage;
        ipv4->sin_family = AF_INET;
        ipv4->sin_port = hl_linux_socket_swap16(in->port);
        memcpy(&ipv4->sin_addr, in->address, 4);
        length = (socklen_t)sizeof(*ipv4);
    } else if (in->family == HL_HOST_NETWORK_IPV6) {
        struct sockaddr_in6 *ipv6 = (struct sockaddr_in6 *)&storage;
        ipv6->sin6_family = AF_INET6;
        ipv6->sin6_port = hl_linux_socket_swap16(in->port);
        memcpy(&ipv6->sin6_addr, in->address, 16);
        ipv6->sin6_scope_id = in->scope_id;
        ipv6->sin6_flowinfo = in->flow_info;
        length = (socklen_t)sizeof(*ipv6);
    } else if (in->family == HL_HOST_NETWORK_LOCAL) {
        struct sockaddr_un *local = (struct sockaddr_un *)&storage;
        local->sun_family = AF_UNIX;
        if (in->size != 0 && in->size < sizeof(local->sun_path)) memcpy(local->sun_path, in->local_path, in->size);
        /* Linux reports an UNNAMED local socket as exactly the family word: two
         * bytes, not the full structure. Callers compare that length against
         * sizeof(sa_family_t) to decide whether a peer has a name at all. */
        length = in->size == 0 ? (socklen_t)sizeof(sa_family_t)
                               : (socklen_t)(offsetof(struct sockaddr_un, sun_path) + in->size + 1u);
    } else {
        length = 0;
    }
    if (out != NULL && capacity != 0 && length != 0)
        memcpy(out, &storage, capacity < length ? (size_t)capacity : (size_t)length);
    return length;
}

/* ---- creation ------------------------------------------------------------ */

static inline int hl_linux_socket_adopt(hl_host_handle handle, int family, int type, int protocol, uint32_t flags) {
    const hl_host_services *host = hl_linux_socket_services();
    int descriptor = hl_linux_socket_reserve();
    if (descriptor < 0) {
        if (host != NULL) (void)host->network->close(host->context, handle);
        errno = EMFILE;
        return -1;
    }
    hl_linux_socket_publish(descriptor, handle, family, type, protocol, flags);
    return descriptor;
}

static inline uint32_t hl_linux_socket_type_flags(int type) {
    uint32_t flags = 0;
    if ((type & SOCK_CLOEXEC) != 0) flags |= HL_LINUX_SOCKET_CLOEXEC;
    if ((type & SOCK_NONBLOCK) != 0) flags |= HL_LINUX_SOCKET_NONBLOCK;
    return flags;
}

/* Push the object's non-blocking bit down to the provider. The provider owns
 * blocking; this is the only thing that tells it which mode to be in. */
static inline int hl_linux_socket_sync_status(int descriptor) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_handle handle;
    uint32_t flags;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) return 0;
    flags = (uint32_t)atomic_load(&hl_linux_socket_table[descriptor].flags);
    (void)host->network->set_status_flags(host->context, handle,
                                          (flags & HL_LINUX_SOCKET_NONBLOCK) != 0 ? HL_HOST_SOCKET_NONBLOCK : 0u);
    return 0;
}

static inline int socket(int family, int type, int protocol) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_result created;
    uint32_t host_family;
    uint32_t host_type;
    int descriptor;
    if (host == NULL) {
        errno = ENOSYS;
        return -1;
    }
    if (!hl_linux_socket_family_to_host(family, &host_family)) {
        errno = EAFNOSUPPORT;
        return -1;
    }
    if (!hl_linux_socket_type_to_host(type, &host_type)) {
        errno = EPROTONOSUPPORT;
        return -1;
    }
    created = host->network->socket(host->context, host_family, host_type, (uint32_t)protocol);
    if (created.status != HL_STATUS_OK) return hl_linux_socket_fail(created);
    descriptor = hl_linux_socket_adopt(created.value, family, type, protocol, hl_linux_socket_type_flags(type));
    if (descriptor >= 0 && (type & SOCK_NONBLOCK) != 0) (void)hl_linux_socket_sync_status(descriptor);
    return descriptor;
}

static inline int socketpair(int family, int type, int protocol, int descriptors[2]) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_handle ends[2];
    hl_host_result created;
    uint32_t host_family;
    uint32_t host_type;
    if (host == NULL) {
        errno = ENOSYS;
        return -1;
    }
    if (descriptors == NULL) {
        errno = EFAULT;
        return -1;
    }
    if (!hl_linux_socket_family_to_host(family, &host_family)) {
        errno = EAFNOSUPPORT;
        return -1;
    }
    if (!hl_linux_socket_type_to_host(type, &host_type)) {
        errno = EPROTONOSUPPORT;
        return -1;
    }
    ends[0] = HL_HOST_HANDLE_INVALID;
    ends[1] = HL_HOST_HANDLE_INVALID;
    created = host->network->pair(host->context, host_family, host_type, (uint32_t)protocol, ends);
    if (created.status != HL_STATUS_OK) return hl_linux_socket_fail(created);
    descriptors[0] = hl_linux_socket_adopt(ends[0], family, type, protocol, hl_linux_socket_type_flags(type));
    if (descriptors[0] < 0) {
        (void)host->network->close(host->context, ends[1]);
        return -1;
    }
    descriptors[1] = hl_linux_socket_adopt(ends[1], family, type, protocol, hl_linux_socket_type_flags(type));
    if (descriptors[1] < 0) {
        int saved = errno;
        hl_host_handle first;
        if (hl_linux_socket_slot_of(descriptors[0], &first)) {
            (void)host->network->close(host->context, first);
            hl_linux_socket_forget(descriptors[0]);
            _close(descriptors[0]);
        }
        errno = saved;
        return -1;
    }
    if ((type & SOCK_NONBLOCK) != 0) {
        (void)hl_linux_socket_sync_status(descriptors[0]);
        (void)hl_linux_socket_sync_status(descriptors[1]);
    }
    return 0;
}

/* ---- naming -------------------------------------------------------------- */

static inline int bind(int descriptor, const struct sockaddr *address, socklen_t length) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_network_address translated;
    hl_host_handle handle;
    hl_host_result result;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    if (hl_linux_socket_address_to_host(address, length, &translated) != 0) return -1;
    result = host->network->bind(host->context, handle, &translated);
    return result.status == HL_STATUS_OK ? 0 : hl_linux_socket_fail(result);
}

static inline int listen(int descriptor, int backlog) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_handle handle;
    hl_host_result result;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    if (backlog < 0) backlog = 0;
    if (backlog > SOMAXCONN) backlog = SOMAXCONN;
    result = host->network->listen(host->context, handle, (uint32_t)backlog);
    if (result.status != HL_STATUS_OK) return hl_linux_socket_fail(result);
    atomic_fetch_or(&hl_linux_socket_table[descriptor].flags, (uint_least32_t)HL_LINUX_SOCKET_LISTENING);
    return 0;
}

static inline int accept4(int descriptor, struct sockaddr *address, socklen_t *length, int flags) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_network_address peer;
    hl_host_handle handle;
    hl_host_result result;
    hl_linux_socket_slot *slot;
    int accepted;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    if ((flags & ~(SOCK_CLOEXEC | SOCK_NONBLOCK)) != 0) {
        errno = EINVAL;
        return -1;
    }
    slot = &hl_linux_socket_table[descriptor];
    memset(&peer, 0, sizeof(peer));
    result = host->network->accept(host->context, handle, &peer,
                                   (flags & SOCK_NONBLOCK) != 0 ? HL_HOST_SOCKET_NONBLOCK : 0u);
    if (result.status != HL_STATUS_OK) return hl_linux_socket_fail(result);
    accepted = hl_linux_socket_adopt(result.value, (int)slot->family, (int)slot->type, (int)slot->protocol,
                                     hl_linux_socket_type_flags(flags));
    if (accepted < 0) return -1;
    if (address != NULL && length != NULL) {
        const socklen_t needed = hl_linux_socket_address_from_host(&peer, address, *length);
        *length = needed;
    }
    return accepted;
}

static inline int accept(int descriptor, struct sockaddr *address, socklen_t *length) {
    return accept4(descriptor, address, length, 0);
}

static inline int connect(int descriptor, const struct sockaddr *address, socklen_t length) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_network_address translated;
    hl_host_handle handle;
    hl_host_result result;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    if (hl_linux_socket_address_to_host(address, length, &translated) != 0) return -1;
    result = host->network->connect(host->context, handle, &translated);
    return result.status == HL_STATUS_OK ? 0 : hl_linux_socket_fail(result);
}

static inline int shutdown(int descriptor, int direction) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_handle handle;
    hl_host_result result;
    uint32_t neutral;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    switch (direction) {
    case SHUT_RD: neutral = HL_HOST_SHUTDOWN_READ; break;
    case SHUT_WR: neutral = HL_HOST_SHUTDOWN_WRITE; break;
    case SHUT_RDWR: neutral = HL_HOST_SHUTDOWN_BOTH; break;
    default: errno = EINVAL; return -1;
    }
    result = host->network->shutdown(host->context, handle, neutral);
    return result.status == HL_STATUS_OK ? 0 : hl_linux_socket_fail(result);
}

static inline int hl_linux_socket_name(int descriptor, struct sockaddr *address, socklen_t *length, int peer) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_network_address named;
    hl_host_handle handle;
    hl_host_result result;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    if (address == NULL || length == NULL) {
        errno = EFAULT;
        return -1;
    }
    memset(&named, 0, sizeof(named));
    result = peer ? host->network->peer_address(host->context, handle, &named)
                  : host->network->local_address(host->context, handle, &named);
    if (result.status != HL_STATUS_OK) return hl_linux_socket_fail(result);
    *length = hl_linux_socket_address_from_host(&named, address, *length);
    return 0;
}

static inline int getsockname(int descriptor, struct sockaddr *address, socklen_t *length) {
    return hl_linux_socket_name(descriptor, address, length, 0);
}

static inline int getpeername(int descriptor, struct sockaddr *address, socklen_t *length) {
    return hl_linux_socket_name(descriptor, address, length, 1);
}

/* ---- options ------------------------------------------------------------- */

/*
 * The three options answered from this side rather than from the provider,
 * because this side is the only one that knows the answer in the GUEST's
 * numbering: SO_TYPE, SO_DOMAIN and SO_PROTOCOL report what the guest asked
 * socket() for. A provider could report only its own family and type constants,
 * and translating them back would turn AF_UNIX into whatever that host spells
 * it -- which is exactly the class of round-trip this file exists to avoid.
 */
static inline int hl_linux_socket_option_local(int descriptor, int level, int name, void *value, socklen_t *length) {
    const hl_linux_socket_slot *slot = &hl_linux_socket_table[descriptor];
    int answer;
    if (level != SOL_SOCKET) return 0;
    if (name == SO_TYPE)
        answer = (int)slot->type;
    else if (name == SO_DOMAIN)
        answer = (int)slot->family;
    else if (name == SO_PROTOCOL)
        answer = (int)slot->protocol;
    else if (name == SO_ACCEPTCONN)
        answer = (atomic_load(&hl_linux_socket_table[descriptor].flags) & HL_LINUX_SOCKET_LISTENING) != 0 ? 1 : 0;
    else
        return 0;
    if (value == NULL || length == NULL || *length < (socklen_t)sizeof(int)) {
        errno = EINVAL;
        return -1;
    }
    memcpy(value, &answer, sizeof(answer));
    *length = (socklen_t)sizeof(answer);
    return 1;
}

static inline int getsockopt(int descriptor, int level, int option, void *value, socklen_t *length) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_handle handle;
    hl_host_result result;
    hl_host_bytes span;
    uint32_t neutral;
    unsigned char scratch[32];
    int local;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    local = hl_linux_socket_option_local(descriptor, level, option, value, length);
    if (local != 0) return local < 0 ? -1 : 0;
    neutral = hl_linux_socket_option(level, option);
    if (neutral == 0) {
        errno = ENOPROTOOPT;
        return -1;
    }
    if (value == NULL || length == NULL) {
        errno = EFAULT;
        return -1;
    }
    memset(scratch, 0, sizeof(scratch));
    span.data = scratch;
    span.size = sizeof(scratch);
    result = host->network->get_option(host->context, handle, neutral, span);
    if (result.status != HL_STATUS_OK) return hl_linux_socket_fail(result);
    if (neutral == HL_HOST_SOCKOPT_LINGER) {
        hl_host_network_linger neutral_linger;
        struct linger guest;
        if (*length < (socklen_t)sizeof(guest)) {
            errno = EINVAL;
            return -1;
        }
        memcpy(&neutral_linger, scratch, sizeof(neutral_linger));
        guest.l_onoff = (int)neutral_linger.enabled;
        guest.l_linger = (int)neutral_linger.seconds;
        memcpy(value, &guest, sizeof(guest));
        *length = (socklen_t)sizeof(guest);
        return 0;
    }
    if (neutral == HL_HOST_SOCKOPT_SEND_TIMEOUT || neutral == HL_HOST_SOCKOPT_RECEIVE_TIMEOUT) {
        /* The guest carries a timeout as a struct timeval; the contract carries
         * nanoseconds, because one of the three hosts takes milliseconds in a
         * DWORD and none of them agree on the structure. */
        uint64_t nanoseconds;

        struct {
            int64_t seconds;
            int64_t microseconds;
        } guest;

        if (*length < (socklen_t)sizeof(guest)) {
            errno = EINVAL;
            return -1;
        }
        memcpy(&nanoseconds, scratch, sizeof(nanoseconds));
        guest.seconds = (int64_t)(nanoseconds / UINT64_C(1000000000));
        guest.microseconds = (int64_t)((nanoseconds % UINT64_C(1000000000)) / UINT64_C(1000));
        memcpy(value, &guest, sizeof(guest));
        *length = (socklen_t)sizeof(guest);
        return 0;
    }
    if (neutral == HL_HOST_SOCKOPT_ERROR) {
        /* The contract reports a status here, never a host error number, which
         * is what makes SO_ERROR portable at all. It becomes the guest's errno
         * on the way out and nothing else. */
        uint32_t status;
        int answer;
        if (*length < (socklen_t)sizeof(int)) {
            errno = EINVAL;
            return -1;
        }
        memcpy(&status, scratch, sizeof(status));
        answer = status == (uint32_t)HL_STATUS_OK ? 0 : hl_fdhandle_errno((int32_t)status);
        memcpy(value, &answer, sizeof(answer));
        *length = (socklen_t)sizeof(answer);
        return 0;
    }
    {
        uint32_t scalar;
        int answer;
        if (*length < (socklen_t)sizeof(int)) {
            errno = EINVAL;
            return -1;
        }
        memcpy(&scalar, scratch, sizeof(scalar));
        answer = (int)scalar;
        memcpy(value, &answer, sizeof(answer));
        *length = (socklen_t)sizeof(answer);
    }
    return 0;
}

static inline int setsockopt(int descriptor, int level, int option, const void *value, socklen_t length) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_handle handle;
    hl_host_result result;
    hl_host_const_bytes span;
    uint32_t neutral;
    unsigned char scratch[32];
    size_t size = 0;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    neutral = hl_linux_socket_option(level, option);
    if (neutral == 0) {
        errno = ENOPROTOOPT;
        return -1;
    }
    if (value == NULL) {
        errno = EFAULT;
        return -1;
    }
    memset(scratch, 0, sizeof(scratch));
    if (neutral == HL_HOST_SOCKOPT_LINGER) {
        hl_host_network_linger neutral_linger;
        struct linger guest;
        if (length < (socklen_t)sizeof(guest)) {
            errno = EINVAL;
            return -1;
        }
        memcpy(&guest, value, sizeof(guest));
        neutral_linger.enabled = guest.l_onoff != 0 ? 1u : 0u;
        neutral_linger.seconds = (uint32_t)(guest.l_linger < 0 ? 0 : guest.l_linger);
        memcpy(scratch, &neutral_linger, sizeof(neutral_linger));
        size = sizeof(neutral_linger);
    } else if (neutral == HL_HOST_SOCKOPT_SEND_TIMEOUT || neutral == HL_HOST_SOCKOPT_RECEIVE_TIMEOUT) {
        struct {
            int64_t seconds;
            int64_t microseconds;
        } guest;

        uint64_t nanoseconds;
        if (length < (socklen_t)sizeof(guest)) {
            errno = EINVAL;
            return -1;
        }
        memcpy(&guest, value, sizeof(guest));
        if (guest.seconds < 0 || guest.microseconds < 0) {
            errno = EINVAL;
            return -1;
        }
        nanoseconds = (uint64_t)guest.seconds * UINT64_C(1000000000) + (uint64_t)guest.microseconds * UINT64_C(1000);
        memcpy(scratch, &nanoseconds, sizeof(nanoseconds));
        size = sizeof(nanoseconds);
    } else {
        uint32_t scalar;
        int given = 0;
        if (length < (socklen_t)sizeof(int)) {
            errno = EINVAL;
            return -1;
        }
        memcpy(&given, value, sizeof(given));
        scalar = (uint32_t)given;
        memcpy(scratch, &scalar, sizeof(scalar));
        size = sizeof(scalar);
    }
    span.data = scratch;
    span.size = size;
    result = host->network->set_option(host->context, handle, neutral, span);
    return result.status == HL_STATUS_OK ? 0 : hl_linux_socket_fail(result);
}

/* ---- transfers ----------------------------------------------------------- */

enum { HL_LINUX_SOCKET_IOV_MAX = 64 };

static inline ssize_t hl_linux_socket_message(int descriptor, const struct iovec *vectors, unsigned int count,
                                              int flags, const struct sockaddr *destination,
                                              socklen_t destination_length, struct sockaddr *source,
                                              socklen_t *source_length, int *out_flags) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_network_address address;
    hl_host_network_message message;
    hl_host_iovec buffers[HL_LINUX_SOCKET_IOV_MAX];
    hl_host_handle handle;
    hl_host_result result;
    unsigned int index;
    const int sending = destination != NULL || out_flags == NULL;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    if (count > HL_LINUX_SOCKET_IOV_MAX) {
        errno = EMSGSIZE;
        return -1;
    }
    if (count != 0 && vectors == NULL) {
        errno = EFAULT;
        return -1;
    }
    for (index = 0; index < count; ++index) {
        buffers[index].address = (uint64_t)(uintptr_t)vectors[index].iov_base;
        buffers[index].size = (uint64_t)vectors[index].iov_len;
    }
    memset(&address, 0, sizeof(address));
    memset(&message, 0, sizeof(message));
    message.buffers = buffers;
    message.buffer_count = count;
    if (sending) {
        if (destination != NULL) {
            if (hl_linux_socket_address_to_host(destination, destination_length, &address) != 0) return -1;
            message.address = &address;
        }
        result = host->network->send_message(host->context, handle, &message, hl_linux_socket_message_flags(flags));
        return result.status == HL_STATUS_OK ? (ssize_t)result.value : (ssize_t)hl_linux_socket_fail(result);
    }
    if (source != NULL && source_length != NULL) message.address = &address;
    result = host->network->receive_message(host->context, handle, &message, hl_linux_socket_message_flags(flags));
    if (result.status != HL_STATUS_OK) return (ssize_t)hl_linux_socket_fail(result);
    if (source != NULL && source_length != NULL)
        *source_length = hl_linux_socket_address_from_host(&address, source, *source_length);
    if (out_flags != NULL) *out_flags = hl_linux_socket_message_flags_back(message.flags);
    return (ssize_t)result.value;
}

static inline ssize_t send(int descriptor, const void *buffer, size_t length, int flags) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_const_bytes span;
    hl_host_handle handle;
    hl_host_result result;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    span.data = buffer;
    span.size = length;
    result = host->network->send(host->context, handle, span, hl_linux_socket_message_flags(flags));
    return result.status == HL_STATUS_OK ? (ssize_t)result.value : (ssize_t)hl_linux_socket_fail(result);
}

static inline ssize_t recv(int descriptor, void *buffer, size_t length, int flags) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_bytes span;
    hl_host_handle handle;
    hl_host_result result;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    span.data = buffer;
    span.size = length;
    result = host->network->receive(host->context, handle, span, hl_linux_socket_message_flags(flags));
    return result.status == HL_STATUS_OK ? (ssize_t)result.value : (ssize_t)hl_linux_socket_fail(result);
}

static inline ssize_t sendto(int descriptor, const void *buffer, size_t length, int flags,
                             const struct sockaddr *address, socklen_t address_length) {
    struct iovec vector;
    if (address == NULL) return send(descriptor, buffer, length, flags);
    vector.iov_base = (void *)(uintptr_t)buffer;
    vector.iov_len = length;
    return hl_linux_socket_message(descriptor, &vector, 1, flags, address, address_length, NULL, NULL, NULL);
}

static inline ssize_t recvfrom(int descriptor, void *buffer, size_t length, int flags, struct sockaddr *address,
                               socklen_t *address_length) {
    struct iovec vector;
    int out_flags = 0;
    if (address == NULL || address_length == NULL) return recv(descriptor, buffer, length, flags);
    vector.iov_base = buffer;
    vector.iov_len = length;
    return hl_linux_socket_message(descriptor, &vector, 1, flags, NULL, 0, address, address_length, &out_flags);
}

static inline ssize_t sendmsg(int descriptor, const struct msghdr *message, int flags) {
    if (message == NULL) {
        errno = EFAULT;
        return -1;
    }
    /* Ancillary data has no carrier on this seam yet, and a send that dropped
     * it would report success for a descriptor the peer never receives. */
    if (message->msg_control != NULL && message->msg_controllen != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    return hl_linux_socket_message(descriptor, message->msg_iov, (unsigned int)message->msg_iovlen, flags,
                                   (const struct sockaddr *)message->msg_name, (socklen_t)message->msg_namelen, NULL,
                                   NULL, NULL);
}

static inline ssize_t recvmsg(int descriptor, struct msghdr *message, int flags) {
    socklen_t namelen;
    int out_flags = 0;
    ssize_t received;
    if (message == NULL) {
        errno = EFAULT;
        return -1;
    }
    namelen = message->msg_namelen;
    received = hl_linux_socket_message(descriptor, message->msg_iov, (unsigned int)message->msg_iovlen, flags, NULL, 0,
                                       (struct sockaddr *)message->msg_name, &namelen, &out_flags);
    if (received < 0) return -1;
    if (message->msg_name != NULL) message->msg_namelen = namelen;
    message->msg_controllen = 0;
    message->msg_flags = out_flags;
    return received;
}

static inline int sendmmsg(int descriptor, struct mmsghdr *messages, unsigned int count, int flags) {
    unsigned int index;
    if (messages == NULL) {
        errno = EFAULT;
        return -1;
    }
    for (index = 0; index < count; ++index) {
        ssize_t sent = sendmsg(descriptor, &messages[index].msg_hdr, flags);
        if (sent < 0) return index == 0 ? -1 : (int)index;
        messages[index].msg_len = (unsigned int)sent;
    }
    return (int)count;
}

static inline int recvmmsg(int descriptor, struct mmsghdr *messages, unsigned int count, int flags,
                           struct timespec *timeout) {
    unsigned int index;
    (void)timeout;
    if (messages == NULL) {
        errno = EFAULT;
        return -1;
    }
    for (index = 0; index < count; ++index) {
        ssize_t received = recvmsg(descriptor, &messages[index].msg_hdr, index == 0 ? flags : (flags | MSG_DONTWAIT));
        if (received < 0) return index == 0 ? -1 : (int)index;
        messages[index].msg_len = (unsigned int)received;
    }
    return (int)count;
}

/* =========================================================================
 * The entry points the syscall layer reaches a socket descriptor through.
 *
 * read(2), write(2), close(2), dup(2), fcntl(2) and poll(2) are the UCRT's on
 * this host and know nothing about the table above, so the syscall router asks
 * hl_linux_socket_is() first and comes here when the answer is yes. These are
 * not a second socket API -- each is the descriptor-shaped spelling of an
 * operation already defined above or on the network group.
 * ========================================================================= */

static inline ssize_t hl_linux_socket_read(int descriptor, void *buffer, size_t length) {
    return recv(descriptor, buffer, length, 0);
}

static inline ssize_t hl_linux_socket_write(int descriptor, const void *buffer, size_t length) {
    return send(descriptor, buffer, length, 0);
}

static inline int hl_linux_socket_close(int descriptor) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_handle handle;
    if (!hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = EBADF;
        return -1;
    }
    hl_linux_socket_forget(descriptor);
    if (host != NULL) (void)host->network->close(host->context, handle);
    /* The reservation goes back to the allocator last, so the number cannot be
     * reissued while the binding it used to carry is still visible. */
    return _close(descriptor);
}

/* dup(2) aliases the open socket description, which is what duplicate() is
   specified to produce; the two descriptors then share the non-blocking bit. */
static inline int hl_linux_socket_dup(int descriptor, int target) {
    const hl_host_services *host = hl_linux_socket_services();
    const hl_linux_socket_slot *slot;
    hl_host_handle handle;
    hl_host_result result;
    int created;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : EBADF;
        return -1;
    }
    slot = &hl_linux_socket_table[descriptor];
    result = host->network->duplicate(host->context, handle);
    if (result.status != HL_STATUS_OK) return hl_linux_socket_fail(result);
    if (target < 0) {
        created = hl_linux_socket_adopt(result.value, (int)slot->family, (int)slot->type, (int)slot->protocol, 0);
        return created;
    }
    if (target >= HL_LINUX_SOCKET_MAX) {
        (void)host->network->close(host->context, result.value);
        errno = EBADF;
        return -1;
    }
    if (hl_linux_socket_is(target))
        (void)hl_linux_socket_close(target);
    else
        (void)_close(target);
    {
        int reserved = _open("NUL", _O_RDONLY | _O_BINARY);
        if (reserved < 0) {
            (void)host->network->close(host->context, result.value);
            errno = EMFILE;
            return -1;
        }
        if (reserved != target) {
            if (_dup2(reserved, target) < 0) {
                _close(reserved);
                (void)host->network->close(host->context, result.value);
                errno = EBADF;
                return -1;
            }
            _close(reserved);
        }
    }
    hl_linux_socket_publish(target, result.value, (int)slot->family, (int)slot->type, (int)slot->protocol, 0);
    return target;
}

static inline int hl_linux_socket_get_flags(int descriptor, uint32_t *out) {
    if (!hl_linux_socket_is(descriptor)) return -1;
    *out = (uint32_t)atomic_load(&hl_linux_socket_table[descriptor].flags);
    return 0;
}

static inline int hl_linux_socket_set_nonblock(int descriptor, int nonblock) {
    uint_least32_t flags;
    if (!hl_linux_socket_is(descriptor)) {
        errno = EBADF;
        return -1;
    }
    flags = atomic_load(&hl_linux_socket_table[descriptor].flags);
    flags = nonblock ? (flags | HL_LINUX_SOCKET_NONBLOCK) : (flags & ~(uint_least32_t)HL_LINUX_SOCKET_NONBLOCK);
    atomic_store(&hl_linux_socket_table[descriptor].flags, flags);
    return hl_linux_socket_sync_status(descriptor);
}

static inline int hl_linux_socket_set_cloexec(int descriptor, int cloexec) {
    uint_least32_t flags;
    if (!hl_linux_socket_is(descriptor)) {
        errno = EBADF;
        return -1;
    }
    flags = atomic_load(&hl_linux_socket_table[descriptor].flags);
    flags = cloexec ? (flags | HL_LINUX_SOCKET_CLOEXEC) : (flags & ~(uint_least32_t)HL_LINUX_SOCKET_CLOEXEC);
    atomic_store(&hl_linux_socket_table[descriptor].flags, flags);
    return 0;
}

/* Apply the SOCK_CLOEXEC/SOCK_NONBLOCK bits a creation call carried. Exists on
   every host so net.c has one spelling; on a POSIX host it is two fcntls. */
static inline int hl_linux_socket_apply_type_flags(int descriptor, int type) {
    if ((type & SOCK_CLOEXEC) != 0) (void)hl_linux_socket_set_cloexec(descriptor, 1);
    if ((type & SOCK_NONBLOCK) != 0) (void)hl_linux_socket_set_nonblock(descriptor, 1);
    return 0;
}

static inline int hl_linux_socket_readiness_and_pending(int descriptor, uint32_t interests, uint32_t *out,
                                                        uint64_t *pending) {
    const hl_host_services *host = hl_linux_socket_services();
    hl_host_handle handle;
    hl_host_result result;
    if (host == NULL || !hl_linux_socket_slot_of(descriptor, &handle)) {
        errno = host == NULL ? ENOSYS : ENOTSOCK;
        return -1;
    }
    result = host->network->readiness(host->context, handle, interests);
    if (result.status != HL_STATUS_OK) return hl_linux_socket_fail(result);
    if (out != NULL) *out = (uint32_t)result.value;
    if (pending != NULL) *pending = result.detail;
    return 0;
}

static inline int hl_linux_socket_readiness(int descriptor, uint32_t interests, uint32_t *out) {
    return hl_linux_socket_readiness_and_pending(descriptor, interests, out, NULL);
}

/* execve: drop every socket the guest marked close-on-exec. */
static inline int hl_linux_socket_release_cloexec(void) {
    int released = 0;
    int descriptor;
    for (descriptor = 0; descriptor < HL_LINUX_SOCKET_MAX; ++descriptor) {
        if (!hl_linux_socket_is(descriptor)) continue;
        if ((atomic_load(&hl_linux_socket_table[descriptor].flags) & HL_LINUX_SOCKET_CLOEXEC) == 0) continue;
        (void)hl_linux_socket_close(descriptor);
        released++;
    }
    return released;
}

/* =========================================================================
 * REFUSAL -- <netdb.h>.  A different currency, the same honesty problem.
 *
 * getaddrinfo answers EAI_SYSTEM and leaves the detail in errno.  That is the
 * only EAI_* code which asserts nothing about the NAME, and the choice is
 * forced by a caller in this tree: container/netns.c's DNS responder turns
 * EAI_NONAME into a DNS NXDOMAIN -- "no host by this name exists anywhere" --
 * and serves it to the guest, which will cache it.  A resolver that is merely
 * absent must never be able to make that claim about a name it never looked
 * up.  EAI_AGAIN is the neighbouring trap: it means "try again shortly", and a
 * guest that believes it retries forever.
 * ========================================================================= */

static inline int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints,
                              struct addrinfo **result) {
    (void)node;
    (void)service;
    (void)hints;
    if (result != NULL) *result = NULL;
    errno = ENOSYS;
    return EAI_SYSTEM;
}

/* Not a refusal and not a fake success -- a no-op that is exactly correct.
 * Nothing above ever allocates a list, so there is never anything to release,
 * and a caller's unconditional freeaddrinfo(res) on its failure path stays
 * right. */
static inline void freeaddrinfo(struct addrinfo *result) {
    (void)result;
}

static inline int getnameinfo(const struct sockaddr *address, socklen_t address_length, char *host, socklen_t host_size,
                              char *service, socklen_t service_size, int flags) {
    (void)address;
    (void)address_length;
    (void)host;
    (void)host_size;
    (void)service;
    (void)service_size;
    (void)flags;
    errno = ENOSYS;
    return EAI_SYSTEM;
}

/* gai_strerror is the one function here with no failure channel at all: it
 * turns a code into prose and cannot itself fail.  Returning NULL would hand a
 * caller a pointer it passes straight to printf("%s"), so it returns a
 * description instead.  That is not a faked success -- there is no success to
 * fake. */
static inline const char *gai_strerror(int code) {
    switch (code) {
    case 0: return "Success";
    case EAI_SYSTEM: return "System error (see errno)";
    case EAI_NONAME: return "Name or service not known";
    case EAI_AGAIN: return "Temporary failure in name resolution";
    case EAI_FAIL: return "Non-recoverable failure in name resolution";
    case EAI_FAMILY: return "Address family not supported";
    case EAI_SOCKTYPE: return "Socket type not supported";
    case EAI_SERVICE: return "Service not supported for this socket type";
    case EAI_MEMORY: return "Memory allocation failure";
    case EAI_BADFLAGS: return "Bad value for ai_flags";
    default: return "Unknown name resolution error";
    }
}

static inline struct hostent *gethostbyname(const char *name) {
    (void)name;
    errno = ENOSYS;
    return NULL;
}

static inline struct hostent *gethostbyaddr(const void *address, socklen_t length, int family) {
    (void)address;
    (void)length;
    (void)family;
    errno = ENOSYS;
    return NULL;
}

static inline struct servent *getservbyname(const char *name, const char *protocol) {
    (void)name;
    (void)protocol;
    errno = ENOSYS;
    return NULL;
}

static inline struct servent *getservbyport(int port, const char *protocol) {
    (void)port;
    (void)protocol;
    errno = ENOSYS;
    return NULL;
}

#endif /* _WIN32 */

#endif
