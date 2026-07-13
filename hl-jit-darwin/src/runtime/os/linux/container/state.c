// dd/runtime/os/linux/container -- container config state (UTS/cgroup/USER-ns/port-map) + parsers.
#include "../../container_parse.h" // strict numeric parsing (the config trust boundary; see LAUNCH.md)
#include <sys/sysctl.h>            // sysctlbyname("hw.activecpu") -- true host core count (see container_online_cpus)

// DD_NFD: capacity of every per-guest-fd state table (memfd seals, eventfd/epoll/timerfd, socket
// tracking, pty/lock/pipe tables, ...). Was 1024, which HARD-failed for guests that use high fd numbers:
// Chromium's Mojo shared-memory channel creates a memfd whose fd is >=1024, and F_ADD_SEALS then returned
// EINVAL (fd out of the tracked range) and the browser aborted (channel_linux.cc). 65536 covers a realistic
// RLIMIT_NOFILE; the tables are zero-init BSS so the cost is a few MB of never-resident address space.
#define DD_NFD 65536

// ---- container namespace + cgroup state (SentryConfig: ddockerd -> jit) ----
// UTS ns: container hostname (uname/sethostname); "" = host default
static char g_hostname[65] = "";
// cgroup memory.max bytes (0 = unlimited); charged in mmap
static uint64_t g_mem_max = 0;
// cgroup pids.max (0 = unlimited); checked in clone
static int g_pids_max = 0;
// docker --cpus: online-cpu count the container advertises = ceil(NanoCpus/1e9). 0 = unlimited (all host cores).
static int g_cpu_max = 0;
// docker --read-only: writes to the rootfs/overlay-upper jail fail EROFS (/proc /dev /sys /tmp /run stay writable).
static int g_rootfs_ro = 0;
// Runtime `mount -o remount,ro <subpath>` targets: a guest ABSOLUTE path whose subtree is enforced
// read-only (write-intent syscalls -> EROFS), independent of the bind-vol table and the whole-rootfs
// g_rootfs_ro. This is a PATH-based deny (like rootfs_ro_denies) so it enforces RO without perturbing
// read resolution / overlay merge at all. Append-only, count published LAST so a concurrent path
// resolve sees either the old count or a fully-written entry; entries are never removed (a container
// never downgrades a subtree remount,ro back to rw in practice).
static char g_ro_subpath[16][256];
static int g_nro_subpath = 0;
// 1 if `abs` falls under a runtime remount,ro subtree.
static int ro_subpath_denies(const char *abs) {
    int n = __atomic_load_n(&g_nro_subpath, __ATOMIC_ACQUIRE);
    if (n <= 0 || !abs || abs[0] != '/') return 0;
    for (int i = 0; i < n; i++) {
        size_t L = strlen(g_ro_subpath[i]);
        if (!strncmp(abs, g_ro_subpath[i], L) && (abs[L] == 0 || abs[L] == '/')) return 1;
    }
    return 0;
}
// Register a subtree as read-only (runtime remount,ro). Dedupes; 0 on success, -1 if the table is full.
static int ro_subpath_add(const char *abs) {
    if (!abs || abs[0] != '/') return -1;
    for (int i = 0; i < g_nro_subpath; i++)
        if (!strcmp(g_ro_subpath[i], abs)) return 0;
    if (g_nro_subpath >= 16) return -1;
    int i = g_nro_subpath;
    snprintf(g_ro_subpath[i], sizeof g_ro_subpath[i], "%s", abs);
    __atomic_store_n(&g_nro_subpath, i + 1, __ATOMIC_RELEASE);
    return 0;
}
// docker --ulimit overrides, indexed by Linux RLIMIT_* resource number; .set gates the override.
#define DD_RLIM_MAX 16

static struct {
    int set;
    uint64_t cur, max;
} g_ulimit[DD_RLIM_MAX];

// current anon charge (bytes)
static _Atomic uint64_t g_mem_charged = 0;
// Max argv/envp entries the exec-forward + stack-build path carries. Linux caps only at ARG_MAX (bytes);
// a former fixed 256 silently truncated large generated argv lists (a different command ran). 2048 covers
// realistic exec argv/env while keeping the stack arrays bounded.
#ifndef DD_MAXARGV
#define DD_MAXARGV 2048
#endif
// live task count (init = 1)
static _Atomic int g_pids_cur = 1;
// cumulative process creations since boot (fork/clone/clone3). Linux's /proc/stat `processes` is this
// running total, not the live count -- process-creation telemetry keys on its delta.
static _Atomic unsigned long long g_forks_since_boot = 0;
// PID ns: host pid of the container init -> guest sees it as PID 1
static int g_init_hostpid = 0;

// ===================== cross-engine-process cgroup accounting (pids + memory) =====================
// A container's guest processes are SEPARATE host processes (a guest fork() is a real host fork()), so
// the tasks and memory a cgroup must aggregate are spread across DIFFERENT engine processes. The
// process-local g_pids_cur / g_mem_charged therefore under-report: a parent could not see a forked
// child's tasks/memory in cgroup.procs / pids.current / memory.current, and pids.max was enforced only
// for in-process threads (spawn_thread), never for a forked process. FIX: a small MAP_SHARED|MAP_ANON
// slot table, created FRESH when a process becomes a container init (container_init / the forkserver
// warm re-anchor) and inherited by every guest fork of that init -- COW keeps ONE physical page across
// fork, exactly like the futex bucket table (thread.c). Each engine process owns one slot keyed by its
// host pid, in which it publishes its live guest-task count + charged bytes; the cgroup files SUM the
// LIVE slots (a slot whose pid is dead is skipped and reclaimed), so the totals are container-wide AND
// self-healing across a crash -- no fragile running counter to leak on SIGKILL. A fresh segment per
// container-init isolates sibling forkserver workers (each is its own container).
#define DD_ACCT_SLOTS 1024
struct dd_acct_slot {
    _Atomic int pid;                // host pid owning this slot (0 = free)
    _Atomic int tasks;              // this process's live guest-task (thread) count
    _Atomic unsigned long long mem; // this process's charged anon bytes (memory.current aggregate)
};
static struct dd_acct_slot *g_acct;      // shared slot table (NULL if unavailable -> local fallback)
static struct dd_acct_slot *g_acct_self; // this process's own slot (re-found after fork)
static void acct_proc_leave(void);       // (defined below; atexit'd from acct_container_reset)
// g_mem_charged INHERITED at fork: a child COW-inherits the parent's charge, so its memory.current
// contribution must be only what IT allocates AFTER the fork -- else the parent's charge is counted twice
// (once in each slot). init's baseline is 0. This process's slot mem = g_mem_charged - g_mem_base.
static _Atomic unsigned long long g_mem_base = 0;

static unsigned long long acct_self_mem(void) {
    unsigned long long c = atomic_load(&g_mem_charged), b = atomic_load(&g_mem_base);
    return c > b ? c - b : 0;
}

// True if host pid `p` is a live process (self needs no syscall). errno-preserving so it is safe to call
// from the fork/clone gate without perturbing the caller's syscall-boundary errno.
static int acct_pid_live(int p) {
    if (p <= 0) return 0;
    if (p == (int)getpid()) return 1;
    int se = errno;
    int r = (kill(p, 0) == 0 || errno != ESRCH);
    errno = se;
    return r;
}

// Claim (or find) THIS process's slot: reuse one already tagged with our pid, else grab a free/dead one.
static void acct_claim_self(void) {
    if (!g_acct) {
        g_acct_self = NULL;
        return;
    }
    int me = (int)getpid();
    for (int i = 0; i < DD_ACCT_SLOTS && !g_acct_self; i++)
        if (atomic_load(&g_acct[i].pid) == me) g_acct_self = &g_acct[i];
    for (int i = 0; i < DD_ACCT_SLOTS && !g_acct_self; i++) {
        int p = atomic_load(&g_acct[i].pid);
        if (p != 0 && acct_pid_live(p)) continue; // a live peer holds this slot
        int exp = p;
        if (atomic_compare_exchange_strong(&g_acct[i].pid, &exp, me)) g_acct_self = &g_acct[i];
    }
    if (g_acct_self) {
        atomic_store(&g_acct_self->tasks, atomic_load(&g_pids_cur));
        atomic_store(&g_acct_self->mem, acct_self_mem());
    }
}

// (Re)create the shared accounting table for a NEW container init and claim this process's slot. Called
// from container_init (normal launch + cold forkserver) and the forkserver warm re-anchor point.
static void acct_container_reset(void) {
    size_t sz = sizeof(struct dd_acct_slot) * DD_ACCT_SLOTS;
    void *m = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    g_acct = (m == MAP_FAILED) ? NULL : (struct dd_acct_slot *)m; // kernel zero-fills -> all slots free
    g_acct_self = NULL;
    acct_claim_self();
    static int reg = 0; // free our slot on a normal exit (fork children inherit this atexit registration)
    if (!reg) {
        atexit(acct_proc_leave);
        reg = 1;
    }
}

// A forked child is a NEW host process: it kept only the calling thread (one task) and needs its OWN
// slot (its pid differs from the parent's). Called from the fork child path (fork_child_hooks).
static void acct_after_fork(void) {
    atomic_store(&g_pids_cur, 1);                        // fork() clones only the calling thread -> one task now
    atomic_store(&g_mem_base, atomic_load(&g_mem_charged)); // charge inherited COW -> child counts only NEW allocs
    g_acct_self = NULL;                                 // the inherited pointer is the PARENT's slot; re-find ours
    acct_claim_self();
}

// The parent pre-registers a just-forked child's slot so pids.current/memory reflect it immediately
// (no race waiting for the child to schedule); the child later re-adopts this same slot in
// acct_after_fork (same pid -> reused).
static void acct_child_born(int childpid) {
    if (!g_acct || childpid <= 0) return;
    for (int i = 0; i < DD_ACCT_SLOTS; i++)
        if (atomic_load(&g_acct[i].pid) == childpid) return; // child already self-registered
    for (int i = 0; i < DD_ACCT_SLOTS; i++) {
        int p = atomic_load(&g_acct[i].pid);
        if (p != 0 && acct_pid_live(p)) continue;
        int exp = p;
        if (atomic_compare_exchange_strong(&g_acct[i].pid, &exp, childpid)) {
            atomic_store(&g_acct[i].tasks, 1);
            atomic_store(&g_acct[i].mem, 0);
            return;
        }
    }
}

// Publish this process's live task count / charged bytes into its slot (cheap; call after g_pids_cur or
// g_mem_charged changes). No-op when this process has no slot (bare mode / table full -> local fallback).
static inline void acct_publish_tasks(void) {
    if (g_acct_self) atomic_store_explicit(&g_acct_self->tasks, atomic_load(&g_pids_cur), memory_order_relaxed);
}

static inline void acct_publish_mem(void) {
    if (g_acct_self) atomic_store_explicit(&g_acct_self->mem, acct_self_mem(), memory_order_relaxed);
}

// Release this process's slot on exit (idempotent). A crash that skips this leaves a stale slot the
// liveness check reclaims, so this is best-effort cleanliness, not a correctness requirement.
static void acct_proc_leave(void) {
    if (g_acct_self) {
        atomic_store(&g_acct_self->pid, 0);
        g_acct_self = NULL;
    }
}

// Container-wide live guest-task count = sum over every live slot's task count (dead slots skipped +
// reclaimed). Drives pids.current AND the pids.max fork/clone gate. Falls back to the process-local
// count when the shared table is unavailable.
static int acct_pids_total(void) {
    if (!g_acct) {
        int l = atomic_load(&g_pids_cur);
        return l > 0 ? l : 1;
    }
    int me = (int)getpid(), total = 0;
    for (int i = 0; i < DD_ACCT_SLOTS; i++) {
        int p = atomic_load(&g_acct[i].pid);
        if (p == 0) continue;
        if (p != me && !acct_pid_live(p)) { // a dead engine's slot -> reclaim it
            int exp = p;
            atomic_compare_exchange_strong(&g_acct[i].pid, &exp, 0);
            continue;
        }
        int t = atomic_load(&g_acct[i].tasks);
        total += t > 0 ? t : 1;
    }
    return total > 0 ? total : 1;
}

// Container-wide charged memory (bytes) = sum over every live slot (self read live for freshness). The
// cross-process memory.current under a memory.max cap (the charge model is only maintained under a cap).
static unsigned long long acct_mem_total(void) {
    if (!g_acct) return acct_self_mem();
    int me = (int)getpid();
    unsigned long long total = 0;
    for (int i = 0; i < DD_ACCT_SLOTS; i++) {
        int p = atomic_load(&g_acct[i].pid);
        if (p == 0) continue;
        if (p != me && !acct_pid_live(p)) {
            int exp = p;
            atomic_compare_exchange_strong(&g_acct[i].pid, &exp, 0);
            continue;
        }
        total += (p == me) ? acct_self_mem() : atomic_load(&g_acct[i].mem);
    }
    return total;
}

// ---- checkpoint/restore deterministic address placement (armed only when checkpoint/restore is in use) ----
// A same-ISA JIT keeps guest VA == host VA, so a restore MAP_FIXEDs every guest region back to its exact
// address. But macOS ASLR places kernel-chosen guest mmaps (heap, stack, anon/file maps) at LOW addresses
// that differ every exec -- so a VA free in the checkpointed run can be occupied by the fresh restore
// process's own libraries. When armed (DDJIT_CHECKPOINT_DIR or DDJIT_RESTORE_DIR set), guest allocations are
// instead HINTED into a high arena well above the engine's own mappings and the pcache image/interp bases
// (0x40../0x48..TB) -- a range reliably free in any process, so the restore's MAP_FIXED always lands. Inert
// (returns 0 -> normal kernel placement) unless armed, so a normal launch and the whole gate are unchanged.
static int g_ckpt_armed = 0;
static uint64_t g_ckpt_place_next = 0x50000000000ull;

// Shared checkpoint-request state (defined here, early, so the blocking-syscall restart decision in signal.c
// can consult it as well as checkpoint.c). g_ckpt_trigger points at a MAP_SHARED generation counter every
// engine process maps (set up in ckpt_control_init); a checkpoint is REQUESTED when it advances past the
// generation this process last acted on. Both stay 0/NULL unless armed, so ckpt_pending() is inert on a
// normal launch (and always for x86, which never arms checkpoint) -- the whole gate is unaffected.
static volatile uint32_t *g_ckpt_trigger;
static uint32_t g_ckpt_seen_gen;

// A whole-tree checkpoint has been requested. Consulted by syscall_should_restart / svc_poll_retry so a
// process parked in a blocking host syscall (bash in read()/poll on its pty -- every GUI shell at a prompt)
// returns EINTR and reaches the dispatcher safepoint (where G_CKPT_POLL runs) instead of transparently
// re-blocking. Inert (0) unless a checkpoint is actually in flight, so SA_RESTART semantics are untouched in
// normal operation.
static int ckpt_pending(void) __attribute__((unused));
static int ckpt_pending(void) {
    return g_ckpt_trigger && (*g_ckpt_trigger != g_ckpt_seen_gen);
}

static uint64_t ckpt_place_hint(uint64_t len) {
    if (!g_ckpt_armed || !len) return 0;
    uint64_t a = g_ckpt_place_next;
    uint64_t step = (len + 0xffffull) & ~0xffffull; // 16 KB granularity (macOS page)
    g_ckpt_place_next += step + 0x100000ull;        // 1 MB gap between regions
    return a;
}

// After a restore, keep the placement cursor ABOVE every region we mapped back, so a mapping the resumed
// guest makes (and a SECOND checkpoint) does not alias a restored region.
static void ckpt_place_bump_past(uint64_t end) {
    if (end > g_ckpt_place_next) g_ckpt_place_next = (end + 0xfffffull) & ~0xfffffull;
}

// This restored process's OWN guest pid (0 => normal launch, report the host pid). A checkpoint restore
// re-forks the process tree, so the live host pids DIFFER from the ones the guest baked into its memory at
// checkpoint time; a restored non-init process reports its checkpoint-time guest pid here so getpid() stays
// stable. Init keeps 0 and uses the g_init_hostpid<->1 mapping below (which also yields 1).
static int g_self_gpid = 0;
static int g_self_gppid = -1; // this restored process's guest PARENT pid (-1 => unset, use the real getppid)

static int container_pid(void) {
    if (g_self_gpid) return g_self_gpid;
    int h = getpid();
    return (g_init_hostpid && h == g_init_hostpid) ? 1 : h;
}

// ---- checkpoint/restore PID virtualization (INACTIVE on a normal launch) --------------------------------
// dd normally uses the REAL host pid as a guest child's pid (only the init is virtualized). A restore assigns
// NEW host pids to the re-forked tree, so this table maps each restored process's checkpoint-time guest pid
// <-> its new live host pid, keeping guest-visible pids stable across a restore (a blocked wait4's target, a
// reaped-child pid, bash's job table, kill(pid)). It is EMPTY on every normal launch (g_pidmap_n==0 => every
// translator below is an identity no-op), so it changes nothing outside the restore path.
#define PIDMAP_MAX 4096
static struct {
    int gpid, live;
} g_pidmap[PIDMAP_MAX];
static int g_pidmap_n = 0;

static void pidmap_add(int gpid, int live) __attribute__((unused));
static void pidmap_add(int gpid, int live) {
    if (g_pidmap_n >= PIDMAP_MAX || gpid <= 0 || live <= 0) return;
    for (int i = 0; i < g_pidmap_n; i++)
        if (g_pidmap[i].gpid == gpid) {
            g_pidmap[i].live = live;
            return;
        }
    g_pidmap[g_pidmap_n].gpid = gpid;
    g_pidmap[g_pidmap_n].live = live;
    g_pidmap_n++;
}

static void pidmap_del_live(int live) __attribute__((unused));
static void pidmap_del_live(int live) {
    for (int i = 0; i < g_pidmap_n; i++)
        if (g_pidmap[i].live == live) {
            g_pidmap[i] = g_pidmap[--g_pidmap_n];
            return;
        }
}

// guest pid -> live host pid (translate a syscall ARGUMENT: wait4/kill target). Identity on a miss.
static int pidmap_to_live(int gpid) __attribute__((unused));
static int pidmap_to_live(int gpid) {
    if (g_pidmap_n == 0 || gpid <= 0) return gpid;
    for (int i = 0; i < g_pidmap_n; i++)
        if (g_pidmap[i].gpid == gpid) return g_pidmap[i].live;
    return gpid;
}

// live host pid -> guest pid (translate a syscall RETURN: a reaped child's pid). Identity on a miss.
static int pidmap_to_guest(int live) __attribute__((unused));
static int pidmap_to_guest(int live) {
    if (g_pidmap_n == 0 || live <= 0) return live;
    for (int i = 0; i < g_pidmap_n; i++)
        if (g_pidmap[i].live == live) return g_pidmap[i].gpid;
    return live;
}

// `docker run --network none`: the daemon sets DD_NET_ISOLATE=1. The container is then LOOPBACK-ONLY — no
// eth0 is presented in the interface model (netlink RTM_GETLINK/GETADDR/GETROUTE dumps + SIOCGIFCONF in
// netns.c, and /proc/net/dev·route + /sys/class/net in vfs.c), matching docker's `none` network (only lo).
// Defined here (state.c is the FIRST container TU include) so both vfs.c and netns.c can consume it. Lazily
// cached. Off (eth0 present) for the default bridge / user networks, so it never affects a normal container.
static int g_net_isolate = -1;

static int net_isolate(void) {
    if (g_net_isolate < 0) g_net_isolate = getenv("DD_NET_ISOLATE") != NULL;
    return g_net_isolate;
}

// ---- container network-interface model --------------------------------------------------
// dd runs no real network stack, so a container had NO interface introspection at all: /sys/class/net
// and /proc/net/* were absent and AF_NETLINK sockets failed EAFNOSUPPORT, breaking getifaddrs /
// go-sockaddr / netlink (consul, minio, `ip`, ifconfig). To fix that coherently we model exactly two
// interfaces -- lo (127.0.0.1/8, ::1) and eth0 (the container's bridge IP, or a stable synthetic
// 172.17.0.2/16). This ONE model is consumed by the RTNETLINK responder (netns.c) and the procfs /
// sysfs synthesis (vfs.c) so every path agrees. eth0's IPv4 is the bridge IP from DD_IP (set by the
// daemon for a bridged container); with no bridge (--network none/host) it falls back to the synthetic.
// Returns the address as a network-order u32 held in host byte order (a | b<<8 | c<<16 | d<<24), the
// same encoding netns.c's br_parse_ip produces and /proc/net/route prints with %08X.
static uint32_t netif_eth0_ip(void) {
    const char *ip = getenv("DD_IP");
    if (ip && ip[0]) {
        unsigned a = 0, b = 0, cc = 0, d = 0;
        if (sscanf(ip, "%u.%u.%u.%u", &a, &b, &cc, &d) == 4 && a < 256 && b < 256 && cc < 256 && d < 256)
            return (uint32_t)(a | (b << 8) | (cc << 16) | (d << 24));
    }
    return (uint32_t)(172 | (17 << 8) | (0 << 16) | (2 << 24)); // 172.17.0.2 (docker default bridge)
}

static int netif_eth0_prefix(void) {
    return 16;
} // docker default bridge is /16 (cf. br_in_subnet)

// eth0 broadcast = (ip | ~mask); mask = the top prefixlen bits (in network-order-as-host-u32 form).
static uint32_t netif_eth0_bcast(void) {
    int pfx = netif_eth0_prefix();
    uint32_t host_mask = pfx >= 32 ? 0xffffffffu : ((1u << pfx) - 1u); // low `pfx` bits set (= net bytes)
    return netif_eth0_ip() | ~host_mask;
}

// eth0 network base (ip & mask) and gateway (base | .1, i.e. host-octet 1 -> byte3 in this encoding).
static uint32_t netif_eth0_net(void) {
    int pfx = netif_eth0_prefix();
    uint32_t host_mask = pfx >= 32 ? 0xffffffffu : ((1u << pfx) - 1u);
    return netif_eth0_ip() & host_mask;
}

static uint32_t netif_eth0_gw(void) {
    return netif_eth0_net() | 0x01000000u;
} // .1 = octet4 = high byte

// eth0 MAC = 02:42:<4 ip bytes> (docker's bridge-container MAC convention). out[6].
static void netif_eth0_mac(uint8_t *out) {
    uint32_t ip = netif_eth0_ip();
    out[0] = 0x02;
    out[1] = 0x42;
    out[2] = (uint8_t)(ip & 0xff);
    out[3] = (uint8_t)((ip >> 8) & 0xff);
    out[4] = (uint8_t)((ip >> 16) & 0xff);
    out[5] = (uint8_t)((ip >> 24) & 0xff);
}

static int g_uid = -1,
           // USER ns: container uid/gid (-1 = passthrough host id; container defaults to 0=root)
    g_gid = -1;

static int cuid(void) {
    return g_uid >= 0 ? g_uid : (int)getuid();
}

static int cgid(void) {
    return g_gid >= 0 ? g_gid : (int)getgid();
}

// --: guest-set ownership persistence (chown(2)/fchownat on overlay-upper files) ----
// Rootless: a guest chown can't change the host file's REAL owner, and reports host-owned files
// as the container uid/gid -- so a guest-set owner was silently lost (chown returned 0 but a re-stat
// still showed the default). Persist the guest-set (uid,gid) as host xattrs on the overlay
// backing file; fill_linux_stat prefers them over the cuid/cgid default. A guest id of -1 means
// "don't change" (POSIX chown) -> leave that xattr untouched so the other id / the default survives.
// xattrs live on the real APFS upper file, so they persist across a re-stat AND across processes.
#include <sys/xattr.h>
#define DD_XATTR_UID "user.dd.uid"
#define DD_XATTR_GID "user.dd.gid"
// PERF (sqlite-select / any stat-heavy workload): reading the guest-chown xattr back on EVERY stat cost
// two macOS fgetxattr/getxattr per stat (~2.5us each on APFS even for a MISS -> ~5us/stat, 40-50x native
// fstat). But the dd.uid/dd.gid xattr is set ONLY by an explicit guest chown or a cred-dropped create
// (chown_xattr_set_*); the overwhelmingly common file has none. So keep a per-inode NEGATIVE cache: once
// we confirm an inode carries no dd xattr, skip the syscalls on repeat stats of the same inode. A global
// generation counter (bumped on every set) invalidates the whole cache the instant any chown xattr is
// written, so a stale "no xattr" verdict can never outlive the xattr appearing. Cross-process correctness
// is free: a new engine process starts with an empty cache, so its first stat of any inode does the real
// read and sees a pre-existing (persisted-on-disk) xattr. fork inherits the cache (same host files, same
// xattr state); in-process execve keeps it (idem). Kill switch: DDJIT_NOXATTRCACHE=1 forces the old
// always-read path. Keyed on (st_dev,st_ino) which fill_linux_stat already has from the just-done stat.
static uint32_t g_chown_gen = 1; // 0 reserved for "empty slot"
static int g_noxattrcache = -1;  // -1 = uninit; 1 = cache disabled (kill switch)
#define DD_NOXC_N 4096           // direct-mapped; power of two

static struct {
    uint64_t dev, ino;
    uint32_t gen;
} g_noxc[DD_NOXC_N];

static inline uint32_t noxc_slot(uint64_t dev, uint64_t ino) {
    uint64_t h = (ino * 0x9E3779B97F4A7C15ull) ^ (dev * 0xC2B2AE3D27D4EB4Full);
    return (uint32_t)(h >> 33) & (DD_NOXC_N - 1);
}

static void chown_gen_bump(void) { // any new xattr invalidates every cached "no xattr" verdict
    if (++g_chown_gen == 0) {      // wrap: reset to a fresh generation and clear stale slots
        g_chown_gen = 1;
        memset(g_noxc, 0, sizeof g_noxc);
    }
}

static void chown_xattr_set_path(const char *hostpath, int uid, int gid, int nofollow) {
    int opt = nofollow ? XATTR_NOFOLLOW : 0;
    if (uid >= 0) {
        uint32_t v = (uint32_t)uid;
        setxattr(hostpath, DD_XATTR_UID, &v, sizeof v, 0, opt);
    }
    if (gid >= 0) {
        uint32_t v = (uint32_t)gid;
        setxattr(hostpath, DD_XATTR_GID, &v, sizeof v, 0, opt);
    }
    if (uid >= 0 || gid >= 0) chown_gen_bump();
}

static void chown_xattr_set_fd(int fd, int uid, int gid) {
    if (uid >= 0) {
        uint32_t v = (uint32_t)uid;
        fsetxattr(fd, DD_XATTR_UID, &v, sizeof v, 0, 0);
    }
    if (gid >= 0) {
        uint32_t v = (uint32_t)gid;
        fsetxattr(fd, DD_XATTR_GID, &v, sizeof v, 0, 0);
    }
    if (uid >= 0 || gid >= 0) chown_gen_bump();
}

// Read back the guest-set ids (fd preferred when fd>=0, else hostpath). Each out is the set id or -1
// (no xattr -> keep the cuid/cgid default). Returns 1 if either id was guest-set. dev/ino identify
// the just-stat'd inode for the negative cache above (0/0 = skip caching, e.g. synthetic entries).
static int chown_xattr_get(const char *hostpath, int fd, uint64_t dev, uint64_t ino, int *uid, int *gid) {
    *uid = -1;
    *gid = -1;
    if (fd < 0 && !hostpath) return 0; // synthetic: no backing file to read
    if (g_noxattrcache < 0) g_noxattrcache = getenv("DDJIT_NOXATTRCACHE") != NULL ? 1 : 0;
    int use_cache = !g_noxattrcache && ino != 0;
    uint32_t slot = 0;
    if (use_cache) {
        slot = noxc_slot(dev, ino);
        if (g_noxc[slot].gen == g_chown_gen && g_noxc[slot].dev == dev && g_noxc[slot].ino == ino)
            return 0; // cached: this inode carries no dd chown xattr (verified at the current generation)
    }
    uint32_t v;
    int present = 0;
    if (fd >= 0) {
        if (fgetxattr(fd, DD_XATTR_UID, &v, sizeof v, 0, 0) == (ssize_t)sizeof v) {
            *uid = (int)v;
            present = 1;
        }
        if (fgetxattr(fd, DD_XATTR_GID, &v, sizeof v, 0, 0) == (ssize_t)sizeof v) {
            *gid = (int)v;
            present = 1;
        }
    } else {
        if (getxattr(hostpath, DD_XATTR_UID, &v, sizeof v, 0, 0) == (ssize_t)sizeof v) {
            *uid = (int)v;
            present = 1;
        }
        if (getxattr(hostpath, DD_XATTR_GID, &v, sizeof v, 0, 0) == (ssize_t)sizeof v) {
            *gid = (int)v;
            present = 1;
        }
    }
    if (use_cache && !present) { // record the confirmed miss so a repeat stat of this inode skips the reads
        g_noxc[slot].dev = dev;
        g_noxc[slot].ino = ino;
        g_noxc[slot].gen = g_chown_gen;
    }
    return present;
}

// the SINGLE source of truth for guest-visible file ownership. Map a host stat's raw uid/gid to
// the ids the guest must see: the cuid/cgid container default for engine-owned files, overridden by
// the guest-chown xattr (read through the negative cache). EVERY stat-family syscall routes its
// owner fields through here -- fill_linux_stat (fstat/stat/newfstatat) AND the statx handler -- so all of
// them report identical ownership for the same file (statx previously copied the RAW host uid/gid,
// diverging from fstat wherever cuid/cgid or a guest chown applied). hostpath/fd identify the backing file
// for the xattr read (NULL/-1 when synthetic -> the cuid/cgid default applies).
static void stat_virt_ids(const struct stat *s, const char *hostpath, int fd, uint32_t *out_uid, uint32_t *out_gid) {
    uint32_t uid = (s->st_uid == (uid_t)getuid()) ? (uint32_t)cuid() : (uint32_t)s->st_uid;
    uint32_t gid = (s->st_gid == (gid_t)getgid()) ? (uint32_t)cgid() : (uint32_t)s->st_gid;
    int xu, xg;
    if (chown_xattr_get(hostpath, fd, (uint64_t)s->st_dev, (uint64_t)s->st_ino, &xu, &xg)) {
        if (xu >= 0) uid = (uint32_t)xu;
        if (xg >= 0) gid = (uint32_t)xg;
    }
    *out_uid = uid;
    *out_gid = gid;
}

// ---- runtime credential overlay (USER ns) -- defined here (BEFORE fs.c AND proc.c in the unity TU) --
// cuid()/cgid() give the container's CONFIGURED identity (default 0=root); a privileged guest may drop
// to an unprivileged id at runtime (apt forks /usr/lib/apt/methods/http, switching to `_apt`; gosu
// switches postgres to uid 70) and then VERIFIES the drop took -- and that it can NOT regain root. We
// track real/effective/saved uid+gid and honour the Linux permission model (a euid==0 task is
// privileged; otherwise a new id must already be one of its three) so both the drop AND the
// regain-must-fail check behave as on Linux. The base is cuid()/cgid() (fork inherits the copy, exec
// re-seeds from the container default). The set*id syscall HANDLERS live in proc.c and mutate these.
static int g_cred_init = 0;
static int g_ruid, g_euid, g_suid; // real / effective / saved-set uid
static int g_rgid, g_egid, g_sgid; // real / effective / saved-set gid
// model the CAP_SETUID/CAP_SETGID capability that actually governs set*id -- not just euid==0. Real
// Linux clears a task's EFFECTIVE caps when euid transitions 0->nonzero, and clears the PERMITTED set too
// once every uid (r/e/s) is nonzero UNLESS PR_SET_KEEPCAPS is armed; a later capset() can then re-raise
// effective from permitted. setpriv relies on exactly this: it sets KEEPCAPS, does setresuid(1000,...),
// capset()s to re-raise, then setresgid(1000,...) -- which our euid==0-only gate wrongly rejected (EPERM).
// apt/gosu drop WITHOUT keepcaps, so permitted->0 and they correctly can never regain root.
static int g_keepcaps = 0;                    // PR_SET_KEEPCAPS armed (caps survive the all-nonzero uid drop)
static int g_cap_setid_perm, g_cap_setid_eff; // permitted / effective CAP_SETUID+CAP_SETGID (move together)

static void cred_init(void) {
    if (g_cred_init) return;
    g_ruid = g_euid = g_suid = cuid();
    g_rgid = g_egid = g_sgid = cgid();
    // A container starts as root (uid 0 by default) with full caps; a non-root container default holds none.
    g_cap_setid_perm = g_cap_setid_eff = (g_euid == 0);
    g_cred_init = 1;
}

// Recompute the CAP_SETID state after a uid change, per the kernel's credential rules (call from every
// set*uid handler AFTER it mutates g_ruid/euid/suid). effective is cleared the moment euid != 0; permitted
// is cleared once all three uids are nonzero unless KEEPCAPS is armed; root (euid 0) holds both.
static void cred_uid_changed(void) {
    if (g_euid == 0) {
        g_cap_setid_perm = g_cap_setid_eff = 1;
        return;
    }
    g_cap_setid_eff = 0; // euid left 0 -> effective caps dropped (a capset can re-raise from permitted)
    if (g_ruid != 0 && g_suid != 0 && !g_keepcaps) g_cap_setid_perm = 0; // all-nonzero, no keepcaps -> gone
}

// execve of an ordinary (non-setuid, no-file-cap) binary recomputes the capability state: a non-root
// task loses all caps, root keeps them, and PR_SET_KEEPCAPS is cleared -- so a program that dropped uid and
// then exec'd cannot silently retain CAP_SETUID/SETGID. The uid/gid values THEMSELVES persist across exec
// (the engine reloads the image in-process), exactly as the kernel carries credentials over an execve.
static void cred_after_exec(void) {
    cred_init();
    g_keepcaps = 0;
    g_cap_setid_perm = g_cap_setid_eff = (g_euid == 0);
}

static int cred_euid(void) {
    cred_init();
    return g_euid;
}

static int cred_egid(void) {
    cred_init();
    return g_egid;
}

// A task may set an id it already holds (real/effective/saved) or ANY id while it holds effective
// CAP_SETUID/CAP_SETGID (which root does, and which KEEPCAPS+capset preserves across a uid drop). -1 means
// "leave unchanged".
static int uid_permitted(int id) {
    return id == -1 || g_cap_setid_eff || id == g_ruid || id == g_euid || id == g_suid;
}

static int gid_permitted(int id) {
    return id == -1 || g_cap_setid_eff || id == g_rgid || id == g_egid || id == g_sgid;
}

// ---- Container capability + security-context sets (caps audit; extends) ------------------
// A default `docker run` ROOT container is NOT all-powerful: runc keeps only 14 of the ~40 Linux caps and
// drops the rest. The masks below are what /proc/self/status (CapPrm/CapEff/CapBnd), capget(2) and
// prctl(PR_CAPBSET_READ) must ALL report identically — nginx/postgres/systemd/capsh gate privileged
// operations on their effective/bounding set. Defined here (state.c is the first container TU include) so
// both vfs.c (the /proc/self/status builder) and proc.c (capget/prctl handlers) consume ONE source of
// truth. Effective narrows when a guest capset()s a smaller set; the bounding set narrows on
// PR_CAPBSET_DROP; inheritable/ambient stay empty (the docker default). Previously dd reported all-ones
// (0xffffffffffffffff) — grossly over-reporting caps vs real docker.
#define DD_CAP_DEFAULT                                                                                                 \
    0x00000000a80425fbull                   // chown,dac_override,fowner,fsetid,kill,setgid,setuid,setpcap,
                                            // net_bind_service,net_raw,sys_chroot,mknod,audit_write,setfcap
static uint64_t g_cap_eff = DD_CAP_DEFAULT; // process EFFECTIVE cap set (capset(2) may narrow it)
static uint64_t g_cap_bnd = DD_CAP_DEFAULT; // process BOUNDING cap set (PR_CAPBSET_DROP clears bits)
static int g_nnp;                           // PR_SET/GET_NO_NEW_PRIVS: sticky; /proc/self/status NoNewPrivs
// ---- image-derived supplementary groups (runc additionalGids) --------------------------------
// A default `docker run` gives the container's run user (default root, uid 0) the supplementary GID set
// runc DERIVES FROM THE IMAGE ROOTFS -- not a fixed constant. runc reads /etc/passwd for the run user's
// NAME + primary gid, then scans /etc/group in file order and collects every group whose comma-separated
// member list contains that name; the additionalGids = [primary gid] followed by those member-derived gids,
// WITHOUT dedup (so a user that is ALSO a listed member of its own primary group appears twice -- alpine
// root's group set is literally "0 0 1 2 3 4 6 10 11 20 26 27" because group root(0) lists member "root").
// getgroups(2) AND /proc/self/status `Groups:` must report this exact multiset in this order (image-derived:
// alpine root -> the above; ubuntu/debian root -> "0"; busybox root -> "0 10"). Parsed once at
// container_init by container_parse_groups() (in vfs.c, which has the overlay-aware path resolver) into the
// array below; bare (no-rootfs) mode leaves g_groups_parsed=0 so getgroups keeps its prior host-backed
// behavior and the status Groups line stays empty -- nothing regresses. setgroups(2) replaces the set
// (apt/gosu drop their supplementary groups before switching user), keeping getgroups + status coherent.
#define DD_NGROUPS_MAX 64
static gid_t g_groups[DD_NGROUPS_MAX];
static int g_ngroups = 0;       // count in g_groups (may be 0 after a guest setgroups(0))
static int g_groups_parsed = 0; // 1 once container_parse_groups ran (rootfs mode); gates getgroups/setgroups

static void groups_reset(void) {
    g_ngroups = 0;
}

static void groups_append(gid_t g) {
    if (g_ngroups < DD_NGROUPS_MAX) g_groups[g_ngroups++] = g;
}

// Render the set for /proc/[pid]/status: space-separated with a TRAILING space, exactly as the kernel prints
// the Groups: line (e.g. "0 0 1 2 3 4 6 10 11 20 26 27 "). Empty when unparsed -> "Groups:\t\n" as before.
static int groups_status_str(char *b, size_t n) {
    int o = 0;
    for (int i = 0; i < g_ngroups && o >= 0 && (size_t)o < n; i++)
        o += snprintf(b + o, n - (size_t)o, "%u ", (unsigned)g_groups[i]);
    if (o < 0) o = 0;
    b[(size_t)o < n ? (size_t)o : n - 1] = 0;
    return o;
}

// --: new-file ownership stamp (runtime setuid/setgid drop) ----------------------------
// A guest that drops privilege at runtime (setuid/setresuid/setfsuid -> gosu's postgres) and then
// CREATES a file/dir must have the new inode owned by its CURRENT effective fsuid/fsgid, NOT the
// cuid/cgid container default that fill_linux_stat applies to host-owned files. tracked only
// EXPLICIT chown(2); a plain create left no xattr, so a new file re-appeared as the container id (0),
// which broke initdb ("data directory has wrong ownership"). fsuid/fsgid follow the overlay's
// euid/egid unless setfsuid/setfsgid override them (g_fs*_ovr >= 0); any subsequent set*id resets the
// override (POSIX: fsuid tracks euid). We persist the intended owner as the SAME dd.uid/gid xattr the
// chown path uses, so a later stat reports it. The create sites in fs.c call the helpers below.
static int g_fsuid_ovr = -1, g_fsgid_ovr = -1; // -1 = follow euid/egid

static int newfile_uid(void) {
    return g_fsuid_ovr >= 0 ? g_fsuid_ovr : cred_euid();
}

static int newfile_gid(void) {
    return g_fsgid_ovr >= 0 ? g_fsgid_ovr : cred_egid();
}

// True only when a runtime cred drop makes the new-file owner differ from the cuid/cgid default -- the
// create paths gate their pre-existence probe + stamp on this so the common (no-drop) case is free.
static int newfile_stamp_wanted(void) {
    return newfile_uid() != cuid() || newfile_gid() != cgid();
}

// Stamp a freshly-created inode's owner, but only the id(s) that differ from the default (so a
// root-created file stays xattr-free). fd form for openat(O_CREAT); path form for mkdir/mknod.
static void newfile_stamp_fd(int fd) {
    int u = newfile_uid(), g = newfile_gid();
    chown_xattr_set_fd(fd, u != cuid() ? u : -1, g != cgid() ? g : -1);
}

static void newfile_stamp_path(const char *hostpath, int nofollow) {
    int u = newfile_uid(), g = newfile_gid();
    chown_xattr_set_path(hostpath, u != cuid() ? u : -1, g != cgid() ? g : -1, nofollow);
}

// ---- NET ns Phase 1: port-map (docker run -p H:C). bind(:C) actually binds the host port :H;
// getsockname reports :C back so the guest sees the port it asked for. {cport->hport} table.
static struct {
    uint16_t cport, hport;
} g_portmap[32];

static int g_nportmap = 0;
// fd -> the container port it bound (for getsockname)
static uint16_t g_fd_cport[DD_NFD];

static uint16_t pm_host(uint16_t c) {
    for (int i = 0; i < g_nportmap; i++)
        if (g_portmap[i].cport == c) return g_portmap[i].hport;
    return c;
}

// "H:C,H:C,..." (docker -p order: host:container). Ports are strictly validated (1..65535);
// a bad field or more than the cap of entries is an error, not a silent drop.
static void parse_publish(const char *s) {
    while (s && *s) {
        if (g_nportmap >= 32) {
            fprintf(stderr, "dd: too many DD_PUBLISH entries (max 32)\n");
            exit(2);
        }
        const char *colon = strchr(s, ':');
        const char *comma = strchr(s, ',');
        if (!colon || (comma && colon > comma)) {
            fprintf(stderr, "dd: invalid DD_PUBLISH '%s': expected HOST:CONTAINER\n", s);
            exit(2);
        }
        unsigned h = dd_parse_port_field("DD_PUBLISH host port", s, colon);
        unsigned cc = dd_parse_port_field("DD_PUBLISH container port", colon + 1, comma);
        g_portmap[g_nportmap].cport = (uint16_t)cc;
        g_portmap[g_nportmap].hport = (uint16_t)h;
        g_nportmap++;
        if (!comma) break;
        s = comma + 1;
    }
}

// "128M"/"2G"/"512K"/"1048576" -> bytes (docker-style suffixes). Strict: empty/non-numeric/an
// unknown suffix is an error (atoi/strtoull would have silently yielded 0 = unlimited).
static uint64_t parse_size(const char *s) {
    if (!s || !*s) return 0;
    errno = 0;
    char *e = NULL;
    uint64_t v = strtoull(s, &e, 10);
    if (errno != 0 || e == s) {
        fprintf(stderr, "dd: invalid size '%s': not a number\n", s);
        exit(2);
    }
    switch (*e) {
    case '\0': return v;
    case 'k':
    case 'K': return v << 10;
    case 'm':
    case 'M': return v << 20;
    case 'g':
    case 'G': return v << 30;
    default: fprintf(stderr, "dd: invalid size '%s': bad suffix\n", s); exit(2);
    }
}

// ---- resource fidelity: docker --cpus / --read-only / --ulimit (SentryConfig: ddockerd -> jit) ----
// The online-CPU count the container advertises to the guest: the host's online cores, capped by the
// container's --cpus allotment (ceil(NanoCpus/1e9)) and the 64-CPU mask ceiling. A guest's nproc / glibc
// __get_nprocs / GOMAXPROCS / JVM availableProcessors all derive from this (via sched_getaffinity, the
// cpu-topology sysfs, and /proc/{cpuinfo,stat}), so a --cpus-limited container self-sizes to its allotment
// instead of spinning one worker per HOST core and over-subscribing the machine.
static int container_online_cpus(void) {
    // Probe the TRUE host core count via macOS sysctl, NOT sysconf(_SC_NPROCESSORS_ONLN): in the mac-side
    // engine process (the `mac` bridge / x86 fork-server spawn context) macOS sysconf reports 1, which would
    // pin every unconstrained container to a single CPU (nproc=1, affinity mask=1 bit, /sys .../cpu/online="0")
    // and stop guests scaling. hw.activecpu is what Go / the JVM / most runtimes read anyway; mirror the darwin
    // jail (os/darwin/jail/jail.c hw.ncpu/hw.activecpu). Fallbacks: hw.logicalcpu -> hw.ncpu -> sysconf.
    long n = 0;
    int v = 0;
    size_t sz = sizeof v;
    if (sysctlbyname("hw.activecpu", &v, &sz, NULL, 0) == 0 && v > 0)
        n = v;
    else if (sz = sizeof v, sysctlbyname("hw.logicalcpu", &v, &sz, NULL, 0) == 0 && v > 0)
        n = v;
    else if (sz = sizeof v, sysctlbyname("hw.ncpu", &v, &sz, NULL, 0) == 0 && v > 0)
        n = v;
    if (n < 1) n = sysconf(_SC_NPROCESSORS_ONLN); // last-resort (non-darwin build / sysctl failure)
    if (n < 1) n = 1;
    if (g_cpu_max > 0 && (long)g_cpu_max < n) n = g_cpu_max; // docker --cpus allotment clamp
    if (n > 64) n = 64; // one CPU bit per byte fits the 8-byte affinity mask /proc/cpuinfo caps at
    return (int)n;
}

// Map a docker --ulimit NAME to its Linux RLIMIT_* resource number (-1 = unknown). Names per setrlimit(2)
// and docker's ulimit set (docker/opts/opts.go ParseUlimit).
static int ulimit_resource(const char *name) {
    static const struct {
        const char *n;
        int r;
    } t[] = {{"cpu", 0},       {"fsize", 1},  {"data", 2},    {"stack", 3},   {"core", 4},   {"rss", 5},
             {"nproc", 6},     {"nofile", 7}, {"memlock", 8}, {"as", 9},      {"locks", 10}, {"sigpending", 11},
             {"msgqueue", 12}, {"nice", 13},  {"rtprio", 14}, {"rttime", 15}, {0, 0}};

    for (int i = 0; t[i].n; i++)
        if (!strcmp(name, t[i].n)) return t[i].r;
    return -1;
}

// Parse one ulimit numeric value; "unlimited"/"-1" -> RLIM_INFINITY. Fails loud on garbage (trust boundary).
static uint64_t ulimit_val(const char *s) {
    if (!strcmp(s, "unlimited") || !strcmp(s, "-1")) return ~0ull;
    errno = 0;
    char *e = NULL;
    unsigned long long v = strtoull(s, &e, 10);
    if (errno != 0 || e == s || *e) {
        fprintf(stderr, "dd: invalid DD_ULIMITS value '%s': not a number\n", s);
        exit(2);
    }
    return (uint64_t)v;
}

// Parse DD_ULIMITS="name=soft:hard,name=soft:hard,name=both,..." into g_ulimit[] (docker --ulimit set).
static void parse_ulimits(const char *spec) {
    char tb[2048];
    snprintf(tb, sizeof tb, "%s", spec);
    char *sv = NULL;
    for (char *t = strtok_r(tb, ",", &sv); t; t = strtok_r(NULL, ",", &sv)) {
        char *eq = strchr(t, '=');
        if (!eq) {
            fprintf(stderr, "dd: invalid DD_ULIMITS entry '%s': expected NAME=SOFT[:HARD]\n", t);
            exit(2);
        }
        *eq = 0;
        int r = ulimit_resource(t);
        if (r < 0 || r >= DD_RLIM_MAX) continue; // unknown resource -> ignore (forward-compat)
        char *colon = strchr(eq + 1, ':');
        uint64_t soft, hard;
        if (colon) {
            *colon = 0;
            soft = ulimit_val(eq + 1);
            hard = ulimit_val(colon + 1);
        } else {
            soft = hard = ulimit_val(eq + 1);
        }
        g_ulimit[r].set = 1;
        g_ulimit[r].cur = soft;
        g_ulimit[r].max = hard;
    }
}

// Shared resource-config reader (docker --cpus/--read-only/--ulimit). BOTH the aarch64 and x86_64 frontends
// call this from container init, so the contract is engine-identical. Env-only: DD_* survive the mac bridge
// and the x86 fork-server (both inherit env), and the daemon serializes the HostConfig into these vars.
static void container_read_resource_env(void) {
    const char *c = getenv("DD_CPUS");
    if (c && c[0] && !g_cpu_max) {
        int v = dd_parse_id("DD_CPUS", c);
        if (v > 0) g_cpu_max = v;
    }
    if (!g_rootfs_ro && getenv("DD_ROOTFS_RO")) g_rootfs_ro = 1;
    const char *u = getenv("DD_ULIMITS");
    if (u && u[0]) parse_ulimits(u);
}

// 1 if the rootfs is read-only AND `abs` is not under a still-writable pseudo-mount. /proc /dev /sys are
// synthetic (their writes never touch the rootfs); /tmp /run are the container's scratch, kept writable to
// mirror docker's --read-only defaults (runc leaves /proc,/dev,/sys mounted rw + the tmpfses writable).
static int rootfs_ro_denies(const char *abs) {
    if (!g_rootfs_ro || !abs || abs[0] != '/') return 0;
    static const char *const w[] = {"/proc", "/dev", "/sys", "/tmp", "/run", 0};
    for (int i = 0; w[i]; i++) {
        size_t L = strlen(w[i]);
        if (!strncmp(abs, w[i], L) && (abs[L] == 0 || abs[L] == '/')) return 0;
    }
    return 1;
}

// guest PC -> (host = prologue entry for a fresh dispatcher entry,
//             body = post-prologue entry for a CHAINED jump with regs already live)
#define MAP_N 65536
