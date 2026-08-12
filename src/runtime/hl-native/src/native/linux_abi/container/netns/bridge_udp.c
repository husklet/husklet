static uint32_t br_parse_ip(const char *s) {
    unsigned a = 0, b = 0, cc = 0, d = 0;
    if (sscanf(s, "%u.%u.%u.%u", &a, &b, &cc, &d) != 4) return 0;
    if (a > 255 || b > 255 || cc > 255 || d > 255) return 0;
    return (uint32_t)(a | (b << 8) | (cc << 16) | (d << 24));
}

// Lazy, self-contained env ingestion (mirrors the net_isolate getenv pattern in service.c case 203), so
// the bridge needs no edit to the per-target startup code: HL_NETBR=<netid>, HL_IP=<dotted-quad>.
static void br_init(void) {
    if (g_br_init) return;
    g_br_init = 1;
    const char *interfaces = hl_option_get("HL_NETIFS");
    if (interfaces && interfaces[0]) {
        const char *line = interfaces;
        while (*line && g_netif_count < HL_NETIF_MAX) {
            const char *end = strchr(line, '\n');
            const char *equal = strchr(line, '=');
            size_t bridge_size;
            char ip[20];
            size_t ip_size;
            char *slash;
            char *prefix_end;
            unsigned long prefix;
            if (!end) end = line + strlen(line);
            if (!equal || equal >= end) break;
            bridge_size = (size_t)(equal - line);
            ip_size = (size_t)(end - equal - 1);
            if (bridge_size == 0 || bridge_size > 40 || ip_size == 0 || ip_size >= sizeof ip) break;
            snprintf(g_netif[g_netif_count].path, sizeof g_netif[g_netif_count].path, "/tmp/.hl-bridge-%.*s",
                     (int)bridge_size, line);
            memcpy(ip, equal + 1, ip_size);
            ip[ip_size] = 0;
            slash = strchr(ip, '/');
            if (!slash) break;
            *slash++ = 0;
            errno = 0;
            prefix = strtoul(slash, &prefix_end, 10);
            if (errno || prefix_end == slash || *prefix_end || prefix > 32) break;
            g_netif[g_netif_count].ip = br_parse_ip(ip);
            if (!g_netif[g_netif_count].ip) break;
            g_netif[g_netif_count].prefix = (uint8_t)prefix;
            hl_compat_mkdir(g_netif[g_netif_count].path, 0700);
            g_netif_count++;
            line = *end ? end + 1 : end;
        }
    } else {
        const char *nbr = hl_option_get("HL_NETBR");
        const char *dip = hl_option_get("HL_IP");
        if (nbr && nbr[0] && dip && dip[0]) {
            snprintf(g_netif[0].path, sizeof g_netif[0].path, "/tmp/.hl-bridge-%.40s", nbr);
            g_netif[0].ip = br_parse_ip(dip);
            if (g_netif[0].ip) {
                g_netif[0].prefix = 16;
                hl_compat_mkdir(g_netif[0].path, 0700);
                g_netif_count = 1;
            }
        }
    }
}

static int br_on(void) {
    if (!g_br_init) br_init();
    return g_netif_count != 0;
}

static int br_for_ip(uint32_t ip_be) {
    for (uint8_t i = 0; i < g_netif_count; i++) {
        uint8_t prefix = g_netif[i].prefix;
        uint32_t mask = prefix ? UINT32_MAX << (32u - prefix) : 0;
        if ((ntohl(ip_be) & mask) == (ntohl(g_netif[i].ip) & mask)) return (int)i;
    }
    return -1;
}

// connect(dest): bridge if AF_INET, non-127, in our subnet
static int br_connect_interface(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 8 || *(uint16_t *)sa != AF_INET || sa[4] == 127) return -1;
    return br_for_ip(*(uint32_t *)(sa + 4));
}

// bind(addr): bridge if AF_INET, non-127, and 0.0.0.0 (ANY) / our own IP / in-subnet
static int br_bind_interface(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 8 || *(uint16_t *)sa != AF_INET || sa[4] == 127) return -1;
    uint32_t ip = *(uint32_t *)(sa + 4);
    if (ip == 0) return g_netif_count ? 0 : -1;
    for (uint8_t i = 0; i < g_netif_count; i++)
        if (ip == g_netif[i].ip) return (int)i;
    return br_for_ip(ip);
}

// A STREAM bind the private loopback should own. Explicit 127/8 always; INADDR_ANY (0.0.0.0) too when the
// bridge is OFF -- "any" includes loopback, and with no virtual switch the only reachable address under
// isolation IS loopback, so a server that binds 0.0.0.0 (redis/memcached/nats default) must land on lo_path
// or a same-container 127.0.0.1 connect finds nothing (ENOENT). In bridge mode 0.0.0.0 stays on br_path
// (cross-container + publish); the loopback connect path falls back to our own br endpoint there.
static int lo_any_is(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 8 || *(const uint16_t *)sa != AF_INET) return 0;
    if (sa[4] == 127) return 1;
    return *(const uint32_t *)(sa + 4) == 0 && !br_on();
}

// rendezvous path for <ip_be>:<port> under the shared per-network dir
static int br_path(int interface, uint32_t ip_be, uint16_t port, char *out, size_t n) {
    if (out == NULL || n == 0 || interface < 0 || interface >= g_netif_count) {
        if (out != NULL && n != 0) out[0] = 0;
        errno = EINVAL;
        return -1;
    }
    const uint8_t *b = (const uint8_t *)&ip_be;
    int size = snprintf(out, n, "%s/%u.%u.%u.%u:%u", g_netif[interface].path, b[0], b[1], b[2], b[3], (unsigned)port);
    if (size < 0 || (size_t)size >= n) {
        out[0] = 0;
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

static int br_v6only_path(int interface, uint32_t ip_be, uint16_t port, char *out, size_t n) {
    if (br_path(interface, ip_be, port, out, n) != 0) return -1;
    size_t length = strlen(out);
    int size = snprintf(out + length, n - length, ".v6only");
    if (size < 0 || (size_t)size >= n - length) {
        out[0] = 0;
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

// A wildcard listener belongs to every attached interface, not just eth0. The switch transport is one
// AF_UNIX listening socket, so expose that same inode at each interface's rendezvous name with symlinks.
// connect(2) follows the symlink to the bound socket. The aliases are owned by the socket's shared
// udp_ref and therefore disappear only when the final dup/fork reference closes.
static int br_alias_wildcard_listener(int fd, int primary, uint16_t port, int v6only) {
    char target[200];
    int target_status = v6only ? br_v6only_path(primary, g_netif[primary].ip, port, target, sizeof target)
                               : br_path(primary, g_netif[primary].ip, port, target, sizeof target);
    if (target_status != 0) return -1;
    for (uint8_t interface = 0; interface < g_netif_count; interface++) {
        if ((int)interface == primary) continue;
        char alias[200];
        int alias_status = v6only ? br_v6only_path(interface, g_netif[interface].ip, port, alias, sizeof alias)
                                  : br_path(interface, g_netif[interface].ip, port, alias, sizeof alias);
        if (alias_status != 0) return -1;
        unlink(alias);
        if (symlink(target, alias) < 0) return -1;
        if (udp_ref_add_alias(fd, alias) < 0) {
            int saved = errno;
            unlink(alias);
            errno = saved;
            return -1;
        }
    }
    return 0;
}

// bind(:0) on the bridge -> a free, round-trippable ephemeral port keyed by OUR ip (cf. lo_alloc_ephemeral)
static int br_alloc_ephemeral(int interface, uint16_t *port) {
    static uint16_t next;
    if (port == NULL) {
        errno = EINVAL;
        return -1;
    }
    if (next < 1024) next = (uint16_t)(20000 + (getpid() & 0x3fff));
    for (int tries = 0; tries < 45000; tries++) {
        uint16_t cand = next++;
        if (next < 1024) next = 1024;
        if (cand < 1024) continue;
        char path[200];
        if (br_path(interface, g_netif[interface].ip, cand, path, sizeof path) != 0) return -1;
        if (access(path, F_OK) != 0) {
            *port = cand;
            return 0;
        }
    }
    errno = EADDRINUSE;
    return -1;
}

// report the VIRTUAL AF_INET <ip_be>:<port> (not the AF_UNIX path) back to the guest
static void fill_inet_br(uint8_t *sa, socklen_t *l, uint32_t ip_be, uint16_t port) {
    if (!sa) return;
    *(uint16_t *)(sa + 0) = AF_INET;
    *(uint16_t *)(sa + 2) = htons(port);
    *(uint32_t *)(sa + 4) = ip_be;
    memset(sa + 8, 0, 8);
    if (l) *l = 16;
}

// ---- Published-port host forwarder (`docker run -p HOST:CONTAINER`) -----------------------------
// A guest that binds+listens on a published container port does so on the AF_UNIX virtual switch
// (br_path for a 0.0.0.0/eth0 bind, lo_path for a 127.0.0.1 bind) -- reachable by peer containers, but
// NOT by a host process dialing localhost:HOST_PORT, because nothing on the host listens on an AF_INET
// socket for HOST_PORT. This bridges that gap: when the guest listen()s on a published port we start a
// REAL host AF_INET listener on 0.0.0.0:HOST_PORT (matching docker's default publish address, which the
// daemon also reports via NetworkSettings.Ports), and for each accepted host connection we dial the
// guest's AF_UNIX switch socket and relay bytes both ways. The guest's own accept() returns the relayed
// connection exactly as if a peer container had connected, so container<->container, egress, the switch
// and --network none/host are completely untouched -- this purely ADDS the host->container path.
// (Uses the container's explicit port-map state from state.c.) TCP only for now;
// published UDP is a follow-up (the guest's UDP path isn't switch-redirected today).

// Is `cport` a published container port? (pm_host() returns cport on a miss, so it can't answer this.)
static int pm_published(uint16_t cport) {
    return hl_linux_ports_contains(&g_ports, cport);
}

// Host ports we've already spun up a TCP / UDP forwarder for (idempotent across re-listen()/re-bind under
// SO_REUSEADDR). A port is marked BEFORE its forwarder thread is created and the thread UNMARKS it if its
// own bind() fails, so a transient EADDRINUSE doesn't permanently disable forwarding for that port (F7).
static uint16_t g_fwd_started[32];
static int g_nfwd;
static uint16_t g_udp_fwd_started[32];
static int g_nudpfwd;

static void fwd_unmark(uint16_t *arr, int *n, uint16_t hport) {
    for (int i = 0; i < *n; i++)
        if (arr[i] == hport) {
            arr[i] = arr[--(*n)];
            return;
        }
}

// One relay connection: pump bytes between host TCP fd `a` and switch AF_UNIX fd `b` until either EOF.
struct fwd_relay {
    int a, b;
};

// Copy one ready direction src->dst. Returns 0 to keep going, -1 to tear the whole connection down.
// On src EOF we half-close dst (shutdown SHUT_WR) so the peer sees the FIN and can finish its reply,
// and mark that direction done; the connection ends only once BOTH directions have closed.
static int fwd_pump(int src, int dst, int *src_done, char *buf, size_t cap) {
    ssize_t n = read(src, buf, cap);
    if (n == 0) {
        shutdown(dst, SHUT_WR);
        *src_done = 1;
        return 0;
    }
    if (n < 0) {
        if (errno == EINTR || errno == EAGAIN) return 0;
        return -1;
    }
    for (ssize_t off = 0; off < n;) {
        ssize_t w = write(dst, buf + off, (size_t)(n - off));
        if (w <= 0) {
            if (w < 0 && (errno == EINTR || errno == EAGAIN)) continue;
            return -1;
        }
        off += w;
    }
    return 0;
}

static void *fwd_relay_thread(void *p) {
    struct fwd_relay r = *(struct fwd_relay *)p;
    free(p);
    char buf[65536];
    int a_done = 0, b_done = 0; // host->guest / guest->host directions closed
    while (!a_done || !b_done) {
        struct pollfd pf[2] = {{r.a, a_done ? 0 : POLLIN, 0}, {r.b, b_done ? 0 : POLLIN, 0}};
        if (poll(pf, 2, -1) < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (!a_done && (pf[0].revents & (POLLIN | POLLHUP | POLLERR)))
            if (fwd_pump(r.a, r.b, &a_done, buf, sizeof buf) < 0) break;
        if (!b_done && (pf[1].revents & (POLLIN | POLLHUP | POLLERR)))
            if (fwd_pump(r.b, r.a, &b_done, buf, sizeof buf) < 0) break;
    }
    close(r.a);
    close(r.b);
    return NULL;
}

static int switch_dial(const char *path); // defined below; gap-tolerant AF_UNIX switch dial

struct fwd_listen {
    uint16_t hport;
    uint32_t address;
    char upath[200]; // full switch path; truncated into sun_path exactly as the guest's bind did
};

static void *fwd_listen_thread(void *p) {
    struct fwd_listen fl = *(struct fwd_listen *)p;
    free(p);
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    if (ls < 0) return NULL;
    int on = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    struct sockaddr_in sin;
    memset(&sin, 0, sizeof sin);
    sin.sin_family = AF_INET;
    sin.sin_port = htons(fl.hport);
    sin.sin_addr.s_addr = fl.address;
    if (bind(ls, (struct sockaddr *)&sin, sizeof sin) < 0 || listen(ls, 128) < 0) {
        close(ls); // host port busy (e.g. another container already published it) -> no forwarding
        fwd_unmark(g_fwd_started, &g_nfwd, fl.hport); // transient busy: allow a later listen() to retry (F7)
        return NULL;
    }
    for (;;) {
        int hc = accept(ls, NULL, NULL);
        if (hc < 0) {
            if (errno == EINTR) continue;
            break;
        }
        // Dial the guest's switch listen socket (same truncation the guest used when it bound it),
        // retrying briefly across a re-listen gap: a published server looping `nc -l -w N` rebinds the
        // switch inode between connections, so a host connection that lands in the gap sees ENOENT (inode
        // gone) or ECONNREFUSED (stale inode, nothing accepting). Recreate + retry for ~600ms (mirrors TCP
        // SYN retransmit), then drop the host connection. A genuinely-dead guest still fails after the cap.
        int gc = switch_dial(fl.upath);
        if (gc < 0) {
            close(hc);
            continue;
        }
        struct fwd_relay *fr = malloc(sizeof *fr);
        if (!fr) {
            close(gc);
            close(hc);
            continue;
        }
        fr->a = hc;
        fr->b = gc;
        pthread_t t;
        if (pthread_create(&t, NULL, fwd_relay_thread, fr) != 0) {
            free(fr);
            close(gc);
            close(hc);
            continue;
        }
        pthread_detach(t);
    }
    close(ls);
    return NULL;
}

// Dial an AF_UNIX switch socket at `path`, retrying briefly across a peer's re-listen gap: a server
// looping `nc -l -w N` (or any accept-one-then-rebind pattern) unbinds+rebinds the switch inode between
// connections, so a dial that lands in that window sees ENOENT (inode gone) or ECONNREFUSED (stale inode,
// nothing accepting yet). Recreate the socket + retry for ~600ms (mirrors TCP SYN retransmission across a
// transient backlog gap); a genuinely-absent peer still fails after the cap. Returns a connected fd or -1.
// A connection that immediately HUPs with no readable data is a peer that's mid-exit: a `-w N` listener
// whose accept-window just closed (busybox `nc -l -w 1` loops align their 1-second boundary with the
// scenario's `sleep 1`, so a single-shot client connects exactly as the current listener is exiting). It
// accepts nothing and the socket closes with 0 bytes. Distinguish that from a live connection (data
// pending, or a client-first protocol where the server waits for the request) by a brief poll: only a
// POLLHUP/POLLERR WITHOUT POLLIN means "dead on arrival" -> retry a fresh listener. Anything else is live.
static int switch_dead_on_arrival(int fd) {
    struct pollfd pf = {fd, POLLIN, 0};
    int pr = poll(&pf, 1, 40); // returns at once when readable/closed; ~40ms only for a truly-idle live peer
    if (pr <= 0) return 0;     // idle but live: a client-first protocol (server awaits the request) or slow
    // Readable: could be real data OR a peer-closed EOF. PEEK (consumes nothing, so the guest's later read
    // still sees any data): 0 bytes == the peer closed with nothing to serve == dead on arrival.
    char b[1];
    ssize_t n = recv(fd, b, 1, MSG_PEEK | MSG_DONTWAIT);
    if (n == 0) return 1;                                             // clean EOF, no data -> dead
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return 0; // spurious wake, live
    if (n < 0) return 1;                                              // ECONNRESET/ECONNREFUSED -> dead
    return 0;                                                         // real data pending -> live
}

static int switch_dial(const char *path) {
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    size_t path_len = strlen(path);
    if (path_len >= sizeof un.sun_path) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(un.sun_path, path, path_len + 1);
    for (int attempt = 0; attempt < 60; attempt++) {
        int fd = socket(AF_UNIX, SOCK_STREAM, 0);
        if (fd < 0) return -1;
        if (connect(fd, (struct sockaddr *)&un, sizeof un) == 0) {
            // Peer mid-exit guard: a `-w N` listener whose window just closed accepts nothing and the
            // connection HUPs with no data. Detect that and retry a fresh listener; a live peer (data
            // pending, or a client-first published service) is kept.
            if (!switch_dead_on_arrival(fd)) return fd;
            close(fd);
        } else {
            int e = errno;
            close(fd);
            if (e != ENOENT && e != ECONNREFUSED) return -1;
        }
        struct timespec ts = {0, 20000000}; // 20ms
        nanosleep(&ts, NULL);
    }
    return -1;
}

// Is the daemon owning the process-independent host->container TCP forwarder? When set
// (HL_PUBLISH_DAEMON=1), the engine must NOT open its own in-process host AF_INET listener — that listener
// lived in whichever guest process called listen(), so a prefork / re-listening server tore it down on
// every rebind and two guest processes raced EADDRINUSE. The daemon's listener (hl-daemon/containers/
// ports.rs) outlives every guest process and dials this container's switch inode per connection instead.
// The guest-side bind/listen->switch redirect + getsockname->cport reporting below are UNCHANGED (the
// daemon relies on them). Cached (env is fixed for the process).
static int g_hostfwd_daemon = -1;

static int hostfwd_by_daemon(void) {
    if (g_hostfwd_daemon < 0) g_hostfwd_daemon = (hl_option_get("HL_PUBLISH_DAEMON") != NULL);
    return g_hostfwd_daemon;
}

// Called from listen(): if `fd` is a published switch-backed listening socket, start its host forwarder.
static void fwd_maybe_start(int fd) {
    if (hostfwd_by_daemon()) return; // daemon owns the TCP host listener -> don't race it
    if (fd < 0 || fd >= HL_NFD) return;
    uint16_t cport = 0;
    char upath[200];
    if (g_br_port[fd]) {
        cport = g_br_port[fd];
        if (br_path((int)g_br_interface[fd] - 1, g_br_ip[fd], cport, upath, sizeof upath) != 0) return;
    } else if (g_lo_port[fd]) {
        cport = g_lo_port[fd];
        lo_tcp_path(cport, g_lo_v6only[fd], upath, sizeof upath);
    } else
        return; // real host bind (no switch redirect) -> already natively reachable, nothing to do
    if (!pm_published(cport)) return; // not a published port
    uint16_t hport = pm_host(cport);
    for (int i = 0; i < g_nfwd; i++)
        if (g_fwd_started[i] == hport) return; // already forwarding this host port
    if (g_nfwd >= 32) return;
    struct fwd_listen *fl = malloc(sizeof *fl);
    if (!fl) return;
    fl->hport = hport;
    fl->address = pm_address(cport);
    snprintf(fl->upath, sizeof fl->upath, "%s", upath);
    g_fwd_started[g_nfwd++] = hport; // mark BEFORE create (closes the dedup window); thread unmarks on bind fail
    pthread_t t;
    if (pthread_create(&t, NULL, fwd_listen_thread, fl) != 0) {
        free(fl);
        g_nfwd--;
        return;
    }
    pthread_detach(t);
}

// ---- Published-port host UDP forwarder (`docker run -p HOST:CONTAINER/udp`) ----------------------
// UDP has no listen()/accept(): a guest UDP server bind()s a datagram socket on the virtual switch
// (AF_UNIX SOCK_DGRAM at br_path/lo_path, set up in the bind hook below) and recvfrom()s it. As with
// TCP nothing on the host owns an AF_INET socket for HOST_PORT, so a host process sending to
// localhost:HOST_PORT never reaches the guest. This bridges that gap with a real host
// AF_INET/SOCK_DGRAM socket on 0.0.0.0:HOST_PORT that relays datagrams to/from the guest's switch
// socket. Because UDP is connectionless, replies must route back to the right host client: we give
// EACH distinct host client its own guest-facing AF_UNIX/SOCK_DGRAM socket bound to a unique synthetic
// path, and send the client's datagrams to the guest FROM that socket. The guest's recvfrom() then sees
// that synthetic path as the source address, and a standard server that sendto()s its reply back to the
// recvfrom source lands on exactly that per-client socket -- which the forwarder maps back to the host
// client. So per-peer reply routing falls out of the normal UDP request/reply pattern, with no change
// to the guest's sendto/recvfrom path (AF_UNIX addresses pass through service.c's sa_l2m/sa_m2l raw).
// Scoped to PUBLISHED ports only (pm_published): non-published UDP is left entirely untouched (real
// host bind / egress / DNS unchanged), and it is a no-op on --network host/none (br_on()/lo_on() off),
// mirroring the TCP forwarder's guards. TCP publishing, container<->container and egress are unaffected.

// Swap the AF_INET socket at `fd` for a fresh AF_UNIX SOCK_DGRAM one (keeping the fd number + flags).
// Mirrors lo_swap() but for datagram sockets (the switch-backed UDP server socket).
static int udp_swap(int fd) {
    int fl = fcntl(fd, F_GETFL), df = fcntl(fd, F_GETFD);
    int u = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (u < 0) return -1;
    if (u != fd) {
        if (dup2(u, fd) < 0) {
            close(u);
            return -1;
        }
        close(u);
    }
    if (fl >= 0 && (fl & O_NONBLOCK)) fcntl(fd, F_SETFL, O_NONBLOCK);
    if (df >= 0 && (df & FD_CLOEXEC)) fcntl(fd, F_SETFD, FD_CLOEXEC);
    return 0;
}

// Route every private-network IPv4 datagram through the same AF_UNIX rendezvous namespace as streams.
// The pathname is also the sender identity returned by recvfrom/recvmsg, so replies need no side table.
static int udp_switch_bind(int fd, int interface, uint32_t ip, uint16_t port) {
    char path[200];
    if (!port && interface >= 0) {
        if (br_alloc_ephemeral(interface, &port) != 0) return -1;
    } else if (!port) {
        port = lo_alloc_ephemeral();
    }
    if (!port) {
        errno = EADDRINUSE;
        return -1;
    }
    if (interface >= 0) {
        if (br_path(interface, ip, port, path, sizeof path) != 0) return -1;
    } else
        lo_path(port, path, sizeof path);
    struct sockaddr_un un;
    if (unix_addr_set(&un, path) < 0) return -1;
    if (!g_udp_local_port[fd] && udp_swap(fd) < 0) return -1;
    unlink(path);
    if (bind(fd, (struct sockaddr *)&un, sizeof un) < 0) return -1;
    if (udp_ref_create(fd, path) < 0) {
        int saved = errno;
        unlink(path);
        errno = saved;
        return -1;
    }
    g_udp_local_port[fd] = port;
    g_udp_local_ip[fd] = ip;
    g_udp_local_interface[fd] = (uint8_t)(interface + 1);
    return 0;
}

static int udp_switch_ensure_source(int fd, int interface) {
    if (g_udp_local_port[fd]) return 0;
    return udp_switch_bind(fd, interface, interface >= 0 ? g_netif[interface].ip : 0, 0);
}

static int udp_switch_destination(const uint8_t *sa, socklen_t len, int *interface, uint32_t *ip, uint16_t *port,
                                  char *path, size_t capacity) {
    if (lo_on() && lo6_is(sa, len)) {
        *ip = 0;
        *port = ntohs(*(const uint16_t *)(sa + 2));
        *interface = -1;
        lo_path(*port, path, capacity);
        return 1;
    }
    if (!sa || len < 8 || *(const uint16_t *)sa != AF_INET) return 0;
    *ip = *(const uint32_t *)(sa + 4);
    *port = ntohs(*(const uint16_t *)(sa + 2));
    if (lo_on() && sa[4] == 127) {
        *interface = -1;
        lo_path(*port, path, capacity);
        return 1;
    }
    *interface = br_on() ? br_for_ip(*ip) : -1;
    if (*interface >= 0) {
        if (br_path(*interface, *ip, *port, path, capacity) != 0) return -1;
        return 1;
    }
    return 0;
}

// Materialize the rendezvous address for a logically-connected switch-backed UDP socket. UDP connect
// records a default peer but deliberately leaves the AF_UNIX transport unconnected: unlike AF_UNIX,
// Linux UDP connect succeeds when no process is listening and reports refusal on later I/O.
static int udp_switch_peer_path(int fd, char *path, size_t capacity) {
    if (fd < 0 || fd >= HL_NFD || !g_udp_peer_port[fd]) return 0;
    int interface = (int)g_udp_peer_interface[fd] - 1;
    if (interface >= 0) {
        if (br_path(interface, g_udp_peer_ip[fd], g_udp_peer_port[fd], path, capacity) != 0) return -1;
    } else
        lo_path(g_udp_peer_port[fd], path, capacity);
    return 1;
}

// write(2)/writev(2) are valid send operations on a connected datagram socket. The private UDP
// transport deliberately keeps its AF_UNIX backing unconnected so Linux connect() can succeed before
// a peer binds; route descriptor writes to the recorded logical peer explicitly instead of writing the
// unconnected host socket and leaking EDESTADDRREQ to applications such as BusyBox nc.
static int udp_switch_write(int fd, const struct iovec *iov, int iov_count, int64_t *result) {
    char path[200];
    if (fd < 0 || fd >= HL_NFD || !g_sock_dgram[fd]) return 0;
    int route = udp_switch_peer_path(fd, path, sizeof path);
    if (route < 0) {
        *result = -errno;
        return 1;
    }
    if (route == 0) return 0;
    int interface = (int)g_udp_peer_interface[fd] - 1;
    if (udp_switch_ensure_source(fd, interface) < 0) {
        *result = -errno;
        return 1;
    }
    struct sockaddr_un address;
    if (unix_addr_set(&address, path) < 0) {
        *result = -errno;
        return 1;
    }
    struct msghdr message;
    memset(&message, 0, sizeof message);
    message.msg_name = &address;
    message.msg_namelen = sizeof address;
    message.msg_iov = (struct iovec *)iov;
    message.msg_iovlen = iov_count;
    ssize_t sent = sendmsg(fd, &message, 0);
    *result = sent < 0 ? -errno : sent;
    return 1;
}

static int udp_switch_source(const struct sockaddr_storage *source, socklen_t length, uint8_t *guest,
                             socklen_t *guest_length) {
    if (!source || source->ss_family != AF_UNIX || length < offsetof(struct sockaddr_un, sun_path) + 2) return 0;
    const char *path = ((const struct sockaddr_un *)source)->sun_path;
    unsigned port;
    if (g_netns[0] && !strncmp(path, g_netns, strlen(g_netns)) && sscanf(path + strlen(g_netns), "/p%u", &port) == 1) {
        fill_inet_lo(guest, guest_length, (uint16_t)port);
        return 1;
    }
    for (int i = 0; i < g_netif_count; i++) {
        size_t prefix = strlen(g_netif[i].path);
        unsigned a, b, c, d;
        if (!strncmp(path, g_netif[i].path, prefix) &&
            sscanf(path + prefix, "/%u.%u.%u.%u:%u", &a, &b, &c, &d, &port) == 5 && a < 256 && b < 256 && c < 256 &&
            d < 256 && port < 65536) {
            uint8_t bytes[4] = {(uint8_t)a, (uint8_t)b, (uint8_t)c, (uint8_t)d};
            uint32_t address;
            memcpy(&address, bytes, sizeof address);
            fill_inet_br(guest, guest_length, address, (uint16_t)port);
            return 1;
        }
    }
    return 0;
}

#define UDP_FWD_MAXPEERS 64

struct udp_peer {
    struct sockaddr_storage caddr; // host client addr (macOS layout, as recvfrom delivered it)
    socklen_t calen;
    int gs;          // guest-facing AF_UNIX/SOCK_DGRAM socket (bound to its own path,
                     // connected to the guest switch socket) -- this client's identity
    unsigned pathid; // the pseq used to build this peer's bound socket path (pdir/<pathid>); kept so the
                     // on-disk inode can be unlink'd when the peer is evicted or the forwarder tears down
    int used;
};

struct udp_fwd {
    uint16_t hport;
    uint32_t address;
    char upath[200]; // guest switch datagram socket path (br_/lo_path, as the guest bound)
    char pdir[80];   // dir holding this forwarder's synthetic per-client socket paths
    int hs;          // host AF_INET/SOCK_DGRAM socket on the published address and port
    struct udp_peer peers[UDP_FWD_MAXPEERS];
    int npeers;
    unsigned pseq; // monotonic id for unique synthetic peer paths (+ ring eviction)
};

// Remove a peer's bound AF_UNIX socket inode (pdir/<pathid>) from the host filesystem. Without this every
// distinct host UDP client leaves a socket file behind forever -> unbounded on-disk inode growth over a
// UDP-heavy container's life (only 64 peers are ever live, but eviction used to close the fd and orphan the
// file). Safe to call for any recorded peer; a missing file just makes unlink() a no-op.
static void udp_peer_unlink(const struct udp_fwd *f, unsigned pathid) {
    char path[sizeof f->pdir + 24];
    snprintf(path, sizeof path, "%s/%u", f->pdir, pathid);
    unlink(path);
}

// Find the peer slot for host client (sa,sl), or create one (its own AF_UNIX dgram socket bound to a
// fresh synthetic path and connected to the guest switch socket). Returns the guest-facing fd or -1.
static int udp_peer_get(struct udp_fwd *f, const struct sockaddr *sa, socklen_t sl) {
    for (int i = 0; i < f->npeers; i++)
        if (f->peers[i].used && f->peers[i].calen == sl && memcmp(&f->peers[i].caddr, sa, sl) == 0)
            return f->peers[i].gs;
    int slot, appended = 0;
    if (f->npeers < UDP_FWD_MAXPEERS) {
        slot = f->npeers;
        appended = 1;
    } else { // table full: evict a slot round-robin (oldest-ish) so new clients still work
        slot = (int)(f->pseq % UDP_FWD_MAXPEERS);
        if (f->peers[slot].used) {
            close(f->peers[slot].gs);
            udp_peer_unlink(f, f->peers[slot].pathid); // drop the evicted client's on-disk socket inode
        }
        f->peers[slot].used = 0;
    }
    // On an eviction failure the slot is already cleared (used=0) above and stays reusable in place; we must
    // NOT decrement npeers here. !appended means the table was full (npeers==UDP_FWD_MAXPEERS), so lowering it
    // would drop the LAST slot out of the poll + teardown-close loops -> that peer's fd + inode would leak and
    // its forwarding would silently stop. Append failures never bumped npeers (done only on success below), so
    // they need no adjustment either. (Supersedes the earlier F5 npeers-- which caused exactly that hide/leak.)
    int gs = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (gs < 0) return -1;
    unsigned pathid = f->pseq++;
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s/%u", f->pdir, pathid);
    unlink(un.sun_path);
    if (bind(gs, (struct sockaddr *)&un, sizeof un) < 0) {
        close(gs);
        return -1;
    }
    struct sockaddr_un gu;
    memset(&gu, 0, sizeof gu);
    gu.sun_family = AF_UNIX;
    size_t upath_len = strlen(f->upath);
    if (upath_len >= sizeof gu.sun_path) {
        close(gs);
        unlink(un.sun_path);
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(gu.sun_path, f->upath, upath_len + 1);
    if (connect(gs, (struct sockaddr *)&gu, sizeof gu) < 0) {
        close(gs);
        unlink(un.sun_path);
        return -1;
    }
    if (appended) f->npeers++;
    f->peers[slot].used = 1;
    f->peers[slot].gs = gs;
    f->peers[slot].pathid = pathid;
    f->peers[slot].calen = sl;
    memcpy(&f->peers[slot].caddr, sa, sl < sizeof f->peers[slot].caddr ? sl : sizeof f->peers[slot].caddr);
    return gs;
}

static void *udp_fwd_thread(void *p) {
    struct udp_fwd *f = (struct udp_fwd *)p; // heap-owned by this thread for its lifetime
    int hs = socket(AF_INET, SOCK_DGRAM, 0);
    if (hs < 0) {
        fwd_unmark(g_udp_fwd_started, &g_nudpfwd, f->hport);
        free(f);
        return NULL;
    }
    int on = 1;
    setsockopt(hs, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    struct sockaddr_in sin;
    memset(&sin, 0, sizeof sin);
    sin.sin_family = AF_INET;
    sin.sin_port = htons(f->hport);
    sin.sin_addr.s_addr = f->address;
    if (bind(hs, (struct sockaddr *)&sin, sizeof sin) < 0) { // host port busy -> no forwarding
        close(hs);
        fwd_unmark(g_udp_fwd_started, &g_nudpfwd, f->hport); // transient busy: allow a later bind() to retry (F7)
        free(f);
        return NULL;
    }
    f->hs = hs;
    snprintf(f->pdir, sizeof f->pdir, "/tmp/.hl-udp.%d.%u", (int)getpid(), (unsigned)f->hport);
    hl_compat_mkdir(f->pdir, 0700);
    char buf[65536];
    for (;;) {
        struct pollfd pf[1 + UDP_FWD_MAXPEERS];
        pf[0].fd = hs;
        pf[0].events = POLLIN;
        pf[0].revents = 0;
        int n = 1;
        for (int i = 0; i < f->npeers; i++) {
            if (!f->peers[i].used) continue;
            pf[n].fd = f->peers[i].gs;
            pf[n].events = POLLIN;
            pf[n].revents = 0;
            n++;
        }
        if (poll(pf, n, -1) < 0) {
            if (errno == EINTR) continue;
            break;
        }
        // host client -> guest: per-client guest-facing socket preserves the reply path
        if (pf[0].revents & (POLLIN | POLLERR)) {
            struct sockaddr_storage ca;
            socklen_t cl = sizeof ca;
            ssize_t r = recvfrom(hs, buf, sizeof buf, 0, (struct sockaddr *)&ca, &cl);
            if (r >= 0) {
                int gs = udp_peer_get(f, (struct sockaddr *)&ca, cl);
                if (gs >= 0) send(gs, buf, (size_t)r, 0); // connected to the guest switch socket
            }
        }
        // guest replies -> back out to the originating host client (the socket that received it)
        for (int i = 0; i < f->npeers; i++) {
            if (!f->peers[i].used) continue;
            int hit = 0;
            for (int j = 1; j < n; j++)
                if (pf[j].fd == f->peers[i].gs && (pf[j].revents & (POLLIN | POLLERR))) {
                    hit = 1;
                    break;
                }
            if (!hit) continue;
            // Non-blocking: the pre-poll pf[] readiness can match a REUSED fd-number of an evicted+recreated
            // peer socket; a blocking recv() would then hang forever. EAGAIN -> spurious wakeup, just skip (F4).
            ssize_t r = recv(f->peers[i].gs, buf, sizeof buf, MSG_DONTWAIT);
            if (r >= 0) sendto(hs, buf, (size_t)r, 0, (struct sockaddr *)&f->peers[i].caddr, f->peers[i].calen);
        }
    }
    for (int i = 0; i < f->npeers; i++)
        if (f->peers[i].used) {
            close(f->peers[i].gs);
            udp_peer_unlink(f, f->peers[i].pathid); // release each live peer's socket inode on teardown
        }
    close(hs);
    rmdir(f->pdir); // all peer inodes are gone -> drop the now-empty per-forwarder dir
    free(f);
    return NULL;
}

// Called from the UDP bind hook once a published switch-backed datagram socket is bound: start its host
// forwarder. Mirrors fwd_maybe_start() but triggers at bind (UDP has no listen) and keys off g_*_port,
// which the bind hook just set on this fd.
static void udp_fwd_maybe_start(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    uint16_t cport = 0;
    char upath[200];
    if (g_br_port[fd]) {
        cport = g_br_port[fd];
        if (br_path((int)g_br_interface[fd] - 1, g_br_ip[fd], cport, upath, sizeof upath) != 0) return;
    } else if (g_lo_port[fd]) {
        cport = g_lo_port[fd];
        lo_path(cport, upath, sizeof upath);
    } else
        return;
    if (!pm_published(cport)) return;
    uint16_t hport = pm_host(cport);
    for (int i = 0; i < g_nudpfwd; i++)
        if (g_udp_fwd_started[i] == hport) return; // already forwarding this host port
    if (g_nudpfwd >= 32) return;
    struct udp_fwd *f = (struct udp_fwd *)calloc(1, sizeof *f);
    if (!f) return;
    f->hport = hport;
    f->address = pm_address(cport);
    snprintf(f->upath, sizeof f->upath, "%s", upath);
    g_udp_fwd_started[g_nudpfwd++] = hport; // mark BEFORE create; thread unmarks on bind fail (F7)
    pthread_t t;
    if (pthread_create(&t, NULL, udp_fwd_thread, f) != 0) {
        free(f);
        g_nudpfwd--;
        return;
    }
    pthread_detach(t);
}

// UDP bind hook: if `fd` is an AF_INET datagram socket binding a PUBLISHED container port on the bridge
// (0.0.0.0/own-ip/in-subnet) or private loopback, swap it onto an AF_UNIX/SOCK_DGRAM switch socket and
// start the host->guest forwarder. Returns 1 if handled (result in *out), 0 to let the caller bind
// normally (non-published UDP, non-switch nets, or anything not AF_INET datagram -> untouched).
static int udp_bind_maybe(int fd, const uint8_t *sa, socklen_t l, int64_t *out) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_dgram[fd]) return 0;
    uint16_t cport;
    char up[200];
    uint32_t myip = 0;
    int interface = br_bind_interface(sa, l);
    if (br_on() && interface >= 0) {
        cport = ntohs(*(const uint16_t *)(sa + 2));
        if (cport == 0 || !pm_published(cport)) return 0; // only explicit, published ports get switched
        myip = g_netif[interface].ip;
        if (br_path(interface, myip, cport, up, sizeof up) != 0) {
            *out = -errno;
            return 1;
        }
    } else if (lo_on() && lo_is(sa, l)) {
        cport = ntohs(*(const uint16_t *)(sa + 2));
        if (cport == 0 || !pm_published(cport)) return 0;
        lo_path(cport, up, sizeof up);
    } else {
        return 0;
    }
    size_t up_len = strlen(up);
    if (up_len >= sizeof(((struct sockaddr_un *)0)->sun_path)) {
        *out = -ENAMETOOLONG;
        return 1;
    }
    if (udp_swap(fd) < 0) {
        *out = -errno;
        return 1;
    }
    unlink(up);
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    memcpy(un.sun_path, up, up_len + 1);
    int r = bind(fd, (struct sockaddr *)&un, sizeof un);
    if (r == 0) {
        if (myip) {
            g_br_port[fd] = cport;
            g_br_ip[fd] = myip;
            g_br_interface[fd] = (uint8_t)(interface + 1);
        } else {
            g_lo_port[fd] = cport;
        }
        udp_fwd_maybe_start(fd);
    }
    *out = r < 0 ? -errno : 0;
    return 1;
}

// ===== Linux <-> macOS sockaddr translation (AF_INET / AF_INET6) — gate: NOSOCKADDR=1 =====
// The non-isolated socket paths (real host TCP/UDP via bind/connect/accept/getsockname/getpeername/
// sendto/recvfrom/sendmsg) used to hand the guest's *Linux*-layout sockaddr straight to a macOS
// syscall (and vice-versa on output). The two layouts differ in the first two bytes:
//   Linux  sockaddr_in  = { u16 sin_family;  u16 sin_port; u32 sin_addr;  u8 pad[8] }   (AF_INET =2)
//   macOS  sockaddr_in  = { u8 sin_len; u8 sin_family; u16 sin_port; u32 sin_addr; ... } (AF_INET =2)
//   Linux  sockaddr_in6 = { u16 sin6_family; u16 port; u32 flow; u8 addr[16]; u32 scope}(AF_INET6=10)
//   macOS  sockaddr_in6 = { u8 len; u8 sin6_family; u16 port; u32 flow; u8 addr[16]; u32 scope}(=30)
// So a guest AF_INET(2) read as macOS becomes sin_len=2/sin_family=0 (AF_UNSPEC) -> the server never
// really binds; and host output read back as Linux yields sin_family = 0x0210 = 528 (garbage). AF_INET6
// additionally differs in the family *value* (10 vs 30). port/addr/flow/scope share offsets+encoding
// (network byte order) so only family/len need fixing. AF_UNIX and other families pass through.
#define LX_AF_INET 2
#define LX_AF_INET6 10
