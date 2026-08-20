static int saxl_on(void) {
    return 1;
}

// guest domain (Linux) -> host (macOS), for socket()/socketpair(). AF_INET(2)/AF_UNIX(1) match.
static int af_l2m(int d) {
    return (saxl_on() && d == LX_AF_INET6) ? AF_INET6 : d;
}

// guest(Linux) sockaddr -> host(macOS) into `out`; returns host socklen, or -1 if not AF_INET/INET6
// (or gated off) — caller then uses the original guest pointer/len unchanged.
static socklen_t sa_l2m(const uint8_t *g, socklen_t glen, struct sockaddr_storage *out) {
    if (!saxl_on() || !g || glen < 2) return (socklen_t)-1;
    int fam = *(const uint16_t *)g;
    if (fam == LX_AF_INET && glen >= 8) {
        struct sockaddr_in *m = (struct sockaddr_in *)out;
        memset(m, 0, sizeof *m);
#if defined(__APPLE__)
        m->sin_len = sizeof *m;
#endif
        m->sin_family = AF_INET;
        memcpy(&m->sin_port, g + 2, 2); // port (BE), same offset
        memcpy(&m->sin_addr, g + 4, 4); // addr (BE), same offset
        return (socklen_t)sizeof *m;    // 16
    }
    if (fam == LX_AF_INET6 && glen >= 24) {
        struct sockaddr_in6 *m = (struct sockaddr_in6 *)out;
        memset(m, 0, sizeof *m);
#if defined(__APPLE__)
        m->sin6_len = sizeof *m;
#endif
        m->sin6_family = AF_INET6;
        memcpy(&m->sin6_port, g + 2, 2);
        memcpy(&m->sin6_flowinfo, g + 4, 4);
        memcpy(&m->sin6_addr, g + 8, 16);
        if (glen >= 28) memcpy(&m->sin6_scope_id, g + 24, 4);
        return (socklen_t)sizeof *m; // 28
    }
    return (socklen_t)-1;
}

// host(macOS) sockaddr -> guest(Linux) layout written to `g` (capacity gcap, may be 0/NULL). Returns
// the FULL Linux length of the address (16/28) even if it exceeds gcap (Linux truncates the copy but
// reports the full length via *addrlen), or -1 if not AF_INET/INET6 (caller copies raw host bytes).
static int sa_m2l(const struct sockaddr *m, uint8_t *g, socklen_t gcap) {
    if (!saxl_on() || !m) return -1;
    if (m->sa_family == AF_INET) {
        const struct sockaddr_in *s = (const struct sockaddr_in *)m;
        uint8_t t[16];
        memset(t, 0, sizeof t);
        *(uint16_t *)t = LX_AF_INET;
        memcpy(t + 2, &s->sin_port, 2);
        memcpy(t + 4, &s->sin_addr, 4);
        if (g && gcap) memcpy(g, t, gcap < 16 ? gcap : 16);
        return 16;
    }
    if (m->sa_family == AF_INET6) {
        const struct sockaddr_in6 *s = (const struct sockaddr_in6 *)m;
        uint8_t t[28];
        memset(t, 0, sizeof t);
        *(uint16_t *)t = LX_AF_INET6;
        memcpy(t + 2, &s->sin6_port, 2);
        memcpy(t + 4, &s->sin6_flowinfo, 4);
        memcpy(t + 8, &s->sin6_addr, 16);
        memcpy(t + 24, &s->sin6_scope_id, 4);
        if (g && gcap) memcpy(g, t, gcap < 28 ? gcap : 28);
        return 28;
    }
    return -1;
}

// ---- Per-workspace VPN egress redirect ---------------------------------------------------------------
// When HL_EGRESS_SOCKS="host:port" is armed, the guest's genuine external TCP connect()s are funneled
// through that SOCKS5 proxy (the front-end of a per-workspace userspace tunnel) instead of dialing the
// destination directly from the host's default routing domain. When the env var is ABSENT the whole
// feature is inert: egress_should_redirect() returns 0 up front and net.c runs its normal, byte-for-byte
// unchanged direct connect(). Only genuine external AF_INET/AF_INET6 destinations are candidates — the
// lo/bridge/DNS/AF_UNIX classifiers in net.c have already peeled those off before the Real-host-connect
// site that calls us, and we re-guard loopback here as a safety net so a VPN never captures 127/8 or ::1.
static const char *g_egress_socks = NULL; // "host:port"; stays NULL once probed-as-unset
static int g_egress_probed = 0;

static const char *egress_socks(void) {
    if (!g_egress_probed) {
        g_egress_probed = 1;
        const char *e = hl_option_get("HL_EGRESS_SOCKS");
        g_egress_socks = (e && *e) ? e : NULL;
    }
    return g_egress_socks;
}

// Should this (already-translated macOS) destination be tunneled? 0 unless the redirect is armed AND the
// address is a genuine external IPv4/IPv6 (never loopback/unspecified/link-local).
static int egress_should_redirect(const struct sockaddr *m) {
    if (!egress_socks() || !m) return 0;
    if (m->sa_family == AF_INET) {
        uint32_t a = ntohl(((const struct sockaddr_in *)m)->sin_addr.s_addr);
        return !((a >> 24) == 127 || a == 0);
    }
    if (m->sa_family == AF_INET6) {
        const struct in6_addr *a6 = &((const struct sockaddr_in6 *)m)->sin6_addr;
        return !(IN6_IS_ADDR_LOOPBACK(a6) || IN6_IS_ADDR_UNSPECIFIED(a6) || IN6_IS_ADDR_LINKLOCAL(a6));
    }
    return 0;
}

// #261 — IPv4-only container network: hl models eth0 exactly like Docker's default bridge does — one IPv4
// address, NO global IPv6 address, and an empty IPv6 routing table (see the RTM_GETADDR/RTM_GETROUTE dumps
// in nl_emit_dump: eth0 gets only an AF_INET addr, and the only v6 addr is ::1 on lo). So a genuine external
// (global-unicast) IPv6 destination has NO ROUTE and, on a real container kernel, connect()/sendto() to it
// fails *immediately* with ENETUNREACH. hl must reproduce that instead of forwarding the dial to the
// underlying host's v6 stack: on a mac whose v6 path to the destination is black-holed (orbstack NAT is
// v4-only), that forward hangs the guest for the full ~2-minute connect timeout, and a happy-eyeballs client
// (apt, curl, glibc) that tried the AAAA first never falls back — the exact #261 stall. Failing fast is what
// lets it fall back to IPv4 in milliseconds, so `apt-get update` Just Works without Acquire::ForceIPv4.
// The AAAA record itself is still returned by the embedded resolver (dns_build_response), matching Docker's
// embedded DNS which also serves AAAA on a v4-only network — the guest learns the v6 addr, tries it, and is
// bounced instantly. Loopback (::1 -> private lo), link-local, unspecified, and the bridge/DNS classes are
// all peeled off before the direct-connect site, so only true external v6 reaches this predicate.
// When HL_EGRESS_SOCKS is armed the v6 destination is
// tunneled through the proxy instead (egress_should_redirect handles AF_INET6), so this is consulted only on
// the direct path, after that redirect has had its chance.
static int v6_no_route(const struct sockaddr *m) {
    if (!m || m->sa_family != AF_INET6) return 0;
    const struct in6_addr *a6 = &((const struct sockaddr_in6 *)m)->sin6_addr;
    return !(IN6_IS_ADDR_LOOPBACK(a6) || IN6_IS_ADDR_UNSPECIFIED(a6) || IN6_IS_ADDR_LINKLOCAL(a6));
}

// Blocking read/write of exactly `n` bytes (EINTR-retried); 0 = ok, -1 = errno set (peer close = ECONNRESET).
static int egress_io_write(int fd, const void *buf, size_t n) {
    const uint8_t *p = buf;
    size_t off = 0;
    while (off < n) {
        ssize_t w = write(fd, p + off, n - off);
        if (w > 0) {
            off += (size_t)w;
            continue;
        }
        if (w < 0 && errno == EINTR) continue;
        if (w == 0) errno = ECONNRESET;
        return -1;
    }
    return 0;
}

static int egress_io_read(int fd, void *buf, size_t n) {
    uint8_t *p = buf;
    size_t off = 0;
    while (off < n) {
        ssize_t r = read(fd, p + off, n - off);
        if (r > 0) {
            off += (size_t)r;
            continue;
        }
        if (r < 0 && errno == EINTR) continue;
        if (r == 0) errno = ECONNRESET; // proxy closed mid-handshake
        return -1;
    }
    return 0;
}

// Perform a SOCKS5 CONNECT to the macOS destination `m` over the guest socket `fd`, dialing the proxy in
// HL_EGRESS_SOCKS. Returns 0 (fd now relayed to the real dest through the tunnel) or -1/errno mirroring
// connect(). The guest fd is put in blocking mode for the short handshake and its O_NONBLOCK is restored
// after (a non-blocking guest that we return 0 to simply sees connect() complete immediately — legal).
static int egress_connect(int fd, const struct sockaddr *m, socklen_t mlen) {
    (void)mlen;
    const char *hp = egress_socks();
    if (!hp) {
        errno = EINVAL;
        return -1;
    }
    // Parse the proxy "host:port" (host is an IPv4 loopback literal, e.g. 127.30.0.1).
    const char *colon = strrchr(hp, ':');
    if (!colon || colon == hp) {
        errno = EINVAL;
        return -1;
    }
    char host[64];
    size_t hl = (size_t)(colon - hp);
    if (hl >= sizeof host) {
        errno = EINVAL;
        return -1;
    }
    memcpy(host, hp, hl);
    host[hl] = 0;
    int port = atoi(colon + 1);
    if (port <= 0 || port > 65535) {
        errno = EINVAL;
        return -1;
    }
    struct sockaddr_in px;
    memset(&px, 0, sizeof px);
#if defined(__APPLE__)
    px.sin_len = sizeof px;
#endif
    px.sin_family = AF_INET;
    px.sin_port = htons((uint16_t)port);
    if (inet_pton(AF_INET, host, &px.sin_addr) != 1) {
        errno = EINVAL;
        return -1;
    }

    int fl = fcntl(fd, F_GETFL);
    int was_nb = (fl >= 0 && (fl & O_NONBLOCK));
    if (was_nb) fcntl(fd, F_SETFL, fl & ~O_NONBLOCK);

    int rc = -1, e = 0;
    do {
        int cr;
        do {
            cr = connect(fd, (struct sockaddr *)&px, sizeof px);
        } while (cr < 0 && errno == EINTR);
        if (cr != 0) {
            e = errno;
            break;
        } // proxy down -> report as the connect error
        // SOCKS5 greeting: VER=5, NMETHODS=1, METHOD=0 (no auth).
        uint8_t greet[3] = {0x05, 0x01, 0x00};
        if (egress_io_write(fd, greet, 3) != 0) {
            e = errno;
            break;
        }
        uint8_t sel[2];
        if (egress_io_read(fd, sel, 2) != 0) {
            e = errno;
            break;
        }
        if (sel[0] != 0x05 || sel[1] != 0x00) {
            e = ECONNREFUSED;
            break;
        } // no acceptable auth method
        // CONNECT request: VER, CMD=1, RSV=0, ATYP, addr, port(BE) — copied verbatim from the macOS sockaddr.
        uint8_t req[22];
        int n = 0;
        req[n++] = 0x05;
        req[n++] = 0x01;
        req[n++] = 0x00;
        if (m->sa_family == AF_INET) {
            const struct sockaddr_in *s4 = (const struct sockaddr_in *)m;
            req[n++] = 0x01;
            memcpy(req + n, &s4->sin_addr, 4);
            n += 4;
            memcpy(req + n, &s4->sin_port, 2);
            n += 2;
        } else {
            const struct sockaddr_in6 *s6 = (const struct sockaddr_in6 *)m;
            req[n++] = 0x04;
            memcpy(req + n, &s6->sin6_addr, 16);
            n += 16;
            memcpy(req + n, &s6->sin6_port, 2);
            n += 2;
        }
        if (egress_io_write(fd, req, (size_t)n) != 0) {
            e = errno;
            break;
        }
        // Reply: VER, REP, RSV, ATYP, bound-addr, bound-port. Read the 4-byte header, then drain the bound
        // address (length per ATYP) and 2-byte port so the fd is left clean at the start of the relayed stream.
        uint8_t rep[4];
        if (egress_io_read(fd, rep, 4) != 0) {
            e = errno;
            break;
        }
        if (rep[1] != 0x00) { // SOCKS reply code -> map to a connect-style errno
            switch (rep[1]) {
            case 0x03: e = ENETUNREACH; break;  // network unreachable
            case 0x04: e = EHOSTUNREACH; break; // host unreachable
            case 0x05: e = ECONNREFUSED; break; // connection refused
            case 0x06: e = ETIMEDOUT; break;    // TTL expired
            default: e = ECONNREFUSED; break;   // general/ruleset/unsupported failure
            }
            break;
        }
        int skip = (rep[3] == 0x01) ? 4 : (rep[3] == 0x04) ? 16 : -1;
        if (rep[3] == 0x03) { // domain name: 1-byte length prefix
            uint8_t l;
            if (egress_io_read(fd, &l, 1) != 0) {
                e = errno;
                break;
            }
            skip = l;
        }
        if (skip < 0) {
            e = ECONNREFUSED;
            break;
        } // unknown ATYP
        uint8_t junk[256];
        if (skip > 0 && egress_io_read(fd, junk, (size_t)skip) != 0) {
            e = errno;
            break;
        }
        if (egress_io_read(fd, junk, 2) != 0) {
            e = errno;
            break;
        } // bound port
        rc = 0;
    } while (0);

    if (was_nb) fcntl(fd, F_SETFL, fl); // restore the guest's non-blocking flag
    if (rc != 0) {
        errno = e ? e : ECONNREFUSED;
        return -1;
    }
    return 0;
}

// host(macOS) AF_UNIX sockaddr -> guest(Linux) layout. The two structs disagree in the leading bytes:
//   Linux  sockaddr_un = { u16 sun_family;             char sun_path[108] }  (AF_UNIX = 1)
//   macOS  sockaddr_un = { u8 sun_len; u8 sun_family;  char sun_path[104] }  (AF_UNIX = 1)
// A raw byte copy (the old non-INET fallback) made the guest read sun_family as sun_len|(AF_UNIX<<8)
// (e.g. 272/362), so a genuine AF_UNIX peer/name looked like an unknown family -> postgres classified a
// unix-socket client as a TCP "host" and rejected it (no pg_hba host entry). Rewrite to a 2-byte family
// and, for a bound pathname socket, reverse-map the host path (upper/lower/volume) back to the guest path
// so getsockname/getpeername report the path the guest actually bound -- not its on-disk overlay location.
// Returns the full Linux address length (Linux reports it even past gcap), or -1 if `m` is not AF_UNIX.
static int sa_un_m2l(const struct sockaddr *m, socklen_t mlen, uint8_t *g, socklen_t gcap) {
    if (!saxl_on() || !m || m->sa_family != AF_UNIX) return -1;
    const struct sockaddr_un *u = (const struct sockaddr_un *)m;
    size_t off = offsetof(struct sockaddr_un, sun_path);
    // Abstract-namespace name (leading NUL), including a kernel autobind address: an opaque binary blob,
    // not a filesystem path -- echo it to the guest verbatim (no volume/path translation, no NUL scan).
    //
    // A leading NUL only means "abstract" on a kernel that reports a TIGHT length.  Measured on both
    // hosts for a socket that was never bound: Linux getsockname reports offsetof(sun_path) == 2, so the
    // branch is not reached at all; Darwin reports the WHOLE sockaddr_un (len=16 unnamed, len=106 bound)
    // with sun_path all-NUL, so an unnamed socket entered this branch and was published to the guest as a
    // 16-byte abstract address where Linux reports the bare 2-byte family.  Programs read that length to
    // decide whether an endpoint has a name at all, so the padding must not be mistaken for a name.
    // Darwin has no abstract namespace (measured: bind() of a leading-NUL address fails ENOENT), and the
    // guest's abstract binds are rewritten to filesystem paths and echoed from g_unix_bind well before
    // this point -- so on Darwin a leading NUL here is padding, never a name.
#if defined(__APPLE__)
    const int abstract_name = 0;
#else
    const int abstract_name = (size_t)mlen > off && u->sun_path[0] == 0;
#endif
    if (abstract_name) {
        size_t alen = (size_t)mlen - off;
        uint8_t t[2 + sizeof u->sun_path];
        if (alen > sizeof u->sun_path) alen = sizeof u->sun_path;
        *(uint16_t *)t = AF_UNIX;
        memcpy(t + 2, u->sun_path, alen);
        int llen = (int)(2 + alen);
        if (g && gcap) memcpy(g, t, (size_t)gcap < (size_t)llen ? gcap : (size_t)llen);
        return llen;
    }
    size_t hplen = (size_t)mlen > off ? (size_t)mlen - off : 0; // path bytes the host reported (no NUL guarantee)
    char hpath[256];
    size_t i = 0;
    for (; i < hplen && i + 1 < sizeof hpath && u->sun_path[i]; i++)
        hpath[i] = u->sun_path[i];
    hpath[i] = 0;
    char canonical[4200];
    const char *backing = hpath;
    /* Kernels preserve the pathname spelling used at bind time, including symlinked ancestors. Volume
     * roots are canonical, so normalize an existing socket pathname before the prefix lookup; otherwise
     * a peer address created through /tmp -> /private/tmp escapes reverse mapping and cannot be echoed. */
    if (hpath[0] == '/' && canonicalize_path(hpath, canonical, sizeof canonical) == 0) backing = canonical;
    char gpath[256];
    int guest_backing = hpath[0] == '/' && g_rootfs != NULL;
    int matched_volume = -1;
    /* unix_sock_at binds from the socket's parent directory when the complete host path exceeds sun_path.
     * The kernel then reports only the leaf name to recvfrom. Recover an exact volume-root peer before
     * translating it; this is the common /tmp datagram shape and remains unambiguous across mounted roots. */
    if (hpath[0] != 0 && hpath[0] != '/')
        for (int volume = 0; volume < g_nvols; ++volume) {
            struct stat status;
            if (g_vols[volume].dead || g_vols[volume].isfile ||
                snprintf(canonical, sizeof canonical, "%s/%s", g_vols[volume].hcanon, hpath) >= (int)sizeof canonical)
                continue;
            if (lstat(canonical, &status) != 0 || !S_ISSOCK(status.st_mode)) continue;
            backing = canonical;
            guest_backing = 1;
            matched_volume = volume;
            break;
        }
    for (int volume = 0; volume < g_nvols; ++volume)
        if (!g_vols[volume].dead && !strncmp(backing, g_vols[volume].hcanon, g_vols[volume].hlen) &&
            (backing[g_vols[volume].hlen] == '/' || backing[g_vols[volume].hlen] == 0) &&
            (matched_volume < 0 || g_vols[volume].hlen > g_vols[matched_volume].hlen)) {
            guest_backing = 1;
            matched_volume = volume;
        }
    if (matched_volume >= 0) {
        if (path_concat(gpath, sizeof gpath, g_vols[matched_volume].guest, backing + g_vols[matched_volume].hlen) != 0)
            gpath[0] = 0;
    } else if (guest_backing) {
        if (guest_from_host(backing, gpath, sizeof gpath) <= 0) gpath[0] = 0;
    } else {
        snprintf(gpath, sizeof gpath, "%s", hpath); // unnamed/autobind (empty) or non-jail: pass through
    }
    uint8_t t[2 + sizeof gpath];
    *(uint16_t *)t = AF_UNIX;
    size_t pl = strlen(gpath);
    memcpy(t + 2, gpath, pl);
    t[2 + pl] = 0;
    int llen = pl ? (int)(2 + pl + 1) : 2; // pathname: family + path + NUL; unnamed: just the family
    if (g && gcap) memcpy(g, t, (size_t)gcap < (size_t)llen ? gcap : (size_t)llen);
    return llen;
}

// ---- abstract-namespace AF_UNIX (sun_path[0]=='\0'): macOS has no abstract namespace, so map the
// abstract name to a real filesystem socket under a per-namespace dir keyed by HL_NETNS (same key as
// ipc_ns_key), so two guests in one container rendezvous and different containers stay isolated. The
// guest socket is already a real host AF_UNIX socket (case 198), so only the ADDRESS is rewritten.
static char g_absdir[200];
static int g_abs_init;

#if defined(HL_NATIVE_TEST_HOOKS)
/*
 * The host->guest sockaddr and control-message translations, reachable without a guest.  Both are
 * Darwin-portability boundaries whose Linux and Darwin inputs differ in shape rather than in value --
 * an unnamed AF_UNIX address and a truncated SCM_RIGHTS record -- so the property under test is what
 * the guest is told, and that is only observable on the far side of the translation.
 *
 *   operation 0: getsockname(fd)   -> sa_un_m2l
 *   operation 1: getpeername(fd)   -> sa_un_m2l
 *   operation 2: recvmsg(fd) into `capacity` host control bytes -> cmsg_m2l
 *   operation 3: apply the AF_UNIX datagram buffer policy to fd
 *
 * `out`/`out_length` carry the guest-visible bytes in and the guest-visible length out.
 */
HL_API int HL_TARGET_LOCAL(socket_shape_test)(uint32_t operation, int fd, uint32_t capacity, uint8_t *out,
                                              uint32_t *out_length) {
    if (fd < 0 || out == NULL || out_length == NULL) return EINVAL;
    if (operation == 3) {
        sock_unix_dgram_buffers(fd);
        *out_length = 0;
        return 0;
    }
    if (operation == 0 || operation == 1) {
        struct sockaddr_storage host_address;
        socklen_t host_length = sizeof host_address;
        memset(&host_address, 0, sizeof host_address);
        if ((operation == 0 ? getsockname(fd, (struct sockaddr *)&host_address, &host_length)
                            : getpeername(fd, (struct sockaddr *)&host_address, &host_length)) != 0)
            return errno != 0 ? errno : EIO;
        int guest_length = sa_un_m2l((struct sockaddr *)&host_address, host_length, out, (socklen_t)*out_length);
        if (guest_length < 0) return EAFNOSUPPORT;
        *out_length = (uint32_t)guest_length;
        return 0;
    }
    if (operation != 2) return EINVAL;
    if (capacity > 4096) return EINVAL;
    uint8_t control[4096];
    uint8_t payload[8];
    struct iovec vector = {.iov_base = payload, .iov_len = sizeof payload};
    struct msghdr message;
    memset(control, 0, sizeof control);
    memset(&message, 0, sizeof message);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    message.msg_control = control;
    message.msg_controllen = capacity;
    if (recvmsg(fd, &message, 0) < 0) return errno != 0 ? errno : EIO;
    int truncated = 0;
    ssize_t written = cmsg_m2l(&message, out, (size_t)*out_length, 0, &truncated);
    if (written < 0) return EIO;
    *out_length = (uint32_t)written;
    (void)truncated; // the guest's MSG_CTRUNC is asserted through the record the translation produced
    return 0;
}
#endif
