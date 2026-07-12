// Extracted from service(): Network -- sockets/bind/connect/accept/send/recv + socketopt; port-map (-p) and
// the private NET-ns loopback (af_l2m / cmsg / msg-flag translation live in container/netns.c). Returns 1 if
// nr was handled, 0 otherwise. Included by service.c after service/io.c, before service() -- same TU scope.

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

// IPPROTO_IPV6 optname: Linux -> macOS. CRITICAL: like IPPROTO_TCP, these numbers diverge, so a raw
// pass-through silently sets the WRONG option. The load-bearing case is IPV6_V6ONLY (Linux 26 -> macOS 27):
// leaving it untranslated hits macOS's optname 26 (unrelated) instead, so a wildcard `::` bind stays
// dual-stack on the host and reserves the v4 wildcard too -- a later 0.0.0.0 bind on the same port then
// fails EADDRINUSE (breaks dual-stack servers like MariaDB that bind :: v6-only + 0.0.0.0 separately).
// Map the known ones; ignore (-1) unknown rather than pass a Linux number straight to macOS IPPROTO_IPV6.
static int ip6_opt_l2m(int o) {
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
}

// IPPROTO_IP (level 0) optname: Linux -> macOS. Like TCP/IPV6 the numbers diverge (Linux IP_TOS=1/IP_TTL=2/
// IP_HDRINCL=3 vs macOS IP_OPTIONS=1/IP_HDRINCL=2/IP_TOS=3), so a raw pass-through sets the WRONG option
// (e.g. Linux IP_TTL(2) lands on macOS IP_HDRINCL(2)). Map the options whose macOS payload struct matches
// (int TOS/TTL/HDRINCL/loop/mcast-ttl, in_addr mcast-if, ip_mreq membership); ignore (-1) unknown / no-mac-
// equivalent ones (IP_PKTINFO/IP_MTU_DISCOVER/IP_RECVERR/IP_FREEBIND: no macOS analogue or a divergent struct).
static int ip_opt_l2m(int o) {
    switch (o) {
    case 1: return 3;    // IP_TOS
    case 2: return 4;    // IP_TTL
    case 3: return 2;    // IP_HDRINCL
    case 4: return 1;    // IP_OPTIONS
    case 6: return 5;    // IP_RECVOPTS
    case 7: return 8;    // IP_RETOPTS
    case 12: return 24;  // IP_RECVTTL
    case 32: return 9;   // IP_MULTICAST_IF   (in_addr; ip_mreqn extras are Linux-only -> best-effort)
    case 33: return 10;  // IP_MULTICAST_TTL
    case 34: return 11;  // IP_MULTICAST_LOOP
    case 35: return 12;  // IP_ADD_MEMBERSHIP  (struct ip_mreq: same layout)
    case 36: return 13;  // IP_DROP_MEMBERSHIP
    default: return -1;  // unknown / no macOS equivalent -> ignore (never pass a Linux number to macOS IPPROTO_IP)
    }
}

// an AF_UNIX DATAGRAM send to a PATHNAME/abstract dest (sendto/sendmsg with an explicit dest addr --
// e.g. syslog's `logger` writing to /dev/log) must resolve the dest through the SAME overlay/abstract-ns
// mapping bind/connect use, or macOS looks for the socket inode at the literal host path (outside the jail)
// / the wrong abstract-ns dir and the datagram is silently dropped. Mirrors the connect (case 203) and bind
// (case 200) AF_UNIX handling. Returns 1 + fills `host` when the dest should be overlay/abstract-routed
// (caller sends via unix_dgram_sendmsg_at); 0 otherwise (AF_INET, unnamed, or a non-jail pathname whose raw
// sockaddr already round-trips -> caller sends unchanged, keeping the bare-metal AF_UNIX dgram path intact).
static int unix_dgram_dest(const uint8_t *sa, socklen_t l, char *host, size_t hn) {
    if (abs_is(sa, l)) { // abstract namespace (sun_path[0]==0): DD_NETNS-keyed fs socket (same as bind/connect)
        abs_path(sa, l, host, hn);
        return 1;
    }
    if (g_rootfs && unix_path_is(sa, l)) { // pathname socket in the overlay: resolve to the topmost layer
        char gp[200], hb[1024];
        unix_path_copy(sa, l, gp, sizeof gp);
        const char *hp = atpath(-100, gp, hb, sizeof hb, 0); // guest path -> host path (upper then lowers)
        snprintf(host, hn, "%s", hp);
        return 1;
    }
    return 0;
}

// Linux-faithful errno pre-screen for bind(200)/connect(203). macOS hands dd's translated (or raw)
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
    // connect() on an already-connected stream socket -> EISCONN (kernel checks the socket state before
    // the protocol connect). AF_UNSPEC is the "dissolve association" idiom and is never EISCONN.
    if (is_connect && sotype == SOCK_STREAM && lfam != 0) {
        struct sockaddr_storage pn;
        socklen_t pnl = sizeof pn;
        int connected = (getpeername(fd, (struct sockaddr *)&pn, &pnl) == 0) ||
                        (fd >= 0 && fd < DD_NFD && g_sock_conn[fd]); // sticky: survives a peer FIN (see decl)
        if (connected) return -EISCONN;
    }
    // The socket's own family: prefer the value recorded at socket()/accept() (robust even after a prior
    // failed connect on this fd); fall back to a getsockname() probe for an untracked (e.g. inherited) fd.
    int sfam = (fd >= 0 && fd < DD_NFD) ? g_sock_fam[fd] : 0;
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
            socklen_t need = (sfam == LX_AF_INET) ? 16 : 24;
            if (lfam != sfam) return -EAFNOSUPPORT;
            if (alen < need) return -EINVAL;
        }
    }
    return 0;
}

static int svc_net(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                   uint64_t a5) {
    // AF_NETLINK/NETLINK_ROUTE: a guest netlink socket is a socketpair we RTNETLINK-respond on
    // (see netns.c). bind/getsockname/send/recv are handled here; everything else (setsockopt/getsockopt/
    // close) falls through to the generic paths, which work on the underlying AF_UNIX socket.
    if (nl_is((int)a0)) {
        switch (nr) {
        case 200: // bind(sockaddr_nl): no-op success (our socketpair is already connected)
            G_RET(c) = 0;
            return svc_done(c);
        case 204: // getsockname -> sockaddr_nl { family, pid=getpid() }
            nl_getsockname((uint8_t *)a1, (socklen_t *)a2);
            G_RET(c) = 0;
            return svc_done(c);
        case 206: { // sendto/send: parse the RTNETLINK request, queue the dump
            int64_t s = nl_send((int)a0, (const uint8_t *)a1, (size_t)a2);
            G_RET(c) = (uint64_t)s;
            return svc_done(c);
        }
        case 211: { // sendmsg: gather the iov into a scratch buffer, then queue the dump
            uint8_t *g = (uint8_t *)a1;
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
            struct iovec iov = {(void *)a1, (size_t)a2};
            int64_t r = nl_recv((int)a0, &iov, 1, (int)a3, NULL);
            if (r >= 0 && a4) {
                nl_fill_src((uint8_t *)a4, a5 ? *(socklen_t *)a5 : 0);
                if (a5) *(socklen_t *)a5 = 12;
            }
            G_RET(c) = (uint64_t)r; // nl_recv already returns -errno on failure
            return svc_done(c);
        }
        case 212: { // recvmsg: read into the guest iov (Linux MSG_PEEK/TRUNC); report a kernel source addr
            uint8_t *g = (uint8_t *)a1;
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
        // socket (translate Linux domain -> macOS: AF_INET6 10->30, others unchanged). Gate the new fd
        // against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap (the host table is far larger).
        int r = nofile_gate(socket(af_l2m((int)a0), ty & 0xf, (int)a2));
        if (r >= 0) {
            // SIGPIPE suppression: Linux delivers EPIPE (not a fatal signal) to a guest that has
            // SIG_IGN'd SIGPIPE or passes MSG_NOSIGNAL; macOS has no per-call MSG_NOSIGNAL, so make the
            // suppression sticky on the fd. With SO_NOSIGPIPE set at creation, ANY write to a broken
            // socket -- write(2), writev(2), send(2) without MSG_NOSIGNAL -- returns -1/EPIPE instead
            // of raising SIGPIPE. Benign on healthy sockets; only sockets get it, so real pipes/FIFOs
            // keep Linux's default SIGPIPE-on-write semantics.
            {
                int on = 1;
                setsockopt(r, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
            }
            if (ty & 0x80000) fcntl(r, F_SETFD, FD_CLOEXEC);
            if (ty & 0x800) fcntl(r, F_SETFL, O_NONBLOCK);
            if (r < DD_NFD) {
                // AF_INET6 STREAM also gets loopback isolation (::/::1 -> private lo). a0 is the guest's
                // Linux domain value, so test the Linux AF_INET6 (10), not the macOS one (30).
                g_sock_stream[r] = ((ty & 0xf) == SOCK_STREAM && ((int)a0 == AF_INET || (int)a0 == LX_AF_INET6_FAM));
                g_sock_dgram[r] = ((ty & 0xf) == SOCK_DGRAM && (int)a0 == AF_INET);
                g_sock_seqpacket[r] = 0;
                g_sock_conn[r] = 0;           // fresh socket: not yet connected (see g_sock_conn decl)
                g_sock_fam[r] = (uint16_t)a0; // guest address family, for connect/bind EAFNOSUPPORT check
                g_lo_port[r] = 0;
                g_lo_v6[r] = 0;
                g_br_port[r] = 0;
                g_br_ip[r] = 0;
                g_tcp_lport[r] = 0;
                g_tcp_listen[r] = 0;
                g_dns_sock[r] = 0;
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 199: {
        int sv[2];
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
            int on = 1;
            setsockopt(sv[0], SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
            setsockopt(sv[1], SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
            if ((int)a1 & 0x80000) { // SOCK_CLOEXEC
                fcntl(sv[0], F_SETFD, FD_CLOEXEC);
                fcntl(sv[1], F_SETFD, FD_CLOEXEC);
            }
            if ((int)a1 & 0x800) { // SOCK_NONBLOCK
                int f0 = fcntl(sv[0], F_GETFL);
                int f1 = fcntl(sv[1], F_GETFL);
                if (f0 >= 0) fcntl(sv[0], F_SETFL, f0 | O_NONBLOCK);
                if (f1 >= 0) fcntl(sv[1], F_SETFL, f1 | O_NONBLOCK);
            }
            ((int *)a3)[0] = sv[0];
            ((int *)a3)[1] = sv[1];
            if ((int)a0 == AF_UNIX) {
                if (sv[0] >= 0 && sv[0] < DD_NFD) {
                    g_sock_fam[sv[0]] = AF_UNIX;
                    g_sock_stream[sv[0]] = (lty == SOCK_STREAM);
                    g_sock_dgram[sv[0]] = (lty == SOCK_DGRAM || lty == SOCK_SEQPACKET);
                    g_sock_pair_peer[sv[0]] = sv[1] + 1;
                    g_sock_peer_pid[sv[0]] = sock_alloc_synth_peer();
                    g_sock_passcred[sv[0]] = 0;
                }
                if (sv[1] >= 0 && sv[1] < DD_NFD) {
                    g_sock_fam[sv[1]] = AF_UNIX;
                    g_sock_stream[sv[1]] = (lty == SOCK_STREAM);
                    g_sock_dgram[sv[1]] = (lty == SOCK_DGRAM || lty == SOCK_SEQPACKET);
                    g_sock_pair_peer[sv[1]] = sv[0] + 1;
                    g_sock_peer_pid[sv[1]] = sock_alloc_synth_peer();
                    g_sock_passcred[sv[1]] = 0;
                }
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
                // hits a 2KB wall. Chromium's Mojo node channel IS a SEQPACKET socketpair, and its bring-up
                // messages (the invitation carrying serialized handles, GPU/Viz init IPCs) routinely exceed
                // 2KB: on the DGRAM backing those sends fail and the message is lost, so the browser<->child
                // handshake wedges forever (the UI thread blocks on a readiness event the child can never
                // signal -> no window is ever created). Raise SO_SNDBUF/SO_RCVBUF on BOTH ends (a per-socket
                // buffer overrides the maxdgram default; verified: with 1MB buffers, 256KB datagrams send OK)
                // so an emulated SEQPACKET carries the same large messages a real Linux SEQPACKET does. Both
                // ends are bidirectional (each sends and receives), and the setting survives the fork/exec
                // that hands one end to the child (it is a property of the kernel socket object).
                int bufsz = 1 << 20; // 1 MiB: comfortably above any realistic Mojo channel message
                setsockopt(sv[0], SOL_SOCKET, SO_SNDBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[0], SOL_SOCKET, SO_RCVBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[1], SOL_SOCKET, SO_SNDBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[1], SOL_SOCKET, SO_RCVBUF, &bufsz, sizeof bufsz);
                // Stamp each end with a DISTINCT synthetic peer node identity. macOS reports the socketpair
                // CREATOR's pid via LOCAL_PEERPID on BOTH ends (never updated on fork), so once the browser
                // forks a renderer the browser's cred/peercred query on its end degenerates to self; without a
                // distinct id every forked child collided on guest pid 1 and Mojo's ports node-merge hung.
                if (sv[0] >= 0 && sv[0] < DD_NFD) g_sock_seqpacket[sv[0]] = 1;
                if (sv[1] >= 0 && sv[1] < DD_NFD) g_sock_seqpacket[sv[1]] = 1;
            }
        }
        if (r == 0 && (int)a0 == AF_UNIX)
            w7_trace("socketpair AF_UNIX ty=%s fds=[%d,%d] peer_pid=[%d,%d]",
                     lty == SOCK_STREAM ? "STREAM" : lty == SOCK_SEQPACKET ? "SEQPACKET" : "DGRAM", sv[0], sv[1],
                     (sv[0] >= 0 && sv[0] < DD_NFD) ? g_sock_peer_pid[sv[0]] : 0,
                     (sv[1] >= 0 && sv[1] < DD_NFD) ? g_sock_peer_pid[sv[1]] : 0);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // bind -- port-map: bind the published host port
    case 200: {
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
        if ((int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_stream[(int)a0] && a2 >= 8) {
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
        if (lo_on() && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_stream[(int)a0] &&
            (lo_any_is(sa, (socklen_t)a2) || is_lo6)) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            if (p == 0) p = lo_alloc_ephemeral(); // bind(:0) -> a real, round-trippable port
            char up[200];
            lo_path(p, up, sizeof up);
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            unlink(up);
            struct sockaddr_un un;
            memset(&un, 0, sizeof un);
            un.sun_family = AF_UNIX;
            snprintf(un.sun_path, sizeof un.sun_path, "%s", up);
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) {
                g_lo_port[(int)a0] = p ? p : 1;
                g_lo_v6[(int)a0] = (uint8_t)is_lo6; // remember family for getsockname/accept
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // NET bridge: bind(0.0.0.0 / own-ip / in-subnet :port) -> LISTEN on /tmp/.ddbr-<netid>/<ownip>:<port>.
        // A dual-stack listener that binds `::` (busybox nc's default, and many servers') is the IPv6 analogue
        // of 0.0.0.0 and takes the same path (br6_any_is), so it's reachable by peer containers over the switch
        // instead of landing on the isolated per-container loopback (which broke cross-container reach-by-name).
        if (br_on() && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_stream[(int)a0] &&
            (br_bind_is(sa, (socklen_t)a2) || br6_any_is(sa, (socklen_t)a2))) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            if (p == 0) p = br_alloc_ephemeral(); // bind(:0) -> a real, round-trippable port
            char up[200];
            br_path(g_myip, p, up, sizeof up); // we always listen on OUR endpoint IP
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            unlink(up);
            struct sockaddr_un un;
            memset(&un, 0, sizeof un);
            un.sun_family = AF_UNIX;
            snprintf(un.sun_path, sizeof un.sun_path, "%s", up);
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) {
                g_br_port[(int)a0] = p ? p : 1;
                g_br_ip[(int)a0] = g_myip;
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // abstract AF_UNIX (sun_path[0]==0): macOS has no abstract ns -> bind a real fs socket keyed by
        // DD_NETNS. Must run BEFORE any general AF_UNIX passthrough below.
        if (abs_is(sa, (socklen_t)a2)) {
            char up[200];
            abs_path(sa, (socklen_t)a2, up, sizeof up);
            unlink(up); // replace stale (cf. lo_/br_ above)
            struct sockaddr_un un;
            memset(&un, 0, sizeof un);
            un.sun_family = AF_UNIX;
            snprintf(un.sun_path, sizeof un.sun_path, "%s", up);
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
        // AF_UNIX pathname bind: materialize the socket inode in the overlay (writable upper), jail-confined,
        // so the guest can stat/chmod/connect it through the SAME resolver. A raw host bind created the inode
        // OUTSIDE the jail (at the literal guest path on the host fs), so the guest's overlay-routed stat()
        // ENOENT'd it (mongod "Failed to chmod socket file", mariadb "Bind on unix socket"). Jail only.
        if (g_rootfs && unix_path_is(sa, (socklen_t)a2)) {
            char gp[200], host[1024];
            unix_path_copy(sa, (socklen_t)a2, gp, sizeof gp);
            overlay_copyup(gp, host, sizeof host); // guest path -> upper host path (+ materialize parent dirs)
            unlink(host);                          // clear a stale inode (else EADDRINUSE)
            // bind at the FULL upper path (via unix_sock_at, which fchdir-shortens paths past sun_path[104])
            // so the socket inode lands exactly where the guest's stat/chmod/connect resolves -- a plain bind
            // would truncate the long upper path and strand the inode where nothing can find it.
            int r = unix_sock_at((int)a0, host, 0);
            if (r == 0) unix_bind_note((int)a0, gp); // record the guest path for /proc/net/unix
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
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
        if (g_nportmap && sa && a2 >= 8 && *(uint16_t *)(sa + 0) == AF_INET) {
            uint16_t cp = ntohs(*(uint16_t *)(sa + 2)), hp = pm_host(cp);
            // remember for getsockname
            if ((int)a0 >= 0 && (int)a0 < DD_NFD) g_fd_cport[(int)a0] = cp;
            if (hp != cp) {
                uint8_t buf[128];
                socklen_t L = a2 < 128 ? (socklen_t)a2 : 128;
                memcpy(buf, sa, L);
                // publish on :H instead of :C (port @2)
                *(uint16_t *)(buf + 2) = htons(hp);
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
        // Published-port (`-p H:C`) host bridge: if this is a switch-backed listen on a published
        // container port, spin up a real host AF_INET listener on :H that relays into the guest.
        if (lr == 0) fwd_maybe_start((int)a0);
        if (lr == 0) netns_tcp_listen_note((int)a0); // arm the /proc/net/tcp[6] LISTEN row

        G_RET(c) = lr < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 202:
    case 242: {
        int lfd = (int)a0;
        // accept / accept4
        int pl = (lfd >= 0 && lfd < DD_NFD) ? g_lo_port[lfd] : 0;
        int pl6 = (lfd >= 0 && lfd < DD_NFD) ? g_lo_v6[lfd] : 0; // listener is AF_INET6 -> report v6 peer
        int pbr = (lfd >= 0 && lfd < DD_NFD) ? g_br_port[lfd] : 0;
        uint32_t pbrip = (lfd >= 0 && lfd < DD_NFD) ? g_br_ip[lfd] : 0;
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
            // Accepted connections are sockets too: make SIGPIPE suppression sticky on the new fd so a
            // write/send to a peer that closes returns EPIPE instead of killing the guest (see case 198).
            {
                int on = 1;
                setsockopt(r, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
            }
            if (r >= 0 && r < DD_NFD) {
                g_sock_conn[r] = 1;                                          // an accepted socket is already connected
                if (lfd >= 0 && lfd < DD_NFD) g_sock_fam[r] = g_sock_fam[lfd]; // inherit listener's family
            }
            if (nr == 242) {
                if ((int)a3 & 0x800) fcntl(r, F_SETFL, fcntl(r, F_GETFL) | O_NONBLOCK);
                if ((int)a3 & 0x80000) fcntl(r, F_SETFD, FD_CLOEXEC);
            }
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
                if (r < DD_NFD) {
                    g_lo_port[r] = pl;
                    g_lo_v6[r] = (uint8_t)pl6;
                    g_sock_stream[r] = 1;
                }
                if (pl6)
                    fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, pl);
                else
                    fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, pl);
            } else if (pbr) {
                if (r < DD_NFD) {
                    g_br_port[r] = pbr;
                    g_br_ip[r] = pbrip;
                    g_sock_stream[r] = 1;
                }
                // peer reported as our virtual listen addr (cf. lo_* simplification)
                fill_inet_br((uint8_t *)a1, (socklen_t *)a2, pbrip, pbr);
            }
            // peer = 127.0.0.1:lport
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // connect
    case 203: {
        // Linux errno pre-screen (EBADF/ENOTSOCK/EFAULT/EINVAL/EISCONN/EAFNOSUPPORT) before any addr deref.
        {
            int pc = net_precheck((int)a0, a1, (socklen_t)a2, 1);
            if (pc) {
                G_RET(c) = (uint64_t)(int64_t)pc;
                return svc_done(c);
            }
        }
        // --network none: no external egress (DD_NET_ISOLATE). Loopback is redirected by the lo_* path
        // below; any non-127/8 AF_INET destination is refused, matching docker's null network.
        static int net_isolate = -1;
        if (net_isolate < 0) net_isolate = getenv("DD_NET_ISOLATE") != NULL;
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
            int stream = ((int)a0 >= 0 && (int)a0 < DD_NFD) ? g_sock_stream[(int)a0] : 0;
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
        if (lo_on() && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_stream[(int)a0] &&
            // private loopback: v4 127/8 or v6 ::1 (port@2 identical) -> the per-container loopback switch
            (lo_is(sa, (socklen_t)a2) || c_lo6)) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            char up[200];
            lo_path(p, up, sizeof up);
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            struct sockaddr_un un;
            memset(&un, 0, sizeof un);
            un.sun_family = AF_UNIX;
            snprintf(un.sun_path, sizeof un.sun_path, "%s", up);
            int r = connect((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0 || errno == EINPROGRESS) {
                g_lo_port[(int)a0] = p ? p : 1;
                g_lo_v6[(int)a0] = (uint8_t)c_lo6;
            } else if ((errno == ENOENT || errno == ECONNREFUSED) && br_on() && g_myip) {
                // Same-container localhost dial of a server that bound INADDR_ANY on the bridge (br_path,
                // keyed by OUR own IP -- not lo_path): retry there so 127.0.0.1 still reaches a 0.0.0.0
                // listener in bridge mode. The first connect() already POISONED this AF_UNIX socket (a
                // failed BSD connect leaves the fd unusable -- a second connect() on it hangs/EINVALs, which
                // is why a 0.0.0.0 server was unreachable via 127.0.0.1 with a user network attached),
                // so swap in a FRESH AF_UNIX fd before the retry -- exactly as the br_connect loop does.
                char bp[200];
                br_path(g_myip, p, bp, sizeof bp);
                struct sockaddr_un bu;
                memset(&bu, 0, sizeof bu);
                bu.sun_family = AF_UNIX;
                snprintf(bu.sun_path, sizeof bu.sun_path, "%s", bp);
                if (lo_swap((int)a0) < 0) {
                    r = -1;
                } else {
                    r = connect((int)a0, (struct sockaddr *)&bu, sizeof bu);
                    if (r == 0 || errno == EINPROGRESS) {
                        g_br_port[(int)a0] = p ? p : 1;
                        g_br_ip[(int)a0] = g_myip;
                    }
                }
            }
            // a redirected TCP dial to a port with no listener fails ENOENT (the per-port unix
            // inode doesn't exist); Linux returns ECONNREFUSED for a closed TCP port. Map it (host
            // errno, m2l_errno -> Linux 111); other errnos incl. EINPROGRESS pass through.
            G_RET(c) = r < 0 ? (uint64_t)(-(errno == ENOENT ? ECONNREFUSED : errno)) : 0;
            if (r == 0 && (int)a0 >= 0 && (int)a0 < DD_NFD) g_sock_conn[(int)a0] = 1; // sticky-connected
            break;
        }
        // NET bridge: connect(peer-ip:port in our subnet) -> dial /tmp/.ddbr-<netid>/<peerip>:<port>
        if (br_on() && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_stream[(int)a0] && br_connect_is(sa, (socklen_t)a2)) {
            uint32_t dip = *(uint32_t *)(sa + 4);
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            char up[200];
            br_path(dip, p, up, sizeof up);
            struct sockaddr_un un;
            memset(&un, 0, sizeof un);
            un.sun_family = AF_UNIX;
            snprintf(un.sun_path, sizeof un.sun_path, "%s", up);
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
                g_br_port[(int)a0] = p ? p : 1;
                g_br_ip[(int)a0] = dip; // peer ip for getpeername
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            if (r == 0 && (int)a0 >= 0 && (int)a0 < DD_NFD) g_sock_conn[(int)a0] = 1; // sticky-connected
            break;
        }
        // abstract AF_UNIX (sun_path[0]==0): dial the same DD_NETNS-keyed fs socket bind used. Must run
        // BEFORE the general AF_UNIX passthrough below.
        if (abs_is(sa, (socklen_t)a2)) {
            char up[200];
            abs_path(sa, (socklen_t)a2, up, sizeof up);
            struct sockaddr_un un;
            memset(&un, 0, sizeof un);
            un.sun_family = AF_UNIX;
            snprintf(un.sun_path, sizeof un.sun_path, "%s", up);
            int r = connect((int)a0, (struct sockaddr *)&un, sizeof un);
            G_RET(c) = (r < 0 && errno != EINPROGRESS) ? (uint64_t)(-errno) : 0;
            if (r == 0 && (int)a0 >= 0 && (int)a0 < DD_NFD) g_sock_conn[(int)a0] = 1; // sticky-connected
            break;
        }
        // AF_UNIX pathname connect: resolve through the overlay (same resolver as stat/open) so we dial the
        // socket the guest actually bound -- materialized in the upper -- not a host path outside the jail.
        if (g_rootfs && unix_path_is(sa, (socklen_t)a2)) {
            char gp[200], host[1024];
            unix_path_copy(sa, (socklen_t)a2, gp, sizeof gp);
            const char *hp = atpath(-100, gp, host, sizeof host, 0); // guest path -> topmost layer's host path
            // dial via unix_sock_at (matches the bind side): fchdir-shortens paths past sun_path[104] so a
            // long upper socket path is reached exactly, not truncated to some other (nonexistent) inode.
            int r;
            do {
                r = unix_sock_at((int)a0, hp, 1);
            } while (r < 0 && SVC_EINTR_RESTART(c));
            G_RET(c) = (r < 0 && errno != EINPROGRESS) ? (uint64_t)(-errno) : 0;
            if (r == 0 && (int)a0 >= 0 && (int)a0 < DD_NFD) g_sock_conn[(int)a0] = 1; // sticky-connected
            break;
        }
        // Real host connect: translate Linux AF_INET/INET6 sockaddr -> macOS; others pass through.
        {
            struct sockaddr_storage ss;
            socklen_t hl = sa_l2m(sa, (socklen_t)a2, &ss);
            // Per-workspace VPN (docs/VPN.md): when DD_EGRESS_SOCKS is armed, funnel a genuine external TCP
            // connect through the SOCKS5 proxy instead of dialing directly. INERT when unset — egress_should_
            // redirect() short-circuits to 0, so control falls straight to the direct connect() below with no
            // behavior change. Streams only; UDP/raw and non-INET dests use the direct path.
            if (hl != (socklen_t)-1 && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_stream[(int)a0] &&
                egress_should_redirect((struct sockaddr *)&ss)) {
                int er = egress_connect((int)a0, (struct sockaddr *)&ss, hl);
                G_RET(c) = er < 0 ? (uint64_t)(-errno) : 0;
                if (er == 0 && (int)a0 >= 0 && (int)a0 < DD_NFD) g_sock_conn[(int)a0] = 1; // sticky-connected
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
            if (cr == 0 && (int)a0 >= 0 && (int)a0 < DD_NFD) g_sock_conn[(int)a0] = 1; // sticky-connected
        }
        break;
    }
    case 204: {
        // getsockname
        int fd = (int)a0;
        if (fd >= 0 && fd < DD_NFD && g_dns_sock[fd]) { // DNS socket: report an AF_INET local addr (0.0.0.0:0)
            if (a1) {
                uint8_t *g = (uint8_t *)a1;
                memset(g, 0, 8);
                *(uint16_t *)g = AF_INET;
                if (a2) *(socklen_t *)a2 = 16;
            }
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < DD_NFD && g_lo_port[fd]) {
            if (g_lo_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < DD_NFD && g_br_port[fd]) {
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
                if (g_nportmap && fd >= 0 && fd < DD_NFD && g_fd_cport[fd] && gcap >= 4)
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
        if (fd >= 0 && fd < DD_NFD && g_dns_sock[fd]) { // DNS socket: peer is the nameserver 127.0.0.11:53
            dns_fill_ns((uint8_t *)a1, (socklen_t *)a2);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < DD_NFD && g_lo_port[fd]) {
            if (g_lo_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < DD_NFD && g_br_port[fd]) {
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
        // Container DNS: a query sent to 127.0.0.11:53 (connected send, or first unconnected sendto) is
        // parsed + answered via the host resolver; nothing hits the wire. a4/a5 are the optional dest addr.
        {
            int64_t dret;
            if (dns_try_send((int)a0, (const uint8_t *)a1, (size_t)a2, (const uint8_t *)a4, (socklen_t)a5, &dret)) {
                G_RET(c) = (uint64_t)dret;
                break;
            }
        }
        // MSG_NOSIGNAL(0x4000) has no per-call equivalent on macOS; emulate it with the SO_NOSIGPIPE
        // socket option so the send returns EPIPE instead of raising a fatal SIGPIPE.
        if ((int)a3 & 0x4000) {
            int on = 1;
            setsockopt((int)a0, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
        }
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
        if (r >= 0) seq_mark_wrote((int)a0); // genuine writer: may inject peer-EOF on its close (see netns.c)
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 207: {
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
        if (r >= 0 && want && (int)a0 >= 0 && (int)a0 < DD_NFD && g_dns_sock[(int)a0]) {
            // DNS socket: report the source as the nameserver (127.0.0.11:53) so the guest resolver's
            // "answer came from the server we queried" anti-spoof check passes (the real src is AF_UNIX).
            dns_fill_ns((uint8_t *)a4, (socklen_t *)a5);
        } else if (r >= 0 && want) {
            socklen_t gcap = a5 ? *(socklen_t *)a5 : 0;
            int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a4, gcap);
            if (ll < 0) {
                socklen_t n = hsl < gcap ? hsl : gcap;
                if (gcap) memcpy((void *)a4, &hss, n);
                if (a5) *(socklen_t *)a5 = hsl;
            } else if (a5)
                *(socklen_t *)a5 = (socklen_t)ll;
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // setsockopt(fd, level, optname, val, len)
    case 208: {
        int lvl = (int)a1, opt = (int)a2;
        // SO_PASSCRED (Linux SOL_SOCKET/16): macOS has no equivalent. Record it per-fd so recvmsg(212)
        // synthesizes the SCM_CREDENTIALS ancillary record the Linux kernel would auto-attach (Chromium's
        // Mojo bootstrap requires it). Never fail the guest.
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
            if ((int)a0 >= 0 && (int)a0 < DD_NFD) g_sock_passcred[(int)a0] = on ? 1 : 0;
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
        int lvl = (int)a1, opt = (int)a2;
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
                *(int *)a3 = ((int)a0 >= 0 && (int)a0 < DD_NFD) ? g_sock_passcred[(int)a0] : 0;
                *(socklen_t *)a4 = 4;
            }
            G_RET(c) = 0;
            break;
        }
        if (lvl == 1 && opt == 17) {
            if (a3 && a4 && *(socklen_t *)a4 >= 12) {
                pid_t ppid = 0;
                socklen_t pl = sizeof ppid;
                if (getsockopt((int)a0, SOL_LOCAL, LOCAL_PEERPID, &ppid, &pl) < 0 || ppid <= 0 || ppid == getpid()) {
                    // macOS reports the socketpair CREATOR's pid on both ends -> a fork parent (the browser)
                    // reads its OWN pid here for every child. Report the end's peer pid we resolved (the REAL
                    // guest pid of the process holding the OTHER end, stamped across fork/close -- see
                    // g_sock_peer_pid / seq_reassign_peer); else this guest's own pid.
                    int sp = ((int)a0 >= 0 && (int)a0 < DD_NFD) ? g_sock_peer_pid[(int)a0] : 0;
                    ppid = sp ? sp : container_pid();
                } else if (g_init_hostpid && ppid == g_init_hostpid) {
                    ppid = 1; // peer is the container init -> guest pid 1
                }
                uint32_t *u = (uint32_t *)a3;
                u[0] = (uint32_t)ppid;   // pid (resolved above)
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
        uint8_t *g = (uint8_t *)a1;
        // Container DNS: a sendmsg carrying a query to 127.0.0.11:53 (or on an already-swapped DNS socket).
        if (nr == 211 && dns_enabled()) {
            int dfd = (int)a0;
            uint8_t *nm = (uint8_t *)*(uint64_t *)(g + 0);
            socklen_t nml = *(uint32_t *)(g + 8);
            if ((dfd >= 0 && dfd < DD_NFD && g_dns_sock[dfd]) || dns_dest_is(nm, nml)) {
                struct iovec *iv = (struct iovec *)*(uint64_t *)(g + 16);
                int ivn = (int)*(uint64_t *)(g + 24);
                uint8_t tmp[2048];
                size_t tl = dns_gather(iv, ivn, tmp, sizeof tmp);
                int64_t dret;
                if (dns_try_send(dfd, tmp, tl, nm, nml, &dret)) {
                    G_RET(c) = (uint64_t)dret;
                    break;
                }
            }
        }
        struct msghdr mh;
        // Linux: iovlen/controllen are 8-byte; macOS 4
        memset(&mh, 0, sizeof mh);
        mh.msg_name = (void *)*(uint64_t *)(g + 0);
        mh.msg_namelen = *(uint32_t *)(g + 8);
        mh.msg_iov = (void *)*(uint64_t *)(g + 16);
        mh.msg_iovlen = (int)*(uint64_t *)(g + 24);
        mh.msg_flags = *(uint32_t *)(g + 48);
        // msg_name sockaddr: Linux<->macOS translation through a host scratch (AF_INET/INET6 only).
        struct sockaddr_storage nss;
        uint8_t *gname = (uint8_t *)mh.msg_name;
        socklen_t gnamelen = mh.msg_namelen;
        char ud_host[1200];
        int ud_route = 0;                     // AF_UNIX pathname/abstract dgram dest -> overlay/abstract route on send
        if (nr == 211 && gname && gnamelen) { // sendmsg: guest -> host
            if (gnamelen >= 2 && *(const uint16_t *)gname == AF_UNIX &&
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
        } else if (nr == 212 && gname && gnamelen) { // recvmsg: receive into host scratch
            mh.msg_name = &nss;
            mh.msg_namelen = sizeof nss;
        }
        // Ancillary data: the guest control buf is Linux-cmsg layout; macOS reads a different cmsghdr,
        // so route it through a host-layout scratch buffer (NULL-control left untouched, so edge/msgflags
        // with no control buffer stays on the old path).
        uint8_t *gc = (void *)*(uint64_t *)(g + 32);
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
        if (nr == 211) {    // sendmsg: translate guest -> host before the call
            // Ancillary data may carry SCM_RIGHTS fds to another process; flush all RAM-backed scratch so a
            // passed fd (and any other) is a coherent host file on the receiving side.
            if (gc && gcl) memf_materialize_all();
            int cerr = 0;
            ssize_t hn = (gc && gcl) ? cmsg_l2m(gc, gcl, hctl, hcap, &cerr) : 0;
            if (hn < 0) {
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
        if (nr == 211 && ((int)a2 & 0x4000)) {
            int on = 1;
            setsockopt((int)a0, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
        }
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
        if (nr == 211 && r >= 0) seq_mark_wrote((int)a0); // genuine writer: may inject peer-EOF on its close
        if (nr == 211 && w7_on() && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_fam[(int)a0] == AF_UNIX)
            w7_trace("sendmsg fd=%d ret=%ld ctl_l=%u%s%s peer_pid=%d", (int)a0, (long)r, (unsigned)(gc ? gcl : 0),
                     (gc && gcl) ? " SCM" : "", (r < 0) ? " ERR" : "",
                     ((int)a0 < DD_NFD) ? g_sock_peer_pid[(int)a0] : 0);
        if (r > 0 && peekaddr) r = 0; // guest supplied no data room; only the source address was wanted
        // SEQPACKET-as-DGRAM EOF: coerce a peer-closed recvmsg's ECONNRESET to 0 (EOF). (See case 199.)
        if (nr == 212 && r < 0 && errno == ECONNRESET && seq_is((int)a0)) r = 0;
        if (nr == 212 && r >= 0) {
            // recvmsg writes back name len + (host->guest) control + translated flags
            if (gname && gnamelen && (int)a0 >= 0 && (int)a0 < DD_NFD && g_dns_sock[(int)a0]) {
                // DNS socket: report the nameserver (127.0.0.11:53) as the source (see case 207).
                dns_fill_ns(gname, NULL);
                *(uint32_t *)(g + 8) = 16;
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
            // the container pid). Chromium's Mojo bootstrap aborts ("missing credentials") without it.
            int passcred_active = gc && gcl && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_passcred[(int)a0];
            int cred_trunc = 0;
            size_t ln = 0;
            if (passcred_active) {
                int ppid = 0;
                socklen_t pl = sizeof ppid;
                if (getsockopt((int)a0, SOL_LOCAL, LOCAL_PEERPID, &ppid, &pl) < 0 || ppid <= 0 || ppid == getpid()) {
                    // macOS reports the socketpair CREATOR's pid on both ends (never updated on fork), so the
                    // fork parent reads its OWN pid here for every child -> container_pid() collapsed all of
                    // them to guest 1, colliding Mojo's ports node identity. Prefer the end's distinct synthetic
                    // peer node id stamped at socketpair(); fall back to this guest's own pid only if unstamped.
                    int sp = ((int)a0 >= 0 && (int)a0 < DD_NFD) ? g_sock_peer_pid[(int)a0] : 0;
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
            if (!cmsg_trunc && gc && gcl) host_mflags &= ~0x20; // host-side sideband expansion compressed cleanly
            int mfl = msgflags_m2l(host_mflags);
            if (cred_trunc || (cmsg_trunc && !passcred_active)) mfl |= 0x8; // MSG_CTRUNC
            if (((int)a2 & 0x40000000) && gc && ln) cmsg_lx_set_cloexec_fds(gc, ln); // MSG_CMSG_CLOEXEC
            *(uint64_t *)(g + 40) = ln;
            *(uint32_t *)(g + 48) = (uint32_t)mfl;
            if (w7_on() && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_fam[(int)a0] == AF_UNIX)
                w7_trace("recvmsg fd=%d ret=%ld ctl_l=%zu%s%s%s peer_pid=%d", (int)a0, (long)r, ln,
                         passcred_active ? " CRED" : "", (mfl & 0x8) ? " CTRUNC" : "",
                         (mh.msg_controllen > 0) ? " HOSTSCM" : "", g_sock_peer_pid[(int)a0]);
        }
        if (nr == 212 && r <= 0 && w7_on() && (int)a0 >= 0 && (int)a0 < DD_NFD && g_sock_fam[(int)a0] == AF_UNIX)
            w7_trace("recvmsg fd=%d ret=%ld%s (no-data)", (int)a0, (long)r,
                     (r < 0) ? (errno == EAGAIN ? " EAGAIN" : " ERR") : " EOF");
        if (hctl != hstack) free(hctl);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 269:
    // sendmmsg/recvmmsg(fd, mmsghdr[], vlen, flags, [timeout])
    case 243: {
        uint8_t *vec = (uint8_t *)a1;
        unsigned vlen = (unsigned)a2;
        // mmsghdr = msghdr(56) + msg_len(4) + pad
        // Container DNS: glibc's default parallel A+AAAA lookup sends BOTH queries to the nameserver in one
        // sendmmsg. Answer each submessage via the host resolver; the responses are drained by recvfrom (207).
        if (nr == 269 && dns_enabled() && vlen) {
            int dfd = (int)a0;
            uint8_t *g0 = vec;
            uint8_t *nm0 = (uint8_t *)*(uint64_t *)(g0 + 0);
            socklen_t nml0 = *(uint32_t *)(g0 + 8);
            int is_dns = (dfd >= 0 && dfd < DD_NFD && g_dns_sock[dfd]);
            if (!is_dns && dns_dest_is(nm0, nml0) &&
                dns_swap(dfd, (dfd >= 0 && dfd < DD_NFD) ? g_sock_stream[dfd] : 0) == 0)
                is_dns = 1;
            if (is_dns) {
                int stream = (dfd >= 0 && dfd < DD_NFD) ? g_sock_stream[dfd] : 0;
                unsigned n;
                for (n = 0; n < vlen; n++) {
                    uint8_t *g = vec + (size_t)n * 64;
                    struct iovec *iv = (struct iovec *)*(uint64_t *)(g + 16);
                    int ivn = (int)*(uint64_t *)(g + 24);
                    uint8_t tmp[2048];
                    size_t tl = dns_gather(iv, ivn, tmp, sizeof tmp);
                    dns_send(dfd, tmp, tl, stream);
                    *(uint32_t *)(g + 56) = (uint32_t)tl; // msg_len: whole query accepted
                }
                G_RET(c) = (uint64_t)vlen;
                break;
            }
        }
        int done = 0, err = 0;
        // MSG_NOSIGNAL(0x4000) -> SO_NOSIGPIPE once before the fan-out (macOS has no per-call flag).
        if (nr == 269 && ((int)a3 & 0x4000)) {
            int on = 1;
            setsockopt((int)a0, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
        }
        for (unsigned i = 0; i < vlen; i++) {
            uint8_t *g = vec + (size_t)i * 64;
            struct msghdr mh;
            memset(&mh, 0, sizeof mh);
            mh.msg_name = (void *)*(uint64_t *)(g + 0);
            mh.msg_namelen = *(uint32_t *)(g + 8);
            mh.msg_iov = (void *)*(uint64_t *)(g + 16);
            mh.msg_iovlen = (int)*(uint64_t *)(g + 24);
            mh.msg_flags = *(uint32_t *)(g + 48);
            // msg_name sockaddr: Linux<->macOS translation through a host scratch (AF_INET/INET6 only).
            struct sockaddr_storage nss;
            uint8_t *gname = (uint8_t *)mh.msg_name;
            socklen_t gnamelen = mh.msg_namelen;
            char ud_host[1200];
            int ud_route = 0; // AF_UNIX pathname/abstract dgram dest -> overlay/abstract route on send
            if (nr == 269 && gname && gnamelen) { // sendmmsg: guest -> host
                if (gnamelen >= 2 && *(const uint16_t *)gname == AF_UNIX &&
                    unix_dgram_dest(gname, gnamelen, ud_host, sizeof ud_host)) {
                    ud_route = 1; // sent via unix_dgram_sendmsg_at below (it owns msg_name)
                } else {
                    socklen_t hl = sa_l2m(gname, gnamelen, &nss);
                    if (hl != (socklen_t)-1) {
                        mh.msg_name = &nss;
                        mh.msg_namelen = hl;
                    }
                }
            } else if (nr == 243 && gname && gnamelen) { // recvmmsg: receive into host scratch
                mh.msg_name = &nss;
                mh.msg_namelen = sizeof nss;
            }
            // Ancillary data: route the per-submessage control buf through a host-layout scratch buffer.
            uint8_t *gc = (void *)*(uint64_t *)(g + 32);
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
                ssize_t hn = (gc && gcl) ? cmsg_l2m(gc, gcl, hctl, hcap, &cerr) : 0;
                if (hn < 0) {
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
            if (r < 0) {
                err = errno;
                if (hctl != hstack) free(hctl);
                break;
            }
            // msg_len
            *(uint32_t *)(g + 56) = (uint32_t)r;
            if (nr == 243) {
                if (gname && gnamelen && (int)a0 >= 0 && (int)a0 < DD_NFD && g_dns_sock[(int)a0]) {
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
                if (!cmsg_trunc && gc && gcl) host_mflags &= ~0x20; // host-side sideband expansion compressed cleanly
                int mfl = msgflags_m2l(host_mflags);
                if (cmsg_trunc) mfl |= 0x8; // MSG_CTRUNC
                *(uint32_t *)(g + 48) = (uint32_t)mfl;
            }
            if (hctl != hstack) free(hctl);
            done++;
        }
        G_RET(c) = (done == 0 && err) ? (uint64_t)(-(int64_t)err) : (uint64_t)done;
        break;
    }
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
