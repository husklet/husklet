static void abs_init(void) {
    if (g_abs_init) return;
    g_abs_init = 1;
    const char *ns = hl_option_get("HL_NETNS"); // same key used by ipc_ns_key (service.c)
    snprintf(g_absdir, sizeof g_absdir, "/tmp/.hl-abstract-%.40s", (ns && ns[0]) ? ns : "default");
    hl_compat_mkdir(g_absdir, 0700); // EEXIST fine; peers share it (0700, guest is path-jailed)
}

// Is this guest sockaddr an abstract AF_UNIX addr? family u16==AF_UNIX, sun_path[0]==NUL, name>=1B.
static int abs_is(const uint8_t *sa, socklen_t l) {
    return sa && l > 3 && *(const uint16_t *)sa == AF_UNIX && sa[2] == 0; // sun_path[0] @ offset 2
}

// Is this guest sockaddr an AF_UNIX *pathname* addr (a filesystem socket, not abstract/autobind)?
static int unix_path_is(const uint8_t *sa, socklen_t l) {
    return sa && l > 3 && *(const uint16_t *)sa == AF_UNIX && sa[2] != 0; // sun_path[0] @ offset 2
}

// Copy a guest sockaddr_un's sun_path (NUL- or addrlen-bounded) into `out` as a C string.
static void unix_path_copy(const uint8_t *sa, socklen_t l, char *out, size_t n) {
    size_t pl = (size_t)l > 2 ? (size_t)l - 2 : 0; // bytes after the 2-byte family
    size_t i = 0;
    for (; i + 1 < n && i < pl && sa[2 + i]; i++)
        out[i] = (char)sa[2 + i];
    out[i] = 0;
}

// Map abstract name (bytes sa+3 .. for namelen=l-3) to a filesystem path. Hex when it fits macOS
// sun_path[104], else FNV-1a hash (long D-Bus/X11/systemd names overflow); the name may hold NULs,
// '/', non-printables, so hex/hash makes a safe single path component (no traversal).
static void abs_path(const uint8_t *sa, socklen_t l, char *out, size_t n) {
    abs_init();
    const uint8_t *nm = sa + 3;
    size_t nl = (size_t)l - 3;
    size_t dl = strlen(g_absdir);
    if (dl + 1 + nl * 2 + 1 <= n && dl + 1 + nl * 2 < 104) { // full hex (unambiguous, fits sun_path)
        char hx[210];
        static const char *H = "0123456789abcdef";
        for (size_t i = 0; i < nl; i++) {
            hx[2 * i] = H[nm[i] >> 4];
            hx[2 * i + 1] = H[nm[i] & 15];
        }
        hx[2 * nl] = 0;
        snprintf(out, n, "%s/%s", g_absdir, hx);
    } else { // hash fallback (FNV-1a) keeps the path bounded
        uint64_t h = 1469598103934665603ull;
        for (size_t i = 0; i < nl; i++) {
            h ^= nm[i];
            h *= 1099511628211ull;
        }
        snprintf(out, n, "%s/h%016llx", g_absdir, (unsigned long long)h);
    }
}

// ===== AF_NETLINK / NETLINK_ROUTE: a minimal RTNETLINK responder ==========================
// macOS has no AF_NETLINK, so socket(AF_NETLINK,...) returned EAFNOSUPPORT and every interface-
// enumeration path (getifaddrs via glibc/musl, go-sockaddr, `ip`, ifconfig, minio, consul)
// failed with "Address family not supported". hl models exactly two interfaces (lo + eth0; see
// netif_* in state.c). A guest netlink socket is backed by an AF_UNIX SOCK_DGRAM socketpair: the
// guest holds one end; when it sends an RTM_GET* dump request we parse the nlmsghdr and WRITE the
// synthesized dump into OUR peer end, which queues on the guest end so the guest's ordinary
// recv/recvmsg/poll reads it back -- no read-side blocking or extra threads. We only synthesize the
// three dumps real enumeration uses (RTM_GETLINK / RTM_GETADDR / RTM_GETROUTE); any other request
// just gets an NLMSG_DONE so nothing hangs.
#define LX_AF_NETLINK 16
#define NL_RTM_NEWLINK 16
#define NL_RTM_GETLINK 18
#define NL_RTM_NEWADDR 20
#define NL_RTM_GETADDR 22
#define NL_RTM_NEWROUTE 24
#define NL_RTM_GETROUTE 26
#define NL_NLMSG_DONE 3
#define NL_NLM_F_MULTI 2
// guest netlink fd -> our peer socketpair fd, stored +1 (0 = not a netlink socket). Mirrors the
// g_eventfd_peer +1 convention so close()/fd_reset_emul can tear the peer down.
static int g_nl_peer[HL_NFD];

static int nl_is(int fd) {
    return fd >= 0 && fd < HL_NFD && g_nl_peer[fd];
}

// close a netlink fd's peer (called from fd_reset_emul on the guest close). Idempotent.
static void nl_close(int fd) {
    if (fd >= 0 && fd < HL_NFD && g_nl_peer[fd]) {
        close(g_nl_peer[fd] - 1);
        g_nl_peer[fd] = 0;
    }
}

// socket(AF_NETLINK,...): back it with an AF_UNIX SOCK_DGRAM socketpair. Any requested type
// (SOCK_RAW/SOCK_DGRAM) collapses to SOCK_DGRAM (AF_UNIX has no SOCK_RAW). Returns the guest fd or
// -errno. Honors SOCK_CLOEXEC(0x80000)/SOCK_NONBLOCK(0x800) on the guest end.
static int nl_open(int type, int proto) {
    (void)proto; // only NETLINK_ROUTE is modelled; others still get a working (empty-dump) socket
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) < 0) return -errno;
    int g = sv[0], peer = sv[1];
    if (g < 0 || g >= HL_NFD) { // untracked fd range -> can't route sends; refuse cleanly
        close(g);
        close(peer);
        return -EMFILE;
    }
    if (type & 0x80000) fcntl(g, F_SETFD, FD_CLOEXEC);
    if (type & 0x800) fcntl(g, F_SETFL, O_NONBLOCK);
    fcntl(peer, F_SETFD, FD_CLOEXEC); // keep our end out of a guest execve
    g_nl_peer[g] = peer + 1;
    return g;
}

// getsockname on a netlink fd: report a sockaddr_nl { u16 family; u16 pad; u32 pid; u32 groups } with
// pid = getpid() (the port id our dump replies also stamp in nlmsg_pid, so go's pid check matches).
static void nl_getsockname(uint8_t *sa, socklen_t *sl) {
    if (sa && sl && *sl >= 12) {
        memset(sa, 0, 12);
        *(uint16_t *)(sa + 0) = LX_AF_NETLINK;
        *(uint32_t *)(sa + 4) = (uint32_t)getpid();
        *sl = 12;
    } else if (sl)
        *sl = 12;
}

// Fill a Linux sockaddr_nl "from the kernel" (pid 0) as a recv source address; 12 bytes.
static void nl_fill_src(uint8_t *sa, socklen_t cap) {
    if (!sa || cap < 12) return;
    memset(sa, 0, 12);
    *(uint16_t *)(sa + 0) = LX_AF_NETLINK; // nl_pid=0 => from kernel (glibc/go accept only pid 0 source)
}

// rtattr append: { u16 rta_len; u16 rta_type; data } padded to RTA_ALIGN(4).
static void nl_put_attr(uint8_t *b, size_t *o, uint16_t type, const void *data, uint16_t dlen) {
    *(uint16_t *)(b + *o) = (uint16_t)(4 + dlen);
    *(uint16_t *)(b + *o + 2) = type;
    if (data && dlen) memcpy(b + *o + 4, data, dlen);
    *o += (size_t)((4 + dlen + 3) & ~3);
}

// begin an nlmsg (16-byte header); returns its offset for nl_end() to backpatch nlmsg_len.
static size_t nl_begin(uint8_t *b, size_t *o, uint16_t type, uint32_t seq) {
    size_t h = *o;
    memset(b + h, 0, 16);
    *(uint16_t *)(b + h + 4) = type;
    *(uint16_t *)(b + h + 6) = NL_NLM_F_MULTI;
    *(uint32_t *)(b + h + 8) = seq;
    *(uint32_t *)(b + h + 12) = (uint32_t)getpid();
    *o = h + 16;
    return h;
}

static void nl_end(uint8_t *b, size_t *o, size_t h) {
    *(uint32_t *)(b + h) = (uint32_t)(*o - h); // nlmsg_len (unpadded); attrs already 4-aligned
    *o = (*o + 3) & ~(size_t)3;
}

// one RTM_NEWLINK message
static void nl_link(uint8_t *b, size_t *o, uint32_t seq, const char *name, int idx, uint16_t iftype, uint32_t flags,
                    const uint8_t *mac, uint32_t mtu, const uint8_t *bcast) {
    size_t h = nl_begin(b, o, NL_RTM_NEWLINK, seq);
    uint8_t *ii = b + *o; // ifinfomsg (16B): family,pad,type(2),index(4),flags(4),change(4)
    memset(ii, 0, 16);
    *(uint16_t *)(ii + 2) = iftype;
    *(int32_t *)(ii + 4) = idx;
    *(uint32_t *)(ii + 8) = flags;
    *(uint32_t *)(ii + 12) = 0xffffffffu;
    *o += 16;
    nl_put_attr(b, o, 3, name, (uint16_t)(strlen(name) + 1)); // IFLA_IFNAME
    nl_put_attr(b, o, 1, mac, 6);                             // IFLA_ADDRESS
    nl_put_attr(b, o, 2, bcast, 6);                           // IFLA_BROADCAST
    uint32_t v = mtu;
    nl_put_attr(b, o, 4, &v, 4);      // IFLA_MTU
    v = (iftype == 772) ? 0u : 1000u; // IFLA_TXQLEN
    nl_put_attr(b, o, 13, &v, 4);
    uint8_t op = 6, lm = 0;        // IF_OPER_UP / IFLA_LINKMODE
    nl_put_attr(b, o, 16, &op, 1); // IFLA_OPERSTATE
    nl_put_attr(b, o, 17, &lm, 1); // IFLA_LINKMODE
    nl_end(b, o, h);
}

// one RTM_NEWADDR message (v4: alen=4; v6: alen=16). bcast!=NULL adds IFA_BROADCAST (v4 eth0 only).
static void nl_addr(uint8_t *b, size_t *o, uint32_t seq, uint8_t family, uint8_t prefix, uint8_t scope, int idx,
                    const char *label, const void *addr, int alen, const void *bcast) {
    size_t h = nl_begin(b, o, NL_RTM_NEWADDR, seq);
    uint8_t *ia = b + *o; // ifaddrmsg (8B): family,prefixlen,flags,scope,index(4)
    memset(ia, 0, 8);
    ia[0] = family;
    ia[1] = prefix;
    ia[3] = scope;
    *(uint32_t *)(ia + 4) = (uint32_t)idx;
    *o += 8;
    nl_put_attr(b, o, 1, addr, (uint16_t)alen);                            // IFA_ADDRESS
    nl_put_attr(b, o, 2, addr, (uint16_t)alen);                            // IFA_LOCAL
    if (bcast) nl_put_attr(b, o, 4, bcast, (uint16_t)alen);                // IFA_BROADCAST
    if (label) nl_put_attr(b, o, 3, label, (uint16_t)(strlen(label) + 1)); // IFA_LABEL
    nl_end(b, o, h);
}

// one RTM_NEWROUTE message
static void nl_route(uint8_t *b, size_t *o, uint32_t seq, uint8_t dst_len, uint8_t scope, uint8_t type,
                     const uint32_t *dst, const uint32_t *gw, const uint32_t *prefsrc, int oif) {
    size_t h = nl_begin(b, o, NL_RTM_NEWROUTE, seq);
    uint8_t *rm = b + *o; // rtmsg (12B): family,dst_len,src_len,tos,table,protocol,scope,type,flags(4)
    memset(rm, 0, 12);
    rm[0] = 2; // AF_INET
    rm[1] = dst_len;
    rm[4] = 254; // RT_TABLE_MAIN
    rm[5] = 3;   // RTPROT_BOOT
    rm[6] = scope;
    rm[7] = type; // RTN_UNICAST=1
    *o += 12;
    if (dst) nl_put_attr(b, o, 1, dst, 4); // RTA_DST
    if (oif) {
        uint32_t v = (uint32_t)oif;
        nl_put_attr(b, o, 4, &v, 4);
    } // RTA_OIF
    if (gw) nl_put_attr(b, o, 5, gw, 4);           // RTA_GATEWAY
    if (prefsrc) nl_put_attr(b, o, 7, prefsrc, 4); // RTA_PREFSRC
    nl_end(b, o, h);
}

static void nl_done(uint8_t *b, size_t *o, uint32_t seq) {
    uint8_t *h = b + *o;
    memset(h, 0, 16);
    *(uint32_t *)(h + 0) = 16;
    *(uint16_t *)(h + 4) = NL_NLMSG_DONE;
    *(uint16_t *)(h + 6) = NL_NLM_F_MULTI;
    *(uint32_t *)(h + 8) = seq;
    *(uint32_t *)(h + 12) = (uint32_t)getpid();
    *o += 16;
}

// One RTM_NEWLINK for a modelled interface slot (0=lo, 1=eth0). Shared by the dump and the
// single-interface (non-dump) query paths so both stay byte-identical.
static void nl_link_slot(uint8_t *b, size_t *o, uint32_t seq, int slot) {
    uint8_t zero6[6] = {0}, ff6[6] = {0xff, 0xff, 0xff, 0xff, 0xff, 0xff}, mac[6];
    if (slot == 0)
        nl_link(b, o, seq, "lo", 1, 772 /*ARPHRD_LOOPBACK*/, 0x10049u /*UP|LOOP|RUN|LOWER_UP*/, zero6, 65536, zero6);
    else {
        netif_eth0_mac(mac);
        nl_link(b, o, seq, "eth0", 2, 1 /*ARPHRD_ETHER*/, 0x11043u /*UP|BCAST|RUN|MCAST|LOWER_UP*/, mac, 1500, ff6);
    }
}

// Resolve a non-dump RTM_GETLINK target (ifi_index and/or IFLA_IFNAME) to a modelled slot, or -1 if
// no such interface exists (eth0 is absent under --network none). Mirrors the dump's lo(+eth0) set.
static int nl_link_slot_for(int32_t idx, const char *name) {
    if (idx == 1 || (name && strcmp(name, "lo") == 0)) return 0;
    if (!net_isolate() && (idx == 2 || (name && strcmp(name, "eth0") == 0))) return 1;
    return -1;
}

// Build + queue (one datagram to `peer`) the dump for request `type` with echoed `seq`.
static void nl_emit_dump(int peer, uint16_t type, uint32_t seq) {
    uint8_t out[4096];
    size_t o = 0;
    if (type == NL_RTM_GETLINK) {
        nl_link_slot(out, &o, seq, 0); // lo
        // --network none: loopback-only, so eth0 is absent from the link dump (`ip link` sees just lo).
        if (!net_isolate()) nl_link_slot(out, &o, seq, 1); // eth0
    } else if (type == NL_RTM_GETADDR) {
        uint32_t lo4 = 0x0100007fu; // 127.0.0.1
        uint8_t lo6[16] = {0};
        lo6[15] = 1; // ::1
        uint32_t e4 = netif_eth0_ip(), eb = netif_eth0_bcast();
        nl_addr(out, &o, seq, 2 /*AF_INET*/, 8, 254 /*RT_SCOPE_HOST*/, 1, "lo", &lo4, 4, NULL);
        nl_addr(out, &o, seq, 10 /*AF_INET6*/, 128, 254, 1, NULL, lo6, 16, NULL);
        if (!net_isolate()) // --network none: no eth0 address
            nl_addr(out, &o, seq, 2, (uint8_t)netif_eth0_prefix(), 0 /*RT_SCOPE_UNIVERSE*/, 2, "eth0", &e4, 4, &eb);
    } else if (type == NL_RTM_GETROUTE) {
        if (!net_isolate()) { // --network none: no eth0 routes (loopback carries no L3 routing table)
            uint32_t net = netif_eth0_net(), gw = netif_eth0_gw(), src = netif_eth0_ip();
            nl_route(out, &o, seq, 0, 0 /*UNIVERSE*/, 1, NULL, &gw, NULL, 2); // default via gw dev eth0
            nl_route(out, &o, seq, (uint8_t)netif_eth0_prefix(), 253 /*LINK*/, 1, &net, NULL, &src, 2); // subnet
        }
    }
    // (unknown request types fall through to just NLMSG_DONE -> an empty, harmless dump)
    nl_done(out, &o, seq);
    ssize_t w = send(peer, out, o, 0); // one datagram; guest reads it via its own recv/recvmsg
    (void)w;
}

// Copy `n` bytes from `src` into the guest scatter buffer (iov array). Returns bytes actually copied.
static size_t nl_scatter(const uint8_t *src, size_t n, struct iovec *iov, int iovn) {
    size_t off = 0;
    for (int i = 0; i < iovn && off < n; i++) {
        size_t take = iov[i].iov_len;
        if (take > n - off) take = n - off;
        if (take && iov[i].iov_base) memcpy(iov[i].iov_base, src + off, take);
        off += take;
    }
    return off;
}

// Receive one queued netlink datagram into the guest iov, emulating the Linux MSG_PEEK / MSG_TRUNC
// semantics that macOS lacks. Two macOS gaps break busybox `ip`/libnetlink here:
//   (1) recv(...,MSG_TRUNC) on Linux returns the datagram's TRUE length (not the copied length) so a
//       caller can size a buffer; macOS ignores MSG_TRUNC on input and returns only what it copied.
//   (2) macOS short-circuits ANY zero-length receive to 0 without touching the queue, so busybox's
//       "peek the size first" idiom -- recvmsg(fd, {iov_len=0}, MSG_PEEK|MSG_TRUNC) -- reports 0, so it
//       reads nothing, never advances past the request, and its recv-loop spins/blocks forever.
// We first PEEK the whole datagram into a host scratch (buffer >= our <=4KB dumps, so the host recv
// returns the real length even on macOS), then honor the guest's flags precisely: MSG_PEEK leaves the
// datagram queued; a real read consumes it (excess discarded, as for any DGRAM); MSG_TRUNC makes the
// return value the true length. *msgflags (if set) gets Linux MSG_TRUNC when the copy was truncated.
// gflags are Linux MSG_* flags. Returns bytes (per Linux) or -errno.
static int64_t nl_recv(int fd, struct iovec *iov, int iovn, int gflags, int *msgflags) {
    uint8_t hb[8192]; // dumps are <=4096 (see nl_emit_dump's out[]); big enough to peek the full length
    ssize_t truelen;
    int hpeek = MSG_PEEK | ((gflags & 0x40 /* Linux MSG_DONTWAIT */) ? MSG_DONTWAIT : 0);
    int hread = (gflags & 0x40 /* Linux MSG_DONTWAIT */) ? MSG_DONTWAIT : 0;
    do {
        truelen = recv(fd, hb, sizeof hb, hpeek);
    } while (truelen < 0 && errno == EINTR);
    if (truelen < 0) {
        if (msgflags) *msgflags = 0;
        return -errno;
    }
    size_t cap = 0;
    for (int i = 0; i < iovn; i++)
        cap += iov[i].iov_len;
    size_t copylen = (size_t)truelen < cap ? (size_t)truelen : cap;
    if (!(gflags & 0x2 /*MSG_PEEK*/)) { // real read: consume the whole datagram (rest discarded, DGRAM)
        ssize_t consumed;
        do {
            consumed = recv(fd, hb, sizeof hb, hread);
        } while (consumed < 0 && errno == EINTR);
        if (consumed < 0) {
            if (msgflags) *msgflags = 0;
            return -errno;
        }
    }
    size_t got = nl_scatter(hb, copylen, iov, iovn);
    if (msgflags) *msgflags = ((size_t)truelen > got) ? 0x20 /*Linux MSG_TRUNC*/ : 0;
    return (gflags & 0x20 /*MSG_TRUNC*/) ? (int64_t)truelen : (int64_t)got;
}

// Queue one NLMSG_ERROR (type 2) reply to `peer` for the request whose 16-byte header is at `req_hdr`.
// Linux nlmsgerr = { s32 error; nlmsghdr orig_request; }: error<0 is an errno, error==0 is a plain ACK
// (sent only when the request set NLM_F_ACK). We echo the request's seq/header exactly as the kernel does
// so libnetlink/glibc match the reply to the outstanding request.
static void nl_error(int peer, const uint8_t *req_hdr, int err) {
    uint8_t out[36];
    memset(out, 0, sizeof out);
    *(uint32_t *)(out + 0) = 36;                               // nlmsg_len = hdr(16) + err(4) + echoed hdr(16)
    *(uint16_t *)(out + 4) = 2;                                // NLMSG_ERROR
    *(uint16_t *)(out + 6) = 0;                                // nlmsg_flags
    *(uint32_t *)(out + 8) = *(const uint32_t *)(req_hdr + 8); // echo the request's seq
    *(uint32_t *)(out + 12) = (uint32_t)getpid();              // nlmsg_pid == our port id (matches the dumps)
    *(int32_t *)(out + 16) = (int32_t)err;                     // nlmsgerr.error (negative errno; 0 == ACK)
    memcpy(out + 20, req_hdr, 16);                             // nlmsgerr.msg = echoed request header
    ssize_t w = send(peer, out, sizeof out, 0);
    (void)w;
}

// NLM_F_DUMP = NLM_F_ROOT(0x100)|NLM_F_MATCH(0x200); the kernel routes a request to the dump handler
// when either bit is set, otherwise to the single-object (.doit) handler.
#define NL_NLM_F_DUMP 0x300

// A non-dump RTM_GETLINK targets one interface by ifi_index or IFLA_IFNAME. Emit its single
// RTM_NEWLINK (no NLM_F_MULTI, no trailing NLMSG_DONE), or NLMSG_ERROR -ENODEV if it does not exist --
// matching the kernel's rtnl_getlink, where the old code wrongly replied with the whole link dump.
static void nl_getlink_one(int peer, const uint8_t *req, uint32_t nlen, uint32_t seq) {
    int32_t idx = 0;
    const char *name = NULL;
    char nbuf[64];
    if (nlen >= 16 + 16) idx = *(const int32_t *)(req + 16 + 4); // ifinfomsg.ifi_index
    // Optional IFLA_IFNAME attribute after the ifinfomsg.
    for (uint32_t ao = 16 + 16; ao + 4 <= nlen;) {
        uint16_t rlen = *(const uint16_t *)(req + ao), rtype = *(const uint16_t *)(req + ao + 2);
        if (rlen < 4 || ao + rlen > nlen) break;
        if (rtype == 3 /*IFLA_IFNAME*/) {
            uint16_t dl = rlen - 4;
            if (dl >= sizeof nbuf) dl = sizeof nbuf - 1;
            memcpy(nbuf, req + ao + 4, dl);
            nbuf[dl] = 0;
            name = nbuf;
        }
        ao += (rlen + 3u) & ~3u;
    }
    int slot = nl_link_slot_for(idx, name);
    if (slot < 0) {
        nl_error(peer, req, -ENODEV);
        return;
    }
    uint8_t out[1024];
    size_t o = 0;
    nl_link_slot(out, &o, seq, slot);
    *(uint16_t *)(out + 6) = 0; // clear NLM_F_MULTI: a single .doit reply carries no NLMSG_DONE
    ssize_t w = send(peer, out, o, 0);
    (void)w;
}

// A send on a netlink fd: walk the request's nlmsghdr(s) and queue each one's reply. Returns bytes
// consumed (== len; requests are tiny) so the guest's send returns success.
static int64_t nl_send(int fd, const uint8_t *buf, size_t len) {
    int peer = g_nl_peer[fd] - 1;
    size_t off = 0;
    while (off + 16 <= len) {
        uint32_t nlen = *(const uint32_t *)(buf + off);
        uint16_t ntype = *(const uint16_t *)(buf + off + 4);
        uint16_t nflags = *(const uint16_t *)(buf + off + 6);
        uint32_t nseq = *(const uint32_t *)(buf + off + 8);
        // RTM message groups run base+0=NEW, +1=DEL, +2=GET, +3=SET. A GET (type%4==2) is a read the dump
        // responder answers; a NEW/DEL/SET (type%4!=2) is a MODIFICATION hl has no writable netlink stack to
        // apply. Reply to modifications with a real NLMSG_ERROR (-EPERM) instead of the old phantom empty
        // NLMSG_DONE, so `ip addr/route add`/SETLINK fail loudly rather than silently succeeding unchanged.
        if (ntype >= 16 && (ntype % 4) != 2)
            nl_error(peer, buf + off, -EPERM);
        else if (ntype == NL_RTM_GETLINK && !(nflags & NL_NLM_F_DUMP))
            // `ip link show dev X`: a single-interface query, not a dump -> one reply or -ENODEV.
            nl_getlink_one(peer, buf + off, nlen < len - off ? nlen : (uint32_t)(len - off), nseq);
        else
            nl_emit_dump(peer, ntype, nseq);
        if (nlen < 16 || off + ((nlen + 3) & ~3u) <= off) break; // malformed -> stop
        off += (nlen + 3) & ~3u;
    }
    return (int64_t)len;
}

// ===== Socket ioctls (SIOCGIF*): the ioctl half of the shared lo+eth0 model ================
// busybox `ifconfig` (and any getifaddrs-free tool) enumerates via socket ioctls, not netlink: an
// AF_INET SOCK_DGRAM socket + SIOCGIFCONF/SIOCGIFADDR/SIOCGIFFLAGS/... . macOS has these too, but its
// kernel knows nothing of our synthesized container interfaces, so it returned ENOTTY -> "ioctl 0x8912
// failed: Not a tty". We answer them from the SAME lo+eth0 model the netlink responder + procfs use
// (netif_* in state.c), writing the Linux struct layouts directly (guest expects Linux sockaddr_in:
// family u16 @0). Dispatched from the ioctl handler (fs.c) for socket fds; guest result pointers are
// range-checked (-EFAULT) since we memcpy into them directly rather than via a bounds-checking syscall.
#define LX_IFNAMSIZ 16
#define LX_IFREQ_SZ 40 // sizeof(struct ifreq) on 64-bit Linux: name[16] + 24-byte union

// The two modelled interfaces, filled per slot (0=lo, 1=eth0). All IPv4 fields are network-order held
// as a host u32 (a | b<<8 | c<<16 | d<<24), matching netif_eth0_ip()'s encoding.
struct nif {
    const char *name;
    int index, mtu;
    uint32_t ip, mask, bcast;
    uint16_t flags, arphrd;
    uint8_t mac[6];
};

// prefixlen -> IPv4 netmask (network-order-as-host-u32). /16 -> 255.255.0.0 (0x0000ffff).
static uint32_t netif_mask_be(int prefix) {
    uint8_t m[4] = {0, 0, 0, 0};
    for (int i = 0; i < 4; i++) {
        int bits = prefix - i * 8;
        m[i] = bits >= 8 ? 0xff : bits > 0 ? (uint8_t)(0xff << (8 - bits)) : 0;
    }
    return (uint32_t)(m[0] | (m[1] << 8) | (m[2] << 16) | (m[3] << 24));
}

static void nif_get(int slot, struct nif *o) {
    memset(o, 0, sizeof *o);
    if (slot == 0) { // lo: 127.0.0.1/8, UP|LOOPBACK|RUNNING
        o->name = "lo";
        o->index = 1;
        o->ip = 0x0100007fu;   // 127.0.0.1
        o->mask = 0x000000ffu; // 255.0.0.0 (/8)
        o->mtu = 65536;
        o->flags = 0x49; // IFF_UP|IFF_LOOPBACK|IFF_RUNNING
        o->arphrd = 772; // ARPHRD_LOOPBACK
    } else {             // eth0: the bridge IP, UP|BROADCAST|RUNNING|MULTICAST
        o->name = "eth0";
        o->index = 2;
        o->ip = netif_eth0_ip();
        o->mask = netif_mask_be(netif_eth0_prefix());
        o->bcast = netif_eth0_bcast();
        o->mtu = 1500;
        o->flags = 0x1043; // IFF_UP|IFF_BROADCAST|IFF_RUNNING|IFF_MULTICAST
        o->arphrd = 1;     // ARPHRD_ETHER
        netif_eth0_mac(o->mac);
    }
}

static int nif_by_name(const char *name, struct nif *o) {
    for (int i = 0; i < 2; i++) {
        nif_get(i, o);
        if (strcmp(o->name, name) == 0) return 1;
    }
    return 0;
}

static int nif_by_index(int idx, struct nif *o) {
    for (int i = 0; i < 2; i++) {
        nif_get(i, o);
        if (o->index == idx) return 1;
    }
    return 0;
}

// Write a Linux sockaddr_in { u16 family=AF_INET(2); u16 port=0; u32 addr; u8 pad[8] } into the 24-byte
// ifreq union at `u` (whole union cleared so stale caller bytes don't leak).
static void ifr_set_in(uint8_t *u, uint32_t addr_be) {
    memset(u, 0, 24);
    *(uint16_t *)(u + 0) = 2; // AF_INET (Linux value)
    *(uint32_t *)(u + 4) = addr_be;
}

// Handle a socket ioctl against the lo+eth0 model. Returns 1 if `rq` is one we own (result in *out,
// 0 or -errno), 0 to let the caller fall through (non-socket-ioctl request).
static int net_ioctl(int fd, unsigned long rq, uint8_t *arg, int64_t *out) {
    if (rq < 0x8900 || rq > 0x89ff) return 0; // not a socket-device (SIOC*) ioctl -> caller's normal path
    // Must be a socket (Linux returns ENOTTY for these on a non-socket fd).
    {
        int ty;
        socklen_t tl = sizeof ty;
        if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &ty, &tl) < 0) {
            *out = -ENOTTY;
            return 1;
        }
    }
    if (rq == 0x8942) { // SIOCETHTOOL: busybox `ip link` probes it per interface. A real kernel answers
        // it (driver/link info); we don't model ethtool, but must not FAIL -- busybox prints
        // "ioctl 0x8942 failed" on ANY error (incl. EOPNOTSUPP). Report success, leaving the guest's
        // ethtool struct as it pre-zeroed it (plain `ip link`/`ip addr` display nothing from it), so the
        // output matches real docker's clean listing.
        *out = 0;
        return 1;
    }
    switch (rq) {
    case 0x8912: // SIOCGIFCONF
    case 0x8910: // SIOCGIFNAME
    case 0x8913: // SIOCGIFFLAGS
    case 0x8915: // SIOCGIFADDR
    case 0x8919: // SIOCGIFBRDADDR
    case 0x891b: // SIOCGIFNETMASK
    case 0x8921: // SIOCGIFMTU
    case 0x8927: // SIOCGIFHWADDR
    case 0x8933: // SIOCGIFINDEX
        break;
    default:
        // A socket ioctl we don't model (e.g. SIOCETHTOOL 0x8942). Report EOPNOTSUPP like a kernel that
        // lacks the op -- NOT ENOTTY: busybox `ip` prints "ioctl 0x.. failed" on any error except
        // EOPNOTSUPP, which it silently tolerates (matching real docker's clean `ip link` output). We
        // return the macOS ENOTSUP(45) value: svc_done's host->Linux errno xlate maps it to Linux
        // EOPNOTSUPP(95) (whereas macOS EOPNOTSUPP(102) would wrongly map to EINVAL).
        *out = -ENOTSUP;
        return 1;
    }
    if (rq == 0x8912) { // SIOCGIFCONF: fill an ifreq array (one per interface with an AF_INET addr)
        if (!host_range_mapped((uintptr_t)arg, 16)) {
            *out = -EFAULT;
            return 1;
        }
        int32_t ifc_len = *(int32_t *)(arg + 0);
        uint8_t *buf = (uint8_t *)*(uint64_t *)(arg + 8);
        int total = net_isolate() ? 1 : 2; // lo (+ eth0 unless --network none)
        if (!buf) {
            *(int32_t *)(arg + 0) = total * LX_IFREQ_SZ;
            *out = 0;
            return 1;
        } // size probe
        int maxn = ifc_len / LX_IFREQ_SZ;
        int n = maxn < total ? maxn : total;
        if (n > 0 && !host_range_mapped((uintptr_t)buf, (size_t)n * LX_IFREQ_SZ)) {
            *out = -EFAULT;
            return 1;
        }
        for (int i = 0; i < n; i++) {
            struct nif nif;
            nif_get(i, &nif);
            uint8_t *e = buf + (size_t)i * LX_IFREQ_SZ;
            memset(e, 0, LX_IFREQ_SZ);
            snprintf((char *)e, LX_IFNAMSIZ, "%s", nif.name);
            ifr_set_in(e + LX_IFNAMSIZ, nif.ip);
        }
        *(int32_t *)(arg + 0) = n * LX_IFREQ_SZ;
        *out = 0;
        return 1;
    }
    // The remaining requests all operate on a single struct ifreq.
    if (!host_range_mapped((uintptr_t)arg, LX_IFREQ_SZ)) {
        *out = -EFAULT;
        return 1;
    }
    struct nif nif;
    uint8_t *u = arg + LX_IFNAMSIZ; // the ifreq union
    if (rq == 0x8910) {             // SIOCGIFNAME: index -> name
        if (!nif_by_index(*(int32_t *)u, &nif)) {
            *out = -ENODEV;
            return 1;
        }
        memset(arg, 0, LX_IFNAMSIZ);
        snprintf((char *)arg, LX_IFNAMSIZ, "%s", nif.name);
        *out = 0;
        return 1;
    }
    char name[LX_IFNAMSIZ + 1];
    memcpy(name, arg, LX_IFNAMSIZ);
    name[LX_IFNAMSIZ] = 0;
    if (!nif_by_name(name, &nif)) {
        *out = -ENODEV;
        return 1;
    }
    switch (rq) {
    case 0x8915: ifr_set_in(u, nif.ip); break;    // SIOCGIFADDR
    case 0x8919: ifr_set_in(u, nif.bcast); break; // SIOCGIFBRDADDR
    case 0x891b: ifr_set_in(u, nif.mask); break;  // SIOCGIFNETMASK
    case 0x8913:
        memset(u, 0, 24);
        *(int16_t *)u = (int16_t)nif.flags;
        break; // SIOCGIFFLAGS
    case 0x8921:
        memset(u, 0, 24);
        *(int32_t *)u = nif.mtu;
        break; // SIOCGIFMTU
    case 0x8933:
        memset(u, 0, 24);
        *(int32_t *)u = nif.index;
        break;   // SIOCGIFINDEX
    case 0x8927: // SIOCGIFHWADDR: sockaddr { u16 sa_family=ARPHRD_*; u8 mac[6] ... }
        memset(u, 0, 24);
        *(uint16_t *)u = nif.arphrd;
        memcpy(u + 2, nif.mac, 6);
        break;
    default:
        *out = -ENOTSUP;
        return 1;
    }
    *out = 0;
    return 1;
}

// ============================ Container DNS (embedded nameserver -> host resolver) ============================
// The container's /etc/resolv.conf (provisioned by the daemon) points at 127.0.0.11 -- hl's embedded
// nameserver, the same address Docker uses. glibc/musl in the guest then send DNS as UDP (default) or TCP
// (fallback) to 127.0.0.11:53. We intercept those sends here, parse the query, resolve it via the macOS
// host resolver (getaddrinfo / getnameinfo -- which honor the host's system DNS, INCLUDING a corporate
// VPN's split-DNS, synthesize a wire-format DNS response, and make
// it readable on the guest socket. The guest fd is swapped to one end of an AF_UNIX socketpair; the
// response is written into the engine-held peer end, so poll/select/epoll + recv/read all see a real fd
// with real buffered data (no polling/timeout hacks). recvfrom/recvmsg report the source as 127.0.0.11:53
// so glibc/musl's anti-spoofing "answer came from the nameserver we asked" check passes.
//
// Coverage: A (1), AAAA (28), PTR (12, in-addr.arpa + ip6.arpa reverse), CNAME chains (flattened -- the
// resolved addresses are returned under the queried name, which is what getaddrinfo/gethostbyname consume),
// NXDOMAIN (name has no address of any family), NODATA (name exists but not this type), SERVFAIL (transient
// resolver error), multiple answers, TTL. Other qtypes (MX/TXT/SRV/SOA/NS/CAA/HTTPS/SVCB...) return NOERROR
// with no answer (NODATA), so a client falls back to A/AAAA -- see the tracked-remaining note in the report.
#define HL_DNS_NS 0x0b00007fu      // 127.0.0.11, network byte order (bytes 7f 00 00 0b == LE u32 0x0b00007f)
static uint8_t g_dns_sock[HL_NFD]; // fd -> 1 if this fd is an intercepted, socketpair-backed DNS socket
static int g_dns_peer[HL_NFD];     // fd -> engine-held socketpair end we write synthesized responses into
