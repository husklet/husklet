// Extracted from service(): Network -- sockets/bind/connect/accept/send/recv + socketopt; port-map (-p) and
// the private NET-ns loopback (af_l2m / cmsg / msg-flag translation live in container/netns.c). Returns 1 if
// nr was handled, 0 otherwise. Included by service.c after service/io.c, before service() -- same TU scope.

static inline uint64_t net_nonpie_p(uint64_t address) {
    return nonpie_fold(address);
}
// A zero-length datagram receive that asks for the sender address. macOS short-circuits any receive with
// a zero-length buffer (returns 0 at once, filling neither data nor the source address), but Linux blocks
// until a datagram arrives and reports its sender. busybox `nc -u -l` depends on the Linux behaviour: it
// peeks the first datagram's source with a zero-length recvmsg(MSG_PEEK) purely to learn whom to connect()
// its reply back to. To match Linux we receive into a 1-byte host scratch instead, so macOS blocks and
// fills the address; a MSG_PEEK leaves the datagram queued for the guest's real read, and a non-peek
// receive consumes the whole datagram exactly as a zero-length Linux recv would. The guest still sees 0
// bytes (it asked for none). Restricted to datagram/raw sockets -- a zero-length stream recv legitimately
// returns 0 immediately -- so ordinary `recv(fd, buf, 0, 0)` probes are unaffected.
static int dgram_addr_peek(int fd, int wantaddr, size_t totlen) {
    if (!wantaddr || totlen != 0) return 0;
    int ty = 0;
    socklen_t tl = sizeof ty;
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &ty, &tl) < 0) return 0;
    return ty == SOCK_DGRAM || ty == SOCK_RAW;
}

// UDP datagram size limit. A Linux AF_INET/AF_INET6 SOCK_DGRAM send whose payload exceeds the maximum
// single UDP datagram fails EMSGSIZE regardless of destination -- the size check happens before the
// datagram is routed. The cap is 65507 over IPv4 (65535 - 8 UDP header - 20 IP header) and 65535 over
// IPv6. The private-loopback AF_UNIX switch backing has a different (buffer-based) limit, so without
// this gate an oversized UDP send leaks the wrong errno (e.g. ENOENT from the missing switch peer path)
// instead of EMSGSIZE. Returns the EMSGSIZE limit for an INET dgram fd, or 0 for any fd not capped this
// way (g_sock_dgram is set only for AF_INET/AF_INET6 datagram sockets, never AF_UNIX).
static size_t udp_dgram_maxlen(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_dgram[fd]) return 0;
    return (g_sock_fam[fd] == LX_AF_INET6_FAM) ? 65535 : 65507;
}

// IPPROTO_IPV6 optname: Linux -> macOS. CRITICAL: like IPPROTO_TCP, these numbers diverge, so a raw
// pass-through silently sets the WRONG option. The load-bearing case is IPV6_V6ONLY (Linux 26 -> macOS 27):
// leaving it untranslated hits macOS's optname 26 (unrelated) instead, so a wildcard `::` bind stays
// dual-stack on the host and reserves the v4 wildcard too -- a later 0.0.0.0 bind on the same port then
// fails EADDRINUSE (breaks dual-stack servers like MariaDB that bind :: v6-only + 0.0.0.0 separately).
// Map the known ones; ignore (-1) unknown rather than pass a Linux number straight to macOS IPPROTO_IPV6.
static int ip6_opt_l2m(int o) {
#if defined(__linux__) || defined(_WIN32)
    return o;
#else
    switch (o) {
    case 16: return 4;  // IPV6_UNICAST_HOPS
    case 17: return 9;  // IPV6_MULTICAST_IF
    case 18: return 10; // IPV6_MULTICAST_HOPS
    case 19: return 11; // IPV6_MULTICAST_LOOP
    case 20: return 12; // IPV6_ADD_MEMBERSHIP / IPV6_JOIN_GROUP  (struct ipv6_mreq: same layout)
    case 21: return 13; // IPV6_DROP_MEMBERSHIP / IPV6_LEAVE_GROUP
    case 26: return 27; // IPV6_V6ONLY  (the fix)
    case 66: return 35; // IPV6_RECVTCLASS
    case 67: return 36; // IPV6_TCLASS
    default: return -1; // unknown -> ignore (never pass a Linux number straight to macOS IPPROTO_IPV6)
    }
#endif
}

// IPPROTO_IP (level 0) optname: Linux -> macOS. Like TCP/IPV6 the numbers diverge (Linux IP_TOS=1/IP_TTL=2/
// IP_HDRINCL=3 vs macOS IP_OPTIONS=1/IP_HDRINCL=2/IP_TOS=3), so a raw pass-through sets the WRONG option
// (e.g. Linux IP_TTL(2) lands on macOS IP_HDRINCL(2)). Map the options whose macOS payload struct matches
// (int TOS/TTL/HDRINCL/loop/mcast-ttl, in_addr mcast-if, ip_mreq membership); ignore (-1) unknown / no-mac-
// equivalent ones (IP_PKTINFO/IP_MTU_DISCOVER/IP_RECVERR/IP_FREEBIND: no macOS analogue or a divergent struct).
static int ip_opt_l2m(int o) {
#if defined(__linux__) || defined(_WIN32)
    return o;
#else
    switch (o) {
    case 1: return 3;   // IP_TOS
    case 2: return 4;   // IP_TTL
    case 3: return 2;   // IP_HDRINCL
    case 4: return 1;   // IP_OPTIONS
    case 6: return 5;   // IP_RECVOPTS
    case 7: return 8;   // IP_RETOPTS
    case 12: return 24; // IP_RECVTTL
    case 32: return 9;  // IP_MULTICAST_IF   (in_addr; ip_mreqn extras are Linux-only -> best-effort)
    case 33: return 10; // IP_MULTICAST_TTL
    case 34: return 11; // IP_MULTICAST_LOOP
    case 35: return 12; // IP_ADD_MEMBERSHIP  (struct ip_mreq: same layout)
    case 36: return 13; // IP_DROP_MEMBERSHIP
    default: return -1; // unknown / no macOS equivalent -> ignore (never pass a Linux number to macOS IPPROTO_IP)
    }
#endif
}

// an AF_UNIX DATAGRAM send to a PATHNAME/abstract dest (sendto/sendmsg with an explicit dest addr --
// e.g. syslog's `logger` writing to /dev/log) must resolve the dest through the SAME overlay/abstract-ns
// mapping bind/connect use, or macOS looks for the socket inode at the literal host path (outside the jail)
// / the wrong abstract-ns dir and the datagram is silently dropped. Mirrors the connect (case 203) and bind
// (case 200) AF_UNIX handling. Returns 1 + fills `host` when the dest should be overlay/abstract-routed
// (caller sends via unix_dgram_sendmsg_at); 0 otherwise (AF_INET, unnamed, or a non-jail pathname whose raw
// sockaddr already round-trips -> caller sends unchanged, keeping the bare-metal AF_UNIX dgram path intact).
static int unix_path_routed(const char *guest) {
    if (g_rootfs) return 1;
    if (!guest || guest[0] != '/') return 0;
    char normalized[4200];
    confine(guest, normalized, sizeof normalized);
    return jail_match(normalized) >= 0;
}

static int unix_dgram_dest(const uint8_t *sa, socklen_t l, char *host, size_t hn) {
    if (abs_is(sa, l)) { // abstract namespace (sun_path[0]==0): HL_NETNS-keyed fs socket (same as bind/connect)
        abs_path(sa, l, host, hn);
        return 1;
    }
    if (unix_path_is(sa, l)) {
        char gp[200], hb[1024];
        unix_path_copy(sa, l, gp, sizeof gp);
        if (!unix_path_routed(gp)) return 0;
        const char *hp = atpath(-100, gp, hb, sizeof hb, 0); // guest path -> host path (upper then lowers)
        snprintf(host, hn, "%s", hp);
        return 1;
    }
    return 0;
}

// Linux-faithful errno pre-screen for bind(200)/connect(203). macOS hands hl's translated (or raw)
// sockaddr to its own bind()/connect(), which then reports the WRONG errno for several inputs the LTP
// net-errno suite (bind01/connect01) checks — a bad sockaddr pointer, a wrong sa_family, an
// already-connected socket. Replicate the kernel's ORDER + values here, up front, so every path (real
// host, private-lo, bridge, unix) inherits the correct errno:
//   1. fd lookup   -> EBADF (bad fd) / ENOTSOCK (fd is not a socket)          [before the addr is read]
//   2. addr copy   -> EINVAL (addrlen > sockaddr_storage) / EFAULT (unreadable sockaddr buffer)
//   3. proto layer -> EISCONN (connect on a connected stream socket),
//                     EAFNOSUPPORT (sa_family != the socket's family),
//                     EINVAL (addrlen < sizeof(sockaddr_in/in6))              [AF_INET/INET6 sockets only]
// Returns 0 to continue, or a negative *macOS* errno to return now (svc_done_host does the m2l boundary xlate,
// exactly as for a real failed syscall). Family/length checks are gated on the socket actually being
// AF_INET/AF_INET6 (the family recorded at socket()/accept(), see g_sock_fam) so AF_UNIX / AF_NETLINK /
// AF_PACKET bind+connect are untouched.
static int net_precheck(int fd, uintptr_t addr, socklen_t alen, int is_connect) {
    int sotype = 0;
    socklen_t sl = sizeof sotype;
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &sotype, &sl) < 0) return -errno; // EBADF / ENOTSOCK
    if (alen > (socklen_t)sizeof(struct sockaddr_storage)) return -EINVAL;    // move_addr_to_kernel range
    // Unreadable sockaddr -> EFAULT: unmapped OR a guest PROT_NONE page that this DBT force-mapped
    // host-readable. Both are caught by guest_bad_ptr -> host_range_mapped (its internal gna_hit check
    // handles the PROT_NONE case; see thread.c).
    if (addr && guest_bad_ptr(addr, alen)) return -EFAULT;
    int lfam = (addr && alen >= 2) ? *(const uint16_t *)addr : 0; // guest (Linux) sa_family
    // bind() on an already-bound switch-backed socket -> EINVAL. lo_swap()/br mint a fresh AF_UNIX socket
    // per bind, so a real host rebind would spuriously succeed; the recorded virtual port is the bound
    // marker (a plain host-backed bind is rejected by the host itself). AF_UNSPEC never reaches bind here.
    if (!is_connect && fd >= 0 && fd < HL_NFD && (g_lo_port[fd] || g_br_port[fd])) return -EINVAL;
    // connect() on a listening socket -> EISCONN (the kernel rejects it on socket state before the protocol
    // connect, regardless of the destination). The switch model would otherwise dial the destination and
    // surface ECONNREFUSED.
    if (is_connect && fd >= 0 && fd < HL_NFD && g_tcp_listen[fd] && lfam != 0) return -EISCONN;
    // connect() on an already-connected stream socket -> EISCONN (kernel checks the socket state before
    // the protocol connect). AF_UNSPEC is the "dissolve association" idiom and is never EISCONN.
    if (is_connect && sotype == SOCK_STREAM && lfam != 0) {
        struct sockaddr_storage pn;
        socklen_t pnl = sizeof pn;
        int connected = (getpeername(fd, (struct sockaddr *)&pn, &pnl) == 0) ||
                        (fd >= 0 && fd < HL_NFD && g_sock_conn[fd]); // sticky: survives a peer FIN (see decl)
        if (connected) return -EISCONN;
    }
    // The socket's own family: prefer the value recorded at socket()/accept() (robust even after a prior
    // failed connect on this fd); fall back to a getsockname() probe for an untracked (e.g. inherited) fd.
    int sfam = (fd >= 0 && fd < HL_NFD) ? g_sock_fam[fd] : 0;
    if (sfam == 0) {
        struct sockaddr_storage ln;
        socklen_t lnl = sizeof ln;
        if (getsockname(fd, (struct sockaddr *)&ln, &lnl) == 0) {
            if (ln.ss_family == AF_INET)
                sfam = LX_AF_INET;
            else if (ln.ss_family == AF_INET6)
                sfam = LX_AF_INET6;
        }
    }
    if (sfam == LX_AF_INET || sfam == LX_AF_INET6) {
        if (!(is_connect && lfam == 0)) { // AF_UNSPEC connect on an INET socket = disconnect: allow
            socklen_t need = (lfam == LX_AF_INET) ? 16 : 24;
            // The private UDP switch is family-neutral: dual-stack listeners and BusyBox clients may
            // select opposite INET families while still addressing the same loopback endpoint. Preserve
            // strict Linux family validation for every other socket, but allow that narrow datagram pair.
            int private_udp_dual =
                fd >= 0 && fd < HL_NFD && g_sock_dgram[fd] && lo_on() &&
                ((sfam == LX_AF_INET && lfam == LX_AF_INET6) || (sfam == LX_AF_INET6 && lfam == LX_AF_INET));
            if (lfam != sfam && !private_udp_dual) return -EAFNOSUPPORT;
            if (alen < need) return -EINVAL;
        }
    }
    return 0;
}

typedef struct net_sockaddr_copyout {
    struct cpu *cpu;
    uint64_t guest_address;
    uint64_t guest_length;
    socklen_t capacity;
    socklen_t length;
    uint8_t address[sizeof(struct sockaddr_storage)];
    int active;
} net_sockaddr_copyout;

static int net_sockaddr_copyout_begin(net_sockaddr_copyout *copyout, struct cpu *cpu, uint64_t guest_address,
                                      uint64_t guest_length) {
    memset(copyout, 0, sizeof(*copyout));
    copyout->cpu = cpu;
    copyout->guest_address = guest_address;
    copyout->guest_length = guest_length;
    if (!guest_address) return 0;
    if (!guest_length ||
        guest_copy_from(&copyout->capacity, guest_length, sizeof(copyout->capacity)) != sizeof(copyout->capacity))
        return -EFAULT;
    size_t accessible = copyout->capacity;
    if (accessible > sizeof(copyout->address)) accessible = sizeof(copyout->address);
    if (accessible && guest_accessible_prefix(guest_address, accessible, PROT_WRITE) != accessible) return -EFAULT;
    copyout->length = copyout->capacity;
    copyout->active = 1;
    return 0;
}

static void net_sockaddr_copyout_finish(void *opaque) {
    net_sockaddr_copyout *copyout = opaque;
    if (!copyout->active || (int64_t)G_RET(copyout->cpu) < 0) return;
    size_t length = copyout->capacity < copyout->length ? copyout->capacity : copyout->length;
    if (length > sizeof(copyout->address)) length = sizeof(copyout->address);
    if ((length && guest_copy_to(copyout->guest_address, copyout->address, length) != (ssize_t)length) ||
        guest_copy_to(copyout->guest_length, &copyout->length, sizeof(copyout->length)) != sizeof(copyout->length))
        G_RET(copyout->cpu) = (uint64_t)(int64_t)(-EFAULT);
}

typedef struct net_option_copyout {
    struct cpu *cpu;
    uint64_t guest_value;
    uint64_t guest_length;
    socklen_t capacity;
    socklen_t length;
    void *value;
    int active;
} net_option_copyout;

static int net_option_copyout_begin(net_option_copyout *copyout, struct cpu *cpu, uint64_t guest_value,
                                    uint64_t guest_length) {
    memset(copyout, 0, sizeof(*copyout));
    copyout->cpu = cpu;
    copyout->guest_value = guest_value;
    copyout->guest_length = guest_length;
    if (!guest_value) return 0;
    if (!guest_length ||
        guest_copy_from(&copyout->capacity, guest_length, sizeof(copyout->capacity)) != sizeof(copyout->capacity))
        return -EFAULT;
    if (copyout->capacity > 1024 * 1024) return -EINVAL;
    if (copyout->capacity && guest_accessible_prefix(guest_value, copyout->capacity, PROT_WRITE) != copyout->capacity)
        return -EFAULT;
    copyout->value = calloc(copyout->capacity ? copyout->capacity : 1, 1);
    if (!copyout->value) return -ENOMEM;
    copyout->length = copyout->capacity;
    copyout->active = 1;
    return 0;
}

static void net_option_copyout_finish(void *opaque) {
    net_option_copyout *copyout = opaque;
    if (!copyout->active) return;
    if ((int64_t)G_RET(copyout->cpu) >= 0) {
        size_t length = copyout->capacity < copyout->length ? copyout->capacity : copyout->length;
        if ((length && guest_copy_to(copyout->guest_value, copyout->value, length) != (ssize_t)length) ||
            guest_copy_to(copyout->guest_length, &copyout->length, sizeof(copyout->length)) != sizeof(copyout->length))
            G_RET(copyout->cpu) = (uint64_t)(int64_t)(-EFAULT);
    }
    free(copyout->value);
}

typedef struct net_message_bounce {
    struct cpu *cpu;
    uint64_t guest_header;
    uint8_t header[56];
    uint64_t count;
    uint64_t guest_iov_address;
    uint64_t raw_guest_iov_address;
    struct iovec guest_iov[1024];
    struct iovec host_iov[1024];
    uint64_t guest_name;
    uint64_t raw_guest_name;
    uint32_t name_capacity;
    void *name;
    uint64_t guest_control;
    uint64_t raw_guest_control;
    size_t control_capacity;
    void *control;
    int receive;
    int active;
} net_message_bounce;

static void net_message_bounce_finish(void *opaque) {
    net_message_bounce *bounce = opaque;
    if (bounce->active && bounce->receive && (int64_t)G_RET(bounce->cpu) >= 0) {
        size_t remaining = (size_t)G_RET(bounce->cpu);
        for (uint64_t i = 0; i < bounce->count && remaining; ++i) {
            size_t length = bounce->host_iov[i].iov_len < remaining ? bounce->host_iov[i].iov_len : remaining;
            if (length && guest_copy_to((uint64_t)(uintptr_t)bounce->guest_iov[i].iov_base,
                                        bounce->host_iov[i].iov_base, length) != (ssize_t)length) {
                G_RET(bounce->cpu) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            remaining -= length;
        }
        uint32_t returned_name = 0;
        memcpy(&returned_name, bounce->header + 8, sizeof(returned_name));
        size_t name_length = bounce->name_capacity < returned_name ? bounce->name_capacity : returned_name;
        uint64_t returned_control = 0;
        memcpy(&returned_control, bounce->header + 40, sizeof(returned_control));
        size_t control_length =
            bounce->control_capacity < returned_control ? bounce->control_capacity : (size_t)returned_control;
        if ((int64_t)G_RET(bounce->cpu) >= 0 &&
            ((name_length && guest_copy_to(bounce->guest_name, bounce->name, name_length) != (ssize_t)name_length) ||
             (control_length &&
              guest_copy_to(bounce->guest_control, bounce->control, control_length) != (ssize_t)control_length) ||
             0))
            G_RET(bounce->cpu) = (uint64_t)(int64_t)(-EFAULT);
        memcpy(bounce->header + 0, &bounce->raw_guest_name, sizeof(bounce->raw_guest_name));
        memcpy(bounce->header + 16, &bounce->raw_guest_iov_address, sizeof(bounce->raw_guest_iov_address));
        memcpy(bounce->header + 32, &bounce->raw_guest_control, sizeof(bounce->raw_guest_control));
        if ((int64_t)G_RET(bounce->cpu) >= 0 &&
            guest_copy_to(bounce->guest_header, bounce->header, sizeof(bounce->header)) != sizeof(bounce->header))
            G_RET(bounce->cpu) = (uint64_t)(int64_t)(-EFAULT);
    }
    for (uint64_t i = 0; i < bounce->count; ++i)
        free(bounce->host_iov[i].iov_base);
    free(bounce->name);
    free(bounce->control);
}

static int net_message_bounce_begin(net_message_bounce *bounce, struct cpu *cpu, uint64_t guest_header, int receive) {
    memset(bounce, 0, sizeof(*bounce));
    bounce->cpu = cpu;
    bounce->guest_header = guest_header;
    bounce->receive = receive;
    if (guest_copy_from(bounce->header, guest_header, sizeof(bounce->header)) != sizeof(bounce->header)) return -EFAULT;
    memcpy(&bounce->count, bounce->header + 24, sizeof(bounce->count));
    if (bounce->count > 1024) {
        bounce->count = 0;
        return -EMSGSIZE;
    }
    memcpy(&bounce->raw_guest_iov_address, bounce->header + 16, sizeof(bounce->raw_guest_iov_address));
    bounce->guest_iov_address = bounce->raw_guest_iov_address;
    bounce->guest_iov_address = net_nonpie_p(bounce->guest_iov_address);
    size_t iov_bytes = (size_t)bounce->count * sizeof(struct iovec);
    if (iov_bytes && guest_copy_from(bounce->guest_iov, bounce->guest_iov_address, iov_bytes) != (ssize_t)iov_bytes)
        return -EFAULT;
    for (uint64_t i = 0; i < bounce->count; ++i) {
        size_t length = bounce->guest_iov[i].iov_len;
        bounce->host_iov[i].iov_len = length;
        bounce->host_iov[i].iov_base = malloc(length ? length : 1);
        if (!bounce->host_iov[i].iov_base) return -ENOMEM;
        uint64_t guest = net_nonpie_p((uint64_t)(uintptr_t)bounce->guest_iov[i].iov_base);
        bounce->guest_iov[i].iov_base = (void *)(uintptr_t)guest;
        if (!receive && length && guest_copy_from(bounce->host_iov[i].iov_base, guest, length) != (ssize_t)length)
            return -EFAULT;
    }
    memcpy(&bounce->raw_guest_name, bounce->header, sizeof(bounce->raw_guest_name));
    bounce->guest_name = bounce->raw_guest_name;
    bounce->guest_name = net_nonpie_p(bounce->guest_name);
    memcpy(&bounce->name_capacity, bounce->header + 8, sizeof(bounce->name_capacity));
    if (bounce->guest_name && bounce->name_capacity) {
        if (bounce->name_capacity > 4096) return -EINVAL;
        bounce->name = calloc(bounce->name_capacity, 1);
        if (!bounce->name) return -ENOMEM;
        if (!receive &&
            guest_copy_from(bounce->name, bounce->guest_name, bounce->name_capacity) != bounce->name_capacity)
            return -EFAULT;
    }
    memcpy(&bounce->raw_guest_control, bounce->header + 32, sizeof(bounce->raw_guest_control));
    bounce->guest_control = bounce->raw_guest_control;
    bounce->guest_control = net_nonpie_p(bounce->guest_control);
    memcpy(&bounce->control_capacity, bounce->header + 40, sizeof(bounce->control_capacity));
    if (bounce->guest_control && bounce->control_capacity) {
        if (bounce->control_capacity > 1024 * 1024) return -ENOMEM;
        bounce->control = calloc(bounce->control_capacity, 1);
        if (!bounce->control) return -ENOMEM;
        if (!receive && guest_copy_from(bounce->control, bounce->guest_control, bounce->control_capacity) !=
                            (ssize_t)bounce->control_capacity)
            return -EFAULT;
    }
    uint64_t host_iov_address = (uint64_t)(uintptr_t)bounce->host_iov;
    uint64_t host_name_address = (uint64_t)(uintptr_t)bounce->name;
    uint64_t host_control_address = (uint64_t)(uintptr_t)bounce->control;
    memcpy(bounce->header + 16, &host_iov_address, sizeof(host_iov_address));
    memcpy(bounce->header + 0, &host_name_address, sizeof(host_name_address));
    memcpy(bounce->header + 32, &host_control_address, sizeof(host_control_address));
    bounce->active = 1;
    return 0;
}

static int svc_net(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                   uint64_t a5) {
    if ((nr >= 198 && nr <= 212) || nr == 242 || nr == 243 || nr == 269)
        HL_LOGF(&g_jit_log, HL_LOG_TAG_NETWORK, "nr=%llu fd=%lld", (unsigned long long)nr, (long long)a0);
    // AF_NETLINK/NETLINK_ROUTE: a guest netlink socket is a socketpair we RTNETLINK-respond on
    // (see netns.c). bind/getsockname/send/recv are handled here; everything else (setsockopt/getsockopt/
    // close) falls through to the generic paths, which work on the underlying AF_UNIX socket.
    if (nl_is((int)a0)) {
        switch (nr) {
        case 200: // bind(sockaddr_nl): no-op success (our socketpair is already connected)
            G_RET(c) = 0;
            return svc_done_host(c);
        case 204: // getsockname -> sockaddr_nl { family, pid=getpid() }
        {
            net_sockaddr_copyout address_copyout __attribute__((cleanup(net_sockaddr_copyout_finish)));
            int error = net_sockaddr_copyout_begin(&address_copyout, c, a1, a2);
            if (error < 0) {
                G_RET(c) = (uint64_t)(int64_t)error;
                return svc_done_host(c);
            }
            nl_getsockname(address_copyout.active ? address_copyout.address : NULL,
                           address_copyout.active ? &address_copyout.length : NULL);
            G_RET(c) = 0;
            return svc_done_host(c);
        }
        case 206: { // sendto/send: parse the RTNETLINK request, queue the dump
            void *payload __attribute__((cleanup(guest_free_bounce))) = malloc(a2 ? (size_t)a2 : 1);
            if (!payload) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
                return svc_done_host(c);
            }
            if (a2 && guest_copy_from(payload, a1, (size_t)a2) != (ssize_t)a2) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return svc_done_host(c);
            }
            int64_t s = nl_send((int)a0, payload, (size_t)a2);
            G_RET(c) = (uint64_t)s;
            return svc_done_host(c);
        }
        case 211: { // sendmsg: gather the iov into a scratch buffer, then queue the dump
            net_message_bounce message_bounce __attribute__((cleanup(net_message_bounce_finish)));
            int error = net_message_bounce_begin(&message_bounce, c, a1, 0);
            if (error < 0) {
                G_RET(c) = (uint64_t)(int64_t)error;
                return svc_done_host(c);
            }
            uint8_t *g = message_bounce.header;
            struct iovec *iv = (struct iovec *)*(uint64_t *)(g + 16);
            int ivn = (int)*(uint64_t *)(g + 24);
            uint8_t tmp[4096];
            size_t tl = 0;
            for (int i = 0; iv && i < ivn && tl < sizeof tmp; i++) {
                size_t n = iv[i].iov_len;
                if (tl + n > sizeof tmp) n = sizeof tmp - tl;
                memcpy(tmp + tl, iv[i].iov_base, n);
                tl += n;
            }
            nl_send((int)a0, tmp, tl);
            G_RET(c) = (uint64_t)tl;
            return svc_done_host(c);
        }
        case 207: { // recvfrom/recv: drain our queued dump (Linux MSG_PEEK/TRUNC); kernel (pid 0) source
            void *payload __attribute__((cleanup(guest_free_bounce))) = calloc(a2 ? (size_t)a2 : 1, 1);
            if (!payload) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
                return svc_done_host(c);
            }
            struct iovec iov = {payload, (size_t)a2};
            int64_t r = nl_recv((int)a0, &iov, 1, (int)a3, NULL);
            if (r >= 0) {
                size_t copied = (size_t)r < (size_t)a2 ? (size_t)r : (size_t)a2;
                if (copied && guest_copy_to(a1, payload, copied) != (ssize_t)copied) r = -EFAULT;
            }
            if (r >= 0 && a4) {
                net_sockaddr_copyout address_copyout __attribute__((cleanup(net_sockaddr_copyout_finish)));
                int error = net_sockaddr_copyout_begin(&address_copyout, c, a4, a5);
                if (error < 0) {
                    G_RET(c) = (uint64_t)(int64_t)error;
                    return svc_done_host(c);
                }
                nl_fill_src(address_copyout.address, address_copyout.capacity);
                address_copyout.length = 12;
                G_RET(c) = (uint64_t)r;
                return svc_done_host(c);
            }
            G_RET(c) = (uint64_t)r; // nl_recv already returns -errno on failure
            return svc_done_host(c);
        }
        case 212: { // recvmsg: read into the guest iov (Linux MSG_PEEK/TRUNC); report a kernel source addr
            net_message_bounce message_bounce __attribute__((cleanup(net_message_bounce_finish)));
            int error = net_message_bounce_begin(&message_bounce, c, a1, 1);
            if (error < 0) {
                G_RET(c) = (uint64_t)(int64_t)error;
                return svc_done_host(c);
            }
            uint8_t *g = message_bounce.header;
            struct iovec *iov = (struct iovec *)*(uint64_t *)(g + 16);
            int iovn = (int)*(uint64_t *)(g + 24);
            int mf = 0;
            int64_t r = nl_recv((int)a0, iov, iovn, (int)a2, &mf);
            if (r >= 0) {
                uint8_t *gname = (uint8_t *)*(uint64_t *)(g + 0);
                uint32_t gnl = *(uint32_t *)(g + 8);
                if (gname && gnl >= 12) {
                    nl_fill_src(gname, gnl);
                    *(uint32_t *)(g + 8) = 12;
                } else
                    *(uint32_t *)(g + 8) = 0;
                *(uint64_t *)(g + 40) = 0;            // msg_controllen
                *(uint32_t *)(g + 48) = (uint32_t)mf; // msg_flags (Linux MSG_TRUNC iff the copy truncated)
            }
            G_RET(c) = (uint64_t)r; // nl_recv already returns -errno on failure
            return svc_done_host(c);
        }
        default: break; // setsockopt/getsockopt/shutdown/etc.: generic path on the AF_UNIX socket
        }
    }
    switch (nr) {
#include "net/endpoint.inc"
#include "net/data.inc"
#include "net/message.inc"
    default: return 0;
    }
    return svc_done_host(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done_host
}
