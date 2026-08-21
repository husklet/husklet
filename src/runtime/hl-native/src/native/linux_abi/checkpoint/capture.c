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
//     reaped : struct ckpt_reaped_header + n_reaped * struct ckpt_reaped_child -- the exit statuses of this
//              process's children that the FREEZE consumed, so restore can hand them back (see below)
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
#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_vm.h>
#endif

#define CKPT_MAGIC UINT64_C(0x373054504b434c48)          // "HLCKPT07" (LE) -- per-process meta
#define CKPT_MANIFEST_MAGIC UINT64_C(0x3730304e414d4c48) // "HLMAN007" (LE) -- workspace manifest
#define CKPT_VERSION 8                                   // v8 carries the child exit statuses the freeze reaped
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
/* A CKF_TTY record that duplicates one of the launch-time standard descriptors rather than the
   controlling terminal. Bits 2..3 name which standard descriptor (0, 1 or 2) it duplicates. Restore
   rebuilds it from the fresh stdio bridge the restore fork already holds instead of from the ctty. */
#define CKFA_STDIO_ALIAS UINT64_C(2)
#define CKFA_STDIO_ALIAS_SHIFT 2
#define CKFA_STDIO_ALIAS_MASK UINT64_C(3)

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

/* A CHILD EXIT STATUS THE CAPTURE ITSELF DESTROYED.
 *
 * The rendezvous reaps with waitpid(-1, WNOHANG) from inside the container init -- which IS a guest process
 * -- and a guest process's kernel zombie IS the pending-status state: there is no engine-side table behind
 * it. So every status that reap collects for a child the guest had not yet waited for is state the capture
 * deleted. Measured: a `/bin/sh` loop running `sleep .05` spends most of its life blocked in wait4 for a
 * transient child, the freeze lands while that child is a zombie, the coordinator reaps it, the child never
 * registers and is exempted, and the image is published with `checkpoint OK` one process short. The restored
 * shell resumes straight back into wait4 for a pid that no longer exists -- 725 s of `State: S`,
 * `wchan=do_wait`, zero CPU, and nothing logged anywhere. A hang, out of a capture that reported success.
 *
 * The status therefore travels in the image and the restore re-synthesizes a real corpse carrying it, under
 * the same guest pid, as a child of the same parent -- so the parent's wait4 completes exactly as it would
 * have. `status` is the RAW HOST status word, because syscall/process/wait.c applies its host->Linux
 * translation to whatever the host hands back and the synthesized corpse is produced on the same host: the
 * round trip is closed at the same place the live one is. */
struct ckpt_reaped_child {
    int32_t gpid;   // the child's guest pid, which is what the parent's wait4 must report
    int32_t status; // raw host wait status, as waitpid wrote it
};

#define CKPT_REAPED_MAGIC UINT64_C(0x484c524541503031) // "HLREAP01"
// A guest that had this many unreaped children at the freeze is not a shape this mechanism was built for,
// and an unbounded count is an unbounded allocation driven by image bytes.
#define CKPT_REAPED_MAX 4096

struct ckpt_reaped_header {
    uint64_t magic;
    uint64_t count;
};

struct ckpt_meta {
    uint64_t magic, version, arch;
    hl_identity_digest engine_identity;
    uint64_t cpu_sz, pagesz;
    uint64_t n_regions, n_threads, n_fds;
    // Child exit statuses this process's capture consumed, carried in the group's "reaped" object. The count
    // lives in the meta rather than only in that object so a restore KNOWS to expect it: an absent or short
    // object is then an image error, not a silently empty set.
    uint64_t n_reaped;
    uint64_t brk_lo, brk_cur, brk_hi;
    uint64_t nonpie_lo, nonpie_hi, nonpie_bias;
    uint64_t stack_lo, stack_hi;
    int32_t self_gpid, ppid_gpid; // guest identity: this process's pid + its parent's (0 for init's parent)
    int32_t pgid_gpid, sid_gpid;  // guest process group + session (1 == the container init's group/session)
    // The container process domain this member belongs to when it has NO parent inside the container's pid
    // namespace, named by that domain's init gpid (1); 0 for a member that has an ordinary guest parent.
    // A container exec session is forked by the hl-container daemon, not by the container init, so it is
    // parentless in guest terms exactly as Docker reports it -- measured on Docker 29.1.3, an exec top reads
    // PPID 0 in the container's pid namespace, and hl's own getppid (syscall/process/identity.c:345) answers
    // 0 for it too. Recording a fabricated ppid of 1 instead would enrol it in the container init's child
    // set and change what it observes; this field carries the membership WITHOUT claiming a parent edge.
    int32_t domain_root_gpid;
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
    /* SOCK_SHUTDOWN_READ|SOCK_SHUTDOWN_WRITE for THIS endpoint, taken from the shared socket-state arena.
     * Linux does not expose half-close through getsockopt, so the arena is the only source, and recv()==0
     * cannot supply it: measured on this host, an AF_UNIX STREAM survivor reads 0 identically for a peer
     * that closed and for a peer that merely shutdown(SHUT_WR) and is still open, and reads 0 for its OWN
     * SHUT_RD as well.  Each endpoint therefore records the direction IT closed and restore replays it. */
    uint8_t shutdown_mask;
    uint8_t reserved_socket_state[1];
    uint32_t tcp_local_address;
    uint8_t tcp_local_address_v6[16];
    int32_t tcp_option_value[TCP_SHADOW_N];
    uint8_t tcp_option_set[TCP_SHADOW_N];
    int32_t ip_option_value[IPOPT_SHADOW_N];
    uint8_t ip_option_set[IPOPT_SHADOW_N];
    struct linger linger;
    struct sockaddr_storage local;
};

/* The recorded mask -> the single shutdown(2) direction that reproduces it, or -1 for an endpoint that
 * closed neither direction.  SHUT_RDWR is one call rather than two so a restored endpoint never sits
 * momentarily half-closed while the other half is still being applied. */
static int ckpt_socket_shutdown_direction(uint8_t mask) {
    unsigned read_closed = (mask & SOCK_SHUTDOWN_READ) != 0, write_closed = (mask & SOCK_SHUTDOWN_WRITE) != 0;
    if (read_closed && write_closed) return SHUT_RDWR;
    if (read_closed) return SHUT_RD;
    if (write_closed) return SHUT_WR;
    return -1;
}

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

// The disposition the coordinator released this process with (HL_CKPT_RELEASE_*), or RESUME when it never
// parked. Written by ckpt_dump_self at the end of its park; read by ckpt_poll to decide exit-versus-resume.
static uint64_t g_ckpt_release_state = HL_CKPT_RELEASE_RESUME;

// Set the moment this process wins an image-wide claim on a SHARED kernel object (a pipe, a socket queue),
// because winning the claim is the point at which it becomes the one that DRAINS that object. The drain is
// destructive -- recvmsg(MSG_DONTWAIT) consumes, and MSG_PEEK is not an alternative: it installs a fresh
// descriptor in this process for every in-flight SCM_RIGHTS, so peeking a queue leaks a descriptor per peek.
//
// ABORT CONTRACT, stated rather than retrofitted:
//   * an abort BEFORE this flag is set leaves the container running unharmed -- nothing was consumed, and
//     every member simply resumes out of its park;
//   * an abort AFTER it is set is TERMINAL for this member. Its queues have been consumed and there is no
//     roll-back phase yet, so resuming would hand the guest a silently emptied pipe or socket. It exits
//     instead of pretending the container is intact.
// The non-destructive route is closed, not unexplored (MSG_PEEK, above); the route that reopens "unharmed"
// after a drain is roll-back UNDER the freeze -- re-injecting each drained message by writing the PEER's
// end, performed by the member holding that end, which is still parked and still cooperative, and arbitrated
// by the same claim protocol in reverse. It is orderable only because of the park: with every member frozen,
// nothing can read or write the object between the drain and the write-back. Two known bounds when it is
// built: the refill must be done by the far-end owner, and SCM_CREDENTIALS on re-injected messages would be
// recomputed to the re-injector's identity (SCM_RIGHTS is safe -- those descriptors are in flight, not yet
// guest-visible). Until then this flag is what keeps a post-drain abort honest.
static int g_ckpt_capture_destructive;

// ADMISSION BEFORE CONSUMPTION.
//
// ckpt_scan_fds walks the descriptor table in ascending guest-fd order, and every capture arm is both an
// admission decision and, for a shared kernel object, the consumer of that object. Interleaving the two
// meant a pipe at fd 3 was DRAINED -- setting g_ckpt_capture_destructive -- before a guaranteed refusal at
// fd 10 was ever evaluated, so a purely non-destructive policy refusal became terminal for every member of
// the tree ("cannot resume: its capture was destructive and was not published"). The product property the
// freeze exists to provide -- an abort before the destructive flag leaves the container running unharmed --
// was unreachable, because the flag was always set first.
//
// So the scan runs twice. This flag is set for the first pass, in which every arm evaluates every gate that
// can refuse and then stops short of the two things it must not do yet:
//   - CONSUME: no pipe, signalfd or socket receive queue is read;
//   - CLAIM or PUBLISH: no image object is claimed or written.
// The claim election deliberately stays in pass 2. A claim is a first-writer election on an image-wide name,
// and a claim won in pass 1 by a member that pass 1 then refuses would strand the name for every other
// holder -- the loser arms return 0 on the assumption that the winner drains, so the object would be
// published empty or not at all. Election must therefore happen no earlier than the point the capture is
// committed to running, which is pass 2.
//
// Cross-member ordering: pass 1 needs no new barrier. ckpt_dump_self_locked runs entirely inside this
// member's freeze, and a member whose pass 1 refuses returns -1 from ckpt_dump_self_locked and then PARKS
// (image.c) exactly as a succeeding member does, holding its freeze until the coordinator's decision. The
// coordinator's group accounting therefore observes the refusal before it publishes a manifest, and a
// sibling that has already reached pass 2 is draining under the same freeze. What the split removes is the
// case the barrier could never have fixed: a member destroying its OWN state for a capture its OWN later
// gate was always going to refuse.
static int g_ckpt_admission_only;

// Run a descriptor walk twice: once to prove it admissible with no consumption, and -- only if every gate
// passed -- once for real. A refusal in the first pass returns before anything has been consumed, claimed
// or published, which is what makes an abort at that point leave the container unharmed.
static int ckpt_admit_then_consume(int (*walk)(void *), void *context) {
    g_ckpt_admission_only = 1;
    int admitted = walk(context);
    g_ckpt_admission_only = 0;
    if (admitted != 0) return -1; // the refusing arm has already reported its own cause
    return walk(context);
}

static int ckpt_dump_self(struct cpu *c, const char *group, int park);
static void ckpt_coordinate_and_exit(struct cpu *c);

#include "reopen_path.c"

#include "trigger.c"

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

// A pipe end is drainable only if it can be read. The bytes buffered in a pipe are reachable through a read
// end and nowhere else, so a write-only holder must never take the image-wide claim: it would win the
// election it cannot satisfy, publish an empty object, and leave the buffered bytes in the kernel to be
// lost at restore. The reverted fail-closed change (935dae440) dropped this test at both call sites, which
// was safe only because the function behind it refused every pipe unconditionally; with a real drain behind
// it the test is load-bearing, so it is deliberately NOT re-landed.
//
// The test is "not write-only" rather than "read-only". A pipe end opened O_RDWR -- a FIFO opened
// read-write, or a descriptor the guest reopened through /proc/self/fd -- is readable and must drain. The
// two capture paths disagreed on exactly this: image.c tested `!= O_WRONLY` and the queued-rights path in
// ckpt_capture_right_resource tested `== O_RDONLY`, so an O_RDWR end reached through SCM_RIGHTS published
// no object at all and its pipe came back EMPTY, silently, on a checkpoint that reported success.
#include "descriptor_capture.c"

static int ckpt_process_coordinates(void) {
    return container_pid() == 1 && hl_option_get("HL_CHECKPOINT_COORDINATOR") != NULL;
}

// The group a non-coordinating member commits, named with the SAME function the coordinator names it with
// (ckpt_peer_gpid over this process's host pid), so the rendezvous cannot disagree by construction.
// container_pid() is wrong here for the same reason it is wrong for election: an exec session's top process
// would commit proc.1 and collide with the coordinator's own group while the coordinator waited forever for
// proc.<its host pid>.
static int ckpt_self_gpid(void) {
    int gpid = ckpt_peer_gpid(getpid());
    return gpid > 0 ? gpid : container_pid();
}

static void ckpt_self_group(char *out, size_t size) {
    snprintf(out, size, "proc.%d", ckpt_self_gpid());
}

// The guest pid THIS launch's own image group is filed under, decided the same way the capture decides
// it and available before the guest starts.
//
// Two different answers, because two different namers: a non-coordinating member commits
// ckpt_self_group, so its identity is ckpt_self_gpid (its host pid on a fresh launch, its mapped guest
// pid on a restored one); the container init's group is named "proc.1" by the COORDINATOR instead, and
// container_pid() would report 1 for both -- which is precisely the fold that once filed three exec
// sessions as proc.1.
//
// The host reads this to learn which member a sealed exec record names, so it has to be the image's
// answer and not the launch's opinion of itself.
static int ckpt_image_self_gpid(void) {
    return ckpt_process_coordinates() ? 1 : ckpt_self_gpid();
}

// The guest pid a member's image is filed under, read back from the group name the coordinator and the
// member agreed on. THE GROUP NAME IS THE AUTHORITY: it is the only identity both sides of the rendezvous
// have already committed to, so deriving the meta's self_gpid from anything else -- container_pid(),
// a second call to ckpt_peer_gpid -- reintroduces the disagreement that named three exec sessions proc.1.
// The container init is the one member whose group the COORDINATOR names ("proc.1") rather than
// ckpt_self_group, and parsing covers it for free.
static int ckpt_group_gpid(const char *group) {
    return strncmp(group, "proc.", 5) == 0 ? atoi(group + 5) : -1;
}

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
    if (ckpt_process_coordinates()) {
        ckpt_coordinate_and_exit(c); // never returns (dumps the tree + _exit)
    }
    char pd[64];
    ckpt_self_group(pd, sizeof pd);
    // PARK, do not exit. ckpt_dump_self holds this process's whole freeze across the coordinator's decision,
    // so every member is simultaneously stopped AND ALIVE for the entire capture -- which is the only state
    // in which "both owners of this shared object were frozen when it was captured" is a provable fact
    // rather than an inference from group membership. Only the coordinator's release ends the freeze.
    // Test-only, and the ONLY deterministic way to produce the shape that made closing a healthy
    // workspace fail intermittently: a peer the coordinator has already enumerated and interrupted, which
    // then exits on its own before it can join the capture. A real transient fork child (`sleep .05` in a
    // shell, a short `make` job) does exactly this, but only when it happens to lose the race, which is
    // why the original evidence was a 42%-under-load rig rather than a test.
    // Both peer-exit fixtures below have to be ENUMERATED before they die, or they exercise nothing: a
    // peer the coordinator never saw is never waited for. The coordinator kicks every participant it
    // enumerated, so waiting here for that kick is the edge that puts this process in the peer set. The
    // sleep is a ceiling rather than the mechanism -- the kick interrupts it -- and it is far inside the
    // whole-tree rendezvous budget.
    int exit_before_join = hl_option_get("HL_CKPT_TEST_PEER_EXIT_BEFORE_JOIN") != NULL;
    if (exit_before_join || hl_option_get("HL_CKPT_TEST_PEER_EXIT_AFTER_JOIN") != NULL) {
        struct timespec ceiling = {1, 0};
        (void)nanosleep(&ceiling, NULL);
        // Exiting HERE is a peer that never sent REGISTER_READY and therefore published nothing. The
        // other option exits inside ckpt_dump_self, after the registration round trip: same corpse, and
        // the capture must refuse for it.
        if (exit_before_join) _exit(0);
    }
    /* Test-only, and the two shapes the rendezvous has to tell apart. Both are what the coordinator sees
     * from the outside; neither can be approximated by anything the coordinator itself does.
     *
     *   SLOW: a member that takes far longer than the old fixed ~5 s tree budget to reach the point where
     *   it commits, while genuinely working the whole time. That is what a starved member looks like on a
     *   loaded box, and burning real CPU here is not incidental -- consumed CPU time is precisely the
     *   signal the rendezvous now waits on, so this fixture progresses in exactly the way a descheduled
     *   member does, only compressed into one process.
     *
     *   STALLED: a member that reaches this dispatcher, never registers, never commits, and consumes no
     *   CPU at all from here on. Nothing will ever make it finish, and the capture must still refuse,
     *   bounded, rather than wait for a member that is not coming. */
    if (hl_option_get("HL_CKPT_TEST_PEER_SLOW_SAFEPOINT") != NULL) {
        struct timespec started, now;
        volatile unsigned long long spin = 0;
        long long elapsed_ns;
        (void)clock_gettime(CLOCK_MONOTONIC, &started);
        do {
            for (int i = 0; i < 100000; i++) spin += (unsigned long long)i;
            (void)clock_gettime(CLOCK_MONOTONIC, &now);
            elapsed_ns = (long long)(now.tv_sec - started.tv_sec) * 1000000000LL + (now.tv_nsec - started.tv_nsec);
        } while (elapsed_ns < 8000000000LL);
        fprintf(stderr, "[ckpt] %s reached its safepoint slowly (test hook)\n", pd);
    }
    if (hl_option_get("HL_CKPT_TEST_PEER_STALLS_AT_SAFEPOINT") != NULL) {
        fprintf(stderr, "[ckpt] %s will never commit and will consume no CPU (test hook)\n", pd);
        { /* no later kick can move it either */
            sigset_t deaf;
            sigemptyset(&deaf);
            sigaddset(&deaf, THREAD_INT_SIG);
            pthread_sigmask(SIG_BLOCK, &deaf, NULL);
        }
        for (;;) {
            struct timespec forever = {3600, 0};
            (void)nanosleep(&forever, NULL);
        }
    }
    int rc = ckpt_dump_self(c, pd, 1);
    uint64_t released = g_ckpt_release_state;
    // Report the GROUP, not container_pid(): an exec session's top process is guest pid 1 by g_init_hostpid
    // and three of them printing "proc 1" reads as three coordinators when it is one group name each.
    fprintf(stderr, "[ckpt] %s %s (%s)\n", pd, rc == 0 ? "OK" : "FAILED",
            released == HL_CKPT_RELEASE_EXIT ? "released: image published" : "released: capture abandoned");
    if (rc == 0 && released == HL_CKPT_RELEASE_EXIT) _exit(0);
    if (released == HL_CKPT_RELEASE_EXIT || g_ckpt_capture_destructive) {
        // Either the image owns this process and its own dump failed, or the capture was abandoned after
        // this process had already consumed a shared pipe or socket queue. Neither leaves a guest that can
        // be resumed honestly; see the abort contract on g_ckpt_capture_destructive.
        fprintf(stderr, "[ckpt] %s cannot resume: its capture was destructive and was not published\n", pd);
        _exit(70);
    }
    // Nothing was consumed and nothing was published: the container is unharmed and the guest runs on.
    // g_ckpt_seen_gen already names this generation, so returning to the dispatcher does not re-enter here.
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

// IDENTITY FOR AN ANONYMOUS MAP_SHARED REGION.
//
// mmap(MAP_SHARED|MAP_ANONYMOUS) is the one shared object with no descriptor anywhere: map.c
// registers a mapping in g_filemap only when it is NOT anonymous, and backing_object is only ever
// set from g_filemap, so such a region reached the image with backing_object == 0 and
// memory_restore.c mapped it MAP_ANON|MAP_PRIVATE -- a PER-PROCESS PRIVATE COPY of memory the
// guest believes is shared. PostgreSQL 16 with shared_memory_type=mmap puts its whole shared
// buffer pool, ProcArray, lock tables and PMChildFlags there; a live cluster dumped nine members
// each carrying its own 256 MiB copy of the same VA.
//
// WHAT NAMES THE OBJECT. Linux backs a shared anonymous mapping with an unnamed shmem inode, and
// reports its (dev, ino) in /proc/self/maps for every process that maps it -- measured identical
// in parent and child across fork, and distinct between two mappings of the same size made back to
// back in one process. That inode IS the object: it is minted by the kernel, unique for the
// object's whole lifetime, agreed on by every sharer without any engine-side coordination, and it
// survives mremap. It feeds the SAME ckpt_backing_id hash the file-backed path uses, so one id
// space covers both.
//
// WHY A VA JOIN WOULD NOT DO. A shared anonymous object can only be shared by fork inheritance, so
// its sharers do tend to hold it at a common VA -- but the converse fails: two members that are
// not fork-related (a container exec session, or a child that unmapped the inherited region and
// mmap'd a fresh shared object) can hold DIFFERENT objects at the SAME VA, and joining on the VA
// would fuse them into one. The VA is kept as a restore-side fidelity check (the region is
// re-mapped at exactly its captured address or the restore refuses), never as the identity.
//
// PRIVATE anonymous regions are untouched: /proc/self/maps has no entry for them at all -- a
// private mapping is not an object, it is this process's pages -- so the lookup below misses and
// they keep the existing MAP_ANON|MAP_PRIVATE per-process restore, fork-inherited ones included.
#define CKPT_ANON_SHARED_MAX 256
struct ckpt_anon_shared_row {
    uint64_t lo, hi, offset, object_id;
};
static struct ckpt_anon_shared_row g_anon_shared[CKPT_ANON_SHARED_MAX];
static int g_nanon_shared;
static int g_anon_shared_truncated;

// Read the host mapping table ONCE per dump. Doing it per region would be O(regions x mappings)
// inside the stop-the-world freeze, which the ~5s whole-tree budget cannot absorb on a guest with
// thousands of mappings. Shared anonymous mappings are a handful even in a large cluster.
//
// THE MAPPING TABLE IS PER HOST OS, AND macOS HAS NO /proc. Reading /proc/self/maps unconditionally
// made this scan report "unenumerable" on every Darwin host, so every macOS checkpoint refused --
// which is a whole-platform outage, not a conservative refusal. Darwin's mapping table is the Mach
// VM map, walked with mach_vm_region_recurse, and it carries both halves of the same answer: the
// entry's VM_INHERIT_SHARE marker is the kernel's own record of MAP_SHARED, and `object_id` names
// the vm_object exactly as the shmem inode names the object on Linux. Both hosts therefore read the
// kernel's own record, and neither guesses "private".
#if defined(__APPLE__)
// MEASURED ON macOS 26.3.1 (arm64). Every field this reads was chosen against a reading, and the
// two obvious candidates were both rejected by one:
//
//  - THE DISCRIMINATOR IS `inheritance`, not `share_mode`. XNU records MAP_SHARED as
//    VM_INHERIT_SHARE on the map entry and MAP_PRIVATE as VM_INHERIT_COPY, so it is the exact
//    counterpart of Linux's 's' permission character and is true of the mapping the kernel actually
//    made. `share_mode` describes how the object is shared AT THIS INSTANT: the same MAP_SHARED
//    region read SM_PRIVATE(2) before a fork and SM_SHARED(4) after it, so keying on it would have
//    silently restored a not-yet-shared MAP_SHARED region as a private copy -- the very defect this
//    table exists to stop. Measured: MAP_SHARED|MAP_ANON read inh=0 (VM_INHERIT_SHARE) both before
//    and after the fork; MAP_PRIVATE|MAP_ANON read inh=1 (VM_INHERIT_COPY) both times.
//  - THE IDENTITY IS `object_id`, the Darwin counterpart of Linux's shmem inode. Measured identical
//    in parent and child across fork (3705039015 both sides) and distinct between two shared
//    mappings made back to back (3705039015 against 3976147844).
//  - `proc_regionfilename` is NOT usable as the file/anonymous discriminator: it answered
//    "/usr/lib/dyld" for a freshly mmap'd anonymous shared region. `external_pager` is the honest
//    vnode-backed marker, and named file mappings are carried by g_filemap regardless.
//  - PROT_NONE rows are malloc's guard regions (user_tag 1). They are not objects any guest reads,
//    and excluding them keeps this table inside its bound.
//
// A stray host row that survives every filter is inert: the table is only ever consulted with a
// guest mapping's address and requires full containment, exactly as the Linux table is.
static void ckpt_anon_shared_scan(void) {
    g_nanon_shared = 0;
    g_anon_shared_truncated = 0;
    mach_vm_address_t address = 0;
    for (;;) {
        mach_vm_size_t size = 0;
        vm_region_submap_info_data_64_t info;
        mach_msg_type_number_t count = VM_REGION_SUBMAP_INFO_COUNT_64;
        // The depth is an IN/OUT parameter and must be re-armed for every query. Carrying it across
        // iterations -- or advancing it on is_submap without advancing the address -- walks the map
        // forever, which inside the stop-the-world freeze is a hang rather than a refusal. A fixed
        // depth makes the call resolve submaps itself and never return one.
        natural_t depth = 1;
        memset(&info, 0, sizeof info);
        kern_return_t status = mach_vm_region_recurse(mach_task_self(), &address, &size, &depth,
                                                     (vm_region_recurse_info_t)&info, &count);
        // KERN_INVALID_ADDRESS terminates the walk: no region at or above this address. Any other
        // failure means the table was not fully read, which is the truncation this refuses on.
        if (status == KERN_INVALID_ADDRESS) break;
        if (status != KERN_SUCCESS) {
            g_anon_shared_truncated = 1;
            return;
        }
        if (size == 0) break;
        if (info.inheritance == VM_INHERIT_SHARE && info.external_pager == 0 && info.protection != 0 &&
            info.object_id != 0) {
            if (g_nanon_shared >= CKPT_ANON_SHARED_MAX) {
                g_anon_shared_truncated = 1;
                return;
            }
            // Darwin has no device number for a vm_object. A synthetic one keeps the Mach object id
            // space disjoint from the (st_dev, st_ino) space the file-backed path hashes, so the two
            // families can never collide inside one id.
            g_anon_shared[g_nanon_shared++] = (struct ckpt_anon_shared_row){
                (uint64_t)address, (uint64_t)address + (uint64_t)size, (uint64_t)info.offset,
                ckpt_backing_values(UINT64_C(0xffffffffffffffff), (uint64_t)info.object_id)};
        }
        if ((uint64_t)address > UINT64_MAX - (uint64_t)size) break;
        address += size;
    }
}
#else
static void ckpt_anon_shared_scan(void) {
    g_nanon_shared = 0;
    g_anon_shared_truncated = 0;
    FILE *maps = fopen("/proc/self/maps", "r");
    if (maps == NULL) {
        // No mapping table means no way to tell a shared anonymous region from a private one, and
        // the silent answer is the defect. Mark the scan truncated so capture refuses.
        g_anon_shared_truncated = 1;
        return;
    }
    char line[512];
    while (fgets(line, sizeof line, maps) != NULL) {
        unsigned long long lo = 0, hi = 0, file_offset = 0, inode = 0;
        unsigned major = 0, minor = 0;
        char permissions[8];
        int consumed = 0;
        if (sscanf(line, "%llx-%llx %7s %llx %x:%x %llu %n", &lo, &hi, permissions, &file_offset, &major, &minor,
                   &inode, &consumed) != 7)
            continue;
        // 's' is the kernel's own MAP_SHARED marker; 'p' is private. This is the discriminator, and
        // it is read from the mapping the kernel actually made, not from remembered mmap flags.
        if (permissions[3] != 's') continue;
        // A shared anonymous mapping is the shmem-backed row with no real pathname (the kernel
        // labels it "/dev/zero (deleted)"). A named file mapping is already carried by g_filemap
        // and must not be re-identified here.
        const char *path = line + consumed;
        while (*path == ' ') path++;
        if (*path != '\0' && *path != '\n' && strncmp(path, "/dev/zero", 9) != 0) continue;
        if (inode == 0 || hi <= lo) continue;
        if (g_nanon_shared >= CKPT_ANON_SHARED_MAX) {
            g_anon_shared_truncated = 1;
            break;
        }
        g_anon_shared[g_nanon_shared++] = (struct ckpt_anon_shared_row){
            (uint64_t)lo, (uint64_t)hi, (uint64_t)file_offset,
            ckpt_backing_values(((uint64_t)major << 8) | minor, (uint64_t)inode)};
    }
    fclose(maps);
}
#endif

static int ckpt_anon_shared_object(uint64_t address, uint64_t length, uint64_t *object_id, uint64_t *offset) {
    *object_id = 0;
    *offset = 0;
    if (length == 0 || address > UINT64_MAX - length) return 0;
    for (int index = 0; index < g_nanon_shared; index++) {
        const struct ckpt_anon_shared_row *row = &g_anon_shared[index];
        if (address < row->lo || address + length > row->hi) continue;
        *object_id = row->object_id;
        *offset = row->offset + (address - row->lo);
        return 1;
    }
    return 0;
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
        if (ckpt_pipe_end_drains(flags) && ckpt_capture_pipe(fd, g_pipe_identity[fd]) != 0) return -1;
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

// Is this live engine process a member of the capture the coordinator at `root` is taking?
//
// MEMBERSHIP IS THE CONTAINER, NOT THE PROCESS TREE. `ckpt_is_descendant` is immune to a guest
// re-parenting or re-sessioning itself, which is what made it the right answer to the session defect --
// but it is strictly too tight for a container. hl-container forks an `exec` session out of its own
// daemon, so an exec session is a SIBLING of guest pid 1 and no descendancy walk can reach it. Measured
// on a live PostgreSQL cluster: three of eleven engine processes -- every `psql` client -- reported
// `descendant=0` while holding the far end of a connected socket owned by a process that WAS captured.
// They were alive, frozen by nothing, and outside the freeze, so the capture could only publish an image
// whose socket topology named an endpoint nobody stopped.
//
// The container's process domain answers it authoritatively: it is assigned from OUTSIDE the container
// (hl-container gives the container's own HL_PROCESS_DOMAIN to each of its exec sessions), so unlike a
// session it is not guest-mutable, and unlike "runs our executable" it names ONE container rather than
// every engine process on the host.
//
// The two are a UNION, not a switch. Descendancy of the container init remains a sufficient condition on
// its own: a host descendant of guest pid 1 IS a guest process of this container, and it is the only rule
// a bare engine launch -- which has no container domain to belong to -- can use at all. Deciding solely on
// the domain would drop a genuine descendant in the window before it publishes its birth record, and a
// member dropped from the set is never interrupted, never commits, and is therefore missing from the very
// freeze whose exactness the manifest asserts.
static int ckpt_capture_member(int64_t candidate, int64_t root) {
    if (candidate <= 0 || candidate > INT32_MAX) return 0;
    return hl_linux_container_process_domain_member((int32_t)candidate) || ckpt_is_descendant(candidate, root);
}

#if defined(HL_NATIVE_TEST_HOOKS)
// ------------------------------------------------------- capture membership: behavioral fixture
//
// Drives the REAL ckpt_capture_member against a REAL live host process in the exact shape a container
// exec session has: a SIBLING, not a descendant, of the process taking the capture. hl-container forks
// an exec session out of its own daemon, so no in-memory fake and no same-tree fork can express the
// case -- the fixture must produce an orphaned grandchild and then join it to a domain through the same
// production birth-record publisher the engine's own startup uses.
//
// Scenario 0 is the positive half: the sibling IS a member, and the descendancy rule the coordinator
// used to apply says it is not. Scenarios 1 and 2 are the half that stops the fix from being a widening:
// a live process running THIS SAME EXECUTABLE is refused when it belongs to another container's domain,
// and refused again when it has published no membership at all.
static int ckpt_membership_orphan(int *reported) {
    int ready[2];
    if (pipe(ready) != 0) return -1;
    pid_t middle = hl_host_process_clone_current();
    if (middle < 0) {
        (void)close(ready[0]);
        (void)close(ready[1]);
        return -1;
    }
    if (middle == 0) { // async-signal-safe only: forked out of a multi-threaded caller
        pid_t orphan = hl_host_process_clone_current();
        if (orphan == 0) {
            (void)setsid(); // a guest session leader, the shape that once emptied peer enumeration
            struct timespec span = {30, 0};
            (void)nanosleep(&span, NULL);
            _exit(0);
        }
        int value = (int)orphan;
        (void)write(ready[1], &value, sizeof value);
        _exit(0);
    }
    (void)close(ready[1]);
    int orphan = -1;
    ssize_t got = read(ready[0], &orphan, sizeof orphan);
    (void)close(ready[0]);
    int status = 0;
    (void)waitpid(middle, &status, 0); // the middle process is gone: the orphan is now OUR SIBLING
    if (got != (ssize_t)sizeof orphan || orphan <= 0) return -1;
    *reported = orphan;
    return 0;
}

// ------------------------------------------------------- coordinator election: behavioral fixture
//
// Drives the REAL ckpt_process_coordinates and ckpt_self_group in the three shapes one container's
// process domain actually contains once every launch shares a trigger word and a broker. There is one
// broker and one manifest, so exactly one process may coordinate; every other member must commit a group
// of its OWN, under the name the coordinator waits for.
//
// The scenarios differ only in what the production code is allowed to read: g_init_hostpid (which EVERY
// engine launch's top process sets to its own pid, so it does not distinguish a container init from an
// exec session's top) and HL_CHECKPOINT_COORDINATOR (which only the launch the embedder can send
// REQUEST_CHECKPOINT to carries). Scenario 1 is the postgres failure verbatim: an exec session's top
// process, guest pid 1 by g_init_hostpid, elected itself and therefore committed nothing.
HL_API int HL_TARGET_LOCAL(checkpoint_election_test)(uint32_t scenario) {
    if (scenario > 2) return -22;
    int saved_init = g_init_hostpid;
    int saved_cache = g_hostpid_cache;
    int saved_gpid = g_self_gpid;
    const char *saved_option = hl_option_get("HL_CHECKPOINT_COORDINATOR");
    int had_option = saved_option != NULL;
    int verdict = -1;
    char group[64];
    char expected[64];

    g_self_gpid = 0;
    g_hostpid_cache = 0;
    // Scenarios 0 and 1 are both a LAUNCH TOP: guest pid 1 by the only rule the engine has. Scenario 2 is
    // an ordinary guest process of the coordinating launch.
    g_init_hostpid = scenario == 2 ? 0 : getpid();
    if (scenario == 1)
        (void)hl_option_unset("HL_CHECKPOINT_COORDINATOR");
    else
        (void)hl_option_set("HL_CHECKPOINT_COORDINATOR", "1", 1);

    int coordinates = ckpt_process_coordinates();
    if (coordinates != (scenario == 0)) goto done;
    if (scenario != 0) {
        // A member names its group with the coordinator's own naming function applied to its host pid.
        // Committing proc.1 here would collide with the coordinator's group and leave the group the
        // coordinator waits for permanently absent.
        ckpt_self_group(group, sizeof group);
        snprintf(expected, sizeof expected, "proc.%d", ckpt_peer_gpid(getpid()));
        if (strcmp(group, expected) != 0) goto done;
        if (strcmp(group, "proc.1") == 0) goto done;
    }
    verdict = 0;
done:
    if (had_option)
        (void)hl_option_set("HL_CHECKPOINT_COORDINATOR", "1", 1);
    else
        (void)hl_option_unset("HL_CHECKPOINT_COORDINATOR");
    g_init_hostpid = saved_init;
    g_hostpid_cache = saved_cache;
    g_self_gpid = saved_gpid;
    return verdict;
}

HL_API int HL_TARGET_LOCAL(checkpoint_membership_test)(uint32_t scenario) {
    char self_key[33], other_key[33], directory[80];
    int orphan = -1, verdict = -1;
    if (scenario > 2) return -22;
    // The registry publisher writes through the bound host file services, which a bare hook process has
    // not created. Binding them is what an engine launch does at startup, so the fixture publishes through
    // exactly the production path rather than opening the record itself.
    if (g_jit_services.file == NULL && hl_target_services_bind(&g_target_services) != 0) return -1;
    snprintf(self_key, sizeof self_key, "%08x%08x%08x%08x", (unsigned)getpid(), 0x5eec7edu, 0u, 1u);
    snprintf(other_key, sizeof other_key, "%08x%08x%08x%08x", (unsigned)getpid(), 0x5eec7edu, 0u, 2u);
    if (ckpt_membership_orphan(&orphan) != 0) return -1;
    hl_option_set("HL_PROCESS_DOMAIN", self_key, 1);
    if (ckpt_is_descendant(orphan, getpid())) goto done; // the fixture is not the exec-session shape
    if (scenario != 2) { // join the orphan to this container's domain, or to a second container's
        hl_option_set("HL_PROCESS_DOMAIN", scenario == 0 ? self_key : other_key, 1);
        if (!proc_reg_domain_key(directory, sizeof directory)) goto done;
        hl_compat_mkdir(directory, 0777);
        proc_reg_birth_publish(directory, orphan, NULL, 0);
        hl_option_set("HL_PROCESS_DOMAIN", self_key, 1);
    }
    verdict = ckpt_capture_member(orphan, getpid()) == (scenario == 0) ? 0 : -1;
done:
    (void)kill((pid_t)orphan, SIGKILL);
    return verdict;
}
#endif

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

#if defined(HL_NATIVE_TEST_HOOKS)
// ------------------------------------------------------- pipe capture under the freeze: behavioral fixture
//
// Drives the REAL ckpt_capture_pipe_reason against a REAL kernel pipe through a sink that implements only
// the two operations the pipe path uses -- the image-wide claim and a byte stream -- so the election, the
// drain and the abort contract are exercised rather than modelled. The claim table and the drained bytes
// live in one MAP_SHARED page, which is what lets scenario 0 fork and still arbitrate a single election
// across processes that inherited the same pipe end, the postmaster/backend shape.

// The refill half of the round trip is defined later in the unity translation unit (resource_restore.c),
// where restore lives. The fixture drives capture and refill against each other, so it needs both.
static int ckpt_refill_restore_pipe(int writer, uint64_t identity);

#define CKPT_PIPE_TEST_PAYLOAD 24000u

struct ckpt_pipe_test_shared {
    _Atomic int claim;
    _Atomic int winners;
    _Atomic int losers;
    _Atomic unsigned length;
    unsigned char bytes[CKPT_PIPE_TEST_PAYLOAD];
};

static struct ckpt_pipe_test_shared *g_ckpt_pipe_test_shared;

static int ckpt_pipe_test_begin(struct ckpt_sink *sink, const char *group, const char *name, uint32_t flags,
                                struct ckpt_sink_stream **out) {
    (void)group;
    (void)name;
    (void)flags;
    // No allocation: scenario 0 runs this inside a fork of a threaded test process, where malloc's lock
    // may have been held by another thread at the instant of the fork.
    static struct ckpt_sink_stream stream;
    memset(&stream, 0, sizeof stream);
    stream.sink = sink;
    *out = &stream;
    return 0;
}

static int ckpt_pipe_test_write(struct ckpt_sink_stream *stream, const void *data, size_t size) {
    (void)stream;
    struct ckpt_pipe_test_shared *shared = g_ckpt_pipe_test_shared;
    unsigned at = atomic_load(&shared->length);
    if (size > sizeof shared->bytes - at) return -1;
    memcpy(shared->bytes + at, data, size);
    atomic_store(&shared->length, at + (unsigned)size);
    return 0;
}

static int ckpt_pipe_test_finish(struct ckpt_sink_stream *stream) {
    (void)stream;
    atomic_fetch_add(&g_ckpt_pipe_test_shared->winners, 1);
    return 0;
}

static void ckpt_pipe_test_abort(struct ckpt_sink_stream *stream) { (void)stream; }

static int ckpt_pipe_test_claim(struct ckpt_sink *sink, const char *name) {
    (void)sink;
    (void)name;
    int free_slot = 0;
    if (atomic_compare_exchange_strong(&g_ckpt_pipe_test_shared->claim, &free_slot, 1)) return 0;
    atomic_fetch_add(&g_ckpt_pipe_test_shared->losers, 1);
    return 1;
}

static void ckpt_pipe_test_unclaim(struct ckpt_sink *sink, const char *name) {
    (void)sink;
    (void)name;
    atomic_store(&g_ckpt_pipe_test_shared->claim, 0);
}

static const ckpt_sink_vtable g_ckpt_pipe_test_ops = {
    .begin = ckpt_pipe_test_begin,
    .write = ckpt_pipe_test_write,
    .finish = ckpt_pipe_test_finish,
    .abort = ckpt_pipe_test_abort,
    .claim = ckpt_pipe_test_claim,
    .unclaim = ckpt_pipe_test_unclaim,
};

// The same shared page read back as an image source, so the restore refill consumes exactly the object the
// capture drain published rather than a hand-built copy of it.
static int64_t ckpt_pipe_test_source_size(struct ckpt_source *source, const char *name) {
    (void)source;
    (void)name;
    return (int64_t)atomic_load(&g_ckpt_pipe_test_shared->length);
}

static int64_t ckpt_pipe_test_source_read(struct ckpt_source *source, const char *name, uint64_t offset, void *out,
                                          size_t size) {
    (void)source;
    (void)name;
    unsigned length = atomic_load(&g_ckpt_pipe_test_shared->length);
    if (offset > length || size > length - offset) return -1;
    memcpy(out, g_ckpt_pipe_test_shared->bytes + offset, size);
    return (int64_t)size;
}

static const ckpt_source_vtable g_ckpt_pipe_test_source_ops = {
    .size = ckpt_pipe_test_source_size,
    .read = ckpt_pipe_test_source_read,
};

static int ckpt_pipe_test_open_shared(void) {
    void *page = mmap(NULL, sizeof(struct ckpt_pipe_test_shared), PROT_READ | PROT_WRITE,
                      MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) return -1;
    memset(page, 0, sizeof(struct ckpt_pipe_test_shared));
    g_ckpt_pipe_test_shared = page;
    return 0;
}

static void ckpt_pipe_test_close_shared(void) {
    if (g_ckpt_pipe_test_shared) munmap(g_ckpt_pipe_test_shared, sizeof(struct ckpt_pipe_test_shared));
    g_ckpt_pipe_test_shared = NULL;
}

// The bytes must come back in order, so make every byte a function of its position.
static unsigned char ckpt_pipe_test_byte(unsigned index) { return (unsigned char)(index * 7u + 3u); }

static int ckpt_pipe_test_fill(int writer) {
    for (unsigned at = 0; at < CKPT_PIPE_TEST_PAYLOAD;) {
        unsigned char block[4096];
        unsigned size = CKPT_PIPE_TEST_PAYLOAD - at < sizeof block ? CKPT_PIPE_TEST_PAYLOAD - at : sizeof block;
        for (unsigned index = 0; index < size; ++index) block[index] = ckpt_pipe_test_byte(at + index);
        ssize_t written = write(writer, block, size);
        if (written <= 0) return -1;
        at += (unsigned)written;
    }
    return 0;
}

static int ckpt_pipe_test_buffered(int fd) {
    int pending = -1;
    return ioctl(fd, FIONREAD, &pending) == 0 ? pending : -1;
}

// A two-descriptor walk in ascending guest-fd order, driven through the SAME production driver
// ckpt_scan_fds uses: a drainable pipe first, then a descriptor that is refused. The refusal is the real
// one -- ckpt_capture_native_fd rejects a socket outright -- and it consumes nothing.
struct ckpt_pipe_test_scan {
    int pipe_fd;
    uint64_t identity;
    int refused_fd;
};

static int ckpt_pipe_test_scan_pass(void *context) {
    struct ckpt_pipe_test_scan *walk = context;
    if (ckpt_capture_pipe_reason(walk->pipe_fd, walk->identity, NULL, NULL) != 0) return -1;
    hl_host_process_fd detail;
    char path[512];
    size_t path_size = 0;
    if (!hl_host_process_fd_read(getpid(), walk->refused_fd, &detail, path, sizeof(path) - 1, &path_size)) return -1;
    if (detail.kind == HL_HOST_FD_SOCKET) return -1; // the same arm ckpt_capture_native_fd takes
    return 0;
}

static int ckpt_pipe_test_refused_scan(int pipe_fd, uint64_t identity, int refused_fd) {
    struct ckpt_pipe_test_scan walk = {pipe_fd, identity, refused_fd};
    return ckpt_admit_then_consume(ckpt_pipe_test_scan_pass, &walk);
}

HL_API int HL_TARGET_LOCAL(checkpoint_pipe_capture_test)(uint32_t scenario) {
    struct ckpt_sink *saved_sink = ckpt_sink_current();
    int saved_destructive = g_ckpt_capture_destructive;
    int verdict = 99;
    int pair[2] = {-1, -1};

    if (scenario == 0) {
        // An inherited pipe held by three processes: exactly one drains, and the bytes it publishes are the
        // bytes that were written, in order.
        if (ckpt_pipe_test_open_shared() != 0) return 10;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 11;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 12;
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        pid_t children[2] = {-1, -1};
        for (int index = 0; verdict == 0 && index < 2; ++index) {
            children[index] = hl_host_process_clone_current();
            if (children[index] == 0) {
                g_ckpt_capture_destructive = 0;
                int rc = ckpt_capture_pipe_reason(pair[0], 0x11, NULL, NULL);
                _exit(rc == 0 ? (g_ckpt_capture_destructive ? 0 : 1) : 2);
            }
            if (children[index] < 0) verdict = 13;
        }
        g_ckpt_capture_destructive = 0;
        if (verdict == 0 && ckpt_capture_pipe_reason(pair[0], 0x11, NULL, NULL) != 0) verdict = 14;
        if (verdict == 0 && !g_ckpt_capture_destructive) verdict = 15;
        for (int index = 0; index < 2; ++index) {
            if (children[index] <= 0) continue;
            int status = 0;
            if (waitpid(children[index], &status, 0) != children[index]) verdict = verdict ? verdict : 16;
            if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) verdict = verdict ? verdict : 17;
        }
        struct ckpt_pipe_test_shared *shared = g_ckpt_pipe_test_shared;
        if (verdict == 0 && atomic_load(&shared->winners) != 1) verdict = 18;
        if (verdict == 0 && atomic_load(&shared->losers) != 2) verdict = 19;
        if (verdict == 0 && atomic_load(&shared->length) != CKPT_PIPE_TEST_PAYLOAD) verdict = 20;
        for (unsigned index = 0; verdict == 0 && index < CKPT_PIPE_TEST_PAYLOAD; ++index)
            if (shared->bytes[index] != ckpt_pipe_test_byte(index)) verdict = 21;
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != 0) verdict = 22;
    } else if (scenario == 1) {
        // The abort contract: the drain empties the pipe for EVERY holder, so every holder -- the winner and
        // each co-holder that returned 0 without reading a byte -- must be marked destructive. A holder left
        // unmarked resumes out of its park onto a pipe whose bytes are gone.
        if (ckpt_pipe_test_open_shared() != 0) return 30;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 31;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 32;
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        // Three descriptors on ONE open file description, exactly what fork hands each holder.
        int holders[3] = {pair[0], -1, -1};
        for (int index = 1; verdict == 0 && index < 3; ++index) {
            holders[index] = dup(pair[0]);
            if (holders[index] < 0) verdict = 33;
        }
        for (int index = 0; verdict == 0 && index < 3; ++index) {
            g_ckpt_capture_destructive = 0;
            if (ckpt_capture_pipe_reason(holders[index], 0x11, NULL, NULL) != 0) verdict = 34;
            if (verdict == 0 && !g_ckpt_capture_destructive) verdict = 35 + index;
        }
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != 0) verdict = 38;
        if (verdict == 0 && atomic_load(&g_ckpt_pipe_test_shared->winners) != 1) verdict = 39;
        for (int index = 1; index < 3; ++index)
            if (holders[index] >= 0) close(holders[index]);
    } else if (scenario == 2) {
        // A write-only end is not drainable and must never take the claim: winning an election it cannot
        // satisfy would publish an empty object and strand the buffered bytes in the kernel.
        if (ckpt_pipe_test_open_shared() != 0) return 50;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 51;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 52;
        int reader_flags = fcntl(pair[0], F_GETFL);
        int writer_flags = fcntl(pair[1], F_GETFL);
        if (verdict == 0 && (reader_flags < 0 || writer_flags < 0)) verdict = 53;
        if (verdict == 0 && !ckpt_pipe_end_drains(reader_flags)) verdict = 54;
        if (verdict == 0 && ckpt_pipe_end_drains(writer_flags)) verdict = 55;
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != (int)CKPT_PIPE_TEST_PAYLOAD) verdict = 56;
        if (verdict == 0 && atomic_load(&g_ckpt_pipe_test_shared->claim) != 0) verdict = 57;
    } else if (scenario == 3) {
        // Both capture paths ask the same question. The queued-rights path used to ask "== O_RDONLY", which
        // dropped an O_RDWR end on the floor: no object published, and the pipe restored empty.
        verdict = 0;
        if (!ckpt_pipe_end_drains(O_RDONLY)) verdict = 60;
        if (verdict == 0 && !ckpt_pipe_end_drains(O_RDWR)) verdict = 61;
        if (verdict == 0 && ckpt_pipe_end_drains(O_WRONLY)) verdict = 62;
        return verdict;
    } else if (scenario == 4) {
        // The whole closed round trip, capture half against restore half. Drain a pipe holding a known
        // payload, then hand the published object to the production refill and read the bytes back out of a
        // freshly created pipe: same bytes, same order, nothing left over.
        if (ckpt_pipe_test_open_shared() != 0) return 70;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 71;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 72;
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        g_ckpt_capture_destructive = 0;
        if (verdict == 0 && ckpt_capture_pipe_reason(pair[0], 0x11, NULL, NULL) != 0) verdict = 73;
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != 0) verdict = 74;
        int restored[2] = {-1, -1};
        struct ckpt_source *saved_source = ckpt_source_current();
        ckpt_source_install(&g_ckpt_pipe_test_source_ops);
        if (verdict == 0 && pipe(restored) != 0) verdict = 75;
        if (verdict == 0 && ckpt_refill_restore_pipe(restored[1], 0x11) != 0) verdict = 76;
        if (verdict == 0 && ckpt_pipe_test_buffered(restored[0]) != (int)CKPT_PIPE_TEST_PAYLOAD) verdict = 77;
        for (unsigned at = 0; verdict == 0 && at < CKPT_PIPE_TEST_PAYLOAD;) {
            unsigned char block[4096];
            ssize_t count = read(restored[0], block, sizeof block);
            if (count <= 0) {
                verdict = 78;
                break;
            }
            for (ssize_t index = 0; index < count; ++index)
                if (block[index] != ckpt_pipe_test_byte(at + (unsigned)index)) verdict = 79;
            at += (unsigned)count;
        }
        ckpt_source_install(saved_source ? saved_source->ops : NULL);
        if (restored[0] >= 0) close(restored[0]);
        if (restored[1] >= 0) close(restored[1]);
    } else if (scenario == 5) {
        // THE PRODUCT PROPERTY: a checkpoint refused for a non-destructive reason leaves the container
        // running unharmed.
        //
        // The descriptor set is walked in ASCENDING guest-fd order, so this fixture puts a drainable pipe
        // holding a known payload BELOW a descriptor the scan is guaranteed to refuse (a socket, which
        // ckpt_capture_native_fd refuses outright). Under the old single-pass ordering the pipe was drained
        // on the way to a refusal that consumed nothing, g_ckpt_capture_destructive latched, and every
        // member of the tree -- winner and losers alike -- took the terminal `cannot resume` arm.
        //
        // The assertions are the property, not the mechanism: after the refusal the pipe still holds every
        // byte the guest had buffered, and the disposition ckpt_coordinate_and_exit reads
        // (g_ckpt_capture_destructive) still says "resume". Nothing here names a pass or an ordering.
        if (pipe(pair) != 0) return 80;
        int refused_socket = socket(AF_UNIX, SOCK_STREAM, 0);
        if (refused_socket < 0) {
            close(pair[0]);
            close(pair[1]);
            return 81;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 82;
        // Ascending order is the whole point: the pipe must be admitted before the socket is reached.
        if (verdict == 0 && !(pair[0] < refused_socket)) verdict = 83;
        if (ckpt_pipe_test_open_shared() != 0) verdict = 84;
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        g_ckpt_capture_destructive = 0;
        if (verdict == 0 &&
            ckpt_pipe_test_refused_scan(pair[0], (uint64_t)0x11, refused_socket) == 0)
            verdict = 85; // the scan must refuse; a fixture whose refusal stopped firing proves nothing
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != (int)CKPT_PIPE_TEST_PAYLOAD)
            verdict = 86; // the guest's buffered bytes were consumed for a capture that was refused
        if (verdict == 0 && g_ckpt_capture_destructive != 0)
            verdict = 87; // the refusal is terminal for this member: the container does not survive it
        close(refused_socket);
    } else {
        return 99;
    }

    if (pair[0] >= 0) close(pair[0]);
    if (pair[1] >= 0) close(pair[1]);
    ckpt_sink_install(saved_sink ? saved_sink->ops : NULL);
    g_ckpt_capture_destructive = saved_destructive;
    ckpt_pipe_test_close_shared();
    return verdict;
}

// -------------------------------------------------- half-close capture and replay: behavioral fixture
//
// Measured on this host before any of it was written (AF_UNIX STREAM and SEQPACKET alike): a survivor's
// recv() returns 0 for a peer that CLOSED and for a peer that merely shutdown(SHUT_WR) and is still open,
// and returns 0 for the survivor's OWN shutdown(SHUT_RD).  One value, three states.  The only thing that
// tells them apart from the survivor's side is send(): EPIPE when the peer is gone, success when the peer
// merely stopped writing -- and a capture cannot send a probe byte into a guest's stream to find out.
//
// So capture records the direction each endpoint closed, from the arena that already tracks it, and
// restore replays it with shutdown(2).  These fixtures drive the production capture and the production
// mask->direction replay against real kernel sockets.

#define CKPT_HALFCLOSE_TEST_CAPACITY 8192u

static unsigned char g_ckpt_halfclose_test_bytes[CKPT_HALFCLOSE_TEST_CAPACITY];
static size_t g_ckpt_halfclose_test_length;

static int ckpt_halfclose_test_begin(struct ckpt_sink *sink, const char *group, const char *name, uint32_t flags,
                                     struct ckpt_sink_stream **out) {
    (void)group;
    (void)name;
    (void)flags;
    static struct ckpt_sink_stream stream;
    memset(&stream, 0, sizeof stream);
    stream.sink = sink;
    g_ckpt_halfclose_test_length = 0;
    *out = &stream;
    return 0;
}

static int ckpt_halfclose_test_write(struct ckpt_sink_stream *stream, const void *data, size_t size) {
    (void)stream;
    if (size > sizeof g_ckpt_halfclose_test_bytes - g_ckpt_halfclose_test_length) return -1;
    memcpy(g_ckpt_halfclose_test_bytes + g_ckpt_halfclose_test_length, data, size);
    g_ckpt_halfclose_test_length += size;
    return 0;
}

static int ckpt_halfclose_test_write_at(struct ckpt_sink_stream *stream, uint64_t offset, const void *data,
                                        size_t size) {
    (void)stream;
    if (offset > g_ckpt_halfclose_test_length || size > g_ckpt_halfclose_test_length - offset) return -1;
    memcpy(g_ckpt_halfclose_test_bytes + offset, data, size);
    return 0;
}

static int ckpt_halfclose_test_finish(struct ckpt_sink_stream *stream) {
    (void)stream;
    return 0;
}

static void ckpt_halfclose_test_abort(struct ckpt_sink_stream *stream) { (void)stream; }

static int ckpt_halfclose_test_claim(struct ckpt_sink *sink, const char *name) {
    (void)sink;
    (void)name;
    return 0;
}

static void ckpt_halfclose_test_unclaim(struct ckpt_sink *sink, const char *name) {
    (void)sink;
    (void)name;
}

static const ckpt_sink_vtable g_ckpt_halfclose_test_ops = {
    .begin = ckpt_halfclose_test_begin,
    .write = ckpt_halfclose_test_write,
    .write_at = ckpt_halfclose_test_write_at,
    .finish = ckpt_halfclose_test_finish,
    .abort = ckpt_halfclose_test_abort,
    .claim = ckpt_halfclose_test_claim,
    .unclaim = ckpt_halfclose_test_unclaim,
};

// Give the pair the identity and retained arena state an accepted guest connection would carry, so the
// production admission and capture arms see a socket they recognise rather than a bare host fd.
static void ckpt_halfclose_test_identify(int endpoint, int peer, uint64_t object) {
    // Attaches the shared socket-state arena as well as clearing the slots: without it every endpoint is
    // unretained, which is a REFUSAL arm of its own and would let this fixture pass for the wrong reason.
    sock_internal_identity_test_initialize(endpoint, object, 0);
    sock_internal_identity_test_initialize(peer, object + 1, 0);
    g_sock_object[endpoint] = object;
    g_sock_peer_object[endpoint] = object + 1;
    g_sock_object[peer] = object + 1;
    g_sock_peer_object[peer] = object;
    g_sock_fam[endpoint] = g_sock_fam[peer] = AF_UNIX;
    g_sock_stream[endpoint] = g_sock_stream[peer] = 1;
    g_sock_conn[endpoint] = g_sock_conn[peer] = 1;
}

static void ckpt_halfclose_test_forget(int endpoint, int peer) {
    sock_state_drop(endpoint);
    sock_state_drop(peer);
    g_sock_object[endpoint] = g_sock_peer_object[endpoint] = 0;
    g_sock_object[peer] = g_sock_peer_object[peer] = 0;
    g_sock_fam[endpoint] = g_sock_fam[peer] = 0;
    g_sock_stream[endpoint] = g_sock_stream[peer] = 0;
    g_sock_conn[endpoint] = g_sock_conn[peer] = 0;
}

HL_API int HL_TARGET_LOCAL(checkpoint_socket_halfclose_test)(uint32_t scenario) {
    struct ckpt_sink *saved_sink = ckpt_sink_current();
    int saved_destructive = g_ckpt_capture_destructive;
    int verdict = 99;
    int pair[2] = {-1, -1};
    if (scenario > 3) return 99;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) != 0) return 10;
    ckpt_halfclose_test_identify(pair[0], pair[1], UINT64_C(0x00c10cd000000001));
    ckpt_sink_install(&g_ckpt_halfclose_test_ops);

    if (scenario == 0) {
        // Capture records the direction THIS endpoint closed. The value is unreachable from getsockopt, so
        // a record that does not carry it cannot be replayed by anything downstream.
        verdict = 0;
        if (shutdown(pair[0], SHUT_WR) != 0) verdict = 11;
        sock_state_shutdown_observed(pair[0], SHUT_WR);
        if (verdict == 0 && ckpt_capture_socket_state(pair[0], UINT64_C(0x00c10cd000000001), 0) != 0) verdict = 12;
        struct ckpt_socket_state recorded;
        if (verdict == 0 && g_ckpt_halfclose_test_length < sizeof recorded) verdict = 13;
        if (verdict == 0) {
            memcpy(&recorded, g_ckpt_halfclose_test_bytes, sizeof recorded);
            if (recorded.magic != CKPT_SOCKET_STATE_MAGIC) verdict = 14;
            else if (recorded.shutdown_mask != SOCK_SHUTDOWN_WRITE) verdict = 15;
        }
        // The peer closed nothing and must record nothing: a mask that is merely "the pair is half-closed"
        // cannot say which end may still write.
        if (verdict == 0 && ckpt_capture_socket_state(pair[1], UINT64_C(0x00c10cd000000002), 0) != 0) verdict = 16;
        if (verdict == 0) {
            memcpy(&recorded, g_ckpt_halfclose_test_bytes, sizeof recorded);
            if (recorded.shutdown_mask != 0) verdict = 17;
        }
    } else if (scenario == 1) {
        // The drain reads 0 from a peer that is STILL OPEN, and must not record that as a closed peer. This
        // is the misinference: on restore a peer recorded closed has its descriptor destroyed, so a live
        // half-closed client would come back with no far end at all.
        verdict = 0;
        if (shutdown(pair[1], SHUT_WR) != 0) verdict = 21;
        sock_state_shutdown_observed(pair[1], SHUT_WR);
        if (verdict == 0 && ckpt_capture_socket_queue(pair[0], UINT64_C(0x00c10cd000000001), SOCK_STREAM) != 0)
            verdict = 22;
        struct ckpt_socket_queue_header header;
        if (verdict == 0 && g_ckpt_halfclose_test_length < sizeof header) verdict = 23;
        if (verdict == 0) {
            memcpy(&header, g_ckpt_halfclose_test_bytes, sizeof header);
            if (header.magic != CKPT_SOCKET_QUEUE_MAGIC) verdict = 24;
            else if (header.peer_closed != 0) verdict = 25;
        }
    } else if (scenario == 2) {
        // A half-closed endpoint is admissible now that the mask is representable. It used to be refused
        // outright, which failed the whole image for a state every long-lived client reaches.
        verdict = 0;
        if (shutdown(pair[0], SHUT_RD) != 0) verdict = 31;
        sock_state_shutdown_observed(pair[0], SHUT_RD);
        if (verdict == 0 && sock_state_shutdown(pair[0]) != SOCK_SHUTDOWN_READ) verdict = 32;
        if (verdict == 0 && sock_internal_checkpoint_admit(pair[0]) != 0) verdict = 33;
        if (verdict == 0 && sock_internal_checkpoint_admit(pair[1]) != 0) verdict = 34;
    } else {
        // The replay reproduces the measured kernel state, driven through the production mask->direction
        // map rather than a hand-written shutdown(): the survivor reads end of stream, BOTH ends stay open,
        // and the survivor can still send to a peer that only stopped writing.
        verdict = 0;
        int direction = ckpt_socket_shutdown_direction(SOCK_SHUTDOWN_WRITE);
        if (direction != SHUT_WR) verdict = 41;
        if (verdict == 0 && shutdown(pair[1], direction) != 0) verdict = 42;
        char received[8];
        if (verdict == 0 && recv(pair[0], received, sizeof received, MSG_DONTWAIT) != 0) verdict = 43;
        if (verdict == 0 && send(pair[0], "z", 1, MSG_NOSIGNAL | MSG_DONTWAIT) != 1) verdict = 44;
        if (verdict == 0 && recv(pair[1], received, sizeof received, MSG_DONTWAIT) != 1) verdict = 45;
        if (verdict == 0 && send(pair[1], "z", 1, MSG_NOSIGNAL | MSG_DONTWAIT) != -1) verdict = 46;
        if (verdict == 0 && ckpt_socket_shutdown_direction(0) != -1) verdict = 47;
        if (verdict == 0 && ckpt_socket_shutdown_direction(SOCK_SHUTDOWN_READ) != SHUT_RD) verdict = 48;
        if (verdict == 0 && ckpt_socket_shutdown_direction(SOCK_SHUTDOWN_READ | SOCK_SHUTDOWN_WRITE) != SHUT_RDWR)
            verdict = 49;
    }

    ckpt_halfclose_test_forget(pair[0], pair[1]);
    close(pair[0]);
    close(pair[1]);
    ckpt_sink_install(saved_sink ? saved_sink->ops : NULL);
    g_ckpt_capture_destructive = saved_destructive;
    return verdict;
}
#endif
