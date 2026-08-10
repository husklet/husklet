// hl/linux_abi -- native checkpoint/restore ("CRIU-equivalent"), MULTI-PROCESS.
#include "pipe.h"
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

#include "host_fd.h"   // the null-device spelling behind the placeholder descriptors below
#include "host_wait.h" // waitid/waitpid: coordinator peer-reap; multi-thread refusal probe
#include "host_tty.h"  // the controlling terminal's line discipline is captured and replayed

#include "../host/file.h"
#include "../host/system.h"
#include "ckpt_sink_stream.h" // the writer emits every image byte through the sink
#include "ckpt_source.h"      // restore reads the image back through the symmetric source interface
#include "logical_vma.h"

#define CKPT_MAGIC UINT64_C(0x373054504b434c48)          // "HLCKPT07" (LE) -- per-process meta
#define CKPT_MANIFEST_MAGIC UINT64_C(0x3730304e414d4c48) // "HLMAN007" (LE) -- workspace manifest
#define CKPT_VERSION 1                                   // current checkpoint images are advertised and written as v1
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

// Wire values of HL_CHECKPOINT_POLICY (HL_CONFIG_CHECKPOINT_*). Zero means the caller asked for nothing.
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
    uint64_t magic, version, arch, engine_id;
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

struct ckpt_region {
    uint64_t addr, len, glen;
    int32_t prot;   // guest-intent protection (from the anon registry; PROT_READ|WRITE default)
    int32_t is_gna; // 1 if this region is guest-PROT_NONE (rebuild the g_gna EFAULT registry on restore)
    uint64_t npages;
    uint64_t backing_object;
    uint64_t backing_offset;
    uint32_t backing_shared;
    uint32_t backing_emulated;
    uint32_t format_version;
    uint32_t logical;
};

#define CKPT_REGION_VERSION 1

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

#define CKPT_SIGNAL_MAGIC UINT64_C(0x484c5349474e3031)

struct ckpt_signal_state {
    uint64_t magic;
    uint64_t pending;
    int32_t error[65];
    int32_t code[65];
    int32_t pid[65];
    int32_t uid[65];
    uint32_t queue_count[65];
    uint64_t value[65];
    uint64_t address[65];
    struct sigq_ent queue[65][SIGQ_DEPTH];
};

static int ckpt_capture_file_blob(int fd, char *record_path, size_t record_capacity);
static int ckpt_capture_right_resource(int fd, struct ckpt_fd *record);
static void ckpt_release_captured_right(int fd);
static uint64_t ckpt_epoll_identity(int fd);
static int ckpt_dump_epoll(struct ckpt_sink *sink, const char *group, const struct ckpt_fd *records, int count);
static int ckpt_restore_epoll_watches(const char *directory, const struct ckpt_fd *record);
static int ckpt_rd_all(FILE *f, void *buf, size_t n);
static int ckpt_restore_epoll_marker(const struct ckpt_fd *record, uint32_t ordinal);

static int ckpt_restore_epoll_marker(const struct ckpt_fd *record, uint32_t ordinal) {
    FILE *image_file = ckpt_source_fopen(record->path);
    struct ckpt_epoll_header header;
    if (image_file == NULL || ckpt_rd_all(image_file, &header, sizeof header) != 0 ||
        header.magic != CKPT_EPOLL_MAGIC || header.count > HL_NFD + EP_PROVIDER_WATCH_LIMIT + EP_OBJECT_WATCH_LIMIT) {
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
        for (int index = 0; index < count; ++index)
            state->queue[signal][index] = g_sigq[signal].e[(g_sigq[signal].head + index) % SIGQ_DEPTH];
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
        for (int index = 0; index < g_sigq[signal].count; ++index)
            g_sigq[signal].e[index] = state->queue[signal][index];
    }
    pthread_mutex_unlock(&g_sigq_lk);
    __atomic_store_n(&g_pending, state->pending, __ATOMIC_SEQ_CST);
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

static int ckpt_capture_pipe(int fd, uint64_t identity) {
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "pipe.%016llx", (unsigned long long)identity);
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed != 0) return claimed > 0 ? 0 : -1; // another process owns this shared object
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) return -1;
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) {
        ckpt_sink_abort(sink, &output);
        return -1;
    }
    unsigned char buffer[65536];
    int failed = 0;
    for (;;) {
        ssize_t count = read(fd, buffer, sizeof buffer);
        if (count > 0) {
            if (ckpt_sink_write(sink, output, buffer, (size_t)count) != 0) {
                failed = 1;
                break;
            }
            continue;
        }
        if (count == 0 || errno == EAGAIN || errno == EWOULDBLOCK) break;
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
        if (count == 0 || errno == EAGAIN || errno == EWOULDBLOCK) break;
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
        if (received < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) break;
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
    unsigned char buffer[65536];
    off_t offset = 0;
    int failed = 0;
    while (offset < status.st_size) {
        size_t wanted =
            (uint64_t)(status.st_size - offset) < sizeof buffer ? (size_t)(status.st_size - offset) : sizeof buffer;
        ssize_t count = pread(fd, buffer, wanted, offset);
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
        return -1;
    }
    return ckpt_sink_finish(sink, &output);
}

// Called at the top of the dispatcher loop (a clean safepoint: all guest arch state is spilled into `c`).
// Referenced by engine/dispatch.c via the G_CKPT_POLL seam (aarch64-only). Cheap: a NULL test + one shared
// memory load on the hot path. When the trigger generation advances, the container INIT coordinates the
// whole tree; a peer dumps only itself. Never returns once it fires (all processes _exit after snapshotting).
static void ckpt_poll(struct cpu *c) {
    if (!g_ckpt_trigger) return;
    uint32_t g = *g_ckpt_trigger;
    if (g == g_ckpt_seen_gen) return;
    // One deterministic coordinator per host process: the thread-group leader owns generation consumption.
    // A peer that observes the trigger returns to translated execution with irq armed by the process kick;
    // the leader will shortly arm the strict barrier and park it at this dispatcher boundary.
    if (c->tid != 0) return;
    g_ckpt_seen_gen = g;
    if (container_pid() == 1) {
        ckpt_coordinate_and_exit(c); // never returns (dumps the tree + _exit)
    }
    char pd[64];
    snprintf(pd, sizeof pd, "proc.%d", container_pid());
    int rc = ckpt_dump_self(c, pd);
    fprintf(stderr, "[ckpt] proc %d %s\n", container_pid(), rc == 0 ? "OK" : "FAILED");
    _exit(rc == 0 ? 0 : 70);
}

// Arm checkpoint/restore if HL_CHECKPOINT / HL_RESTORE is set. Called from engine_global_init (in every
// process, so a forked child is armed too). Maps the shared trigger and records the CURRENT generation as
// already-seen, so a stale trigger from a previous run never false-fires on a fresh launch or a restore
// (only a later increment triggers a checkpoint).
static int ckpt_control_init(void) {
    int restore = hl_option_get("HL_RESTORE") != NULL;
    int capture = hl_option_get("HL_CHECKPOINT") != NULL;
    if (!capture && !restore) return 0;
    if (!g_init_hostpid) g_init_hostpid = getpid();
    hl_linux_snapshot_enable(&g_ckpt_snapshot);
    // One channel serves both directions. Restore binds the sink too: the recovery report is an object of
    // the image like any other, and there is nowhere else for it to go.
    if (ckpt_sink_bind_stream() == NULL) {
        fprintf(stderr, "[ckpt] checkpoint requested without a broker descriptor\n");
        return -1;
    }
    if (restore && ckpt_source_current() == NULL && ckpt_source_bind() == NULL) return -1;
    if (!capture) return 0;
    g_ckpt_trigger = ckpt_map_trigger();
    if (!g_ckpt_trigger) return -1;
    g_ckpt_seen_gen = *g_ckpt_trigger;
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
        snprintf(record->path, sizeof record->path, "%d", g_pipesz[fd]);
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
        __atomic_store_n(&peer->irq, 1, __ATOMIC_SEQ_CST);
        pthread_cond_t *condition = __atomic_load_n(&g_threg[i].waitc, __ATOMIC_SEQ_CST);
        if (condition) {
            pthread_mutex_t *mutex = g_threg[i].waitm;
            pthread_mutex_lock(mutex);
            pthread_cond_broadcast(condition);
            pthread_mutex_unlock(mutex);
        }
        pthread_kill(g_threg[i].th, THREAD_INT_SIG);
        pthread_kill(g_threg[i].th, STW_SIG);
    }
    pthread_mutex_unlock(&g_threg_m);
}

// A peer's image group is named proc.<GUEST pid> -- what the peer itself uses (ckpt_poll) -- and its guest
// pid equals its host pid only until the first restore. A restored tree keeps its guest pids in g_pidmap
// while every process carries a fresh host pid, so naming the rendezvous group from the host pid made the
// coordinator wait for proc.<new host pid> while the peer had committed proc.<guest pid>: re-capturing a
// restored multi-process tree always refused as an incomplete manifest. Identity outside a restore.
static int ckpt_peer_gpid(int64_t host_pid) {
    return (int)hl_linux_pidmap_guest(&g_pidmap, (int32_t)host_pid);
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

// A descriptor for the container's controlling terminal, or -1. Close it with ckpt_ctty_close.
//
// Probing the engine's own 0/1/2 is not enough: they are the launcher's pty only in the launch shapes that
// leave them inherited. Where the embedder asks for a terminal, the guest's terminal descriptors are hoisted
// into the engine's private descriptor band and its own 0/1/2 are redirected, so every isatty() there says
// "no" and the terminal is invisible to the capture. /dev/tty names the controlling terminal however the
// descriptor table happens to look.
static int ckpt_ctty_open(void) {
    if (isatty(0)) return 0;
    if (isatty(1)) return 1;
    if (isatty(2)) return 2;
    int fd = open("/dev/tty", O_RDWR | O_NOCTTY | O_CLOEXEC);
    if (fd < 0) fd = open("/dev/tty", O_RDONLY | O_NOCTTY | O_CLOEXEC);
    return fd;
}

static void ckpt_ctty_close(int fd) {
    if (fd > STDERR_FILENO) (void)close(fd);
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
static int ckpt_scan_fds(struct ckpt_fd *recs, int cap, int *out_n) {
    static struct fdvis_view views[HL_NFD];
    int n = 0;
    size_t visible = proc_fdvis_list((int)getpid(), NULL, 0);
    if (visible > sizeof views / sizeof views[0] || visible > (size_t)cap) {
        fprintf(stderr, "[ckpt] refuse: %zu guest descriptors exceed checkpoint limit %d\n", visible, cap);
        return -1;
    }
    if (proc_fdvis_list((int)getpid(), views, visible) != visible) {
        fprintf(stderr, "[ckpt] refuse: guest descriptor table changed during checkpoint\n");
        return -1;
    }
    for (size_t index = 0; index < visible; index++) {
        int fd = views[index].guest_fd;
        int already_captured = 0;
        for (int prior = 0; prior < n; ++prior)
            if (recs[prior].gfd == fd) {
                already_captured = 1;
                break;
            }
        if (already_captured) continue;
        hl_linux_fd_snapshot snapshot;
        struct ckpt_fd r;
        memset(&r, 0, sizeof r);
        r.gfd = fd;
        const char *early_emulated = ckpt_guest_kernel_fd(fd);
        if (early_emulated && strcmp(early_emulated, "socket") == 0 && fd >= 0 && fd < HL_NFD &&
            g_sock_object[fd] != 0) {
            if (g_sock_peer_object[fd] == 0) {
                r.kind = CKF_SOCKET;
                r.flags = fcntl(fd, F_GETFL);
                r.descriptor_flags = fcntl(fd, F_GETFD);
                r.object_id = g_sock_object[fd];
                r.ofd_id = r.object_id;
                snprintf(r.path, sizeof r.path, "socket-state.%016llx", (unsigned long long)r.object_id);
                if (r.flags < 0 || r.descriptor_flags < 0 || ckpt_capture_socket_state(fd, r.object_id, 1) != 0)
                    return -1;
                recs[n++] = r;
                continue;
            }
            int type = g_sock_seqpacket[fd] ? SOCK_SEQPACKET : g_sock_dgram[fd] ? SOCK_DGRAM : SOCK_STREAM;
            r.kind = CKF_SOCKETPAIR;
            r.flags = fcntl(fd, F_GETFL);
            r.descriptor_flags = fcntl(fd, F_GETFD);
            r.object_id = g_sock_object[fd];
            r.ofd_id = r.object_id;
            r.auxiliary = g_sock_peer_object[fd];
            r.offset = type;
            snprintf(r.path, sizeof r.path, "socket.%016llx", (unsigned long long)r.object_id);
            if (r.flags < 0 || r.descriptor_flags < 0 || r.auxiliary == 0 ||
                ckpt_capture_socket_state(fd, r.object_id, 0) != 0 ||
                ckpt_capture_socket_queue(fd, r.object_id, (uint32_t)type) != 0)
                return -1;
            recs[n++] = r;
            continue;
        }
        if (early_emulated && strcmp(early_emulated, "epoll") == 0) {
            r.kind = CKF_EPOLL;
            r.descriptor_flags = fcntl(fd, F_GETFD);
            r.object_id = ckpt_epoll_identity(fd);
            r.ofd_id = r.object_id;
            if (r.descriptor_flags < 0 || !r.object_id) return -1;
            snprintf(r.path, sizeof r.path, "epoll.%016llx", (unsigned long long)r.object_id);
            recs[n++] = r;
            continue;
        }
        if (early_emulated && strcmp(early_emulated, "inotify") == 0) {
            inotify_object_assign(fd);
            r.kind = CKF_INOTIFY;
            r.flags = g_inotify_nb[fd] ? O_NONBLOCK : 0;
            r.descriptor_flags = fcntl(fd, F_GETFD);
            r.object_id = g_inotify_object[fd];
            r.ofd_id = r.object_id;
            if (r.descriptor_flags < 0 || !r.object_id) return -1;
            recs[n++] = r;
            continue;
        }
        if (g_linux_box != NULL && hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &snapshot) == HL_STATUS_OK) {
            hl_host_file_metadata metadata;
            if (snapshot.kind == HL_LINUX_OBJECT_INOTIFY) {
                r.kind = CKF_INOTIFY;
                r.flags = (int32_t)snapshot.status_flags;
                r.descriptor_flags = (int32_t)snapshot.descriptor_flags;
                r.object_id = UINT64_C(0x9000000000000000) | (uint64_t)snapshot.ofd;
                r.ofd_id = r.object_id;
                snprintf(r.path, sizeof r.path, "inotify.%016llx", (unsigned long long)r.object_id);
                recs[n++] = r;
                continue;
            }
            if (g_linux_box->host == NULL || g_linux_box->host->file == NULL ||
                g_linux_box->host->file->metadata == NULL ||
                g_linux_box->host->file->metadata(g_linux_box->host->context, snapshot.host_handle, &metadata).status !=
                    HL_STATUS_OK) {
                if (fcntl(fd, F_GETFD) < 0 && errno == EBADF) {
                    proc_fdvis_close(fd);
                    continue;
                }
                fprintf(stderr, "[ckpt] refuse: cannot inspect typed guest fd %d (inotify=%u owner=%d watch=%s)\n", fd,
                        (unsigned)((fd >= 0 && fd < HL_NFD) ? g_inotify[fd] : 0),
                        (fd >= 0 && fd < HL_NFD) ? g_inotify_owner[fd] : 0,
                        (fd >= 0 && fd < HL_NFD && g_inotify_wpath[fd][0]) ? g_inotify_wpath[fd] : "-");
                return -1;
            }
            r.flags = (int32_t)snapshot.status_flags;
            r.descriptor_flags = (int32_t)snapshot.descriptor_flags;
            r.offset = (int64_t)snapshot.offset;
            r.object_id = metadata.stable_object ? metadata.stable_object : (uint64_t)snapshot.host_handle;
            r.ofd_id = UINT64_C(0x8000000000000000) | (uint64_t)snapshot.ofd;
            if (snapshot.kind == HL_LINUX_OBJECT_PIPE || metadata.type == HL_HOST_FILE_TYPE_FIFO) {
                fprintf(stderr, "[ckpt] refuse: guest fd %d is a pipe -- shared pipe restore is not yet supported\n",
                        fd);
                return -1;
            }
            if (metadata.type == HL_HOST_FILE_TYPE_SOCKET) {
                fprintf(stderr, "[ckpt] refuse: guest fd %d is a socket -- socket restore is not yet supported\n", fd);
                return -1;
            }
            if (metadata.type == HL_HOST_FILE_TYPE_CHARACTER || metadata.type == HL_HOST_FILE_TYPE_BLOCK) {
                char fp[512];
                hl_host_result device_path = g_linux_box->host->file->path(
                    g_linux_box->host->context, snapshot.host_handle, (hl_host_bytes){fp, sizeof(fp) - 1});
                if (metadata.type == HL_HOST_FILE_TYPE_CHARACTER && isatty(fd)) {
                    r.kind = CKF_TTY;
                    r.offset = 0;
                } else if (device_path.status == HL_STATUS_OK && device_path.value < sizeof fp) {
                    fp[device_path.value] = 0;
                    if (metadata.type == HL_HOST_FILE_TYPE_CHARACTER && ckpt_path_is_ctty(fp)) {
                        r.kind = CKF_TTY;
                        r.offset = 0;
                    } else {
                        r.kind = CKF_DEVICE;
                        if (path_copy(r.path, sizeof r.path, fp) != 0) return -1;
                    }
                } else {
                    fprintf(stderr, "[ckpt] refuse: device fd %d has no recoverable path\n", fd);
                    return -1;
                }
            } else if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY) {
                char fp[512];
                hl_host_result path = g_linux_box->host->file->path(g_linux_box->host->context, snapshot.host_handle,
                                                                    (hl_host_bytes){fp, sizeof(fp) - 1});
                if (path.status != HL_STATUS_OK || path.value >= sizeof fp) {
                    fprintf(stderr, "[ckpt] refuse: fd %d has no recoverable path\n", fd);
                    return -1;
                }
                fp[path.value] = '\0';
                if (ckpt_normalize_reopen_path(fp) != 0 ||
                    (metadata.type == HL_HOST_FILE_TYPE_REGULAR && access(fp, F_OK) != 0)) {
                    if (metadata.type != HL_HOST_FILE_TYPE_REGULAR ||
                        ckpt_capture_file_blob(fd, r.path, sizeof r.path) != 0) {
                        fprintf(stderr, "[ckpt] refuse: cannot persist deleted fd %d\n", fd);
                        return -1;
                    }
                    r.kind = CKF_BLOB;
                } else {
                    r.kind = CKF_FILE;
                    if (metadata.type == HL_HOST_FILE_TYPE_DIRECTORY) r.auxiliary |= CKFA_DIRECTORY;
                    if (path_copy(r.path, sizeof r.path, fp) != 0) return -1;
                }
            } else {
                fprintf(stderr, "[ckpt] refuse: typed guest fd %d has unsupported type %u\n", fd, metadata.type);
                return -1;
            }
        } else {
            hl_host_process_fd detail;
            char path[512];
            size_t path_size = 0;
            if (!hl_host_process_fd_read(getpid(), fd, &detail, path, sizeof(path) - 1, &path_size) ||
                (detail.flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) != 0) {
                if (fcntl(fd, F_GETFD) < 0 && errno == EBADF) {
                    proc_fdvis_close(fd);
                    continue;
                }
                fprintf(stderr, "[ckpt] refuse: cannot inspect native guest fd %d\n", fd);
                return -1;
            }
            const char *emulated = ckpt_guest_kernel_fd(fd);
            if (emulated && strcmp(emulated, "signalfd") == 0) {
                int slot = g_sigfd_slot[fd] - 1;
                uint64_t identity = ofd_identity_ensure(fd);
                if (slot < 0 || slot >= HL_SFD_MAX || !identity) return -1;
                r.kind = CKF_SIGNALFD;
                r.flags = fcntl(fd, F_GETFL);
                r.descriptor_flags = fcntl(fd, F_GETFD);
                r.object_id = identity;
                r.ofd_id = identity;
                r.auxiliary = g_sfd[slot].mask;
                snprintf(r.path, sizeof r.path, "signalfd.%016llx", (unsigned long long)identity);
                if (r.flags < 0 || r.descriptor_flags < 0 || ckpt_capture_signalfd(fd, identity) != 0) return -1;
                recs[n++] = r;
                continue;
            }
            if (emulated && strcmp(emulated, "eventfd") == 0) {
                int slot = eventfd_counter_slot(fd);
                if (slot < 0 || slot >= HL_NFD || !g_eventfd_count) return -1;
                r.kind = CKF_EVENTFD;
                r.flags = eventfd_guest_nb(fd) ? O_NONBLOCK : 0;
                r.descriptor_flags = fcntl(fd, F_GETFD);
                if (r.descriptor_flags < 0) return -1;
                r.object_id = UINT64_C(0x2000000000000000) | (uint64_t)(unsigned)(slot + 1);
                r.ofd_id = r.object_id;
                r.auxiliary = g_eventfd_count[slot];
                r.offset = g_eventfd_sema[fd] ? 1 : 0;
                recs[n++] = r;
                continue;
            }
            if (emulated && strcmp(emulated, "timerfd") == 0) {
                int slot = timerfd_slot(fd);
                if (slot < 0 || slot >= HL_NFD) return -1;
                timerfd_object_assign(fd);
                r.kind = CKF_TIMERFD;
                r.flags = g_tfd_nb[fd] ? O_NONBLOCK : 0;
                r.descriptor_flags = fcntl(fd, F_GETFD);
                if (r.flags < 0 || r.descriptor_flags < 0 || !g_tfd_object[fd]) return -1;
                r.object_id = g_tfd_object[fd];
                r.ofd_id = r.object_id;
                r.offset = g_tfd_deadline[slot];
                r.auxiliary = (uint64_t)g_tfd_interval[slot];
                uint64_t pending = g_tfd_pending[slot];
                int copied = 0;
                for (int prior = 0; prior < n; prior++)
                    if (recs[prior].kind == CKF_TIMERFD && recs[prior].object_id == r.object_id) {
                        pending = strtoull(recs[prior].path + strcspn(recs[prior].path, " ") + 1, NULL, 10);
                        copied = 1;
                        break;
                    }
                if (!copied) {
                    struct kevent event;
                    struct timespec zero = {0, 0};
                    int ready = kevent(fd, NULL, 0, &event, 1, &zero);
                    if (ready < 0) return -1;
                    if (ready > 0) pending += g_tfd_interval[slot] == 0 ? 1 : (uint64_t)event.data;
                }
                struct timespec captured;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &captured);
                int64_t captured_ns = (int64_t)captured.tv_sec * 1000000000LL + captured.tv_nsec;
                snprintf(r.path, sizeof r.path, "%d %llu %u %lld", g_tfd_clock[slot], (unsigned long long)pending,
                         (unsigned)g_tfd_first_oneshot[slot], (long long)captured_ns);
                recs[n++] = r;
                continue;
            }
            if (emulated && strcmp(emulated, "inotify") == 0) {
                inotify_object_assign(fd);
                r.kind = CKF_INOTIFY;
                r.flags = g_inotify_nb[fd] ? O_NONBLOCK : 0;
                r.descriptor_flags = fcntl(fd, F_GETFD);
                r.object_id = g_inotify_object[fd];
                r.ofd_id = r.object_id;
                if (r.descriptor_flags < 0 || !r.object_id) return -1;
                recs[n++] = r;
                continue;
            }
            if (detail.kind == HL_HOST_FD_PIPE) {
                int flags = fcntl(fd, F_GETFL);
                int descriptor_flags = fcntl(fd, F_GETFD);
                uint64_t identity = views[index].object ? views[index].object : g_pipe_identity[fd];
                if (flags < 0 || descriptor_flags < 0 || identity == 0) return -1;
                if ((flags & O_ACCMODE) != O_WRONLY && ckpt_capture_pipe(fd, identity) != 0) return -1;
                r.kind = CKF_PIPE;
                r.flags = flags;
                r.descriptor_flags = descriptor_flags;
                r.offset = (int64_t)identity;
                snprintf(r.path, sizeof r.path, "%d", (fd >= 0 && fd < HL_NFD) ? g_pipesz[fd] : 0);
                if ((r.flags & O_ACCMODE) == O_RDONLY && ckpt_capture_pipe(fd, identity) != 0) return -1;
                recs[n++] = r;
                continue;
            }
            if (detail.kind == HL_HOST_FD_SOCKET) {
                fprintf(stderr, "[ckpt] refuse: guest fd %d is a socket -- socket restore is not yet supported\n", fd);
                return -1;
            }
            if (emulated && strcmp(emulated, "memfd") == 0) {
                struct stat status;
                if (fstat(fd, &status) != 0) return -1;
                r.flags = fcntl(fd, F_GETFL);
                r.descriptor_flags = fcntl(fd, F_GETFD);
                r.offset = lseek(fd, 0, SEEK_CUR);
                if (r.flags < 0 || r.descriptor_flags < 0 || r.offset < 0 ||
                    ckpt_capture_file_blob(fd, r.path, sizeof r.path) != 0)
                    return -1;
                r.kind = CKF_MEMFD;
                r.object_id = ckpt_backing_id(&status);
                r.ofd_id = ckpt_native_ofd_id(recs, n, fd, r.object_id);
                int seals = g_memfd_seal[fd];
                (void)memfd_reg_get_fd(fd, &seals);
                r.auxiliary = (uint64_t)(unsigned)seals;
                recs[n++] = r;
                continue;
            }
            if (emulated) {
                fprintf(stderr, "[ckpt] refuse: guest fd %d is a %s -- restore is not yet supported\n", fd, emulated);
                return -1;
            }
            struct stat status;
            if (fstat(fd, &status) != 0) return -1;
            r.flags = fcntl(fd, F_GETFL);
            r.descriptor_flags = fcntl(fd, F_GETFD);
            if (r.flags < 0 || r.descriptor_flags < 0) return -1;
            r.offset = lseek(fd, 0, SEEK_CUR);
            r.object_id = ckpt_backing_id(&status);
            r.ofd_id = ckpt_native_ofd_id(recs, n, fd, r.object_id);
            if (S_ISCHR(status.st_mode) && isatty(fd)) {
                r.kind = CKF_TTY;
                r.offset = 0;
            } else if (S_ISCHR(status.st_mode) || S_ISBLK(status.st_mode)) {
                if (path_size >= sizeof path) return -1;
                path[path_size] = '\0';
                if (S_ISCHR(status.st_mode) && ckpt_path_is_ctty(path)) {
                    r.kind = CKF_TTY;
                    r.offset = 0;
                } else {
                    r.kind = CKF_DEVICE;
                    if (path_copy(r.path, sizeof r.path, path) != 0) return -1;
                }
            } else if (S_ISREG(status.st_mode) || S_ISDIR(status.st_mode)) {
                if (path_size >= sizeof path) return -1;
                path[path_size] = '\0';
                if (ckpt_normalize_reopen_path(path) != 0 || (S_ISREG(status.st_mode) && access(path, F_OK) != 0)) {
                    if (!S_ISREG(status.st_mode) || ckpt_capture_file_blob(fd, r.path, sizeof r.path) != 0) {
                        fprintf(stderr, "[ckpt] refuse: cannot persist deleted fd %d\n", fd);
                        return -1;
                    }
                    r.kind = CKF_BLOB;
                } else {
                    r.kind = CKF_FILE;
                    if (S_ISDIR(status.st_mode)) r.auxiliary |= CKFA_DIRECTORY;
                    if (path_copy(r.path, sizeof r.path, path) != 0) return -1;
                }
            } else {
                fprintf(stderr, "[ckpt] refuse: native guest fd %d has unsupported mode %o\n", fd,
                        (unsigned)status.st_mode);
                return -1;
            }
        }
        recs[n++] = r;
    }
    *out_n = n;
    return 0;
}

static uint32_t ckpt_inotify_fflags(uint32_t flags) {
    uint32_t mask = 0;
    if (flags & (NOTE_WRITE | NOTE_EXTEND)) mask |= 0x2;
    if (flags & NOTE_ATTRIB) mask |= 0x4;
    if (flags & NOTE_DELETE) mask |= 0x400;
    if (flags & NOTE_RENAME) mask |= 0x800;
    return mask;
}

static int ckpt_dump_inotify(struct ckpt_sink *sink, const char *group) {
    for (int instance = 0; instance < HL_NFD; instance++) {
        if (!g_inotify[instance]) continue;
#if defined(__linux__)
        int original_flags = fcntl(instance, F_GETFL);
        if (original_flags < 0 || fcntl(instance, F_SETFL, original_flags | O_NONBLOCK) != 0) return -1;
        for (;;) {
            uint8_t buffer[16384];
            ssize_t count = read(instance, buffer, sizeof buffer);
            if (count < 0 && errno == EAGAIN) break;
            if (count < 0) return -1;
            if (!count) break;
            size_t old = g_inotify_raw_len[instance];
            if ((size_t)count > SIZE_MAX - old) return -1;
            uint8_t *grown = realloc(g_inotify_raw[instance], old + (size_t)count);
            if (!grown) return -1;
            g_inotify_raw[instance] = grown;
            memcpy(grown + old, buffer, (size_t)count);
            g_inotify_raw_len[instance] = old + (size_t)count;
        }
        if (fcntl(instance, F_SETFL, original_flags) != 0) return -1;
#else
        for (;;) {
            struct kevent events[32];
            struct timespec zero = {0, 0};
            int count = kevent(instance, NULL, 0, events, 32, &zero);
            if (count < 0) return -1;
            if (!count) break;
            for (int index = 0; index < count; index++) {
                int wd = (int)events[index].ident;
                if (wd >= 0 && wd < HL_NFD && g_inotify_owner[wd] == instance)
                    g_inotify_pending[wd] |= g_inotify_isdir[wd] ? 1u : ckpt_inotify_fflags(events[index].fflags);
            }
        }
#endif
    }
    uint32_t watches = 0, moves = 0, raw_instances = 0;
    for (int wd = 0; wd < HL_NFD; wd++)
        if (g_inotify_owner[wd]) watches++;
    for (int index = 0; index < g_inomv_n; index++) {
        int wd = g_inomv[index].wd;
        if (wd >= 0 && wd < HL_NFD && g_inotify_owner[wd]) moves++;
    }
    for (int instance = 0; instance < HL_NFD; instance++)
        if (g_inotify_raw_len[instance] > g_inotify_raw_pos[instance]) raw_instances++;
    struct ckpt_sink_stream *file = NULL;
    if (ckpt_sink_begin(sink, group, "inotify", 0, &file) != 0) return -1;
    if (ckpt_sink_write(sink, file, &watches, sizeof watches) != 0 ||
        ckpt_sink_write(sink, file, &moves, sizeof moves) != 0 ||
        ckpt_sink_write(sink, file, &raw_instances, sizeof raw_instances) != 0)
        goto fail;
    for (int wd = 0; wd < HL_NFD; wd++) {
        if (!g_inotify_owner[wd]) continue;
        size_t snapshot_size = g_inotify_snap[wd] ? strlen(g_inotify_snap[wd]) + 1 : 0;
        if (snapshot_size > UINT32_MAX) goto fail;
        struct ckpt_inotify_watch watch = {
            .instance = g_inotify_owner[wd],
            .wd = wd,
            .mask = g_inotify_mask[wd],
            .pending = g_inotify_pending[wd],
            .snapshot_size = (uint32_t)snapshot_size,
            .is_directory = g_inotify_isdir[wd],
        };
        memcpy(watch.path, g_inotify_wpath[wd], sizeof watch.path);
        watch.path[sizeof watch.path - 1] = 0;
        if (ckpt_sink_write(sink, file, &watch, sizeof watch) != 0 ||
            (snapshot_size && ckpt_sink_write(sink, file, g_inotify_snap[wd], snapshot_size) != 0))
            goto fail;
    }
    for (int index = 0; index < g_inomv_n; index++) {
        int wd = g_inomv[index].wd;
        if (wd < 0 || wd >= HL_NFD || !g_inotify_owner[wd]) continue;
        struct ckpt_inotify_move move = {
            .wd = wd,
            .mask = g_inomv[index].mask,
            .cookie = g_inomv[index].cookie,
        };
        snprintf(move.name, sizeof move.name, "%s", g_inomv[index].name);
        if (ckpt_sink_write(sink, file, &move, sizeof move) != 0) goto fail;
    }
    for (int instance = 0; instance < HL_NFD; instance++) {
        size_t remaining = g_inotify_raw_len[instance] - g_inotify_raw_pos[instance];
        if (!remaining) continue;
        if (remaining > UINT32_MAX) goto fail;
        struct ckpt_inotify_raw raw = {.instance = instance, .size = (uint32_t)remaining};
        if (ckpt_sink_write(sink, file, &raw, sizeof raw) != 0 ||
            ckpt_sink_write(sink, file, g_inotify_raw[instance] + g_inotify_raw_pos[instance], remaining) != 0)
            goto fail;
    }
    return ckpt_sink_finish(sink, &file);
fail:
    ckpt_sink_abort(sink, &file);
    return -1;
}

static int ckpt_dump_epoll(struct ckpt_sink *sink, const char *group, const struct ckpt_fd *records, int count) {
    for (int record_index = 0; record_index < count; ++record_index) {
        const struct ckpt_fd *record = &records[record_index];
        if (record->kind != CKF_EPOLL) continue;
        int duplicate = 0;
        for (int prior = 0; prior < record_index; ++prior)
            if (records[prior].kind == CKF_EPOLL && records[prior].object_id == record->object_id) duplicate = 1;
        if (duplicate) continue;
        struct ckpt_epoll_watch watches[HL_NFD + EP_PROVIDER_WATCH_LIMIT + EP_OBJECT_WATCH_LIMIT];
        uint32_t used = 0;
        for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
            ep_native_watch *watch = &g_ep_native_watches[index];
            if (__atomic_load_n(&watch->active, __ATOMIC_ACQUIRE) != 1 ||
                ckpt_epoll_identity(watch->epoll) != record->object_id)
                continue;
            watches[used++] = (struct ckpt_epoll_watch){watch->logical_descriptor, watch->events,
                                                        ((watch->events & 1u) ? HL_LINUX_READY_READ : 0u) |
                                                            ((watch->events & 4u) ? HL_LINUX_READY_WRITE : 0u),
                                                        watch->armed, watch->data};
        }
        for (uint32_t index = 0; index < EP_PROVIDER_WATCH_LIMIT; ++index) {
            ep_provider_watch *watch = &g_ep_provider_watches[index];
            if (atomic_load_explicit(&watch->state, memory_order_acquire) != EP_PROVIDER_ACTIVE ||
                ckpt_epoll_identity(watch->epoll) != record->object_id)
                continue;
            watches[used++] = (struct ckpt_epoll_watch){watch->descriptor, watch->events, watch->interests,
                                                        watch->interests != 0 ? 3u : 0u, watch->data};
        }
        for (uint32_t index = 0; index < EP_OBJECT_WATCH_LIMIT; ++index) {
            ep_object_watch *watch = &g_ep_object_watches[index];
            if (atomic_load_explicit(&watch->active, memory_order_acquire) == 0 ||
                ckpt_epoll_identity(watch->epoll) != record->object_id)
                continue;
            watches[used++] =
                (struct ckpt_epoll_watch){watch->descriptor, watch->events, watch->interests, 3u, watch->data};
        }
        size_t bytes = sizeof(struct ckpt_epoll_header) + (size_t)used * sizeof(*watches);
        unsigned char *image = malloc(bytes);
        if (image == NULL) return -1;
        struct ckpt_epoll_header header = {CKPT_EPOLL_MAGIC, used, 0};
        memcpy(image, &header, sizeof header);
        memcpy(image + sizeof header, watches, (size_t)used * sizeof(*watches));
        int result = ckpt_sink_put(sink, group, record->path, 0, image, bytes);
        free(image);
        if (result != 0) return -1;
    }
    return 0;
}

static int ckpt_region_prot(uint64_t addr, uint64_t glen) {
    int p = anon_prot_if_contained(addr, glen ? glen : 1);
    return p >= 0 ? p : (PROT_READ | PROT_WRITE);
}

static int ckpt_logical_descriptor_compare(const void *left, const void *right) {
    const hl_logical_vma_descriptor *a = left;
    const hl_logical_vma_descriptor *b = right;
    if (a->guest_first < b->guest_first) return -1;
    if (a->guest_first > b->guest_first) return 1;
    return 0;
}

static int ckpt_dump_region_bytes(struct ckpt_sink *sink, struct ckpt_sink_stream *f, size_t pagesz,
                                  struct ckpt_region *reg) {
    static uint8_t zero[65536];
    uint8_t *logical_page = reg->logical ? malloc(pagesz) : NULL;
    if (reg->logical && logical_page == NULL) return -1;
    for (uint64_t off = 0; off < reg->len; off += pagesz) {
        uint64_t va = reg->addr + off;
        size_t n = (reg->len - off < pagesz) ? (size_t)(reg->len - off) : pagesz;
        const void *bytes = (const void *)(uintptr_t)va;
        if (reg->logical) {
            if (hl_logical_vma_global_copy_out(va, logical_page, n) != 0) {
                free(logical_page);
                return -1;
            }
            bytes = logical_page;
        } else if (!host_range_mapped((uintptr_t)va, n)) {
            continue;
        }
        if (n <= sizeof zero && memcmp(bytes, zero, n) == 0) continue;
        if (ckpt_sink_write(sink, f, &va, sizeof va) != 0 || ckpt_sink_write(sink, f, bytes, n) != 0) {
            free(logical_page);
            return -1;
        }
        reg->npages++;
    }
    free(logical_page);
    return 0;
}

static int ckpt_write_region(struct ckpt_sink *sink, struct ckpt_sink_stream *stream,
                             const struct ckpt_region *region) {
    return ckpt_sink_write(sink, stream, region, sizeof *region);
}

static int ckpt_write_region_at(struct ckpt_sink *sink, struct ckpt_sink_stream *stream, uint64_t offset,
                                const struct ckpt_region *region) {
    return ckpt_sink_write_at(sink, stream, offset, region, sizeof *region);
}

// Sparse-dump every tracked guest mapping (image/interp/heap/stack/anon/file mmap). Non-zero HOST pages only.
static int ckpt_dump_pages(struct ckpt_sink *sink, struct ckpt_sink_stream *f, size_t pagesz, uint64_t *out_n) {
    uint64_t nreg = 0;
    size_t mapping_count = hl_gmap_count();
    for (size_t i = 0; i < mapping_count; i++) {
        hl_gmap_entry mapping;
        if (!hl_gmap_get(i, &mapping)) continue;
        uint64_t addr = mapping.address, len = mapping.length, glen = mapping.guest_length;
        if (!addr || !len) continue;
        struct ckpt_region reg;
        memset(&reg, 0, sizeof reg);
        reg.format_version = CKPT_REGION_VERSION;
        reg.addr = addr;
        reg.len = len;
        reg.glen = glen;
        reg.prot = ckpt_region_prot(addr, glen);
        // is_gna is a WHOLE-REGION claim (restore gna_adds the whole region), so ask it as one: gna_hit's
        // first-page test is true of every glibc pthread stack guard, which poisoned whole stacks on restore
        // -> -EFAULT in pthread_join's futex -> abort.
        reg.is_gna = gna_all(addr, glen ? glen : 1);
        pthread_mutex_lock(&g_filemap_lock);
        for (int map_index = 0; map_index < g_nfilemap; map_index++) {
            struct guest_file_mapping *filemap = &g_filemap[map_index];
            if (addr < filemap->lo || addr + glen > filemap->hi) continue;
            reg.backing_object = ckpt_backing_values(filemap->device, filemap->inode);
            reg.backing_offset = filemap->offset + (addr - filemap->lo);
            reg.backing_shared = filemap->shared;
            reg.backing_emulated = filemap->emulated;
            break;
        }
        pthread_mutex_unlock(&g_filemap_lock);
        hl_logical_vma_descriptor logical;
        int is_logical = hl_logical_vma_global_describe(addr, &logical);
        if (is_logical < 0) return -1;
        if (is_logical == 1) {
            /*
             * gmap tracks the original mmap while mprotect may split the
             * logical ledger. Emit every descriptor in this gmap separately;
             * the next outer entry (if any) skips descriptors it does not own.
             */
            size_t descriptor_count = hl_logical_vma_global_export(NULL, 0);
            hl_logical_vma_descriptor *descriptors =
                descriptor_count ? malloc(descriptor_count * sizeof(*descriptors)) : NULL;
            if (descriptor_count && descriptors == NULL) return -1;
            if (hl_logical_vma_global_export(descriptors, descriptor_count) != descriptor_count) {
                free(descriptors);
                errno = EAGAIN;
                return -1;
            }
            qsort(descriptors, descriptor_count, sizeof(*descriptors), ckpt_logical_descriptor_compare);
            for (size_t descriptor_index = 0; descriptor_index < descriptor_count; ++descriptor_index) {
                const hl_logical_vma_descriptor *descriptor = &descriptors[descriptor_index];
                if (descriptor->guest_first < addr || descriptor->guest_first >= addr + glen) continue;
                struct ckpt_region logical_region = {0};
                logical_region.addr = descriptor->guest_first;
                logical_region.len = descriptor->length;
                logical_region.glen = descriptor->length;
                logical_region.prot = (int32_t)descriptor->protection;
                logical_region.backing_object = ckpt_backing_values(descriptor->device, descriptor->inode);
                logical_region.backing_offset = descriptor->backing_offset;
                logical_region.backing_shared = 1;
                logical_region.format_version = CKPT_REGION_VERSION;
                logical_region.logical = 1;
                int64_t logical_header = ckpt_sink_tell(sink, f);
                if (logical_header < 0 || ckpt_write_region(sink, f, &logical_region) != 0 ||
                    ckpt_dump_region_bytes(sink, f, pagesz, &logical_region) != 0 ||
                    ckpt_write_region_at(sink, f, (uint64_t)logical_header, &logical_region) != 0) {
                    free(descriptors);
                    return -1;
                }
                nreg++;
            }
            free(descriptors);
            continue;
        }
        int64_t header_offset = ckpt_sink_tell(sink, f);
        if (header_offset < 0) return -1;
        if (ckpt_write_region(sink, f, &reg) != 0) return -1;
        if (ckpt_dump_region_bytes(sink, f, pagesz, &reg) != 0) return -1;
        // Patch the region header in place now that npages is known (the streaming equivalent of the
        // old seek-back-and-rewrite).
        if (ckpt_write_region_at(sink, f, (uint64_t)header_offset, &reg) != 0) return -1;
        nreg++;
    }
    *out_n = nreg;
    return 0;
}

// This process's guest identity (pid / parent / group / session), mapped from host ids to guest space (the
// container init's real host pid/group/session all read back as 1). getppid()==g_init_hostpid means "child
// of init"; a host pgid/sid equal to g_init_hostpid is the container's own group/session (guest 1).
static void ckpt_self_identity(struct ckpt_meta *m) {
    hl_host_process_info process;
    int self = container_pid();
    m->self_gpid = self;
    if (self == 1) {
        m->ppid_gpid = 0;
    } else {
        int pp = getppid();
        m->ppid_gpid = (g_init_hostpid && pp == g_init_hostpid) ? 1 : pp;
    }
    int pg = getpgid(0);
    m->pgid_gpid = (g_init_hostpid && pg == g_init_hostpid) ? 1 : pg;
    int sd = hl_host_process_read(getpid(), &process) ? (int)process.session : getsid(0);
    m->sid_gpid = (g_init_hostpid && sd == g_init_hostpid) ? 1 : sd;
}

// Dump THIS process (RAM + cpu + fds) into `procdir` (temp dir + rename). Returns 0 on success, -1 on any
// failure or P3 refusal (nothing published on failure).
static struct cpu *g_ckpt_cpu_images;
static int g_ckpt_cpu_count;

static int ckpt_dump_self_locked(struct cpu *c, const char *group);

static int ckpt_dump_self(struct cpu *c, const char *procdir) {
    struct cpu *live[THREAD_REG_MAX];
    atomic_store_explicit(&g_ckpt_barrier_active, 1, memory_order_release);
    uint64_t request = stw_checkpoint_arm();
    ckpt_interrupt_threads(c);
    if (stw_checkpoint_wait(request) != 0) {
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    int count = stw_checkpoint_cpus(live, THREAD_REG_MAX);
    if (count < 1 || count > THREAD_REG_MAX) {
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    struct cpu *images = malloc((size_t)count * sizeof *images);
    if (!images) {
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    for (int i = 0; i < count; i++)
        images[i] = *live[i];
    g_ckpt_cpu_images = images;
    g_ckpt_cpu_count = count;
    int result = ckpt_dump_self_locked(c, procdir);
    g_ckpt_cpu_images = NULL;
    g_ckpt_cpu_count = 0;
    free(images);
    atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
    stw_checkpoint_end();
    return result;
}

static int ckpt_dump_self_locked(struct cpu *c, const char *group) {
    if (g_untrusted) {
        fprintf(stderr, "[ckpt] refuse: sentry/untrusted split is P3\n");
        return -1;
    }
    struct ckpt_sink *sink = ckpt_sink_current();
    struct ckpt_fd *fdrecs = calloc(HL_NFD, sizeof *fdrecs);
    int nfd = 0;
    if (fdrecs == NULL || ckpt_scan_fds(fdrecs, HL_NFD, &nfd) != 0) {
        free(fdrecs);
        return -1; // P3 refusal already reported
    }

    // Open this process's image group. The sink stages it; nothing is visible until group_commit.
    fprintf(stderr, "[ckpt] %s: begin (pid %d)\n", group, (int)getpid());
    if (ckpt_sink_group_begin(sink, group) != 0) {
        free(fdrecs);
        return -1;
    }
    struct ckpt_sink_stream *fp = NULL, *ff = NULL;
    int ok = 0;
    size_t pagesz = hl_linux_host_map_granularity();

    struct ckpt_meta m;
    memset(&m, 0, sizeof m);
    m.magic = CKPT_MAGIC;
    m.version = CKPT_VERSION;
    m.arch = G_CKPT_ARCH;
    m.engine_id = pcache_engine_id();
    m.cpu_sz = sizeof(struct cpu);
    m.pagesz = pagesz;
    m.n_threads = (uint64_t)g_ckpt_cpu_count;
    m.brk_lo = brk_lo;
    m.brk_cur = brk_cur;
    m.brk_hi = brk_hi;
    m.nonpie_lo = g_nonpie_lo;
    m.nonpie_hi = g_nonpie_hi;
    m.nonpie_bias = g_nonpie_bias;
    m.stack_lo = g_stack_lo;
    m.stack_hi = g_stack_hi;
    m.n_fds = (uint64_t)nfd;
    ckpt_self_identity(&m);
    snprintf(m.exe_path, sizeof m.exe_path, "%s", g_exe_path ? g_exe_path : "");
    for (int s = 0; s < 65; s++) { // capture this process's guest signal dispositions (restored on thaw)
        m.sig_handler[s] = g_sigact[s].handler;
        m.sig_flags[s] = g_sigact[s].flags;
        m.sig_mask[s] = g_sigact[s].mask;
    }

    if (ckpt_sink_begin(sink, group, "pages", 0, &fp) != 0) goto done;
    if (ckpt_dump_pages(sink, fp, pagesz, &m.n_regions) != 0) goto done;
    if (ckpt_sink_finish(sink, &fp) != 0) goto done;

    {
        size_t payload = (size_t)g_ckpt_cpu_count * sizeof *g_ckpt_cpu_images;
        size_t total = sizeof(struct ckpt_cpu_header) + payload;
        struct ckpt_cpu_header *cpu_file = malloc(total);
        if (!cpu_file) goto done;
        *cpu_file = (struct ckpt_cpu_header){CKPT_CPU_MAGIC, CKPT_VERSION, G_CKPT_ARCH, (uint64_t)g_ckpt_cpu_count,
                                             sizeof(struct cpu)};
        memcpy(cpu_file + 1, g_ckpt_cpu_images, payload);
        int cpu_rc = ckpt_sink_put(sink, group, "cpu", 0, cpu_file, total);
        free(cpu_file);
        if (cpu_rc != 0) goto done;
    }

    if (ckpt_sink_begin(sink, group, "fds", 0, &ff) != 0) goto done;
    for (int i = 0; i < nfd; i++)
        if (ckpt_sink_write(sink, ff, &fdrecs[i], sizeof fdrecs[i]) != 0) goto done;
    if (ckpt_sink_finish(sink, &ff) != 0) goto done;

    for (int i = 0; i < nfd; i++) {
        if (fdrecs[i].kind != CKF_INOTIFY || fdrecs[i].path[0] == 0) continue;
        int duplicate = 0;
        for (int j = 0; j < i; j++)
            if (fdrecs[j].kind == CKF_INOTIFY && fdrecs[j].ofd_id == fdrecs[i].ofd_id) duplicate = 1;
        if (duplicate) continue;
        size_t bytes = 0;
        if (hl_linux_inotify_export(g_linux_box, (hl_linux_fd)fdrecs[i].gfd, NULL, 0, &bytes) != HL_STATUS_OK)
            goto done;
        void *image = malloc(bytes);
        if (image == NULL) goto done;
        size_t actual = 0;
        if (hl_linux_inotify_export(g_linux_box, (hl_linux_fd)fdrecs[i].gfd, image, bytes, &actual) != HL_STATUS_OK ||
            actual != bytes) {
            free(image);
            goto done;
        }
        int stored = ckpt_sink_put(sink, group, fdrecs[i].path, 0, image, bytes);
        free(image);
        if (stored != 0) goto done;
    }

    if (ckpt_dump_epoll(sink, group, fdrecs, nfd) != 0) goto done;
    if (ckpt_dump_inotify(sink, group) != 0) goto done;
    if (ckpt_dump_signal_state(sink, group) != 0) goto done;

    // meta written LAST within the group (it carries the section counts).
    if (ckpt_sink_put(sink, group, "meta", 0, &m, sizeof m) != 0) goto done;
    ok = 1;

done:
    if (fp) ckpt_sink_abort(sink, &fp);
    if (ff) ckpt_sink_abort(sink, &ff);
    free(fdrecs);
    if (!ok) {
        fprintf(stderr, "[ckpt] %s: ABORT -- see the refusal above; nothing from this process is published\n", group);
        ckpt_sink_group_abort(sink, group);
        return -1;
    }
    fprintf(stderr, "[ckpt] %s: commit\n", group);
    return ckpt_sink_group_commit(sink, group);
}

// Enumerate the container's whole process tree = every ENGINE process in the init's session. hl runs each
// guest process as a real host process and the launcher setsid()s the container init, so every guest process
// (even a fork-without-exec bash subshell, even one orphaned to launchd after its parent exited) keeps the
// init's session id. The pid registry is unreliable here (a short-lived fork child inherits + unlinks the
// parent's registry entry on exit), so we scan the session table directly and filter to processes running
// OUR OWN executable -- excluding the launcher and any unrelated session member. The host contract returns
// peers only; native process-table details stay in the backend.
// The container INIT (guest pid 1) coordinates a whole-tree checkpoint at its safepoint: freeze + dump every
// peer, then itself, then publish the MANIFEST. Never returns (_exit frees init's RAM).
static void ckpt_coordinate_and_exit(struct cpu *c) {
    struct ckpt_sink *sink = ckpt_sink_current();

    size_t peer_capacity = 512;
    hl_host_process_peer *foll = malloc(peer_capacity * sizeof *foll);
    size_t observed = 0;
    if (foll == NULL) _exit(70);
    for (;;) {
        if (!hl_host_process_peers(foll, peer_capacity, &observed)) _exit(70);
        if (observed <= peer_capacity) break;
        if (observed > (size_t)INT_MAX || observed > SIZE_MAX / sizeof *foll) _exit(70);
        hl_host_process_peer *expanded = realloc(foll, observed * sizeof *foll);
        if (expanded == NULL) _exit(70);
        foll = expanded;
        peer_capacity = observed;
    }
    int nfoll = (int)observed;
    int descendants = 0;
    for (int i = 0; i < nfoll; i++)
        if (ckpt_is_descendant(foll[i].identity, getpid())) foll[descendants++] = foll[i];
    nfoll = descendants;
    fprintf(stderr, "[ckpt] coordinator pid=%d found %d peer(s)\n", getpid(), nfoll);

    // Freeze + dump every peer: the shared trigger generation is already advanced (the requester bumped it),
    // so KICK each peer with the guest-proof THREAD_INT_SIG to bounce it out of a blocked syscall / chained
    // in-cache loop to its safepoint, where ckpt_poll sees the new generation and dumps proc.<gpid> + _exit()s.
    for (int i = 0; i < nfoll; i++) {
        int kicked = hl_host_process_interrupt(foll[i]);
        fprintf(stderr, "[ckpt] participant %lld %s\n", (long long)foll[i].identity,
                kicked ? "interrupted" : "NOT interrupted (it cannot reach a safepoint)");
    }
    unsigned char *completed = calloc((size_t)(nfoll ? nfoll : 1), 1);
    if (completed == NULL) _exit(70);
    int ndone = 0;
    for (int t = 0; t < 500 && ndone != nfoll; t++) { // one whole-tree deadline: at most ~5s total
        for (int i = 0; i < nfoll; i++) {
            if (completed[i]) continue;
            char pd[64];
            snprintf(pd, sizeof pd, "proc.%d", ckpt_peer_gpid(foll[i].identity));
            // Rendezvous through the sink, not through the store: "that peer finished" is defined as
            // "its group was committed", which is exactly what group_commit means for every implementation.
            if (ckpt_sink_group_present(sink, pd) == 1) {
                completed[i] = 1;
                ndone++;
            }
        }
        int st;
        while (waitpid(-1, &st, WNOHANG) > 0) {} // reap so a peer zombie doesn't linger
        if (ndone != nfoll) usleep(10000);
    }
    if (ndone != nfoll) {
        // Name every participant still outstanding at the rendezvous deadline: "the group never committed"
        // is otherwise indistinguishable from "nothing was ever asked to commit".
        for (int i = 0; i < nfoll; i++)
            if (!completed[i])
                fprintf(stderr,
                        "[ckpt] participant %lld never committed proc.%d (it did not reach a checkpoint "
                        "safepoint, or its dump was refused); refusing incomplete manifest\n",
                        (long long)foll[i].identity, ckpt_peer_gpid(foll[i].identity));
        _exit(70);
    }

    // Dump ourselves (the init) last.
    if (ckpt_dump_self(c, "proc.1") != 0) {
        fprintf(stderr, "[ckpt] init dump FAILED -- checkpoint incomplete\n");
        _exit(70);
    }

    // Publish the MANIFEST last: its presence == a complete, restorable checkpoint.
    int nproc = 0;
    // A descendant can observe the shared generation and publish its image even if the host peer snapshot
    // raced its registration. Do not commit while that independently frozen process is still assembling its
    // group. A fixed quiescence window also covers the registration gap where neither the peer snapshot nor
    // the store contains the child yet.
    for (int settle = 0; settle < 200; settle++) {
        int complete = ckpt_sink_group_count(sink, "proc.");
        if (complete >= 0) nproc = complete;
        usleep(10000);
    }
    if (nproc < nfoll + 1) {
        fprintf(stderr, "[ckpt] process-count mismatch: expected at least %d, captured %d; refusing manifest\n",
                nfoll + 1, nproc);
        _exit(70);
    }
    struct ckpt_manifest man;
    memset(&man, 0, sizeof man);
    man.magic = CKPT_MANIFEST_MAGIC;
    man.version = CKPT_VERSION;
    man.arch = G_CKPT_ARCH;
    man.n_procs = (uint64_t)nproc;
    man.root_gpid = 1;
    // Record which group owns the controlling terminal's foreground (in guest terms). The init is the tty's
    // session leader here, so tcgetpgrp reads the real fg host pgid; child job groups pass through untranslated
    // (guest pgid == host pgid), only the init's own group folds to guest pgid 1.
    {
        int tf = ckpt_ctty_open();
        int fgh = (tf >= 0) ? tcgetpgrp(tf) : -1;
        struct termios tio;
        man.fg_pgid_gpid = (fgh <= 0) ? 0 : (g_init_hostpid && fgh == g_init_hostpid) ? 1 : fgh;
        if (tf >= 0 && tcgetattr(tf, &tio) == 0) {
            size_t cc = sizeof tio.c_cc < sizeof man.tty_cc ? sizeof tio.c_cc : sizeof man.tty_cc;
            man.tty_termios = 1;
            man.tty_iflag = (uint32_t)tio.c_iflag;
            man.tty_oflag = (uint32_t)tio.c_oflag;
            man.tty_cflag = (uint32_t)tio.c_cflag;
            man.tty_lflag = (uint32_t)tio.c_lflag;
            man.tty_ispeed = (uint32_t)cfgetispeed(&tio);
            man.tty_ospeed = (uint32_t)cfgetospeed(&tio);
            memcpy(man.tty_cc, tio.c_cc, cc);
        }
        ckpt_ctty_close(tf);
    }
    // The digest is asked of the sink: the server accumulated it while the bytes went past, so nothing
    // re-reads the embedder's store.
    if (ckpt_sink_digest(sink, &man.image_hash, &man.image_files, &man.image_bytes) != 0) {
        fprintf(stderr, "[ckpt] cannot hash checkpoint image: %s\n", strerror(errno));
        _exit(70);
    }
    // Explicit completion: the only signal that the image is complete.
    if (ckpt_sink_commit(sink, &man, sizeof man) != 0) {
        fprintf(stderr, "[ckpt] cannot publish checkpoint manifest: %s\n", strerror(errno));
        _exit(70);
    }
    fprintf(stderr, "[ckpt] checkpoint OK: %d process(es)\n", nproc);
    int st;
    while (waitpid(-1, &st, WNOHANG) > 0) {} // final reap
    hl_engine_child_result_publish(0, HL_STATUS_OK, 0);
    _exit(0);
}

// ================================= RESTORE =================================

static int ckpt_read_manifest(struct ckpt_manifest *man) {
    if (ckpt_source_load("MANIFEST", man, sizeof *man) != 0) {
        fprintf(stderr, "[restore] the store has no MANIFEST (not a complete checkpoint)\n");
        return -1;
    }
    if (man->magic != CKPT_MANIFEST_MAGIC) {
        fprintf(stderr, "[restore] bad manifest magic\n");
        return -1;
    }
    if (man->version != CKPT_VERSION || man->arch != G_CKPT_ARCH) {
        fprintf(stderr, "[restore] manifest version/arch mismatch\n");
        return -1;
    }
    uint64_t image_hash, image_files, image_bytes;
    if (ckpt_source_digest(&image_hash, &image_files, &image_bytes) != 0 || image_hash != man->image_hash ||
        image_files != man->image_files || image_bytes != man->image_bytes) {
        fprintf(stderr, "[restore] checkpoint image integrity mismatch\n");
        return -1;
    }
    if (man->n_procs == 0 || man->n_procs > 512 || man->root_gpid != 1) {
        fprintf(stderr, "[restore] invalid manifest process count/root\n");
        return -1;
    }
    return 0;
}

static int ckpt_read_meta_dir(const char *procdir, struct ckpt_meta *m) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/meta", procdir);
    if (ckpt_source_load(pf, m, sizeof *m) != 0) {
        fprintf(stderr, "[restore] open %s: %s\n", pf, strerror(errno));
        return -1;
    }
    if (m->magic != CKPT_MAGIC) {
        fprintf(stderr, "[restore] %s is not a checkpoint (bad magic/short read)\n", procdir);
        return -1;
    }
    if (m->version != CKPT_VERSION || m->arch != G_CKPT_ARCH) {
        fprintf(stderr, "[restore] version/arch mismatch (file v%llu arch %llu)\n", (unsigned long long)m->version,
                (unsigned long long)m->arch);
        return -1;
    }
    if (m->cpu_sz != sizeof(struct cpu)) {
        fprintf(stderr, "[restore] cpu-struct size mismatch (file %llu, expected %zu)\n", (unsigned long long)m->cpu_sz,
                sizeof(struct cpu));
        return -1;
    }
    if (m->n_threads < 1 || m->n_threads > THREAD_REG_MAX) {
        fprintf(stderr, "[restore] invalid checkpoint thread count %llu\n", (unsigned long long)m->n_threads);
        return -1;
    }
    return 0;
}

struct ckpt_restore_backing {
    uint64_t object_id;
    int fd;
    int expandable;
};
static struct ckpt_restore_backing *g_restore_backings;
static int g_nrestore_backings;
static int g_restore_backings_capacity;

static int ckpt_vector_reserve(void **items, int *capacity, size_t item_size, int needed) {
    if (needed <= *capacity) return 0;
    int expanded = *capacity > 0 ? *capacity : 64;
    while (expanded < needed) {
        if (expanded > INT_MAX / 2) return -1;
        expanded *= 2;
    }
    if ((size_t)expanded > SIZE_MAX / item_size) return -1;
    void *replacement = realloc(*items, (size_t)expanded * item_size);
    if (replacement == NULL) return -1;
    *items = replacement;
    *capacity = expanded;
    return 0;
}

// Materialize an image object into `destination`. The blob and memfd seeds need a real descriptor, and the
// object only exists in the embedder's store.
static int ckpt_source_copy_to_fd(const char *name, int destination) {
    FILE *source = ckpt_source_fopen(name);
    unsigned char buffer[65536];
    size_t count;
    int failed = 0;
    if (source == NULL) return -1;
    while (!failed && (count = fread(buffer, 1, sizeof buffer, source)) != 0) {
        size_t offset = 0;
        while (offset < count) {
            ssize_t written = write(destination, buffer + offset, count - offset);
            if (written > 0) {
                offset += (size_t)written;
                continue;
            }
            if (written < 0 && errno == EINTR) continue;
            failed = 1;
            break;
        }
    }
    if (ferror(source)) failed = 1;
    ckpt_source_fclose(source);
    return failed ? -1 : 0;
}

static int ckpt_restore_backing_seed(const char *procdir, uint64_t object_id, uint64_t minimum_size) {
    for (int i = 0; i < g_nrestore_backings; i++)
        if (g_restore_backings[i].object_id == object_id) {
            if (g_restore_backings[i].expandable) {
                struct stat status;
                if (minimum_size > (uint64_t)INT64_MAX || fstat(g_restore_backings[i].fd, &status) != 0 ||
                    ((uint64_t)status.st_size < minimum_size &&
                     ftruncate(g_restore_backings[i].fd, (off_t)minimum_size) != 0))
                    return -1;
            }
            return g_restore_backings[i].fd;
        }
    if (ckpt_vector_reserve((void **)&g_restore_backings, &g_restore_backings_capacity, sizeof *g_restore_backings,
                            g_nrestore_backings + 1) != 0)
        return -1;
    char records_path[1300];
    snprintf(records_path, sizeof records_path, "%s/fds", procdir);
    FILE *records = ckpt_source_fopen(records_path);
    if (!records) return -1;
    struct ckpt_fd record;
    int found = 0;
    int expandable = 0;
    while (ckpt_rd_fd(records, &record) == 0)
        if (record.object_id == object_id &&
            (record.kind == CKF_FILE || record.kind == CKF_BLOB || record.kind == CKF_MEMFD)) {
            found = 1;
            break;
        }
    ckpt_source_fclose(records);
    int fd = -1;
    if (!found) {
        /*
         * mmap keeps a vnode alive after its guest descriptor is closed.
         * Such a backing has no fd record, but the sparse page stream still
         * contains every mapped byte needed for restoration.  Recreate a
         * private anonymous seed now; later regions with the same object id
         * reuse it and therefore recover alias topology.
         */
        char temporary[] = "/tmp/.hl-restore-mapXXXXXX";
        fd = mkstemp(temporary);
        if (fd >= 0) unlink(temporary);
        if (fd < 0 || minimum_size > (uint64_t)INT64_MAX || ftruncate(fd, (off_t)minimum_size) != 0) {
            if (fd >= 0) close(fd);
            return -1;
        }
        expandable = 1;
    } else if (record.kind == CKF_FILE) {
        fd = open(record.path, O_RDWR);
        if (fd < 0) fd = open(record.path, O_RDONLY);
    } else {
        char temporary[] = "/tmp/.hl-restore-mapXXXXXX";
        fd = mkstemp(temporary);
        if (fd >= 0) unlink(temporary);
        if (fd < 0 || ckpt_source_copy_to_fd(record.path, fd) != 0 || lseek(fd, 0, SEEK_SET) < 0) {
            if (fd >= 0) close(fd);
            return -1;
        }
    }
    int private_fd = fd >= 0 ? hl_host_process_fd_private_adopt(fd) : -1;
    if (private_fd < 0) {
        if (fd >= 0) close(fd);
        return -1;
    }
    fd = private_fd;
    g_restore_backings[g_nrestore_backings++] = (struct ckpt_restore_backing){object_id, fd, expandable};
    return fd;
}

static int ckpt_restore_backing_find(uint64_t object_id) {
    for (int i = 0; i < g_nrestore_backings; i++)
        if (g_restore_backings[i].object_id == object_id) return g_restore_backings[i].fd;
    return -1;
}

static void ckpt_restore_backings_close(void) {
    for (int i = 0; i < g_nrestore_backings; i++) {
        hl_host_process_fd_private_remove(g_restore_backings[i].fd);
        close(g_restore_backings[i].fd);
    }
    g_nrestore_backings = 0;
}

// Name whatever already holds [lo, hi). Only reached from the collision path below, where "what is in the
// way" is the whole question and a bare address answers none of it.
static void ckpt_report_overlap(uint64_t lo, uint64_t hi) {
    FILE *maps = fopen("/proc/self/maps", "r");
    char line[512];
    if (maps == NULL) return;
    while (fgets(line, sizeof line, maps) != NULL) {
        unsigned long long start = 0, end = 0;
        if (sscanf(line, "%llx-%llx", &start, &end) != 2) continue;
        if (end <= lo || start >= hi) continue;
        fprintf(stderr, "[restore]   in the way: %s", line);
    }
    fclose(maps);
}

// Rebuild this process's guest memory (MAP_FIXED) + the mapping side-registries from `procdir`. For the init
// this runs BEFORE engine init (so MAP_FIXED lands on free VAs); a re-forked child calls hl_gmap_reset() +
// clears the anon/gna counters FIRST (dropping the COW-inherited init mappings) so its own RAM lands clean.
static int ckpt_restore_mem_dir(const char *procdir, const struct ckpt_meta *m) {
    uint64_t *mapped = NULL;
    struct ckpt_region *topology = NULL;
    uint64_t *mapped_a;
    uint64_t *mapped_e;
    size_t nmapped = 0;
    jit_guest_soft_restore_deactivate();
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/pages", procdir);
    FILE *f = ckpt_source_fopen(pf);
    if (!f) {
        fprintf(stderr, "[restore] open %s: %s\n", pf, strerror(errno));
        return -1;
    }
    if (m->n_regions > SIZE_MAX / (2u * sizeof(*mapped))) {
        ckpt_source_fclose(f);
        return -1;
    }
    if (m->n_regions != 0) {
        mapped = calloc((size_t)m->n_regions * 2u, sizeof(*mapped));
        topology = calloc((size_t)m->n_regions, sizeof(*topology));
        if (mapped == NULL || topology == NULL) {
            ckpt_source_fclose(f);
            free(mapped);
            free(topology);
            return -1;
        }
    }
    mapped_a = mapped;
    mapped_e = mapped != NULL ? mapped + (size_t)m->n_regions : NULL;
    for (uint64_t i = 0; i < m->n_regions; i++) {
        struct ckpt_region reg;
        if (ckpt_read_region(f, &reg) != 0) { goto fail; }
        if (reg.format_version != CKPT_REGION_VERSION || reg.logical > 1) {
            fprintf(stderr, "[restore] invalid region format=%u logical=%u\n", reg.format_version, reg.logical);
            goto fail;
        }
        topology[i] = reg;
        uint64_t a = reg.addr, e = reg.addr + reg.len;
        int contained = 0;
        for (size_t j = 0; j < nmapped; j++)
            if (mapped_a[j] <= a && e <= mapped_e[j]) {
                contained = 1;
                break;
            }
        if (reg.logical) {
            if (reg.backing_object == 0 || !reg.backing_shared || reg.backing_emulated) {
                fprintf(stderr, "[restore] invalid logical backing metadata\n");
                goto fail;
            }
            jit_guest_soft_restore_activate();
            uint64_t seed_size = reg.backing_offset + reg.glen;
            if (seed_size < reg.backing_offset) goto fail;
            int seed = ckpt_restore_backing_seed(procdir, reg.backing_object, seed_size);
            if (seed < 0 ||
                hl_logical_vma_global_restore_shared(reg.addr, reg.glen, (uint32_t)reg.prot, seed, reg.backing_offset,
                                                     hl_linux_host_map_granularity()) != 0) {
                fprintf(stderr, "[restore] cannot rebuild logical guest region %llx+%llx: %s\n",
                        (unsigned long long)reg.addr, (unsigned long long)reg.glen, strerror(errno));
                goto fail;
            }
            contained = 1;
        } else if (!contained) {
            int map_flags = MAP_FIXED | MAP_ANON | MAP_PRIVATE;
            int map_fd = -1;
            off_t map_offset = 0;
            if (reg.backing_object != 0 && !reg.backing_emulated) {
                if (reg.backing_offset > UINT64_MAX - reg.len) goto fail;
                map_fd = ckpt_restore_backing_seed(procdir, reg.backing_object, reg.backing_offset + reg.len);
                if (map_fd < 0) {
                    fprintf(stderr, "[restore] cannot prepare backing object %llx\n",
                            (unsigned long long)reg.backing_object);
                    goto fail;
                }
                map_flags = MAP_FIXED | (reg.backing_shared ? MAP_SHARED : MAP_PRIVATE);
                map_offset = (off_t)reg.backing_offset;
            }
            // A guest mmap's VA is an ordinary host mmap result, so a saved region can name VA the restoring
            // process is already using for ENGINE state -- and MAP_FIXED would replace it silently. Probe
            // with MAP_FIXED_NOREPLACE first so the collision is named; the retry keeps the guest's VA (the
            // guest's own pointers are unrelocatable), but a corrupted engine is now diagnosed, not silent.
#ifdef MAP_FIXED_NOREPLACE
            int probe_flags = map_flags | MAP_FIXED_NOREPLACE;
#else
            int probe_flags = map_flags;
#endif
            void *r = mmap((void *)a, (size_t)reg.len, PROT_READ | PROT_WRITE, probe_flags, map_fd, map_offset);
            if (r == MAP_FAILED || (uint64_t)(uintptr_t)r != a) {
                if (r != MAP_FAILED) munmap(r, (size_t)reg.len);
                fprintf(stderr, "[restore] guest region %llx+%llx overlaps a live host mapping; reclaiming it\n",
                        (unsigned long long)a, (unsigned long long)reg.len);
                ckpt_report_overlap(a, e);
                r = mmap((void *)a, (size_t)reg.len, PROT_READ | PROT_WRITE, map_flags, map_fd, map_offset);
            }
            if (r == MAP_FAILED || (uint64_t)(uintptr_t)r != a) {
                fprintf(stderr, "[restore] cannot map guest region %llx+%llx: %s\n", (unsigned long long)a,
                        (unsigned long long)reg.len, strerror(errno));
                goto fail;
            }
            mapped_a[nmapped] = a;
            mapped_e[nmapped] = e;
            nmapped++;
        }
        for (uint64_t p = 0; p < reg.npages; p++) {
            uint64_t va;
            if (ckpt_rd_all(f, &va, sizeof va) != 0) { goto fail; }
            size_t n = (va - reg.addr + m->pagesz > reg.len) ? (size_t)(reg.len - (va - reg.addr)) : (size_t)m->pagesz;
            if (reg.logical) {
                void *page = malloc(n);
                if (page == NULL || ckpt_rd_all(f, page, n) != 0 || hl_logical_vma_global_copy_in(va, page, n) != 0) {
                    fprintf(stderr, "[restore] cannot copy logical guest page %llx+%zx: %s\n", (unsigned long long)va,
                            n, strerror(errno));
                    free(page);
                    goto fail;
                }
                free(page);
            } else if (ckpt_rd_all(f, (void *)va, n) != 0)
                goto fail;
        }
        hl_linux_snapshot_advance(&g_ckpt_snapshot, reg.addr + reg.len);
        hl_gmap_add(reg.addr, reg.len);
        hl_gmap_set_guest_length(reg.addr, reg.glen);
        // ONE verdict per region, so PROT_NONE sub-intervals of a piecewise-mprotect'd region are dropped (a
        // restored guard page reads accessible). Do NOT widen the claim back to any-page: whole-region
        // poisoning is far worse.
        if (reg.is_gna)
            gna_add(reg.addr & ~(uint64_t)0xfff, (reg.addr + reg.glen + 0xfff) & ~(uint64_t)0xfff);
        else
            anon_track(reg.addr, reg.len, reg.prot);
    }
    ckpt_source_fclose(f);
    for (uint64_t i = 0; i < m->n_regions; i++) {
        struct ckpt_region *reg = &topology[i];
        if (reg->backing_object == 0) continue;
        if (reg->backing_offset > UINT64_MAX - reg->glen) {
            free(mapped);
            free(topology);
            return -1;
        }
        int seed = ckpt_restore_backing_seed(procdir, reg->backing_object, reg->backing_offset + reg->glen);
        if (seed < 0) {
            fprintf(stderr, "[restore] cannot rebuild backing object %llx\n", (unsigned long long)reg->backing_object);
            free(mapped);
            free(topology);
            return -1;
        }
        filemap_register(reg->addr, reg->glen, seed, reg->backing_offset, reg->backing_shared, reg->backing_emulated);
        if (reg->backing_shared && !reg->backing_emulated)
            futex_shared_register(reg->addr, reg->glen, seed, reg->backing_offset);
    }
    free(mapped);
    free(topology);
    brk_lo = m->brk_lo;
    brk_cur = m->brk_cur;
    brk_hi = m->brk_hi;
    g_nonpie_lo = m->nonpie_lo;
    g_nonpie_hi = m->nonpie_hi;
    g_nonpie_bias = m->nonpie_bias;
    g_stack_lo = m->stack_lo;
    g_stack_hi = m->stack_hi;
    return 0;
fail:
    ckpt_source_fclose(f);
    free(mapped);
    free(topology);
    return -1;
}

// Reopen this process's own path-backed fds. TTY fds are NOT reopened here -- they are inherited down the
// restore fork from the launcher's pty (init got 0/1/2 from the launcher; each child inherits them).
struct ckpt_restore_pipe {
    uint64_t identity;
    int reader;
    int writer;
    int size;
};
static struct ckpt_restore_pipe *g_restore_pipes;
static int g_nrestore_pipes;
static int g_restore_pipes_capacity;

struct ckpt_restore_eventfd {
    uint64_t identity;
    uint64_t count;
    int reader;
    int writer;
    int slot;
    uint8_t semaphore;
    uint8_t guest_nonblock;
};
static struct ckpt_restore_eventfd *g_restore_eventfds;
static int g_nrestore_eventfds;
static int g_restore_eventfds_capacity;

struct ckpt_restore_timerfd {
    uint64_t identity;
    struct timerfd_shared_state *state;
    int clock_id;
    int fd;
    int slot;
    uint8_t first_oneshot;
};
static struct ckpt_restore_timerfd *g_restore_timerfds;
static int g_nrestore_timerfds;
static int g_restore_timerfds_capacity;

struct ckpt_restore_signalfd {
    uint64_t identity;
    uint64_t mask;
    int reader;
    int writer;
};
static struct ckpt_restore_signalfd *g_restore_signalfds;
static int g_nrestore_signalfds;
static int g_restore_signalfds_capacity;

struct ckpt_restore_socket_endpoint {
    uint64_t identity;
    uint64_t peer_identity;
    int fd;
    int type;
    uint8_t guest_present;
    uint8_t peer_closed;
    uint8_t state_loaded;
    struct ckpt_socket_state state;
};
static struct ckpt_restore_socket_endpoint *g_restore_socket_endpoints;
static int g_nrestore_socket_endpoints;
static int g_restore_socket_endpoints_capacity;

struct ckpt_restore_right {
    uint64_t ofd_id;
    uint64_t object_id;
    int fd;
    uint8_t owned;
};
static struct ckpt_restore_right *g_restore_rights;
static int g_nrestore_rights;
static int g_restore_rights_capacity;

static struct ckpt_restore_right *ckpt_restore_right_find(uint64_t ofd_id) {
    for (int index = 0; index < g_nrestore_rights; ++index)
        if (g_restore_rights[index].ofd_id == ofd_id) return &g_restore_rights[index];
    return NULL;
}

struct ckpt_restore_socket {
    uint64_t identity;
    int fd;
    struct ckpt_socket_state state;
};
static struct ckpt_restore_socket *g_restore_sockets;
static int g_nrestore_sockets;
static int g_restore_sockets_capacity;

static struct ckpt_restore_socket *ckpt_restore_socket_state_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_sockets; ++i)
        if (g_restore_sockets[i].identity == identity) return &g_restore_sockets[i];
    return NULL;
}

static struct ckpt_restore_socket_endpoint *ckpt_restore_socket_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_socket_endpoints; ++i)
        if (g_restore_socket_endpoints[i].identity == identity) return &g_restore_socket_endpoints[i];
    return NULL;
}

static struct ckpt_restore_timerfd *ckpt_restore_timerfd_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_timerfds; i++)
        if (g_restore_timerfds[i].identity == identity) return &g_restore_timerfds[i];
    return NULL;
}

static struct ckpt_restore_signalfd *ckpt_restore_signalfd_find(uint64_t identity) {
    for (int index = 0; index < g_nrestore_signalfds; ++index)
        if (g_restore_signalfds[index].identity == identity) return &g_restore_signalfds[index];
    return NULL;
}

static struct ckpt_restore_eventfd *ckpt_restore_eventfd_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_eventfds; i++)
        if (g_restore_eventfds[i].identity == identity) return &g_restore_eventfds[i];
    return NULL;
}

static struct ckpt_restore_pipe *ckpt_restore_pipe_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_pipes; i++)
        if (g_restore_pipes[i].identity == identity) return &g_restore_pipes[i];
    return NULL;
}

static void ckpt_restore_pipe_seeds_close(void) {
    for (int i = 0; i < g_nrestore_pipes; i++) {
        hl_host_process_fd_private_remove(g_restore_pipes[i].reader);
        hl_host_process_fd_private_remove(g_restore_pipes[i].writer);
        close(g_restore_pipes[i].reader);
        close(g_restore_pipes[i].writer);
    }
}

static void ckpt_restore_eventfd_seeds_close(void) {
    for (int i = 0; i < g_nrestore_eventfds; i++) {
        hl_host_process_fd_private_remove(g_restore_eventfds[i].reader);
        close(g_restore_eventfds[i].reader);
        /* The writer is not a disposable seed: it is the live hidden peer referenced by every restored
         * alias in this process. fd_reset_emul closes it when the process's final alias is released. */
    }
}

static void ckpt_restore_signalfd_seeds_close(void) {
    for (int index = 0; index < g_nrestore_signalfds; ++index) {
        hl_host_process_fd_private_remove(g_restore_signalfds[index].reader);
        hl_host_process_fd_private_remove(g_restore_signalfds[index].writer);
        close(g_restore_signalfds[index].reader);
        close(g_restore_signalfds[index].writer);
    }
}

static void ckpt_restore_socket_seeds_close(void) {
    for (int i = 0; i < g_nrestore_socket_endpoints; ++i) {
        if (g_restore_socket_endpoints[i].fd < 0) continue;
        hl_host_process_fd_private_remove(g_restore_socket_endpoints[i].fd);
        close(g_restore_socket_endpoints[i].fd);
        g_restore_socket_endpoints[i].fd = -1;
    }
    g_nrestore_socket_endpoints = 0;
    for (int i = 0; i < g_nrestore_sockets; ++i) {
        if (g_restore_sockets[i].fd < 0) continue;
        hl_host_process_fd_private_remove(g_restore_sockets[i].fd);
        close(g_restore_sockets[i].fd);
        g_restore_sockets[i].fd = -1;
    }
    g_nrestore_sockets = 0;
    for (int i = 0; i < g_nrestore_rights; ++i) {
        if (g_restore_rights[i].owned == 2) {
            if (g_linux_box != NULL) (void)hl_linux_close(g_linux_box, (hl_linux_fd)g_restore_rights[i].fd);
            proc_fdvis_close(g_restore_rights[i].fd);
            close(g_restore_rights[i].fd);
        } else if (g_restore_rights[i].owned) {
            hl_host_process_fd_private_remove(g_restore_rights[i].fd);
            close(g_restore_rights[i].fd);
        }
    }
    g_nrestore_rights = 0;
}

static int ckpt_restore_file_blob(const char *procdir, const struct ckpt_fd *record) {
    char source_path[1400], temporary[] = "/tmp/hl-checkpoint-file.XXXXXX";
    snprintf(source_path, sizeof source_path, "%s", record->path);
    FILE *source = ckpt_source_fopen(source_path);
    if (!source) return -1;
    int staging = mkstemp(temporary);
    if (staging < 0) {
        ckpt_source_fclose(source);
        return -1;
    }
    unsigned char buffer[65536];
    int failed = 0;
    size_t count;
    while ((count = fread(buffer, 1, sizeof buffer, source)) != 0) {
        size_t offset = 0;
        while (offset < count) {
            ssize_t written = write(staging, buffer + offset, count - offset);
            if (written > 0) {
                offset += (size_t)written;
                continue;
            }
            if (written < 0 && errno == EINTR) continue;
            failed = 1;
            break;
        }
        if (failed) break;
    }
    if (ferror(source)) failed = 1;
    ckpt_source_fclose(source);
    if (!failed && fsync(staging) != 0) failed = 1;
    close(staging);
    if (failed) {
        unlink(temporary);
        return -1;
    }
    int flags = record->flags & ~(O_CREAT | O_EXCL | O_TRUNC);
    int restored = open(temporary, flags);
    unlink(temporary);
    if (restored < 0) return -1;
    if (restored != record->gfd) {
        if (dup2(restored, record->gfd) < 0) {
            close(restored);
            return -1;
        }
        close(restored);
    }
    if (lseek(record->gfd, (off_t)record->offset, SEEK_SET) < 0) return -1;
    if (record->descriptor_flags & FD_CLOEXEC)
        if (fcntl(record->gfd, F_SETFD, FD_CLOEXEC) != 0) return -1;
    return proc_fdvis_publish_native_fd(record->gfd);
}

static int ckpt_restore_epoll_watches(const char *procdir, const struct ckpt_fd *record) {
    char path[1400];
    snprintf(path, sizeof path, "%s/%s", procdir, record->path);
    int64_t stored = ckpt_source_object_size(path);
    if (stored < (int64_t)sizeof(struct ckpt_epoll_header)) return -1;
    size_t size = (size_t)stored;
    unsigned char *image = malloc(size);
    if (image == NULL || ckpt_source_load(path, image, size) != 0) {
        free(image);
        return -1;
    }
    struct ckpt_epoll_header header;
    memcpy(&header, image, sizeof header);
    if (header.magic != CKPT_EPOLL_MAGIC || header.count > (size - sizeof header) / sizeof(struct ckpt_epoll_watch) ||
        sizeof header + (size_t)header.count * sizeof(struct ckpt_epoll_watch) != size) {
        free(image);
        return -1;
    }
    const struct ckpt_epoll_watch *watches = (const void *)(image + sizeof header);
    for (uint32_t index = 0; index < header.count; ++index) {
        const struct ckpt_epoll_watch *saved = &watches[index];
        if (saved->descriptor < 0 || saved->descriptor >= HL_NFD || fcntl(saved->descriptor, F_GETFD) < 0) {
            free(image);
            return -1;
        }
        hl_linux_fd_snapshot snapshot;
        int typed = g_linux_box != NULL &&
                    hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)saved->descriptor, &snapshot) == HL_STATUS_OK;
        if (typed && hl_provider_files_is_handle(snapshot.host_handle)) {
            ep_provider_watch *watch = ep_provider_alloc(g_ep_provider_watches, EP_PROVIDER_WATCH_LIMIT);
            if (watch == NULL) {
                free(image);
                return -1;
            }
            uint32_t serial = g_ep_provider_serial = ep_provider_next(g_ep_provider_serial);
            ep_provider_activate(watch, record->gfd, g_ep_provider_generations[record->gfd], saved->descriptor,
                                 snapshot.descriptor_generation, serial, snapshot.host_handle, saved->events,
                                 saved->interests, saved->data);
            if (saved->interests != 0 &&
                hl_provider_files_subscribe(snapshot.host_handle, saved->interests, bound_epoll_provider_ready, watch,
                                            atomic_load(&watch->serial)) != 0) {
                ep_provider_reservation_cancel(watch);
                free(image);
                return -1;
            }
            continue;
        }
        if (typed) {
            hl_linux_object_pin pin;
            int object_ready = 0;
            if (hl_linux_object_pin_fd(g_linux_box, (hl_linux_fd)saved->descriptor, &pin) == HL_STATUS_OK) {
                object_ready = pin.ops != NULL && pin.ops->readiness != NULL;
                hl_linux_object_unpin(&pin);
            }
            if (object_ready) {
                ep_object_watch *watch = ep_object_alloc();
                if (watch == NULL) {
                    free(image);
                    return -1;
                }
                watch->epoll = record->gfd;
                watch->epoll_generation = g_ep_provider_generations[record->gfd];
                watch->descriptor = saved->descriptor;
                watch->descriptor_generation = snapshot.descriptor_generation;
                watch->events = saved->events;
                watch->interests = saved->interests;
                watch->data = saved->data;
                g_ep_object_count[record->gfd]++;
                continue;
            }
        }
        struct kevent changes[2];
        int change_count = 0;
        uint16_t flags = (uint16_t)((saved->events & UINT32_C(0x80000000) ? EV_CLEAR : 0) |
                                    (saved->events & UINT32_C(0x40000000) ? EV_ONESHOT : 0));
        if ((saved->armed & 1u) != 0) {
            EV_SET(&changes[change_count++], saved->descriptor, EVFILT_READ, EV_ADD | flags, 0, 0,
                   (void *)(uintptr_t)saved->data);
        }
        if ((saved->armed & 2u) != 0) {
            EV_SET(&changes[change_count++], saved->descriptor, EVFILT_WRITE, EV_ADD | flags, 0, 0,
                   (void *)(uintptr_t)saved->data);
        }
        if (change_count != 0 && kevent(record->gfd, changes, change_count, NULL, 0, NULL) < 0) {
            free(image);
            return -1;
        }
        ep_mem_set(record->gfd, saved->descriptor, 1);
        g_ep_owner[saved->descriptor] = record->gfd + 1;
        g_ep_events[saved->descriptor] = saved->events;
        g_ep_udata[saved->descriptor] = saved->data;
        g_ep_rd[saved->descriptor] = (saved->armed & 1u) != 0;
        g_ep_wr[saved->descriptor] = (saved->armed & 2u) != 0;
        g_ep_os[saved->descriptor] = (saved->events & UINT32_C(0x40000000)) != 0;
        if (ep_native_set(record->gfd, saved->descriptor, 3, saved->events, saved->data) != 0) {
            free(image);
            return -1;
        }
        ep_native_watch *native = ep_native_find(record->gfd, saved->descriptor);
        if (native) native->armed = saved->armed;
    }
    ep_wake_arm(record->gfd);
    free(image);
    return 0;
}

static int ckpt_restore_inotify_sidecar(const char *procdir) {
    char path[1300];
    snprintf(path, sizeof path, "%s/inotify", procdir);
    FILE *file = ckpt_source_fopen(path);
    if (!file) return errno == ENOENT ? 0 : -1;
    uint32_t watches = 0, moves = 0, raw_instances = 0;
    if (ckpt_rd_all(file, &watches, sizeof watches) != 0 || ckpt_rd_all(file, &moves, sizeof moves) != 0 ||
        ckpt_rd_all(file, &raw_instances, sizeof raw_instances) != 0 || watches > HL_NFD ||
        moves > (uint32_t)(sizeof g_inomv / sizeof g_inomv[0]) || raw_instances > HL_NFD)
        goto fail;
    for (uint32_t index = 0; index < watches; index++) {
        struct ckpt_inotify_watch watch;
        if (ckpt_rd_all(file, &watch, sizeof watch) != 0 || watch.instance < 0 || watch.instance >= HL_NFD ||
            watch.wd < 0 || watch.wd >= HL_NFD || !g_inotify[watch.instance] || !watch.path[0] ||
            watch.snapshot_size > 16 * 1024 * 1024u)
            goto fail;
        char *snapshot = NULL;
        if (watch.snapshot_size) {
            snapshot = malloc(watch.snapshot_size);
            if (!snapshot || ckpt_rd_all(file, snapshot, watch.snapshot_size) != 0 ||
                snapshot[watch.snapshot_size - 1] != '\0') {
                free(snapshot);
                goto fail;
            }
        }
#if defined(__linux__)
        int restored_wd = inotify_add_watch(watch.instance, watch.path, watch.mask);
        if (restored_wd != watch.wd) {
            free(snapshot);
            goto fail;
        }
#else
        int opened = hl_native_open_watch(watch.path);
        if (opened < 0) {
            free(snapshot);
            goto fail;
        }
        engine_fd_vacate(watch.wd);
        if (opened != watch.wd) {
            if (dup2(opened, watch.wd) < 0) {
                close(opened);
                free(snapshot);
                goto fail;
            }
            close(opened);
        }
        struct kevent event;
        EV_SET(&event, watch.wd, EVFILT_VNODE, EV_ADD | EV_CLEAR,
               NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND, 0, (void *)(intptr_t)watch.wd);
        if (kevent(watch.instance, &event, 1, NULL, 0, NULL) < 0) {
            close(watch.wd);
            free(snapshot);
            goto fail;
        }
#endif
        g_inotify_owner[watch.wd] = watch.instance;
        g_inotify_mask[watch.wd] = watch.mask;
        g_inotify_pending[watch.wd] = watch.pending;
        g_inotify_isdir[watch.wd] = (uint8_t)(watch.is_directory != 0);
        snprintf(g_inotify_wpath[watch.wd], sizeof g_inotify_wpath[watch.wd], "%s", watch.path);
        free(g_inotify_snap[watch.wd]);
        g_inotify_snap[watch.wd] = snapshot;
    }
    for (uint32_t index = 0; index < moves; index++) {
        struct ckpt_inotify_move move;
        if (ckpt_rd_all(file, &move, sizeof move) != 0 || move.wd < 0 || move.wd >= HL_NFD ||
            !g_inotify_owner[move.wd] || g_inomv_n >= (int)(sizeof g_inomv / sizeof g_inomv[0]))
            goto fail;
        g_inomv[g_inomv_n].wd = move.wd;
        g_inomv[g_inomv_n].mask = move.mask;
        g_inomv[g_inomv_n].cookie = move.cookie;
        snprintf(g_inomv[g_inomv_n].name, sizeof g_inomv[g_inomv_n].name, "%s", move.name);
        g_inomv_n++;
    }
    for (uint32_t index = 0; index < raw_instances; index++) {
        struct ckpt_inotify_raw raw;
        if (ckpt_rd_all(file, &raw, sizeof raw) != 0 || raw.instance < 0 || raw.instance >= HL_NFD ||
            !g_inotify[raw.instance] || raw.size > 16 * 1024 * 1024u)
            goto fail;
        uint8_t *bytes = malloc(raw.size ? raw.size : 1);
        if (!bytes || (raw.size && ckpt_rd_all(file, bytes, raw.size) != 0)) {
            free(bytes);
            goto fail;
        }
        free(g_inotify_raw[raw.instance]);
        g_inotify_raw[raw.instance] = bytes;
        g_inotify_raw_len[raw.instance] = raw.size;
        g_inotify_raw_pos[raw.instance] = 0;
    }
    if (!feof(file)) {
        int byte = fgetc(file);
        if (byte != EOF) goto fail;
    }
    ckpt_source_fclose(file);
    return 0;
fail:
    ckpt_source_fclose(file);
    return -1;
}

static int ckpt_restore_fds_dir(const char *procdir) {
    struct ckpt_fd *records = NULL;
    static unsigned char desired_pipe[HL_NFD];
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/fds", procdir);
    FILE *f = ckpt_source_fopen(pf);
    if (!f) return 0;
    // The object's length, asked of the source. fstat(fileno()) does not work here: a streamed object is a
    // memory stream with no descriptor behind it.
    int64_t record_bytes = ckpt_source_object_size(pf);
    if (record_bytes < 0 || record_bytes % (int64_t)sizeof *records != 0 ||
        (uint64_t)record_bytes / sizeof *records > HL_NFD) {
        ckpt_source_fclose(f);
        return -1;
    }
    int count = (int)((uint64_t)record_bytes / sizeof *records);
    records = calloc((size_t)(count ? count : 1), sizeof *records);
    if (records == NULL || (count != 0 && fread(records, sizeof *records, (size_t)count, f) != (size_t)count) ||
        fgetc(f) != EOF) {
        free(records);
        ckpt_source_fclose(f);
        return -1;
    }
    ckpt_source_fclose(f);
    ckpt_fd_terminate_all(records, (size_t)count);
    /* A restored child inherits its restorer parent's public eventfd descriptors and process-local routing
     * tables. They are not part of the child's saved fd table merely because the parent owned them. Drop
     * those public copies without closing the inherited hidden writer seeds, then rebuild exactly this
     * process's aliases below. The counter arena is shared across the whole restored tree and is deliberately
     * not modified here. */
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (!g_eventfd_peer[fd]) continue;
        proc_fdvis_close(fd);
        close(fd);
        g_eventfd_peer[fd] = 0;
        g_eventfd_cslot[fd] = 0;
        g_eventfd_sema[fd] = 0;
        g_eventfd_gnb[fd] = 0;
    }
    memset(g_eventfd_refs, 0, sizeof g_eventfd_refs);
    memset(desired_pipe, 0, sizeof desired_pipe);
    for (int i = 0; i < count; i++)
        if (records[i].kind == CKF_PIPE && records[i].gfd >= 0 && records[i].gfd < HL_NFD)
            desired_pipe[records[i].gfd] = 1;
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (g_pipe_identity[fd] == 0 || desired_pipe[fd]) continue;
        proc_fdvis_close(fd);
        g_pipe_identity[fd] = 0;
        close(fd);
    }
    for (int i = 0; i < count; i++) {
        struct ckpt_fd r = records[i];
        /* Embedded/Rust launches seed stdio in the typed hl_linux_abi table.
         * A checkpoint may contain a later native dup2 replacement at the same
         * guest number.  Retire the fresh-launch typed binding before installing
         * serialized native state; otherwise typed syscall dispatch continues to
         * route to the launch object (typically /dev/null) and masks the restored
         * descriptor. */
        if ((r.kind == CKF_FILE || r.kind == CKF_PIPE || r.kind == CKF_BLOB || r.kind == CKF_MEMFD ||
             r.kind == CKF_EVENTFD || r.kind == CKF_TIMERFD || r.kind == CKF_INOTIFY || r.kind == CKF_EPOLL ||
             r.kind == CKF_SOCKETPAIR || r.kind == CKF_SOCKET || r.kind == CKF_SIGNALFD) &&
            g_linux_box != NULL &&
            hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)r.gfd, &(hl_linux_fd_snapshot){0}) == HL_STATUS_OK) {
            (void)hl_linux_close(g_linux_box, (hl_linux_fd)r.gfd);
            proc_fdvis_close(r.gfd);
            (void)close(r.gfd); /* retire the legacy same-number native shadow */
        }
        if (r.kind == CKF_EPOLL) continue;
        if (r.kind == CKF_SOCKETPAIR) {
            struct ckpt_restore_socket_endpoint *endpoint = ckpt_restore_socket_find(r.object_id);
            if (endpoint == NULL || endpoint->fd < 0 || r.gfd < 0 || r.gfd >= HL_NFD || dup2(endpoint->fd, r.gfd) < 0)
                return -1;
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0 ||
                fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
                return -1;
            g_sock_object[r.gfd] = r.object_id;
            g_sock_peer_object[r.gfd] = r.auxiliary;
            const struct ckpt_socket_state *state = &endpoint->state;
            g_sock_fam[r.gfd] = endpoint->state_loaded ? (uint16_t)state->guest_family : AF_UNIX;
            g_sock_stream[r.gfd] = r.offset == SOCK_STREAM;
            g_sock_dgram[r.gfd] = r.offset == SOCK_DGRAM || r.offset == SOCK_SEQPACKET;
            g_sock_seqpacket[r.gfd] = r.offset == SOCK_SEQPACKET;
            g_sock_conn[r.gfd] = 1;
            if (endpoint->state_loaded) {
                g_tcp_listen[r.gfd] = state->listening != 0;
                g_sock_backlog[r.gfd] = state->backlog;
                g_lo_port[r.gfd] = state->lo_port;
                g_lo_v6[r.gfd] = state->lo_v6;
                g_lo_v6only[r.gfd] = state->lo_v6only;
                g_br_port[r.gfd] = state->br_port;
                g_br_ip[r.gfd] = state->br_ip;
                g_br_interface[r.gfd] = state->br_interface;
                g_tcp_lport[r.gfd] = state->tcp_local_port;
                g_tcp_laddr[r.gfd] = state->tcp_local_address;
                g_tcp_l6[r.gfd] = state->tcp_local_v6;
                memcpy(g_tcp_laddr6[r.gfd], state->tcp_local_address_v6, sizeof state->tcp_local_address_v6);
                g_so_error[r.gfd] = state->pending_error;
                g_so_reuseport[r.gfd] = state->shadow_reuse_port;
                memcpy(g_tcp_optval[r.gfd], state->tcp_option_value, sizeof state->tcp_option_value);
                memcpy(g_tcp_optset[r.gfd], state->tcp_option_set, sizeof state->tcp_option_set);
                memcpy(g_ipopt_val[r.gfd], state->ip_option_value, sizeof state->ip_option_value);
                memcpy(g_ipopt_set[r.gfd], state->ip_option_set, sizeof state->ip_option_set);
            }
            for (int peer_index = 0; peer_index < count; ++peer_index)
                if (records[peer_index].kind == CKF_SOCKETPAIR && records[peer_index].object_id == r.auxiliary) {
                    g_sock_pair_peer[r.gfd] = records[peer_index].gfd + 1;
                    break;
                }
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_SOCKET) {
            struct ckpt_restore_socket *saved = ckpt_restore_socket_state_find(r.object_id);
            if (saved == NULL || saved->fd < 0 || r.gfd < 0 || r.gfd >= HL_NFD || dup2(saved->fd, r.gfd) < 0) return -1;
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0 ||
                fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
                return -1;
            const struct ckpt_socket_state *state = &saved->state;
            g_sock_object[r.gfd] = r.object_id;
            g_sock_peer_object[r.gfd] = 0;
            g_sock_fam[r.gfd] = (uint16_t)state->guest_family;
            g_sock_stream[r.gfd] = state->type == SOCK_STREAM;
            g_sock_dgram[r.gfd] = state->type == SOCK_DGRAM;
            g_sock_conn[r.gfd] = 0;
            g_tcp_listen[r.gfd] = state->listening != 0;
            g_sock_backlog[r.gfd] = state->backlog;
            g_lo_port[r.gfd] = state->lo_port;
            g_lo_v6[r.gfd] = state->lo_v6;
            g_lo_v6only[r.gfd] = state->lo_v6only;
            g_br_port[r.gfd] = state->br_port;
            g_br_ip[r.gfd] = state->br_ip;
            g_br_interface[r.gfd] = state->br_interface;
            g_tcp_lport[r.gfd] = state->tcp_local_port;
            g_tcp_laddr[r.gfd] = state->tcp_local_address;
            g_tcp_l6[r.gfd] = state->tcp_local_v6;
            memcpy(g_tcp_laddr6[r.gfd], state->tcp_local_address_v6, sizeof state->tcp_local_address_v6);
            g_so_error[r.gfd] = state->pending_error;
            g_so_reuseport[r.gfd] = state->shadow_reuse_port;
            memcpy(g_tcp_optval[r.gfd], state->tcp_option_value, sizeof state->tcp_option_value);
            memcpy(g_tcp_optset[r.gfd], state->tcp_option_set, sizeof state->tcp_option_set);
            memcpy(g_ipopt_val[r.gfd], state->ip_option_value, sizeof state->ip_option_value);
            memcpy(g_ipopt_set[r.gfd], state->ip_option_set, sizeof state->ip_option_set);
            g_udp_local_port[r.gfd] = (uint16_t)state->udp_local_port;
            g_udp_peer_port[r.gfd] = (uint16_t)state->udp_peer_port;
            g_udp_local_ip[r.gfd] = state->udp_local_ip;
            g_udp_peer_ip[r.gfd] = state->udp_peer_ip;
            g_udp_local_v6[r.gfd] = state->udp_local_v6;
            g_udp_peer_v6[r.gfd] = state->udp_peer_v6;
            g_udp_local_interface[r.gfd] = state->udp_local_interface;
            g_udp_peer_interface[r.gfd] = state->udp_peer_interface;
            if (state->udp_local_port != 0 && state->host_family == AF_UNIX) {
                int source = -1;
                for (int prior = 0; prior < i; ++prior)
                    if (records[prior].kind == CKF_SOCKET && records[prior].object_id == r.object_id) {
                        source = records[prior].gfd;
                        break;
                    }
                if (source >= 0) {
                    udp_ref_dup(r.gfd, source);
                } else {
                    const struct sockaddr_un *address = (const void *)&state->local;
                    if (udp_ref_create(r.gfd, address->sun_path) != 0) return -1;
                }
            }
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_SIGNALFD) {
            struct ckpt_restore_signalfd *object = ckpt_restore_signalfd_find(r.object_id);
            if (object == NULL || dup2(object->reader, r.gfd) < 0) return -1;
            int source = -1;
            for (int prior = 0; prior < i; ++prior)
                if (records[prior].kind == CKF_SIGNALFD && records[prior].object_id == r.object_id) {
                    source = records[prior].gfd;
                    break;
                }
            int slot;
            if (source >= 0) {
                slot = g_sigfd_slot[source] - 1;
                if (slot < 0 || slot >= HL_SFD_MAX) return -1;
                g_sfd[slot].refs++;
            } else {
                slot = sfd_alloc();
                int writer = slot >= 0 ? fcntl(object->writer, F_DUPFD, 1 << 20) : -1;
                if (writer < 0 && slot >= 0) writer = fcntl(object->writer, F_DUPFD, 64);
                if (slot < 0 || writer < 0 || hl_host_process_fd_private_adopt(writer) < 0) {
                    if (writer >= 0) close(writer);
                    if (slot >= 0) g_sfd[slot].refs = 0;
                    return -1;
                }
                g_sfd[slot].rd = r.gfd;
                g_sfd[slot].wr = writer;
                g_sfd[slot].mask = object->mask;
            }
            g_sigfd_slot[r.gfd] = (uint8_t)(slot + 1);
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0 ||
                fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
                return -1;
            g_ofd_id[r.gfd] = r.ofd_id;
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_EVENTFD) {
            struct ckpt_restore_eventfd *object = ckpt_restore_eventfd_find(r.object_id);
            if (!object || object->slot < 0 || object->slot >= HL_NFD || r.gfd < 0 || r.gfd >= HL_NFD ||
                dup2(object->reader, r.gfd) < 0)
                return -1;
            if (fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, live_flags | O_NONBLOCK) != 0) return -1;
            g_eventfd_peer[r.gfd] = object->writer + 1;
            g_eventfd_cslot[r.gfd] = object->slot + 1;
            g_eventfd_sema[r.gfd] = object->semaphore;
            eventfd_guest_nb_set(r.gfd, object->guest_nonblock);
            g_eventfd_refs[object->slot]++;
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_TIMERFD) {
            int clock_id = 0, first = 0;
            unsigned long long pending_value = 0;
            long long captured_ns = 0;
            if (sscanf(r.path, "%d %llu %u %lld", &clock_id, &pending_value, (unsigned *)&first, &captured_ns) != 4) {
                fprintf(stderr, "[restore] timerfd %d has invalid metadata '%s'\n", r.gfd, r.path);
                return -1;
            }
            int source = -1;
            for (int j = 0; j < i; j++)
                if (records[j].kind == CKF_TIMERFD && records[j].object_id == r.object_id) {
                    source = records[j].gfd;
                    break;
                }
            struct ckpt_restore_timerfd *restored = ckpt_restore_timerfd_find(r.object_id);
            if (!restored || !restored->state) return -1;
            int timer = source >= 0 ? dup(source) : kqueue();
            if (timer < 0) {
                fprintf(stderr, "[restore] timerfd %d create/dup failed: %s\n", r.gfd, strerror(errno));
                return -1;
            }
            if (source >= 0) hl_native_kqueue_duplicate(source, timer);
            if (timer != r.gfd) {
                if (dup2(timer, r.gfd) < 0) {
                    fprintf(stderr, "[restore] timerfd %d target dup failed: %s\n", r.gfd, strerror(errno));
                    close(timer);
                    return -1;
                }
                // dup2 moved the timer onto the guest's fd number; a shim kqueue keys its queue by descriptor
                // NUMBER, so it must be told or the arming kevent() below is EBADF.
                hl_native_kqueue_relocate(timer, r.gfd);
                close(timer);
            }
            if (fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) {
                fprintf(stderr, "[restore] timerfd %d flag restore failed: %s\n", r.gfd, strerror(errno));
                return -1;
            }
            int slot = source >= 0 ? timerfd_slot(source) : r.gfd;
            if (slot < 0 || slot >= HL_NFD) {
                fprintf(stderr, "[restore] timerfd %d invalid canonical slot %d\n", r.gfd, slot);
                return -1;
            }
            g_timerfd[r.gfd] = 1;
            g_epoll_family_seen = 1;
            g_tfd_cslot[r.gfd] = slot + 1;
            g_tfd_object[r.gfd] = r.object_id;
            g_tfd_clock[r.gfd] = clock_id;
            g_tfd_nb[r.gfd] = (r.flags & O_NONBLOCK) != 0;
            g_tfd_shared[r.gfd] = restored->state;
            g_tfd_refs[slot]++;
            if (source < 0) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                timerfd_shared_lock(restored->state);
                int64_t next = restored->state->deadline;
                int64_t interval = restored->state->interval;
                uint64_t pending = restored->state->pending;
                timerfd_shared_unlock(restored->state);
                g_tfd_deadline[slot] = next;
                g_tfd_interval[slot] = interval;
                g_tfd_pending[slot] = pending;
                g_tfd_first_oneshot[slot] = interval > 0 ? 1 : (uint8_t)first;
                if (pending != 0 || next > now_ns) {
                    struct kevent event;
                    int64_t delay = pending != 0 ? 1 : next - now_ns;
                    EV_SET(&event, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS, delay, NULL);
                    if (kevent(r.gfd, &event, 1, NULL, 0, NULL) < 0) {
                        fprintf(stderr, "[restore] timerfd %d arm failed: %s\n", r.gfd, strerror(errno));
                        return -1;
                    }
                }
            } else {
                g_tfd_deadline[r.gfd] = g_tfd_deadline[slot];
                g_tfd_interval[r.gfd] = g_tfd_interval[slot];
                g_tfd_first_oneshot[r.gfd] = g_tfd_first_oneshot[slot];
            }
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) {
                fprintf(stderr, "[restore] timerfd %d publication failed\n", r.gfd);
                return -1;
            }
            continue;
        }
        if (r.kind == CKF_INOTIFY) {
            int source = -1;
            for (int j = 0; j < i; j++)
                if (records[j].kind == CKF_INOTIFY && records[j].object_id == r.object_id) {
                    source = records[j].gfd;
                    break;
                }
            if (r.path[0] != 0) {
                if (source >= 0) {
                    if (dup2(source, r.gfd) < 0 ||
                        hl_linux_dup3(g_linux_box, (hl_linux_fd)source, (hl_linux_fd)r.gfd,
                                      (r.descriptor_flags & FD_CLOEXEC) ? HL_LINUX_O_CLOEXEC : 0) < 0)
                        return -1;
                } else {
                    char image_path[1400];
                    snprintf(image_path, sizeof image_path, "%s/%s", procdir, r.path);
                    int64_t stored = ckpt_source_object_size(image_path);
                    if (stored <= 0) {
                        fprintf(stderr, "[restore] inotify %d image %s is invalid: %s\n", r.gfd, image_path,
                                strerror(errno));
                        return -1;
                    }
                    size_t image_size = (size_t)stored;
                    void *image = malloc(image_size);
                    if (image == NULL || ckpt_source_load(image_path, image, image_size) != 0) {
                        fprintf(stderr, "[restore] inotify %d cannot load %s\n", r.gfd, image_path);
                        free(image);
                        return -1;
                    }
                    int shadow =
                        open(HL_LINUX_HOST_NULL_DEVICE, O_RDONLY | ((r.descriptor_flags & FD_CLOEXEC) ? O_CLOEXEC : 0));
                    if (shadow < 0 || (shadow != r.gfd && dup2(shadow, r.gfd) < 0)) {
                        fprintf(stderr, "[restore] inotify %d cannot reserve native shadow: %s\n", r.gfd,
                                strerror(errno));
                        if (shadow >= 0) close(shadow);
                        free(image);
                        return -1;
                    }
                    if (shadow != r.gfd) close(shadow);
                    void *provider = bound_inotify_provider_create(g_host_services);
                    int64_t imported = provider == NULL
                                           ? -HL_LINUX_ENOMEM
                                           : hl_linux_inotify_import_at(
                                                 g_linux_box, (hl_linux_fd)r.gfd, &bound_inotify_ops, provider,
                                                 (uint32_t)r.descriptor_flags, (uint32_t)r.flags, image, image_size);
                    free(image);
                    if (imported < 0) {
                        fprintf(stderr, "[restore] inotify %d typed import failed: %lld\n", r.gfd, (long long)imported);
                        close(r.gfd);
                        return -1;
                    }
                }
                if (proc_fdvis_publish(r.gfd, HL_HOST_FD_OTHER, 0, 0) != 0) {
                    fprintf(stderr, "[restore] inotify %d fd visibility publication failed\n", r.gfd);
                    return -1;
                }
                continue;
            }
#if defined(__linux__)
            int instance =
                source >= 0 ? dup(source)
                            : inotify_init1((r.flags & O_NONBLOCK) | ((r.descriptor_flags & FD_CLOEXEC) ? 0x80000 : 0));
#else
            int instance = source >= 0 ? dup(source) : kqueue();
#endif
            if (instance < 0) return -1;
            if (source >= 0) hl_native_kqueue_duplicate(source, instance);
            if (instance != r.gfd) {
                if (dup2(instance, r.gfd) < 0) {
                    close(instance);
                    return -1;
                }
                if (source >= 0) hl_native_kqueue_duplicate(source, r.gfd);
                close(instance);
            }
            if (fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
            g_inotify[r.gfd] = 1;
            g_inotify_nb[r.gfd] = (r.flags & O_NONBLOCK) != 0;
            g_inotify_object[r.gfd] = r.object_id;
            g_epoll_family_seen = 1;
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.ofd_id != 0 && r.kind != CKF_PIPE && r.kind != CKF_TTY) {
            int source = -1;
            for (int j = 0; j < i; j++)
                if (records[j].ofd_id == r.ofd_id) {
                    source = records[j].gfd;
                    break;
                }
            if (source >= 0) {
                if (dup2(source, r.gfd) < 0) return -1;
                if (r.descriptor_flags & FD_CLOEXEC)
                    fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
                else
                    fcntl(r.gfd, F_SETFD, 0);
                if (r.kind == CKF_MEMFD && r.gfd >= 0 && r.gfd < HL_NFD) {
                    g_memfd_is[r.gfd] = 1;
                    g_memfd_seal[r.gfd] = (int)r.auxiliary;
                    memfd_reg_set_fd(r.gfd, g_memfd_seal[r.gfd]);
                }
                if (r.gfd >= 0 && r.gfd < HL_NFD) g_ofd_id[r.gfd] = r.ofd_id;
                if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
                continue;
            }
        }
        if ((r.kind == CKF_FILE || r.kind == CKF_BLOB || r.kind == CKF_MEMFD) && r.ofd_id != 0) {
            struct ckpt_restore_right *right = ckpt_restore_right_find(r.ofd_id);
            if (right != NULL) {
                if (right->object_id != r.object_id || dup2(right->fd, r.gfd) < 0 ||
                    fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
                    return -1;
                g_ofd_id[r.gfd] = r.ofd_id;
                if (r.kind == CKF_MEMFD) {
                    g_memfd_is[r.gfd] = 1;
                    g_memfd_seal[r.gfd] = (int)r.auxiliary;
                    memfd_reg_set_fd(r.gfd, g_memfd_seal[r.gfd]);
                }
                if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
                continue;
            }
        }
        if (r.kind == CKF_TTY) {
            // 0/1/2 are inherited from the launcher pty. But an interactive shell also keeps a HIGH-fd dup of
            // the controlling terminal for job control (bash uses fd 255); the launcher doesn't provide it, so
            // recreate it by duping the ctty onto that number -- else the shell's tcsetattr/tcgetattr on it
            // fails EBADF after restore ("tcsetattr: Bad file descriptor" when a foreground job finishes).
            if (r.gfd > 2) {
                int ct = ckpt_ctty_open();
                if (ct >= 0 && r.gfd != ct && dup2(ct, r.gfd) >= 0 && (r.flags & FD_CLOEXEC))
                    fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
                ckpt_ctty_close(ct);
            }
            if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            continue;
        }
        if (r.kind == CKF_PIPE) {
            uint64_t identity = (uint64_t)r.offset;
            struct ckpt_restore_pipe *pipe = ckpt_restore_pipe_find(identity);
            int source = ((r.flags & O_ACCMODE) == O_WRONLY) ? (pipe ? pipe->writer : -1) : (pipe ? pipe->reader : -1);
            if (source < 0 || dup2(source, r.gfd) < 0) return -1;
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0)
                return -1;
            if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            g_pipe_identity[r.gfd] = identity;
            g_pipesz[r.gfd] = pipe->size;
            if (proc_fdvis_publish(r.gfd, HL_HOST_FD_PIPE, 1, identity) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_BLOB) {
            if (ckpt_restore_file_blob(procdir, &r) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_MEMFD) {
            int seed = ckpt_restore_backing_find(r.object_id);
            if (seed >= 0) {
                if (dup2(seed, r.gfd) < 0) return -1;
                int live_flags = fcntl(r.gfd, F_GETFL);
                if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0 ||
                    lseek(r.gfd, (off_t)r.offset, SEEK_SET) < 0)
                    return -1;
                if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
                if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            } else if (ckpt_restore_file_blob(procdir, &r) != 0) {
                return -1;
            }
            if (r.gfd < 0 || r.gfd >= HL_NFD) return -1;
            g_memfd_is[r.gfd] = 1;
            g_memfd_seal[r.gfd] = (int)r.auxiliary;
            memfd_reg_set_fd(r.gfd, g_memfd_seal[r.gfd]);
            continue;
        }
        if (r.kind == CKF_FILE) {
            int flags = r.flags & ~(O_CREAT | O_EXCL | O_TRUNC);
            int hf = open(r.path, flags);
            if (hf < 0) {
                fprintf(stderr, "[restore] cannot reopen fd %d (%s): %s\n", r.gfd, r.path, strerror(errno));
                return -1;
            }
            if (hf != r.gfd) {
                dup2(hf, r.gfd);
                close(hf);
            }
            if (r.offset > 0) lseek(r.gfd, (off_t)r.offset, SEEK_SET);
            if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            if (r.gfd >= 0 && r.gfd < 1024 && path_copy(g_fdpath[r.gfd], sizeof g_fdpath[r.gfd], r.path) != 0)
                g_fdpath[r.gfd][0] = 0;
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
        }
        if (r.kind == CKF_DEVICE) {
            int flags = r.flags & ~(O_CREAT | O_EXCL | O_TRUNC);
            int host_fd = open(r.path, flags);
            if (host_fd < 0 || (host_fd != r.gfd && dup2(host_fd, r.gfd) < 0)) {
                if (host_fd >= 0) close(host_fd);
                return -1;
            }
            if (host_fd != r.gfd) close(host_fd);
            if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
        }
    }
    for (int i = 0; i < count; ++i) {
        struct ckpt_fd r = records[i];
        if (r.kind != CKF_EPOLL) continue;
        int source = -1;
        for (int prior = 0; prior < i; ++prior)
            if (records[prior].kind == CKF_EPOLL && records[prior].object_id == r.object_id) {
                source = records[prior].gfd;
                break;
            }
        int instance = source >= 0 ? dup(source) : kqueue();
        if (instance < 0) return -1;
        if (source >= 0) hl_native_kqueue_duplicate(source, instance);
        if (instance != r.gfd) {
            if (dup2(instance, r.gfd) < 0) {
                close(instance);
                return -1;
            }
            // Same shim-kqueue descriptor-number identity as the timerfd path above.
            hl_native_kqueue_relocate(instance, r.gfd);
            close(instance);
        }
        if (fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
        g_ep_provider_generations[r.gfd] = ep_provider_next(g_ep_provider_generations[r.gfd]);
        g_epoll[r.gfd] = 1;
        g_ep_cslot[r.gfd] = (uint16_t)((source >= 0 ? epoll_slot(source) : r.gfd) + 1);
        g_ep_dupd[r.gfd] = source >= 0;
        if (source >= 0) {
            g_ep_dupd[source] = 1;
            ofd_link_dup(r.gfd, source);
        }
        g_epoll_family_seen = 1;
        ep_mem_clear(r.gfd);
        if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
    }
    for (int i = 0; i < count; ++i) {
        if (records[i].kind != CKF_EPOLL) continue;
        int duplicate = 0;
        for (int prior = 0; prior < i; ++prior)
            if (records[prior].kind == CKF_EPOLL && records[prior].object_id == records[i].object_id) duplicate = 1;
        if (!duplicate && ckpt_restore_epoll_watches(procdir, &records[i]) != 0) return -1;
    }
    int restored = ckpt_restore_inotify_sidecar(procdir);
    free(records);
    return restored;
}

static int ckpt_restore_cpu_dir(const char *procdir, const struct ckpt_meta *m, struct cpu **out) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/cpu", procdir);
    if (m->n_threads > SIZE_MAX / sizeof(struct cpu)) return -1;
    size_t bytes = (size_t)m->n_threads * sizeof(struct cpu);
    if (bytes > SIZE_MAX - sizeof(struct ckpt_cpu_header)) return -1;
    size_t file_bytes = sizeof(struct ckpt_cpu_header) + bytes;
    struct ckpt_cpu_header *cpu_file = malloc(file_bytes);
    if (!cpu_file || ckpt_source_load(pf, cpu_file, file_bytes) != 0) {
        free(cpu_file);
        fprintf(stderr, "[restore] cannot read cpu state\n");
        return -1;
    }
    if (cpu_file->magic != CKPT_CPU_MAGIC || cpu_file->version != m->version || cpu_file->arch != G_CKPT_ARCH ||
        cpu_file->count != m->n_threads || cpu_file->payload_size != sizeof(struct cpu)) {
        fprintf(stderr, "[restore] cpu image version/architecture/layout mismatch\n");
        free(cpu_file);
        return -1;
    }
    struct cpu *images = malloc(bytes);
    if (!images) {
        free(cpu_file);
        return -1;
    }
    memcpy(images, cpu_file + 1, bytes);
    free(cpu_file);
    // Zero host-transient fields (meaningful only WHILE a block runs; run_block re-populates them). The
    // architectural state (x[],sp,pc,tls,nzcv,v[],sigmask,tpending,alt_*,tid,ctid) + shadow stack are verbatim.
    for (uint64_t i = 0; i < m->n_threads; i++) {
        struct cpu *c = &images[i];
        memset(c->host_save, 0, sizeof c->host_save);
        memset(c->host_v, 0, sizeof c->host_v);
        c->host_sp = 0;
        c->reason = 0;
        c->irq = 0;
        c->in_service = 0;
        c->exited = 0;
        c->redirect = 0;
        G_CKPT_CPU_SANITIZE(c);
    }
    *out = images;
    return 0;
}

static int ckpt_restore_leader(const struct cpu *images, uint64_t count, struct cpu *leader) {
    for (uint64_t i = 0; i < count; i++) {
        if (images[i].tid != 0) continue;
        *leader = images[i];
        return 0;
    }
    fprintf(stderr, "[restore] checkpoint thread group has no leader\n");
    return -1;
}

// Re-install this process's guest signal DISPOSITIONS after a restore, from the table carried in `m`.
// g_sigact (the guest handler table) is engine C state, not guest RAM, so it is NOT in the page dump; a
// freshly re-launched/-forked restore process starts with an all-SIG_DFL table and DEFAULT host
// dispositions -- so a ^C (SIGINT) at a restored bash prompt hit the host default action (terminate) and
// KILLED the shell instead of running bash's interrupt handler (the "restore + Ctrl-C closes the tab" bug).
// This restores g_sigact and replays the exact host-side installation rt_sigaction(case 134) performs, so
// async signals route back through the engine's host_sigh and reach the guest handler.
static void ckpt_reinstall_sigacts(const struct ckpt_meta *m) {
    for (int s = 1; s <= 64; s++) {
        uint64_t h = m->sig_handler[s];
        g_sigact[s].handler = h;
        g_sigact[s].flags = m->sig_flags[s];
        g_sigact[s].mask = m->sig_mask[s];
        if (s == 9 || s == 19) continue; // SIGKILL/SIGSTOP: unmaskable, no host disposition to forward
        // SIGSEGV(11)/SIGBUS(7) keep the engine's permanent hardware-fault guard -- never overwrite it with a
        // plain disposition (that is exactly what rt_sigaction refuses to do). Only ILL/TRAP/FPE(4/5/8), whose
        // POSIX handler only ever sees an EXTERNAL kill, and the async signals forward to the host here.
        if (s == 7 || s == 11) continue;
        int ms = sig_l2m(s);
        if (sig_host_is_engine_control(ms)) continue;
        if (h == 0) {
            signal(ms, SIG_DFL);
        } else if (h == 1) {
            signal(ms, SIG_IGN);
        } else {
            struct sigaction sa;
            memset(&sa, 0, sizeof sa);
            sa.sa_sigaction = (s == 4 || s == 5 || s == 8) ? host_sigh_sync : host_sigh_si;
            sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
            sigfillset(&sa.sa_mask);
            sigaction(ms, &sa, NULL);
        }
    }
}

// Survive a terminal signal typed while the tree is still being rebuilt.
//
// A restoring process is not the guest yet: until it has replayed the guest's dispositions
// (ckpt_reinstall_sigacts) it carries the HOST defaults, so a ^C at the pty -- which the embedder's user can
// type at any moment, and which an acceptance test aims at the restored foreground job -- terminates the
// restore driver itself and takes the whole machine with it. Ignore the terminal-generated signals for that
// window; reinstall_sigacts then overwrites every disposition, SIG_DFL included. Each re-forked process does
// this again on entry, because it inherits the init's already-replayed dispositions.
static void ckpt_restore_hold_tty_signals(void) {
    static const int tty[] = {SIGINT, SIGQUIT, SIGHUP, SIGTSTP, SIGTTIN, SIGTTOU};
    for (size_t i = 0; i < sizeof tty / sizeof tty[0]; ++i)
        (void)signal(tty[i], SIG_IGN);
}

// The process table read from the checkpoint (one entry per proc.<gpid>/meta), used to rebuild the tree.
struct ckpt_proc {
    int gpid, ppid, pgid, sid;
    uint64_t version;
    int viable;
    char reason[192];
};
static struct ckpt_proc *g_rprocs;
static int g_nrprocs;
static int g_rprocs_capacity;

// Enumerate the captured process images. This is the restore-side counterpart of the coordinator's group
// count: "which processes are in this image" is answered by the store, over the same channel.
static int ckpt_scan_procs(void) {
    char names[64 * 1024];
    g_nrprocs = 0;
    int listed = ckpt_source_list("proc.", names, sizeof names);
    if (listed < 0) return -1;
    const char *cursor = names;
    for (int index = 0; index < listed; ++index, cursor += strlen(cursor) + 1) {
        if (strstr(cursor, ".tmp.")) continue;
        int gpid = atoi(cursor + 5);
        if (gpid <= 0) continue;
        char pd[1200];
        snprintf(pd, sizeof pd, "%.200s", cursor);
        struct ckpt_meta m;
        if (ckpt_read_meta_dir(pd, &m) != 0) continue;
        if (ckpt_vector_reserve((void **)&g_rprocs, &g_rprocs_capacity, sizeof *g_rprocs, g_nrprocs + 1) != 0)
            return -1;
        g_rprocs[g_nrprocs].gpid = gpid;
        g_rprocs[g_nrprocs].ppid = m.ppid_gpid;
        g_rprocs[g_nrprocs].pgid = m.pgid_gpid;
        g_rprocs[g_nrprocs].sid = m.sid_gpid;
        g_rprocs[g_nrprocs].version = m.version;
        g_rprocs[g_nrprocs].viable = 1;
        g_rprocs[g_nrprocs].reason[0] = 0;
        g_nrprocs++;
    }
    return g_nrprocs > 0 ? 0 : -1;
}

static int ckpt_validate_proc_tree(const struct ckpt_manifest *man) {
    if ((uint64_t)g_nrprocs != man->n_procs) return -1;
    int roots = 0;
    for (int i = 0; i < g_nrprocs; i++) {
        if (g_rprocs[i].version != man->version) return -1;
        if (g_rprocs[i].gpid == man->root_gpid) {
            if (g_rprocs[i].ppid != 0) return -1;
            roots++;
            continue;
        }
        int ancestor = g_rprocs[i].ppid;
        int reached_root = 0;
        for (int depth = 0; depth < g_nrprocs; depth++) {
            if (ancestor == man->root_gpid) {
                reached_root = 1;
                break;
            }
            int parent = -1;
            for (int j = 0; j < g_nrprocs; j++)
                if (g_rprocs[j].gpid == ancestor) {
                    parent = g_rprocs[j].ppid;
                    break;
                }
            if (parent <= 0) break;
            ancestor = parent;
        }
        if (!reached_root) return -1; // missing parent or detached cycle
    }
    return roots == 1 ? 0 : -1;
}

// The requested policy, or -1 when the caller never asked for one (unset, or the DEFAULT wire value).
static int ckpt_recovery_policy_requested(void) {
    const char *value = hl_option_get("HL_CHECKPOINT_POLICY");
    if (value && value[0] > '0' && value[0] <= '3' && value[1] == 0) return value[0] - '0';
    return -1;
}

// No policy asked for means "restore whatever can be restored", so the default is the most permissive one.
static int ckpt_recovery_policy(void) {
    int requested = ckpt_recovery_policy_requested();
    return requested < 0 ? CKPT_RECOVERY_DISCARD_OPTIONAL : requested;
}

// Capture only relaxes for an explicitly permissive policy; the permissive restore default must not reach it.
static int ckpt_recovery_permissive_requested(void) {
    int requested = ckpt_recovery_policy_requested();
    return requested == CKPT_RECOVERY_RECONNECT || requested == CKPT_RECOVERY_DISCARD_OPTIONAL;
}

static int ckpt_recovery_policy_set(const char *value) {
    const char *encoded = NULL;
    if (!strcmp(value, "refuse"))
        encoded = "3";
    else if (!strcmp(value, "reconnect"))
        encoded = "1";
    else if (!strcmp(value, "discard-optional"))
        encoded = "2";
    else if (!strcmp(value, "default"))
        encoded = "0";
    if (!encoded) return -1;
    return hl_option_set("HL_CHECKPOINT_POLICY", encoded, 1);
}

static struct ckpt_proc *ckpt_proc_find(int gpid) {
    for (int i = 0; i < g_nrprocs; ++i)
        if (g_rprocs[i].gpid == gpid) return &g_rprocs[i];
    return NULL;
}

static void ckpt_process_stop(struct ckpt_proc *process, const char *reason) {
    if (!process || !process->viable) return;
    process->viable = 0;
    snprintf(process->reason, sizeof process->reason, "%s", reason);
    for (int changed = 1; changed;) {
        changed = 0;
        for (int i = 0; i < g_nrprocs; ++i) {
            struct ckpt_proc *child = &g_rprocs[i];
            struct ckpt_proc *parent = ckpt_proc_find(child->ppid);
            if (child->viable && parent && !parent->viable) {
                child->viable = 0;
                snprintf(child->reason, sizeof child->reason, "ancestor %d was stopped", parent->gpid);
                changed = 1;
            }
        }
    }
}

static void ckpt_json_string(FILE *file, const char *value) {
    fputc('"', file);
    for (const unsigned char *p = (const unsigned char *)(value ? value : ""); *p; ++p) {
        if (*p == '"' || *p == '\\')
            fprintf(file, "\\%c", *p);
        else if (*p == '\n')
            fputs("\\n", file);
        else if (*p == '\r')
            fputs("\\r", file);
        else if (*p == '\t')
            fputs("\\t", file);
        else if (*p < 0x20)
            fprintf(file, "\\u%04x", *p);
        else
            fputc(*p, file);
    }
    fputc('"', file);
}

static int ckpt_recovery_report_queue(FILE *report, const struct ckpt_proc *process,
                                      const struct ckpt_fd *socket_record) {
    char path[1400];
    snprintf(path, sizeof path, "%s", socket_record->path);
    FILE *queue = ckpt_source_fopen(path);
    if (!queue) return -1;
    struct ckpt_socket_queue_header header;
    if (ckpt_rd_all(queue, &header, sizeof header) != 0 || header.magic != CKPT_SOCKET_QUEUE_MAGIC) {
        ckpt_source_fclose(queue);
        return -1;
    }
    for (;;) {
        struct ckpt_socket_queue_frame frame;
        size_t received = fread(&frame, 1, sizeof frame, queue);
        if (received == 0 && feof(queue)) break;
        if (received != sizeof frame || frame.size > (1u << 20) || frame.rights_count > 253) {
            ckpt_source_fclose(queue);
            return -1;
        }
        unsigned char payload[4096];
        uint32_t remaining = frame.size;
        while (remaining != 0) {
            size_t chunk = remaining < sizeof payload ? remaining : sizeof payload;
            if (ckpt_rd_all(queue, payload, chunk) != 0) {
                ckpt_source_fclose(queue);
                return -1;
            }
            remaining -= (uint32_t)chunk;
        }
        for (uint32_t index = 0; index < frame.rights_count; ++index) {
            struct ckpt_fd right;
            if (ckpt_rd_fd(queue, &right) != 0) {
                ckpt_source_fclose(queue);
                return -1;
            }
            const char *outcome = !process->viable                                       ? "stopped"
                                  : (right.kind == CKF_FILE || right.kind == CKF_DEVICE) ? "reconnected"
                                                                                         : "reconstructed";
            fprintf(
                report,
                "{\"type\":\"resource\",\"gpid\":%d,\"fd\":-1,\"kind\":%d,\"queued\":true,\"outcome\":\"%s\",\"path\":",
                process->gpid, right.kind, outcome);
            ckpt_json_string(report, (right.kind == CKF_FILE || right.kind == CKF_DEVICE) ? right.path : "");
            fputs("}\n", report);
        }
    }
    ckpt_source_fclose(queue);
    return 0;
}

// The restore-side recovery journal. It is the one thing restore WRITES, and it is written back into the
// image next to what it describes: assembled in memory, then handed to the store as the object
// "RECOVERY.jsonl" -- all at once, or not at all.
static int ckpt_recovery_report(int policy) {
    char *buffer = NULL;
    size_t buffered = 0;
    FILE *file = open_memstream(&buffer, &buffered);
    if (!file) return -1;
    fprintf(file, "{\"type\":\"summary\",\"format\":1,\"policy\":%d,\"processes\":%d}\n", policy, g_nrprocs);
    for (int i = 0; i < g_nrprocs; ++i) {
        struct ckpt_proc *process = &g_rprocs[i];
        fprintf(file, "{\"type\":\"process\",\"gpid\":%d,\"ppid\":%d,\"outcome\":\"%s\",\"reason\":", process->gpid,
                process->ppid, process->viable ? "restored" : "stopped");
        ckpt_json_string(file, process->reason);
        fputs("}\n", file);
        char fd_path[1300];
        snprintf(fd_path, sizeof fd_path, "proc.%d/fds", process->gpid);
        FILE *fds = ckpt_source_fopen(fd_path);
        if (fds) {
            struct ckpt_fd record;
            while (ckpt_rd_fd(fds, &record) == 0) {
                const char *outcome = "reconstructed";
                if (!process->viable)
                    outcome = "stopped";
                else if (record.kind == CKF_FILE || record.kind == CKF_TTY || record.kind == CKF_DEVICE ||
                         record.kind == CKF_SOCKET)
                    outcome = "reconnected";
                fprintf(file, "{\"type\":\"resource\",\"gpid\":%d,\"fd\":%d,\"kind\":%d,\"outcome\":\"%s\",\"path\":",
                        process->gpid, record.gfd, record.kind, outcome);
                ckpt_json_string(file, (record.kind == CKF_FILE || record.kind == CKF_DEVICE) ? record.path : "");
                fputs("}\n", file);
                if (record.kind == CKF_SOCKETPAIR && ckpt_recovery_report_queue(file, process, &record) != 0) {
                    ckpt_source_fclose(fds);
                    fclose(file);
                    free(buffer);
                    return -1;
                }
            }
            ckpt_source_fclose(fds);
        }
    }
    {
        int failed = fclose(file) != 0;
        if (!failed) {
            struct ckpt_sink_stream *object = NULL;
            failed = ckpt_sink_stream_begin(NULL, NULL, "RECOVERY.jsonl", 0, &object) != 0 ||
                     ckpt_sink_stream_write(object, buffer, buffered) != 0 || ckpt_sink_stream_finish(object) != 0;
        }
        free(buffer);
        return failed ? -1 : 0;
    }
}

static int ckpt_validate_process_image(const struct ckpt_proc *process, struct ckpt_meta *meta) {
    char procdir[1300], path[1400];
    snprintf(procdir, sizeof procdir, "proc.%d", process->gpid);
    if (ckpt_read_meta_dir(procdir, meta) != 0 || meta->self_gpid != process->gpid ||
        meta->ppid_gpid != process->ppid || meta->pagesz == 0 || meta->pagesz > UINT64_C(1048576) ||
        (meta->pagesz & (meta->pagesz - 1)) != 0 || meta->n_regions > UINT64_C(1048576) || meta->n_fds > HL_NFD)
        return -1;

    snprintf(path, sizeof path, "%s/pages", procdir);
    FILE *pages = ckpt_source_fopen(path);
    if (!pages) return -1;
    for (uint64_t index = 0; index < meta->n_regions; ++index) {
        struct ckpt_region region;
        if (ckpt_read_region(pages, &region) != 0 || region.addr == 0 || region.len == 0 ||
            region.addr > UINT64_MAX - region.len || region.glen > region.len ||
            region.npages > (region.len - 1) / meta->pagesz + 1 || region.format_version != CKPT_REGION_VERSION ||
            region.logical > 1 ||
            (region.backing_object != 0 && (region.backing_offset > UINT64_MAX - region.glen ||
                                            region.backing_offset + region.glen > (uint64_t)INT64_MAX)) ||
            (region.logical && (region.backing_object == 0 || !region.backing_shared || region.backing_emulated ||
                                region.glen != region.len))) {
            ckpt_source_fclose(pages);
            return -1;
        }
        for (uint64_t page = 0; page < region.npages; ++page) {
            uint64_t address;
            if (ckpt_rd_all(pages, &address, sizeof address) != 0 || address < region.addr ||
                address >= region.addr + region.len || (address - region.addr) % meta->pagesz != 0) {
                ckpt_source_fclose(pages);
                return -1;
            }
            uint64_t remaining = region.addr + region.len - address;
            size_t size = (size_t)(remaining < meta->pagesz ? remaining : meta->pagesz);
            unsigned char buffer[4096];
            while (size != 0) {
                size_t chunk = size < sizeof buffer ? size : sizeof buffer;
                if (ckpt_rd_all(pages, buffer, chunk) != 0) {
                    ckpt_source_fclose(pages);
                    return -1;
                }
                size -= chunk;
            }
        }
    }
    int trailing = fgetc(pages);
    ckpt_source_fclose(pages);
    if (trailing != EOF) return -1;

    struct cpu *images = NULL, leader;
    if (ckpt_restore_cpu_dir(procdir, meta, &images) != 0 ||
        ckpt_restore_leader(images, meta->n_threads, &leader) != 0) {
        free(images);
        return -1;
    }
    free(images);

    snprintf(path, sizeof path, "%s/fds", procdir);
    FILE *fds = ckpt_source_fopen(path);
    if (!fds) return -1;
    uint64_t descriptors = 0;
    struct ckpt_fd record;
    while (ckpt_rd_fd(fds, &record) == 0) {
        if (record.gfd < 0 || record.gfd >= HL_NFD || record.kind < CKF_TTY || record.kind > CKF_DEVICE) {
            ckpt_source_fclose(fds);
            return -1;
        }
        descriptors++;
    }
    int valid_fds = feof(fds) && descriptors == meta->n_fds;
    ckpt_source_fclose(fds);
    return valid_fds ? 0 : -1;
}

static int ckpt_external_unavailable(const struct ckpt_fd *record) {
    if (record->kind != CKF_FILE && record->kind != CKF_DEVICE) return 0;
    struct stat status;
    if (stat(record->path, &status) != 0) return 1;
    if (record->kind == CKF_DEVICE) {
        if (!S_ISCHR(status.st_mode) && !S_ISBLK(status.st_mode)) return 1;
    } else {
        int directory = (record->auxiliary & CKFA_DIRECTORY) != 0;
        if (directory ? !S_ISDIR(status.st_mode) : !S_ISREG(status.st_mode)) return 1;
    }
    int flags = record->flags & ~(O_CREAT | O_EXCL | O_TRUNC);
    int probe = open(record->path, flags);
    if (probe < 0) return 1;
    close(probe);
    return 0;
}

static int ckpt_preflight_socket_queue(const struct ckpt_fd *socket_record, struct ckpt_fd *unavailable) {
    char path[1400];
    snprintf(path, sizeof path, "%s", socket_record->path);
    FILE *file = ckpt_source_fopen(path);
    if (!file) return -1;
    struct ckpt_socket_queue_header header;
    if (ckpt_rd_all(file, &header, sizeof header) != 0 || header.magic != CKPT_SOCKET_QUEUE_MAGIC) {
        ckpt_source_fclose(file);
        return -1;
    }
    for (;;) {
        struct ckpt_socket_queue_frame frame;
        size_t received = fread(&frame, 1, sizeof frame, file);
        if (received == 0 && feof(file)) break;
        if (received != sizeof frame || frame.size > (1u << 20) || frame.rights_count > 253) {
            ckpt_source_fclose(file);
            return -1;
        }
        unsigned char payload[4096];
        uint32_t remaining = frame.size;
        while (remaining != 0) {
            size_t chunk = remaining < sizeof payload ? remaining : sizeof payload;
            if (ckpt_rd_all(file, payload, chunk) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            remaining -= (uint32_t)chunk;
        }
        for (uint32_t index = 0; index < frame.rights_count; ++index) {
            struct ckpt_fd right;
            if (ckpt_rd_fd(file, &right) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            if (ckpt_external_unavailable(&right)) {
                *unavailable = right;
                ckpt_source_fclose(file);
                return 1;
            }
        }
    }
    ckpt_source_fclose(file);
    return 0;
}

static int ckpt_restore_preflight(int policy) {
    for (int i = 0; i < g_nrprocs; ++i) {
        struct ckpt_proc *process = &g_rprocs[i];
        struct ckpt_meta meta;
        if (ckpt_validate_process_image(process, &meta) != 0) {
            ckpt_process_stop(process, "checkpoint process image is invalid");
            continue;
        }
        char path[1300];
        snprintf(path, sizeof path, "proc.%d/fds", process->gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) {
            ckpt_process_stop(process, "descriptor image is missing");
            continue;
        }
        struct ckpt_fd record;
        while (process->viable && ckpt_rd_fd(file, &record) == 0) {
            if (record.kind == CKF_FILE || record.kind == CKF_DEVICE) {
                if (ckpt_external_unavailable(&record)) {
                    char reason[192];
                    snprintf(reason, sizeof reason, "required external %s is unavailable: %.130s",
                             record.kind == CKF_DEVICE ? "device" : "path", record.path);
                    ckpt_process_stop(process, reason);
                }
            }
            if (process->viable && record.kind == CKF_SOCKETPAIR) {
                struct ckpt_fd unavailable;
                int queue = ckpt_preflight_socket_queue(&record, &unavailable);
                if (queue < 0)
                    ckpt_process_stop(process, "socket queue image is corrupt");
                else if (queue > 0) {
                    char reason[192];
                    snprintf(reason, sizeof reason, "queued external %s is unavailable: %.130s",
                             unavailable.kind == CKF_DEVICE ? "device" : "path", unavailable.path);
                    ckpt_process_stop(process, reason);
                }
            }
        }
        if (!feof(file) && process->viable) ckpt_process_stop(process, "descriptor image is corrupt");
        ckpt_source_fclose(file);
    }
    struct ckpt_proc *root = ckpt_proc_find(1);
    int stopped = 0;
    for (int i = 0; i < g_nrprocs; ++i)
        stopped += !g_rprocs[i].viable;
    if (ckpt_recovery_report(policy) != 0) {
        fprintf(stderr, "[restore] cannot publish recovery report\n");
        return -1;
    }
    if (stopped && policy == CKPT_RECOVERY_REFUSE) {
        fprintf(stderr, "[restore] preflight refused %d process(es); see RECOVERY.jsonl\n", stopped);
        return -1;
    }
    if (!root || !root->viable) {
        // Name the reason here too: RECOVERY.jsonl goes to the embedder's store, which a caller watching the
        // machine's own terminal cannot read, and "not recoverable" alone says nothing about what was missing.
        fprintf(stderr, "[restore] container init is not recoverable: %s (see RECOVERY.jsonl)\n",
                root ? root->reason : "no init image");
        return -1;
    }
    return 0;
}

static int ckpt_prepare_restore_pipes(void) {
    g_nrestore_pipes = 0;
    for (int process = 0; process < g_nrprocs; process++) {
        char path[1300];
        if (!g_rprocs[process].viable) continue; // a stopped process must not fail the restore it was pruned from
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_PIPE) continue;
            uint64_t identity = (uint64_t)record.offset;
            struct ckpt_restore_pipe *pipe = ckpt_restore_pipe_find(identity);
            if (!pipe) {
                if (ckpt_vector_reserve((void **)&g_restore_pipes, &g_restore_pipes_capacity, sizeof *g_restore_pipes,
                                        g_nrestore_pipes + 1) != 0) {
                    ckpt_source_fclose(file);
                    return -1;
                }
                pipe = &g_restore_pipes[g_nrestore_pipes++];
                *pipe = (struct ckpt_restore_pipe){.identity = identity, .reader = -1, .writer = -1};
            }
            int size = atoi(record.path);
            if (size > pipe->size) pipe->size = size;
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    for (int i = 0; i < g_nrestore_pipes; i++) {
        char data_path[1300];
        int pair[2];
        if (pipe(pair) != 0) return -1;
#ifdef F_SETPIPE_SZ
        if (g_restore_pipes[i].size > 0) (void)fcntl(pair[1], F_SETPIPE_SZ, g_restore_pipes[i].size);
#endif
        int reader = hl_host_process_fd_private_adopt(pair[0]);
        if (reader < 0) {
            close(pair[0]);
            close(pair[1]);
            return -1;
        }
        int writer = hl_host_process_fd_private_adopt(pair[1]);
        if (writer < 0) {
            hl_host_process_fd_private_remove(reader);
            close(reader);
            close(pair[1]);
            return -1;
        }
        g_restore_pipes[i].reader = reader;
        g_restore_pipes[i].writer = writer;
        int flags = fcntl(writer, F_GETFL);
        if (flags < 0 || fcntl(writer, F_SETFL, flags | O_NONBLOCK) != 0) return -1;
        snprintf(data_path, sizeof data_path, "pipe.%016llx", (unsigned long long)g_restore_pipes[i].identity);
        FILE *data = ckpt_source_fopen(data_path);
        if (!data) continue;
        unsigned char buffer[65536];
        size_t count;
        while ((count = fread(buffer, 1, sizeof buffer, data)) != 0) {
            size_t offset = 0;
            while (offset < count) {
                ssize_t written = write(writer, buffer + offset, count - offset);
                if (written > 0) {
                    offset += (size_t)written;
                    continue;
                }
                if (written < 0 && errno == EINTR) continue;
                ckpt_source_fclose(data);
                return -1;
            }
        }
        if (ferror(data)) {
            ckpt_source_fclose(data);
            return -1;
        }
        ckpt_source_fclose(data);
    }
    return 0;
}

static int ckpt_restore_right_prepare(const struct ckpt_fd *record) {
    struct ckpt_restore_right *existing = ckpt_restore_right_find(record->ofd_id);
    if (existing != NULL) return existing->object_id == record->object_id ? existing->fd : -1;
    if (!record->ofd_id ||
        (record->kind != CKF_FILE && record->kind != CKF_DEVICE && record->kind != CKF_BLOB &&
         record->kind != CKF_MEMFD && record->kind != CKF_PIPE && record->kind != CKF_SIGNALFD &&
         record->kind != CKF_INOTIFY && record->kind != CKF_EVENTFD && record->kind != CKF_TIMERFD &&
         record->kind != CKF_EPOLL) ||
        ckpt_vector_reserve((void **)&g_restore_rights, &g_restore_rights_capacity, sizeof *g_restore_rights,
                            g_nrestore_rights + 1) != 0)
        return fprintf(stderr, "[restore] invalid queued right kind=%d ofd=%llx\n", record->kind,
                       (unsigned long long)record->ofd_id),
               -1;
    int fd = -1;
    if (record->kind == CKF_EPOLL) {
        // Any open descriptor will do as the placeholder, but it must not stay where open(2) put it: it is
        // closed AFTER the guest's own descriptors are dup2'd into place, so a low number destroys whichever
        // guest descriptor now owns it (it landed on 4, a socketpair endpoint). Hoist it into the private
        // high band, like every other queued right.
        int placeholder = open(HL_LINUX_HOST_NULL_DEVICE, O_RDONLY | O_CLOEXEC);
        if (placeholder < 0) return -1;
        fd = hl_host_process_fd_private_adopt(placeholder);
        if (fd < 0) {
            close(placeholder);
            return -1;
        }
        g_restore_rights[g_nrestore_rights++] = (struct ckpt_restore_right){record->ofd_id, record->object_id, fd, 1};
        return fd;
    }
    if (record->kind == CKF_INOTIFY) {
        char image_path[1400];
        snprintf(image_path, sizeof image_path, "%s", record->path);
        int64_t stored = ckpt_source_object_size(image_path);
        if (g_linux_box == NULL || stored <= 0 || (uint64_t)stored > 64u * 1024u * 1024u) return -1;
        size_t size = (size_t)stored;
        void *image = malloc(size);
        int shadow = bound_shadow_reserve(0);
        if (image == NULL || shadow < 0 || ckpt_source_load(image_path, image, size) != 0) {
            free(image);
            if (shadow >= 0) close(shadow);
            return -1;
        }
        void *provider = bound_inotify_provider_create(g_host_services);
        int64_t imported =
            provider == NULL
                ? -HL_LINUX_ENOMEM
                : hl_linux_inotify_import_at(g_linux_box, (hl_linux_fd)shadow, &bound_inotify_ops, provider,
                                             (uint32_t)record->descriptor_flags, (uint32_t)record->flags, image, size);
        free(image);
        if (imported < 0) {
            close(shadow);
            return -1;
        }
        hl_linux_fd_snapshot snapshot;
        if (!bound_snapshot((uint64_t)(uint32_t)shadow, &snapshot) ||
            bound_fdvis_publish_snapshot(shadow, &snapshot) != 0) {
            (void)hl_linux_close(g_linux_box, (hl_linux_fd)shadow);
            close(shadow);
            return -1;
        }
        g_restore_rights[g_nrestore_rights++] =
            (struct ckpt_restore_right){record->ofd_id, record->object_id, shadow, 2};
        return shadow;
    }
    if (record->kind == CKF_SIGNALFD) {
        struct ckpt_restore_signalfd *object = ckpt_restore_signalfd_find(record->object_id);
        if (object == NULL) {
            if (!record->object_id || ckpt_vector_reserve((void **)&g_restore_signalfds, &g_restore_signalfds_capacity,
                                                          sizeof *g_restore_signalfds, g_nrestore_signalfds + 1) != 0)
                return -1;
            int pair[2];
            if (pipe(pair) != 0) return -1;
            int seed_reader = fcntl(pair[0], F_DUPFD_CLOEXEC, HL_NFD);
            int seed_writer = fcntl(pair[1], F_DUPFD_CLOEXEC, HL_NFD);
            close(pair[0]);
            close(pair[1]);
            if (seed_reader < 0 || seed_writer < 0) {
                if (seed_reader >= 0) close(seed_reader);
                if (seed_writer >= 0) close(seed_writer);
                return -1;
            }
            int reader = hl_host_process_fd_private_adopt(seed_reader);
            int writer = reader >= 0 ? hl_host_process_fd_private_adopt(seed_writer) : -1;
            if (reader < 0 || writer < 0) {
                if (reader >= 0) {
                    hl_host_process_fd_private_remove(reader);
                    close(reader);
                } else
                    close(seed_reader);
                if (writer >= 0) {
                    hl_host_process_fd_private_remove(writer);
                    close(writer);
                } else
                    close(seed_writer);
                return -1;
            }
            int writer_flags = fcntl(writer, F_GETFL);
            if (writer_flags < 0 || fcntl(writer, F_SETFL, writer_flags | O_NONBLOCK) != 0) return -1;
            object = &g_restore_signalfds[g_nrestore_signalfds++];
            *object = (struct ckpt_restore_signalfd){record->object_id, record->auxiliary, reader, writer};
            char queue_path[1300];
            snprintf(queue_path, sizeof queue_path, "%s", record->path);
            FILE *queue = ckpt_source_fopen(queue_path);
            if (queue != NULL) {
                unsigned char bytes[4096];
                size_t count;
                while ((count = fread(bytes, 1, sizeof bytes, queue)) != 0) {
                    size_t offset = 0;
                    while (offset < count) {
                        ssize_t written = write(writer, bytes + offset, count - offset);
                        if (written > 0)
                            offset += (size_t)written;
                        else if (written < 0 && errno == EINTR)
                            continue;
                        else {
                            ckpt_source_fclose(queue);
                            return -1;
                        }
                    }
                }
                if (ferror(queue)) {
                    ckpt_source_fclose(queue);
                    return -1;
                }
                ckpt_source_fclose(queue);
            }
        } else if (object->mask != record->auxiliary) {
            return -1;
        }
        g_restore_rights[g_nrestore_rights++] =
            (struct ckpt_restore_right){record->ofd_id, record->object_id, object->reader, 0};
        return object->reader;
    }
    if (record->kind == CKF_PIPE) {
        uint64_t identity = (uint64_t)record->offset;
        struct ckpt_restore_pipe *pipe_object = ckpt_restore_pipe_find(identity);
        if (pipe_object == NULL) {
            if (!identity || ckpt_vector_reserve((void **)&g_restore_pipes, &g_restore_pipes_capacity,
                                                 sizeof *g_restore_pipes, g_nrestore_pipes + 1) != 0)
                return -1;
            pipe_object = &g_restore_pipes[g_nrestore_pipes++];
            *pipe_object = (struct ckpt_restore_pipe){
                .identity = identity, .reader = -1, .writer = -1, .size = atoi(record->path)};
            int pair[2];
            if (pipe(pair) != 0) return -1;
            if (fcntl(pair[0], F_SETFD, FD_CLOEXEC) != 0 || fcntl(pair[1], F_SETFD, FD_CLOEXEC) != 0) {
                close(pair[0]);
                close(pair[1]);
                return -1;
            }
#ifdef F_SETPIPE_SZ
            if (pipe_object->size > 0) (void)fcntl(pair[1], F_SETPIPE_SZ, pipe_object->size);
#endif
            pipe_object->reader = hl_host_process_fd_private_adopt(pair[0]);
            pipe_object->writer = pipe_object->reader >= 0 ? hl_host_process_fd_private_adopt(pair[1]) : -1;
            if (pipe_object->reader < 0 || pipe_object->writer < 0) {
                if (pipe_object->reader >= 0) {
                    hl_host_process_fd_private_remove(pipe_object->reader);
                    close(pipe_object->reader);
                } else
                    close(pair[0]);
                if (pipe_object->writer >= 0) {
                    hl_host_process_fd_private_remove(pipe_object->writer);
                    close(pipe_object->writer);
                } else
                    close(pair[1]);
                return -1;
            }
            int writer_flags = fcntl(pipe_object->writer, F_GETFL);
            if (writer_flags < 0 || fcntl(pipe_object->writer, F_SETFL, writer_flags | O_NONBLOCK) != 0) return -1;
            char data_path[1300];
            snprintf(data_path, sizeof data_path, "pipe.%016llx", (unsigned long long)identity);
            FILE *data = ckpt_source_fopen(data_path);
            if (data != NULL) {
                unsigned char buffer[65536];
                size_t count;
                while ((count = fread(buffer, 1, sizeof buffer, data)) != 0) {
                    size_t offset = 0;
                    while (offset < count) {
                        ssize_t written = write(pipe_object->writer, buffer + offset, count - offset);
                        if (written > 0)
                            offset += (size_t)written;
                        else if (written < 0 && errno == EINTR)
                            continue;
                        else {
                            ckpt_source_fclose(data);
                            return -1;
                        }
                    }
                }
                if (ferror(data)) {
                    ckpt_source_fclose(data);
                    return -1;
                }
                ckpt_source_fclose(data);
            }
        }
        fd = ((record->flags & O_ACCMODE) == O_WRONLY) ? pipe_object->writer : pipe_object->reader;
        if (fd < 0) return -1;
        g_restore_rights[g_nrestore_rights++] = (struct ckpt_restore_right){record->ofd_id, record->object_id, fd, 0};
        return fd;
    }
    if (record->kind == CKF_EVENTFD) {
        struct ckpt_restore_eventfd *eventfd = ckpt_restore_eventfd_find(record->object_id);
        if (eventfd == NULL) {
            if (ckpt_vector_reserve((void **)&g_restore_eventfds, &g_restore_eventfds_capacity,
                                    sizeof *g_restore_eventfds, g_nrestore_eventfds + 1) != 0)
                return -1;
            int slot = (int)((record->object_id & UINT64_C(0xffffffff)) - 1);
            if (slot < 0 || slot >= HL_NFD) return -1;
            int pair[2];
            if (pipe(pair) != 0) return -1;
            int flags = fcntl(pair[0], F_GETFL);
            if (flags < 0 || fcntl(pair[0], F_SETFL, flags | O_NONBLOCK) != 0) {
                close(pair[0]);
                close(pair[1]);
                return -1;
            }
            int reader = hl_host_process_fd_private_adopt(pair[0]);
            int writer = reader >= 0 ? hl_host_process_fd_private_adopt(pair[1]) : -1;
            if (reader < 0 || writer < 0) {
                if (reader >= 0) {
                    hl_host_process_fd_private_remove(reader);
                    close(reader);
                } else
                    close(pair[0]);
                if (writer >= 0) {
                    hl_host_process_fd_private_remove(writer);
                    close(writer);
                } else
                    close(pair[1]);
                return -1;
            }
            eventfd = &g_restore_eventfds[g_nrestore_eventfds++];
            *eventfd = (struct ckpt_restore_eventfd){
                .identity = record->object_id,
                .count = record->auxiliary,
                .reader = reader,
                .writer = writer,
                .slot = slot,
                .semaphore = record->offset != 0,
                .guest_nonblock = (record->flags & O_NONBLOCK) != 0,
            };
            if (eventfd->count != 0) {
                char byte = 1;
                if (write(writer, &byte, 1) != 1) return -1;
            }
        } else if (eventfd->count != record->auxiliary || eventfd->semaphore != (record->offset != 0) ||
                   eventfd->guest_nonblock != ((record->flags & O_NONBLOCK) != 0)) {
            return -1;
        }
        g_restore_rights[g_nrestore_rights++] =
            (struct ckpt_restore_right){record->ofd_id, record->object_id, eventfd->reader, 0};
        return eventfd->reader;
    }
    if (record->kind == CKF_TIMERFD) {
        struct ckpt_restore_timerfd *timerfd = ckpt_restore_timerfd_find(record->object_id);
        if (timerfd == NULL) {
            int clock_id = 0;
            unsigned first = 0;
            unsigned long long pending = 0;
            long long captured_ns = 0;
            if (!record->object_id ||
                ckpt_vector_reserve((void **)&g_restore_timerfds, &g_restore_timerfds_capacity,
                                    sizeof *g_restore_timerfds, g_nrestore_timerfds + 1) != 0 ||
                sscanf(record->path, "%d %llu %u %lld", &clock_id, &pending, &first, &captured_ns) != 4)
                return -1;
            struct timerfd_shared_state *state =
                mmap(NULL, sizeof *state, PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
            if (state == MAP_FAILED) return -1;
            memset(state, 0, sizeof *state);
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
            int64_t deadline = record->offset;
            int64_t interval = (int64_t)record->auxiliary;
            int64_t next = deadline;
            uint64_t accumulated = (uint64_t)pending;
            if (deadline > 0 && interval > 0) {
                if (next <= captured_ns) next += ((captured_ns - next) / interval + 1) * interval;
                if (now_ns >= next) {
                    accumulated += 1 + (uint64_t)((now_ns - next) / interval);
                    next += ((now_ns - next) / interval + 1) * interval;
                }
            } else if (deadline > 0 && now_ns >= deadline) {
                accumulated = 1;
                next = 0;
            }
            state->deadline = next;
            state->interval = interval;
            state->pending = accumulated;
            g_restore_timerfds[g_nrestore_timerfds++] = (struct ckpt_restore_timerfd){
                .identity = record->object_id,
                .state = state,
                .clock_id = clock_id,
                .fd = -1,
                .slot = -1,
                .first_oneshot = (uint8_t)(first != 0),
            };
            timerfd = &g_restore_timerfds[g_nrestore_timerfds - 1];
        }
        if (timerfd->state == NULL) return -1;
        if (timerfd->fd < 0) {
            int timer = kqueue();
            if (timer < 0) return -1;
            timerfd->fd = hl_host_process_fd_private_adopt(timer);
            if (timerfd->fd < 0) {
                hl_native_kqueue_close(timer);
                close(timer);
                return -1;
            }
            // adopt() moves the descriptor and closes the original, so a number-keyed shim kqueue must move
            // with it or the arming kevent() below is EBADF.
            hl_native_kqueue_relocate(timer, timerfd->fd);
            timerfd->slot = timerfd->fd;
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
            timerfd_shared_lock(timerfd->state);
            int64_t next = timerfd->state->deadline;
            uint64_t pending = timerfd->state->pending;
            timerfd_shared_unlock(timerfd->state);
            if (pending != 0 || next > now_ns) {
                struct kevent event;
                int64_t delay = pending != 0 ? 1 : next - now_ns;
                EV_SET(&event, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS, delay, NULL);
                if (kevent(timerfd->fd, &event, 1, NULL, 0, NULL) < 0) return -1;
            }
        }
        g_restore_rights[g_nrestore_rights++] =
            (struct ckpt_restore_right){record->ofd_id, record->object_id, timerfd->fd, 0};
        return timerfd->fd;
    }
    if (record->kind == CKF_FILE || record->kind == CKF_DEVICE) {
        int open_flags = record->flags & (O_ACCMODE | O_APPEND | O_NONBLOCK);
        fd = open(record->path, open_flags);
        if (record->kind == CKF_FILE && fd < 0 && (open_flags & O_ACCMODE) == O_RDWR) fd = open(record->path, O_RDONLY);
    } else {
        char temporary[] = "/tmp/.hl-restore-rightXXXXXX";
        fd = mkstemp(temporary);
        if (fd >= 0) unlink(temporary);
        if (fd < 0 || ckpt_source_copy_to_fd(record->path, fd) != 0) {
            if (fd >= 0) close(fd);
            return fprintf(stderr, "[restore] queued right blob %s copy failed: %s\n", record->path, strerror(errno)),
                   -1;
        }
        int live_flags = fcntl(fd, F_GETFL);
        if (live_flags < 0 || fcntl(fd, F_SETFL, (live_flags & O_ACCMODE) | (record->flags & ~O_ACCMODE)) != 0) {
            close(fd);
            return fprintf(stderr, "[restore] queued right flags failed: %s\n", strerror(errno)), -1;
        }
    }
    if (fd < 0 || (record->kind != CKF_DEVICE && lseek(fd, (off_t)record->offset, SEEK_SET) != (off_t)record->offset)) {
        if (fd >= 0) close(fd);
        return fprintf(stderr, "[restore] queued right open/seek kind=%d path=%s offset=%lld: %s\n", record->kind,
                       record->path, (long long)record->offset, strerror(errno)),
               -1;
    }
    int adopted = hl_host_process_fd_private_adopt(fd);
    if (adopted < 0) {
        close(fd);
        return fprintf(stderr, "[restore] queued right adopt failed: %s\n", strerror(errno)), -1;
    }
    if (record->kind == CKF_MEMFD) {
        g_memfd_is[adopted] = 1;
        g_memfd_seal[adopted] = (int)record->auxiliary;
        memfd_reg_set_fd(adopted, g_memfd_seal[adopted]);
    }
    g_restore_rights[g_nrestore_rights++] = (struct ckpt_restore_right){record->ofd_id, record->object_id, adopted, 1};
    return adopted;
}

static int ckpt_restore_socket_queue_load(struct ckpt_restore_socket_endpoint *endpoint) {
    char path[1300];
    snprintf(path, sizeof path, "socket.%016llx", (unsigned long long)endpoint->identity);
    FILE *file = ckpt_source_fopen(path);
    if (!file) return errno == ENOENT ? 0 : -1;
    struct ckpt_socket_queue_header header;
    if (ckpt_rd_all(file, &header, sizeof header) != 0 || header.magic != CKPT_SOCKET_QUEUE_MAGIC ||
        header.type != (uint32_t)endpoint->type) {
        ckpt_source_fclose(file);
        return -1;
    }
    endpoint->peer_closed = header.peer_closed != 0;
    struct ckpt_restore_socket_endpoint *peer = ckpt_restore_socket_find(endpoint->peer_identity);
    if (peer == NULL || peer->fd < 0) {
        ckpt_source_fclose(file);
        return -1;
    }
    for (;;) {
        struct ckpt_socket_queue_frame frame;
        size_t frame_bytes = fread(&frame, 1, sizeof frame, file);
        if (frame_bytes == 0 && feof(file)) break;
        if (frame_bytes != sizeof frame || ferror(file) || frame.rights_count > 253 || frame.size > (1u << 20)) {
            ckpt_source_fclose(file);
            return -1;
        }
        unsigned char *payload = malloc(frame.size ? frame.size : 1u);
        if (payload == NULL || (frame.size != 0 && fread(payload, 1, frame.size, file) != frame.size)) {
            free(payload);
            ckpt_source_fclose(file);
            return -1;
        }
        struct ckpt_fd rights[253];
        int right_fds[253];
        if (frame.rights_count != 0 &&
            fread(rights, sizeof rights[0], frame.rights_count, file) != frame.rights_count) {
            free(payload);
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_fd_terminate_all(rights, frame.rights_count);
        for (uint32_t index = 0; index < frame.rights_count; ++index) {
            right_fds[index] = ckpt_restore_right_prepare(&rights[index]);
            if (right_fds[index] < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
        }
        size_t offset = 0;
        if (frame.rights_count != 0) {
            int combo[253 * 4];
            int combo_count = 0;
            cmsg_tmpfds_close();
            for (uint32_t index = 0; index < frame.rights_count; ++index)
                combo[combo_count++] = right_fds[index];
            for (uint32_t index = 0; index < frame.rights_count; ++index) {
                if (rights[index].kind == CKF_EVENTFD) {
                    struct ckpt_restore_eventfd *eventfd = ckpt_restore_eventfd_find(rights[index].object_id);
                    if (eventfd == NULL || combo_count + 2 > (int)(sizeof combo / sizeof combo[0])) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    struct hl_cmsg_eventfd_meta metadata = {
                        .magic = HL_CMSG_EVENTFD_MAGIC,
                        .ordinal = index,
                        .slot = (uint32_t)eventfd->slot,
                        .sema = (uint32_t)eventfd->semaphore,
                        .nb = (uint32_t)eventfd->guest_nonblock,
                    };
                    int writer_flags = fcntl(eventfd->writer, F_GETFL);
                    if (writer_flags < 0 || fcntl(eventfd->writer, F_SETFL, writer_flags | O_NONBLOCK) != 0 ||
                        fcntl(eventfd->writer, F_SETFD, FD_CLOEXEC) != 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    int marker = cmsg_eventfd_marker(&metadata);
                    if (marker < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[combo_count++] = eventfd->writer;
                    combo[combo_count++] = marker;
                }
                if (rights[index].kind == CKF_TIMERFD) {
                    struct ckpt_restore_timerfd *timerfd = ckpt_restore_timerfd_find(rights[index].object_id);
                    if (timerfd == NULL || timerfd->state == NULL ||
                        combo_count + 1 > (int)(sizeof combo / sizeof combo[0])) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    struct hl_cmsg_timerfd_meta metadata = {
                        .magic = HL_CMSG_TIMERFD_MAGIC,
                        .ordinal = index,
                        .first_oneshot = timerfd->first_oneshot,
                        .clock = timerfd->clock_id,
                        .deadline = timerfd->state->deadline,
                        .interval = timerfd->state->interval,
                        .source_fd = timerfd->fd,
                        .source_pid = (int32_t)getpid(),
                        .nb = (rights[index].flags & O_NONBLOCK) != 0,
                        .portable = 1,
                        .restore_shared = 1,
                        .object = timerfd->identity,
                        .shared_state = (uint64_t)(uintptr_t)timerfd->state,
                    };
                    struct hl_cmsg_timerfd_meta placeholder_metadata;
                    memset(&placeholder_metadata, 0, sizeof placeholder_metadata);
                    int placeholder = cmsg_timerfd_marker(&placeholder_metadata);
                    if (placeholder < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[index] = placeholder;
                    int marker = cmsg_timerfd_marker(&metadata);
                    if (marker < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[combo_count++] = marker;
                }
                if (rights[index].kind == CKF_PIPE) {
                    uint64_t identity = (uint64_t)rights[index].offset;
                    if (!identity || combo_count + 1 > (int)(sizeof combo / sizeof combo[0])) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    struct hl_cmsg_pipe_meta metadata = {
                        .magic = UINT32_C(0x484c5049),
                        .ordinal = index,
                        .identity = identity,
                        .size = atoi(rights[index].path),
                    };
                    int marker = cmsg_pipe_marker(&metadata);
                    if (marker < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[combo_count++] = marker;
                }
                if (rights[index].kind == CKF_SIGNALFD) {
                    struct ckpt_restore_signalfd *object = ckpt_restore_signalfd_find(rights[index].object_id);
                    if (object == NULL || combo_count + 2 > (int)(sizeof combo / sizeof combo[0])) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    struct hl_cmsg_signalfd_meta metadata = {
                        .magic = UINT32_C(0x484c5346),
                        .ordinal = index,
                        .source_pid = (int32_t)getpid(),
                        .source_slot = -1,
                        .mask = object->mask,
                    };
                    int marker = cmsg_signalfd_marker(&metadata);
                    if (marker < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[combo_count++] = object->writer;
                    combo[combo_count++] = marker;
                }
                if (rights[index].kind == CKF_INOTIFY) {
                    if (combo_count + 1 > (int)(sizeof combo / sizeof combo[0])) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    struct hl_cmsg_kqueue_meta metadata = {
                        .magic = UINT32_C(0x484c4b51),
                        .ordinal = index,
                        .source_pid = (int32_t)getpid(),
                        .source_fd = right_fds[index],
                        .kind = 3,
                        .nonblock = (rights[index].flags & O_NONBLOCK) != 0,
                        .object_id = rights[index].object_id,
                        .descriptor_flags = (uint32_t)rights[index].descriptor_flags,
                    };
                    int placeholder = cmsg_kqueue_placeholder();
                    if (placeholder < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[index] = placeholder;
                    int marker = cmsg_kqueue_marker(&metadata);
                    if (marker < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[combo_count++] = marker;
                }
                if (rights[index].kind == CKF_EPOLL) {
                    if (combo_count + 1 > (int)(sizeof combo / sizeof combo[0])) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    int placeholder = cmsg_kqueue_placeholder();
                    if (placeholder < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[index] = placeholder;
                    int marker = ckpt_restore_epoll_marker(&rights[index], index);
                    if (marker < 0) {
                        free(payload);
                        ckpt_source_fclose(file);
                        return -1;
                    }
                    combo[combo_count++] = marker;
                }
            }
            for (uint32_t index = 0; index < frame.rights_count; ++index) {
                if (combo_count == (int)(sizeof combo / sizeof combo[0])) {
                    cmsg_tmpfds_close();
                    free(payload);
                    ckpt_source_fclose(file);
                    return -1;
                }
                struct hl_cmsg_ofd_meta metadata = {
                    .magic = HL_CMSG_OFD_MAGIC,
                    .ordinal = index,
                    .identity = rights[index].ofd_id,
                };
                int marker = cmsg_ofd_marker(&metadata);
                if (marker < 0) {
                    cmsg_tmpfds_close();
                    free(payload);
                    ckpt_source_fclose(file);
                    return -1;
                }
                combo[combo_count++] = marker;
            }
            unsigned char control[CMSG_SPACE(253 * 4 * sizeof(int))];
            struct iovec iov = {payload, frame.size};
            struct msghdr message;
            memset(&message, 0, sizeof message);
            message.msg_iov = &iov;
            message.msg_iovlen = 1;
            message.msg_control = control;
            message.msg_controllen = CMSG_SPACE((size_t)combo_count * sizeof(int));
            memset(control, 0, message.msg_controllen);
            struct cmsghdr *header = CMSG_FIRSTHDR(&message);
            header->cmsg_level = SOL_SOCKET;
            header->cmsg_type = SCM_RIGHTS;
            header->cmsg_len = CMSG_LEN((size_t)combo_count * sizeof(int));
            memcpy(CMSG_DATA(header), combo, (size_t)combo_count * sizeof(int));
            ssize_t sent;
            do {
                sent = sendmsg(peer->fd, &message, 0);
            } while (sent < 0 && errno == EINTR);
            cmsg_tmpfds_close();
            if (sent < 0 || (endpoint->type != SOCK_STREAM && sent != (ssize_t)frame.size)) {
                fprintf(stderr, "[restore] queued rights send endpoint=%llx count=%u size=%u sent=%lld: %s\n",
                        (unsigned long long)endpoint->identity, frame.rights_count, frame.size, (long long)sent,
                        strerror(errno));
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            offset = (size_t)sent;
        }
        if (frame.size == 0 && frame.rights_count == 0) {
            ssize_t sent;
            do {
                sent = send(peer->fd, "", 0, 0);
            } while (sent < 0 && errno == EINTR);
            if (sent < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
        }
        while (offset < frame.size) {
            ssize_t sent = send(peer->fd, payload + offset, frame.size - offset, 0);
            if (sent > 0) {
                offset += (size_t)sent;
                continue;
            }
            if (sent < 0 && errno == EINTR) continue;
            free(payload);
            ckpt_source_fclose(file);
            return -1;
        }
        free(payload);
    }
    ckpt_source_fclose(file);
    return 0;
}

// SO_RCVBUF/SO_SNDBUF do not round-trip on Linux: the kernel stores twice what setsockopt gets, getsockopt
// reports the doubled value, so replaying a capture doubles the buffer each generation. Halve on the way in
// (Linux only -- macOS stores what it is given).
static int ckpt_restore_socket_buffer(int fd, int name, int32_t captured) {
    int value = captured;
#if defined(__linux__)
    value = captured / 2;
#endif
    return setsockopt(fd, SOL_SOCKET, name, &value, sizeof value);
}

static int ckpt_restore_socket_options(int fd, const struct ckpt_socket_state *state) {
#define CKPT_RESTORE_SOCKET_OPTION(name, field)                                                                        \
    do {                                                                                                               \
        if (setsockopt(fd, SOL_SOCKET, name, &state->field, sizeof state->field) != 0) return -1;                      \
    } while (0)
    if (ckpt_restore_socket_buffer(fd, SO_RCVBUF, state->receive_buffer) != 0 ||
        ckpt_restore_socket_buffer(fd, SO_SNDBUF, state->send_buffer) != 0)
        return -1;
    CKPT_RESTORE_SOCKET_OPTION(SO_REUSEADDR, reuse_address);
    CKPT_RESTORE_SOCKET_OPTION(SO_REUSEPORT, reuse_port);
    CKPT_RESTORE_SOCKET_OPTION(SO_KEEPALIVE, keepalive);
    CKPT_RESTORE_SOCKET_OPTION(SO_BROADCAST, broadcast);
    CKPT_RESTORE_SOCKET_OPTION(SO_LINGER, linger);
#undef CKPT_RESTORE_SOCKET_OPTION
    return 0;
}

static int ckpt_prepare_restore_sockets(void) {
    g_nrestore_socket_endpoints = 0;
    g_nrestore_rights = 0;
    for (int process = 0; process < g_nrprocs; ++process) {
        char path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_SOCKETPAIR) continue;
            struct ckpt_restore_socket_endpoint *endpoint = ckpt_restore_socket_find(record.object_id);
            if (endpoint != NULL) {
                if (endpoint->peer_identity != record.auxiliary || endpoint->type != record.offset) {
                    ckpt_source_fclose(file);
                    return -1;
                }
                endpoint->guest_present = 1;
                continue;
            }
            if (!record.object_id || !record.auxiliary ||
                (record.offset != SOCK_STREAM && record.offset != SOCK_DGRAM && record.offset != SOCK_SEQPACKET) ||
                ckpt_vector_reserve((void **)&g_restore_socket_endpoints, &g_restore_socket_endpoints_capacity,
                                    sizeof *g_restore_socket_endpoints, g_nrestore_socket_endpoints + 1) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            endpoint = &g_restore_socket_endpoints[g_nrestore_socket_endpoints++];
            *endpoint = (struct ckpt_restore_socket_endpoint){
                .identity = record.object_id,
                .peer_identity = record.auxiliary,
                .fd = -1,
                .type = (int)record.offset,
                .guest_present = 1,
            };
            char state_path[1400];
            snprintf(state_path, sizeof state_path, "socket-state.%016llx", (unsigned long long)record.object_id);
            if (ckpt_source_load(state_path, &endpoint->state, sizeof endpoint->state) != 0 ||
                endpoint->state.magic != CKPT_SOCKET_STATE_MAGIC ||
                endpoint->state.local_size > sizeof endpoint->state.local) {
                ckpt_source_fclose(file);
                return -1;
            }
            endpoint->state_loaded = 1;
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index) {
        struct ckpt_restore_socket_endpoint *endpoint = &g_restore_socket_endpoints[index];
        if (ckpt_restore_socket_find(endpoint->peer_identity) != NULL) continue;
        uint64_t identity = endpoint->identity;
        uint64_t peer_identity = endpoint->peer_identity;
        int type = endpoint->type;
        if (ckpt_vector_reserve((void **)&g_restore_socket_endpoints, &g_restore_socket_endpoints_capacity,
                                sizeof *g_restore_socket_endpoints, g_nrestore_socket_endpoints + 1) != 0)
            return -1;
        struct ckpt_restore_socket_endpoint *peer = &g_restore_socket_endpoints[g_nrestore_socket_endpoints++];
        *peer = (struct ckpt_restore_socket_endpoint){
            .identity = peer_identity,
            .peer_identity = identity,
            .fd = -1,
            .type = type,
        };
    }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index) {
        struct ckpt_restore_socket_endpoint *endpoint = &g_restore_socket_endpoints[index];
        if (endpoint->fd >= 0) continue;
        struct ckpt_restore_socket_endpoint *peer = ckpt_restore_socket_find(endpoint->peer_identity);
        if (peer == NULL || peer->peer_identity != endpoint->identity || peer->type != endpoint->type) return -1;
        int pair[2];
        int host_type = endpoint->type == SOCK_SEQPACKET ? SOCK_DGRAM : endpoint->type;
        if (socketpair(AF_UNIX, host_type, 0, pair) != 0) return -1;
        endpoint->fd = hl_host_process_fd_private_adopt(pair[0]);
        peer->fd = hl_host_process_fd_private_adopt(pair[1]);
        if (endpoint->fd < 0 || peer->fd < 0) {
            if (endpoint->fd >= 0) {
                hl_host_process_fd_private_remove(endpoint->fd);
                close(endpoint->fd);
            } else
                close(pair[0]);
            if (peer->fd >= 0) {
                hl_host_process_fd_private_remove(peer->fd);
                close(peer->fd);
            } else
                close(pair[1]);
            return -1;
        }
        (void)hl_native_set_no_sigpipe(endpoint->fd);
        (void)hl_native_set_no_sigpipe(peer->fd);
    }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index) {
        struct ckpt_restore_socket_endpoint *endpoint = &g_restore_socket_endpoints[index];
        if (endpoint->state_loaded && ckpt_restore_socket_options(endpoint->fd, &endpoint->state) != 0) return -1;
    }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index)
        if (g_restore_socket_endpoints[index].guest_present &&
            ckpt_restore_socket_queue_load(&g_restore_socket_endpoints[index]) != 0) {
            fprintf(stderr, "[restore] socket queue load failed endpoint=%016llx\n",
                    (unsigned long long)g_restore_socket_endpoints[index].identity);
            return -1;
        }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index) {
        struct ckpt_restore_socket_endpoint *endpoint = &g_restore_socket_endpoints[index];
        if (!endpoint->peer_closed) continue;
        struct ckpt_restore_socket_endpoint *peer = ckpt_restore_socket_find(endpoint->peer_identity);
        if (peer == NULL) return -1;
        if (peer->guest_present) {
            endpoint->peer_closed = 0;
            continue;
        }
        if (peer->fd >= 0) {
            hl_host_process_fd_private_remove(peer->fd);
            close(peer->fd);
            peer->fd = -1;
        }
    }
    return 0;
}

static int ckpt_socket_state_is_bound(const struct ckpt_socket_state *state) {
    if (state->host_family == AF_UNIX) {
        const struct sockaddr_un *address = (const void *)&state->local;
        return state->local_size > offsetof(struct sockaddr_un, sun_path) &&
               (address->sun_path[0] != 0 || state->local_size > offsetof(struct sockaddr_un, sun_path) + 1u);
    }
    if (state->host_family == AF_INET) return ((const struct sockaddr_in *)&state->local)->sin_port != 0;
    if (state->host_family == AF_INET6) return ((const struct sockaddr_in6 *)&state->local)->sin6_port != 0;
    return 0;
}

static int ckpt_prepare_restore_socket_states(void) {
    g_nrestore_sockets = 0;
    for (int process = 0; process < g_nrprocs; ++process) {
        char records_path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(records_path, sizeof records_path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *records = ckpt_source_fopen(records_path);
        if (!records) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(records, &record) == 0) {
            if (record.kind != CKF_SOCKET || ckpt_restore_socket_state_find(record.object_id) != NULL) continue;
            if (!record.object_id || ckpt_vector_reserve((void **)&g_restore_sockets, &g_restore_sockets_capacity,
                                                         sizeof *g_restore_sockets, g_nrestore_sockets + 1) != 0) {
                ckpt_source_fclose(records);
                return -1;
            }
            struct ckpt_restore_socket *socket_state = &g_restore_sockets[g_nrestore_sockets++];
            *socket_state = (struct ckpt_restore_socket){.identity = record.object_id, .fd = -1};
            char state_path[1400];
            snprintf(state_path, sizeof state_path, "%s", record.path);
            if (ckpt_source_load(state_path, &socket_state->state, sizeof socket_state->state) != 0 ||
                socket_state->state.magic != CKPT_SOCKET_STATE_MAGIC ||
                socket_state->state.local_size > sizeof socket_state->state.local)
                return -1;
        }
        if (!feof(records)) {
            ckpt_source_fclose(records);
            return -1;
        }
        ckpt_source_fclose(records);
    }
    for (int index = 0; index < g_nrestore_sockets; ++index) {
        struct ckpt_restore_socket *saved = &g_restore_sockets[index];
        struct ckpt_socket_state *state = &saved->state;
        if (state->host_family == AF_UNIX &&
            (state->udp_local_port != 0 || state->lo_port != 0 || state->br_port != 0)) {
            char virtual_path[200];
            if (state->udp_local_port != 0) {
                if (state->udp_local_interface != 0) {
                    if (br_path((int)state->udp_local_interface - 1, state->udp_local_ip,
                                (uint16_t)state->udp_local_port, virtual_path, sizeof virtual_path) != 0)
                        return -1;
                } else {
                    lo_path((uint16_t)state->udp_local_port, virtual_path, sizeof virtual_path);
                }
            } else if (state->br_port != 0) {
                if (br_path((int)state->br_interface - 1, state->br_ip, (uint16_t)state->br_port, virtual_path,
                            sizeof virtual_path) != 0)
                    return -1;
            } else {
                lo_tcp_path((uint16_t)state->lo_port, state->lo_v6only, virtual_path, sizeof virtual_path);
            }
            struct sockaddr_un address;
            if (unix_addr_set(&address, virtual_path) != 0) return -1;
            memset(&state->local, 0, sizeof state->local);
            memcpy(&state->local, &address, sizeof address);
            state->local_size = sizeof address;
        }
        int fd = socket((int)state->host_family, (int)state->type, (int)state->protocol);
        if (fd < 0) {
            fprintf(stderr, "[restore] socket %016llx create family=%u type=%u protocol=%u: %s\n",
                    (unsigned long long)saved->identity, state->host_family, state->type, state->protocol,
                    strerror(errno));
            return -1;
        }
        (void)hl_native_set_no_sigpipe(fd);
        if (ckpt_restore_socket_options(fd, state) != 0) {
            close(fd);
            return -1;
        }
        if (ckpt_socket_state_is_bound(state)) {
            if (state->host_family == AF_UNIX) {
                struct sockaddr_un *address = (void *)&state->local;
                if (address->sun_path[0] != 0) unlink(address->sun_path);
            }
            if (bind(fd, (struct sockaddr *)&state->local, (socklen_t)state->local_size) != 0) {
                fprintf(stderr, "[restore] socket %016llx bind failed: %s\n", (unsigned long long)saved->identity,
                        strerror(errno));
                close(fd);
                return -1;
            }
        }
        if (state->listening && listen(fd, state->backlog) != 0) {
            fprintf(stderr, "[restore] socket %016llx listen backlog=%d failed: %s\n",
                    (unsigned long long)saved->identity, state->backlog, strerror(errno));
            close(fd);
            return -1;
        }
        saved->fd = hl_host_process_fd_private_adopt(fd);
        if (saved->fd < 0) {
            close(fd);
            return -1;
        }
    }
    return 0;
}

static int ckpt_prepare_restore_eventfds(void) {
    g_nrestore_eventfds = 0;
    for (int process = 0; process < g_nrprocs; process++) {
        char path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_EVENTFD) continue;
            struct ckpt_restore_eventfd *object = ckpt_restore_eventfd_find(record.object_id);
            if (object) {
                if (object->count != record.auxiliary || object->semaphore != (record.offset != 0) ||
                    object->guest_nonblock != ((record.flags & O_NONBLOCK) != 0)) {
                    ckpt_source_fclose(file);
                    return -1;
                }
                continue;
            }
            if (!record.object_id || ckpt_vector_reserve((void **)&g_restore_eventfds, &g_restore_eventfds_capacity,
                                                         sizeof *g_restore_eventfds, g_nrestore_eventfds + 1) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            object = &g_restore_eventfds[g_nrestore_eventfds++];
            *object = (struct ckpt_restore_eventfd){
                .identity = record.object_id,
                .count = record.auxiliary,
                .reader = -1,
                .writer = -1,
                .slot = (int)((record.object_id & UINT64_C(0xffffffff)) - 1),
                .semaphore = record.offset != 0,
                .guest_nonblock = (record.flags & O_NONBLOCK) != 0,
            };
            if (object->slot < 0 || object->slot >= HL_NFD) {
                ckpt_source_fclose(file);
                return -1;
            }
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    for (int i = 0; i < g_nrestore_eventfds; i++) {
        int pair[2];
        if (pipe(pair) != 0) return -1;
        int flags = fcntl(pair[0], F_GETFL);
        if (flags < 0 || fcntl(pair[0], F_SETFL, flags | O_NONBLOCK) != 0) {
            close(pair[0]);
            close(pair[1]);
            return -1;
        }
        int reader = hl_host_process_fd_private_adopt(pair[0]);
        if (reader < 0) {
            close(pair[0]);
            close(pair[1]);
            return -1;
        }
        int writer = hl_host_process_fd_private_adopt(pair[1]);
        if (writer < 0) {
            hl_host_process_fd_private_remove(reader);
            close(reader);
            close(pair[1]);
            return -1;
        }
        g_restore_eventfds[i].reader = reader;
        g_restore_eventfds[i].writer = writer;
        if (g_restore_eventfds[i].count != 0) {
            char byte = 1;
            if (write(writer, &byte, 1) != 1) return -1;
        }
    }
    return 0;
}

static int ckpt_prepare_restore_signalfds(void) {
    g_nrestore_signalfds = 0;
    for (int process = 0; process < g_nrprocs; ++process) {
        char path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_SIGNALFD || ckpt_restore_signalfd_find(record.object_id)) continue;
            if (ckpt_restore_right_prepare(&record) < 0) {
                ckpt_source_fclose(file);
                return -1;
            }
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    g_nrestore_rights = 0;
    return 0;
}

static int ckpt_prepare_restore_timerfds(void) {
    g_nrestore_timerfds = 0;
    for (int process = 0; process < g_nrprocs; process++) {
        char path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_TIMERFD || ckpt_restore_timerfd_find(record.object_id)) continue;
            if (!record.object_id || ckpt_vector_reserve((void **)&g_restore_timerfds, &g_restore_timerfds_capacity,
                                                         sizeof *g_restore_timerfds, g_nrestore_timerfds + 1) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            int clock_id = 0;
            unsigned first = 0;
            unsigned long long pending = 0;
            long long captured_ns = 0;
            if (sscanf(record.path, "%d %llu %u %lld", &clock_id, &pending, &first, &captured_ns) != 4) {
                ckpt_source_fclose(file);
                return -1;
            }
            struct timerfd_shared_state *state =
                mmap(NULL, sizeof *state, PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
            if (state == MAP_FAILED) {
                ckpt_source_fclose(file);
                return -1;
            }
            memset(state, 0, sizeof *state);
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
            int64_t deadline = record.offset;
            int64_t interval = (int64_t)record.auxiliary;
            int64_t next = deadline;
            uint64_t accumulated = (uint64_t)pending;
            if (deadline > 0 && interval > 0) {
                if (next <= captured_ns) next += ((captured_ns - next) / interval + 1) * interval;
                if (now_ns >= next) {
                    accumulated += 1 + (uint64_t)((now_ns - next) / interval);
                    next += ((now_ns - next) / interval + 1) * interval;
                }
            } else if (deadline > 0 && now_ns >= deadline) {
                accumulated = 1;
                next = 0;
            }
            state->deadline = next;
            state->interval = interval;
            state->pending = accumulated;
            g_restore_timerfds[g_nrestore_timerfds++] = (struct ckpt_restore_timerfd){
                .identity = record.object_id,
                .state = state,
                .clock_id = clock_id,
                .fd = -1,
                .slot = -1,
                .first_oneshot = (uint8_t)(first != 0),
            };
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    return 0;
}

static int ckpt_restore_eventfds_initialize(void) {
    if (g_nrestore_eventfds != 0 && !g_eventfd_count) return -1;
    for (int i = 0; i < g_nrestore_eventfds; i++)
        g_eventfd_count[g_restore_eventfds[i].slot] = g_restore_eventfds[i].count;
    return 0;
}

// The guest group (guest pgid; 1 == init) that owned the controlling terminal's foreground at checkpoint,
// carried from the manifest so the matching re-forked leader can reclaim the tty. Inherited across the
// restore forks (set in the init before it forks the tree).
static int g_ckpt_fg_gpid = 0;

// Make THIS process's group the controlling terminal's foreground group. Called by the process that led the
// foreground job's group at checkpoint, right AFTER it re-creates that group (ckpt_restore_pgrp), so the pgid
// argument names a group that already exists. SIGTTOU is blocked so a background caller isn't stopped by the
// handoff. Best-effort -- a failure just leaves the (safe) inherited foreground group.
static void ckpt_claim_tty_fg(void) {
    int tf = ckpt_ctty_open();
    if (tf < 0) return;
    sigset_t sv, bl;
    sigemptyset(&bl);
    sigaddset(&bl, SIGTTOU);
    sigprocmask(SIG_BLOCK, &bl, &sv);
    (void)tcsetpgrp(tf, getpgrp());
    sigprocmask(SIG_SETMASK, &sv, NULL);
    ckpt_ctty_close(tf);
}

// Replay the captured line discipline onto the fresh pty. The restored guest keeps its in-memory belief about
// the terminal (readline's "already prepared" flag), so a mode it set before the capture has to be there when
// it resumes. SIGTTOU is blocked: the init is not yet the foreground group at this point. Best effort -- a
// non-tty or an unsettable mode just leaves the launcher's default cooked terminal.
static void ckpt_restore_tty_mode(const struct ckpt_manifest *man) {
    struct termios tio;
    int tf = man->tty_termios ? ckpt_ctty_open() : -1;
    if (tf < 0 || tcgetattr(tf, &tio) != 0) {
        ckpt_ctty_close(tf);
        return;
    }
    size_t cc = sizeof tio.c_cc < sizeof man->tty_cc ? sizeof tio.c_cc : sizeof man->tty_cc;
    tio.c_iflag = (tcflag_t)man->tty_iflag;
    tio.c_oflag = (tcflag_t)man->tty_oflag;
    tio.c_cflag = (tcflag_t)man->tty_cflag;
    tio.c_lflag = (tcflag_t)man->tty_lflag;
    memcpy(tio.c_cc, man->tty_cc, cc);
    (void)cfsetispeed(&tio, (speed_t)man->tty_ispeed);
    (void)cfsetospeed(&tio, (speed_t)man->tty_ospeed);
    sigset_t sv, bl;
    sigemptyset(&bl);
    sigaddset(&bl, SIGTTOU);
    sigprocmask(SIG_BLOCK, &bl, &sv);
    (void)tcsetattr(tf, TCSANOW, &tio);
    sigprocmask(SIG_SETMASK, &sv, NULL);
    ckpt_ctty_close(tf);
}

// Best-effort reconstruction of this process's group/session relative to the LIVE (re-forked) tree. A
// process that led its own group/session re-creates it; otherwise it joins its (already-inherited) parent
// group. Errors are ignored (the process is at worst left in its inherited group -- never fatal).
static void ckpt_restore_pgrp(int gpid, int pgid_gpid, int sid_gpid) {
    if (sid_gpid == gpid && getsid(0) != getpid()) setsid(); // was a session leader
    if (pgid_gpid == gpid) {
        setpgid(0, 0); // was its own group leader
    } else if (pgid_gpid > 0) {
        int leader = (pgid_gpid == 1 && g_init_hostpid) ? g_init_hostpid : hl_linux_pidmap_host(&g_pidmap, pgid_gpid);
        setpgid(0, leader); // join the (usually already-inherited) parent group
    }
}

static void ckpt_restore_proc_run(int gpid); // fwd

// Re-fork every child of `gpid` (per the checkpoint ppid table); each child restores its own subtree and
// resumes. Records the checkpoint-gpid -> live-hostpid mapping so this process's guest pids resolve.
static void ckpt_fork_children(int gpid) {
    for (int i = 0; i < g_nrprocs; i++) {
        if (!g_rprocs[i].viable || g_rprocs[i].ppid != gpid || g_rprocs[i].gpid == gpid) continue;
        int cg = g_rprocs[i].gpid;
        pid_t p = fork();
        if (p == 0) {
            ckpt_restore_proc_run(cg); // never returns
            _exit(0);
        } else if (p > 0) {
            (void)hl_linux_pidmap_add(&g_pidmap, cg, (int)p);
        } else {
            fprintf(stderr, "[restore] fork for gpid %d failed: %s\n", cg, strerror(errno));
        }
    }
}

// Restore a re-forked CHILD process (runs in the fresh fork; the engine is already inited, inherited from the
// parent) and resume it. Never returns.
static void ckpt_restore_proc_run(int gpid) {
    char pd[1200];
    ckpt_restore_hold_tty_signals();
    snprintf(pd, sizeof pd, "proc.%d", gpid);
    struct ckpt_meta m;
    if (ckpt_read_meta_dir(pd, &m) != 0) _exit(70);

    // adopt our restored identity BEFORE any pid-reporting syscall or /proc publish
    g_self_gpid = m.self_gpid;
    g_self_gppid = m.ppid_gpid;

    // The cpu image is read from the store, not from guest RAM, so it is available before the memory restore
    // -- which fork_child_hooks needs, and which now has to run FIRST. See below.
    struct cpu c, *images = NULL;
    if (ckpt_restore_cpu_dir(pd, &m, &images) != 0 || ckpt_restore_leader(images, m.n_threads, &c) != 0) _exit(70);
    // BEFORE the memory restore, not after. jit_after_fork() inside this hook rebuilds the translated-code
    // arena at a fresh VA and UNMAPS the ~64MB pair inherited from the restoring parent -- and a guest
    // mapping's saved VA is an ordinary host mmap result, so the child's MAP_FIXED regions frequently land
    // INSIDE that inherited arena. Run after the restore, the release then punched the restored guest pages
    // back out: x86_64 checkpoint.threads died with a host SIGSEGV on the resumed peer's own stack
    // (si_addr == sp, pc at glibc's __syscall_cancel_arch_end).
    fork_child_hooks(&c); // shared after-fork engine reset (cache re-alias, kqueue rebuild, lock/threg/Mach)

    // drop the COW-inherited parent guest memory + registries, then load our own
    /* The forked restorer inherited a COW copy of the parent's typed VMA ledger and
     * host mapping ownership. Release those handles before forgetting the generic
     * map registry, otherwise every restored child leaks its parent's mappings. */
    bound_mapping_reset();
    hl_logical_vma_global_reset_quiescent();
    hl_gmap_reset();
    g_nanonmap = 0;
    gna_reset();
    if (ckpt_restore_mem_dir(pd, &m) != 0) _exit(70);

    ckpt_reinstall_sigacts(&m); // restore guest signal dispositions (AFTER the fork hooks reset host state)

    if (ckpt_restore_fds_dir(pd) != 0 || ckpt_restore_signal_state(pd) != 0) _exit(70);
    ckpt_restore_pgrp(gpid, m.pgid_gpid, m.sid_gpid);
    if (g_ckpt_fg_gpid == gpid) ckpt_claim_tty_fg(); // this process led the tty's foreground job -> reclaim it

    static char exe[512];
    snprintf(exe, sizeof exe, "%s", m.exe_path);
    if (exe[0]) g_exe_path = exe;
    char *pubargv[2] = {(char *)(exe[0] ? exe : "guest"), NULL};
    proc_reg_publish(g_exe_path, 1, pubargv);

    ckpt_fork_children(gpid); // re-fork our own children before we resume (so a wait finds them)
    if (thread_restore_group(images, (int)m.n_threads, &c) != 0) _exit(70);
    free(images);
    ckpt_restore_backings_close();
    ckpt_restore_pipe_seeds_close();
    ckpt_restore_eventfd_seeds_close();
    ckpt_restore_signalfd_seeds_close(); /* was omitted here (it is closed in
                                          * ckpt_restore_tree): every re-forked
                                          * process leaked its signalfd seed
                                          * reader+writer pair for its lifetime */
    ckpt_restore_socket_seeds_close();
    run_guest(&c);
    _exit(c.exit_code);
}

// Full restore driver: rebuild the whole tree from the store and resume it. The INIT (gpid 1) restores its
// RAM FIRST (before engine init, so MAP_FIXED lands on free VAs), then re-forks the tree.
static int ckpt_restore_tree(const char *rootfs) {
    struct ckpt_manifest man;
    ckpt_restore_hold_tty_signals();
    // Bind the image source before anything is read; every re-forked child inherits the binding.
    if (ckpt_source_bind() == NULL) {
        fprintf(stderr, "[restore] restore requested without a broker descriptor\n");
        return 2;
    }
    if (ckpt_read_manifest(&man) != 0) return 2;
    if (ckpt_scan_procs() != 0) {
        fprintf(stderr, "[restore] the store holds no process images\n");
        return 2;
    }
    if (ckpt_validate_proc_tree(&man) != 0) {
        fprintf(stderr, "[restore] process tree does not match manifest\n");
        return 2;
    }
    int recovery_policy = ckpt_recovery_policy();
    if (ckpt_restore_preflight(recovery_policy) != 0) return 2;
    if (ckpt_prepare_restore_pipes() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint pipe objects\n");
        return 2;
    }
    if (ckpt_prepare_restore_eventfds() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint eventfd objects\n");
        return 2;
    }
    if (ckpt_prepare_restore_timerfds() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint timerfd objects\n");
        return 2;
    }
    if (ckpt_prepare_restore_signalfds() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint signalfd objects\n");
        return 2;
    }
    if (ckpt_prepare_restore_sockets() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint socketpair objects\n");
        return 2;
    }

    const char *ipd = "proc.1";
    struct ckpt_meta im;
    if (ckpt_read_meta_dir(ipd, &im) != 0) return 2;
    if (ckpt_restore_mem_dir(ipd, &im) != 0) {
        fprintf(stderr, "[restore] init memory restore failed\n");
        return 2;
    } // init RAM before any engine allocation

    container_init(rootfs); // sets g_init_hostpid = getpid() -> this process becomes guest pid 1
    int irc = engine_global_init();
    if (irc) return irc;
    if (ckpt_prepare_restore_socket_states() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint standalone sockets\n");
        return 2;
    }
    if (ckpt_restore_eventfds_initialize() != 0) {
        fprintf(stderr, "[restore] init eventfd state initialization failed\n");
        return 70;
    }

    static char exe[512];
    snprintf(exe, sizeof exe, "%s", im.exe_path);
    if (exe[0]) g_exe_path = exe;
    if (ckpt_restore_fds_dir(ipd) != 0) {
        fprintf(stderr, "[restore] init descriptor restore failed\n");
        return 70;
    }
    struct cpu c, *images = NULL;
    if (ckpt_restore_cpu_dir(ipd, &im, &images) != 0 || ckpt_restore_leader(images, im.n_threads, &c) != 0) {
        fprintf(stderr, "[restore] init CPU restore failed\n");
        return 70;
    }
    ckpt_reinstall_sigacts(&im); // restore the init's guest signal dispositions (so ^C reaches bash's handler)
    if (ckpt_restore_signal_state(ipd) != 0) {
        fprintf(stderr, "[restore] init signal-state restore failed\n");
        return 70;
    }
    char *pubargv[2] = {(char *)(exe[0] ? exe : "guest"), NULL};
    proc_reg_publish(g_exe_path, 1, pubargv);

    // Re-create the init's OWN process group. Restore re-forks the init under the launcher, so it starts in
    // the launcher's group, while at capture it led its own (an interactive shell's job-control setup does
    // setpgid(0,1), which it never repeats after resume). Uncorrected, the guest pgid 1 -> g_init_hostpid
    // translation names a group the shell is not in: its tcsetpgrp handoff after a foreground job puts the
    // terminal's foreground on that empty group, its next read on the ctty is a background read, and with
    // SIGTTIN blocked that read fails EIO -- readline reports EOF and the shell logs out after one line.
    // Only the GROUP is re-created: the session belongs to the launcher, which owns the pty.
    if (im.pgid_gpid == 1) (void)setpgid(0, 0);
    ckpt_restore_tty_mode(&man);
    // Publish which guest group owned the tty foreground, so whichever re-forked process is that group's leader
    // claims the controlling terminal AFTER it re-creates its group (see ckpt_claim_tty_fg). Set before the fork
    // so every child inherits it. Without this the resumed tree's fg group defaults to the init's, and a tty
    // SIGINT hits the init instead of the foreground job -> the whole tree dies on ^C.
    g_ckpt_fg_gpid = man.fg_pgid_gpid;
    ckpt_fork_children(1); // rebuild the tree BEFORE init runs (empty block map -> no stale translation)
    if (thread_restore_group(images, (int)im.n_threads, &c) != 0) {
        fprintf(stderr, "[restore] init thread-group restore failed\n");
        return 70;
    }
    free(images);
    ckpt_restore_backings_close();
    ckpt_restore_pipe_seeds_close();
    ckpt_restore_eventfd_seeds_close();
    ckpt_restore_signalfd_seeds_close();
    ckpt_restore_socket_seeds_close();
    if (g_ckpt_fg_gpid == 1) ckpt_claim_tty_fg(); // the init itself was foreground (idle prompt)

    run_guest(&c);
    return c.exit_code;
}
