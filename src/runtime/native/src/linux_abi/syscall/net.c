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
// Returns 0 to continue, or a negative *macOS* errno to return now (svc_done does the m2l boundary xlate,
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
            return svc_done(c);
        case 204: // getsockname -> sockaddr_nl { family, pid=getpid() }
        {
            net_sockaddr_copyout address_copyout __attribute__((cleanup(net_sockaddr_copyout_finish)));
            int error = net_sockaddr_copyout_begin(&address_copyout, c, a1, a2);
            if (error < 0) {
                G_RET(c) = (uint64_t)(int64_t)error;
                return svc_done(c);
            }
            nl_getsockname(address_copyout.active ? address_copyout.address : NULL,
                           address_copyout.active ? &address_copyout.length : NULL);
            G_RET(c) = 0;
            return svc_done(c);
        }
        case 206: { // sendto/send: parse the RTNETLINK request, queue the dump
            void *payload __attribute__((cleanup(guest_free_bounce))) = malloc(a2 ? (size_t)a2 : 1);
            if (!payload) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
                return svc_done(c);
            }
            if (a2 && guest_copy_from(payload, a1, (size_t)a2) != (ssize_t)a2) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return svc_done(c);
            }
            int64_t s = nl_send((int)a0, payload, (size_t)a2);
            G_RET(c) = (uint64_t)s;
            return svc_done(c);
        }
        case 211: { // sendmsg: gather the iov into a scratch buffer, then queue the dump
            net_message_bounce message_bounce __attribute__((cleanup(net_message_bounce_finish)));
            int error = net_message_bounce_begin(&message_bounce, c, a1, 0);
            if (error < 0) {
                G_RET(c) = (uint64_t)(int64_t)error;
                return svc_done(c);
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
            return svc_done(c);
        }
        case 207: { // recvfrom/recv: drain our queued dump (Linux MSG_PEEK/TRUNC); kernel (pid 0) source
            void *payload __attribute__((cleanup(guest_free_bounce))) = calloc(a2 ? (size_t)a2 : 1, 1);
            if (!payload) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
                return svc_done(c);
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
                    return svc_done(c);
                }
                nl_fill_src(address_copyout.address, address_copyout.capacity);
                address_copyout.length = 12;
                G_RET(c) = (uint64_t)r;
                return svc_done(c);
            }
            G_RET(c) = (uint64_t)r; // nl_recv already returns -errno on failure
            return svc_done(c);
        }
        case 212: { // recvmsg: read into the guest iov (Linux MSG_PEEK/TRUNC); report a kernel source addr
            net_message_bounce message_bounce __attribute__((cleanup(net_message_bounce_finish)));
            int error = net_message_bounce_begin(&message_bounce, c, a1, 1);
            if (error < 0) {
                G_RET(c) = (uint64_t)(int64_t)error;
                return svc_done(c);
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
            return svc_done(c);
        }
        default: break; // setsockopt/getsockopt/shutdown/etc.: generic path on the AF_UNIX socket
        }
    }
    switch (nr) {
    case 198: {
        if ((int)a0 == LX_AF_NETLINK) {                     // socket(AF_NETLINK,...) -> socketpair-backed netlink
            G_RET(c) = (uint64_t)nl_open((int)a1, (int)a2); // -host_errno on fail -> svc_done translates
            break;
        }
        int ty = (int)a1;
        // Linux rejects a type carrying bits outside SOCK_TYPE_MASK(0xf) other than SOCK_CLOEXEC(0x80000)
        // and SOCK_NONBLOCK(0x800) with EINVAL, before consulting the family. Validate here so a junk-bit
        // type does not silently mask down to a valid type (host would either accept it or return the wrong
        // ESOCKTNOSUPPORT for a masked-but-unsupported value).
        if (ty & ~(0xf | 0x80000 | 0x800)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int icmp_ty = ty & 0xf;
        int is_icmp =
            (int)a0 == AF_INET && (int)a2 == IPPROTO_ICMP && ((icmp_ty == SOCK_DGRAM) || (icmp_ty == SOCK_RAW));
        // The unprivileged ICMP *datagram* ping socket (`ping localhost` healthcheck) is synthesized from an
        // AF_UNIX socket and its loopback (127/8) echo reflected locally (see icmp_try_send). Gating only on
        // br_on() left the no-bridge / loopback-only case falling through to a real host
        // socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP), which needs net.ipv4.ping_group_range / CAP_NET_RAW and
        // EACCES's on a locked-down host (e.g. the CI runner's default empty ping_group_range). lo_on() is
        // true whenever the private loopback namespace is active, so synthesize the datagram ping there too.
        // SOCK_RAW ICMP stays gated on br_on() only: a raw socket is privileged, and under loopback-only
        // isolation the host must still enforce EPERM (see the socket-matrix raw_icmp=EPERM oracle).
        int virtual_icmp = is_icmp && (br_on() || (icmp_ty == SOCK_DGRAM && lo_on()));
        // socket (translate Linux domain -> macOS: AF_INET6 10->30, others unchanged). Gate the new fd
        // against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap (the host table is far larger).
        // Bridge ICMP is synthesized below and must not depend on the host's privileged ping-socket policy.
        int r = nofile_gate(virtual_icmp ? socket(AF_UNIX, SOCK_DGRAM, 0) : socket(af_l2m((int)a0), ty & 0xf, (int)a2));
        if (r >= 0) {
            // SIGPIPE suppression: Linux delivers EPIPE (not a fatal signal) to a guest that has
            // SIG_IGN'd SIGPIPE or passes MSG_NOSIGNAL; macOS has no per-call MSG_NOSIGNAL, so make the
            // suppression sticky on the fd. With SO_NOSIGPIPE set at creation, ANY write to a broken
            // socket -- write(2), writev(2), send(2) without MSG_NOSIGNAL -- returns -1/EPIPE instead
            // of raising SIGPIPE. Benign on healthy sockets; only sockets get it, so real pipes/FIFOs
            // keep Linux's default SIGPIPE-on-write semantics.
            (void)hl_native_set_no_sigpipe(r);
            // One spelling on every host: on a host whose descriptor status cannot be queried the two
            // bits live in a side record rather than in fcntl, and the caller must not have to know which.
            hl_linux_socket_apply_type_flags(r, ty);
            if (r < HL_NFD) {
                // AF_INET6 STREAM also gets loopback isolation (::/::1 -> private lo). a0 is the guest's
                // Linux domain value, so test the Linux AF_INET6 (10), not the macOS one (30).
                g_sock_stream[r] = ((ty & 0xf) == SOCK_STREAM && ((int)a0 == AF_INET || (int)a0 == LX_AF_INET6_FAM));
                g_sock_dgram[r] = ((ty & 0xf) == SOCK_DGRAM && ((int)a0 == AF_INET || (int)a0 == LX_AF_INET6_FAM));
                g_udp_local_port[r] = g_udp_peer_port[r] = 0;
                g_udp_local_ip[r] = g_udp_peer_ip[r] = 0;
                g_udp_local_interface[r] = g_udp_peer_interface[r] = 0;
                g_udp_local_v6[r] = g_udp_peer_v6[r] = 0;
                g_sock_seqpacket[r] = 0;
                g_so_error[r] = 0;     // no pending async socket error on a fresh fd
                g_so_reuseport[r] = 0; // SO_REUSEPORT not yet set on a fresh fd
                tcp_shadow_clear(r);   // no shadowed IPPROTO_TCP options on a fresh fd
                ipopt_shadow_clear(r); // ...nor shadowed IPPROTO_IP/IPV6 options
                g_sock_conn[r] = 0;    // fresh socket: not yet connected (see g_sock_conn decl)
                g_sock_connecting[r] = 0;
                g_sock_host_backed[r] = 0;
                g_sock_fam[r] = (uint16_t)a0; // guest address family, for connect/bind EAFNOSUPPORT check
                g_lo_port[r] = 0;
                g_lo_v6[r] = 0;
                g_lo_v6only[r] = 0;
                g_br_port[r] = 0;
                g_br_ip[r] = 0;
                g_br_interface[r] = 0;
                g_tcp_lport[r] = 0;
                g_tcp_listen[r] = 0;
                g_dns_sock[r] = 0;
                g_icmp_kind[r] =
                    ((int)a0 == AF_INET && (int)a2 == 1 && (((ty & 0xf) == SOCK_DGRAM) || ((ty & 0xf) == SOCK_RAW)))
                        ? (uint8_t)(((ty & 0xf) == SOCK_RAW) ? 2 : 1)
                        : 0;
                g_icmp_sock[r] = 0;
                g_icmp_ip[r] = 0;
                g_sock_object[r] = sock_object_new();
                g_sock_peer_object[r] = 0;
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 199: {
        int sv[2];
        // Linux rejects a type carrying bits outside SOCK_TYPE_MASK(0xf) other than SOCK_CLOEXEC(0x80000)
        // and SOCK_NONBLOCK(0x800) with EINVAL, before consulting the family (net/socket.c __sys_socketpair
        // strips the flag bits and validates them exactly like __sys_socket). Mirror the case 198 check so a
        // junk-bit type does not silently mask down to a valid type and wrongly succeed.
        if ((int)a1 & ~(0xf | 0x80000 | 0x800)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // socketpair (translate Linux domain -> macOS). macOS AF_UNIX has no SOCK_SEQPACKET socketpair;
        // emulate it with SOCK_DGRAM, which over a local AF_UNIX pair is reliable, ordered, and preserves
        // message boundaries -- exactly the SEQPACKET guarantees the guest relies on.
        int lty = (int)a1 & 0xf;
        int hty = (lty == SOCK_SEQPACKET) ? SOCK_DGRAM : lty;
        int r = socketpair(af_l2m((int)a0), hty, (int)a2, sv);
        // Either new fd past the guest's soft RLIMIT_NOFILE -> EMFILE; close both so nothing leaks.
        if (r == 0) {
            int cap = guest_nofile_cur();
            if (sv[0] >= cap || sv[1] >= cap) {
                close(sv[0]);
                close(sv[1]);
                G_RET(c) = (uint64_t)(-EMFILE);
                break;
            }
        }
        if (r == 0) {
            // SO_NOSIGPIPE on both ends so a write/send to a peer-closed pair returns EPIPE, never a
            // fatal SIGPIPE (matches Linux EPIPE-to-guest behaviour). See case 198 for the rationale.
            (void)hl_native_set_no_sigpipe(sv[0]);
            (void)hl_native_set_no_sigpipe(sv[1]);
            if ((int)a1 & HL_LINUX_SOCK_CLOEXEC) {
                fcntl(sv[0], F_SETFD, FD_CLOEXEC);
                fcntl(sv[1], F_SETFD, FD_CLOEXEC);
            }
            if ((int)a1 & HL_LINUX_SOCK_NONBLOCK) {
                hl_linux_socket_apply_type_flags(sv[0], HL_LINUX_SOCK_NONBLOCK);
                hl_linux_socket_apply_type_flags(sv[1], HL_LINUX_SOCK_NONBLOCK);
            }
            // The fd pair is written straight into the guest array; a bad/unmapped destination must EFAULT
            // (and leak no fds) like Linux, not fault the engine writing the pair.
            if (guest_accessible_prefix(a3, 2 * sizeof(int), PROT_WRITE) != 2 * sizeof(int)) {
                close(sv[0]);
                close(sv[1]);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            int guest_pair[2] = {sv[0], sv[1]};
            if (guest_copy_to(a3, guest_pair, sizeof(guest_pair)) != (ssize_t)sizeof(guest_pair)) {
                close(sv[0]);
                close(sv[1]);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            if ((int)a0 == AF_UNIX) {
                if (sv[0] >= 0 && sv[0] < HL_NFD) {
                    g_sock_fam[sv[0]] = AF_UNIX;
                    g_sock_stream[sv[0]] = (lty == SOCK_STREAM);
                    g_sock_dgram[sv[0]] = (lty == SOCK_DGRAM || lty == SOCK_SEQPACKET);
                    g_sock_pair_peer[sv[0]] = sv[1] + 1;
                    g_sock_peer_pid[sv[0]] = sock_alloc_synth_peer();
                    g_sock_passcred[sv[0]] = 0;
                }
                if (sv[1] >= 0 && sv[1] < HL_NFD) {
                    g_sock_fam[sv[1]] = AF_UNIX;
                    g_sock_stream[sv[1]] = (lty == SOCK_STREAM);
                    g_sock_dgram[sv[1]] = (lty == SOCK_DGRAM || lty == SOCK_SEQPACKET);
                    g_sock_pair_peer[sv[1]] = sv[0] + 1;
                    g_sock_peer_pid[sv[1]] = sock_alloc_synth_peer();
                    g_sock_passcred[sv[1]] = 0;
                }
                sock_pair_identity_assign(sv[0], sv[1]);
            }
            // macOS AF_UNIX has no SEQPACKET, so a SEQPACKET pair is backed by SOCK_DGRAM (above) to keep
            // message boundaries. But a connected DGRAM socket does NOT deliver EOF when its peer closes
            // (a blocked recv never wakes; a fresh recv gets ECONNRESET) -- whereas Linux SEQPACKET recv
            // returns 0 (EOF). Mark both ends so close() injects a zero-length EOF datagram and recv/read
            // translate the peer-closed ECONNRESET to 0. (rustc's jobserver relies on this EOF to exit.)
            if (lty == SOCK_SEQPACKET) {
                // _seqpacket-dgram-maxmsg_: a macOS AF_UNIX DGRAM socket caps SO_SNDBUF at the tiny
                // net.local.dgram.maxdgram default (2048), so ANY send > 2048 bytes fails with EMSGSIZE --
                // whereas a Linux SEQPACKET message is bounded only by SO_SNDBUF (~208KB default) and never
                // hits a 2KB wall. Large SEQPACKET bootstrap messages require the Linux-sized buffer, and bring-up
                // messages carrying serialized handles and initialization IPC routinely exceed
                // 2KB: on the DGRAM backing those sends fail and the message is lost, so the parent/child
                // handshake wedges forever (the UI thread blocks on a readiness event the child can never
                // signal -> no window is ever created). Raise SO_SNDBUF/SO_RCVBUF on BOTH ends (a per-socket
                // buffer overrides the maxdgram default; verified: with 1MB buffers, 256KB datagrams send OK)
                // so an emulated SEQPACKET carries the same large messages a real Linux SEQPACKET does. Both
                // ends are bidirectional (each sends and receives), and the setting survives the fork/exec
                // that hands one end to the child (it is a property of the kernel socket object).
                int bufsz = 1 << 20; // 1 MiB: comfortably above a realistic IPC channel message
                setsockopt(sv[0], SOL_SOCKET, SO_SNDBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[0], SOL_SOCKET, SO_RCVBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[1], SOL_SOCKET, SO_SNDBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[1], SOL_SOCKET, SO_RCVBUF, &bufsz, sizeof bufsz);
                // Stamp each end with a DISTINCT synthetic peer node identity. macOS reports the socketpair
                // CREATOR's pid via LOCAL_PEERPID on BOTH ends (never updated on fork), so once the parent
                // forks a child its cred/peercred query degenerates to self; without a distinct id every
                // forked child collides on guest pid 1 and peer-node merging hangs.
                if (sv[0] >= 0 && sv[0] < HL_NFD) g_sock_seqpacket[sv[0]] = 1;
                if (sv[1] >= 0 && sv[1] < HL_NFD) g_sock_seqpacket[sv[1]] = 1;
                if (seq_ref_pair(sv[0], sv[1]) != 0) {
                    int e = errno;
                    g_sock_seqpacket[sv[0]] = 0;
                    g_sock_seqpacket[sv[1]] = 0;
                    close(sv[0]);
                    close(sv[1]);
                    G_RET(c) = (uint64_t)(-e);
                    break;
                }
            }
            (void)proc_fdvis_publish_native_fd(sv[0]);
            (void)proc_fdvis_publish_native_fd(sv[1]);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // bind -- port-map: bind the published host port
    case 200: {
        int socket_type = 0;
        socklen_t socket_type_length = sizeof(socket_type);
        if (getsockopt((int)a0, SOL_SOCKET, SO_TYPE, &socket_type, &socket_type_length) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        void *bind_address __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a2 > sizeof(struct sockaddr_storage)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        bind_address = malloc(a2 ? (size_t)a2 : 1);
        if (!bind_address) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if (a2 && guest_copy_from(bind_address, a1, (size_t)a2) != (ssize_t)a2) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        a1 = (uint64_t)(uintptr_t)bind_address;
        // Linux errno pre-screen (EBADF/ENOTSOCK/EFAULT/EINVAL/EAFNOSUPPORT) before any addr deref.
        {
            int pc = net_precheck((int)a0, a1, (socklen_t)a2, 0);
            if (pc) {
                G_RET(c) = (uint64_t)(int64_t)pc;
                return svc_done(c);
            }
        }
        // GUEST Linux sockaddr_in: family@0(u16 LE), port@2(BE)
        uint8_t *sa = (uint8_t *)a1;
        // Bad address POINTER -> EFAULT, not an engine fault: the loopback/bridge/AF_UNIX classifiers
        // below deref `sa` directly. Validate the declared addrlen (clamped) first. (LTP bind01 EFAULT)
        {
            size_t alc = (size_t)(socklen_t)a2;
            if (alc > sizeof(struct sockaddr_storage)) alc = sizeof(struct sockaddr_storage);
            if (!host_range_mapped((uintptr_t)a1, alc)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        // remember the guest-requested (addr,port) of a stream socket so a later listen can surface a
        // LISTEN row in /proc/net/tcp[6] (ss/netstat -l). Independent of which network mode the bind resolves
        // to below -- the synthesized table has no real IP stack to read back from. AF is the guest sockaddr
        // family at offset 0 (LE u16); port is BE at offset 2 (identical v4/v6 layout).
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] && a2 >= 8) {
            uint16_t fam = *(uint16_t *)(sa + 0), bp = ntohs(*(uint16_t *)(sa + 2));
            if (fam == AF_INET)
                netns_tcp_bind_note((int)a0, bp, 0, *(uint32_t *)(sa + 4), NULL);
            else if (fam == LX_AF_INET6_FAM && a2 >= 24)
                netns_tcp_bind_note((int)a0, bp, 1, 0, sa + 8);
        }
        // private loopback: v4 127/8 (and 0.0.0.0 in direct mode -- a 0.0.0.0 server must answer 127.0.0.1),
        // or v6 ::1/:: (dual-stack servers bind v6 too; route it to the SAME per-container loopback so it is
        // isolated from the real host stack instead of escaping it). port@2 is identical in v4/v6 layout.
        int is_lo6 = lo6_any_is(sa, (socklen_t)a2);
        if (lo_on() && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] &&
            (lo_any_is(sa, (socklen_t)a2) || is_lo6)) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            if (p == 0) p = lo_alloc_ephemeral(); // bind(:0) -> a real, round-trippable port
            int v6only = 0;
            if (is_lo6) {
                socklen_t ol = sizeof v6only;
                if (getsockopt((int)a0, IPPROTO_IPV6, IPV6_V6ONLY, &v6only, &ol) != 0) v6only = 0;
            }
            char up[200];
            lo_tcp_path(p, is_lo6 && v6only, up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // SO_REUSEPORT dual-bind: native Linux lets a second REUSEPORT socket bind the same
            // addr:port. The AF_UNIX switch backs each port by one fs inode, so a second bind would
            // hit EADDRINUSE. Only when THIS socket has SO_REUSEPORT set, drop the existing inode so
            // the rebind replaces it and succeeds. A normal (non-REUSEPORT) rebind keeps the inode and
            // correctly returns EADDRINUSE, which also preserves an active wildcard listener's binding.
            if (g_so_reuseport[(int)a0]) unlink(up);
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) {
                g_lo_port[(int)a0] = p ? p : 1;
                g_lo_v6[(int)a0] = (uint8_t)is_lo6; // remember family for getsockname/accept
                g_lo_v6only[(int)a0] = (uint8_t)(is_lo6 && v6only);
                netns_tcp_port_note((int)a0, p); // surface the resolved (ephemeral) port in /proc/net/tcp
                // Own the rendezvous inode with the refcounted-unlink mechanism (shared with UDP): unlike a
                // real INADDR_ANY TCP bind whose port is freed by the kernel on last close, this AF_UNIX
                // inode would otherwise linger on the fs and EADDRINUSE the next bind of the same ip:port
                // (fixed HL_IP -> deterministic path). udp_ref_drop (fd_reset_emul) unlinks it on the LAST
                // reference's close; seq_ref_fork_prepare bumps the ref across fork so a client child that
                // inherits the listener does not orphan it.
                udp_ref_create((int)a0, up);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // NET bridge: bind(0.0.0.0 / own-ip / in-subnet :port) -> the namespace's private bridge path.
        // A dual-stack listener that binds `::` (busybox nc's default, and many servers') is the IPv6 analogue
        // of 0.0.0.0 and takes the same path (br6_any_is), so it's reachable by peer containers over the switch
        // instead of landing on the isolated per-container loopback (which broke cross-container reach-by-name).
        int bridge_enabled = br_on();
        int bridge_interface = bridge_enabled ? br_bind_interface(sa, (socklen_t)a2) : -1;
        int bridge_v6_any = bridge_enabled && br6_any_is(sa, (socklen_t)a2);
        int bridge_v4_any = bridge_enabled && sa && a2 >= 8 && *(uint16_t *)sa == AF_INET && *(uint32_t *)(sa + 4) == 0;
        if (bridge_v6_any) bridge_interface = 0;
        if (bridge_enabled && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] && bridge_interface >= 0) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            if (p == 0 && br_alloc_ephemeral(bridge_interface, &p) != 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int bridge_v6only = 0;
            if (br6_any_is(sa, (socklen_t)a2)) {
                socklen_t option_length = sizeof bridge_v6only;
                if (getsockopt((int)a0, IPPROTO_IPV6, IPV6_V6ONLY, &bridge_v6only, &option_length) != 0)
                    bridge_v6only = 0;
            }
            char up[200];
            int path_status = bridge_v6only
                                  ? br_v6only_path(bridge_interface, g_netif[bridge_interface].ip, p, up, sizeof up)
                                  : br_path(bridge_interface, g_netif[bridge_interface].ip, p, up, sizeof up);
            if (path_status != 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // SO_REUSEPORT dual-bind over the bridge switch: same rule as the private-loopback path --
            // only a REUSEPORT socket replaces the existing per-port inode; a normal rebind keeps it and
            // returns EADDRINUSE, preserving an active wildcard listener's binding.
            if (g_so_reuseport[(int)a0]) unlink(up);
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) {
                g_br_port[(int)a0] = p ? p : 1;
                g_br_ip[(int)a0] = g_netif[bridge_interface].ip;
                g_br_interface[(int)a0] = (uint8_t)(bridge_interface + 1);
                netns_tcp_port_note((int)a0, p); // surface the resolved (ephemeral) port in /proc/net/tcp
                // Own the bridge rendezvous inode with the refcounted-unlink mechanism (shared with UDP) so
                // it is removed on the LAST reference's close instead of lingering to EADDRINUSE the next
                // bind of the same ip:port (see the private-loopback path above for the full rationale).
                if (udp_ref_create((int)a0, up) < 0 ||
                    ((bridge_v4_any || bridge_v6_any) &&
                     br_alias_wildcard_listener((int)a0, bridge_interface, p, bridge_v6only) < 0)) {
                    int saved = errno;
                    udp_ref_drop((int)a0);
                    (void)lo_swap((int)a0); // discard the bound socket after a partial registration
                    r = -1;
                    errno = saved;
                }
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // abstract AF_UNIX (sun_path[0]==0): macOS has no abstract ns -> bind a real fs socket keyed by
        // HL_NETNS. Must run BEFORE any general AF_UNIX passthrough below.
        if (abs_is(sa, (socklen_t)a2)) {
            char up[200];
            abs_path(sa, (socklen_t)a2, up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            unlink(up); // replace stale (cf. lo_/br_ above)
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) { // record the guest-visible abstract name "@<name>" for /proc/net/unix
                char an[108];
                int L = (int)a2 - 3; // sun_path[0]==0, name follows; addrlen = 2 (family) + 1 (nul) + name
                if (L < 0) L = 0;
                if (L > (int)sizeof an - 2) L = (int)sizeof an - 2;
                an[0] = '@';
                memcpy(an + 1, sa + 3, (size_t)L);
                an[L + 1] = 0;
                unix_bind_note((int)a0, an);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // AF_UNIX autobind: bind() with only the family (no name) -> Linux assigns a unique abstract-namespace
        // address. Route it through the same HL_NETNS-keyed fs backing as an explicit abstract bind so the
        // assigned name is both reported by getsockname and reachable by a connecting peer.
        if (sa && a2 >= 2 && *(uint16_t *)sa == AF_UNIX &&
            (socklen_t)a2 <= (socklen_t)offsetof(struct sockaddr_un, sun_path)) {
            static uint32_t g_autobind_seq;
            uint8_t syn[3 + 16];
            *(uint16_t *)syn = AF_UNIX;
            syn[2] = 0; // abstract (leading NUL)
            int nl = snprintf((char *)syn + 3, 13, "%05x",
                              (unsigned)(((uint32_t)getpid() << 12) ^ ++g_autobind_seq) & 0xfffffu);
            char up[200];
            abs_path(syn, (socklen_t)(3 + nl), up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            unlink(up);
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) {
                char an[108];
                an[0] = '@';
                memcpy(an + 1, syn + 3, (size_t)nl);
                an[nl + 1] = 0;
                unix_bind_note((int)a0, an);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // AF_UNIX pathname bind: materialize the socket inode in the overlay (writable upper), jail-confined,
        // so the guest can stat/chmod/connect it through the SAME resolver. A raw host bind created the inode
        // OUTSIDE the jail (at the literal guest path on the host fs), so the guest's overlay-routed stat()
        // ENOENT'd it (mongod "Failed to chmod socket file", mariadb "Bind on unix socket"). This also
        // applies to a typed bind volume in bare mode: every pathname operation must select its backing.
        if (unix_path_is(sa, (socklen_t)a2)) {
            char gp[200], host[1024];
            unix_path_copy(sa, (socklen_t)a2, gp, sizeof gp);
            if (!unix_path_routed(gp)) goto bind_passthrough;
            if (g_rootfs)
                overlay_copyup(gp, host, sizeof host); // guest path -> upper host path (+ materialize parent dirs)
            else
                xlate(gp, host, sizeof host); // missing final socket name inside the bare bind volume
            unlink(host);                     // clear a stale inode (else EADDRINUSE)
            // bind at the FULL upper path (via unix_sock_at, which fchdir-shortens paths past sun_path[104])
            // so the socket inode lands exactly where the guest's stat/chmod/connect resolves -- a plain bind
            // would truncate the long upper path and strand the inode where nothing can find it.
            int r = unix_sock_at((int)a0, host, 0);
            if (r == 0) unix_bind_note((int)a0, gp); // record the guest path for /proc/net/unix
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
    bind_passthrough:
        // Private UDP uses the same launch-scoped AF_UNIX switch as TCP. This includes ordinary,
        // unpublished loopback and user-network sockets; publishing is an additional host forwarder.
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
            int ui = br_on() ? br_bind_interface(sa, (socklen_t)a2) : -1;
            int udp_lo6 = lo_on() && lo6_any_is(sa, (socklen_t)a2);
            if (ui >= 0 || (lo_on() && lo_any_is(sa, (socklen_t)a2)) || udp_lo6) {
                uint16_t up = ntohs(*(uint16_t *)(sa + 2));
                uint32_t uip = ui >= 0 ? g_netif[ui].ip : 0;
                int ur = udp_switch_bind((int)a0, ui, uip, up);
                if (ur == 0) g_udp_local_v6[(int)a0] = (uint8_t)udp_lo6;
                if (ur == 0 && up && pm_published(up)) udp_fwd_maybe_start((int)a0);
                G_RET(c) = ur < 0 ? (uint64_t)(-errno) : 0;
                break;
            }
        }
        // Published UDP (`-p H:C/udp`): swap an AF_INET datagram socket bound to a published port onto
        // the AF_UNIX switch + start its host->guest datagram forwarder. No-op (returns 0) for
        // non-published UDP, non-switch nets, or non-datagram sockets -> they fall through unchanged.
        {
            int64_t uret;
            if (udp_bind_maybe((int)a0, sa, (socklen_t)a2, &uret)) {
                G_RET(c) = (uint64_t)uret;
                break;
            }
        }
        if (hl_linux_ports_count(&g_ports) != 0 && sa && a2 >= 8 && *(uint16_t *)(sa + 0) == AF_INET) {
            uint16_t cp = ntohs(*(uint16_t *)(sa + 2)), hp = pm_host(cp);
            // remember for getsockname
            if ((int)a0 >= 0 && (int)a0 < HL_NFD) g_fd_cport[(int)a0] = cp;
            if (hp != cp) {
                uint8_t buf[128];
                socklen_t L = a2 < 128 ? (socklen_t)a2 : 128;
                memcpy(buf, sa, L);
                // publish on :H instead of :C (port @2)
                *(uint16_t *)(buf + 2) = htons(hp);
                if (pm_address(cp) != 0) *(uint32_t *)(buf + 4) = pm_address(cp);
                // Linux->macOS sockaddr translation (sin_len/family) before the real host bind.
                struct sockaddr_storage ss;
                socklen_t hl = sa_l2m(buf, L, &ss);
                int br = (hl != (socklen_t)-1) ? bind((int)a0, (struct sockaddr *)&ss, hl)
                                               : bind((int)a0, (struct sockaddr *)buf, L);
                G_RET(c) = br < 0 ? (uint64_t)(-errno) : 0;
                break;
            }
        }
        // Real host bind: translate Linux AF_INET/INET6 sockaddr -> macOS (sin_len/family); AF_UNIX
        // and others pass through unchanged. (Was: raw bind of the Linux struct -> AF_UNSPEC bind.)
        {
            struct sockaddr_storage ss;
            socklen_t hl = sa_l2m(sa, (socklen_t)a2, &ss);
            int br = (hl != (socklen_t)-1) ? bind((int)a0, (struct sockaddr *)&ss, hl)
                                           : bind((int)a0, (void *)a1, (socklen_t)a2);
            // bare-mode AF_UNIX pathname bind (no overlay jail): record the guest path for /proc/net/unix.
            if (br == 0 && a2 >= 3 && *(uint16_t *)(sa + 0) == AF_UNIX && sa[2]) {
                char gp[108];
                snprintf(gp, sizeof gp, "%.*s", (int)sizeof gp - 1, (const char *)(sa + 2));
                unix_bind_note((int)a0, gp);
            }
            G_RET(c) = br < 0 ? (uint64_t)(-errno) : 0;
        }
        break;
    }
    case 201: {
        int lr = listen((int)a0, (int)a1);
        if (lr == 0 && (int)a0 >= 0 && (int)a0 < HL_NFD) {
            g_tcp_listen[(int)a0] = 1;
            g_sock_backlog[(int)a0] = (int)a1;
        }
        // Published-port (`-p H:C`) host bridge: if this is a switch-backed listen on a published
        // container port, spin up a real host AF_INET listener on :H that relays into the guest.
        if (lr == 0) fwd_maybe_start((int)a0);
        if (lr == 0) netns_tcp_listen_note((int)a0); // arm the /proc/net/tcp[6] LISTEN row

        G_RET(c) = lr < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 202:
    case 242: {
        // accept4 flags: only SOCK_CLOEXEC(0x80000) | SOCK_NONBLOCK(0x800) are defined. Linux
        // (__sys_accept4) rejects any other bit with EINVAL as its FIRST check -- before the listen
        // fd is even looked up (so EINVAL wins over EBADF and over a would-be EAGAIN on a nonblocking
        // listener). Validate here so a junk-bit accept4 doesn't slip through to a real accept().
        if (nr == 242 && ((int)a3 & ~(0x80000 | 0x800))) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int lfd = (int)a0;
        uint64_t guest_peer = a1, guest_peer_length = a2;
        uint8_t peer_address[sizeof(struct sockaddr_storage)] = {0};
        socklen_t peer_length = 0, peer_capacity = 0;
        // accept / accept4
        int pl = (lfd >= 0 && lfd < HL_NFD) ? g_lo_port[lfd] : 0;
        int pl6 = (lfd >= 0 && lfd < HL_NFD) ? g_lo_v6[lfd] : 0; // listener is AF_INET6 -> report v6 peer
        int pbr = (lfd >= 0 && lfd < HL_NFD) ? g_br_port[lfd] : 0;
        uint32_t pbrip = (lfd >= 0 && lfd < HL_NFD) ? g_br_ip[lfd] : 0;
        // Real host accept writes a macOS sockaddr; receive into a host scratch then translate the
        // peer addr back to Linux layout for the guest. (private-lo / bridge: don't expose unix peer.)
        struct sockaddr_storage hss;
        socklen_t hsl = sizeof hss;
        int want_peer = (!pl && !pbr && a1);
        int r;
        do {
            r = (pl || pbr)
                    ? accept(lfd, NULL, NULL)
                    : accept(lfd, want_peer ? (struct sockaddr *)&hss : NULL, want_peer ? &hsl : (socklen_t *)a2);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        r = nofile_gate(r); // accepted fd past the guest's soft RLIMIT_NOFILE -> EMFILE (host table is larger)
        if (r >= 0) {
            // The peer sockaddr (a1) + addrlen (a2) are written straight into the guest buffer by the
            // writeback below; a bad/unmapped destination must EFAULT like Linux -- consuming the accepted
            // connection but not leaking its fd -- not fault the engine. Validate the addrlen cell then the
            // declared (clamped) sockaddr capacity before any writeback; close the accepted fd on failure.
            if (a1) {
                int bad = !a2 || guest_copy_from(&peer_length, a2, sizeof(peer_length)) != sizeof(peer_length);
                if (!bad) {
                    size_t alc = peer_length;
                    if (alc > sizeof(struct sockaddr_storage)) alc = sizeof(struct sockaddr_storage);
                    if (alc && guest_accessible_prefix(a1, alc, PROT_WRITE) != alc) bad = 1;
                }
                if (bad) {
                    close(r);
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                peer_capacity = peer_length;
                a1 = (uint64_t)(uintptr_t)peer_address;
                a2 = (uint64_t)(uintptr_t)&peer_length;
            }
            // Accepted connections are sockets too: make SIGPIPE suppression sticky on the new fd so a
            // write/send to a peer that closes returns EPIPE instead of killing the guest (see case 198).
            (void)hl_native_set_no_sigpipe(r);
            if (r >= 0 && r < HL_NFD) {
                g_so_error[r] = 0; // fresh accepted fd carries no pending async error
                g_so_reuseport[r] = 0;
                tcp_shadow_clear(r); // no shadowed IPPROTO_TCP options on a fresh accepted fd
                ipopt_shadow_clear(r);
                g_sock_conn[r] = 1; // an accepted socket is already connected
                g_sock_connecting[r] = 0;
                if (lfd >= 0 && lfd < HL_NFD) g_sock_fam[r] = g_sock_fam[lfd]; // inherit listener's family
                g_sock_object[r] = sock_object_new();
                g_sock_peer_object[r] = 0;
            }
            if ((pl || pbr) && sock_internal_accept_identify(r) < 0) {
                int error = errno;
                close(r);
                G_RET(c) = (uint64_t)(int64_t)(-error);
                break;
            }
            if (nr == 242) { hl_linux_socket_apply_type_flags(r, (int)a3); }
            if (want_peer) {
                socklen_t gcap = a2 ? *(socklen_t *)a2 : 0;
                int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a1, gcap);
                if (ll < 0) ll = sa_un_m2l((struct sockaddr *)&hss, hsl, (uint8_t *)a1, gcap); // AF_UNIX -> Linux
                if (ll < 0) { // other non-inet peer: copy raw host bytes
                    socklen_t n = hsl < gcap ? hsl : gcap;
                    if (gcap) memcpy((void *)a1, &hss, n);
                    if (a2) *(socklen_t *)a2 = hsl;
                } else if (a2)
                    *(socklen_t *)a2 = (socklen_t)ll;
            }
            if (pl) {
                if (r < HL_NFD) {
                    g_lo_port[r] = pl;
                    g_lo_v6[r] = (uint8_t)pl6;
                    g_sock_stream[r] = 1;
                }
                if (pl6)
                    fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, pl);
                else
                    fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, pl);
            } else if (pbr) {
                if (r < HL_NFD) {
                    g_br_port[r] = pbr;
                    g_br_ip[r] = pbrip;
                    g_sock_stream[r] = 1;
                }
                // peer reported as our virtual listen addr (cf. lo_* simplification)
                fill_inet_br((uint8_t *)a1, (socklen_t *)a2, pbrip, pbr);
            }
            if (guest_peer) {
                size_t copy_length = peer_capacity < peer_length ? peer_capacity : peer_length;
                if (copy_length > sizeof(peer_address)) copy_length = sizeof(peer_address);
                if ((copy_length && guest_copy_to(guest_peer, peer_address, copy_length) != (ssize_t)copy_length) ||
                    guest_copy_to(guest_peer_length, &peer_length, sizeof(peer_length)) != sizeof(peer_length)) {
                    close(r);
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
            }
            // peer = 127.0.0.1:lport
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // connect
    case 203: {
        int socket_type = 0;
        socklen_t socket_type_length = sizeof(socket_type);
        if (getsockopt((int)a0, SOL_SOCKET, SO_TYPE, &socket_type, &socket_type_length) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        void *connect_address __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a2 > sizeof(struct sockaddr_storage)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        connect_address = malloc(a2 ? (size_t)a2 : 1);
        if (!connect_address) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if (a2 && guest_copy_from(connect_address, a1, (size_t)a2) != (ssize_t)a2) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        a1 = (uint64_t)(uintptr_t)connect_address;
        // Linux errno pre-screen (EBADF/ENOTSOCK/EFAULT/EINVAL/EISCONN/EAFNOSUPPORT) before any addr deref.
        {
            int pc = net_precheck((int)a0, a1, (socklen_t)a2, 1);
            if (pc) {
                G_RET(c) = (uint64_t)(int64_t)pc;
                return svc_done(c);
            }
        }
        // Isolated networking: no external egress (HL_NET_ISOLATE). Loopback is redirected by the lo_* path
        // below; any non-127/8 AF_INET destination is refused, matching docker's null network.
        static int net_isolate = -1;
        if (net_isolate < 0) net_isolate = hl_option_get("HL_NET_ISOLATE") != NULL;
        uint8_t *sa = (uint8_t *)a1;
        // A bad address POINTER must return EFAULT, not fault the engine: the DNS/loopback/AF_UNIX
        // classifiers below deref `sa` directly (Linux copies the sockaddr in before any routing).
        // Validate the declared addrlen (clamped to a real sockaddr) up front. (LTP connect01 EFAULT)
        {
            size_t alc = (size_t)(socklen_t)a2;
            if (alc > sizeof(struct sockaddr_storage)) alc = sizeof(struct sockaddr_storage);
            if (!host_range_mapped((uintptr_t)a1, alc)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        // Container DNS: connect(127.0.0.11:53) -> swap the socket to a socketpair we answer on (host
        // resolver). Subsequent send/recv on the connected fd are handled by the DNS paths below.
        if (dns_enabled() && dns_dest_is(sa, (socklen_t)a2)) {
            int stream = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_sock_stream[(int)a0] : 0;
            if (dns_swap((int)a0, stream) == 0) {
                G_RET(c) = 0;
                break;
            } // swap failed -> fall through to the normal (host loopback) connect
        }
        if (net_isolate && sa && (socklen_t)a2 >= 8 && *(uint16_t *)(sa + 0) == AF_INET &&
            (ntohl(*(uint32_t *)(sa + 4)) >> 24) != 127) {
            G_RET(c) = (uint64_t)(-ENETUNREACH);
            break;
        }
        int c_lo6 = lo6_is(sa, (socklen_t)a2);
        if (lo_on() && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] &&
            // private loopback: v4 127/8 or v6 ::1 (port@2 identical) -> the per-container loopback switch
            (lo_is(sa, (socklen_t)a2) || c_lo6)) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            char up[200];
            lo_tcp_path(p, c_lo6, up, sizeof up);
            if (c_lo6 && access(up, F_OK) != 0) lo_tcp_path(p, 0, up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (sock_internal_connect_prepare((int)a0) != 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int r = connect((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0 || errno == EINPROGRESS) {
                g_sock_connecting[(int)a0] = r < 0;
                g_lo_port[(int)a0] = p ? p : 1;
                g_lo_v6[(int)a0] = (uint8_t)c_lo6;
            } else if ((errno == ENOENT || errno == ECONNREFUSED) && br_on()) {
                // Same-container localhost dial of a server that bound INADDR_ANY on the bridge (br_path,
                // keyed by OUR own IP -- not lo_path): retry there so 127.0.0.1 still reaches a 0.0.0.0
                // listener in bridge mode. The first connect() already POISONED this AF_UNIX socket (a
                // failed BSD connect leaves the fd unusable -- a second connect() on it hangs/EINVALs, which
                // is why a 0.0.0.0 server was unreachable via 127.0.0.1 with a user network attached),
                // so swap in a FRESH AF_UNIX fd before the retry -- exactly as the br_connect loop does.
                char bp[200];
                struct sockaddr_un bu;
                if (br_path(0, g_netif[0].ip, p, bp, sizeof bp) != 0 || unix_addr_set(&bu, bp) < 0 ||
                    lo_swap((int)a0) < 0) {
                    r = -1;
                } else if (sock_internal_connect_prepare((int)a0) != 0) {
                    r = -1;
                } else {
                    r = connect((int)a0, (struct sockaddr *)&bu, sizeof bu);
                    if (r == 0 || errno == EINPROGRESS) {
                        g_sock_connecting[(int)a0] = r < 0;
                        g_br_port[(int)a0] = p ? p : 1;
                        g_br_ip[(int)a0] = g_netif[0].ip;
                        g_br_interface[(int)a0] = 1;
                    }
                }
            }
            // a redirected TCP dial to a port with no listener fails ENOENT (the per-port unix
            // inode doesn't exist); Linux returns ECONNREFUSED for a closed TCP port. Map it (host
            // errno, translated to Linux 111); other errnos including EINPROGRESS pass through.
            if (r < 0) {
                int le = (errno == ENOENT) ? ECONNREFUSED : errno;
                // A non-blocking connect must not surface ECONNREFUSED synchronously: a real TCP stack
                // defers the refusal to the next poll/getsockopt(SO_ERROR). The AF_UNIX switch has no live
                // INET peer to deliver that, so stash the error and report EINPROGRESS -- poll then wakes on
                // the failed AF_UNIX socket and getsockopt(SO_ERROR) hands the ECONNREFUSED back once.
                if (le == ECONNREFUSED && (int)a0 >= 0 && (int)a0 < HL_NFD && (fcntl((int)a0, F_GETFL) & O_NONBLOCK)) {
                    g_so_error[(int)a0] = ECONNREFUSED;
                    G_RET(c) = (uint64_t)(-EINPROGRESS);
                } else {
                    G_RET(c) = (uint64_t)(-le);
                }
            } else {
                G_RET(c) = 0;
                if ((int)a0 >= 0 && (int)a0 < HL_NFD)
                    g_sock_conn[(int)a0] = 1, g_sock_connecting[(int)a0] = 0; // sticky-connected
            }
            break;
        }
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_kind[(int)a0] && sa && (socklen_t)a2 >= 8 &&
            *(const uint16_t *)sa == AF_INET &&
            (((*(const uint32_t *)(sa + 4)) & 0xffu) == 127u || // loopback 127/8: locally reflected echo
             (br_on() && br_for_ip(*(const uint32_t *)(sa + 4)) >= 0))) {
            g_icmp_ip[(int)a0] = *(const uint32_t *)(sa + 4);
            G_RET(c) = 0;
            break;
        }
        // NET bridge: connect(peer-ip:port in our subnet) -> dial the namespace's private bridge path.
        int bridge_enabled = br_on();
        int connect_interface = bridge_enabled ? br_connect_interface(sa, (socklen_t)a2) : -1;
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
            // connect(AF_UNSPEC) dissolves a datagram socket's association: clear the recorded peer so a
            // later getpeername reports ENOTCONN, matching the kernel's disconnect idiom.
            if (sa && (socklen_t)a2 >= 2 && *(const uint16_t *)sa == AF_UNSPEC) {
                g_udp_peer_port[(int)a0] = 0;
                g_udp_peer_ip[(int)a0] = 0;
                g_udp_peer_interface[(int)a0] = 0;
                g_udp_peer_v6[(int)a0] = 0;
                g_sock_conn[(int)a0] = 0;
                g_sock_connecting[(int)a0] = 0;
                G_RET(c) = 0;
                break;
            }
            char udp_path[200];
            int udp_interface;
            uint32_t udp_ip;
            uint16_t udp_port;
            int udp_route = udp_switch_destination(sa, (socklen_t)a2, &udp_interface, &udp_ip, &udp_port, udp_path,
                                                   sizeof udp_path);
            if (udp_route < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (udp_route > 0) {
                if (udp_switch_ensure_source((int)a0, udp_interface) < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
                g_udp_peer_port[(int)a0] = udp_port;
                g_udp_peer_ip[(int)a0] = udp_ip;
                g_udp_peer_interface[(int)a0] = (uint8_t)(udp_interface + 1);
                g_udp_peer_v6[(int)a0] = (uint8_t)(*(const uint16_t *)sa == LX_AF_INET6_FAM);
                G_RET(c) = 0;
                break;
            }
        }
        if (bridge_enabled && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] && connect_interface >= 0) {
            uint32_t dip = *(uint32_t *)(sa + 4);
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            char up[200];
            if (br_path(connect_interface, dip, p, up, sizeof up) != 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // Retry across a peer's brief re-listen gap: a server looping `nc -l -w N` unbinds+rebinds the
            // switch inode between connections, so a dial that lands in the window sees ENOENT (inode gone)
            // or ECONNREFUSED (stale inode). Recreate the guest fd (lo_swap) + retry for ~600ms, mirroring
            // TCP SYN retransmission; a genuinely-absent peer still fails after the cap. This is what makes
            // a single-shot client (`nc -w 3 <peer> <port>`) reliably reach a `-w 1`-looping listener.
            int r = -1;
            for (int attempt = 0; attempt < 60; attempt++) {
                if (lo_swap((int)a0) < 0) {
                    r = -1;
                    break;
                }
                if (sock_internal_connect_prepare((int)a0) != 0) {
                    r = -1;
                    break;
                }
                r = connect((int)a0, (struct sockaddr *)&un, sizeof un);
                if (r == 0) {
                    // A blocking connect succeeded: verify it isn't a peer mid-exit (a `-w N` listener whose
                    // window just closed accepts nothing and HUPs with no data). If dead-on-arrival, retry a
                    // fresh listener; otherwise it's live (data pending, or a client-first protocol).
                    if (!switch_dead_on_arrival((int)a0)) break;
                } else if (errno == EINPROGRESS) {
                    break; // non-blocking: the guest polls the result itself
                } else if (errno != ENOENT && errno != ECONNREFUSED) {
                    break; // a genuine error -> report it
                }
                r = -1;                             // not connected yet
                errno = ECONNREFUSED;               // if this was the last attempt, report a closed-port error
                struct timespec ts = {0, 20000000}; // 20ms
                nanosleep(&ts, NULL);
            }
            if (r == 0 || errno == EINPROGRESS) {
                g_sock_connecting[(int)a0] = r < 0;
                g_br_port[(int)a0] = p ? p : 1;
                g_br_ip[(int)a0] = dip; // peer ip for getpeername
                g_br_interface[(int)a0] = (uint8_t)(connect_interface + 1);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            if (r == 0 && (int)a0 >= 0 && (int)a0 < HL_NFD)
                g_sock_conn[(int)a0] = 1, g_sock_connecting[(int)a0] = 0; // sticky-connected
            break;
        }
        // abstract AF_UNIX (sun_path[0]==0): dial the same HL_NETNS-keyed fs socket bind used. Must run
        // BEFORE the general AF_UNIX passthrough below.
        if (abs_is(sa, (socklen_t)a2)) {
            char up[200];
            abs_path(sa, (socklen_t)a2, up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int r = connect((int)a0, (struct sockaddr *)&un, sizeof un);
            int ce = errno;
            // An abstract name is not a filesystem object: a missing backing socket means no bound listener,
            // which Linux reports as ECONNREFUSED (the host fs backing yields ENOENT).
            if (r < 0 && ce == ENOENT) ce = ECONNREFUSED;
            G_RET(c) = (r < 0 && ce != EINPROGRESS) ? (uint64_t)(-ce) : 0;
            if ((r == 0 || ce == EINPROGRESS) && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                g_sock_conn[(int)a0] = 1; // sticky-connected
                g_sock_connecting[(int)a0] = r < 0;
                char an[108];
                int L = (int)a2 - 3; // sun_path[0]==0, name follows; addrlen = 2 (family) + 1 (nul) + name
                if (L < 0) L = 0;
                if (L > (int)sizeof an - 2) L = (int)sizeof an - 2;
                an[0] = '@';
                memcpy(an + 1, sa + 3, (size_t)L);
                an[L + 1] = 0;
                unix_peer_note((int)a0, an); // guest-visible peer name for getpeername
            }
            break;
        }
        // AF_UNIX pathname connect: resolve through the overlay (same resolver as stat/open) so we dial the
        // socket the guest actually bound -- materialized in the upper -- not a host path outside the jail.
        if (unix_path_is(sa, (socklen_t)a2)) {
            char gp[200], host[1024];
            unix_path_copy(sa, (socklen_t)a2, gp, sizeof gp);
            if (!unix_path_routed(gp)) goto connect_passthrough;
            const char *hp = atpath(-100, gp, host, sizeof host, 0); // guest path -> topmost layer's host path
            // dial via unix_sock_at (matches the bind side): fchdir-shortens paths past sun_path[104] so a
            // long upper socket path is reached exactly, not truncated to some other (nonexistent) inode.
            int r;
            do {
                r = unix_sock_at((int)a0, hp, 1);
            } while (r < 0 && SVC_EINTR_RESTART(c));
            G_RET(c) = (r < 0 && errno != EINPROGRESS) ? (uint64_t)(-errno) : 0;
            if ((r == 0 || errno == EINPROGRESS) && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                g_sock_conn[(int)a0] = r == 0;
                g_sock_connecting[(int)a0] = r < 0;
                g_sock_native_peer[(int)a0] = (uint8_t)jail_is_projected_socket(gp);
            }
            break;
        }
    connect_passthrough:
        // Real host connect: translate Linux AF_INET/INET6 sockaddr -> macOS; others pass through.
        {
            struct sockaddr_storage ss;
            socklen_t hl = sa_l2m(sa, (socklen_t)a2, &ss);
            // bind(0.0.0.0, ...) on a virtual network swaps a stream onto the AF_UNIX switch so peer
            // containers can reach a listener. Linux also permits that bound socket to connect outward.
            // An external INET sockaddr cannot be passed to the substituted AF_UNIX descriptor
            // (EAFNOSUPPORT), so restore a real INET stream while retaining the guest-visible local bind
            // metadata. Drop the now-defunct switch rendezvous only after replacement succeeds.
            if (hl != (socklen_t)-1 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] &&
                (g_lo_port[(int)a0] || g_br_port[(int)a0]) && !g_sock_host_backed[(int)a0]) {
                if (inet_stream_swap((int)a0) < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
                udp_ref_drop((int)a0);
                g_sock_host_backed[(int)a0] = 1;
            }
            // When HL_EGRESS_SOCKS is armed, funnel a genuine external TCP
            // connect through the SOCKS5 proxy instead of dialing directly. INERT when unset — egress_should_
            // redirect() short-circuits to 0, so control falls straight to the direct connect() below with no
            // behavior change. Streams only; UDP/raw and non-INET dests use the direct path.
            if (hl != (socklen_t)-1 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] &&
                egress_should_redirect((struct sockaddr *)&ss)) {
                int er = egress_connect((int)a0, (struct sockaddr *)&ss, hl);
                G_RET(c) = er < 0 ? (uint64_t)(-errno) : 0;
                if ((er == 0 || errno == EINPROGRESS) && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                    g_sock_conn[(int)a0] = er == 0;
                    g_sock_connecting[(int)a0] = er < 0;
                }
                break;
            }
            // #261 IPv4-only network: a genuine external IPv6 dest has no route -> ENETUNREACH *now* (not a
            // 2-min host-v6 timeout), so happy-eyeballs (apt/curl) falls back to the IPv4 answer immediately.
            if (hl != (socklen_t)-1 && v6_no_route((struct sockaddr *)&ss)) {
                G_RET(c) = (uint64_t)(-ENETUNREACH);
                break;
            }
            int cr;
            do {
                cr = (hl != (socklen_t)-1) ? connect((int)a0, (struct sockaddr *)&ss, hl)
                                           : connect((int)a0, (void *)a1, (socklen_t)a2);
            } while (cr < 0 && SVC_EINTR_RESTART(c));
            G_RET(c) = cr < 0 ? (uint64_t)(-errno) : 0;
            if ((cr == 0 || errno == EINPROGRESS) && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                g_sock_conn[(int)a0] = cr == 0;
                g_sock_connecting[(int)a0] = cr < 0;
            }
        }
        break;
    }
    case 204: {
        // getsockname
        int fd = (int)a0;
        net_sockaddr_copyout address_copyout __attribute__((cleanup(net_sockaddr_copyout_finish)));
        // The addrlen pointer (a2) is dereferenced and the sockaddr buffer (a1) written directly by every
        // branch below; a bad/unmapped pointer must EFAULT like Linux, not fault the engine. Validate the
        // addrlen cell first, then the declared (clamped) sockaddr capacity.
        if (net_sockaddr_copyout_begin(&address_copyout, c, a1, a2) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (address_copyout.active) {
            a1 = (uint64_t)(uintptr_t)address_copyout.address;
            a2 = (uint64_t)(uintptr_t)&address_copyout.length;
        }
        // Abstract-namespace local name: echo the guest name, not the engine's HL_NETNS-keyed backing fs path.
        if (fd >= 0 && fd < HL_NFD && g_unix_bind[fd][0] == '@' && a1) {
            socklen_t gl = 0;
            int ll = unix_name_fill(g_unix_bind[fd], (uint8_t *)a1, a2 ? *(socklen_t *)a2 : 0, &gl);
            if (ll >= 0) {
                if (a2) *(socklen_t *)a2 = gl;
                G_RET(c) = 0;
                break;
            }
        }
        if (fd >= 0 && fd < HL_NFD && g_udp_local_port[fd]) {
            if (g_udp_local_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_udp_local_port[fd]);
            else if (g_udp_local_interface[fd])
                fill_inet_br((uint8_t *)a1, (socklen_t *)a2, g_udp_local_ip[fd], g_udp_local_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_udp_local_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_dns_sock[fd]) { // DNS socket: report an AF_INET local addr (0.0.0.0:0)
            if (a1) {
                uint8_t *g = (uint8_t *)a1;
                memset(g, 0, 8);
                *(uint16_t *)g = AF_INET;
                if (a2) *(socklen_t *)a2 = 16;
            }
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_lo_port[fd]) {
            if (g_lo_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_br_port[fd]) {
            fill_inet_br((uint8_t *)a1, (socklen_t *)a2, g_br_ip[fd], g_br_port[fd]);
            G_RET(c) = 0;
            break;
        }
        // Real host getsockname returns a macOS sockaddr; receive into host scratch, translate back to
        // Linux layout for the guest (fixes sin_family/sin_len), preserving the portmap port rewrite.
        struct sockaddr_storage hss;
        socklen_t hsl = sizeof hss;
        int r = getsockname(fd, (struct sockaddr *)&hss, &hsl);
        if (r == 0 && a1) {
            socklen_t gcap = a2 ? *(socklen_t *)a2 : 0;
            int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a1, gcap);
            if (ll < 0)
                ll = sa_un_m2l((struct sockaddr *)&hss, hsl, (uint8_t *)a1, gcap); // AF_UNIX -> Linux + guest path
            if (ll < 0) {
                socklen_t n = hsl < gcap ? hsl : gcap;
                if (gcap) memcpy((void *)a1, &hss, n);
                if (a2) *(socklen_t *)a2 = hsl;
            } else {
                if (a2) *(socklen_t *)a2 = (socklen_t)ll;
                if (hl_linux_ports_count(&g_ports) != 0 && fd >= 0 && fd < HL_NFD && g_fd_cport[fd] && gcap >= 4)
                    // app sees the port it asked for (port @2)
                    *(uint16_t *)((uint8_t *)a1 + 2) = htons(g_fd_cport[fd]);
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 205: {
        // getpeername
        int fd = (int)a0;
        net_sockaddr_copyout address_copyout __attribute__((cleanup(net_sockaddr_copyout_finish)));
        // The addrlen pointer (a2) is dereferenced and the sockaddr buffer (a1) written directly by every
        // branch below; a bad/unmapped pointer must EFAULT like Linux, not fault the engine. Validate the
        // addrlen cell first, then the declared (clamped) sockaddr capacity.
        if (net_sockaddr_copyout_begin(&address_copyout, c, a1, a2) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (address_copyout.active) {
            a1 = (uint64_t)(uintptr_t)address_copyout.address;
            a2 = (uint64_t)(uintptr_t)&address_copyout.length;
        }
        // Abstract-namespace peer name: echo the guest name recorded on connect, not the backing fs path.
        if (fd >= 0 && fd < HL_NFD && g_unix_peer[fd][0] == '@' && a1) {
            socklen_t gl = 0;
            int ll = unix_name_fill(g_unix_peer[fd], (uint8_t *)a1, a2 ? *(socklen_t *)a2 : 0, &gl);
            if (ll >= 0) {
                if (a2) *(socklen_t *)a2 = gl;
                G_RET(c) = 0;
                break;
            }
        }
        if (fd >= 0 && fd < HL_NFD && g_udp_peer_port[fd]) {
            if (g_udp_peer_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_udp_peer_port[fd]);
            else if (g_udp_peer_interface[fd])
                fill_inet_br((uint8_t *)a1, (socklen_t *)a2, g_udp_peer_ip[fd], g_udp_peer_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_udp_peer_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_dns_sock[fd]) { // DNS socket: peer is the nameserver 127.0.0.11:53
            dns_fill_ns((uint8_t *)a1, (socklen_t *)a2);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_lo_port[fd] && !g_sock_host_backed[fd]) {
            if (g_lo_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_br_port[fd] && !g_sock_host_backed[fd]) {
            fill_inet_br((uint8_t *)a1, (socklen_t *)a2, g_br_ip[fd], g_br_port[fd]);
            G_RET(c) = 0;
            break;
        }
        // Real host getpeername: translate macOS sockaddr back to Linux layout for the guest.
        struct sockaddr_storage hss;
        socklen_t hsl = sizeof hss;
        int r = getpeername(fd, (struct sockaddr *)&hss, &hsl);
        if (r == 0 && a1) {
            socklen_t gcap = a2 ? *(socklen_t *)a2 : 0;
            int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a1, gcap);
            if (ll < 0)
                ll = sa_un_m2l((struct sockaddr *)&hss, hsl, (uint8_t *)a1, gcap); // AF_UNIX -> Linux + guest path
            if (ll < 0) {
                socklen_t n = hsl < gcap ? hsl : gcap;
                if (gcap) memcpy((void *)a1, &hss, n);
                if (a2) *(socklen_t *)a2 = hsl;
            } else if (a2)
                *(socklen_t *)a2 = (socklen_t)ll;
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 206: {
        void *send_payload __attribute__((cleanup(guest_free_bounce))) = NULL;
        void *send_destination __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a2) {
            send_payload = malloc((size_t)a2);
            if (!send_payload) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            if (guest_copy_from(send_payload, a1, (size_t)a2) != (ssize_t)a2) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            a1 = (uint64_t)(uintptr_t)send_payload;
        }
        if (a4) {
            if (a5 > 4096) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            send_destination = calloc(a5 ? (size_t)a5 : 1, 1);
            if (!send_destination) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            if (a5 && guest_copy_from(send_destination, a4, (size_t)a5) != (ssize_t)a5) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            a4 = (uint64_t)(uintptr_t)send_destination;
        }
        // A bad DEST-address pointer -> EFAULT, not an engine fault: the DNS/AF_UNIX/INET classifiers
        // below deref a4 directly. (The data buffer a1 is validated by the host sendto itself.) The dest
        // is optional (NULL on a connected socket), so only validate when present. (LTP sendto02 EFAULT)
        if (a4) {
            size_t dlc = (size_t)(socklen_t)a5;
            if (dlc > sizeof(struct sockaddr_storage)) dlc = sizeof(struct sockaddr_storage);
            if (!host_range_mapped((uintptr_t)a4, dlc)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        // Oversized UDP datagram -> EMSGSIZE, enforced before routing (matches Linux; the AF_UNIX switch
        // backing would otherwise leak a wrong errno for a too-large payload).
        {
            size_t udp_cap = udp_dgram_maxlen((int)a0);
            if (udp_cap && (size_t)a2 > udp_cap) {
                G_RET(c) = (uint64_t)(-EMSGSIZE);
                break;
            }
        }
        // Container DNS: a query sent to 127.0.0.11:53 (connected send, or first unconnected sendto) is
        // parsed + answered via the host resolver; nothing hits the wire. a4/a5 are the optional dest addr.
        {
            int64_t dret;
            if (dns_try_send((int)a0, (const uint8_t *)a1, (size_t)a2, (const uint8_t *)a4, (socklen_t)a5, &dret)) {
                G_RET(c) = (uint64_t)dret;
                break;
            }
            if (icmp_try_send((int)a0, (const uint8_t *)a1, (size_t)a2, (const uint8_t *)a4, (socklen_t)a5, &dret)) {
                G_RET(c) = (uint64_t)dret;
                break;
            }
        }
        // MSG_NOSIGNAL(0x4000) has no per-call equivalent on macOS; emulate it with the SO_NOSIGPIPE
        // socket option so the send returns EPIPE instead of raising a fatal SIGPIPE.
        if ((int)a3 & 0x4000) { (void)hl_native_set_no_sigpipe((int)a0); }
        // AF_UNIX pathname/abstract dest -> overlay/abstract-route it (syslog `logger` -> /dev/log).
        if (a4 && (socklen_t)a5 >= 2 && *(const uint16_t *)a4 == AF_UNIX) {
            char uhost[1200];
            if (unix_dgram_dest((const uint8_t *)a4, (socklen_t)a5, uhost, sizeof uhost)) {
                struct iovec iov = {(void *)a1, (size_t)a2};
                struct msghdr mh;
                memset(&mh, 0, sizeof mh);
                mh.msg_iov = &iov;
                mh.msg_iovlen = 1;
                int64_t ur;
                do {
                    ur = unix_dgram_sendmsg_at((int)a0, uhost, &mh, msgflags_l2m((int)a3));
                } while (ur < 0 && SVC_EINTR_RESTART(c));
                G_RET(c) = ur < 0 ? (uint64_t)(-errno) : (uint64_t)ur;
                break;
            }
        }
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
            char udp_path[200];
            int udp_interface;
            uint32_t udp_ip;
            uint16_t udp_port;
            int udp_route = a4 ? udp_switch_destination((const uint8_t *)a4, (socklen_t)a5, &udp_interface, &udp_ip,
                                                        &udp_port, udp_path, sizeof udp_path)
                               : udp_switch_peer_path((int)a0, udp_path, sizeof udp_path);
            if (udp_route < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (udp_route > 0) {
                if (!a4) udp_interface = (int)g_udp_peer_interface[(int)a0] - 1;
                if (udp_switch_ensure_source((int)a0, udp_interface) < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
                struct sockaddr_un un;
                ssize_t ur = unix_addr_set(&un, udp_path) < 0
                                 ? -1
                                 : sendto((int)a0, (void *)a1, (size_t)a2, msgflags_l2m((int)a3),
                                          (struct sockaddr *)&un, sizeof un);
                // Unconnected UDP send to a loopback port with no bound receiver: Linux drops the datagram
                // and reports success (no ICMP error is delivered to an unconnected socket). The AF_UNIX
                // switch backing instead surfaces ENOENT/ECONNREFUSED from the missing peer path -- coerce
                // it to a fire-and-forget success. Only for an explicit dest (a4); a connected socket (peer
                // path) keeps the underlying error so ICMP port-unreach can map to ECONNREFUSED.
                if (ur < 0 && a4 && (errno == ENOENT || errno == ECONNREFUSED)) {
                    G_RET(c) = (uint64_t)a2;
                    break;
                }
                G_RET(c) = ur < 0 ? (uint64_t)(-errno) : (uint64_t)ur;
                break;
            }
        }
        // dest addr (UDP): translate Linux AF_INET/INET6 sockaddr -> macOS; NULL/non-inet pass through.
        struct sockaddr_storage dss;
        socklen_t dhl = a4 ? sa_l2m((uint8_t *)a4, (socklen_t)a5, &dss) : (socklen_t)-1;
        const void *dst = (dhl != (socklen_t)-1) ? (void *)&dss : (void *)a4;
        socklen_t dl = (dhl != (socklen_t)-1) ? dhl : (socklen_t)a5;
        // #261 IPv4-only network: an external IPv6 datagram dest has no route -> ENETUNREACH now (mirrors the
        // connect() path; a QUIC/DoH client's v6 attempt fails fast and it retries over IPv4).
        if (dhl != (socklen_t)-1 && v6_no_route((struct sockaddr *)&dss)) {
            G_RET(c) = (uint64_t)(-ENETUNREACH);
            break;
        }
        ssize_t r;
        do {
            r = sendto((int)a0, (void *)a1, (size_t)a2, msgflags_l2m((int)a3), dst, dl);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        // send/sendto to a peer-closed socket -> guest SIGPIPE (Linux), unless MSG_NOSIGNAL was requested.
        if (!((int)a3 & 0x4000)) svc_sigpipe_on_epipe(c, (int64_t)G_RET(c));
        break;
    }
    case 207: {
        uint64_t guest_buffer = a1, guest_source = a4, guest_source_length = a5;
        socklen_t source_capacity = 0, source_length = 0;
        void *receive_buffer __attribute__((cleanup(guest_free_bounce))) = NULL;
        void *receive_source __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a2) {
            receive_buffer = malloc((size_t)a2);
            if (!receive_buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            a1 = (uint64_t)(uintptr_t)receive_buffer;
        }
        if (a4) {
            if (!a5 || guest_copy_from(&source_capacity, a5, sizeof(source_capacity)) != sizeof(source_capacity)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            size_t source_bytes =
                source_capacity > sizeof(struct sockaddr_storage) ? source_capacity : sizeof(struct sockaddr_storage);
            if (source_bytes > 4096) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            receive_source = calloc(source_bytes, 1);
            if (!receive_source) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            source_length = source_capacity;
            a4 = (uint64_t)(uintptr_t)receive_source;
            a5 = (uint64_t)(uintptr_t)&source_length;
        }
        // src addr: receive into host scratch (macOS layout) then translate back to Linux for the guest.
        struct sockaddr_storage hss;
        socklen_t hsl = sizeof hss;
        int want = a4 != 0;
        // Zero-length address-peek idiom: force macOS to block + fill the sender via a 1-byte scratch.
        char one;
        int peekaddr = dgram_addr_peek((int)a0, want, (size_t)a2);
        void *rbuf = peekaddr ? &one : (void *)a1;
        size_t rlen = peekaddr ? 1 : (size_t)a2;
        ssize_t r;
        ts_wait_enter(); // 'S' while blocked in recvfrom/recv
        do {
            hsl = sizeof hss;
            r = recvfrom((int)a0, rbuf, rlen, msgflags_l2m((int)a3), want ? (struct sockaddr *)&hss : NULL,
                         want ? &hsl : NULL);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        if (r > 0 && peekaddr) r = 0; // guest asked for 0 bytes; the address is what it wanted
        // SEQPACKET-as-DGRAM EOF: a peer-closed DGRAM recv reports ECONNRESET, but Linux SEQPACKET
        // returns 0 (EOF). Translate so the guest sees the expected end-of-stream. (See case 199.)
        if (r < 0 && errno == ECONNRESET && seq_is((int)a0)) r = 0;
        if (r >= 0 && want && (int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_sock[(int)a0]) {
            fill_inet_br((uint8_t *)a4, (socklen_t *)a5, g_icmp_ip[(int)a0], 0);
        } else if (r >= 0 && want && (int)a0 >= 0 && (int)a0 < HL_NFD && g_dns_sock[(int)a0]) {
            // DNS socket: report the source as the nameserver (127.0.0.11:53) so the guest resolver's
            // "answer came from the server we queried" anti-spoof check passes (the real src is AF_UNIX).
            dns_fill_ns((uint8_t *)a4, (socklen_t *)a5);
        } else if (r >= 0 && want && udp_switch_source(&hss, hsl, (uint8_t *)a4, (socklen_t *)a5)) {
            // translated from the switch's opaque AF_UNIX sender identity
        } else if (r >= 0 && want) {
            socklen_t gcap = a5 ? *(socklen_t *)a5 : 0;
            int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a4, gcap);
            if (ll < 0) ll = sa_un_m2l((struct sockaddr *)&hss, hsl, (uint8_t *)a4, gcap);
            if (ll < 0) {
                socklen_t n = hsl < gcap ? hsl : gcap;
                if (gcap) memcpy((void *)a4, &hss, n);
                if (a5) *(socklen_t *)a5 = hsl;
            } else if (a5)
                *(socklen_t *)a5 = (socklen_t)ll;
        }
        if (r >= 0 && r > 0 && guest_copy_to(guest_buffer, receive_buffer, (size_t)r) != r) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (r >= 0 && guest_source) {
            size_t copy_length = source_capacity < source_length ? source_capacity : source_length;
            if ((copy_length && guest_copy_to(guest_source, receive_source, copy_length) != (ssize_t)copy_length) ||
                guest_copy_to(guest_source_length, &source_length, sizeof(source_length)) != sizeof(source_length)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // setsockopt(fd, level, optname, val, len)
    case 208: {
        void *option_value __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a3 && a4) {
            if (a4 > 1024 * 1024) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            option_value = malloc((size_t)a4);
            if (!option_value) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            if (guest_copy_from(option_value, a3, (size_t)a4) != (ssize_t)a4) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            a3 = (uint64_t)(uintptr_t)option_value;
        }
        int lvl = (int)a1, opt = (int)a2;
        // SO_REUSEPORT (Linux SOL_SOCKET/15): remember the guest's intent so getsockopt reports it even when
        // this INET socket is later backed by an AF_UNIX switch socket (which reads SO_REUSEPORT back as 0).
        // The real setsockopt still runs below (dual-bind on the switch relies on it); this only tracks readback.
        if (lvl == 1 && opt == 15 && (int)a0 >= 0 && (int)a0 < HL_NFD)
            g_so_reuseport[(int)a0] = (a3 && (socklen_t)a4 >= 4 && *(int *)a3) ? 1 : 0;
        // SO_PASSCRED (Linux SOL_SOCKET/16): macOS has no equivalent. Record it per-fd so recvmsg(212)
        // synthesizes the SCM_CREDENTIALS ancillary record the Linux kernel would auto-attach (credential-aware
        // credential-aware IPC bootstrap requires it). Never fail the guest.
        if (lvl == 1 && opt == 16) {
            // Validate the fd like the real kernel before recording state: a closed fd is EBADF, a non-socket
            // is ENOTSOCK. SO_TYPE succeeds for any socket and reproduces both errnos on the host.
            int st_;
            socklen_t stl_ = sizeof st_;
            if (getsockopt((int)a0, SOL_SOCKET, SO_TYPE, &st_, &stl_) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int on = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
#if defined(__linux__)
            if (setsockopt((int)a0, SOL_SOCKET, SO_PASSCRED, &on, sizeof on) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
#endif
            if ((int)a0 >= 0 && (int)a0 < HL_NFD) g_sock_passcred[(int)a0] = on ? 1 : 0;
            G_RET(c) = 0;
            break;
        }
        // SO_RCVTIMEO(20)/SO_SNDTIMEO(21) (+ the 64-bit-time _NEW variants 66/67 glibc may use): a real
        // recv/send timeout the guest expects to ARM (a blocking recv with no data must return EAGAIN after
        // it, not hang forever). so_opt_l2m maps these to -1 (ignore) -> they were silently dropped. Translate
        // the Linux sock_timeval {s64 tv_sec; s64 tv_usec} (16B on 64-bit) into the macOS struct timeval and
        // set the real macOS option, reporting the true errno.
        if (lvl == 1 && (opt == 20 || opt == 21 || opt == 66 || opt == 67)) {
            struct timeval tv;
            memset(&tv, 0, sizeof tv);
            if (a3 && (socklen_t)a4 >= 16) {
                tv.tv_sec = (time_t)*(int64_t *)a3;
                tv.tv_usec = (suseconds_t) * (int64_t *)((uint8_t *)a3 + 8);
            }
            int mo = (opt == 21 || opt == 67) ? SO_SNDTIMEO : SO_RCVTIMEO;
            int r = setsockopt((int)a0, SOL_SOCKET, mo, &tv, sizeof tv);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // IPPROTO_TCP integer options on a tracked guest INET stream socket: once bind/connect swaps its host
        // backing to an AF_UNIX switch socket, the host rejects IPPROTO_TCP setsockopt with ENOPROTOOPT, but
        // Linux round-trips these on a real TCP socket and apps set TCP_NODELAY *after* connect(). Record the
        // guest's value so a later getsockopt reports it, and best-effort apply it to the host fd (a genuine
        // unswapped AF_INET socket still honors it). Never fail the guest.
        if (lvl == 6 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0]) {
            int slot = tcp_shadow_slot((int)a2);
            if (slot >= 0) {
                int val = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
                g_tcp_optval[(int)a0][slot] = val;
                g_tcp_optset[(int)a0][slot] = 1;
                int mo = tcp_opt_l2m((int)a2);
                if (mo >= 0) (void)setsockopt((int)a0, IPPROTO_TCP, mo, &val, sizeof val);
                G_RET(c) = 0;
                break;
            }
        }
        // IPPROTO_IP integer options on a tracked AF_INET socket: same story as TCP -- the AF_UNIX switch
        // backing rejects them with ENOPROTOOPT while native round-trips them. Record + best-effort apply to
        // the host fd (an unswapped socket still honors it). Only options native accepts on a connected
        // socket are shadow-slotted; the rest fall through to the real setsockopt (true ENOPROTOOPT/EPERM).
        if (lvl == 0 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_fam[(int)a0] == AF_INET) {
            int slot = ip_shadow_slot((int)a2);
            if (slot >= 0) {
                int val = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
                g_ipopt_val[(int)a0][slot] = val;
                g_ipopt_set[(int)a0][slot] = 1;
                int mo = ip_opt_l2m((int)a2);
                if (mo >= 0) (void)setsockopt((int)a0, IPPROTO_IP, mo, &val, sizeof val);
                G_RET(c) = 0;
                break;
            }
        }
        // IPPROTO_IPV6 integer options on a tracked AF_INET6 socket. IPV6_V6ONLY(26) is special: native
        // rejects a change once the socket is bound/connected (EINVAL), so try the real setsockopt first --
        // it succeeds pre-bind (unswapped) and its success/failure decides the shadow update and the errno.
        if (lvl == 41 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_fam[(int)a0] == LX_AF_INET6_FAM) {
            if ((int)a2 == 26) { // IPV6_V6ONLY
                int val = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
                int mo = ip6_opt_l2m(26);
                int r = (mo >= 0) ? setsockopt((int)a0, IPPROTO_IPV6, mo, &val, sizeof val) : -1;
                if (r == 0) { // pre-bind, host honored it -> record and report the new value
                    g_ipopt_val[(int)a0][IPOPT_V6ONLY_SLOT] = val ? 1 : 0;
                    g_ipopt_set[(int)a0][IPOPT_V6ONLY_SLOT] = 1;
                    G_RET(c) = 0;
                } else {
                    // Swapped backing rejected it: native cannot change V6ONLY after bind/connect -> EINVAL.
                    G_RET(c) = (uint64_t)(-EINVAL);
                }
                break;
            }
            int slot = ip6_shadow_slot((int)a2);
            if (slot >= 0) {
                int val = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
                g_ipopt_val[(int)a0][slot] = val;
                g_ipopt_set[(int)a0][slot] = 1;
                int mo = ip6_opt_l2m((int)a2);
                if (mo >= 0) (void)setsockopt((int)a0, IPPROTO_IPV6, mo, &val, sizeof val);
                G_RET(c) = 0;
                break;
            }
        }
        if (lvl == 1) {
            lvl = SOL_SOCKET;
            opt = so_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = 0;
                break;
            }
            // translate SOL_SOCKET; ignore unknown
        } else if (lvl == 6) { // IPPROTO_TCP: optnames diverge — translate, ignore unknown (never cork by accident)
            opt = tcp_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = 0;
                break;
            }
        } else if (lvl == 41) { // IPPROTO_IPV6: optnames diverge — translate (esp. IPV6_V6ONLY), ignore unknown
            opt = ip6_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = 0;
                break;
            }
        } else if (lvl == 0) { // IPPROTO_IP: optnames diverge — translate (IP_TOS/TTL/HDRINCL/mcast), ignore unknown
            opt = ip_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = 0;
                break;
            }
        }
        int r = setsockopt((int)a0, lvl, opt, (void *)a3, (socklen_t)a4);
        // A KNOWN-unsupported-but-harmless option already short-circuited to success above (opt<0). Anything
        // that reaches here is a real op on a translated/passthrough option; surface its true errno instead
        // of masking EINVAL/ENOPROTOOPT/EPERM (feature-probing code needs the real result).
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // getsockopt(fd, level, optname, val, len)
    case 209: {
        net_option_copyout option_copyout __attribute__((cleanup(net_option_copyout_finish)));
        int copyout_error = net_option_copyout_begin(&option_copyout, c, a3, a4);
        if (copyout_error < 0) {
            G_RET(c) = (uint64_t)(int64_t)copyout_error;
            break;
        }
        if (option_copyout.active) {
            a3 = (uint64_t)(uintptr_t)option_copyout.value;
            a4 = (uint64_t)(uintptr_t)&option_copyout.length;
        }
        int lvl = (int)a1, opt = (int)a2;
        int gfd = (int)a0;
        // A deferred asynchronous connect error (stashed for a non-blocking dial to a closed private-loopback
        // port) is delivered exactly once through SO_ERROR, mirroring a real TCP stack.
        if (lvl == 1 && opt == 4 && gfd >= 0 && gfd < HL_NFD && g_so_error[gfd]) {
            if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                *(int *)a3 = hl_linux_errno_from_macos(g_so_error[gfd]);
                *(socklen_t *)a4 = 4;
            }
            g_so_error[gfd] = 0;
            G_RET(c) = 0;
            break;
        }
        // SO_REUSEPORT(15): report the guest's recorded intent (an AF_UNIX switch socket always reads it
        // back as 0), for any tracked socket that had it set. An un-set fd falls through to the host value.
        if (lvl == 1 && opt == 15 && gfd >= 0 && gfd < HL_NFD && g_so_reuseport[gfd]) {
            if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                *(int *)a3 = 1;
                *(socklen_t *)a4 = 4;
            }
            G_RET(c) = 0;
            break;
        }
        // SO_DOMAIN(39)/SO_PROTOCOL(38): a private-loopback/bridge INET socket is backed on the host by an
        // AF_UNIX switch socket, so a raw getsockopt would report AF_UNIX/0 rather than the guest's real
        // AF_INET[6]/IPPROTO_TCP[UDP]. Report the family/protocol recorded at socket() for any tracked INET
        // socket -- identical to the host answer for an un-swapped fd, corrected for a swapped one.
        if (lvl == 1 && (opt == 38 || opt == 39) && gfd >= 0 && gfd < HL_NFD &&
            (g_sock_fam[gfd] == AF_INET || g_sock_fam[gfd] == LX_AF_INET6_FAM)) {
            int val = 0, have = 1;
            if (opt == 39)
                val = g_sock_fam[gfd]; // SO_DOMAIN: the guest address family
            else if (g_sock_stream[gfd])
                val = IPPROTO_TCP;
            else if (g_sock_dgram[gfd])
                val = IPPROTO_UDP;
            else
                have = 0; // unknown protocol -> fall through to the host value
            if (have) {
                if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                    *(int *)a3 = val;
                    *(socklen_t *)a4 = 4;
                }
                G_RET(c) = 0;
                break;
            }
        }
        // SO_PEERCRED (Linux SOL_SOCKET/17): macOS has no SO_PEERCRED. Report the peer's credentials as the
        // container identity (so cr.uid/gid match the guest's getuid/getgid) and the peer pid via macOS
        // LOCAL_PEERPID. struct ucred is { pid_t pid; uid_t uid; gid_t gid; } (3x u32 = 12 bytes).
        // SO_PASSCRED (16): report the per-fd flag we recorded at setsockopt (macOS has no such option).
        // Both SO_PASSCRED and SO_PEERCRED must first validate the fd like the kernel: EBADF for a closed fd,
        // ENOTSOCK for a regular file. Returning synthetic creds on a non-socket is fake success.
        if (lvl == 1 && (opt == 16 || opt == 17)) {
            int st_;
            socklen_t stl_ = sizeof st_;
            if (getsockopt((int)a0, SOL_SOCKET, SO_TYPE, &st_, &stl_) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
        }
        if (lvl == 1 && opt == 16) {
            if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                *(int *)a3 = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_sock_passcred[(int)a0] : 0;
                *(socklen_t *)a4 = 4;
            }
            G_RET(c) = 0;
            break;
        }
        if (lvl == 1 && opt == 17) {
            if (a3 && a4 && *(socklen_t *)a4 >= 12) {
                pid_t ppid = 0;
#if defined(__APPLE__)
                socklen_t pl = sizeof ppid;
                if (getsockopt((int)a0, SOL_LOCAL, LOCAL_PEERPID, &ppid, &pl) < 0 || ppid <= 0 || ppid == getpid()) {
#else
                struct ucred peer = {0};
                socklen_t pl = sizeof peer;
                if (getsockopt((int)a0, SOL_SOCKET, SO_PEERCRED, &peer, &pl) == 0) ppid = peer.pid;
                if (ppid <= 0 || ppid == getpid()) {
#endif
                    // macOS reports the socketpair CREATOR's pid on both ends -> a fork parent
                    // reads its OWN pid here for every child. Report the end's peer pid we resolved (the REAL
                    // guest pid of the process holding the OTHER end, stamped across fork/close -- see
                    // g_sock_peer_pid / seq_reassign_peer); else this guest's own pid.
                    int sp = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_sock_peer_pid[(int)a0] : 0;
#if defined(__linux__)
                    // On Linux the host SO_PEERCRED is authoritative: ppid==getpid() means the peer end is
                    // held in THIS process (an un-forked socketpair), whose guest pid is our own. A synthetic
                    // distinct id here would make SO_PEERCRED disagree with the guest's getpid() for a plain
                    // socketpair; report the guest self pid, keeping the synthetic only as a last resort.
                    ppid = (ppid == getpid()) ? container_pid() : (sp ? sp : container_pid());
#else
                    ppid = sp ? sp : container_pid();
#endif
                } else if (g_init_hostpid && ppid == g_init_hostpid) {
                    ppid = 1; // peer is the container init -> guest pid 1
                }
                uint32_t *u = (uint32_t *)a3;
                u[0] = (uint32_t)ppid; // pid (resolved above)
                // NOTE: peer uid/gid are reported as this container's identity, NOT the peer's real guest
                // uid/gid. A truthful per-peer value is infeasible here: (a) macOS LOCAL_PEERCRED yields the
                // peer's HOST uid, but every container process runs under the same host uid (guest uids are
                // emulated), and guest setuid is ownership-only (see setfsuid note), so LOCAL_PEERCRED can't
                // reflect a guest that dropped privileges; (b) cross-process we have no channel to the peer's
                // emulated guest uid. cuid()/cgid() is the closest available. Impact: Postgres `peer`/ident
                // auth and polkit/systemd uid checks see the container identity, not a setuid'd client uid.
                u[1] = (uint32_t)cuid(); // uid (see NOTE: container identity, not the peer's true guest uid)
                u[2] = (uint32_t)cgid(); // gid (see NOTE)
                *(socklen_t *)a4 = 12;
            }
            G_RET(c) = 0;
            break;
        }
        // SO_RCVTIMEO(20)/SO_SNDTIMEO(21) (+ _NEW 66/67): report the armed timeout back in the Linux
        // sock_timeval {s64 tv_sec; s64 tv_usec} layout, translated from the macOS struct timeval.
        if (lvl == 1 && (opt == 20 || opt == 21 || opt == 66 || opt == 67)) {
            struct timeval tv;
            memset(&tv, 0, sizeof tv);
            socklen_t tl = sizeof tv;
            int mo = (opt == 21 || opt == 67) ? SO_SNDTIMEO : SO_RCVTIMEO;
            int r = getsockopt((int)a0, SOL_SOCKET, mo, &tv, &tl);
            if (r < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (a3 && a4 && *(socklen_t *)a4 >= 16) {
                *(int64_t *)a3 = (int64_t)tv.tv_sec;
                *(int64_t *)((uint8_t *)a3 + 8) = (int64_t)tv.tv_usec;
                *(socklen_t *)a4 = 16;
            }
            G_RET(c) = 0;
            break;
        }
        // IPPROTO_TCP integer options on a tracked guest INET stream socket: report the shadowed value the
        // guest set (see case 208). If unset, prefer the real host value for a still-genuine AF_INET socket,
        // falling back to a stable default only when the AF_UNIX switch backing rejects the query.
        if (lvl == 6 && gfd >= 0 && gfd < HL_NFD && g_sock_stream[gfd]) {
            int slot = tcp_shadow_slot((int)a2);
            if (slot >= 0) {
                int val;
                if (g_tcp_optset[gfd][slot]) {
                    val = g_tcp_optval[gfd][slot];
                } else {
                    val = tcp_shadow_default(slot);
                    int hv;
                    socklen_t hl = sizeof hv;
                    int mo = tcp_opt_l2m((int)a2);
                    if (mo >= 0 && getsockopt(gfd, IPPROTO_TCP, mo, &hv, &hl) == 0) val = hv;
                }
                if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                    *(int *)a3 = val;
                    *(socklen_t *)a4 = 4;
                }
                G_RET(c) = 0;
                break;
            }
            // TCP_INFO(11): a struct the AF_UNIX switch backing cannot answer. Try the host first (an unswapped
            // AF_INET socket returns the real thing); on rejection synthesize a minimal record whose only stable
            // fact is tcpi_state (byte 0) = ESTABLISHED for a connected socket, so diagnostic code sees a live
            // connection instead of ENOPROTOOPT. The rest is zero-filled.
            if ((int)a2 == 11 && a3 && a4) {
                socklen_t cap = *(socklen_t *)a4;
#if defined(__linux__)
                // Linux TCP_INFO == 11; an unswapped AF_INET socket answers it authoritatively.
                if (getsockopt(gfd, IPPROTO_TCP, 11, (void *)a3, (socklen_t *)a4) == 0) {
                    G_RET(c) = 0;
                    break;
                }
#endif
                socklen_t n = cap < 512 ? cap : 512;
                memset((void *)a3, 0, n);
                if (n >= 1) *(uint8_t *)a3 = g_sock_conn[gfd] ? 1 /*TCP_ESTABLISHED*/ : 7 /*TCP_CLOSE*/;
                *(socklen_t *)a4 = n;
                G_RET(c) = 0;
                break;
            }
        }
        // IPPROTO_IP integer options on a tracked AF_INET socket: report the shadowed value the guest set.
        // If unset, prefer the real host value (a still-genuine AF_INET socket answers it), falling back to
        // the Linux default only when the AF_UNIX switch backing rejects the query.
        if (lvl == 0 && gfd >= 0 && gfd < HL_NFD && g_sock_fam[gfd] == AF_INET) {
            int slot = ip_shadow_slot((int)a2);
            if (slot >= 0) {
                int val;
                if (g_ipopt_set[gfd][slot]) {
                    val = g_ipopt_val[gfd][slot];
                } else {
                    val = ipopt_shadow_default(slot);
                    int hv;
                    socklen_t hl = sizeof hv;
                    int mo = ip_opt_l2m((int)a2);
                    if (mo >= 0 && getsockopt(gfd, IPPROTO_IP, mo, &hv, &hl) == 0) val = hv;
                }
                if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                    *(int *)a3 = val;
                    *(socklen_t *)a4 = 4;
                }
                G_RET(c) = 0;
                break;
            }
        }
        // IPPROTO_IPV6 integer options on a tracked AF_INET6 socket, including IPV6_V6ONLY(26) whose readback
        // must survive bind (dual-stack servers set it v6-only then read it back).
        if (lvl == 41 && gfd >= 0 && gfd < HL_NFD && g_sock_fam[gfd] == LX_AF_INET6_FAM) {
            int slot = ((int)a2 == 26) ? IPOPT_V6ONLY_SLOT : ip6_shadow_slot((int)a2);
            if (slot >= 0) {
                int val;
                if (g_ipopt_set[gfd][slot]) {
                    val = g_ipopt_val[gfd][slot];
                } else {
                    val = ipopt_shadow_default(slot);
                    int hv;
                    socklen_t hl = sizeof hv;
                    int mo = ip6_opt_l2m((int)a2);
                    if (mo >= 0 && getsockopt(gfd, IPPROTO_IPV6, mo, &hv, &hl) == 0) val = hv;
                }
                if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                    *(int *)a3 = val;
                    *(socklen_t *)a4 = 4;
                }
                G_RET(c) = 0;
                break;
            }
        }
        if (lvl == 1) {
            lvl = SOL_SOCKET;
            opt = so_opt_l2m((int)a2);
            if (opt < 0) { // genuinely-unknown SOL_SOCKET optname -> Linux getsockopt returns ENOPROTOOPT
                G_RET(c) = (uint64_t)(-ENOPROTOOPT);
                break;
            }
        } else if (lvl == 6) { // IPPROTO_TCP: translate optname; unknown -> ENOPROTOOPT
            opt = tcp_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = (uint64_t)(-ENOPROTOOPT);
                break;
            }
        } else if (lvl == 41) { // IPPROTO_IPV6: translate optname (esp. IPV6_V6ONLY); unknown -> ENOPROTOOPT
            opt = ip6_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = (uint64_t)(-ENOPROTOOPT);
                break;
            }
        } else if (lvl == 0) { // IPPROTO_IP: translate optname; unknown -> ENOPROTOOPT
            opt = ip_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = (uint64_t)(-ENOPROTOOPT);
                break;
            }
        }
        int r = getsockopt((int)a0, lvl, opt, (void *)a3, (socklen_t *)a4);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 210:
        G_RET(c) = shutdown((int)a0, (int)a1) < 0 ? (uint64_t)(-errno) : 0;
        // shutdown(fd, how) -- SHUT_RD/WR/RDWR match
        break;
    case 211:
    // sendmsg/recvmsg -- translate Linux msghdr -> macOS
    case 212: {
        net_message_bounce message_bounce __attribute__((cleanup(net_message_bounce_finish)));
        int bounce_error = net_message_bounce_begin(&message_bounce, c, a1, nr == 212);
        if (bounce_error < 0) {
            G_RET(c) = (uint64_t)(int64_t)bounce_error;
            break;
        }
        uint8_t *g = message_bounce.header;
        // The msghdr struct itself is read (iov ptr/count, name, control, flags) AND written back (recvmsg's
        // namelen/controllen/flags) directly through `g`; a NULL or unmapped msghdr pointer would fault the
        // engine on the very first field access below. Validate it up front so a bad pointer yields EFAULT
        // (like the kernel's copy_from_user), matching the sendmmsg/recvmmsg array guard. Linux msghdr is 56 B.
        uint64_t giov_count = *(uint64_t *)(g + 24);
        struct iovec rebased_iov[1024];
        struct iovec *guest_iov = (struct iovec *)net_nonpie_p(*(uint64_t *)(g + 16));
        if (giov_count > 1024 ||
            (giov_count && guest_bad_ptr((uintptr_t)guest_iov, (size_t)giov_count * sizeof *guest_iov))) {
            G_RET(c) = (uint64_t)(giov_count > 1024 ? -EMSGSIZE : -EFAULT);
            break;
        }
        for (uint64_t i = 0; i < giov_count; ++i) {
            rebased_iov[i] = guest_iov[i];
            rebased_iov[i].iov_base = (void *)net_nonpie_p((uint64_t)(uintptr_t)guest_iov[i].iov_base);
        }
        // msg_name is a guest pointer INSIDE the (already-validated) msghdr; the DNS/icmp classifiers and the
        // sockaddr translator below all deref it directly. A wild pointer (e.g. (void*)-1) must yield EFAULT,
        // not fault the engine -- validate it once here, before any of those derefs (dns_dest_is reads 8 B).
        {
            uint8_t *mn = (uint8_t *)net_nonpie_p(*(uint64_t *)(g + 0));
            socklen_t mnl = *(uint32_t *)(g + 8);
            if (mn && mnl && guest_bad_ptr((uintptr_t)mn, mnl)) {
                G_RET(c) = (uint64_t)(int64_t)-EFAULT;
                break;
            }
        }
        // Oversized UDP datagram -> EMSGSIZE, before routing (see udp_dgram_maxlen).
        if (nr == 211) {
            size_t udp_cap = udp_dgram_maxlen((int)a0);
            if (udp_cap) {
                size_t tot = 0;
                for (uint64_t i = 0; i < giov_count; ++i)
                    tot += rebased_iov[i].iov_len;
                if (tot > udp_cap) {
                    G_RET(c) = (uint64_t)(-EMSGSIZE);
                    break;
                }
            }
        }
        // Container DNS: a sendmsg carrying a query to 127.0.0.11:53 (or on an already-swapped DNS socket).
        if (nr == 211 && dns_enabled()) {
            int dfd = (int)a0;
            uint8_t *nm = (uint8_t *)net_nonpie_p(*(uint64_t *)(g + 0));
            socklen_t nml = *(uint32_t *)(g + 8);
            if ((dfd >= 0 && dfd < HL_NFD && g_dns_sock[dfd]) || dns_dest_is(nm, nml)) {
                uint8_t tmp[2048];
                size_t tl = dns_gather(rebased_iov, (int)giov_count, tmp, sizeof tmp);
                int64_t dret;
                if (dns_try_send(dfd, tmp, tl, nm, nml, &dret)) {
                    G_RET(c) = (uint64_t)dret;
                    break;
                }
            }
        }
        if (nr == 211 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_kind[(int)a0]) {
            uint8_t tmp[2048];
            uint8_t *nm = (uint8_t *)net_nonpie_p(*(uint64_t *)(g + 0));
            socklen_t nml = *(uint32_t *)(g + 8);
            size_t tl = dns_gather(rebased_iov, (int)giov_count, tmp, sizeof tmp);
            int64_t iret;
            if (icmp_try_send((int)a0, tmp, tl, nm, nml, &iret)) {
                G_RET(c) = (uint64_t)iret;
                break;
            }
        }
        struct msghdr mh;
        // Linux: iovlen/controllen are 8-byte; macOS 4
        memset(&mh, 0, sizeof mh);
        mh.msg_name = (void *)net_nonpie_p(*(uint64_t *)(g + 0));
        mh.msg_namelen = *(uint32_t *)(g + 8);
        mh.msg_iov = rebased_iov;
        mh.msg_iovlen = (int)giov_count;
        mh.msg_flags = *(uint32_t *)(g + 48);
        // msg_name sockaddr: Linux<->macOS translation through a host scratch (AF_INET/INET6 only).
        struct sockaddr_storage nss;
        uint8_t *gname = (uint8_t *)mh.msg_name;
        socklen_t gnamelen = mh.msg_namelen;
        char ud_host[1200];
        int ud_route = 0;                     // AF_UNIX pathname/abstract dgram dest -> overlay/abstract route on send
        if (nr == 211 && gname && gnamelen) { // sendmsg: guest -> host
            int udp_interface;
            uint32_t udp_ip;
            uint16_t udp_port;
            int udp_route = (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]
                                ? udp_switch_destination(gname, gnamelen, &udp_interface, &udp_ip, &udp_port, ud_host,
                                                         sizeof ud_host)
                                : 0;
            if (udp_route < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (udp_route > 0) {
                if (udp_switch_ensure_source((int)a0, udp_interface) < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
                ud_route = 1;
            } else if (gnamelen >= 2 && *(const uint16_t *)gname == AF_UNIX &&
                       unix_dgram_dest(gname, gnamelen, ud_host, sizeof ud_host)) {
                ud_route = 1; // sent via unix_dgram_sendmsg_at below (it owns msg_name)
            } else {
                socklen_t hl = sa_l2m(gname, gnamelen, &nss);
                if (hl != (socklen_t)-1) {
                    // #261 IPv4-only network: an external IPv6 dest has no route -> ENETUNREACH now (mirrors
                    // connect()/sendto), so a v6-first datagram client falls back to IPv4 immediately.
                    if (v6_no_route((struct sockaddr *)&nss)) {
                        G_RET(c) = (uint64_t)(-ENETUNREACH);
                        break;
                    }
                    mh.msg_name = &nss;
                    mh.msg_namelen = hl;
                }
            }
        } else if (nr == 211 && !gname && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
            int udp_route = udp_switch_peer_path((int)a0, ud_host, sizeof ud_host);
            if (udp_route < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            ud_route = udp_route > 0;
        } else if (nr == 212 && gname && gnamelen) { // recvmsg: receive into host scratch
            mh.msg_name = &nss;
            mh.msg_namelen = sizeof nss;
        }
        // Ancillary data: the guest control buf is Linux-cmsg layout; macOS reads a different cmsghdr,
        // so route it through a host-layout scratch buffer (NULL-control left untouched, so edge/msgflags
        // with no control buffer stays on the old path).
        uint8_t *gc = (void *)net_nonpie_p(*(uint64_t *)(g + 32));
        size_t gcl = *(uint64_t *)(g + 40);
        size_t hcap = 0;
        if (gc && gcl) {
            if (gcl > (SIZE_MAX - 256) / 3) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            hcap = CMSG_SPACE(gcl * 3 + 256);
        }
        if (hcap < 4096) hcap = 4096;
        uint8_t hstack[4096];
        uint8_t *hctl = hcap <= sizeof hstack ? hstack : malloc(hcap);
        if (hcap && !hctl) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if (nr == 211) { // sendmsg: translate guest -> host before the call
            // Ancillary data may carry SCM_RIGHTS fds to another process; flush all RAM-backed scratch so a
            // passed fd (and any other) is a coherent host file on the receiving side.
            if (gc && gcl) memf_materialize_all();
            int cerr = 0;
            int engine_metadata = (int)a0 < 0 || (int)a0 >= HL_NFD || !g_sock_native_peer[(int)a0];
            ssize_t hn = (gc && gcl) ? cmsg_l2m(gc, gcl, hctl, hcap, engine_metadata, &cerr) : 0;
            if (hn < 0) {
                cmsg_tmpfds_close();
                cmsg_seq_finish(0);
                cmsg_event_finish(0);
                cmsg_inflight_finish(0);
                if (hctl != hstack) free(hctl);
                G_RET(c) = (uint64_t)(-(cerr ? cerr : EINVAL));
                break;
            }
            mh.msg_control = hn > 0 ? hctl : NULL;
            mh.msg_controllen = hn > 0 ? (socklen_t)hn : 0;
        } else { // recvmsg: receive into host scratch
            if (gc && gcl) memset(hctl, 0, hcap);
            mh.msg_control = (gc && gcl) ? hctl : NULL;
            mh.msg_controllen = (gc && gcl) ? (socklen_t)hcap : 0;
        }
        // MSG_NOSIGNAL(0x4000) -> SO_NOSIGPIPE (macOS has no per-call flag); EPIPE instead of SIGPIPE.
        if (nr == 211 && ((int)a2 & 0x4000)) { (void)hl_native_set_no_sigpipe((int)a0); }
        // Zero-length address-peek idiom (recvmsg): if the guest wants the sender but supplies no receive
        // room, macOS returns 0 at once without filling msg_name (see dgram_addr_peek). Receive into a
        // 1-byte scratch iov so it blocks and reports the source; MSG_PEEK keeps the datagram queued.
        char one;
        struct iovec sciov = {&one, 1};
        int peekaddr = 0;
        if (nr == 212) {
            size_t totlen = 0;
            struct iovec *iv = (struct iovec *)mh.msg_iov;
            for (int i = 0; iv && i < (int)mh.msg_iovlen; i++)
                totlen += iv[i].iov_len;
            if ((peekaddr = dgram_addr_peek((int)a0, gname && gnamelen, totlen))) {
                mh.msg_iov = &sciov;
                mh.msg_iovlen = 1;
            }
        }
        ssize_t r;
        do {
            r = (nr == 211) ? (ud_route ? (ssize_t)unix_dgram_sendmsg_at((int)a0, ud_host, &mh, msgflags_l2m((int)a2))
                                        : sendmsg((int)a0, &mh, msgflags_l2m((int)a2)))
                            : recvmsg((int)a0, &mh, msgflags_l2m((int)a2));
        } while (r < 0 && SVC_EINTR_RESTART(c));
        if (nr == 211) cmsg_tmpfds_close();
        if (nr == 211) cmsg_seq_finish(r >= 0);
        if (nr == 211) cmsg_event_finish(r >= 0);
        if (nr == 211) cmsg_inflight_finish(r >= 0);
        if (r > 0 && peekaddr) r = 0; // guest supplied no data room; only the source address was wanted
        // SEQPACKET-as-DGRAM EOF: coerce a peer-closed recvmsg's ECONNRESET to 0 (EOF). (See case 199.)
        if (nr == 212 && r < 0 && errno == ECONNRESET && seq_is((int)a0)) r = 0;
        if (nr == 212 && r >= 0) {
            // recvmsg writes back name len + (host->guest) control + translated flags
            if (gname && gnamelen && (int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_sock[(int)a0]) {
                fill_inet_br(gname, NULL, g_icmp_ip[(int)a0], 0);
                *(uint32_t *)(g + 8) = 16;
            } else if (gname && gnamelen && (int)a0 >= 0 && (int)a0 < HL_NFD && g_dns_sock[(int)a0]) {
                // DNS socket: report the nameserver (127.0.0.11:53) as the source (see case 207).
                dns_fill_ns(gname, NULL);
                *(uint32_t *)(g + 8) = 16;
            } else if (gname && gnamelen && udp_switch_source(&nss, mh.msg_namelen, gname, (socklen_t *)(g + 8))) {
                // AF_UNIX switch sender restored to its Linux AF_INET identity.
            } else if (gname && gnamelen) { // translate received host sockaddr back to Linux layout
                int ll = sa_m2l((struct sockaddr *)&nss, gname, gnamelen);
                *(uint32_t *)(g + 8) = (ll >= 0) ? (uint32_t)ll : mh.msg_namelen;
                if (ll < 0 && mh.msg_namelen) // non-inet: copy raw host bytes back
                    memcpy(gname, &nss, mh.msg_namelen < gnamelen ? mh.msg_namelen : gnamelen);
            } else
                *(uint32_t *)(g + 8) = mh.msg_namelen;
            // SO_PASSCRED: the Linux kernel auto-attaches an SCM_CREDENTIALS record with the peer's ucred to
            // every received message; macOS does not, so synthesize it (uid/gid = container identity, pid =
            // the peer's -- LOCAL_PEERPID, mapping the container init's host pid back to guest pid 1, self as
            // the container pid). IPC bootstrap may abort with "missing credentials" without it.
            int passcred_active = gc && gcl && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_passcred[(int)a0];
            int synth_passcred = passcred_active;
#if defined(__linux__)
            // The Linux kernel attaches the sender's real SCM_CREDENTIALS.  Synthesizing the Darwin
            // fallback here would replace a post-fork sender pid with the socketpair creator's pid.
            synth_passcred = 0;
#endif
            int cred_trunc = 0;
            size_t ln = 0;
            if (synth_passcred) {
                int ppid = 0;
#if defined(__APPLE__)
                socklen_t pl = sizeof ppid;
                if (getsockopt((int)a0, SOL_LOCAL, LOCAL_PEERPID, &ppid, &pl) < 0 || ppid <= 0 || ppid == getpid()) {
#else
                struct ucred peer = {0};
                socklen_t pl = sizeof peer;
                if (getsockopt((int)a0, SOL_SOCKET, SO_PEERCRED, &peer, &pl) == 0) ppid = peer.pid;
                if (ppid <= 0 || ppid == getpid()) {
#endif
                    // macOS reports the socketpair CREATOR's pid on both ends (never updated on fork), so the
                    // fork parent reads its OWN pid here for every child -> container_pid() collapsed all of
                    // them to guest 1, colliding peer node identities. Prefer the end's distinct synthetic
                    // peer node id stamped at socketpair(); fall back to this guest's own pid only if unstamped.
                    int sp = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_sock_peer_pid[(int)a0] : 0;
                    ppid = sp ? sp : container_pid();
                } else if (g_init_hostpid && ppid == g_init_hostpid)
                    ppid = 1;
                size_t ln2 = cmsg_add_cred(gc, 0, gcl, ppid, cuid(), cgid());
                if (ln2 == 0)
                    cred_trunc = 1; // no room for the Linux-mandated credentials record
                else
                    ln = ln2;
            }
            int cmsg_trunc = 0;
            if (gc && gcl) ln = (size_t)cmsg_m2l(&mh, gc, gcl, ln, &cmsg_trunc);
            int host_mflags = (int)mh.msg_flags;
            if (!cmsg_trunc && gc && gcl) host_mflags &= ~MSG_CTRUNC; // host-side sideband expansion compressed cleanly
            int mfl = msgflags_m2l(host_mflags);
            if (cred_trunc || (cmsg_trunc && !passcred_active)) mfl |= 0x8;          // MSG_CTRUNC
            if (((int)a2 & 0x40000000) && gc && ln) cmsg_lx_set_cloexec_fds(gc, ln); // MSG_CMSG_CLOEXEC
            *(uint64_t *)(g + 40) = ln;
            *(uint32_t *)(g + 48) = (uint32_t)mfl;
        }
        if (hctl != hstack) free(hctl);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        // sendmsg to a peer-closed socket -> guest SIGPIPE (Linux), unless MSG_NOSIGNAL was requested.
        if (nr == 211 && !((int)a2 & 0x4000)) svc_sigpipe_on_epipe(c, (int64_t)G_RET(c));
        break;
    }
    case 269:
    // sendmmsg/recvmmsg(fd, mmsghdr[], vlen, flags, [timeout])
    case 243: {
        uint8_t *vec = (uint8_t *)a1;
        unsigned vlen = (unsigned)a2;
        if (vlen > 1024) {
            G_RET(c) = (uint64_t)-EINVAL;
            break;
        }
        /*
         * Drive each element through the already-audited single-message path.
         * This preserves Linux's partial-success contract while sharing the
         * canonical msghdr/iovec/name/control staging and ancillary-data
         * translation with sendmsg/recvmsg.
         */
        size_t vector_bytes = (size_t)vlen * 64;
        if (vector_bytes && (guest_accessible_prefix(a1, vector_bytes, PROT_READ) != vector_bytes ||
                             guest_accessible_prefix(a1, vector_bytes, PROT_WRITE) != vector_bytes)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        unsigned completed = 0;
        for (; completed < vlen; ++completed) {
            uint8_t message[64];
            uint64_t message_address = a1 + (size_t)completed * sizeof(message);
            if (guest_copy_from(message, message_address, sizeof(message)) != sizeof(message)) {
                if (!completed) G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            uint64_t nested_flags = a3;
            if (nr == 243 && completed) nested_flags |= 0x40; /* MSG_DONTWAIT / WAITFORONE */
            (void)svc_net(c, nr == 269 ? 211 : 212, a0, (uint64_t)(uintptr_t)message, nested_flags, 0, 0, 0);
            int64_t result = (int64_t)G_RET(c);
            if (result < 0) {
                if (!completed) return 1; /* nested svc_done already translated the errno */
                break;
            }
            uint32_t message_length = (uint32_t)result;
            memcpy(message + 56, &message_length, sizeof(message_length));
            if (guest_copy_to(message_address, message, sizeof(message)) != sizeof(message)) {
                if (!completed) G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (completed || vlen == 0) G_RET(c) = completed;
        break;
        // The mmsghdr array itself is read AND written (msg_len/msg_namelen) per submessage; a straddling or
        // unmapped vec would fault the engine on the very first field access below. Validate the whole array up
        // front so a bad pointer yields EFAULT (like the kernel's copy_from_user) instead of a guest-crashes-engine
        // SIGSEGV. Each mmsghdr is 64 bytes (msghdr 56 + msg_len 4 + pad).
        if (vlen && guest_bad_ptr((uintptr_t)vec, (size_t)vlen * 64)) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        // mmsghdr = msghdr(56) + msg_len(4) + pad
        // Container DNS: glibc's default parallel A+AAAA lookup sends BOTH queries to the nameserver in one
        // sendmmsg. Answer each submessage via the host resolver; the responses are drained by recvfrom (207).
        if (nr == 269 && dns_enabled() && vlen) {
            int dfd = (int)a0;
            uint8_t *g0 = vec;
            uint8_t *nm0 = (uint8_t *)net_nonpie_p(*(uint64_t *)(g0 + 0));
            socklen_t nml0 = *(uint32_t *)(g0 + 8);
            int is_dns = (dfd >= 0 && dfd < HL_NFD && g_dns_sock[dfd]);
            // nm0 is submessage 0's guest msg_name; dns_dest_is derefs it (8 B). A wild pointer must not fault
            // the engine here -- if unreadable, skip DNS classification and let the main loop below return
            // EFAULT for this submessage (its per-iter msg_name guard). See the sendmsg guard above.
            if (nm0 && nml0 && guest_bad_ptr((uintptr_t)nm0, nml0)) {
                nm0 = NULL;
                nml0 = 0;
            }
            if (!is_dns && dns_dest_is(nm0, nml0) &&
                dns_swap(dfd, (dfd >= 0 && dfd < HL_NFD) ? g_sock_stream[dfd] : 0) == 0)
                is_dns = 1;
            if (is_dns) {
                int stream = (dfd >= 0 && dfd < HL_NFD) ? g_sock_stream[dfd] : 0;
                unsigned n;
                for (n = 0; n < vlen; n++) {
                    uint8_t *g = vec + (size_t)n * 64;
                    uint64_t ivn = *(uint64_t *)(g + 24);
                    struct iovec riv[1024];
                    struct iovec *iv = (struct iovec *)net_nonpie_p(*(uint64_t *)(g + 16));
                    if (ivn > 1024 || (ivn && guest_bad_ptr((uintptr_t)iv, (size_t)ivn * sizeof *iv))) {
                        G_RET(c) = (uint64_t)(ivn > 1024 ? -EMSGSIZE : -EFAULT);
                        goto mmsg_done;
                    }
                    for (uint64_t j = 0; j < ivn; ++j) {
                        riv[j] = iv[j];
                        riv[j].iov_base = (void *)net_nonpie_p((uint64_t)(uintptr_t)iv[j].iov_base);
                    }
                    uint8_t tmp[2048];
                    size_t tl = dns_gather(riv, (int)ivn, tmp, sizeof tmp);
                    dns_send(dfd, tmp, tl, stream);
                    *(uint32_t *)(g + 56) = (uint32_t)tl; // msg_len: whole query accepted
                }
                G_RET(c) = (uint64_t)vlen;
                break;
            }
        }
        int done = 0, err = 0;
        // MSG_NOSIGNAL(0x4000) -> SO_NOSIGPIPE once before the fan-out (macOS has no per-call flag).
        if (nr == 269 && ((int)a3 & 0x4000)) { (void)hl_native_set_no_sigpipe((int)a0); }
        for (unsigned i = 0; i < vlen; i++) {
            uint8_t *g = vec + (size_t)i * 64;
            struct msghdr mh;
            uint64_t giov_count = *(uint64_t *)(g + 24);
            struct iovec rebased_iov[1024];
            struct iovec *guest_iov = (struct iovec *)net_nonpie_p(*(uint64_t *)(g + 16));
            if (giov_count > 1024 ||
                (giov_count && guest_bad_ptr((uintptr_t)guest_iov, (size_t)giov_count * sizeof *guest_iov))) {
                err = giov_count > 1024 ? EMSGSIZE : EFAULT;
                break;
            }
            for (uint64_t j = 0; j < giov_count; ++j) {
                rebased_iov[j] = guest_iov[j];
                rebased_iov[j].iov_base = (void *)net_nonpie_p((uint64_t)(uintptr_t)guest_iov[j].iov_base);
            }
            // Oversized UDP datagram -> EMSGSIZE for this submessage (prior sends still count).
            if (nr == 269) {
                size_t udp_cap = udp_dgram_maxlen((int)a0);
                if (udp_cap) {
                    size_t tot = 0;
                    for (uint64_t j = 0; j < giov_count; ++j)
                        tot += rebased_iov[j].iov_len;
                    if (tot > udp_cap) {
                        err = EMSGSIZE;
                        break;
                    }
                }
            }
            memset(&mh, 0, sizeof mh);
            mh.msg_name = (void *)net_nonpie_p(*(uint64_t *)(g + 0));
            mh.msg_namelen = *(uint32_t *)(g + 8);
            mh.msg_iov = rebased_iov;
            mh.msg_iovlen = (int)giov_count;
            mh.msg_flags = *(uint32_t *)(g + 48);
            // msg_name sockaddr: Linux<->macOS translation through a host scratch (AF_INET/INET6 only).
            struct sockaddr_storage nss;
            uint8_t *gname = (uint8_t *)mh.msg_name;
            socklen_t gnamelen = mh.msg_namelen;
            // Wild per-submessage msg_name must yield EFAULT for this submessage, not fault the engine (the
            // family probe / udp_switch_destination / sa_l2m below all deref gname). Mirrors the iov guard.
            if (gname && gnamelen && guest_bad_ptr((uintptr_t)gname, gnamelen)) {
                err = EFAULT;
                break;
            }
            char ud_host[1200];
            int ud_route = 0; // AF_UNIX pathname/abstract dgram dest -> overlay/abstract route on send
            if (nr == 269 && gname && gnamelen) { // sendmmsg: guest -> host
                int udp_interface;
                uint32_t udp_ip;
                uint16_t udp_port;
                int udp_route = (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]
                                    ? udp_switch_destination(gname, gnamelen, &udp_interface, &udp_ip, &udp_port,
                                                             ud_host, sizeof ud_host)
                                    : 0;
                if (udp_route < 0) {
                    err = errno;
                    break;
                }
                if (udp_route > 0) {
                    // AF_INET(6) dest over the private-loopback switch: resolve to the peer AF_UNIX path
                    // and send there (mirrors sendto/sendmsg cases 206/211).
                    if (udp_switch_ensure_source((int)a0, udp_interface) < 0) {
                        err = errno;
                        break;
                    }
                    ud_route = 1;
                } else if (gnamelen >= 2 && *(const uint16_t *)gname == AF_UNIX &&
                           unix_dgram_dest(gname, gnamelen, ud_host, sizeof ud_host)) {
                    ud_route = 1; // sent via unix_dgram_sendmsg_at below (it owns msg_name)
                } else {
                    socklen_t hl = sa_l2m(gname, gnamelen, &nss);
                    if (hl != (socklen_t)-1) {
                        mh.msg_name = &nss;
                        mh.msg_namelen = hl;
                    }
                }
            } else if (nr == 269 && !gname && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
                int udp_route = udp_switch_peer_path((int)a0, ud_host, sizeof ud_host);
                if (udp_route < 0) {
                    err = errno;
                    break;
                }
                ud_route = udp_route > 0; // connected UDP over the switch: send to the recorded peer AF_UNIX path
            } else if (nr == 243 && gname && gnamelen) { // recvmmsg: receive into host scratch
                mh.msg_name = &nss;
                mh.msg_namelen = sizeof nss;
            }
            // Ancillary data: route the per-submessage control buf through a host-layout scratch buffer.
            uint8_t *gc = (void *)net_nonpie_p(*(uint64_t *)(g + 32));
            size_t gcl = *(uint64_t *)(g + 40);
            size_t hcap = 0;
            if (gc && gcl) {
                if (gcl > (SIZE_MAX - 256) / 3) {
                    err = ENOMEM;
                    break;
                }
                hcap = CMSG_SPACE(gcl * 3 + 256);
            }
            if (hcap < 4096) hcap = 4096;
            uint8_t hstack[4096];
            uint8_t *hctl = hcap <= sizeof hstack ? hstack : malloc(hcap);
            if (hcap && !hctl) {
                err = ENOMEM;
                break;
            }
            if (nr == 269) { // sendmmsg: translate guest -> host
                int cerr = 0;
                int engine_metadata = (int)a0 < 0 || (int)a0 >= HL_NFD || !g_sock_native_peer[(int)a0];
                ssize_t hn = (gc && gcl) ? cmsg_l2m(gc, gcl, hctl, hcap, engine_metadata, &cerr) : 0;
                if (hn < 0) {
                    cmsg_tmpfds_close();
                    cmsg_seq_finish(0);
                    cmsg_event_finish(0);
                    cmsg_inflight_finish(0);
                    if (hctl != hstack) free(hctl);
                    err = cerr ? cerr : EINVAL;
                    break;
                }
                mh.msg_control = hn > 0 ? hctl : NULL;
                mh.msg_controllen = hn > 0 ? (socklen_t)hn : 0;
            } else { // recvmmsg: receive into host scratch
                if (gc && gcl) memset(hctl, 0, hcap);
                mh.msg_control = (gc && gcl) ? hctl : NULL;
                mh.msg_controllen = (gc && gcl) ? (socklen_t)hcap : 0;
            }
            int rf = (int)a3;
            // after the first, don't block (MSG_WAITFORONE-ish)
            if (nr == 243 && i > 0) rf |= 0x40;
            ssize_t r = (nr == 269)
                            ? (ud_route ? (ssize_t)unix_dgram_sendmsg_at((int)a0, ud_host, &mh, msgflags_l2m(rf))
                                        : sendmsg((int)a0, &mh, msgflags_l2m(rf)))
                            : recvmsg((int)a0, &mh, msgflags_l2m(rf));
            if (nr == 269) cmsg_tmpfds_close();
            if (nr == 269) cmsg_event_finish(r >= 0);
            if (nr == 269) cmsg_inflight_finish(r >= 0);
            if (r < 0) {
                err = errno;
                if (hctl != hstack) free(hctl);
                break;
            }
            // msg_len
            *(uint32_t *)(g + 56) = (uint32_t)r;
            if (nr == 243) {
                if (gname && gnamelen && (int)a0 >= 0 && (int)a0 < HL_NFD && g_dns_sock[(int)a0]) {
                    dns_fill_ns(gname, NULL); // DNS socket: source is the nameserver (see case 207)
                    *(uint32_t *)(g + 8) = 16;
                } else if (gname && gnamelen) { // translate received host sockaddr back to Linux layout
                    int ll = sa_m2l((struct sockaddr *)&nss, gname, gnamelen);
                    *(uint32_t *)(g + 8) = (ll >= 0) ? (uint32_t)ll : mh.msg_namelen;
                    if (ll < 0 && mh.msg_namelen)
                        memcpy(gname, &nss, mh.msg_namelen < gnamelen ? mh.msg_namelen : gnamelen);
                } else
                    *(uint32_t *)(g + 8) = mh.msg_namelen;
                int cmsg_trunc = 0;
                size_t ln = (gc && gcl) ? (size_t)cmsg_m2l(&mh, gc, gcl, 0, &cmsg_trunc) : 0;
                if (((int)a3 & 0x40000000) && gc && ln) cmsg_lx_set_cloexec_fds(gc, ln); // MSG_CMSG_CLOEXEC
                *(uint64_t *)(g + 40) = ln;
                int host_mflags = (int)mh.msg_flags;
                if (!cmsg_trunc && gc && gcl)
                    host_mflags &= ~MSG_CTRUNC; // host-side sideband expansion compressed cleanly
                int mfl = msgflags_m2l(host_mflags);
                if (cmsg_trunc) mfl |= 0x8; // MSG_CTRUNC
                *(uint32_t *)(g + 48) = (uint32_t)mfl;
            }
            if (hctl != hstack) free(hctl);
            done++;
        }
        G_RET(c) = (done == 0 && err) ? (uint64_t)(-(int64_t)err) : (uint64_t)done;
    mmsg_done:
        break;
    }
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
