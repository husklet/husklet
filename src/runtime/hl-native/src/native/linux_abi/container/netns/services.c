static void synthetic_reply_send(int fd, const void *data, size_t size, int stream) {
    int flags = MSG_DONTWAIT;
#if defined(__linux__)
    flags |= MSG_NOSIGNAL;
#endif
    ssize_t sent;
    do
        sent = send(g_dns_peer[fd], data, size, flags);
    while (sent < 0 && errno == EINTR);
    if (stream && sent != (ssize_t)size) {
        close(g_dns_peer[fd]);
        g_dns_peer[fd] = -1;
    }
}

// DNS interception is off under HL_NET_ISOLATE: the isolated network has no resolver, so
// :53 to 127.0.0.11 is left to fall through to the (dead) host loopback and name resolution fails, matching.
static int g_dns_off = -1;

static int dns_enabled(void) {
    if (g_dns_off < 0) g_dns_off = (hl_option_get("HL_NET_ISOLATE") != NULL);
    return !g_dns_off;
}

// A Linux sockaddr_in destined for the embedded nameserver 127.0.0.11:53 (family value 2 == macOS AF_INET).
static int dns_dest_is(const uint8_t *sa, socklen_t l) {
    return sa && l >= 8 && *(const uint16_t *)sa == AF_INET && *(const uint16_t *)(sa + 2) == htons(53) &&
           *(const uint32_t *)(sa + 4) == HL_DNS_NS;
}

// Report the nameserver's address (127.0.0.11:53) back to the guest as the packet source / peer.
static void dns_fill_ns(uint8_t *sa, socklen_t *l) {
    if (!sa) return;
    *(uint16_t *)(sa + 0) = AF_INET;
    *(uint16_t *)(sa + 2) = htons(53);
    *(uint32_t *)(sa + 4) = HL_DNS_NS;
    memset(sa + 8, 0, 8);
    if (l) *l = 16;
}

// Swap the guest's AF_INET DNS socket for one end of an AF_UNIX socketpair (keeping the fd number + flags);
// stash the other end so send handlers can push synthesized responses into it. Idempotent per fd.
static int dns_swap(int fd, int stream) {
    if (fd < 0 || fd >= HL_NFD) return -1;
    if (g_dns_sock[fd]) return 0;
    int fl = fcntl(fd, F_GETFL), df = fcntl(fd, F_GETFD);
    int sv[2];
    if (socketpair(AF_UNIX, stream ? SOCK_STREAM : SOCK_DGRAM, 0, sv) < 0) return -1;
    if (sv[0] != fd) {
        if (dup2(sv[0], fd) < 0) {
            close(sv[0]);
            close(sv[1]);
            return -1;
        }
        close(sv[0]);
    }
    if (fl >= 0 && (fl & O_NONBLOCK)) fcntl(fd, F_SETFL, O_NONBLOCK);
    if (df >= 0 && (df & FD_CLOEXEC)) fcntl(fd, F_SETFD, FD_CLOEXEC);
    fcntl(sv[1], F_SETFD, FD_CLOEXEC); // engine end never leaks across a guest execve
    (void)hl_native_set_no_sigpipe(sv[1]);
    g_dns_peer[fd] = sv[1];
    g_dns_sock[fd] = 1;
    return 0;
}

static uint16_t icmp_checksum(const void *data, size_t size) {
    const uint8_t *bytes = data;
    uint32_t sum = 0;
    while (size > 1) {
        sum += (uint32_t)((bytes[0] << 8) | bytes[1]);
        bytes += 2;
        size -= 2;
    }
    if (size) sum += (uint32_t)bytes[0] << 8;
    while (sum >> 16)
        sum = (sum & 0xffffu) + (sum >> 16);
    return htons((uint16_t)~sum);
}

static int icmp_swap(int fd) {
    int fl, df, sv[2];
    if (fd < 0 || fd >= HL_NFD) return -1;
    if (g_icmp_sock[fd]) return 0;
    fl = fcntl(fd, F_GETFL);
    df = fcntl(fd, F_GETFD);
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) < 0) return -1;
    if (sv[0] != fd) {
        if (dup2(sv[0], fd) < 0) {
            close(sv[0]);
            close(sv[1]);
            return -1;
        }
        close(sv[0]);
    }
    if (fl >= 0 && (fl & O_NONBLOCK)) fcntl(fd, F_SETFL, O_NONBLOCK);
    if (df >= 0 && (df & FD_CLOEXEC)) fcntl(fd, F_SETFD, FD_CLOEXEC);
    fcntl(sv[1], F_SETFD, FD_CLOEXEC);
    (void)hl_native_set_no_sigpipe(sv[1]);
    g_dns_peer[fd] = sv[1];
    g_icmp_sock[fd] = 1;
    return 0;
}

static int icmp_try_send(int fd, const uint8_t *input, size_t size, const uint8_t *destination,
                         socklen_t destination_size, int64_t *result) {
    uint8_t reply[2048];
    uint8_t *icmp = reply;
    size_t reply_size = size;
    uint32_t peer;
    if (fd < 0 || fd >= HL_NFD || !g_icmp_kind[fd] || input == NULL || size < 8 || size > 2000) return 0;
    if (destination && destination_size >= 8 && *(const uint16_t *)destination == AF_INET)
        peer = *(const uint32_t *)(destination + 4);
    else
        peer = g_icmp_ip[fd];
    if (!peer) {
        *result = -EDESTADDRREQ;
        return 1;
    }
    // Loopback ping (127/8) is a purely local echo: the kernel reflects the request without touching a wire,
    // so synthesize the reply ourselves regardless of a configured bridge. This is the container-healthcheck
    // `ping 127.0.0.1` / `ping localhost` case. Off-loopback still requires a bridge route.
    int loopback = (peer & 0xffu) == 127u;
    if (!loopback && (!br_on() || br_for_ip(peer) < 0)) {
        *result = -ENETUNREACH;
        return 1;
    }
    if (icmp_swap(fd) < 0) return 0;
    g_icmp_ip[fd] = peer;
    if (g_icmp_kind[fd] == 2) {
        int bidx = br_for_ip(peer);
        memset(reply, 0, 20);
        reply[0] = 0x45;
        *(uint16_t *)(reply + 2) = htons((uint16_t)(20 + size));
        reply[8] = 64;
        reply[9] = 1;
        *(uint32_t *)(reply + 12) = peer;
        *(uint32_t *)(reply + 16) = bidx >= 0 ? g_netif[bidx].ip : peer;
        *(uint16_t *)(reply + 10) = icmp_checksum(reply, 20);
        icmp = reply + 20;
        reply_size += 20;
    }
    memcpy(icmp, input, size);
    if (icmp[0] == 8) icmp[0] = 0;
    icmp[2] = icmp[3] = 0;
    *(uint16_t *)(icmp + 2) = icmp_checksum(icmp, size);
    synthetic_reply_send(fd, reply, reply_size, 0);
    *result = (int64_t)size;
    return 1;
}

// Encode a dotted host name into DNS wire label format (len-prefixed labels + a 0 terminator). -1 if it
// wouldn't fit. A trailing dot / empty labels are skipped.
static int dns_enc_name(uint8_t *out, int cap, const char *name) {
    int o = 0;
    const char *p = name;
    while (*p) {
        const char *dot = strchr(p, '.');
        int len = dot ? (int)(dot - p) : (int)strlen(p);
        if (len > 63) return -1;
        if (len == 0) {
            if (dot) {
                p = dot + 1;
                continue;
            }
            break;
        }
        if (o + 1 + len >= cap) return -1;
        out[o++] = (uint8_t)len;
        memcpy(out + o, p, len);
        o += len;
        if (!dot) break;
        p = dot + 1;
    }
    if (o + 1 > cap) return -1;
    out[o++] = 0;
    return o;
}

// Decode a DNS name in a QUESTION (no compression) at msg[off] into a dotted string; return the number of
// wire bytes consumed (incl the 0 terminator), or -1 on malformation.
static int dns_dec_qname(const uint8_t *msg, int len, int off, char *name, int ncap) {
    int no = 0, o = off;
    while (o < len) {
        int c = msg[o];
        if (c == 0) {
            o++;
            name[no < ncap ? no : ncap - 1] = 0;
            return o - off;
        }
        if (c & 0xc0) return -1; // a query name is never compressed
        o++;
        if (o + c > len) return -1;
        if (no && no < ncap - 1) name[no++] = '.';
        for (int i = 0; i < c && no < ncap - 1; i++)
            name[no++] = (char)msg[o + i];
        o += c;
    }
    return -1;
}

static int dns_hexval(char ch) {
    if (ch >= '0' && ch <= '9') return ch - '0';
    if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
    if (ch >= 'A' && ch <= 'F') return ch - 'A' + 10;
    return -1;
}

static int dns_ci_suffix(const char *s, const char *suf) {
    size_t ls = strlen(s), lf = strlen(suf);
    return ls >= lf && strcasecmp(s + ls - lf, suf) == 0;
}

// Append one resource record header (name = compression pointer to the question at offset 12) + rdata.
static int dns_put_rr(uint8_t *a, int ao, int cap, uint16_t type, const uint8_t *rdata, int rdlen) {
    if (ao + 12 + rdlen > cap) return -1;
    a[ao++] = 0xc0;
    a[ao++] = 0x0c; // NAME -> ptr to the question's QNAME
    a[ao++] = (uint8_t)(type >> 8);
    a[ao++] = (uint8_t)type;
    a[ao++] = 0;
    a[ao++] = 1; // CLASS IN
    a[ao++] = 0;
    a[ao++] = 0;
    a[ao++] = 0;
    a[ao++] = 30; // TTL = 30s
    a[ao++] = (uint8_t)(rdlen >> 8);
    a[ao++] = (uint8_t)rdlen;
    memcpy(a + ao, rdata, rdlen);
    return ao + rdlen;
}

// Reverse (PTR) lookup: parse the in-addr.arpa / ip6.arpa qname, ask the host, emit a PTR RR. Sets *rcode
// (0 = NODATA when unparseable / no name). Returns the new answer offset.
static int dns_answer_ptr(const char *qname, uint8_t *a, int ao, int cap, int *pan) {
    struct sockaddr_storage ss;
    memset(&ss, 0, sizeof ss);
    socklen_t sl = 0;
    if (dns_ci_suffix(qname, "in-addr.arpa")) {
        unsigned d, cc, b, aa;
        if (sscanf(qname, "%u.%u.%u.%u", &d, &cc, &b, &aa) == 4 && d < 256 && cc < 256 && b < 256 && aa < 256) {
            struct sockaddr_in *s = (struct sockaddr_in *)&ss;
            s->sin_family = AF_INET;
            uint8_t *ip = (uint8_t *)&s->sin_addr;
            ip[0] = (uint8_t)aa;
            ip[1] = (uint8_t)b;
            ip[2] = (uint8_t)cc;
            ip[3] = (uint8_t)d; // qname octets are reversed
            sl = sizeof *s;
        }
    } else if (dns_ci_suffix(qname, "ip6.arpa")) {
        uint8_t nib[32];
        int n = 0;
        for (const char *p = qname; *p && n < 33; p++) {
            if (*p == '.') continue;
            int v = dns_hexval(*p);
            if (v < 0) break;
            if (n < 32) nib[n] = (uint8_t)v;
            n++;
        }
        if (n == 32) {
            struct sockaddr_in6 *s = (struct sockaddr_in6 *)&ss;
            s->sin6_family = AF_INET6;
            uint8_t *ip = (uint8_t *)&s->sin6_addr;
            for (int i = 0; i < 16; i++)
                ip[i] = (uint8_t)((nib[31 - 2 * i] << 4) | nib[31 - 2 * i - 1]);
            sl = sizeof *s;
        }
    }
    if (!sl) return ao; // unparseable -> NODATA (rcode already 0)
    char host[NI_MAXHOST];
    if (getnameinfo((struct sockaddr *)&ss, sl, host, sizeof host, NULL, 0, NI_NAMEREQD) != 0) return ao; // NODATA
    uint8_t enc[300];
    int el = dns_enc_name(enc, sizeof enc, host);
    if (el < 0) return ao;
    int nao = dns_put_rr(a, ao, cap, 12 /*PTR*/, enc, el);
    if (nao < 0) return ao;
    (*pan)++;
    return nao;
}

// reach-by-name: resolve a bare container/alias name to a same-network peer's IP from the daemon's
// LIVE per-network table at `<g_netbr>/.names` (one "ip\tname" line per endpoint, rewritten by the daemon
// on every container start -- so a peer that joined AFTER this container launched, and is therefore absent
// from this container's frozen /etc/hosts snapshot, is still resolvable). Consulted BEFORE the macOS host
// resolver so container names stay instant + offline and never leak to external DNS. Gated to user networks
// (the file is only written for those -- Docker withholds embedded-DNS names on the default bridge).
// Returns 1 + fills *ip_be (network byte order) on a case-insensitive name match; 0 otherwise.
static int dns_local_lookup(const char *qname, uint32_t *ip_be) {
    if (!qname || !qname[0]) return 0;
    br_init();
    for (uint8_t interface = 0; interface < g_netif_count; interface++) {
        char path[256];
        int path_size = snprintf(path, sizeof path, "%s/.names", g_netif[interface].path);
        if (path_size < 0 || (size_t)path_size >= sizeof path) continue;
        FILE *f = fopen(path, "re");
        if (!f) continue;
        char line[512];
        while (fgets(line, sizeof line, f)) {
            char *tab = strchr(line, '\t');
            if (!tab) continue;
            *tab = 0;
            char *name = tab + 1;
            size_t nl = strlen(name);
            while (nl && (name[nl - 1] == '\n' || name[nl - 1] == '\r' || name[nl - 1] == ' ' || name[nl - 1] == '\t'))
                name[--nl] = 0;
            if (strcasecmp(name, qname) == 0) {
                uint32_t ip = br_parse_ip(line);
                if (ip) {
                    if (ip_be) *ip_be = ip;
                    fclose(f);
                    return 1;
                }
            }
        }
        fclose(f);
    }
    return 0;
}

// Build a wire-format response for a wire-format query. Returns the response length, or -1 if the query is
// too malformed to answer at all.
static int dns_build_response(const uint8_t *q, int qlen, uint8_t *out, int cap) {
    if (qlen < 12 || cap < 12) return -1;
    uint16_t id = (uint16_t)((q[0] << 8) | q[1]);
    uint16_t qflags = (uint16_t)((q[2] << 8) | q[3]);
    int opcode = (qflags >> 11) & 0xf;
    uint16_t qd = (uint16_t)((q[4] << 8) | q[5]);
    if (qd < 1) return -1;
    char qname[256];
    int nl = dns_dec_qname(q, qlen, 12, qname, sizeof qname);
    if (nl < 0 || 12 + nl + 4 > qlen) return -1;
    int qtoff = 12 + nl;
    uint16_t qtype = (uint16_t)((q[qtoff] << 8) | q[qtoff + 1]);
    uint16_t qclass = (uint16_t)((q[qtoff + 2] << 8) | q[qtoff + 3]);
    int qsectlen = nl + 4; // the single question we echo back verbatim

    int rcode = 0, ancount = 0;
    uint8_t ans[1600];
    int ao = 0;

    if (opcode != 0 || qclass != 1) {
        rcode = 4;                          // not a standard IN query -> NOTIMP
    } else if (qtype == 1 || qtype == 28) { // A / AAAA
        // Reach-by-name: a same-network peer resolved from the daemon's live table wins over the host
        // resolver (instant, offline, and container names must never escape to external DNS). Local
        // endpoints are IPv4 only: A -> one A RR; AAAA -> NOERROR with no answer (NODATA, name exists but
        // has no v6), which is exactly what Docker's embedded DNS returns for a v4-only service.
        uint32_t local_ip = 0;
        if (dns_local_lookup(qname, &local_ip)) {
            if (qtype == 1) {
                int nao = dns_put_rr(ans, ao, sizeof ans, 1, (uint8_t *)&local_ip, 4);
                if (nao >= 0) {
                    ao = nao;
                    ancount++;
                }
            }
            // rcode stays 0 (NOERROR); AAAA falls through with 0 answers (NODATA). Skip the host resolver.
            goto emit;
        }
        struct addrinfo hints, *res = NULL, *ai;
        memset(&hints, 0, sizeof hints);
        hints.ai_family = AF_UNSPEC; // learn whether the name exists at ALL (so we can tell NXDOMAIN vs NODATA)
        hints.ai_socktype = SOCK_STREAM;
        int grc = getaddrinfo(qname, NULL, &hints, &res);
        if (grc == EAI_NONAME)
            rcode = 3; // NXDOMAIN: the name has no address of any family
        else if (grc != 0)
            rcode = 2; // SERVFAIL: transient resolver failure (EAI_AGAIN/EAI_FAIL/EAI_SYSTEM/...)
        else {
            int want = (qtype == 28) ? AF_INET6 : AF_INET;
            for (ai = res; ai; ai = ai->ai_next) {
                if (ai->ai_family != want) continue;
                if (want == AF_INET) {
                    struct sockaddr_in *s = (struct sockaddr_in *)ai->ai_addr;
                    int nao = dns_put_rr(ans, ao, sizeof ans, 1, (uint8_t *)&s->sin_addr, 4);
                    if (nao < 0) break;
                    ao = nao;
                } else {
                    struct sockaddr_in6 *s = (struct sockaddr_in6 *)ai->ai_addr;
                    int nao = dns_put_rr(ans, ao, sizeof ans, 28, (uint8_t *)&s->sin6_addr, 16);
                    if (nao < 0) break;
                    ao = nao;
                }
                ancount++;
            }
            // ancount==0 here means the name exists but not in the requested family -> NODATA (rcode 0).
        }
        if (res) freeaddrinfo(res);
    } else if (qtype == 12) { // PTR (reverse)
        ao = dns_answer_ptr(qname, ans, ao, (int)sizeof ans, &ancount);
    } else {
        // MX/TXT/SRV/SOA/NS/CAA/HTTPS/SVCB/...: NOERROR + no answer (NODATA) so the client falls back.
        rcode = 0;
    }

emit:; // local-name A/AAAA answer assembled above jumps here, skipping the host resolver
    int need = 12 + qsectlen + ao;
    int tc = 0;
    if (need > cap) { // would overflow (UDP 512) -> truncate: drop answers + set TC so the client retries via TCP
        ao = 0;
        ancount = 0;
        tc = 1;
        need = 12 + qsectlen;
        if (need > cap) return -1;
    }
    out[0] = (uint8_t)(id >> 8);
    out[1] = (uint8_t)id;
    uint16_t rflags = (uint16_t)(0x8000 | (qflags & 0x0100) | 0x0080 | (tc ? 0x0200 : 0) | (rcode & 0xf));
    out[2] = (uint8_t)(rflags >> 8);
    out[3] = (uint8_t)rflags;
    out[4] = 0;
    out[5] = 1; // QDCOUNT
    out[6] = (uint8_t)(ancount >> 8);
    out[7] = (uint8_t)ancount;               // ANCOUNT
    out[8] = out[9] = out[10] = out[11] = 0; // NS/AR counts
    memcpy(out + 12, q + 12, qsectlen);      // echo the question
    memcpy(out + 12 + qsectlen, ans, ao);
    return 12 + qsectlen + ao;
}

// Process one query buffer sent on a DNS socket: build the response and push it into the socketpair peer so
// the guest fd becomes readable. `stream` selects the 2-byte-length-prefixed TCP framing. Returns the byte
// count to report as "sent" (always the whole query -- the guest sees a normal send).
static int64_t dns_send(int fd, const uint8_t *buf, size_t len, int stream) {
    const uint8_t *q = buf;
    size_t qn = len;
    if (stream) {
        if (len < 2) return (int64_t)len; // partial length prefix -> best-effort (resolvers send it whole)
        size_t plen = (size_t)((buf[0] << 8) | buf[1]);
        q = buf + 2;
        qn = len - 2;
        if (qn > plen) qn = plen;
    }
    uint8_t resp[2100];
    int hdr = stream ? 2 : 0;
    int rl = dns_build_response(q, (int)qn, resp + hdr, (int)sizeof resp - hdr);
    if (rl < 0) return (int64_t)len; // unparseable -> swallow (guest retries/times out), never crash
    if (stream) {
        resp[0] = (uint8_t)(rl >> 8);
        resp[1] = (uint8_t)rl;
        rl += 2;
    }
    if (fd >= 0 && fd < HL_NFD && g_dns_sock[fd] && g_dns_peer[fd] >= 0) {
        synthetic_reply_send(fd, resp, (size_t)rl, stream);
    }
    return (int64_t)len;
}

// Send-path entry used by sendto/send/sendmsg/sendmmsg (net.c) + write/writev (io.c). If `fd` is already a
// DNS socket, or `dst` targets 127.0.0.11:53 (lazy first-datagram swap for the unconnected sendto path),
// handle it and set *ret; otherwise return 0 so the caller runs the normal socket path.
static int dns_try_send(int fd, const uint8_t *buf, size_t len, const uint8_t *dst, socklen_t dstlen, int64_t *ret) {
    if (!dns_enabled()) return 0;
    int is_dns = (fd >= 0 && fd < HL_NFD && g_dns_sock[fd]);
    if (!is_dns) {
        if (!dns_dest_is(dst, dstlen)) return 0;
        int stream = (fd >= 0 && fd < HL_NFD) ? g_sock_stream[fd] : 0;
        if (dns_swap(fd, stream) < 0) return 0; // couldn't swap -> let the normal path try
    }
    int stream = (fd >= 0 && fd < HL_NFD) ? g_sock_stream[fd] : 0;
    *ret = dns_send(fd, buf, len, stream);
    return 1;
}

// Gather an iovec array into a scratch buffer (shared by the sendmsg/sendmmsg DNS paths).
static size_t dns_gather(const struct iovec *iv, int ivn, uint8_t *tmp, size_t cap) {
    size_t tl = 0;
    for (int i = 0; iv && i < ivn && tl < cap; i++) {
        size_t n = iv[i].iov_len;
        if (tl + n > cap) n = cap - tl;
        memcpy(tmp + tl, iv[i].iov_base, n);
        tl += n;
    }
    return tl;
}

struct loaded {
    uint64_t entry, phdr, base;
    hl_identity_digest identity;
    int phent, phnum;
};
