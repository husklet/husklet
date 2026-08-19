// ---- NET namespace: per-container private loopback. A container's explicit 127.0.0.0/8 TCP sockets
// are routed to AF_UNIX sockets under a per-namespace host dir the guest can't name (it's path-jailed),
// so each container's localhost is isolated from the host + other containers. 0.0.0.0/external stay
// host-passthrough (so `-p` publishing still works). Off when g_netns[0]==0.
// host dir for this container's loopback unix sockets ("" = no isolation)
// Both names are generated internally from a fixed prefix plus at most 40 identifier bytes.  Keeping
// the declared bound honest proves every derived AF_UNIX rendezvous name fits sun_path.
static char g_netns[64];
// fd -> the loopback port it's bound/connected to (0 = not a private-lo socket)
static uint16_t g_lo_port[HL_NFD];
// fd -> 1 if this private-lo socket is AF_INET6 (so getsockname/getpeername/accept report a sockaddr_in6
// with ::1 instead of an AF_INET 127.0.0.1). Dual-stack listeners use the common port rendezvous; a
// v6-only wildcard listener uses a separate path so IPv4 may bind the same port. This flag picks the
// address family reported back to the guest.
static uint8_t g_lo_v6[HL_NFD];
// fd -> 1 when an AF_INET6 wildcard bind requested IPV6_V6ONLY. Such a
// listener owns a rendezvous distinct from an IPv4 listener on the same port.
static uint8_t g_lo_v6only[HL_NFD];
// fd -> 1 if created SOCK_STREAM (only those get loopback isolation)
static uint8_t g_sock_stream[HL_NFD];
// A stream that was initially switched to AF_UNIX by a virtual bind may later connect to a genuine
// external INET peer. In that case getsockname still reports the guest's virtual bind address, while
// getpeername must query the rehydrated host INET socket instead of mistaking the local virtual address
// for its peer. Carried across dup and cleared when the descriptor is reused.
static uint8_t g_sock_host_backed[HL_NFD];
// Connected to an exact host-projected AF_UNIX socket. Native peers do not understand engine-private
// SCM_RIGHTS trailer descriptors; sending one would poison the peer's descriptor queue.
static uint8_t g_sock_native_peer[HL_NFD];
// fd -> 1 once a stream connect() SUCCEEDED on it. Linux keeps a connected stream socket in SS_CONNECTED
// (a second connect -> EISCONN) until close, even after the peer sends FIN; macOS drops the peer
// association after FIN so getpeername() there returns ENOTCONN. This sticky flag lets connect(203) report
// EISCONN faithfully (LTP connect01). Cleared on close (fd_reset_emul) and at socket()/accept re-init.
static uint8_t g_sock_conn[HL_NFD];
static uint8_t g_sock_connecting[HL_NFD];
// fd -> a pending asynchronous socket error (Linux errno) to hand back on the next getsockopt(SO_ERROR),
// then clear (Linux delivers SO_ERROR once). A non-blocking stream connect() to a closed private-loopback
// port has no live INET peer to surface ECONNREFUSED through the AF_UNIX switch, so we stash it here and
// report EINPROGRESS, mirroring a real deferred TCP connect failure. Cleared at socket()/accept() re-init.
static int g_so_error[HL_NFD];
// fd -> 1 if the guest set SO_REUSEPORT on this socket. A private-loopback INET socket is backed by an
// AF_UNIX switch socket, and Linux AF_UNIX accepts setsockopt(SO_REUSEPORT) but always reads it back as 0
// (unlike SO_REUSEADDR), so the guest's get-after-set would wrongly report 0. Record the guest's intent
// here and report it on getsockopt. Cleared at socket()/accept() re-init.
static uint8_t g_so_reuseport[HL_NFD];
// fd -> shadowed IPPROTO_TCP integer options. A private-loopback/bridge guest INET stream socket is backed
// on the host by an AF_UNIX switch socket (see lo_swap), which rejects every setsockopt/getsockopt at
// IPPROTO_TCP with ENOPROTOOPT. Linux round-trips these options on a real TCP socket, and applications
// routinely set TCP_NODELAY *after* connect(), so a get-after-set (or a plain set) must not fail across the
// switch. Record the guest's value here and report it back, matching native. Slots: 0 NODELAY, 1 CORK,
// 2 KEEPIDLE, 3 KEEPINTVL, 4 KEEPCNT, 5 QUICKACK, 6 MAXSEG. Cleared at socket()/accept() re-init.
#define TCP_SHADOW_N 7
static int g_tcp_optval[HL_NFD][TCP_SHADOW_N];
static uint8_t g_tcp_optset[HL_NFD][TCP_SHADOW_N];

// Map a Linux IPPROTO_TCP integer optname to a shadow slot, or -1 if it is not a virtualized round-trip
// option (e.g. TCP_INFO, which is a struct handled separately). MAXSEG is get-mostly but Linux lets a guest
// lower the clamp, so it round-trips through a slot too.
static int tcp_shadow_slot(int optname) {
    switch (optname) {
    case 1: return 0;  // TCP_NODELAY
    case 3: return 1;  // TCP_CORK
    case 4: return 2;  // TCP_KEEPIDLE
    case 5: return 3;  // TCP_KEEPINTVL
    case 6: return 4;  // TCP_KEEPCNT
    case 12: return 5; // TCP_QUICKACK
    case 2: return 6;  // TCP_MAXSEG
    default: return -1;
    }
}

// A get on a MAXSEG slot never set by the guest reports a plausible loopback MSS so diagnostic code that
// requires a nonzero segment size keeps working over the switch (the exact value is host-variable on native
// and therefore not a stable fact); every other slot defaults to 0, the Linux default for the booleans.
static int tcp_shadow_default(int slot) {
    return slot == 6 ? 65483 : 0;
}

// Drop any shadowed TCP options for a reused fd number (socket()/accept()/close re-init).
static void tcp_shadow_clear(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    for (int i = 0; i < TCP_SHADOW_N; i++)
        g_tcp_optset[fd][i] = 0;
}

// fd -> shadowed IPPROTO_IP(level 0) / IPPROTO_IPV6(level 41) integer options. Same class as the TCP shadow
// above: once a private-loopback/bridge guest INET socket is bound/connected, its host backing becomes an
// AF_UNIX switch socket (see lo_swap), which rejects every setsockopt/getsockopt at IPPROTO_IP/IPPROTO_IPV6
// with ENOPROTOOPT. Native Linux round-trips these on a real IP socket -- DNS servers set IP_PKTINFO/
// IP_RECVTTL to reply from the right address, QUIC/HTTP3 and dual-stack servers set IP_TOS/IPV6_TCLASS and
// read IPV6_V6ONLY back, and code sets them *after* bind/connect -- so a get-after-set (or plain set) must
// survive the switch. Only options native actually accepts on a connected/bound unicast stream socket are
// shadowed here; options native itself rejects on such a socket (IP_HDRINCL raw-only -> ENOPROTOOPT,
// IP_MULTICAST_HOPS/IPV6_MULTICAST_HOPS on a unicast socket -> ENOPROTOOPT, IP_TRANSPARENT unprivileged ->
// EPERM) are deliberately left OUT so they fall through to the real setsockopt and surface the true errno.
// Slots 0-7 are IPPROTO_IP, 8-13 IPPROTO_IPV6. Cleared at socket()/accept()/close re-init.
#define IPOPT_SHADOW_N 14
static int g_ipopt_val[HL_NFD][IPOPT_SHADOW_N];
static uint8_t g_ipopt_set[HL_NFD][IPOPT_SHADOW_N];

// Map a Linux IPPROTO_IP integer optname to a shadow slot, or -1 if it is not a virtualized round-trip
// option at this level (unknown, struct-valued, or one native rejects on a unicast stream socket).
static int ip_shadow_slot(int optname) {
    switch (optname) {
    case 1: return 0;  // IP_TOS
    case 2: return 1;  // IP_TTL
    case 8: return 2;  // IP_PKTINFO
    case 10: return 3; // IP_MTU_DISCOVER
    case 11: return 4; // IP_RECVERR
    case 12: return 5; // IP_RECVTTL
    case 13: return 6; // IP_RECVTOS
    case 15: return 7; // IP_FREEBIND
    default: return -1;
    }
}

// Map a Linux IPPROTO_IPV6 integer optname to a shadow slot, or -1. IPV6_V6ONLY(26) uses slot 13 but its
// setsockopt is handled specially (native rejects a change after bind with EINVAL), so it is excluded here
// and matched directly on the optname in the setsockopt/getsockopt paths.
static int ip6_shadow_slot(int optname) {
    switch (optname) {
    case 67: return 8;  // IPV6_TCLASS
    case 16: return 9;  // IPV6_UNICAST_HOPS
    case 49: return 10; // IPV6_RECVPKTINFO
    case 51: return 11; // IPV6_RECVHOPLIMIT
    case 66: return 12; // IPV6_RECVTCLASS
    default: return -1;
    }
}

#define IPOPT_V6ONLY_SLOT 13

// A get on a slot the guest never set reports the Linux default so code that reads an option it did not set
// still sees a plausible value over the switch instead of ENOPROTOOPT (only reached when the real host
// getsockopt is also rejected, i.e. the AF_UNIX switch backing). IP_TTL and IPV6_UNICAST_HOPS default to 64;
// the boolean recv-flags and TOS/TCLASS default to 0.
static int ipopt_shadow_default(int slot) {
    switch (slot) {
    case 1: return 64; // IP_TTL
    case 9: return 64; // IPV6_UNICAST_HOPS
    default: return 0;
    }
}

// Drop any shadowed IP/IPV6 options for a reused fd number (socket()/accept()/close re-init).
static void ipopt_shadow_clear(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    for (int i = 0; i < IPOPT_SHADOW_N; i++)
        g_ipopt_set[fd][i] = 0;
}

// fd -> 1 if created AF_INET SOCK_DGRAM (only those get the published-UDP switch redirect, below)
static uint8_t g_sock_dgram[HL_NFD];
static uint16_t g_udp_local_port[HL_NFD], g_udp_peer_port[HL_NFD];
static uint32_t g_udp_local_ip[HL_NFD], g_udp_peer_ip[HL_NFD];
static uint8_t g_udp_local_interface[HL_NFD], g_udp_peer_interface[HL_NFD];
static uint8_t g_udp_local_v6[HL_NFD], g_udp_peer_v6[HL_NFD];

enum { HL_NETIF_MAX = 8 };

#define UDP_REF_N 4096

struct udp_ref {
    volatile uint32_t used, refs;
    char path[200];
    uint8_t alias_count;
    char aliases[HL_NETIF_MAX - 1][200];
};
static struct udp_ref *g_udp_refs;
static uint16_t g_udp_ref[HL_NFD];
// fd -> the socket's guest (Linux) address family, recorded at socket()/accept() so connect(203)/bind(200)
// can validate the guest sockaddr's sa_family against it (EAFNOSUPPORT) without a getsockname() probe --
// which is unreliable after a prior failed connect on the same fd. 0 = untracked (best-effort fallback).
static uint16_t g_sock_fam[HL_NFD];
// fd -> 1 if this DGRAM socket emulates a connection-oriented endpoint that owes its peer an EOF on
// close (a SEQPACKET socketpair or an O_DIRECT "packet" pipe). macOS DGRAM sockets never deliver EOF on
// peer close, so close() injects a zero-length EOF datagram and recv/read coerce ECONNRESET -> 0 for these.
static uint8_t g_sock_seqpacket[HL_NFD];

// macOS backs Linux SOCK_SEQPACKET (and O_DIRECT packet pipes) with AF_UNIX DGRAM. DGRAM preserves
// records but has no connection lifetime: when the last copy of one endpoint closes, its peer receives
// neither EOF nor a wakeup. Keep endpoint ownership in shared memory so fork/dup/close/exec reproduce the
// Linux last-open-file-description rule. The arena is inherited by every guest descendant; dead pairs are
// recycled only after both endpoint reference counts reach zero.
static void seq_ref_arena_init(const hl_host_services *host) {
    void *arena = NULL;
    if (g_seq_refs != NULL && g_udp_refs != NULL && g_sock_states != NULL) return;
    if (g_seq_refs == NULL && hl_linux_shared_create(host, sizeof(struct seq_ref) * SEQ_REF_N, &arena) == HL_STATUS_OK)
        g_seq_refs = (struct seq_ref *)arena;
    arena = NULL;
    if (g_udp_refs == NULL && hl_linux_shared_create(host, sizeof(struct udp_ref) * UDP_REF_N, &arena) == HL_STATUS_OK)
        g_udp_refs = (struct udp_ref *)arena;
    arena = NULL;
    if (g_sock_states == NULL &&
        hl_linux_shared_create(host, sizeof(struct sock_state) * SOCK_STATE_N, &arena) == HL_STATUS_OK)
        g_sock_states = (struct sock_state *)arena;
}

static int udp_ref_create(int fd, const char *path) {
    if (!g_udp_refs || fd < 0 || fd >= HL_NFD) return 0;
    for (uint32_t i = 0; i < UDP_REF_N; i++) {
        if (!__sync_bool_compare_and_swap(&g_udp_refs[i].used, 0, 1)) continue;
        snprintf(g_udp_refs[i].path, sizeof g_udp_refs[i].path, "%s", path);
        g_udp_refs[i].alias_count = 0;
        __atomic_store_n(&g_udp_refs[i].refs, 1, __ATOMIC_RELEASE);
        g_udp_ref[fd] = (uint16_t)(i + 1);
        return 0;
    }
    errno = ENFILE;
    return -1;
}

static int udp_ref_add_alias(int fd, const char *path) {
    if (!g_udp_refs || fd < 0 || fd >= HL_NFD || !g_udp_ref[fd]) {
        errno = EINVAL;
        return -1;
    }
    struct udp_ref *ref = &g_udp_refs[g_udp_ref[fd] - 1];
    if (ref->alias_count >= sizeof ref->aliases / sizeof ref->aliases[0]) {
        errno = ENOSPC;
        return -1;
    }
    snprintf(ref->aliases[ref->alias_count], sizeof ref->aliases[ref->alias_count], "%s", path);
    ref->alias_count++;
    return 0;
}

static void udp_ref_dup(int dst, int src) {
    if (!g_udp_refs || src < 0 || src >= HL_NFD || dst < 0 || dst >= HL_NFD || !g_udp_ref[src]) return;
    uint32_t slot = g_udp_ref[src] - 1;
    __atomic_add_fetch(&g_udp_refs[slot].refs, 1, __ATOMIC_ACQ_REL);
    g_udp_ref[dst] = g_udp_ref[src];
}

static void udp_ref_drop(int fd) {
    if (!g_udp_refs || fd < 0 || fd >= HL_NFD || !g_udp_ref[fd]) return;
    uint32_t slot = g_udp_ref[fd] - 1;
    g_udp_ref[fd] = 0;
    if (__atomic_sub_fetch(&g_udp_refs[slot].refs, 1, __ATOMIC_ACQ_REL) == 0) {
        unlink(g_udp_refs[slot].path);
        for (uint8_t i = 0; i < g_udp_refs[slot].alias_count; i++) {
            unlink(g_udp_refs[slot].aliases[i]);
            g_udp_refs[slot].aliases[i][0] = 0;
        }
        g_udp_refs[slot].alias_count = 0;
        g_udp_refs[slot].path[0] = 0;
        __atomic_store_n(&g_udp_refs[slot].used, 0, __ATOMIC_RELEASE);
    }
}

enum {
    SOCK_SHUTDOWN_READ = 1u,
    SOCK_SHUTDOWN_WRITE = 2u,
};

static int sock_state_create(int fd) {
    if (fd < 0 || fd >= HL_NFD || g_sock_states == NULL) return -1;
    for (uint32_t i = 0; i < SOCK_STATE_N; i++) {
        if (!__sync_bool_compare_and_swap(&g_sock_states[i].used, 0, 1)) continue;
        __atomic_store_n(&g_sock_states[i].shutdown, 0, __ATOMIC_RELAXED);
        __atomic_store_n(&g_sock_states[i].refs, 1, __ATOMIC_RELEASE);
        g_sock_state_ref[fd] = (uint16_t)(i + 1);
        return 0;
    }
    errno = ENFILE;
    return -1;
}

static void sock_state_drop(int fd);

static void sock_state_dup(int dst, int src) {
    if (g_sock_states == NULL || src < 0 || src >= HL_NFD || dst < 0 || dst >= HL_NFD ||
        g_sock_state_ref[src] == 0)
        return;
    if (dst == src) return;
    sock_state_drop(dst);
    uint32_t slot = g_sock_state_ref[src] - 1;
    __atomic_add_fetch(&g_sock_states[slot].refs, 1, __ATOMIC_ACQ_REL);
    g_sock_state_ref[dst] = g_sock_state_ref[src];
}

static void sock_state_drop(int fd) {
    if (g_sock_states == NULL || fd < 0 || fd >= HL_NFD || g_sock_state_ref[fd] == 0) return;
    uint32_t slot = g_sock_state_ref[fd] - 1;
    g_sock_state_ref[fd] = 0;
    if (__atomic_sub_fetch(&g_sock_states[slot].refs, 1, __ATOMIC_ACQ_REL) == 0) {
        __atomic_store_n(&g_sock_states[slot].shutdown, 0, __ATOMIC_RELAXED);
        __atomic_store_n(&g_sock_states[slot].used, 0, __ATOMIC_RELEASE);
    }
}

static void sock_state_shutdown_observed(int fd, int direction) {
    if (g_sock_states == NULL || fd < 0 || fd >= HL_NFD || g_sock_state_ref[fd] == 0) return;
    uint32_t bits = 0;
    if (direction == SHUT_RD || direction == SHUT_RDWR) bits |= SOCK_SHUTDOWN_READ;
    if (direction == SHUT_WR || direction == SHUT_RDWR) bits |= SOCK_SHUTDOWN_WRITE;
    if (bits != 0)
        __atomic_or_fetch(&g_sock_states[g_sock_state_ref[fd] - 1].shutdown, bits, __ATOMIC_ACQ_REL);
}

static uint32_t sock_state_shutdown(int fd) {
    if (g_sock_states == NULL || fd < 0 || fd >= HL_NFD || g_sock_state_ref[fd] == 0) return 0;
    return __atomic_load_n(&g_sock_states[g_sock_state_ref[fd] - 1].shutdown, __ATOMIC_ACQUIRE);
}

static void sock_state_fork_prepare(void) {
    if (g_sock_states == NULL) return;
    for (int fd = 0; fd < HL_NFD; fd++)
        if (g_sock_state_ref[fd] != 0)
            __atomic_add_fetch(&g_sock_states[g_sock_state_ref[fd] - 1].refs, 1, __ATOMIC_ACQ_REL);
}

static void sock_state_fork_cancel(void) {
    if (g_sock_states == NULL) return;
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (g_sock_state_ref[fd] == 0) continue;
        uint32_t slot = g_sock_state_ref[fd] - 1;
        if (__atomic_sub_fetch(&g_sock_states[slot].refs, 1, __ATOMIC_ACQ_REL) == 0) {
            __atomic_store_n(&g_sock_states[slot].shutdown, 0, __ATOMIC_RELAXED);
            __atomic_store_n(&g_sock_states[slot].used, 0, __ATOMIC_RELEASE);
        }
    }
}

// exit_group(2) tears the whole process down with a raw _exit() that never runs fd_reset_emul, so any
// rendezvous inode still owned by an un-closed descriptor (e.g. a fork()ed client child that inherits the
// server's listening fd and _exit()s without closing it) would keep its refcount pinned above zero and
// orphan the AF_UNIX inode on the fs. Drop this process's remaining udp/bridge refs at exit so the LAST
// reference across the whole process tree unlinks the inode -- mirroring the kernel freeing an INET port
// when its last owning process dies. Per-fd and idempotent: fds already closed carry no ref.
static void socket_ref_process_exit(void) {
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (g_udp_ref[fd]) udp_ref_drop(fd);
        if (g_sock_state_ref[fd]) sock_state_drop(fd);
    }
}

static int seq_ref_pair(int first, int second) {
    if (first < 0 || first >= HL_NFD || second < 0 || second >= HL_NFD || g_seq_refs == NULL) return -1;
    for (uint32_t i = 0; i < SEQ_REF_N; i++) {
        if (!__sync_bool_compare_and_swap(&g_seq_refs[i].used, 0, 1)) continue;
        __atomic_store_n(&g_seq_refs[i].refs[0], 1, __ATOMIC_RELAXED);
        __atomic_store_n(&g_seq_refs[i].refs[1], 1, __ATOMIC_RELAXED);
        __atomic_store_n(&g_seq_refs[i].pending[0], 0, __ATOMIC_RELAXED);
        __atomic_store_n(&g_seq_refs[i].pending[1], 0, __ATOMIC_RELAXED);
        g_seq_ref[first] = (uint16_t)(i + 1);
        g_seq_end[first] = 0;
        g_seq_ref[second] = (uint16_t)(i + 1);
        g_seq_end[second] = 1;
        return 0;
    }
    errno = ENFILE;
    return -1;
}

static void seq_ref_dup(int dst, int src) {
    if (!g_seq_refs || src < 0 || src >= HL_NFD || dst < 0 || dst >= HL_NFD || !g_seq_ref[src]) return;
    uint32_t slot = g_seq_ref[src] - 1;
    uint32_t end = g_seq_end[src];
    __atomic_add_fetch(&g_seq_refs[slot].refs[end], 1, __ATOMIC_ACQ_REL);
    g_seq_ref[dst] = g_seq_ref[src];
    g_seq_end[dst] = (uint8_t)end;
}

static void seq_ref_drop(int fd) {
    if (!g_seq_refs || fd < 0 || fd >= HL_NFD || !g_seq_ref[fd]) return;
    uint32_t slot = g_seq_ref[fd] - 1;
    uint32_t end = g_seq_end[fd];
    g_seq_ref[fd] = 0;
    g_seq_end[fd] = 0;
    if (__atomic_sub_fetch(&g_seq_refs[slot].refs[end], 1, __ATOMIC_ACQ_REL) == 0) (void)send(fd, "", 0, MSG_DONTWAIT);
    if (__atomic_load_n(&g_seq_refs[slot].refs[0], __ATOMIC_ACQUIRE) == 0 &&
        __atomic_load_n(&g_seq_refs[slot].refs[1], __ATOMIC_ACQUIRE) == 0)
        __atomic_store_n(&g_seq_refs[slot].used, 0, __ATOMIC_RELEASE);
}

// Reserve the references the child will inherit before fork. A failed fork rolls them back; a successful
// fork consumes the reservation, requiring no allocation or lock in the post-fork child.
static void seq_ref_fork_prepare(void) {
    if (g_seq_refs != NULL)
        for (int fd = 0; fd < HL_NFD; fd++) {
            if (!g_seq_ref[fd]) continue;
            uint32_t slot = g_seq_ref[fd] - 1;
            __atomic_add_fetch(&g_seq_refs[slot].refs[g_seq_end[fd]], 1, __ATOMIC_ACQ_REL);
        }
    if (g_udp_refs)
        for (int fd = 0; fd < HL_NFD; fd++)
            if (g_udp_ref[fd]) __atomic_add_fetch(&g_udp_refs[g_udp_ref[fd] - 1].refs, 1, __ATOMIC_ACQ_REL);
    sock_state_fork_prepare();
}

static void seq_ref_fork_cancel(void) {
    if (g_seq_refs)
        for (int fd = 0; fd < HL_NFD; fd++) {
            if (!g_seq_ref[fd]) continue;
            uint32_t slot = g_seq_ref[fd] - 1;
            __atomic_sub_fetch(&g_seq_refs[slot].refs[g_seq_end[fd]], 1, __ATOMIC_ACQ_REL);
        }
    if (g_udp_refs)
        for (int fd = 0; fd < HL_NFD; fd++)
            if (g_udp_ref[fd]) __atomic_sub_fetch(&g_udp_refs[g_udp_ref[fd] - 1].refs, 1, __ATOMIC_ACQ_REL);
    sock_state_fork_cancel();
}

// fd -> (its socketpair/O_DIRECT-pipe PARTNER fd + 1); 0 = no known partner. Recorded for both ends at
// socketpair(SEQPACKET)/pipe2(O_DIRECT) so close() can tell a genuine last-local close (inject the synthetic
// EOF) from a parent dropping the child's fork-inherited peer end while it still holds its OWN end (must NOT
// inject: Linux delivers no EOF while the child still references that end, and our zero-length datagram would
// otherwise land in our own retained end's queue and be misread as a premature EOF -- exactly what broke
// a multi-process SEQPACKET handshake). Carried on dup (fd_carry_sock); reset on close (fd_reset_emul).
static int g_sock_pair_peer[HL_NFD];
static uint64_t g_sock_object[HL_NFD];
static uint64_t g_sock_peer_object[HL_NFD];
/* The host pathname carrying a connection identity is engine-private.  These bits keep that transport
 * detail out of the guest's getsockname/getpeername results. */
static uint8_t g_sock_identity_local_hidden[HL_NFD];
static uint8_t g_sock_identity_peer_hidden[HL_NFD];
/* shutdown(2) changes the open socket description and therefore every dup and fork alias. Linux does not
 * expose this state through getsockopt, so the shared socket-state arena retains it at the syscall boundary.
 * Checkpoint capture rejects either direction until the journal can replay half-close ordering safely. */
/* Ordinary pathname/abstract AF_UNIX connections need a whole-process freeze before their reciprocal
 * topology can be captured. Keep them distinct from guest-created socketpairs and the private AF_UNIX
 * transports behind guest INET sockets; until that freeze is authoritative, capture must fail closed. */
static uint8_t g_sock_identity_checkpoint_pending[HL_NFD];
static _Atomic uint32_t g_sock_object_next = 1;

static uint64_t sock_object_new(void) {
    uint32_t sequence = atomic_fetch_add_explicit(&g_sock_object_next, 1u, memory_order_relaxed);
    if (sequence == 0) sequence = atomic_fetch_add_explicit(&g_sock_object_next, 1u, memory_order_relaxed);
    return ((uint64_t)(uint32_t)getpid() << 32) | sequence;
}

static void sock_internal_shutdown_fresh(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    sock_state_drop(fd);
    g_sock_identity_checkpoint_pending[fd] = (uint8_t)(sock_state_create(fd) != 0);
}

static void sock_pair_identity_assign(int first, int second) {
    if (first < 0 || first >= HL_NFD || second < 0 || second >= HL_NFD) return;
    sock_internal_shutdown_fresh(first);
    sock_internal_shutdown_fresh(second);
    uint64_t first_object = sock_object_new();
    uint64_t second_object = sock_object_new();
    g_sock_object[first] = first_object;
    g_sock_peer_object[first] = second_object;
    g_sock_object[second] = second_object;
    g_sock_peer_object[second] = first_object;
}

static int sock_internal_alias_matches(int fd, int candidate, uint64_t ofd) {
    return candidate == fd || (ofd != 0 && g_ofd_id[candidate] == ofd);
}

/* Encode reciprocal object identities in an engine-private client AF_UNIX bind name, so accept can recover
 * them with getpeername() without placing engine-private bytes in the guest stream. */
static void sock_internal_alias_relation(int fd, uint64_t peer, int local_hidden, int checkpoint_pending) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_object[fd]) return;
    uint64_t ofd = g_ofd_id[fd];
    int first = ofd ? 0 : fd, end = ofd ? HL_NFD : fd + 1;
    for (int alias = first; alias < end; alias++) {
        if (!sock_internal_alias_matches(fd, alias, ofd)) continue;
        g_sock_peer_object[alias] = peer;
        if (local_hidden) g_sock_identity_local_hidden[alias] = 1;
        if (checkpoint_pending) g_sock_identity_checkpoint_pending[alias] = 1;
    }
}

static void sock_internal_async_error_store(int fd, int error) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_object[fd]) return;
    uint64_t ofd = g_ofd_id[fd];
    int first = ofd ? 0 : fd, end = ofd ? HL_NFD : fd + 1;
    for (int alias = first; alias < end; alias++)
        if (sock_internal_alias_matches(fd, alias, ofd)) g_so_error[alias] = error;
}

static int sock_internal_async_error_take(int fd) {
    if (fd < 0 || fd >= HL_NFD) return 0;
    int error = g_so_error[fd];
    uint64_t ofd = g_ofd_id[fd];
    if (!ofd) {
        g_so_error[fd] = 0;
        return error;
    }
    for (int alias = 0; alias < HL_NFD; alias++)
        if (sock_internal_alias_matches(fd, alias, ofd)) g_so_error[alias] = 0;
    return error;
}

static void sock_internal_unix_peer_note(int fd, const char *name, int native_peer) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_object[fd]) return;
    uint64_t ofd = g_ofd_id[fd];
    int first = ofd ? 0 : fd, end = ofd ? HL_NFD : fd + 1;
    for (int alias = first; alias < end; alias++) {
        if (!sock_internal_alias_matches(fd, alias, ofd)) continue;
        unix_peer_note(alias, name);
        g_sock_native_peer[alias] = (uint8_t)native_peer;
    }
}

static void sock_internal_connect_observed(int fd, int error) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_object[fd]) return;
    uint64_t ofd = g_ofd_id[fd];
    int first = ofd ? 0 : fd, end = ofd ? HL_NFD : fd + 1;
    for (int alias = first; alias < end; alias++) {
        if (!sock_internal_alias_matches(fd, alias, ofd)) continue;
        if (error == EINPROGRESS) {
            g_sock_conn[alias] = 0;
            g_sock_connecting[alias] = 1;
        } else if (error == 0) {
            g_sock_conn[alias] = 1;
            g_sock_connecting[alias] = 0;
        } else {
            g_sock_conn[alias] = 0;
            g_sock_connecting[alias] = 0;
            if (g_sock_identity_local_hidden[alias]) g_sock_peer_object[alias] = 0;
        }
    }
}

static void sock_internal_shutdown_observed(int fd, int direction) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_object[fd]) return;
    sock_state_shutdown_observed(fd, direction);
}

static int sock_internal_shutdown(int fd, int direction) {
    if (shutdown(fd, direction) != 0) return -1;
    sock_internal_shutdown_observed(fd, direction);
    return 0;
}

static int sock_internal_checkpoint_admit(int fd) {
    if (fd < 0 || fd >= HL_NFD) {
        errno = EBADF;
        return -1;
    }
    if (g_sock_identity_checkpoint_pending[fd]) {
        fprintf(stderr, "[ckpt] admit-arm: fd %d identity_checkpoint_pending\n", fd);
        errno = ENOTSUP;
        return -1;
    }
    if (g_sock_connecting[fd]) {
        fprintf(stderr, "[ckpt] admit-arm: fd %d connecting\n", fd);
        errno = EBUSY;
        return -1;
    }
    if (sock_state_shutdown(fd) != 0) {
        fprintf(stderr, "[ckpt] admit-arm: fd %d shutdown mask=%u\n", fd, sock_state_shutdown(fd));
        errno = ENOTSUP;
        return -1;
    }
    return 0;
}

static int sock_internal_connect_prepare(int fd, int checkpoint_pending) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_object[fd]) {
        errno = EINVAL;
        return -1;
    }
    struct sockaddr_un local = {0};
    socklen_t local_length = sizeof local;
    if (getsockname(fd, (struct sockaddr *)&local, &local_length) != 0) return -1;
    size_t path_offset = offsetof(struct sockaddr_un, sun_path);
    int named = local.sun_path[0] != '\0' || local_length > (socklen_t)(path_offset + 1u);
    if (local.sun_family == AF_UNIX && local_length > (socklen_t)path_offset && named) {
        uint64_t client = 0, server = 0;
        local.sun_path[sizeof local.sun_path - 1] = '\0';
        if (g_sock_identity_local_hidden[fd] &&
            hl_socket_identity_parse(local.sun_path, &client, &server) == 0 && client == g_sock_object[fd]) {
            sock_internal_alias_relation(fd, server, 1, checkpoint_pending);
        }
        /* A guest-bound client must keep its address and still be allowed to connect.  It cannot carry the
         * private identity name, so leave peer identity absent and let checkpoint admission reject it. */
        if (checkpoint_pending) sock_internal_alias_relation(fd, g_sock_peer_object[fd], 0, 1);
        return 0;
    }
    // A failed AF_UNIX connect poisons its socket and the retry path replaces it with lo_swap(). Re-bind
    // every replacement, while retaining the same logical peer object across attempts.
    uint64_t peer = g_sock_peer_object[fd] ? g_sock_peer_object[fd] : sock_object_new();
    char path[HL_SOCKET_IDENTITY_PATH_SIZE];
    if (hl_socket_identity_format(path, sizeof path, g_sock_object[fd], peer) != 0) {
        errno = EINVAL;
        return -1;
    }
    struct sockaddr_un address;
    if (unix_addr_set(&address, path) < 0) return -1;
    unlink(path);
    if (bind(fd, (struct sockaddr *)&address, sizeof address) != 0) return -1;
    unlink(path);
    sock_internal_alias_relation(fd, peer, 1, checkpoint_pending);
    return 0;
}

static void sock_internal_connect_failed(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    /* A reserved peer becomes authoritative only when connect succeeds (or remains in progress).  The
     * private local bind cannot be undone without replacing the descriptor, but clearing the relation makes
     * checkpoint capture fail closed instead of inventing a connected peer after a refused dial. */
    sock_internal_connect_observed(fd, ECONNREFUSED);
}

static int sock_internal_accept_identify(int fd, int checkpoint_pending) {
    if (fd >= 0 && fd < HL_NFD && checkpoint_pending) g_sock_identity_checkpoint_pending[fd] = 1;
    struct sockaddr_un peer = {0};
    socklen_t length = sizeof peer;
    if (getpeername(fd, (struct sockaddr *)&peer, &length) != 0) return -1;
    peer.sun_path[sizeof peer.sun_path - 1] = '\0';
    if (peer.sun_family != AF_UNIX ||
        strncmp(peer.sun_path, HL_SOCKET_IDENTITY_PREFIX, sizeof(HL_SOCKET_IDENTITY_PREFIX) - 1) != 0)
        return 0;
    uint64_t client = 0, server = 0;
    if (hl_socket_identity_parse(peer.sun_path, &client, &server) != 0) {
        errno = EPROTO;
        return -1;
    }
    g_sock_object[fd] = server;
    g_sock_peer_object[fd] = client;
    g_sock_identity_peer_hidden[fd] = 1;
    g_sock_identity_checkpoint_pending[fd] = (uint8_t)checkpoint_pending;
    return 1;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static void sock_internal_identity_test_initialize(int fd, uint64_t object, uint64_t ofd) {
    if (g_sock_states == NULL) {
        void *arena = mmap(NULL, sizeof(struct sock_state) * SOCK_STATE_N, PROT_READ | PROT_WRITE,
                           MAP_ANON | MAP_SHARED, -1, 0);
        if (arena != MAP_FAILED) g_sock_states = (struct sock_state *)arena;
    }
    g_sock_object[fd] = object;
    g_sock_peer_object[fd] = 0;
    g_sock_identity_local_hidden[fd] = 0;
    g_sock_identity_peer_hidden[fd] = 0;
    g_sock_identity_checkpoint_pending[fd] = 0;
    g_sock_connecting[fd] = 0;
    g_sock_conn[fd] = 0;
    g_so_error[fd] = 0;
    sock_state_drop(fd);
    if (object == 0) {
        g_ofd_id[fd] = 0;
        g_sock_fam[fd] = 0;
        g_sock_stream[fd] = 0;
        return;
    }
    int source = -1;
    if (ofd != 0)
        for (int candidate = 0; candidate < HL_NFD; candidate++)
            if (candidate != fd && g_ofd_id[candidate] == ofd && g_sock_state_ref[candidate] != 0) {
                source = candidate;
                break;
            }
    g_ofd_id[fd] = ofd;
    if (source >= 0)
        sock_state_dup(fd, source);
    else if (sock_state_create(fd) != 0)
        g_sock_identity_checkpoint_pending[fd] = 1;
    g_sock_fam[fd] = object ? AF_UNIX : 0;
    g_sock_stream[fd] = object != 0;
}

HL_API int HL_TARGET_LOCAL(unix_identity_test)(uint32_t operation, int fd, uint64_t object, uint64_t *local,
                                               uint64_t *peer, uint32_t *hidden) {
    if (fd < 0 || fd >= HL_NFD || local == NULL || peer == NULL || hidden == NULL) return EINVAL;
    int status = 0;
    switch (operation) {
    case 0:
        sock_internal_identity_test_initialize(fd, object, g_ofd_id[fd]);
        status = sock_internal_connect_prepare(fd, 1);
        break;
    case 1:
        sock_internal_identity_test_initialize(fd, object, g_ofd_id[fd]);
        status = sock_internal_accept_identify(fd, 1);
        break;
    case 2: sock_internal_connect_failed(fd); break;
    case 3: sock_internal_identity_test_initialize(fd, 0, 0); break;
    case 4:
        sock_internal_identity_test_initialize(fd, object, 0);
        status = sock_internal_connect_prepare(fd, 0);
        break;
    case 5: sock_internal_connect_observed(fd, EINPROGRESS); break;
    case 6: sock_internal_connect_observed(fd, ECONNREFUSED); break;
    case 7: sock_internal_connect_observed(fd, 0); break;
    case 8: status = sock_internal_checkpoint_admit(fd); break;
    case 9: sock_internal_identity_test_initialize(fd, object, object); break;
    case 10: break;
    case 11:
        sock_internal_identity_test_initialize(fd, object, 0);
        g_sock_peer_object[fd] = object + 1;
        break;
    case 12: status = sock_internal_shutdown(fd, SHUT_RD); break;
    case 13: status = sock_internal_shutdown(fd, SHUT_WR); break;
    case 14: status = sock_internal_shutdown(fd, SHUT_RDWR); break;
    default: return EINVAL;
    }
    if (status < 0) return errno != 0 ? errno : EIO;
    *local = g_sock_object[fd];
    *peer = g_sock_peer_object[fd];
    uint32_t shutdown_state = sock_state_shutdown(fd);
    *hidden = (uint32_t)g_sock_identity_local_hidden[fd] | ((uint32_t)g_sock_identity_peer_hidden[fd] << 1) |
              ((uint32_t)g_sock_identity_checkpoint_pending[fd] << 2) | ((uint32_t)g_sock_connecting[fd] << 3) |
              ((uint32_t)g_sock_conn[fd] << 4) |
              ((shutdown_state & SOCK_SHUTDOWN_READ) != 0 ? UINT32_C(1) << 5 : 0) |
              ((shutdown_state & SOCK_SHUTDOWN_WRITE) != 0 ? UINT32_C(1) << 6 : 0);
    return status;
}
#endif

// fd -> a DISTINCT synthetic peer pid stamped on both ends at socketpair() creation (0 = none). macOS
// captures LOCAL_PEERPID at socketpair-creation time and reports the CREATOR's pid on BOTH ends, never
// updating it on fork -- so the process that CREATED the pair (typically container init, guest
// pid 1) reads its OWN pid as the peer credential for every forked child, which the
// SCM_CREDENTIALS/SO_PEERCRED self-fallback then collapsed to container_pid() == guest 1 for ALL of them.
// IPC uses that peer pid as a node identity, so every child collided on node "1" and the
// node-merge never finalized (transport up, but OnChannelConnected never fired -> the child's IO thread
// blocked in recvmsg until its connection watchdog killed it).
// Stamping each end with a unique id (>= 1<<30, above Linux PID_MAX ~4M so it never aliases a real guest
// pid the protocol also tracks) gives every child a distinct, non-self node identity whenever LOCAL_PEERPID
// degenerates to self. Carried on dup (fd_carry_sock); reset on close (fd_reset_emul).
static int g_sock_peer_pid[HL_NFD];

static int sock_alloc_synth_peer(void) {
    static int ctr = 0x40000000; // 1<<30
    int v = __atomic_add_fetch(&ctr, 1, __ATOMIC_RELAXED);
    if (v >= 0x7fff0000 || v < 0x40000000) { // never wrap into a real-pid / negative range
        __atomic_store_n(&ctr, 0x40000000, __ATOMIC_RELAXED);
        v = 0x40000000;
    }
    return v;
}

static int seq_is(int fd) {
    return fd >= 0 && fd < HL_NFD && g_sock_seqpacket[fd];
}

// fd -> 1 if the guest enabled SO_PASSCRED on this AF_UNIX socket. macOS has no SO_PASSCRED/SCM_CREDENTIALS,
// so we record the request and synthesize the peer-credentials ancillary record on each recvmsg (below).
// Credential-aware IPC sets SO_PASSCRED and requires an SCM_CREDENTIALS cmsg on the bootstrap message
// -- without it the receiver logs "missing credentials" and aborts. Carried on dup, reset on close.
static uint8_t g_sock_passcred[HL_NFD];

static void cmsg_note_recv_sock_fd(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISSOCK(st.st_mode)) return;

    int ty = 0;
    socklen_t tyl = sizeof ty;
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &ty, &tyl) != 0) ty = 0;

    struct sockaddr_storage ss;
    socklen_t sl = sizeof ss;
    memset(&ss, 0, sizeof ss);
    if (getsockname(fd, (struct sockaddr *)&ss, &sl) != 0 || ss.ss_family != AF_UNIX) return;

    g_sock_fam[fd] = AF_UNIX;
    sock_state_drop(fd);
    /* SCM_RIGHTS currently carries OFD identity but not the authenticated socket endpoint graph.  Refuse
     * checkpointing this descriptor until that graph proves its peer and cross-process shutdown state. */
    g_sock_identity_checkpoint_pending[fd] = 1;
    g_sock_stream[fd] = (ty == SOCK_STREAM);
    g_sock_dgram[fd] = (ty == SOCK_DGRAM);
    if (!g_sock_peer_pid[fd]) g_sock_peer_pid[fd] = sock_alloc_synth_peer();
    g_sock_passcred[fd] = 1;
}

// fd -> guest-requested TCP bind port (host order), 0 = none. Set at bind(200) for AF_INET/INET6 stream
// sockets; consumed by the /proc/net/tcp[6] synth to surface a LISTEN row (see netns_tcp_* below).
static uint16_t g_tcp_lport[HL_NFD];
static uint32_t g_tcp_laddr[HL_NFD];     // fd -> raw __be32 v4 bind addr (0.0.0.0 -> 0), printed %08X kernel-style
static uint8_t g_tcp_l6[HL_NFD];         // fd -> 1 if the bind was AF_INET6 (row goes in /proc/net/tcp6)
static uint8_t g_tcp_laddr6[HL_NFD][16]; // fd -> 16-byte v6 bind addr
static uint8_t g_tcp_listen[HL_NFD];     // fd -> 1 once listen(2) succeeded (row is emitted only then)
static int g_sock_backlog[HL_NFD];

static int lo_on(void) {
#if defined(_WIN32)
    /*
     * The private loopback namespace is off on this host, and the reason is
     * structural rather than a missing feature.
     *
     * lo_swap() implements it by MINTING A FRESH AF_UNIX SOCKET AND dup2()ING IT
     * OVER THE GUEST'S DESCRIPTOR, so that an INET socket the guest still holds
     * is quietly backed by a local one. That requires two things this host does
     * not have. First, a descriptor number here is the C library's, and the
     * socket behind it lives in a side table that a C library dup2() does not
     * move -- the swap would leave the guest's number pointing at the ORIGINAL
     * socket while the new one is bound, which is exactly the failure measured
     * before this guard existed: bind() answering EAFNOSUPPORT because an INET
     * socket was handed a local address. Second, the rendezvous is a filesystem
     * path under the guest's own /tmp, and a local socket here is bound with a
     * host path, so the name the switch agrees on does not exist to bind to.
     *
     * With it off, a guest's 127.0.0.1 reaches the real host loopback. What is
     * lost is ISOLATION between containers on one machine -- two guests would
     * see each other's localhost -- not any socket behaviour, which is why the
     * guard is here and not at the call sites: every one of them is correct
     * unconditionally once the answer is "no namespace".
     */
    return 0;
#else
    return g_netns[0] != 0;
#endif
}

static int lo_is(const uint8_t *sa, socklen_t l) {
    return sa && l >= 8 && *(uint16_t *)sa == AF_INET && sa[4] == 127;
    // 127.x.x.x
}

// ---- IPv6 loopback: same private-namespace redirect as 127/8, for AF_INET6 (::/::1). The guest passes a
// Linux sockaddr_in6 { u16 family(==10); u16 port@2; u32 flow@4; u8 addr[16]@8; u32 scope@24 }; the family
// VALUE is the Linux one (10), not macOS AF_INET6 (30). The 16-byte addr @8 is in6addr_loopback (::1, 15
// zero bytes + 0x01) or in6addr_any (::, all zero). Routing both to the per-container loopback dir keeps a
// dual-stack server's v6 bind isolated instead of escaping to the real host stack (and v6 has no bridge).
#define LX_AF_INET6_FAM 10

static int in6_all_zero(const uint8_t *a, int n) {
    for (int i = 0; i < n; i++)
        if (a[i]) return 0;
    return 1;
}

static int in6_is_loopback(const uint8_t *a) {
    return in6_all_zero(a, 15) && a[15] == 1;
}

static int in6_is_any(const uint8_t *a) {
    return in6_all_zero(a, 16);
}

// connect(dest): v6 loopback if AF_INET6 and dest is ::1 (mirrors lo_is: only the explicit loopback addr)
static int lo6_is(const uint8_t *sa, socklen_t l) {
    return sa && l >= 24 && *(const uint16_t *)sa == LX_AF_INET6_FAM && in6_is_loopback(sa + 8);
}

static int br_on(void); // defined below (per-network bridge on); used by the v6 bind classifiers here

// bind(addr): v6 loopback if AF_INET6 and addr is ::1, OR :: (unspecified) ONLY when the bridge is off.
// `::1` always stays on the private per-container loopback. `::` (dual-stack "any", as busybox nc / many
// servers bind when listening) is the v6 analogue of IPv4 0.0.0.0: with a user network attached it must
// defer to the bridge (br6_any_is below) so a peer container can reach it, instead of landing on the
// isolated loopback (where a cross-container connect would ENOENT). Mirrors lo_any_is's `&& !br_on()`.
static int lo6_any_is(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 24 || *(const uint16_t *)sa != LX_AF_INET6_FAM) return 0;
    if (in6_is_loopback(sa + 8)) return 1;
    return in6_is_any(sa + 8) && !br_on();
}

// bind(::): the IPv6 unspecified address, routed to the per-network bridge (== IPv4 0.0.0.0's br path) so
// a dual-stack listener that binds `::` is reachable by peer containers over the AF_UNIX switch. Only when
// a user network is attached (br_on()); with no bridge, `::` is handled by lo6_any_is (isolated loopback).
static int br6_any_is(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 24 || *(const uint16_t *)sa != LX_AF_INET6_FAM) return 0;
    return in6_is_any(sa + 8) && br_on();
}

static void lo_path(uint16_t port, char *out, size_t n) {
    snprintf(out, n, "%s/p%u", g_netns, (unsigned)port);
}

// A v6-only wildcard listener and an IPv4 wildcard listener may own the same
// numeric port at once. Keep the v6-only rendezvous distinct; dual-stack IPv6
// listeners retain the historical path so IPv4 and IPv6 clients share it.
static void lo_tcp_path(uint16_t port, int v6only, char *out, size_t n) {
    if (v6only)
        snprintf(out, n, "%s/p6-%u", g_netns, (unsigned)port);
    else
        lo_path(port, out, n);
}

// Allocate an ephemeral loopback port for a bind(127.0.0.1:0). The kernel would assign a real port;
// under the unix-socket emulation we instead pick a port whose `p<port>` path is still free so that a
// later getsockname()/connect() round-trips to the same socket. (Without this, port 0 collapsed to a
// fixed sentinel and the client connected to a path that was never bound -> ENOENT.)
static uint16_t lo_alloc_ephemeral(void) {
    static uint16_t next; // seeded once per process; the path-existence check guards collisions
    if (next < 1024) next = (uint16_t)(20000 + (getpid() & 0x3fff));
    for (int tries = 0; tries < 45000; tries++) {
        uint16_t cand = next++;
        if (next < 1024) next = 1024; // wrapped through 0
        if (cand < 1024) continue;    // stay out of the privileged range
        char path[200];
        lo_path(cand, path, sizeof path);
        if (access(path, F_OK) != 0) return cand; // unbound -> usable
    }
    return 0;
}

// Swap the AF_INET socket at `fd` for a fresh AF_UNIX SOCK_STREAM one (keeping the fd number + flags).
// SOL_SOCKET options a guest may set on its INET socket BEFORE the private-loopback swap replaces it with a
// fresh AF_UNIX socket. Each is generic to SOL_SOCKET (valid + readable on AF_UNIX), so carrying them over
// preserves both the option's effect (a receive timeout still fires, so a blocked recv wakes with EAGAIN
// instead of hanging) and its get-after-set readback (SO_REUSEADDR/SO_REUSEPORT report 1, not the fresh
// socket's 0). Options the guest sets AFTER the swap already land on the AF_UNIX fd directly.
static const int lo_carry_opts[] = {SO_REUSEADDR, SO_REUSEPORT, SO_RCVTIMEO,  SO_SNDTIMEO,
                                    SO_KEEPALIVE, SO_BROADCAST, SO_OOBINLINE, SO_LINGER};

static int stream_swap(int fd, int family) {
    int fl = fcntl(fd, F_GETFL), df = fcntl(fd, F_GETFD);
    // Snapshot the carried SOL_SOCKET options from the old (INET) fd before dup2 replaces it.
    unsigned char ov[sizeof lo_carry_opts / sizeof lo_carry_opts[0]][64];
    socklen_t ol[sizeof lo_carry_opts / sizeof lo_carry_opts[0]];
    for (unsigned i = 0; i < sizeof lo_carry_opts / sizeof lo_carry_opts[0]; i++) {
        ol[i] = sizeof ov[i];
        if (getsockopt(fd, SOL_SOCKET, lo_carry_opts[i], ov[i], &ol[i]) < 0) ol[i] = 0;
    }
    int u = socket(family, SOCK_STREAM, 0);
    if (u < 0) return -1;
    (void)hl_native_set_no_sigpipe(u);
    if (u != fd) {
        if (dup2(u, fd) < 0) {
            close(u);
            return -1;
        }
        close(u);
    }
    // keep non-blocking (async connect)
    if (fl >= 0 && (fl & O_NONBLOCK)) fcntl(fd, F_SETFL, O_NONBLOCK);
    if (df >= 0 && (df & FD_CLOEXEC)) fcntl(fd, F_SETFD, FD_CLOEXEC);
    // Re-apply the carried options to the fresh AF_UNIX socket (best-effort: a value the AF_UNIX socket
    // rejects is simply skipped, exactly as it would be ignored on the INET original).
    for (unsigned i = 0; i < sizeof lo_carry_opts / sizeof lo_carry_opts[0]; i++)
        if (ol[i]) setsockopt(fd, SOL_SOCKET, lo_carry_opts[i], ov[i], ol[i]);
    return 0;
}

static int lo_swap(int fd) {
    return stream_swap(fd, AF_UNIX);
}

// Restore the real INET transport after bind(0.0.0.0, ...) selected the virtual switch but connect()
// targets an external address. The guest family is retained separately in g_sock_fam even while the host
// descriptor is AF_UNIX, so IPv4 and IPv6 are reconstructed without inspecting the substituted socket.
static int inet_stream_swap(int fd) {
    if (fd < 0 || fd >= HL_NFD) {
        errno = EBADF;
        return -1;
    }
    int family;
    if (g_sock_fam[fd] == AF_INET)
        family = AF_INET;
    else if (g_sock_fam[fd] == LX_AF_INET6_FAM)
        family = AF_INET6;
    else {
        errno = EAFNOSUPPORT;
        return -1;
    }
    return stream_swap(fd, family);
}

// report AF_INET 127.0.0.1:port back to the guest
static void fill_inet_lo(uint8_t *sa, socklen_t *l, uint16_t port) {
    if (!sa) return;
    *(uint16_t *)(sa + 0) = AF_INET;
    *(uint16_t *)(sa + 2) = htons(port);
    *(uint32_t *)(sa + 4) = 0x0100007fu;
    // 127.0.0.1, zero-pad
    memset(sa + 8, 0, 8);
    if (l) *l = 16;
}

// report AF_INET6 ::1:port back to the guest (Linux sockaddr_in6 layout; family value 10). Mirrors
// fill_inet_lo: reports the loopback addr regardless of whether the socket bound :: or ::1 (apps key off
// the port; cf. the v4 path reporting 127.0.0.1 for a 0.0.0.0 bind).
static void fill_inet6_lo(uint8_t *sa, socklen_t *l, uint16_t port) {
    if (!sa) return;
    memset(sa, 0, 28);                       // family/port/flow/addr/scope
    *(uint16_t *)(sa + 0) = LX_AF_INET6_FAM; // 10
    *(uint16_t *)(sa + 2) = htons(port);     // port (BE) @2
    sa[8 + 15] = 1;                          // addr @8 = ::1 (in6addr_loopback)
    if (l) *l = 28;
}

/* Linux sockaddr_un begins with a two-byte family on every supported guest ISA.  Do not copy the host's
 * sockaddr_un prefix here: Darwin begins it with one-byte sun_len followed by one-byte sun_family. */
static void fill_unix_unnamed(uint8_t *sa, socklen_t *length) {
    if (!length) return;
    socklen_t capacity = *length;
    const uint8_t family[2] = {(uint8_t)(AF_UNIX & 0xff), (uint8_t)((AF_UNIX >> 8) & 0xff)};
    if (sa && capacity != 0) memcpy(sa, family, capacity < 2 ? (size_t)capacity : 2u);
    *length = 2;
}

// ---- NET bridge (2A "virtual switch"): per-USER-NETWORK rendezvous for container<->container traffic.
// Generalizes the loopback redirect from "127/8 -> per-container dir" to "this user network's subnet ->
// SHARED per-network dir". A guest TCP socket whose peer is ANOTHER container's IP on the same user
// network (same /16 as our own HL_IP, and not 127/8) is routed to an AF_UNIX socket at
//   /tmp/.hl-bridge-<HL_NETBR>/<ip>:<port>
// The listening container listens on /tmp/.hl-bridge-<netid>/<ownip>:<port>;
// a peer connect(<ownip>:<port>) dials the same path. Because every container on the host is a JIT
// process under the same user, the two AF_UNIX endpoints rendezvous with no bridge / TUN / root. The dir
// is keyed by <netid> (mode 0700, the guest is path-jailed) so other networks never share sockets. The
// 127/8 loopback path (g_netns / lo_*) is untouched and stays per-container; only non-127 in-subnet
// AF_INET is bridged. Off when g_netbr[0]==0 || g_myip==0.
struct br_interface {
    char path[64];
    uint32_t ip;
    uint8_t prefix;
};
static struct br_interface g_netif[HL_NETIF_MAX];
static uint8_t g_netif_count;
static uint16_t g_br_port[HL_NFD];     // fd -> virtual port of a bridge socket (0 = not a bridge socket)
static uint32_t g_br_ip[HL_NFD];       // fd -> virtual IP (network order) reported via getsockname/getpeername
static uint8_t g_br_interface[HL_NFD]; // fd -> interface index + 1
static int g_br_init;
static uint8_t g_icmp_kind[HL_NFD]; // 1=dgram ping socket, 2=raw ping socket
static uint8_t g_icmp_sock[HL_NFD]; // fd has been replaced by a reply socketpair
static uint32_t g_icmp_ip[HL_NFD];  // connected/last echo destination

// Carry the per-fd socket-emulation metadata (SOCK_STREAM-ness, loopback/bridge port + ip) from `src`
// to `dst` when an fd is duplicated/moved (dup/dup3/fcntl F_DUPFD). Without this, a guest that creates a
// TCP socket then relocates it to another fd number (e.g. busybox's xmove_fd -> a fixed low fd) loses the
// `g_sock_stream` flag that gates the loopback + per-network bridge bind/connect redirection, so its
// AF_INET traffic silently falls through to host passthrough and never rendezvous with a peer container.
static void fd_carry_sock(int dst, int src) {
    if (dst < 0 || dst >= HL_NFD || src < 0 || src >= HL_NFD) return;
    memcpy(g_unix_bind[dst], g_unix_bind[src], sizeof g_unix_bind[dst]);
    memcpy(g_unix_peer[dst], g_unix_peer[src], sizeof g_unix_peer[dst]);
    if (g_unix_path_anchor[src] > 0) {
        int anchor = dup(g_unix_path_anchor[src] - 1);
        if (anchor >= 0) anchor = hl_host_process_fd_private_adopt(anchor);
        g_unix_path_anchor[dst] = anchor < 0 ? 0 : anchor + 1;
    }
    g_sock_stream[dst] = g_sock_stream[src];
    g_sock_dgram[dst] = g_sock_dgram[src];
    udp_ref_dup(dst, src);
    g_udp_local_port[dst] = g_udp_local_port[src];
    g_udp_peer_port[dst] = g_udp_peer_port[src];
    g_udp_local_ip[dst] = g_udp_local_ip[src];
    g_udp_peer_ip[dst] = g_udp_peer_ip[src];
    g_udp_local_interface[dst] = g_udp_local_interface[src];
    g_udp_peer_interface[dst] = g_udp_peer_interface[src];
    g_udp_local_v6[dst] = g_udp_local_v6[src];
    g_udp_peer_v6[dst] = g_udp_peer_v6[src];
    g_sock_seqpacket[dst] = g_sock_seqpacket[src];
    seq_ref_dup(dst, src);
    g_sock_pair_peer[dst] = g_sock_pair_peer[src]; // dup aliases the same end -> same partner
    g_sock_object[dst] = g_sock_object[src];
    g_sock_peer_object[dst] = g_sock_peer_object[src];
    g_sock_identity_local_hidden[dst] = g_sock_identity_local_hidden[src];
    g_sock_identity_peer_hidden[dst] = g_sock_identity_peer_hidden[src];
    g_sock_identity_checkpoint_pending[dst] = g_sock_identity_checkpoint_pending[src];
    sock_state_dup(dst, src);
    g_sock_peer_pid[dst] = g_sock_peer_pid[src]; // ... and the same synthetic peer node identity
    g_sock_passcred[dst] = g_sock_passcred[src];
    g_sock_conn[dst] = g_sock_conn[src];
    g_sock_connecting[dst] = g_sock_connecting[src];
    g_sock_host_backed[dst] = g_sock_host_backed[src];
    g_sock_native_peer[dst] = g_sock_native_peer[src];
    g_so_error[dst] = g_so_error[src];
    g_so_reuseport[dst] = g_so_reuseport[src];
    memcpy(g_tcp_optval[dst], g_tcp_optval[src], sizeof g_tcp_optval[dst]);
    memcpy(g_tcp_optset[dst], g_tcp_optset[src], sizeof g_tcp_optset[dst]);
    memcpy(g_ipopt_val[dst], g_ipopt_val[src], sizeof g_ipopt_val[dst]);
    memcpy(g_ipopt_set[dst], g_ipopt_set[src], sizeof g_ipopt_set[dst]);
    g_sock_fam[dst] = g_sock_fam[src];
    g_lo_port[dst] = g_lo_port[src];
    g_lo_v6[dst] = g_lo_v6[src];
    g_lo_v6only[dst] = g_lo_v6only[src];
    g_br_port[dst] = g_br_port[src];
    g_br_ip[dst] = g_br_ip[src];
    g_br_interface[dst] = g_br_interface[src];
    g_tcp_lport[dst] = g_tcp_lport[src];
    g_tcp_laddr[dst] = g_tcp_laddr[src];
    g_tcp_l6[dst] = g_tcp_l6[src];
    g_tcp_listen[dst] = g_tcp_listen[src];
    g_sock_backlog[dst] = g_sock_backlog[src];
    memcpy(g_tcp_laddr6[dst], g_tcp_laddr6[src], 16);
    g_icmp_kind[dst] = g_icmp_kind[src];
    g_icmp_ip[dst] = g_icmp_ip[src];
}

// ---- listening-TCP introspection (ss/netstat -l): a socket the guest bind()+listen()s MUST appear in
// /proc/net/tcp[6] with state 0A (TCP_LISTEN). hl translates the guest's AF_INET(6) bind onto a host
// AF_UNIX switch (or a real host bind in passthrough), so the synthesized /proc/net/tcp table -- which
// runs no real IP stack -- has to remember the guest-requested (addr,port) itself. bind(200) records it;
// listen(201) arms g_tcp_listen; the vfs synth walks these to emit the LISTEN rows. Cleared on
// close/socket-reinit (fd_reset_emul) so a reused fd never reports a stale listener.
// Note: g_tcp_lport/laddr/l6/listen/laddr6 are declared up top alongside the other per-fd socket arrays.
static void netns_tcp_bind_note(int fd, uint16_t port_host, int v6, uint32_t addr4_be, const uint8_t *addr6) {
    if (fd < 0 || fd >= HL_NFD) return;
    g_tcp_lport[fd] = port_host;
    g_tcp_laddr[fd] = addr4_be; // raw __be32 as it sits in memory (printed %08X, kernel-style)
    g_tcp_l6[fd] = (uint8_t)!!v6;
    g_tcp_listen[fd] = 0; // a fresh bind is not yet listening
    if (v6 && addr6)
        memcpy(g_tcp_laddr6[fd], addr6, 16);
    else
        memset(g_tcp_laddr6[fd], 0, 16);
}

static void netns_tcp_listen_note(int fd) {
    if (fd >= 0 && fd < HL_NFD && g_tcp_lport[fd]) g_tcp_listen[fd] = 1;
}

// Update just the recorded local port after the initial bind_note. bind(:0) on the loopback/bridge switch
// records port 0 at bind time (the guest-requested port), then allocates a real ephemeral port -- without
// this write-back the /proc/net/tcp[6] row would carry :0000 AND be suppressed entirely (listen_note gates
// on a nonzero port). The address/family recorded by the earlier bind_note stay correct.
static void netns_tcp_port_note(int fd, uint16_t port_host) {
    if (fd >= 0 && fd < HL_NFD) g_tcp_lport[fd] = port_host;
}

// Emit the LISTEN rows for the v4 (v6==0) or v6 (v6==1) table into `out` (<=cap). Returns bytes written.
// Row layout mirrors the kernel's tcp4_seq/tcp6_seq: sl, local_address:port, rem 0, st 0A, queues 0,
// uid 0, a synthetic-but-stable inode, refcount 1. Values a real ss/netstat parses positionally.
static int netns_tcp_emit(char *out, size_t cap, int v6) {
    int off = 0, sl = 0;
    for (int fd = 0; fd < HL_NFD && off < (int)cap - 256; fd++) {
        if (!g_tcp_listen[fd] || !g_tcp_lport[fd]) continue;
        if ((int)g_tcp_l6[fd] != !!v6) continue;
        unsigned long ino = 100000UL + (unsigned)fd; // stable within a run; distinct per listener
        if (v6) {
            const uint8_t *a = g_tcp_laddr6[fd];
            char h[33];
            for (int i = 0; i < 16; i++)
                snprintf(h + i * 2, 3, "%02x", a[i]);
            off +=
                snprintf(out + off, cap - off,
                         "%4d: %s:%04X 00000000000000000000000000000000:0000 0A "
                         "00000000:00000000 00:00000000 00000000     0        0 %lu 1 0000000000000000 100 0 0 10 0\n",
                         sl++, h, g_tcp_lport[fd], ino);
        } else {
            off +=
                snprintf(out + off, cap - off,
                         "%4d: %08X:%04X 00000000:0000 0A "
                         "00000000:00000000 00:00000000 00000000     0        0 %lu 1 0000000000000000 100 0 0 10 0\n",
                         sl++, g_tcp_laddr[fd], g_tcp_lport[fd], ino);
        }
    }
    return off;
}

// dotted-quad -> network-order u32 (bytes a.b.c.d), 0 on parse failure
