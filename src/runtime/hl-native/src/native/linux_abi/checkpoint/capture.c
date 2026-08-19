// hl/linux_abi -- native checkpoint/restore ("CRIU-equivalent"), MULTI-PROCESS.
#include "../host_errno.h"
#include "../pipe.h"
#include "region.h"
//
// Freezes a running guest -- a WHOLE process tree (multiple shells, background jobs, their children) -- to an
// on-disk directory (RAM + CPU + fds, per process), so every host engine process can exit and free its
// memory, then later resume the entire tree EXACTLY from where it left off. hl has no Linux kernel (guests
// run in-process on macOS via the JIT), so criu (ptrace, /proc, freezer cgroup) cannot run; but hl IS the
// kernel for its guests -- it owns every guest page, the CPU context and the fd table -- so checkpoint/
// restore is implemented natively in the engine, snapshotting at a guest block boundary.
//
// WHY THE MEMORY RESTORE IS ROBUST: this engine keeps every guest page host-RW and NEVER executes guest
// pages (translation READS their bytes; host code runs in a separate RX arena). So a restore can MAP_FIXED
// every region back as plain anon-RW and memcpy the saved bytes; RELRO/PROT_NONE intent is carried in the
// side registries (g_anonmap prot, g_gna) exactly as the live engine carries it. The translated-code arena
// is NOT persisted -- restore begins with an EMPTY block map (children are re-forked BEFORE the tree runs,
// so no stale translation is ever inherited) and re-translates from the restored guest bytes.
//
// MULTI-PROCESS MODEL (the core): a guest process tree == a tree of host engine processes (each guest
// fork/clone is a real host fork(), proc.c). CHECKPOINT is triggered by advancing a SHARED-MEMORY generation
// counter (a MAP_SHARED anonymous descriptor every engine process maps) -- NOT a signal, because a guest's own
// rt_sigaction silently remaps every guest-reachable host signal (bash SIG_IGNs SIGUSR1). ckpt_poll reads the
// generation at each safepoint; the host's guest-clobber-proof engine interrupt is reused only to
// KICK a process out of a blocking host syscall (EINTR) or a chained in-cache loop (thread_int_handler sets
// cpu->irq when armed) to its next safepoint. The container init (guest pid 1) is the coordinator: it
// enumerates the tree (every ENGINE process in its session -- robust vs the lossy pid registry), kicks each
// peer, waits for each to dump, then dumps itself + writes the MANIFEST (its presence == a complete
// checkpoint). Each process dumps its OWN private memory + cpu + fds into its proc.<gpid> group (memory is
// COW-private per process, so per-process dumps need no cross-process coherency).
// RESTORE: rebuild the tree in ppid order (CRIU's proven ordering) -- the
// init restores its own RAM FIRST (before engine allocation, so MAP_FIXED lands on free VAs), then re-forks
// each child; each child resets its inherited registries, MAP_FIXEDs its OWN saved RAM, runs the shared
// after-fork engine reset (fork_child_hooks: cache re-alias, kqueue rebuild, lock/threg/Mach-port reset),
// reopens its own path fds (tty fds are INHERITED down the fork from the launcher's pty), recursively re-
// forks its own children, then resumes at its saved pc. A restore installs a PID-translation table
// (state.c g_pidmap) so guest-visible pids (a blocked wait4's target, a reaped child's pid, bash's job
// table) stay stable even though the re-forked tree has new host pids.
//
// IMAGE FORMAT (object names in the embedder's store):
//   MANIFEST         : struct ckpt_manifest (magic/version/arch, process count, root gpid) -- written LAST
//   proc.<gpid>/     : one group per guest process (published all-or-nothing)
//     meta   : struct ckpt_meta (identity: self/ppid/pgid/sid gpid; brk/stack/nonpie bounds; exe path; ...)
//     pages  : [struct ckpt_region][region's non-zero pages: {u64 va}{pagesz bytes}] ...  (sparse)
//     cpu    : the whole per-thread struct cpu (host-transient fields zeroed on restore)
//     fds    : n_fds * struct ckpt_fd (TTY | FILE by host path + seek offset + open flags)
//
// Trigger: HL_CHECKPOINT arms capture and maps the inherited trigger descriptor in every forked guest
// process. Advancing its generation and sending the reserved host interrupt to init checkpoints the whole
// tree, then exits it. Restore: HL_RESTORE (or `--restore`) calls the restore path. Both directions carry
// bytes over the socket activation handed the engine; the embedder owns the other end.

#include "../host_fd.h"   // the null-device spelling behind the placeholder descriptors below
#include "../host_wait.h" // waitid/waitpid: coordinator peer-reap; multi-thread refusal probe
#include "../host_tty.h"  // the controlling terminal's line discipline is captured and replayed

#include "../../host/file.h"
#include "../../host/system.h"
#include "../sink_stream.h" // the writer emits every image byte through the sink
#include "../ckpt_source.h" // restore reads the image back through the symmetric source interface
#include "../logical_vma.h"

#define CKPT_MAGIC UINT64_C(0x373054504b434c48)          // "HLCKPT07" (LE) -- per-process meta
#define CKPT_MANIFEST_MAGIC UINT64_C(0x3730304e414d4c48) // "HLMAN007" (LE) -- workspace manifest
#define CKPT_VERSION 6                                   // v6 serializes interrupted-syscall continuation state
#define CKPT_ARCH_X86_64 1
#define CKPT_ARCH_AARCH64 2
#define CKPT_CPU_MAGIC UINT64_C(0x31305550434c4848) // "HHLCPU01" (LE)

#ifndef G_CKPT_ARCH
#error "checkpoint target must define G_CKPT_ARCH"
#endif

struct ckpt_cpu_header {
    uint64_t magic;
    uint64_t version;
    uint64_t arch;
    uint64_t count;
    uint64_t payload_size;
};

#define CKF_TTY 1         // controlling terminal / any tty -- inherited down the restore fork from the launcher pty
#define CKF_FILE 2        // path-backed regular file -- reopened by host path + lseek to the saved offset
#define CKF_PIPE 3        // shared anonymous pipe -- rebuilt once by stable pipe identity before the process refork
#define CKF_BLOB 4        // unlinked/pathless regular file -- content copied into the image and recreated anonymously
#define CKF_MEMFD 5       // anonymous memfd -- blob content plus engine seal metadata
#define CKF_EVENTFD 6     // emulated eventfd -- shared counter/readiness object plus per-descriptor flags
#define CKF_TIMERFD 7     // emulated timerfd -- phase, interval, pending expirations and clock identity
#define CKF_INOTIFY 8     // inotify instance; watches and queued events live in the per-process sidecar
#define CKF_EPOLL 9       // epoll instance; interest graph is rebuilt after all target descriptors exist
#define CKF_SOCKETPAIR 10 // reconstructible AF_UNIX pair endpoint with framed unread queue
#define CKF_SOCKET 11     // unconnected socket or empty-backlog listener
#define CKF_SIGNALFD 12   // signalfd OFD mask plus unread wake-byte queue
#define CKF_DEVICE 13     // path-backed character/block device; reconnect to current host device
#define CKFA_DIRECTORY UINT64_C(1)

// Wire values of HL_CHECKPOINT_POLICY (HL_ENGINE_CHECKPOINT_*). Zero means the caller asked for nothing.
enum ckpt_recovery_policy {
    CKPT_RECOVERY_DEFAULT = 0,
    CKPT_RECOVERY_RECONNECT = 1,
    CKPT_RECOVERY_DISCARD_OPTIONAL = 2,
    CKPT_RECOVERY_REFUSE = 3,
};

struct ckpt_inotify_watch {
    int32_t instance;
    int32_t wd;
    uint32_t mask;
    uint32_t pending;
    uint32_t snapshot_size;
    uint32_t is_directory;
    char path[512];
};

struct ckpt_inotify_move {
    int32_t wd;
    uint32_t mask;
    uint32_t cookie;
    char name[256];
};

struct ckpt_inotify_raw {
    int32_t instance;
    uint32_t size;
};

struct ckpt_manifest {
    uint64_t magic, version, arch;
    uint64_t n_procs;
    uint64_t image_hash, image_files, image_bytes;
    int32_t root_gpid;
    // The controlling terminal's FOREGROUND process group at checkpoint, in guest terms (1 == the container
    // init's own group; 0 == none/unknown). Restore re-points the fresh pty at it (tcsetpgrp) so ^C/^Z reach
    // the foreground job -- without it the resumed tree's fg group defaults to the init and ^C kills the tree.
    int32_t fg_pgid_gpid;
    // The controlling terminal's LINE DISCIPLINE at checkpoint. Restore attaches a fresh pty, which the host
    // creates in cooked mode (ICANON|ECHO); a guest that had put its terminal in raw mode -- every readline
    // shell at a prompt -- does not re-issue tcsetattr after resume, because it believes the terminal is
    // already prepared. The tty then echoes the typed line itself on top of the shell's own echo (bytes come
    // back doubled) and hands the shell whole lines instead of characters. 0 == no terminal / unreadable.
    uint32_t tty_termios;
    uint32_t tty_iflag, tty_oflag, tty_cflag, tty_lflag;
    uint32_t tty_ispeed, tty_ospeed;
    uint8_t tty_cc[32];
};

struct ckpt_meta {
    uint64_t magic, version, arch;
    hl_identity_digest engine_identity;
    uint64_t cpu_sz, pagesz;
    uint64_t n_regions, n_threads, n_fds;
    uint64_t brk_lo, brk_cur, brk_hi;
    uint64_t nonpie_lo, nonpie_hi, nonpie_bias;
    uint64_t stack_lo, stack_hi;
    int32_t self_gpid, ppid_gpid; // guest identity: this process's pid + its parent's (0 for init's parent)
    int32_t pgid_gpid, sid_gpid;  // guest process group + session (1 == the container init's group/session)
    char exe_path[512];
    // Guest signal-disposition table (g_sigact[65]), captured per process. It is ENGINE C state -- not in
    // the guest RAM dump and not in struct cpu -- so a restored process would otherwise start all-SIG_DFL
    // with DEFAULT host dispositions, and a ^C (SIGINT) at a restored prompt would hit the host default
    // action (terminate) and KILL the shell instead of running its interrupt handler. Carried here and
    // replayed on restore (ckpt_reinstall_sigacts) so async signals route back through the engine handler.
    uint64_t sig_handler[65], sig_flags[65], sig_mask[65];
};

static int ckpt_rd_all(FILE *f, void *buf, size_t n);

static int ckpt_read_region(FILE *file, struct ckpt_region *region) {
    return ckpt_rd_all(file, region, sizeof *region);
}

struct ckpt_fd {
    int32_t gfd, kind, flags, descriptor_flags;
    int64_t offset;
    uint64_t object_id;
    uint64_t ofd_id;
    uint64_t auxiliary;
    char path[512];
};

// `path` arrives as 512 raw image bytes with no guaranteed NUL. Every C-string use (open, snprintf,
// the recovery-journal escaper) would otherwise scan past the record. Terminate at read time.
static void ckpt_fd_terminate(struct ckpt_fd *record) {
    record->path[sizeof record->path - 1] = 0;
}

static void ckpt_fd_terminate_all(struct ckpt_fd *records, size_t count) {
    for (size_t index = 0; index < count; ++index)
        ckpt_fd_terminate(&records[index]);
}

static int ckpt_rd_fd(FILE *file, struct ckpt_fd *record) {
    if (ckpt_rd_all(file, record, sizeof *record) != 0) return -1;
    ckpt_fd_terminate(record);
    return 0;
}

#define CKPT_EPOLL_MAGIC UINT64_C(0x484c45504f4c4c31)

struct ckpt_epoll_header {
    uint64_t magic;
    uint32_t count;
    uint32_t reserved;
};

struct ckpt_epoll_watch {
    int32_t descriptor;
    uint32_t events;
    uint32_t interests;
    uint32_t armed;
    uint64_t data;
};

#define CKPT_SOCKET_QUEUE_MAGIC UINT64_C(0x484c534f434b5131)

struct ckpt_socket_queue_header {
    uint64_t magic;
    uint32_t type;
    uint32_t peer_closed;
};

struct ckpt_socket_queue_frame {
    uint32_t size;
    uint32_t rights_count;
};

#define CKPT_SIGNAL_MAGIC UINT64_C(0x484c5349474e3033)

struct ckpt_signal_state {
    uint64_t magic;
    uint64_t pending;
    uint64_t pending_hi;
    int32_t error[65];
    int32_t code[65];
    int32_t pid[65];
    int32_t uid[65];
    uint32_t queue_count[65];
    uint64_t value[65];
    uint64_t address[65];
    struct sigq_ent queue[65][SIGQ_DEPTH];
};

#define CKPT_FILESYSTEM_MAGIC UINT64_C(0x484c465354415431)

struct ckpt_filesystem_state {
    uint64_t magic;
    char guest_cwd[4200];
    char guest_root[4200];
};

static int ckpt_capture_file_blob(int fd, char *record_path, size_t record_capacity);
static int ckpt_capture_right_resource(int fd, struct ckpt_fd *record);
static void ckpt_release_captured_right(int fd);
static uint64_t ckpt_epoll_identity(int fd);
static int ckpt_dump_epoll(struct ckpt_sink *sink, const char *group, const struct ckpt_fd *records, int count);
static int ckpt_restore_epoll_watches(const char *directory, const struct ckpt_fd *record);
static int ckpt_rd_all(FILE *f, void *buf, size_t n);
static int ckpt_restore_epoll_marker(const struct ckpt_fd *record, uint32_t ordinal);

#include "object_bounds.h"

#define CKPT_EPOLL_WATCH_LIMIT (HL_NFD + EP_PROVIDER_WATCH_LIMIT + EP_OBJECT_WATCH_LIMIT)

static int ckpt_restore_epoll_marker(const struct ckpt_fd *record, uint32_t ordinal) {
    FILE *image_file = ckpt_source_fopen(record->path);
    struct ckpt_epoll_header header;
    if (image_file == NULL || ckpt_rd_all(image_file, &header, sizeof header) != 0 ||
        header.magic != CKPT_EPOLL_MAGIC || header.count > CKPT_EPOLL_WATCH_LIMIT) {
        if (image_file != NULL) ckpt_source_fclose(image_file);
        return -1;
    }
    size_t size = sizeof(uint32_t) + (size_t)header.count * sizeof(struct hl_cmsg_epoll_watch);
    unsigned char *image = calloc(1, size);
    if (image == NULL) {
        ckpt_source_fclose(image_file);
        return -1;
    }
    memcpy(image, &header.count, sizeof header.count);
    for (uint32_t index = 0; index < header.count; ++index) {
        struct ckpt_epoll_watch source;
        if (ckpt_rd_all(image_file, &source, sizeof source) != 0) {
            free(image);
            ckpt_source_fclose(image_file);
            return -1;
        }
        struct hl_cmsg_epoll_watch destination = {
            .descriptor = source.descriptor,
            .events = source.events,
            .armed = source.armed,
            .data = source.data,
        };
        memcpy(image + sizeof(uint32_t) + (size_t)index * sizeof destination, &destination, sizeof destination);
    }
    ckpt_source_fclose(image_file);
    struct hl_cmsg_kqueue_meta metadata = {
        .magic = UINT32_C(0x484c4b51),
        .ordinal = ordinal,
        .source_pid = 0,
        .source_fd = -1,
        .kind = 1,
        .object_id = record->object_id,
        .descriptor_flags = (uint32_t)record->descriptor_flags,
        .canonical_fd = -1,
    };
    int marker = cmsg_kqueue_marker(&metadata);
    metadata.image_size = size;
    if (marker < 0 || pwrite(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
        pwrite(marker, image, size, (off_t)sizeof metadata) != (ssize_t)size || lseek(marker, 0, SEEK_SET) < 0) {
        free(image);
        return -1;
    }
    free(image);
    return marker;
}

static int ckpt_dump_signal_state(struct ckpt_sink *sink, const char *group) {
    struct ckpt_signal_state *state = calloc(1, sizeof *state);
    if (state == NULL) return -1;
    state->magic = CKPT_SIGNAL_MAGIC;
    state->pending = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST);
    state->pending_hi = __atomic_load_n(&g_pending_hi, __ATOMIC_SEQ_CST);
    memcpy(state->error, g_sigerror, sizeof state->error);
    memcpy(state->code, g_sigcode, sizeof state->code);
    memcpy(state->pid, g_sigpid, sizeof state->pid);
    memcpy(state->uid, g_siguid, sizeof state->uid);
    memcpy(state->value, g_sigval, sizeof state->value);
    memcpy(state->address, g_sigaddr, sizeof state->address);
    pthread_mutex_lock(&g_sigq_lk);
    for (int signal = 1; signal <= 64; ++signal) {
        int count = g_sigq[signal].count;
        if (count < 0 || count > SIGQ_DEPTH) {
            pthread_mutex_unlock(&g_sigq_lk);
            free(state);
            return -1;
        }
        state->queue_count[signal] = (uint32_t)count;
        for (int index = 0; index < count; ++index) {
            state->queue[signal][index] = g_sigq[signal].e[(g_sigq[signal].head + index) % SIGQ_DEPTH];
            // A slot number identifies this process's transient signalfd pool.
            // Descriptors and their masks are restored independently, so never
            // serialize that process-local routing cache.
            state->queue[signal][index].signalfd_slots = 0;
        }
    }
    pthread_mutex_unlock(&g_sigq_lk);
    int result = ckpt_sink_put(sink, group, "signals", 0, state, sizeof *state);
    free(state);
    return result;
}

static int ckpt_restore_signal_state(const char *procdir) {
    char path[1300];
    snprintf(path, sizeof path, "%s/signals", procdir);
    struct ckpt_signal_state *state = malloc(sizeof *state);
    if (state == NULL || ckpt_source_load(path, state, sizeof *state) != 0 || state->magic != CKPT_SIGNAL_MAGIC) {
        free(state);
        return -1;
    }
    for (int signal = 1; signal <= 64; ++signal)
        if (state->queue_count[signal] > SIGQ_DEPTH) {
            free(state);
            return -1;
        }
    memcpy(g_sigerror, state->error, sizeof state->error);
    memcpy(g_sigcode, state->code, sizeof state->code);
    memcpy(g_sigpid, state->pid, sizeof state->pid);
    memcpy(g_siguid, state->uid, sizeof state->uid);
    memcpy(g_sigval, state->value, sizeof state->value);
    memcpy(g_sigaddr, state->address, sizeof state->address);
    pthread_mutex_lock(&g_sigq_lk);
    memset(g_sigq, 0, sizeof g_sigq);
    for (int signal = 1; signal <= 64; ++signal) {
        g_sigq[signal].count = (int)state->queue_count[signal];
        for (int index = 0; index < g_sigq[signal].count; ++index) {
            g_sigq[signal].e[index] = state->queue[signal][index];
            g_sigq[signal].e[index].signalfd_slots = 0;
        }
    }
    pthread_mutex_unlock(&g_sigq_lk);
    __atomic_store_n(&g_pending, state->pending, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_pending_hi, state->pending_hi, __ATOMIC_SEQ_CST);
    // Per-thread pending/defer state, including signal 64, is part of each
    // serialized CPU image. The process word above is the only side state.
    // Rebuild host-pipe wake hints from restored descriptors and authoritative
    // pending words; targeted readiness is derived per calling CPU.
    sfd_refresh_all();
    free(state);
    return 0;
}

static int ckpt_dump_filesystem_state(struct ckpt_sink *sink, const char *group) {
    struct ckpt_filesystem_state *state = calloc(1, sizeof *state);
    if (state == NULL) return -1;
    state->magic = CKPT_FILESYSTEM_MAGIC;
    if (g_rootfs) {
        snprintf(state->guest_cwd, sizeof state->guest_cwd, "%s", g_cwd[0] ? g_cwd : "/");
    } else if (getcwd(state->guest_cwd, sizeof state->guest_cwd) == NULL) {
        fprintf(stderr, "[ckpt] refuse: cannot capture cwd: %s\n", strerror(errno));
        free(state);
        return -1;
    }
    snprintf(state->guest_root, sizeof state->guest_root, "%s", g_chroot);
    int result = ckpt_sink_put(sink, group, "filesystem", 0, state, sizeof *state);
    free(state);
    return result;
}

static int ckpt_restore_filesystem_state(const char *procdir) {
    char path[1300], host[4200];
    char previous_root[sizeof g_chroot];
    snprintf(path, sizeof path, "%s/filesystem", procdir);
    struct ckpt_filesystem_state *state = malloc(sizeof *state);
    if (state == NULL || ckpt_source_load(path, state, sizeof *state) != 0 || state->magic != CKPT_FILESYSTEM_MAGIC ||
        memchr(state->guest_cwd, 0, sizeof state->guest_cwd) == NULL ||
        memchr(state->guest_root, 0, sizeof state->guest_root) == NULL || state->guest_cwd[0] != '/' ||
        (state->guest_root[0] != 0 && state->guest_root[0] != '/')) {
        free(state);
        return -1;
    }
    memcpy(previous_root, g_chroot, sizeof previous_root);
    memcpy(g_chroot, state->guest_root, sizeof state->guest_root);
    const char *resolved = atpath(-100, state->guest_cwd, host, sizeof host, 0);
    if (resolved == NULL || chdir(resolved) != 0) {
        // Resolution needs the restored root projection, but failure must not leave init's process-global
        // namespace half-restored while the caller reports the image error. Child restorers exit immediately;
        // init can return through its normal teardown path with the exact prior root still installed.
        memcpy(g_chroot, previous_root, sizeof previous_root);
        fprintf(stderr, "[restore] cannot restore guest cwd %s: %s\n", state->guest_cwd, strerror(errno));
        free(state);
        return -1;
    }
    memcpy(g_cwd, state->guest_cwd, sizeof state->guest_cwd);
    free(state);
    return 0;
}

#define CKPT_SOCKET_STATE_MAGIC UINT64_C(0x484c534f434b5331)

struct ckpt_socket_state {
    uint64_t magic;
    uint32_t guest_family;
    uint32_t host_family;
    uint32_t type;
    uint32_t protocol;
    uint32_t local_size;
    uint32_t listening;
    int32_t backlog;
    int32_t receive_buffer;
    int32_t send_buffer;
    int32_t reuse_address;
    int32_t reuse_port;
    int32_t keepalive;
    int32_t broadcast;
    int32_t lo_port;
    int32_t br_port;
    int32_t br_interface;
    int32_t tcp_local_port;
    uint32_t br_ip;
    uint32_t udp_local_port;
    uint32_t udp_peer_port;
    uint32_t udp_local_ip;
    uint32_t udp_peer_ip;
    uint8_t lo_v6;
    uint8_t lo_v6only;
    uint8_t udp_local_v6;
    uint8_t udp_peer_v6;
    uint8_t udp_local_interface;
    uint8_t udp_peer_interface;
    int32_t pending_error;
    uint8_t shadow_reuse_port;
    uint8_t tcp_local_v6;
    uint8_t reserved_socket_state[2];
    uint32_t tcp_local_address;
    uint8_t tcp_local_address_v6[16];
    int32_t tcp_option_value[TCP_SHADOW_N];
    uint8_t tcp_option_set[TCP_SHADOW_N];
    int32_t ip_option_value[IPOPT_SHADOW_N];
    uint8_t ip_option_set[IPOPT_SHADOW_N];
    struct linger linger;
    struct sockaddr_storage local;
};

// ---- control channel (armed only when HL_CHECKPOINT / HL_RESTORE is set) ----
// The checkpoint request is conveyed by a SHARED-MEMORY generation counter, NOT a signal: a MAP_SHARED
// mmap of an anonymous descriptor activation hands the engine alongside the store channel.
// Every engine process maps it (inherited across fork, remapped after exec). ckpt_poll
// reads it each safepoint (a cheap memory load) and checkpoints when the generation advances past the one it
// last saw. Signals are unusable as the trigger because a guest's own rt_sigaction remaps every guest-
// reachable host signal (bash sets SIG_IGN on SIGUSR1, silently swallowing it). The generation carries the
// INTENT; the host process contract selects the reserved engine interrupt used only to kick a blocked or
// spinning process out to its safepoint (thread_int_handler sets cpu->irq when armed).
// g_ckpt_trigger / g_ckpt_seen_gen live in container/state.c (early include) so signal.c's blocking-syscall
// restart decision (ckpt_pending) can consult them too.

static int ckpt_dump_self(struct cpu *c, const char *group);
static void ckpt_coordinate_and_exit(struct cpu *c);

/* Linux renders an unlinked descriptor as "<path> (deleted)".  A concurrent
 * atomic replacement may already have recreated the pathname, in which case
 * reopening that live path is the only path-backed interpretation available.
 * A genuinely pathless file must fail closed until file-content images are
 * supported; never serialize the procfs annotation as a literal pathname. */
static int ckpt_normalize_reopen_path(char *path) {
    static const char deleted[] = " (deleted)";
    size_t length = strlen(path);
    size_t suffix = sizeof deleted - 1;
    if (length < suffix || strcmp(path + length - suffix, deleted) != 0) return 0;
    path[length - suffix] = '\0';
    return access(path, F_OK) == 0 ? 0 : 1;
}

static volatile uint32_t *ckpt_map_trigger_descriptor(int fd) {
    void *m = mmap(NULL, 4, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (m == MAP_FAILED) fprintf(stderr, "[ckpt] cannot map inherited trigger: %s\n", strerror(errno));
    return (m == MAP_FAILED) ? NULL : (volatile uint32_t *)m;
}

// The generation counter is an anonymous shared descriptor inherited from activation: one shared word, read
// by ckpt_poll at every safepoint, bumped by the embedder to request a capture.
static volatile uint32_t *ckpt_map_trigger(void) {
    int inherited = hl_ckpt_trigger_descriptor();
    if (inherited < 0) {
        fprintf(stderr, "[ckpt] checkpoint requested without a trigger descriptor\n");
        return NULL;
    }
    return ckpt_map_trigger_descriptor(inherited);
}

// A restored child rebuilds its guest address space with MAP_FIXED. It inherited
// the parent's trigger mapping at an address chosen for the parent's layout;
// that address can belong to the child's saved guest image. Detach it before
// replay so MAP_FIXED cannot silently replace engine state, then map the same
// shared descriptor again after the guest topology owns all of its addresses.
static int ckpt_trigger_detach_for_restore(void) {
    if (g_ckpt_trigger == NULL) return 0;
    if (munmap((void *)g_ckpt_trigger, sizeof *g_ckpt_trigger) != 0) return -1;
    g_ckpt_trigger = NULL;
    return 1;
}

static int ckpt_trigger_reattach_after_restore(int detached) {
    if (!detached) return 0;
    if (hl_option_get("HL_CKPT_TEST_FAIL_TRIGGER_REATTACH") != NULL) return -1;
    g_ckpt_trigger = ckpt_map_trigger();
    return g_ckpt_trigger == NULL ? -1 : 0;
}

static int ckpt_rd_all(FILE *f, void *buf, size_t n) {
    return fread(buf, 1, n, f) == n ? 0 : -1;
}

static int ckpt_close_sync(FILE **file) {
    FILE *f = *file;
    if (!f) return 0;
    *file = NULL;
    int failed = fflush(f) != 0 || fsync(fileno(f)) != 0;
    if (fclose(f) != 0) failed = 1;
    return failed ? -1 : 0;
}

static uint64_t ckpt_hash_bytes(uint64_t hash, const void *data, size_t size) {
    const unsigned char *bytes = data;
    for (size_t index = 0; index < size; ++index) {
        hash ^= bytes[index];
        hash *= UINT64_C(1099511628211);
    }
    return hash;
}

static int ckpt_name_compare(const void *left, const void *right) {
    return strcmp(*(const char *const *)left, *(const char *const *)right);
}

// ---------------------------------------------------------------- image digest
//
// The manifest carries a digest that restore recomputes to authenticate the image. It used to be a single
// FNV-1a fold over the whole workspace, in sorted path order, computed by RE-READING every finished file.
// A streaming sink cannot be re-read, so the digest is now two-level:
//
//   per object : h = FNV1a(name '\0' || u64 size || contents)   -- accumulable while the bytes are emitted
//   image      : H = FNV1a over (name '\0' || u64 h) for every object, in ascending name order
//
// The per-object hash is computable by a writer that sees the object exactly once and never again; the image
// hash needs only the (name, h) pairs, which the streaming server accumulates as objects are finished. The
// directory sink computes exactly the same value by walking the workspace, so the two sinks agree on what a
// given image hashes to and neither needs a format flag. MANIFEST and the restore-side RECOVERY.jsonl are
// excluded, as before.
#define CKPT_HASH_BASIS UINT64_C(14695981039346656037)

static uint64_t ckpt_hash_object(uint64_t hash, const char *name, uint64_t size, const void *data, size_t length) {
    hash = ckpt_hash_bytes(hash, name, strlen(name) + 1);
    hash = ckpt_hash_bytes(hash, &size, sizeof size);
    return ckpt_hash_bytes(hash, data, length);
}

static uint64_t ckpt_hash_combine(uint64_t image, const char *name, uint64_t object) {
    image = ckpt_hash_bytes(image, name, strlen(name) + 1);
    return ckpt_hash_bytes(image, &object, sizeof object);
}

// Capture the bytes in flight in a shared anonymous pipe.
//
// The only way to observe a pipe's buffered bytes is to read them, which removes them. That is safe here
// because it is one half of a closed round trip: every byte read lands in the image object
// "pipe.<identity>", and ckpt_prepare_restore_pipes() writes exactly those bytes back into the freshly
// created pipe (after restoring its capacity with F_SETPIPE_SZ) before any guest process is reforked. The
// capture therefore never loses data on a checkpoint that completes, and a capture that cannot complete
// must fail the whole checkpoint rather than publish a short object -- which is why every error path below
// aborts the stream and returns -1 instead of finishing what it has.
//
// Two properties the drain must not damage in the live process, since a checkpoint is not required to be
// the process's last act:
//   - O_NONBLOCK lives on the open file description, so it is shared with every process that inherited this
//     pipe end through fork. Setting it for the drain and leaving it set would turn a blocking guest read()
//     into a spurious EAGAIN afterwards. The original file status flags are restored on every exit path.
//   - the identity is claimed image-wide, so exactly one participant drains a pipe several processes hold.
//
// `reason` receives a short static description of the first failing step; the caller reports it, because a
// bare "cannot capture pipe" hides whether the sink, the descriptor, or the read failed.
static int ckpt_capture_pipe_reason(int fd, uint64_t identity, const char **reason, int *cause) {
    const char *unused_reason = NULL;
    int unused_cause = 0;
    if (!reason) reason = &unused_reason;
    if (!cause) cause = &unused_cause;
    *reason = NULL;
    *cause = 0;
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "pipe.%016llx", (unsigned long long)identity);
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed > 0) return 0; // another process already captured this shared object
    if (claimed < 0) {
        *reason = "sink refused the image-wide claim";
        *cause = errno;
        return -1;
    }
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) {
        *reason = "sink refused to open the pipe object";
        *cause = errno;
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) {
        *reason = "cannot make the pipe end non-blocking for the drain";
        *cause = errno;
        ckpt_sink_abort(sink, &output);
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    unsigned char buffer[65536];
    int failed = 0;
    for (;;) {
        ssize_t count = read(fd, buffer, sizeof buffer);
        if (count > 0) {
            if (ckpt_sink_write(sink, output, buffer, (size_t)count) != 0) {
                // The bytes are already out of the pipe and the object is being discarded, so the image can
                // no longer describe this pipe: fail the checkpoint rather than restore a truncated one.
                *reason = "sink rejected buffered pipe bytes";
                *cause = errno;
                failed = 1;
                break;
            }
            continue;
        }
        if (count == 0 || HL_HOST_ERRNO_WOULD_BLOCK(errno)) break;
        if (errno == EINTR) continue;
        *reason = "read of the buffered pipe bytes failed";
        *cause = errno;
        failed = 1;
        break;
    }
    // Restore the shared open file description exactly as the guest left it, before deciding the outcome.
    int restored = fcntl(fd, F_SETFL, flags);
    if (failed) {
        ckpt_sink_abort(sink, &output);
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    if (restored != 0) {
        *reason = "cannot restore the pipe end's file status flags after the drain";
        *cause = errno;
        ckpt_sink_abort(sink, &output);
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    if (ckpt_sink_finish(sink, &output) != 0) {
        *reason = "sink refused to publish the pipe object";
        *cause = errno;
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    return 0;
}

static int ckpt_capture_pipe(int fd, uint64_t identity) {
    return ckpt_capture_pipe_reason(fd, identity, NULL, NULL);
}

// The restore side recreates the pipe with F_SETPIPE_SZ and refills it, and refuses any capacity it cannot
// parse, so the capacity written into the record must be the live kernel capacity of this pipe rather than
// the engine's cached g_pipesz, which is 0 for a pipe the engine never resized.
static int ckpt_pipe_capacity(int fd) {
    int cached = (fd >= 0 && fd < HL_NFD) ? g_pipesz[fd] : 0;
#ifdef F_GETPIPE_SZ
    int live = fcntl(fd, F_GETPIPE_SZ);
    if (live > 0) return live;
#endif
    return cached;
}

static int ckpt_capture_signalfd(int fd, uint64_t identity) {
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "signalfd.%016llx", (unsigned long long)identity);
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed != 0) return claimed > 0 ? 0 : -1;
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) return -1;
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) {
        ckpt_sink_abort(sink, &output);
        return -1;
    }
    unsigned char buffer[4096];
    int failed = 0;
    for (;;) {
        ssize_t count = read(fd, buffer, sizeof buffer);
        if (count > 0) {
            if (ckpt_sink_write(sink, output, buffer, (size_t)count) != 0) failed = 1;
            if (failed) break;
            continue;
        }
        if (count == 0 || HL_HOST_ERRNO_WOULD_BLOCK(errno)) break;
        if (errno == EINTR) continue;
        failed = 1;
        break;
    }
    if (failed) {
        ckpt_sink_abort(sink, &output);
        return -1;
    }
    return ckpt_sink_finish(sink, &output);
}

static int ckpt_capture_socket_queue(int fd, uint64_t identity, uint32_t type) {
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "socket.%016llx", (unsigned long long)identity);
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed != 0) return claimed > 0 ? 0 : -1;
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) return -1;
    struct ckpt_socket_queue_header header = {CKPT_SOCKET_QUEUE_MAGIC, type, 0};
    if (ckpt_sink_write(sink, output, &header, sizeof header) != 0) goto fail;
    size_t capacity = 1u << 20;
    unsigned char *payload = malloc(capacity);
    if (payload == NULL) goto fail;
    for (;;) {
        unsigned char control[4096];
        struct iovec iov = {payload, capacity};
        struct msghdr message;
        memset(&message, 0, sizeof message);
        message.msg_iov = &iov;
        message.msg_iovlen = 1;
        message.msg_control = control;
        message.msg_controllen = sizeof control;
        ssize_t received = recvmsg(fd, &message, MSG_DONTWAIT);
        if (received < 0 && errno == EINTR) continue;
        if (received < 0 && HL_HOST_ERRNO_WOULD_BLOCK(errno)) break;
        if (received < 0 && errno == ECONNRESET && type != SOCK_STREAM) {
            header.peer_closed = 1;
            break;
        }
        if (received < 0 || (message.msg_flags & (MSG_TRUNC | MSG_CTRUNC)) != 0) {
            fprintf(stderr, "[ckpt] socket queue %016llx recv failed: n=%lld errno=%d flags=%x control=%zu\n",
                    (unsigned long long)identity, (long long)received, errno, message.msg_flags,
                    (size_t)message.msg_controllen);
            free(payload);
            goto fail;
        }
        if (received == 0 && type == SOCK_STREAM) {
            header.peer_closed = 1;
            break;
        }
        struct ckpt_fd rights[253];
        uint32_t nrights = 0;
        for (struct cmsghdr *control_message = CMSG_FIRSTHDR(&message); control_message != NULL;
             control_message = CMSG_NXTHDR(&message, control_message)) {
            if (control_message->cmsg_level != SOL_SOCKET || control_message->cmsg_type != SCM_RIGHTS ||
                control_message->cmsg_len < CMSG_LEN(0)) {
                fprintf(stderr, "[ckpt] socket queue %016llx has unsupported ancillary type\n",
                        (unsigned long long)identity);
                free(payload);
                goto fail;
            }
            size_t bytes = (size_t)control_message->cmsg_len - CMSG_LEN(0);
            int *fds = (int *)CMSG_DATA(control_message);
            int count = (int)(bytes / sizeof(int));
            int visible = cmsg_import_ofd_trailer(fds, count);
            visible = cmsg_import_signalfd_trailer(fds, visible);
            visible = cmsg_import_kqueue_trailer(fds, visible);
            visible = cmsg_import_pipe_trailer(fds, visible);
            visible = cmsg_import_memfd_trailer(fds, visible);
            visible = cmsg_import_timerfd_trailer(fds, visible);
            visible = cmsg_import_eventfd_trailer(fds, visible);
            visible = cmsg_import_seq_trailer(fds, visible);
            if (nrights + (uint32_t)visible > 253) {
                for (int index = 0; index < visible; ++index)
                    close(fds[index]);
                free(payload);
                goto fail;
            }
            for (int index = 0; index < visible; ++index) {
                cmsg_note_recv_sock_fd(fds[index]);
                if (ckpt_capture_right_resource(fds[index], &rights[nrights]) != 0) {
                    fprintf(stderr, "[ckpt] socket queue %016llx has unsupported SCM_RIGHTS fd\n",
                            (unsigned long long)identity);
                    for (int rest = index; rest < visible; ++rest)
                        close(fds[rest]);
                    free(payload);
                    goto fail;
                }
                ckpt_release_captured_right(fds[index]);
                close(fds[index]);
                nrights++;
            }
        }
        struct ckpt_socket_queue_frame frame = {(uint32_t)received, nrights};
        if ((uint64_t)received > UINT32_MAX || ckpt_sink_write(sink, output, &frame, sizeof frame) != 0 ||
            ckpt_sink_write(sink, output, payload, (size_t)received) != 0 ||
            (nrights && ckpt_sink_write(sink, output, rights, (size_t)nrights * sizeof rights[0]) != 0)) {
            free(payload);
            goto fail;
        }
    }
    free(payload);
    // peer_closed is only known after the drain loop: patch the header that was emitted first.
    if (ckpt_sink_write_at(sink, output, 0, &header, sizeof header) != 0) goto fail;
    if (ckpt_sink_finish(sink, &output) != 0) {
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    return 0;
fail:
    ckpt_sink_abort(sink, &output);
    ckpt_sink_unclaim(sink, name);
    return -1;
}

static int ckpt_socket_option_int(int fd, int option, int *value) {
    socklen_t size = sizeof(*value);
    *value = 0;
    return getsockopt(fd, SOL_SOCKET, option, value, &size);
}

static int ckpt_recovery_permissive_requested(void);

static int ckpt_capture_socket_state(int fd, uint64_t identity, int require_quiescent) {
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "socket-state.%016llx", (unsigned long long)identity);
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed != 0) return claimed > 0 ? 0 : -1;
    struct sockaddr_storage peer;
    socklen_t peer_size = sizeof peer;
    int degraded_connection = require_quiescent && fd >= 0 && fd < HL_NFD &&
                              (g_sock_conn[fd] || g_sock_connecting[fd]) &&
                              ckpt_recovery_permissive_requested(); // capture stays strict unless asked
    if (require_quiescent && !degraded_connection && fd >= 0 && fd < HL_NFD &&
        (g_sock_conn[fd] || g_sock_connecting[fd])) {
        fprintf(stderr, "[ckpt] refuse: connected/in-progress socket fd %d requires connection-state transfer\n", fd);
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    if (require_quiescent && !degraded_connection && getpeername(fd, (struct sockaddr *)&peer, &peer_size) == 0) {
        fprintf(stderr, "[ckpt] refuse: connected socket fd %d requires connection-state transfer\n", fd);
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    struct pollfd readiness = {fd, POLLIN, 0};
    if (require_quiescent && !degraded_connection &&
        (poll(&readiness, 1, 0) < 0 || (readiness.revents & (POLLIN | POLLERR | POLLHUP)) != 0)) {
        fprintf(stderr, "[ckpt] refuse: socket fd %d has pending input/accept/error state\n", fd);
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    struct ckpt_socket_state state;
    memset(&state, 0, sizeof state);
    state.magic = CKPT_SOCKET_STATE_MAGIC;
    state.guest_family = g_sock_fam[fd];
    socklen_t type_size = sizeof state.type;
    socklen_t local_size = sizeof state.local;
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &state.type, &type_size) != 0 ||
        getsockname(fd, (struct sockaddr *)&state.local, &local_size) != 0 || local_size > sizeof state.local)
        goto fail;
    state.host_family = state.local.ss_family;
    state.local_size = local_size;
    if (state.guest_family == AF_UNIX && g_unix_bind[fd][0] == '/') {
        struct sockaddr_un *local = (void *)&state.local;
        size_t path_length = strlen(g_unix_bind[fd]);
        if (path_length >= sizeof local->sun_path) goto fail;
        memset(local, 0, sizeof *local);
        local->sun_family = AF_UNIX;
        memcpy(local->sun_path, g_unix_bind[fd], path_length + 1);
        state.host_family = AF_UNIX;
        state.local_size = (uint32_t)(offsetof(struct sockaddr_un, sun_path) + path_length + 1);
    }
    if (state.guest_family == AF_UNIX && state.host_family == 0) {
        state.host_family = AF_UNIX;
#if defined(__APPLE__)
        ((struct sockaddr *)&state.local)->sa_len = (uint8_t)local_size;
#endif
        ((struct sockaddr *)&state.local)->sa_family = AF_UNIX;
    }
    state.protocol = state.type == SOCK_STREAM ? IPPROTO_TCP : state.type == SOCK_DGRAM ? IPPROTO_UDP : 0;
    if (state.host_family == AF_UNIX) state.protocol = 0;
    state.listening = g_tcp_listen[fd] != 0;
    state.backlog = g_sock_backlog[fd];
    state.lo_port = g_lo_port[fd];
    state.lo_v6 = g_lo_v6[fd];
    state.lo_v6only = g_lo_v6only[fd];
    state.br_port = g_br_port[fd];
    state.br_ip = g_br_ip[fd];
    state.br_interface = g_br_interface[fd];
    state.tcp_local_port = g_tcp_lport[fd];
    state.udp_local_port = g_udp_local_port[fd];
    state.udp_peer_port = g_udp_peer_port[fd];
    state.udp_local_ip = g_udp_local_ip[fd];
    state.udp_peer_ip = g_udp_peer_ip[fd];
    state.udp_local_v6 = g_udp_local_v6[fd];
    state.udp_peer_v6 = g_udp_peer_v6[fd];
    state.udp_local_interface = g_udp_local_interface[fd];
    state.udp_peer_interface = g_udp_peer_interface[fd];
    state.pending_error = degraded_connection ? ECONNRESET : g_so_error[fd];
    state.shadow_reuse_port = g_so_reuseport[fd];
    state.tcp_local_address = g_tcp_laddr[fd];
    state.tcp_local_v6 = g_tcp_l6[fd];
    memcpy(state.tcp_local_address_v6, g_tcp_laddr6[fd], sizeof state.tcp_local_address_v6);
    memcpy(state.tcp_option_value, g_tcp_optval[fd], sizeof state.tcp_option_value);
    memcpy(state.tcp_option_set, g_tcp_optset[fd], sizeof state.tcp_option_set);
    memcpy(state.ip_option_value, g_ipopt_val[fd], sizeof state.ip_option_value);
    memcpy(state.ip_option_set, g_ipopt_set[fd], sizeof state.ip_option_set);
    socklen_t linger_size = sizeof state.linger;
    if (ckpt_socket_option_int(fd, SO_RCVBUF, &state.receive_buffer) != 0 ||
        ckpt_socket_option_int(fd, SO_SNDBUF, &state.send_buffer) != 0 ||
        ckpt_socket_option_int(fd, SO_REUSEADDR, &state.reuse_address) != 0 ||
        ckpt_socket_option_int(fd, SO_REUSEPORT, &state.reuse_port) != 0 ||
        ckpt_socket_option_int(fd, SO_KEEPALIVE, &state.keepalive) != 0 ||
        ckpt_socket_option_int(fd, SO_BROADCAST, &state.broadcast) != 0 ||
        getsockopt(fd, SOL_SOCKET, SO_LINGER, &state.linger, &linger_size) != 0)
        goto fail;
    if (ckpt_sink_put(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &state, sizeof state) != 0) goto fail;
    return 0;
fail:
    ckpt_sink_unclaim(sink, name);
    return -1;
}

static int ckpt_capture_file_blob(int fd, char *record_path, size_t record_capacity) {
    static _Atomic uint64_t blob_sequence;
    char destination[1280], temporary[1320];
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) || status.st_size < 0) return -1;
    uint64_t sequence = atomic_fetch_add_explicit(&blob_sequence, 1, memory_order_relaxed) + 1;
    if (snprintf(record_path, record_capacity, "file.%d.%d.%llu", (int)getpid(), fd, (unsigned long long)sequence) >=
        (int)record_capacity)
        return -1;
    struct ckpt_sink *sink = ckpt_sink_current();
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, record_path, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) return -1;
    int input = fd;
#if defined(__linux__)
    int reader = -1;
    int access_mode = fcntl(fd, F_GETFL);
    if (access_mode >= 0 && (access_mode & O_ACCMODE) == O_WRONLY) {
        char descriptor_path[64];
        if (snprintf(descriptor_path, sizeof descriptor_path, "/proc/self/fd/%d", fd) < (int)sizeof descriptor_path)
            reader = open(descriptor_path, O_RDONLY | O_CLOEXEC);
        if (reader >= 0) input = reader;
    }
#endif
    unsigned char buffer[65536];
    off_t offset = 0;
    int failed = 0;
    while (offset < status.st_size) {
        size_t wanted =
            (uint64_t)(status.st_size - offset) < sizeof buffer ? (size_t)(status.st_size - offset) : sizeof buffer;
        ssize_t count = pread(input, buffer, wanted, offset);
        if (count > 0) {
            if (ckpt_sink_write(sink, output, buffer, (size_t)count) != 0) {
                failed = 1;
                break;
            }
            offset += count;
            continue;
        }
        if (count < 0 && errno == EINTR) continue;
        failed = 1;
        break;
    }
    if (failed) {
        ckpt_sink_abort(sink, &output);
#if defined(__linux__)
        if (reader >= 0) close(reader);
#endif
        return -1;
    }
    int result = ckpt_sink_finish(sink, &output);
#if defined(__linux__)
    if (reader >= 0) close(reader);
#endif
    return result;
}

int hl_ckpt_interrupt_executors(void);

// Called at the top of the dispatcher loop (a clean safepoint: all guest arch state is spilled into `c`).
// Referenced by engine/dispatch.c via the G_CKPT_POLL seam (aarch64-only). Cheap: a NULL test + one shared
// memory load on the hot path. When the trigger generation advances, the container INIT coordinates the
// whole tree; a peer dumps only itself. Never returns once it fires (all processes _exit after snapshotting).
static void ckpt_poll(struct cpu *c) {
    if (!g_ckpt_trigger) return;
    uint32_t g = __atomic_load_n(g_ckpt_trigger, __ATOMIC_ACQUIRE);
    if (g == atomic_load_explicit(&g_ckpt_seen_gen, memory_order_acquire)) return;
    // One deterministic coordinator per host process: the thread-group leader owns generation consumption.
    // A peer that observes the trigger returns to translated execution with irq armed by the process kick;
    // the leader will shortly arm the strict barrier and park it at this dispatcher boundary.
    if (c->tid != 0) {
        /* A process-directed host kick may wake any executor. Fan it out from
         * this safe dispatcher context so the leader always consumes the new
         * generation and blocked peers are released as well. Only the first
         * peer to observe a generation may fan it out: recursively signalling
         * every executor on every peer poll creates a self-sustaining signal
         * storm that can starve the leader before it reaches this safepoint. */
        if (atomic_exchange_explicit(&g_ckpt_fanout_gen, g, memory_order_acq_rel) != g)
            (void)hl_ckpt_interrupt_executors();
        return;
    }
    atomic_store_explicit(&g_ckpt_seen_gen, g, memory_order_release);
    if (container_pid() == 1) {
        ckpt_coordinate_and_exit(c); // never returns (dumps the tree + _exit)
    }
    char pd[64];
    snprintf(pd, sizeof pd, "proc.%d", container_pid());
    int rc = ckpt_dump_self(c, pd);
    fprintf(stderr, "[ckpt] proc %d %s\n", container_pid(), rc == 0 ? "OK" : "FAILED");
    _exit(rc == 0 ? 0 : 70);
}

// Export the exact host control signal selected by this unity translation unit. Embedders must not
// reconstruct it from libc's SIGRTMIN: host_signal.h owns a separate Linux signal namespace, and
// repeating the arithmetic outside this translation unit can turn a safepoint kick into termination.
#if G_CKPT_ARCH == 2 || defined(HL_CKPT_INTERRUPT_EXPORT)
void hl_ckpt_interrupt_block(void) {
    sigset_t blocked;
    sigemptyset(&blocked);
    sigaddset(&blocked, THREAD_INT_SIG);
    pthread_sigmask(SIG_BLOCK, &blocked, NULL);
}

int hl_ckpt_interrupt_signal(void) {
    return THREAD_INT_SIG;
}
#endif

// Wake an executor out of either a host syscall or an engine-managed condition wait. The irq store and
// waitc load form the signaler half of the waiter's publish-then-check handshake: if we miss a not-yet-
// published condition, the waiter must observe irq/checkpoint intent before it parks; if we observe it,
// broadcasting under the wait mutex releases an already-published park.
static int ckpt_executor_kick(int slot, int stop_the_world) {
    struct cpu *cpu = g_threg[slot].c;
    pthread_t thread = g_threg[slot].th;
    __atomic_store_n(&cpu->irq, 1, __ATOMIC_SEQ_CST);
    pthread_cond_t *condition = __atomic_load_n(&g_threg[slot].waitc, __ATOMIC_SEQ_CST);
    if (condition) {
        pthread_mutex_t *mutex = g_threg[slot].waitm;
        pthread_mutex_lock(mutex);
        pthread_cond_broadcast(condition);
        pthread_mutex_unlock(mutex);
    }
    if (pthread_kill(thread, THREAD_INT_SIG) != 0) return 0;
    if (stop_the_world) (void)pthread_kill(thread, STW_SIG);
    return 1;
}

int hl_ckpt_interrupt_executors(void) {
    int interrupted = 0;
    int leader_interrupted = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; ++i) {
        if (g_threg[i].c == NULL) continue;
        if (!ckpt_executor_kick(i, 0)) continue;
        /* THREAD_INT_SIG is the activation kick.  Do not queue STW_SIG here:
         * delivery can be delayed until the leader has armed its own checkpoint
         * gate, at which point the gate owner parks in stw_park_handler waiting
         * for itself to release that gate.  ckpt_dump_self() sends STW_SIG only
         * to peer threads after the barrier is armed. */
        interrupted++;
        if (g_threg[i].c->tid == 0) leader_interrupted = 1;
    }
    pthread_mutex_unlock(&g_threg_m);
    return leader_interrupted ? interrupted : 0;
}

// Arm checkpoint/restore if HL_CHECKPOINT / HL_RESTORE is set. Called from engine_global_init (in every
// process, so a forked child is armed too). Maps the shared trigger and records the CURRENT generation as
// already-seen, so a stale trigger from a previous run never false-fires on a fresh launch or a restore
// (only a later increment triggers a checkpoint).
static int ckpt_control_init(void) {
    int restore = hl_option_get("HL_RESTORE") != NULL;
    int capture = hl_option_get("HL_CHECKPOINT") != NULL;
    if (!capture && !restore) return 0;
    thread_int_ensure_installed();
    if (!g_init_hostpid) g_init_hostpid = getpid();
    hl_linux_snapshot_enable(&g_ckpt_snapshot);
    // One channel serves both directions. Restore binds the sink too: the recovery report is an object of
    // the image like any other, and there is nowhere else for it to go.
    if (ckpt_sink_bind_stream() == NULL) {
        fprintf(stderr, "[ckpt] checkpoint requested without a broker descriptor\n");
        return -1;
    }
    if (restore && ckpt_source_current() == NULL && ckpt_source_bind() == NULL) return -1;
    /* Restore requests carry an explicit embedder-issued generation too.  Mapping the trigger here does not
       arm capture by itself: a restore-only launch receives no later bump.  It only prevents restore source
       and RECOVERY.jsonl traffic from falling back to the globally reusable generation zero. */
    g_ckpt_trigger = ckpt_map_trigger();
    if (!g_ckpt_trigger) return -1;
    atomic_store_explicit(&g_ckpt_seen_gen, __atomic_load_n(g_ckpt_trigger, __ATOMIC_ACQUIRE), memory_order_release);
    if (checkpoint_relay_start() != 0) {
        fprintf(stderr, "[ckpt] unable to initialize process checkpoint relay\n");
        return -1;
    }
    return 0;
}

// Is `fd` a GUEST-owned pathless kernel object (socket/pipe/epoll/eventfd/timerfd/inotify/memfd)? Tracked in
// the engine per-fd side-tables. A non-tty, non-regular fd absent from all of them is an ENGINE-internal
// descriptor (a global kqueue, the netns control socket, ...) the guest cannot see -- skipped. A guest-owned
// one is the P3 case ckpt_dump_self refuses cleanly.
static const char *ckpt_guest_kernel_fd(int fd) {
    if (fd < 0 || fd >= HL_NFD) return NULL;
    if (g_epoll[fd]) return "epoll";
    if (g_sock_stream[fd] || g_sock_dgram[fd] || g_sock_seqpacket[fd] || g_dns_sock[fd] || g_sock_fam[fd])
        return "socket";
    if (g_pipesz[fd]) return "pipe";
    if (g_timerfd[fd]) return "timerfd";
    if (g_inotify[fd]) return "inotify";
    if (g_sigfd_slot[fd]) return "signalfd";
    if (g_memfd_is[fd]) return "memfd";
    if (g_eventfd_peer[fd]) return "eventfd";
    return NULL;
}

static uint64_t ckpt_backing_id(const struct stat *status) {
    uint64_t value = ((uint64_t)status->st_dev * UINT64_C(0x9e3779b97f4a7c15)) ^ (uint64_t)status->st_ino;
    value ^= value >> 30;
    value *= UINT64_C(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    return value ? value : 1;
}

static uint64_t ckpt_backing_values(uint64_t device, uint64_t object) {
    struct stat status;
    memset(&status, 0, sizeof status);
    status.st_dev = (dev_t)device;
    status.st_ino = (ino_t)object;
    return ckpt_backing_id(&status);
}

// Determine whether two seekable native descriptors share one open file description. Checkpoint capture
// owns a frozen guest, so temporarily moving the candidate offset is race-free; every offset is restored
// before return. A shared OFD necessarily had equal offsets before the probe.
static int ckpt_same_native_ofd(int first, int second) {
    off_t first_offset = lseek(first, 0, SEEK_CUR);
    off_t second_offset = lseek(second, 0, SEEK_CUR);
    if (first_offset < 0 || second_offset < 0) return 0;
    // Moving one descriptor can only identify a shared open file description when both views began at
    // the same offset. Otherwise an independent descriptor may already sit at the probe value and appear
    // to have followed the move. Besides producing a false identity, that collapses independent offsets
    // and status flags onto one OFD during restore.
    if (first_offset != second_offset) return 0;
    off_t probe = first_offset == 0 ? 1 : 0;
    if (lseek(first, probe, SEEK_SET) != probe) return 0;
    off_t observed = lseek(second, 0, SEEK_CUR);
    int shared = observed == probe;
    if (shared) {
        (void)lseek(first, first_offset, SEEK_SET);
    } else {
        (void)lseek(first, first_offset, SEEK_SET);
        (void)lseek(second, second_offset, SEEK_SET);
    }
    return shared;
}

static uint64_t ckpt_native_ofd_id(const struct ckpt_fd *records, int count, int fd, uint64_t object_id) {
    if (fd >= 0 && fd < HL_NFD && g_ofd_id[fd]) return g_ofd_id[fd];
    for (int i = 0; i < count; i++) {
        if (records[i].object_id != object_id || records[i].gfd < 0 || records[i].ofd_id == 0) continue;
        if (records[i].kind != CKF_FILE && records[i].kind != CKF_BLOB && records[i].kind != CKF_MEMFD) continue;
        if (ckpt_same_native_ofd(records[i].gfd, fd)) return records[i].ofd_id;
    }
    return ofd_identity_ensure(fd) ? g_ofd_id[fd] : UINT64_C(0x4000000000000000) | (uint64_t)(unsigned)(count + 1);
}

static int ckpt_capture_right_resource(int fd, struct ckpt_fd *record) {
    struct stat status;
    hl_host_process_fd detail;
    char path[512];
    size_t path_size = 0;
    memset(record, 0, sizeof *record);
    record->gfd = -1;
    if (g_linux_box != NULL) {
        hl_linux_fd_snapshot snapshot;
        if (hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &snapshot) == HL_STATUS_OK &&
            snapshot.kind == HL_LINUX_OBJECT_INOTIFY) {
            size_t size = 0;
            if (hl_linux_inotify_export(g_linux_box, (hl_linux_fd)fd, NULL, 0, &size) != HL_STATUS_OK || size == 0 ||
                size > 64u * 1024u * 1024u)
                return -1;
            void *image = malloc(size);
            size_t actual = 0;
            record->kind = CKF_INOTIFY;
            record->flags = (int32_t)snapshot.status_flags;
            record->descriptor_flags = (int32_t)snapshot.descriptor_flags;
            record->object_id = UINT64_C(0x9000000000000000) | (uint64_t)snapshot.ofd;
            record->ofd_id = record->object_id;
            snprintf(record->path, sizeof record->path, "inotify-right.%016llx", (unsigned long long)record->object_id);
            if (image == NULL ||
                hl_linux_inotify_export(g_linux_box, (hl_linux_fd)fd, image, size, &actual) != HL_STATUS_OK ||
                actual != size || ckpt_sink_put(ckpt_sink_current(), NULL, record->path, 0, image, size) != 0) {
                free(image);
                return -1;
            }
            free(image);
            return 0;
        }
    }
    (void)memfd_ensure_fd(fd);
    const char *emulated = ckpt_guest_kernel_fd(fd);
    if (emulated && strcmp(emulated, "epoll") == 0) {
        record->kind = CKF_EPOLL;
        record->gfd = fd;
        record->flags = fcntl(fd, F_GETFL);
        record->descriptor_flags = fcntl(fd, F_GETFD);
        record->object_id = ckpt_epoll_identity(fd);
        record->ofd_id = record->object_id;
        snprintf(record->path, sizeof record->path, "epoll-right.%016llx", (unsigned long long)record->object_id);
        if (record->flags < 0 || record->descriptor_flags < 0 || !record->object_id ||
            ckpt_dump_epoll(ckpt_sink_current(), NULL, record, 1) != 0)
            return -1;
        record->gfd = -1;
        return 0;
    }
    if (emulated && strcmp(emulated, "signalfd") == 0) {
        int slot = g_sigfd_slot[fd] - 1;
        uint64_t identity = ofd_identity_ensure(fd);
        if (slot < 0 || slot >= HL_SFD_MAX || !identity) return -1;
        record->kind = CKF_SIGNALFD;
        record->flags = fcntl(fd, F_GETFL);
        record->object_id = identity;
        record->ofd_id = identity;
        record->auxiliary = g_sfd[slot].mask;
        snprintf(record->path, sizeof record->path, "signalfd.%016llx", (unsigned long long)identity);
        if (record->flags < 0 || ckpt_capture_signalfd(fd, identity) != 0) return -1;
        return 0;
    }
    if (emulated && strcmp(emulated, "eventfd") == 0) {
        int slot = eventfd_counter_slot(fd);
        if (slot < 0 || slot >= HL_NFD || !g_eventfd_count) return -1;
        record->kind = CKF_EVENTFD;
        record->flags = eventfd_guest_nb(fd) ? O_NONBLOCK : 0;
        record->object_id = UINT64_C(0x2000000000000000) | (uint64_t)(unsigned)(slot + 1);
        record->ofd_id = record->object_id;
        record->auxiliary = g_eventfd_count[slot];
        record->offset = g_eventfd_sema[fd] ? 1 : 0;
        return 0;
    }
    if (emulated && strcmp(emulated, "timerfd") == 0) {
        int slot = timerfd_slot(fd);
        if (slot < 0 || slot >= HL_NFD) return -1;
        timerfd_object_assign(fd);
        record->kind = CKF_TIMERFD;
        record->flags = g_tfd_nb[fd] ? O_NONBLOCK : 0;
        record->object_id = g_tfd_object[fd];
        record->ofd_id = record->object_id;
        record->offset = g_tfd_deadline[slot];
        record->auxiliary = (uint64_t)g_tfd_interval[slot];
        uint64_t pending = g_tfd_pending[slot];
        struct kevent event;
        struct timespec zero = {0, 0};
        int ready = kevent(fd, NULL, 0, &event, 1, &zero);
        if (ready < 0) return -1;
        if (ready > 0) pending += g_tfd_interval[slot] == 0 ? 1 : (uint64_t)event.data;
        struct timespec captured;
        hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &captured);
        int64_t captured_ns = (int64_t)captured.tv_sec * 1000000000LL + captured.tv_nsec;
        snprintf(record->path, sizeof record->path, "%d %llu %u %lld", g_tfd_clock[slot], (unsigned long long)pending,
                 (unsigned)g_tfd_first_oneshot[slot], (long long)captured_ns);
        return record->object_id ? 0 : -1;
    }
    if (emulated && strcmp(emulated, "memfd") == 0) {
        struct stat status;
        if (fstat(fd, &status) != 0) return -1;
        record->flags = fcntl(fd, F_GETFL);
        record->offset = lseek(fd, 0, SEEK_CUR);
        record->object_id = ckpt_backing_id(&status);
        record->ofd_id = ofd_identity_ensure(fd);
        if (record->flags < 0 || record->offset < 0 || !record->ofd_id ||
            ckpt_capture_file_blob(fd, record->path, sizeof record->path) != 0)
            return -1;
        record->kind = CKF_MEMFD;
        int seals = g_memfd_seal[fd];
        (void)memfd_reg_get_fd(fd, &seals);
        record->auxiliary = (uint64_t)(unsigned)seals;
        return 0;
    }
    if (fd >= 0 && fd < HL_NFD && g_pipe_identity[fd] != 0) {
        int flags = fcntl(fd, F_GETFL);
        if (flags < 0) return -1;
        record->kind = CKF_PIPE;
        record->flags = flags;
        record->object_id = g_pipe_identity[fd];
        record->ofd_id = ofd_identity_ensure(fd);
        record->offset = (int64_t)g_pipe_identity[fd];
        int capacity = ckpt_pipe_capacity(fd);
        if (capacity <= 0) return -1;
        snprintf(record->path, sizeof record->path, "%d", capacity);
        if (!record->ofd_id) return -1;
        if ((flags & O_ACCMODE) == O_RDONLY && ckpt_capture_pipe(fd, g_pipe_identity[fd]) != 0) return -1;
        return 0;
    }
    if (fd < 0 || fstat(fd, &status) != 0 ||
        !hl_host_process_fd_read(getpid(), fd, &detail, path, sizeof path - 1, &path_size) ||
        (detail.flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) != 0 ||
        (!S_ISREG(status.st_mode) && !S_ISDIR(status.st_mode) && !S_ISCHR(status.st_mode) && !S_ISBLK(status.st_mode)))
        return -1;
    record->flags = fcntl(fd, F_GETFL);
    record->object_id = ckpt_backing_id(&status);
    record->ofd_id = ofd_identity_ensure(fd);
    if (record->flags < 0 || !record->ofd_id || path_size >= sizeof path) return -1;
    path[path_size] = '\0';
    if (S_ISCHR(status.st_mode) || S_ISBLK(status.st_mode)) {
        record->kind = CKF_DEVICE;
        record->offset = 0;
        if (path_copy(record->path, sizeof record->path, path) != 0) return -1;
    } else if ((record->offset = lseek(fd, 0, SEEK_CUR)) < 0) {
        return -1;
    } else if (ckpt_normalize_reopen_path(path) != 0 || (S_ISREG(status.st_mode) && access(path, F_OK) != 0)) {
        if (!S_ISREG(status.st_mode) || ckpt_capture_file_blob(fd, record->path, sizeof record->path) != 0) return -1;
        record->kind = CKF_BLOB;
    } else {
        record->kind = CKF_FILE;
        if (S_ISDIR(status.st_mode)) record->auxiliary |= CKFA_DIRECTORY;
        if (path_copy(record->path, sizeof record->path, path) != 0) return -1;
    }
    return 0;
}

static void ckpt_release_captured_right(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    if (g_linux_box != NULL) {
        hl_linux_fd_snapshot snapshot;
        if (hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &snapshot) == HL_STATUS_OK &&
            snapshot.kind == HL_LINUX_OBJECT_INOTIFY) {
            (void)hl_linux_close(g_linux_box, (hl_linux_fd)fd);
            proc_fdvis_close(fd);
        }
    }
    if (g_eventfd_peer[fd]) {
        int slot = eventfd_counter_slot(fd);
        int hidden = g_eventfd_peer[fd] - 1;
        hl_host_process_fd_private_remove(hidden);
        close(hidden);
        if (slot >= 0 && slot < HL_NFD && g_eventfd_refs[slot] > 0) g_eventfd_refs[slot]--;
        g_eventfd_peer[fd] = 0;
        g_eventfd_cslot[fd] = 0;
        g_eventfd_sema[fd] = 0;
        g_eventfd_gnb[fd] = 0;
    }
    if (g_timerfd[fd]) {
        int slot = timerfd_slot(fd);
        if (slot >= 0 && slot < HL_NFD && g_tfd_refs[slot] > 0) g_tfd_refs[slot]--;
        g_timerfd[fd] = 0;
        g_tfd_deadline[fd] = 0;
        g_tfd_interval[fd] = 0;
        g_tfd_pending[fd] = 0;
        g_tfd_clock[fd] = 0;
        g_tfd_first_oneshot[fd] = 0;
        g_tfd_nb[fd] = 0;
        g_tfd_object[fd] = 0;
        g_tfd_shared[fd] = NULL;
        g_tfd_cslot[fd] = 0;
    }
    if (g_epoll[fd]) {
        int slot = epoll_slot(fd);
        ep_native_retire_epoll(slot);
        ep_mem_clear(fd);
        g_epoll[fd] = 0;
        g_ep_dupd[fd] = 0;
        g_ep_cslot[fd] = 0;
    }
    g_memfd_is[fd] = 0;
    g_memfd_seal[fd] = 0;
    g_pipe_identity[fd] = 0;
    g_pipesz[fd] = 0;
    if (g_sigfd_slot[fd]) {
        int slot = g_sigfd_slot[fd] - 1;
        g_sigfd_slot[fd] = 0;
        if (slot >= 0 && slot < HL_SFD_MAX && --g_sfd[slot].refs <= 0) {
            if (g_sfd[slot].wr >= 0) {
                hl_host_process_fd_private_remove(g_sfd[slot].wr);
                close(g_sfd[slot].wr);
            }
            g_sfd[slot] = (struct sfd_ofd){.rd = -1, .wr = -1};
        }
    }
    g_sock_fam[fd] = 0;
    g_sock_stream[fd] = 0;
    g_sock_dgram[fd] = 0;
    g_sock_seqpacket[fd] = 0;
    g_sock_object[fd] = 0;
    g_sock_peer_object[fd] = 0;
    sock_state_drop(fd);
    g_ofd_id[fd] = 0;
}

static uint64_t ckpt_epoll_identity(int fd) {
    if (fd < 0 || fd >= HL_NFD) return 0;
    return UINT64_C(0xa000000000000000) |
           (g_ofd_id[fd] ? (UINT64_C(1) << 32) | g_ofd_id[fd] : (uint64_t)(unsigned)(fd + 1));
}

static void ckpt_interrupt_threads(struct cpu *self) {
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++) {
        struct cpu *peer = g_threg[i].c;
        if (!peer || peer == self) continue;
        (void)ckpt_executor_kick(i, 1);
    }
    pthread_mutex_unlock(&g_threg_m);
}

// A peer's image group is named proc.<GUEST pid> -- what the peer itself uses (ckpt_poll) -- and its guest
// pid equals its host pid only until the first restore. A restored tree keeps its guest pids in g_pidmap
// while every process carries a fresh host pid, so naming the rendezvous group from the host pid made the
// coordinator wait for proc.<new host pid> while the peer had committed proc.<guest pid>: re-capturing a
// restored multi-process tree always refused as an incomplete manifest. Identity outside a restore.
static int ckpt_peer_gpid(int64_t host_pid) {
    int guest;
    return hl_linux_pidmap_guest_checked(&g_pidmap, (int32_t)host_pid, &guest) == 0 ? guest : -1;
}

static int ckpt_is_descendant(int64_t candidate, int64_t root) {
    for (int depth = 0; depth < 512 && candidate > 1; depth++) {
        hl_host_process_info info;
        if (!hl_host_process_read(candidate, &info)) return 0;
        if (info.parent_pid == root) return 1;
        if (info.parent_pid <= 1 || info.parent_pid == candidate) return 0;
        candidate = info.parent_pid;
    }
    return 0;
}

// ================================ CHECKPOINT (per process) ================================

// A duplicate of the container terminal attachment, or -1. The duplicate makes the caller's close operation
// independent of whether the attachment service lent us a scoped handle or the direct launcher supplied a
// native standard descriptor. Never use /dev/tty here: when an embedder itself runs under a terminal that
// path names the embedder's outer terminal, not the private PTY attached to guest descriptor zero.
static int ckpt_ctty_open(void) {
    int descriptor = -1;
    int borrowed = bound_attachment_borrow(STDIN_FILENO, &descriptor);
    if (borrowed >= 0 && isatty(descriptor)) {
        int duplicate = fcntl(descriptor, F_DUPFD_CLOEXEC, STDERR_FILENO + 1);
        if (borrowed > 0) bound_attachment_release(descriptor);
        return duplicate;
    }
    if (borrowed > 0) bound_attachment_release(descriptor);

    pid_t session = getsid(0);
    for (int fd = STDIN_FILENO; fd <= STDERR_FILENO; ++fd)
        if (isatty(fd) && session > 0 && tcgetsid(fd) == session) return fcntl(fd, F_DUPFD_CLOEXEC, STDERR_FILENO + 1);
    errno = ENOTTY;
    return -1;
}

static void ckpt_ctty_close(int fd) {
    if (fd >= 0) (void)close(fd);
}

// Does `path` name the container's controlling terminal?
//
// `isatty(guest_fd)` cannot answer that. The capture runs inside the engine, and the engine's own descriptor
// table is not the guest's: guest fd numbers index the engine's descriptors, which are the pty only where a
// guest fd happens to alias the engine's inherited stdin. An interactive shell's stdin, stdout, stderr and
// its job-control dup (bash's fd 255) therefore looked like ordinary character DEVICES and were recorded by
// host path -- "/dev/pts/7". Restore then reopened that path, which by then names a recycled, unrelated pty
// (or nothing at all, refusing the whole image) instead of inheriting the launcher's terminal.
static int ckpt_path_is_ctty(const char *path) {
    struct stat device, terminal;
    int tf = ckpt_ctty_open();
    int same = tf >= 0 && path != NULL && fstat(tf, &terminal) == 0 && stat(path, &device) == 0 &&
               S_ISCHR(device.st_mode) && S_ISCHR(terminal.st_mode) && device.st_rdev == terminal.st_rdev;
    ckpt_ctty_close(tf);
    return same;
}

// Snapshot every path-backed / tty guest fd into `recs`; REFUSE (return -1) on any GUEST-owned pathless
// kernel-object fd (P3). MUST run BEFORE any checkpoint output file is opened, so the writer's own fds are
// never mistaken for guest fds.
