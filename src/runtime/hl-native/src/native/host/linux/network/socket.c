static hl_host_result hl_linux_network_socket(void *context, uint32_t family, uint32_t type, uint32_t protocol) {
    hl_host_linux *host = context;
    int native_family;
    int native_type;
    int descriptor;
    if (family == HL_HOST_NETWORK_IPV4)
        native_family = AF_INET;
    else if (family == HL_HOST_NETWORK_IPV6)
        native_family = AF_INET6;
    else if (family == HL_HOST_NETWORK_LOCAL)
        native_family = AF_UNIX;
    else
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (type == HL_HOST_NETWORK_STREAM)
        native_type = SOCK_STREAM;
    else if (type == HL_HOST_NETWORK_DATAGRAM)
        native_type = SOCK_DGRAM;
    else
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = socket(native_family, native_type | SOCK_CLOEXEC, (int)protocol);
    if (descriptor < 0) return hl_linux_errno_result();
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, descriptor, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(descriptor);
    return result;
}

static int hl_linux_socket_descriptor(hl_host_linux *host, hl_host_handle socket_handle) {
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, socket_handle, HL_LINUX_HANDLE_SOCKET, HL_LINUX_HANDLE_SOCKET);
    pthread_mutex_unlock(&host->lock);
    return descriptor;
}

static hl_status hl_linux_network_address(const hl_host_network_address *address, struct sockaddr_storage *storage,
                                          socklen_t *size) {
    memset(storage, 0, sizeof(*storage));
    if (address == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (address->family == HL_HOST_NETWORK_IPV4 && address->size == 4) {
        struct sockaddr_in *ipv4 = (struct sockaddr_in *)storage;
        ipv4->sin_family = AF_INET;
        ipv4->sin_port = htons(address->port);
        memcpy(&ipv4->sin_addr, address->address, 4);
        *size = sizeof(*ipv4);
        return HL_STATUS_OK;
    }
    if (address->family == HL_HOST_NETWORK_IPV6 && address->size == 16) {
        struct sockaddr_in6 *ipv6 = (struct sockaddr_in6 *)storage;
        ipv6->sin6_family = AF_INET6;
        ipv6->sin6_port = htons(address->port);
        memcpy(&ipv6->sin6_addr, address->address, 16);
        *size = sizeof(*ipv6);
        return HL_STATUS_OK;
    }
    if (address->family == HL_HOST_NETWORK_LOCAL && address->size > 0 && address->size < sizeof(address->local_path)) {
        struct sockaddr_un *local = (struct sockaddr_un *)storage;
        local->sun_family = AF_UNIX;
        memcpy(local->sun_path, address->local_path, address->size);
        local->sun_path[address->size] = '\0';
        *size = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + address->size + 1u);
        return HL_STATUS_OK;
    }
    return HL_STATUS_INVALID_ARGUMENT;
}

static hl_host_result hl_linux_network_bind(void *context, hl_host_handle socket_handle,
                                            const hl_host_network_address *address) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    socklen_t size;
    hl_status status = hl_linux_network_address(address, &storage, &size);
    if (descriptor < 0 || status != HL_STATUS_OK) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return bind(descriptor, (const struct sockaddr *)&storage, size) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                          : hl_linux_errno_result();
}

static hl_host_result hl_linux_network_connect(void *context, hl_host_handle socket_handle,
                                               const hl_host_network_address *address) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    socklen_t size;
    hl_status status = hl_linux_network_address(address, &storage, &size);
    if (descriptor < 0 || status != HL_STATUS_OK) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return connect(descriptor, (const struct sockaddr *)&storage, size) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                             : hl_linux_errno_result();
}

/* Defined with the rest of the ABI 2 arm below; declared here because the two
 * transfer callbacks predate it and must not keep casting the contract's flag
 * word to a native one. The two words are disjoint sets of bits and always
 * were: the contract's MSG_DONT_WAIT is bit 2 and Linux's is bit 6, so the old
 * cast asked for MSG_DONTROUTE whenever a caller asked not to block. */
static uint32_t hl_linux_network_flags(uint32_t flags);
static hl_host_result hl_linux_network_error(void);

static hl_host_result hl_linux_network_send(void *context, hl_host_handle socket_handle, hl_host_const_bytes data,
                                            uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    ssize_t count;
    if (descriptor < 0 || (data.size != 0 && data.data == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = send(descriptor, data.data, data.size, (int)hl_linux_network_flags(flags));
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_receive(void *context, hl_host_handle socket_handle, hl_host_bytes data,
                                               uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    ssize_t count;
    if (descriptor < 0 || (data.size != 0 && data.data == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = recv(descriptor, data.data, data.size, (int)hl_linux_network_flags(flags));
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_network_error();
}

/* --- network, ABI 2 --------------------------------------------------------
 *
 * The Linux arm of the socket contract. Everything here is a thin translation
 * because the host IS the guest's own kernel; the value of the layer on this
 * host is not that it hides a difference but that it names one interface both
 * hosts can be held to. Two things are still translated rather than passed
 * through, and both are defects the previous six-callback group had:
 *
 *   - the transfer FLAGS word. It used to be cast straight to int, so a caller
 *     that had translated its guest flags into the contract's would have set
 *     MSG_OOB where it meant MSG_PEEK. Contract bits and Linux bits do not
 *     agree and were never specified to.
 *   - the PORT. hl_host_network_address carries host byte order on both sides
 *     of the seam, and the provider owns the swap.
 */

static uint32_t hl_linux_network_flags(uint32_t flags) {
    uint32_t native = 0;
    if ((flags & HL_HOST_MSG_PEEK) != 0) native |= MSG_PEEK;
    if ((flags & HL_HOST_MSG_OUT_OF_BAND) != 0) native |= MSG_OOB;
    if ((flags & HL_HOST_MSG_DONT_WAIT) != 0) native |= MSG_DONTWAIT;
    if ((flags & HL_HOST_MSG_WAIT_ALL) != 0) native |= MSG_WAITALL;
    if ((flags & HL_HOST_MSG_DONT_ROUTE) != 0) native |= MSG_DONTROUTE;
    if ((flags & HL_HOST_MSG_NO_SIGNAL) != 0) native |= MSG_NOSIGNAL;
    if ((flags & HL_HOST_MSG_END_OF_RECORD) != 0) native |= MSG_EOR;
    if ((flags & HL_HOST_MSG_MORE) != 0) native |= MSG_MORE;
    return native;
}

static uint32_t hl_linux_network_flags_back(int flags) {
    uint32_t out = 0;
    if ((flags & MSG_TRUNC) != 0) out |= HL_HOST_MSG_TRUNCATED;
    if ((flags & MSG_CTRUNC) != 0) out |= HL_HOST_MSG_CONTROL_TRUNCATED;
    if ((flags & MSG_OOB) != 0) out |= HL_HOST_MSG_RECEIVED_OUT_OF_BAND;
    if ((flags & MSG_EOR) != 0) out |= HL_HOST_MSG_RECEIVED_END_OF_RECORD;
    return out;
}

/* The neutral condition that goes with an errno, so a caller gets the precise
 * answer without the host's own numbering. Zero means "the coarse status says
 * everything there is to say". */
static uint32_t hl_linux_network_condition(int code) {
    switch (code) {
    case EAGAIN: return HL_HOST_NETWORK_CONDITION_WOULD_BLOCK;
    case EINPROGRESS: return HL_HOST_NETWORK_CONDITION_CONNECT_IN_PROGRESS;
    case EALREADY: return HL_HOST_NETWORK_CONDITION_CONNECT_PENDING;
    case EISCONN: return HL_HOST_NETWORK_CONDITION_ALREADY_CONNECTED;
    case ENOTCONN: return HL_HOST_NETWORK_CONDITION_NOT_CONNECTED;
    case EADDRINUSE: return HL_HOST_NETWORK_CONDITION_ADDRESS_IN_USE;
    case EADDRNOTAVAIL: return HL_HOST_NETWORK_CONDITION_ADDRESS_NOT_AVAILABLE;
    case ECONNREFUSED: return HL_HOST_NETWORK_CONDITION_CONNECTION_REFUSED;
    case ECONNRESET: return HL_HOST_NETWORK_CONDITION_CONNECTION_RESET;
    case ECONNABORTED: return HL_HOST_NETWORK_CONDITION_CONNECTION_ABORTED;
    case EDESTADDRREQ: return HL_HOST_NETWORK_CONDITION_DESTINATION_REQUIRED;
    case EMSGSIZE: return HL_HOST_NETWORK_CONDITION_MESSAGE_TOO_LARGE;
    case EAFNOSUPPORT: return HL_HOST_NETWORK_CONDITION_FAMILY_NOT_SUPPORTED;
    case EPROTONOSUPPORT: return HL_HOST_NETWORK_CONDITION_PROTOCOL_NOT_SUPPORTED;
    case ESOCKTNOSUPPORT: return HL_HOST_NETWORK_CONDITION_TYPE_NOT_SUPPORTED;
    case ENOPROTOOPT: return HL_HOST_NETWORK_CONDITION_OPTION_NOT_SUPPORTED;
    case EPROTOTYPE: return HL_HOST_NETWORK_CONDITION_WRONG_PROTOCOL;
    case ENOTSOCK: return HL_HOST_NETWORK_CONDITION_NOT_A_SOCKET;
    case EHOSTUNREACH: return HL_HOST_NETWORK_CONDITION_HOST_UNREACHABLE;
    case ENETUNREACH: return HL_HOST_NETWORK_CONDITION_NETWORK_UNREACHABLE;
    case ENETDOWN: return HL_HOST_NETWORK_CONDITION_NETWORK_DOWN;
    case ENETRESET: return HL_HOST_NETWORK_CONDITION_NETWORK_RESET;
    case ENOBUFS: return HL_HOST_NETWORK_CONDITION_BUFFER_EXHAUSTED;
    case ESHUTDOWN: return HL_HOST_NETWORK_CONDITION_SHUT_DOWN;
    case EPIPE: return HL_HOST_NETWORK_CONDITION_BROKEN_PIPE;
    case EOPNOTSUPP: return HL_HOST_NETWORK_CONDITION_OPERATION_NOT_SUPPORTED;
    case ETIMEDOUT: return HL_HOST_NETWORK_CONDITION_TIMED_OUT;
    case EINTR: return HL_HOST_NETWORK_CONDITION_INTERRUPTED;
    default: return HL_HOST_NETWORK_CONDITION_NONE;
    }
}

static hl_host_result hl_linux_network_error(void) {
    const int code = errno;
    const uint32_t condition = hl_linux_network_condition(code);
    hl_host_result result = hl_linux_errno_result();
    if (condition != HL_HOST_NETWORK_CONDITION_NONE) {
        result.detail_domain = HL_HOST_DETAIL_NETWORK;
        result.detail = condition;
    }
    return result;
}

static hl_status hl_linux_network_decode(const struct sockaddr_storage *storage, socklen_t size,
                                         hl_host_network_address *out) {
    memset(out, 0, sizeof(*out));
    if (size < (socklen_t)sizeof(sa_family_t)) return HL_STATUS_INVALID_ARGUMENT;
    if (storage->ss_family == AF_INET && size >= (socklen_t)sizeof(struct sockaddr_in)) {
        const struct sockaddr_in *ipv4 = (const struct sockaddr_in *)storage;
        out->family = HL_HOST_NETWORK_IPV4;
        out->port = ntohs(ipv4->sin_port);
        out->size = 4;
        memcpy(out->address, &ipv4->sin_addr, 4);
        return HL_STATUS_OK;
    }
    if (storage->ss_family == AF_INET6 && size >= (socklen_t)sizeof(struct sockaddr_in6)) {
        const struct sockaddr_in6 *ipv6 = (const struct sockaddr_in6 *)storage;
        out->family = HL_HOST_NETWORK_IPV6;
        out->port = ntohs(ipv6->sin6_port);
        out->size = 16;
        memcpy(out->address, &ipv6->sin6_addr, 16);
        out->scope_id = ipv6->sin6_scope_id;
        out->flow_info = ipv6->sin6_flowinfo;
        return HL_STATUS_OK;
    }
    if (storage->ss_family == AF_UNIX) {
        const struct sockaddr_un *local = (const struct sockaddr_un *)storage;
        size_t capacity = size > (socklen_t)offsetof(struct sockaddr_un, sun_path)
                              ? (size_t)size - offsetof(struct sockaddr_un, sun_path)
                              : 0;
        size_t length = 0;
        if (capacity > sizeof(local->sun_path)) capacity = sizeof(local->sun_path);
        while (length < capacity && local->sun_path[length] != '\0')
            length++;
        out->family = HL_HOST_NETWORK_LOCAL;
        out->size = (uint16_t)length;
        if (length != 0) memcpy(out->local_path, local->sun_path, length);
        return HL_STATUS_OK;
    }
    return HL_STATUS_NOT_SUPPORTED;
}

static int hl_linux_network_type_native(uint32_t type) {
    switch (type) {
    case HL_HOST_NETWORK_STREAM: return SOCK_STREAM;
    case HL_HOST_NETWORK_DATAGRAM: return SOCK_DGRAM;
    case HL_HOST_NETWORK_SEQPACKET: return SOCK_SEQPACKET;
    case HL_HOST_NETWORK_RAW: return SOCK_RAW;
    default: return -1;
    }
}

static int hl_linux_network_family_native(uint32_t family) {
    switch (family) {
    case HL_HOST_NETWORK_IPV4: return AF_INET;
    case HL_HOST_NETWORK_IPV6: return AF_INET6;
    case HL_HOST_NETWORK_LOCAL: return AF_UNIX;
    default: return -1;
    }
}

static hl_host_result hl_linux_network_listen(void *context, hl_host_handle socket_handle, uint32_t backlog) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (backlog > (uint32_t)INT_MAX) backlog = (uint32_t)INT_MAX;
    return listen(descriptor, (int)backlog) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_accept(void *context, hl_host_handle socket_handle,
                                              hl_host_network_address *peer, uint32_t flags) {
    hl_host_linux *host = context;
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    socklen_t size = (socklen_t)sizeof(storage);
    hl_host_result result;
    int accepted;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&storage, 0, sizeof(storage));
    accepted = accept4(descriptor, (struct sockaddr *)&storage, &size,
                       SOCK_CLOEXEC | ((flags & HL_HOST_SOCKET_NONBLOCK) != 0 ? SOCK_NONBLOCK : 0));
    if (accepted < 0) return hl_linux_network_error();
    if (peer != NULL && hl_linux_network_decode(&storage, size, peer) != HL_STATUS_OK) {
        memset(peer, 0, sizeof(*peer));
        peer->family = HL_HOST_NETWORK_LOCAL;
    }
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, accepted, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(accepted);
    return result;
}

static hl_host_result hl_linux_network_pair(void *context, uint32_t family, uint32_t type, uint32_t protocol,
                                            hl_host_handle ends[2]) {
    hl_host_linux *host = context;
    int native_family = hl_linux_network_family_native(family);
    int native_type = hl_linux_network_type_native(type);
    int descriptors[2];
    hl_host_result first;
    hl_host_result second;
    if (ends == NULL || native_family < 0 || native_type < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (socketpair(native_family, native_type | SOCK_CLOEXEC, (int)protocol, descriptors) != 0)
        return hl_linux_network_error();
    first = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, descriptors[0], NULL, NULL, 0, -1);
    if (first.status != HL_STATUS_OK) {
        close(descriptors[0]);
        close(descriptors[1]);
        return first;
    }
    second = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, descriptors[1], NULL, NULL, 0, -1);
    if (second.status != HL_STATUS_OK) {
        (void)hl_linux_network_close(host, first.value);
        close(descriptors[1]);
        return second;
    }
    ends[0] = first.value;
    ends[1] = second.value;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_network_shutdown(void *context, hl_host_handle socket_handle, uint32_t direction) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    int native;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    switch (direction) {
    case HL_HOST_SHUTDOWN_READ: native = SHUT_RD; break;
    case HL_HOST_SHUTDOWN_WRITE: native = SHUT_WR; break;
    case HL_HOST_SHUTDOWN_BOTH: native = SHUT_RDWR; break;
    default: return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    return shutdown(descriptor, native) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_name(void *context, hl_host_handle socket_handle,
                                            hl_host_network_address *address, int peer) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    socklen_t size = (socklen_t)sizeof(storage);
    hl_status status;
    if (descriptor < 0 || address == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&storage, 0, sizeof(storage));
    if ((peer ? getpeername(descriptor, (struct sockaddr *)&storage, &size)
              : getsockname(descriptor, (struct sockaddr *)&storage, &size)) != 0)
        return hl_linux_network_error();
    status = hl_linux_network_decode(&storage, size, address);
    return status == HL_STATUS_OK ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_result(status, 0, 0);
}

static hl_host_result hl_linux_network_local_address(void *context, hl_host_handle socket_handle,
                                                     hl_host_network_address *address) {
    return hl_linux_network_name(context, socket_handle, address, 0);
}

static hl_host_result hl_linux_network_peer_address(void *context, hl_host_handle socket_handle,
                                                    hl_host_network_address *address) {
    return hl_linux_network_name(context, socket_handle, address, 1);
}

/* The flat neutral option -> (level, name). One table, and no path from a
 * caller's number to a native number that does not pass through it -- which is
 * the property the flat enum exists to give, and the reason there is no
 * `default: return option;` anywhere below. */
typedef struct hl_linux_network_option {
    uint32_t option;
    int level;
    int name;
} hl_linux_network_option;

static const hl_linux_network_option hl_linux_network_options[] = {
    {HL_HOST_SOCKOPT_REUSE_ADDRESS, SOL_SOCKET, SO_REUSEADDR},
    {HL_HOST_SOCKOPT_REUSE_PORT, SOL_SOCKET, SO_REUSEPORT},
    {HL_HOST_SOCKOPT_KEEP_ALIVE, SOL_SOCKET, SO_KEEPALIVE},
    {HL_HOST_SOCKOPT_BROADCAST, SOL_SOCKET, SO_BROADCAST},
    {HL_HOST_SOCKOPT_DONT_ROUTE, SOL_SOCKET, SO_DONTROUTE},
    {HL_HOST_SOCKOPT_OUT_OF_BAND_INLINE, SOL_SOCKET, SO_OOBINLINE},
    {HL_HOST_SOCKOPT_SEND_BUFFER, SOL_SOCKET, SO_SNDBUF},
    {HL_HOST_SOCKOPT_RECEIVE_BUFFER, SOL_SOCKET, SO_RCVBUF},
    {HL_HOST_SOCKOPT_SEND_LOW_WATER, SOL_SOCKET, SO_SNDLOWAT},
    {HL_HOST_SOCKOPT_RECEIVE_LOW_WATER, SOL_SOCKET, SO_RCVLOWAT},
    {HL_HOST_SOCKOPT_TYPE, SOL_SOCKET, SO_TYPE},
    {HL_HOST_SOCKOPT_PROTOCOL, SOL_SOCKET, SO_PROTOCOL},
    {HL_HOST_SOCKOPT_DOMAIN, SOL_SOCKET, SO_DOMAIN},
    {HL_HOST_SOCKOPT_ACCEPT_CONNECTIONS, SOL_SOCKET, SO_ACCEPTCONN},
    {HL_HOST_SOCKOPT_PASS_CREDENTIALS, SOL_SOCKET, SO_PASSCRED},
    {HL_HOST_SOCKOPT_TCP_NO_DELAY, IPPROTO_TCP, TCP_NODELAY},
    {HL_HOST_SOCKOPT_TCP_KEEP_IDLE, IPPROTO_TCP, TCP_KEEPIDLE},
    {HL_HOST_SOCKOPT_TCP_KEEP_INTERVAL, IPPROTO_TCP, TCP_KEEPINTVL},
    {HL_HOST_SOCKOPT_TCP_KEEP_COUNT, IPPROTO_TCP, TCP_KEEPCNT},
    {HL_HOST_SOCKOPT_TCP_MAX_SEGMENT, IPPROTO_TCP, TCP_MAXSEG},
    {HL_HOST_SOCKOPT_TCP_CORK, IPPROTO_TCP, TCP_CORK},
    {HL_HOST_SOCKOPT_TCP_QUICK_ACK, IPPROTO_TCP, TCP_QUICKACK},
    {HL_HOST_SOCKOPT_TCP_USER_TIMEOUT, IPPROTO_TCP, TCP_USER_TIMEOUT},
    {HL_HOST_SOCKOPT_IP_TIME_TO_LIVE, IPPROTO_IP, IP_TTL},
    {HL_HOST_SOCKOPT_IP_TYPE_OF_SERVICE, IPPROTO_IP, IP_TOS},
    {HL_HOST_SOCKOPT_IP_HEADER_INCLUDED, IPPROTO_IP, IP_HDRINCL},
    {HL_HOST_SOCKOPT_IP_MULTICAST_TTL, IPPROTO_IP, IP_MULTICAST_TTL},
    {HL_HOST_SOCKOPT_IP_MULTICAST_LOOP, IPPROTO_IP, IP_MULTICAST_LOOP},
    {HL_HOST_SOCKOPT_IP_PACKET_INFO, IPPROTO_IP, IP_PKTINFO},
    {HL_HOST_SOCKOPT_IPV6_ONLY, IPPROTO_IPV6, IPV6_V6ONLY},
    {HL_HOST_SOCKOPT_IPV6_UNICAST_HOPS, IPPROTO_IPV6, IPV6_UNICAST_HOPS},
    {HL_HOST_SOCKOPT_IPV6_MULTICAST_HOPS, IPPROTO_IPV6, IPV6_MULTICAST_HOPS},
    {HL_HOST_SOCKOPT_IPV6_MULTICAST_LOOP, IPPROTO_IPV6, IPV6_MULTICAST_LOOP},
    {HL_HOST_SOCKOPT_IPV6_PACKET_INFO, IPPROTO_IPV6, IPV6_RECVPKTINFO}};

static const hl_linux_network_option *hl_linux_network_option_find(uint32_t option) {
    size_t index;
    for (index = 0; index < sizeof(hl_linux_network_options) / sizeof(hl_linux_network_options[0]); ++index)
        if (hl_linux_network_options[index].option == option) return &hl_linux_network_options[index];
    return NULL;
}

static hl_host_result hl_linux_network_get_option(void *context, hl_host_handle socket_handle, uint32_t option,
                                                  hl_host_bytes value) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    const hl_linux_network_option *entry;
    if (descriptor < 0 || value.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (option == HL_HOST_SOCKOPT_ERROR) {
        /* Reported as an hl_status and never as an errno, so a caller need not
         * know whose numbering it is holding. The host number stays in detail. */
        int code = 0;
        socklen_t size = (socklen_t)sizeof(code);
        uint32_t status;
        if (value.size < sizeof(uint32_t)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        if (getsockopt(descriptor, SOL_SOCKET, SO_ERROR, &code, &size) != 0) return hl_linux_network_error();
        status = code == 0 ? (uint32_t)HL_STATUS_OK : (uint32_t)hl_linux_status_from_errno(code);
        memcpy(value.data, &status, sizeof(status));
        return (hl_host_result){(int32_t)HL_STATUS_OK, HL_HOST_DETAIL_ERRNO, sizeof(status), (uint64_t)(uint32_t)code};
    }
    if (option == HL_HOST_SOCKOPT_LINGER) {
        struct linger native;
        hl_host_network_linger out;
        socklen_t size = (socklen_t)sizeof(native);
        if (value.size < sizeof(out)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memset(&native, 0, sizeof(native));
        if (getsockopt(descriptor, SOL_SOCKET, SO_LINGER, &native, &size) != 0) return hl_linux_network_error();
        out.enabled = native.l_onoff != 0 ? 1u : 0u;
        out.seconds = (uint32_t)(native.l_linger < 0 ? 0 : native.l_linger);
        memcpy(value.data, &out, sizeof(out));
        return hl_linux_result(HL_STATUS_OK, sizeof(out), 0);
    }
    if (option == HL_HOST_SOCKOPT_SEND_TIMEOUT || option == HL_HOST_SOCKOPT_RECEIVE_TIMEOUT) {
        struct timeval native;
        uint64_t nanoseconds;
        socklen_t size = (socklen_t)sizeof(native);
        if (value.size < sizeof(nanoseconds)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memset(&native, 0, sizeof(native));
        if (getsockopt(descriptor, SOL_SOCKET, option == HL_HOST_SOCKOPT_SEND_TIMEOUT ? SO_SNDTIMEO : SO_RCVTIMEO,
                       &native, &size) != 0)
            return hl_linux_network_error();
        nanoseconds = (uint64_t)native.tv_sec * UINT64_C(1000000000) + (uint64_t)native.tv_usec * UINT64_C(1000);
        memcpy(value.data, &nanoseconds, sizeof(nanoseconds));
        return hl_linux_result(HL_STATUS_OK, sizeof(nanoseconds), 0);
    }
    if (option == HL_HOST_SOCKOPT_PEER_CREDENTIALS) {
        struct ucred native;
        hl_host_network_credentials out;
        socklen_t size = (socklen_t)sizeof(native);
        if (value.size < sizeof(out)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memset(&native, 0, sizeof(native));
        memset(&out, 0, sizeof(out));
        if (getsockopt(descriptor, SOL_SOCKET, SO_PEERCRED, &native, &size) != 0) return hl_linux_network_error();
        out.process = (int32_t)native.pid;
        out.user = (uint32_t)native.uid;
        out.group = (uint32_t)native.gid;
        memcpy(value.data, &out, sizeof(out));
        return hl_linux_result(HL_STATUS_OK, sizeof(out), 0);
    }
    entry = hl_linux_network_option_find(option);
    if (entry == NULL) return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    {
        int scalar = 0;
        uint32_t out;
        socklen_t size = (socklen_t)sizeof(scalar);
        if (value.size < sizeof(uint32_t)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        if (getsockopt(descriptor, entry->level, entry->name, &scalar, &size) != 0) return hl_linux_network_error();
        if (option == HL_HOST_SOCKOPT_TYPE) {
            /* SO_TYPE answers in the HOST's numbering; the caller asked in the
             * contract's, and those are two different questions on some host. */
            out = scalar == SOCK_STREAM      ? HL_HOST_NETWORK_STREAM
                  : scalar == SOCK_DGRAM     ? HL_HOST_NETWORK_DATAGRAM
                  : scalar == SOCK_SEQPACKET ? HL_HOST_NETWORK_SEQPACKET
                  : scalar == SOCK_RAW       ? HL_HOST_NETWORK_RAW
                                             : 0u;
        } else if (option == HL_HOST_SOCKOPT_DOMAIN) {
            out = scalar == AF_INET    ? HL_HOST_NETWORK_IPV4
                  : scalar == AF_INET6 ? HL_HOST_NETWORK_IPV6
                  : scalar == AF_UNIX  ? HL_HOST_NETWORK_LOCAL
                                       : 0u;
        } else {
            out = (uint32_t)scalar;
        }
        memcpy(value.data, &out, sizeof(out));
        return hl_linux_result(HL_STATUS_OK, sizeof(out), 0);
    }
}

static hl_host_result hl_linux_network_set_option(void *context, hl_host_handle socket_handle, uint32_t option,
                                                  const hl_host_const_bytes value) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    const hl_linux_network_option *entry;
    if (descriptor < 0 || value.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (option == HL_HOST_SOCKOPT_LINGER) {
        hl_host_network_linger in;
        struct linger native;
        if (value.size < sizeof(in)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memcpy(&in, value.data, sizeof(in));
        native.l_onoff = in.enabled != 0 ? 1 : 0;
        native.l_linger = (int)in.seconds;
        return setsockopt(descriptor, SOL_SOCKET, SO_LINGER, &native, (socklen_t)sizeof(native)) == 0
                   ? hl_linux_result(HL_STATUS_OK, 0, 0)
                   : hl_linux_network_error();
    }
    if (option == HL_HOST_SOCKOPT_SEND_TIMEOUT || option == HL_HOST_SOCKOPT_RECEIVE_TIMEOUT) {
        uint64_t nanoseconds;
        struct timeval native;
        if (value.size < sizeof(nanoseconds)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memcpy(&nanoseconds, value.data, sizeof(nanoseconds));
        native.tv_sec = (time_t)(nanoseconds / UINT64_C(1000000000));
        native.tv_usec = (suseconds_t)((nanoseconds % UINT64_C(1000000000)) / UINT64_C(1000));
        return setsockopt(descriptor, SOL_SOCKET, option == HL_HOST_SOCKOPT_SEND_TIMEOUT ? SO_SNDTIMEO : SO_RCVTIMEO,
                          &native, (socklen_t)sizeof(native)) == 0
                   ? hl_linux_result(HL_STATUS_OK, 0, 0)
                   : hl_linux_network_error();
    }
    if (option == HL_HOST_SOCKOPT_ERROR || option == HL_HOST_SOCKOPT_TYPE || option == HL_HOST_SOCKOPT_DOMAIN ||
        option == HL_HOST_SOCKOPT_PROTOCOL || option == HL_HOST_SOCKOPT_ACCEPT_CONNECTIONS ||
        option == HL_HOST_SOCKOPT_PEER_CREDENTIALS)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    entry = hl_linux_network_option_find(option);
    if (entry == NULL) return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    {
        uint32_t scalar;
        int native;
        if (value.size < sizeof(scalar)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memcpy(&scalar, value.data, sizeof(scalar));
        native = (int)scalar;
        return setsockopt(descriptor, entry->level, entry->name, &native, (socklen_t)sizeof(native)) == 0
                   ? hl_linux_result(HL_STATUS_OK, 0, 0)
                   : hl_linux_network_error();
    }
}

enum { HL_LINUX_NETWORK_IOV_MAX = 64 };

static hl_host_result hl_linux_network_send_message(void *context, hl_host_handle socket_handle,
                                                    const hl_host_network_message *message, uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    struct iovec vectors[HL_LINUX_NETWORK_IOV_MAX];
    struct msghdr header;
    socklen_t size = 0;
    uint32_t index;
    ssize_t sent;
    if (descriptor < 0 || message == NULL || message->buffer_count > HL_LINUX_NETWORK_IOV_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (message->buffer_count != 0 && message->buffers == NULL)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (index = 0; index < message->buffer_count; ++index) {
        vectors[index].iov_base = (void *)(uintptr_t)message->buffers[index].address;
        vectors[index].iov_len = (size_t)message->buffers[index].size;
    }
    memset(&header, 0, sizeof(header));
    header.msg_iov = vectors;
    header.msg_iovlen = message->buffer_count;
    if (message->address != NULL) {
        if (hl_linux_network_address(message->address, &storage, &size) != HL_STATUS_OK)
            return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        header.msg_name = &storage;
        header.msg_namelen = size;
    }
    /* Ancillary data has no encoding on this seam yet. Refused rather than
     * dropped: a caller that passed control bytes and got a success would
     * believe the peer received them. */
    if (message->control != NULL && message->control_size != 0) return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    sent = sendmsg(descriptor, &header, (int)hl_linux_network_flags(flags));
    return sent >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)sent, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_receive_message(void *context, hl_host_handle socket_handle,
                                                       hl_host_network_message *message, uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    struct iovec vectors[HL_LINUX_NETWORK_IOV_MAX];
    struct msghdr header;
    uint32_t index;
    ssize_t received;
    if (descriptor < 0 || message == NULL || message->buffer_count > HL_LINUX_NETWORK_IOV_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (message->buffer_count != 0 && message->buffers == NULL)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (index = 0; index < message->buffer_count; ++index) {
        vectors[index].iov_base = (void *)(uintptr_t)message->buffers[index].address;
        vectors[index].iov_len = (size_t)message->buffers[index].size;
    }
    memset(&storage, 0, sizeof(storage));
    memset(&header, 0, sizeof(header));
    header.msg_iov = vectors;
    header.msg_iovlen = message->buffer_count;
    if (message->address != NULL) {
        header.msg_name = &storage;
        header.msg_namelen = (socklen_t)sizeof(storage);
    }
    received = recvmsg(descriptor, &header, (int)hl_linux_network_flags(flags));
    if (received < 0) return hl_linux_network_error();
    message->flags = hl_linux_network_flags_back(header.msg_flags);
    message->control_size = 0;
    if (message->address != NULL &&
        hl_linux_network_decode(&storage, header.msg_namelen, message->address) != HL_STATUS_OK)
        memset(message->address, 0, sizeof(*message->address));
    return hl_linux_result(HL_STATUS_OK, (uint64_t)received, 0);
}

static hl_host_result hl_linux_network_readiness(void *context, hl_host_handle socket_handle, uint32_t interests) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct pollfd probe;
    uint32_t ready = 0;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    probe.fd = descriptor;
    probe.events = (short)(POLLIN | POLLOUT | POLLPRI);
    probe.revents = 0;
    if (poll(&probe, 1, 0) < 0) return hl_linux_network_error();
    if ((probe.revents & (POLLIN | POLLHUP)) != 0) ready |= HL_HOST_READY_READ;
    if ((probe.revents & POLLOUT) != 0) ready |= HL_HOST_READY_WRITE;
    if ((probe.revents & POLLERR) != 0) ready |= HL_HOST_READY_ERROR;
    if ((probe.revents & POLLHUP) != 0) ready |= HL_HOST_READY_HANGUP;
    {
        /* The queued byte count rides in detail, so a caller never has to name
         * an ioctl to ask a question readiness already almost answers. */
        int queued = 0;
        uint64_t pending = ioctl(descriptor, FIONREAD, &queued) == 0 && queued > 0 ? (uint64_t)queued : 0;
        return (hl_host_result){(int32_t)HL_STATUS_OK, HL_HOST_DETAIL_NONE,
                                interests == 0 ? ready : (ready & interests), pending};
    }
}

/* A socket here IS a pollable kernel object, so the event group accepts it and
   the answer is yes. */
static hl_host_result hl_linux_network_wait_handle(void *context, hl_host_handle socket_handle) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_linux_result(HL_STATUS_OK, 1, 0);
}

static hl_host_result hl_linux_network_set_status_flags(void *context, hl_host_handle socket_handle, uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    int current;
    if (descriptor < 0 || (flags & ~(uint32_t)HL_HOST_SOCKET_NONBLOCK) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    current = fcntl(descriptor, F_GETFL);
    if (current < 0) return hl_linux_network_error();
    current = (current & ~O_NONBLOCK) | ((flags & HL_HOST_SOCKET_NONBLOCK) != 0 ? O_NONBLOCK : 0);
    return fcntl(descriptor, F_SETFL, current) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_duplicate(void *context, hl_host_handle socket_handle) {
    hl_host_linux *host = context;
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    hl_host_result result;
    int copy;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    copy = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    if (copy < 0) return hl_linux_network_error();
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, copy, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(copy);
    return result;
}

