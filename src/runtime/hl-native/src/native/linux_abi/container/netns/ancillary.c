// Shared ownership metadata for macOS's DGRAM-backed Linux SOCK_SEQPACKET emulation. Definitions live
// ahead of ancillary translation because SCM_RIGHTS send/receive participates in the same lifetime.
#include "../ownership/transport.h"

#define SEQ_REF_N 4096
#define SOCK_STATE_N 4096

struct seq_ref {
    volatile uint32_t used;
    volatile uint32_t refs[2];
    volatile uint32_t pending[2];
};
static struct seq_ref *g_seq_refs;
static uint16_t g_seq_ref[HL_NFD];
static uint8_t g_seq_end[HL_NFD];

/* Mutable open-socket-description state must survive a trusted guest fork. The
 * descriptor-to-slot map is process-local and inherited by fork; each slot is
 * allocated from shared memory so a shutdown observed by one descendant is
 * visible to every process that inherited or duplicated that description. */
struct sock_state {
    volatile uint32_t used;
    volatile uint32_t refs;
    volatile uint32_t shutdown;
};
static struct sock_state *g_sock_states;
static uint16_t g_sock_state_ref[HL_NFD];

// ---- SCM ancillary data: Linux<->macOS cmsg framing translation (SOL_SOCKET/SCM_RIGHTS fd passing).
// hl uses host fds directly as guest fds, so the fd integers in an SCM_RIGHTS payload need no remap --
// only the cmsg framing differs: Linux hdr=16B (8B len @0, int level @8, int type @12), 8-byte align,
// SOL_SOCKET=1; macOS hdr=12B (4B len @0, int level @4, int type @8), 4-byte align, SOL_SOCKET=0xffff.
#define LX_CMSG_ALIGN(n) (((n) + 7u) & ~(size_t)7u) // Linux: 8-byte align
#define LX_CMSGHDR 16u                              // Linux cmsg header: 8(len)+4(level)+4(type)
#define LX_SOL_SOCKET 1
#define HL_CMSG_EVENTFD_MAGIC 0xddefd001u
#define HL_CMSG_SEQ_MAGIC 0xdd5e9001u
#define HL_CMSG_TIMERFD_MAGIC 0xdd71e001u
#define HL_CMSG_OFD_MAGIC 0xdd0fd001u

/* Stable open-file-description identity.  This lives before ancillary translation because SCM_RIGHTS
 * import/export must carry it; dup/close helpers later in the unified syscall translation unit share it. */
static uint64_t g_ofd_id[HL_NFD];
static _Atomic uint32_t g_ofd_next = 1;

static uint64_t ofd_identity_new(void) {
    uint32_t sequence = atomic_fetch_add_explicit(&g_ofd_next, 1u, memory_order_relaxed);
    if (sequence == 0) sequence = atomic_fetch_add_explicit(&g_ofd_next, 1u, memory_order_relaxed);
    return UINT64_C(0x4000000000000000) | ((uint64_t)(uint32_t)getpid() << 32) | sequence;
}

static uint64_t ofd_identity_ensure(int fd) {
    if (fd < 0 || fd >= HL_NFD) return 0;
    if (!g_ofd_id[fd]) g_ofd_id[fd] = ofd_identity_new();
    return g_ofd_id[fd];
}

struct hl_cmsg_eventfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint32_t slot;
    uint32_t sema;
    uint32_t nb; // guest EFD_NONBLOCK intent (g_eventfd_gnb) — the host fd is always O_NONBLOCK internally
};

struct hl_cmsg_seq_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint32_t slot;
    uint32_t end;
};

// A timerfd is an engine-emulated object (kqueue-shim host fd + per-fd deadline/interval/clock
// bookkeeping the timerfd read/gettime paths consult, keyed by fd number). Passing one over SCM_RIGHTS
// dups the shared host object into the receiver, but the receiver's fd number carries none of the
// routing state, so its read/poll would miss the timerfd path entirely. Carry that scalar state in a
// hidden trailer marker (mirroring the eventfd/seq trailers) so the received fd behaves like a dup.
struct hl_cmsg_timerfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint32_t first_oneshot;
    int32_t clock;
    int64_t deadline;
    int64_t interval;
    int32_t source_fd;  // sender's fd number (valid only within source_pid)
    int32_t source_pid; // sender engine pid: re-alias the host kqueue only when the receiver shares it
    uint32_t nb;
    uint32_t portable;
    uint32_t restore_shared;
    uint32_t reserved;
    uint64_t object;
    uint64_t shared_state; // restore-only MAP_SHARED address inherited by the reforked process tree
};

struct hl_cmsg_ofd_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint64_t identity;
};
_Static_assert(sizeof(struct hl_cmsg_ofd_meta) == HL_SOCKET_OWNER_OFD_ACK_OFFSET, "immutable OFD marker prefix");

struct hl_cmsg_memfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    int32_t seals;
    uint32_t reserved;
};

struct hl_cmsg_pipe_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint64_t identity;
    int32_t size;
    uint32_t reserved;
};

struct hl_cmsg_signalfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    int32_t source_pid;
    int32_t source_slot;
    uint64_t mask;
};

struct hl_cmsg_kqueue_meta {
    uint32_t magic;
    uint32_t ordinal;
    int32_t source_pid;
    int32_t source_fd;
    uint32_t kind;
    uint32_t nonblock;
    uint64_t object_id;
    uint32_t descriptor_flags;
    int32_t canonical_fd;
    uint32_t hidden_count;
    uint32_t reserved;
    uint64_t image_size;
};

struct hl_cmsg_epoll_watch {
    int32_t descriptor;
    uint32_t events;
    uint32_t armed;
    uint32_t reserved;
    uint64_t data;
};

static int kqueue_scm_export(int fd, struct hl_cmsg_kqueue_meta *metadata);
static int kqueue_scm_import(int fd, const struct hl_cmsg_kqueue_meta *metadata, int marker);
static int epoll_scm_hidden_export(struct hl_cmsg_kqueue_meta *metadata, int *fds, int capacity);
static int epoll_scm_image_remap(const struct hl_cmsg_kqueue_meta *metadata, int marker, const int *fds);

static __thread int g_cmsg_tmpfds[1024];
static __thread uint8_t g_cmsg_tmpfd_borrowed[1024];
static __thread int g_cmsg_ntmpfds;
static __thread uint16_t g_cmsg_seq_slot[253];
static __thread uint8_t g_cmsg_seq_end[253];
static __thread int g_cmsg_nseq;
static __thread uint16_t g_cmsg_event_slot[253];
static __thread int g_cmsg_nevent;
static int bound_attachment_borrow(int guest_fd, int *native_fd);
static void bound_attachment_release(int native_fd);

static int cmsg_tmpfd_track(int fd, int borrowed) {
    if (fd < 0 || g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    g_cmsg_tmpfds[g_cmsg_ntmpfds] = fd;
    g_cmsg_tmpfd_borrowed[g_cmsg_ntmpfds] = (uint8_t)(borrowed != 0);
    g_cmsg_ntmpfds++;
    return 0;
}

static void cmsg_tmpfds_close(void) {
    for (int i = 0; i < g_cmsg_ntmpfds; i++)
        if (g_cmsg_tmpfds[i] >= 0) {
            if (g_cmsg_tmpfd_borrowed[i])
                bound_attachment_release(g_cmsg_tmpfds[i]);
            else
                close(g_cmsg_tmpfds[i]);
        }
    g_cmsg_ntmpfds = 0;
}

static void cmsg_seq_finish(int sent) {
    if (!sent && g_seq_refs) {
        for (int i = 0; i < g_cmsg_nseq; i++) {
            uint32_t slot = g_cmsg_seq_slot[i];
            uint32_t end = g_cmsg_seq_end[i];
            __atomic_sub_fetch(&g_seq_refs[slot].pending[end], 1, __ATOMIC_ACQ_REL);
            __atomic_sub_fetch(&g_seq_refs[slot].refs[end], 1, __ATOMIC_ACQ_REL);
        }
    }
    g_cmsg_nseq = 0;
}

static void cmsg_event_finish(int sent) {
    if (!sent)
        for (int index = 0; index < g_cmsg_nevent; ++index) {
            uint32_t slot = g_cmsg_event_slot[index];
            if (slot < HL_NFD && g_eventfd_refs[slot] > 0) g_eventfd_refs[slot]--;
        }
    g_cmsg_nevent = 0;
}

static int cmsg_level_l2m(int lv) {
    return lv == LX_SOL_SOCKET ? SOL_SOCKET : lv;
}

static int cmsg_level_m2l(int lv) {
    return lv == SOL_SOCKET ? LX_SOL_SOCKET : lv;
}

static int cmsg_eventfd_marker(const struct hl_cmsg_eventfd_meta *m) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char tn[] = "/tmp/.hl-cmsgXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_seq_marker(const struct hl_cmsg_seq_meta *m) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char tn[] = "/tmp/.hl-seqXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_timerfd_marker(const struct hl_cmsg_timerfd_meta *m) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char tn[] = "/tmp/.hl-tfdXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

// ---- SCM_RIGHTS in-flight retention (macOS only) -------------------------------------------------
// Linux keeps a descriptor passed over an AF_UNIX socket alive for as long as the message carrying it
// sits in the receiving socket, so "sendmsg(fd) then close(fd)" -- the canonical fd-handoff idiom every
// broker/zygote uses -- is safe even if the peer has not called recvmsg yet. XNU does NOT: its unix-rights
// garbage collector (unp_gc) treats a file whose ONLY remaining reference is an in-flight message as
// unreachable and CLOSES it, and it runs asynchronously off any unix-socket close while rights are in
// flight. So the sender's close(2) can race a GC pass that has not yet observed the receiver's
// unp_externalize, and the passed socket is torn down under both peers: the receiver's read() reports EOF
// and the sender's next write() gets EPIPE (default disposition: SIGPIPE kills it). It is a scheduling
// accident, which is why it only shows up under load. Reproduced with a plain C program and no engine in
// the picture: ~1 in 150 runs on an idle machine, ~1 in 60 pinned to the efficiency band.
//
// Fix: keep an engine-private duplicate of every descriptor put in flight, so the file always has a
// reference that is NOT an in-flight message and can therefore never become GC-eligible. Every SCM_RIGHTS
// record already carries one OFD marker file per passed fd, so that marker doubles as the delivery
// receipt: the receiving engine stamps an ack byte past the metadata when it imports the trailer, and the
// sender drops the duplicate on the next sweep. Nothing about the guest-visible message changes.
#if defined(__linux__)
#define cmsg_inflight_sweep() ((void)0)
#define cmsg_inflight_hold(fd, marker) ((void)0)
#define cmsg_inflight_mark() ((void)0)
#define cmsg_inflight_finish(sent) ((void)(sent))
#define cmsg_inflight_ack(marker) ((void)(marker))
#define cmsg_inflight_is_retained(fd) (0)
#else
#define SCM_INFLIGHT_MAX 256
#define SCM_INFLIGHT_ACK_OFFSET ((off_t)HL_SOCKET_OWNER_OFD_ACK_OFFSET)

struct scm_inflight_hold {
    int retained; // engine-private duplicate of the passed descriptor
    int marker;   // engine-private duplicate of that fd's OFD marker file (the delivery receipt)
};
static struct scm_inflight_hold g_scm_hold[SCM_INFLIGHT_MAX];
static int g_scm_hold_n;
static int g_scm_hold_mark;

static void cmsg_inflight_drop(int index) {
    if (g_scm_hold[index].retained >= 0) close(g_scm_hold[index].retained);
    if (g_scm_hold[index].marker >= 0) close(g_scm_hold[index].marker);
    for (int i = index + 1; i < g_scm_hold_n; i++)
        g_scm_hold[i - 1] = g_scm_hold[i]; // keep oldest-first order so the eviction below is FIFO
    g_scm_hold_n--;
    if (g_scm_hold_mark > g_scm_hold_n) g_scm_hold_mark = g_scm_hold_n;
}

// Release every hold whose receipt has been stamped by the receiving engine.
static void cmsg_inflight_sweep(void) {
    for (int i = g_scm_hold_n - 1; i >= 0; i--) {
        uint8_t ack = 0;
        if (pread(g_scm_hold[i].marker, &ack, 1, SCM_INFLIGHT_ACK_OFFSET) == 1 && ack != 0) cmsg_inflight_drop(i);
    }
}

static void cmsg_inflight_hold(int fd, int marker) {
    // Bounded: a peer that never reads the message must not cost the sender descriptors without limit.
    // The oldest hold is the likeliest to have been consumed already, so evict that one.
    if (g_scm_hold_n >= SCM_INFLIGHT_MAX) cmsg_inflight_drop(0);
    int retained = fcntl(fd, F_DUPFD_CLOEXEC, 0);
    if (retained < 0) return; // out of descriptors: degrade to bare XNU behaviour rather than fail the send
    int receipt = fcntl(marker, F_DUPFD_CLOEXEC, 0);
    if (receipt < 0) {
        close(retained);
        return;
    }
    g_scm_hold[g_scm_hold_n].retained = retained;
    g_scm_hold[g_scm_hold_n].marker = receipt;
    g_scm_hold_n++;
}

static void cmsg_inflight_mark(void) {
    g_scm_hold_mark = g_scm_hold_n;
}

// A send that never left the building put nothing in flight, so its holds are released immediately.
static void cmsg_inflight_finish(int sent) {
    if (!sent)
        while (g_scm_hold_n > g_scm_hold_mark)
            cmsg_inflight_drop(g_scm_hold_n - 1);
    g_scm_hold_mark = g_scm_hold_n;
}

// Receiver side: stamp the receipt so the sender can drop its duplicate.
static void cmsg_inflight_ack(int marker) {
    uint8_t ack = 1;
    (void)pwrite(marker, &ack, 1, SCM_INFLIGHT_ACK_OFFSET);
}

// The retained duplicates back the engine's own lifetime guarantee, not the guest's fd table: the
// emulated execve's close-on-exec sweep must leave them alone (Linux keeps an in-flight fd alive across
// the sender's exec too).
static int cmsg_inflight_is_retained(int fd) {
    for (int i = 0; i < g_scm_hold_n; i++)
        if (g_scm_hold[i].retained == fd || g_scm_hold[i].marker == fd) return 1;
    return 0;
}
#endif

static int cmsg_ofd_marker(const struct hl_cmsg_ofd_meta *m, const hl_socket_owner_transport *owner) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-ofdXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m ||
        (owner != NULL &&
         pwrite(fd, owner, sizeof *owner, HL_SOCKET_OWNER_OFD_EXTENSION_OFFSET) != (ssize_t)sizeof *owner)) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_import_ofd_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_ofd_meta metadata;
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != HL_CMSG_OFD_MAGIC)
            break;
        if (metadata.ordinal >= (uint32_t)(visible - 1) || !metadata.identity) break;
        int fd = fds[metadata.ordinal];
        if (fd >= 0 && fd < HL_NFD) g_ofd_id[fd] = metadata.identity;
        hl_socket_owner_transport owner;
        if (pread(marker, &owner, sizeof owner, HL_SOCKET_OWNER_OFD_EXTENSION_OFFSET) == (ssize_t)sizeof owner &&
            owner.magic == HL_SOCKET_OWNER_TRANSPORT_MAGIC && owner.version == HL_SOCKET_OWNER_TRANSPORT_VERSION &&
            owner.size == sizeof owner && owner.key.birth_ns != 0 && (owner.key.device != 0 || owner.key.object != 0)) {
            /* hl_socket_owner_attach(fd, owner.key) is wired once the fd-owner
             * lifecycle table is merged. The successful send already owns the
             * descriptor reference, so import must not increment it. */
        }
        cmsg_inflight_ack(marker); // the fd is installed in this process now: the sender may drop its hold
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_memfd_marker(const struct hl_cmsg_memfd_meta *metadata) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-memfd-metaXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (write(fd, metadata, sizeof *metadata) != (ssize_t)sizeof *metadata) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_import_memfd_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_memfd_meta metadata;
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != UINT32_C(0x484c4d46))
            break;
        if (metadata.ordinal >= (uint32_t)(visible - 1)) break;
        int fd = fds[metadata.ordinal];
        if (fd >= 0 && fd < HL_NFD) {
            g_memfd_is[fd] = 1;
            g_memfd_seal[fd] = metadata.seals;
            memfd_reg_set_fd(fd, metadata.seals);
        }
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_pipe_marker(const struct hl_cmsg_pipe_meta *metadata) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-pipe-metaXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (write(fd, metadata, sizeof *metadata) != (ssize_t)sizeof *metadata) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_import_pipe_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_pipe_meta metadata;
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != UINT32_C(0x484c5049))
            break;
        if (metadata.ordinal >= (uint32_t)(visible - 1) || metadata.identity == 0) break;
        int fd = fds[metadata.ordinal];
        if (fd >= 0 && fd < HL_NFD) {
            g_pipe_identity[fd] = metadata.identity;
            g_pipesz[fd] = metadata.size;
        }
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_signalfd_marker(const struct hl_cmsg_signalfd_meta *metadata) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-sigfd-metaXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (write(fd, metadata, sizeof *metadata) != (ssize_t)sizeof *metadata) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_kqueue_marker(struct hl_cmsg_kqueue_meta *metadata) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-kqueue-metaXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (typed_inotify_scm_image_export(metadata, fd) != 0 || epoll_scm_image_export(metadata, fd) != 0 ||
        pwrite(fd, metadata, sizeof *metadata, 0) != (ssize_t)sizeof *metadata || lseek(fd, 0, SEEK_SET) < 0 ||
        cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    return fd;
}

static int cmsg_kqueue_placeholder(void) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-kqueue-fdXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    return fd;
}

static int cmsg_import_kqueue_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_kqueue_meta metadata;
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != UINT32_C(0x484c4b51) || metadata.hidden_count > (uint32_t)(visible - 1))
            break;
        int hidden_base = visible - 1 - (int)metadata.hidden_count;
        if (metadata.ordinal >= (uint32_t)hidden_base) break;
        int fd = fds[metadata.ordinal];
        int imported = -1;
        int adopted = 0;
        for (; adopted < (int)metadata.hidden_count; ++adopted) {
            int private_fd = hl_host_process_fd_private_adopt(fds[hidden_base + adopted]);
            if (private_fd < 0) break;
            fds[hidden_base + adopted] = private_fd;
        }
        if (adopted == (int)metadata.hidden_count &&
            (metadata.kind != 1 || epoll_scm_image_remap(&metadata, marker, fds + hidden_base) == 0) && fd >= 0)
            imported = kqueue_scm_import(fd, &metadata, marker);
        if (imported < 0)
            fprintf(stderr, "[scm-epoll] import failed kind=%u hidden=%u adopted=%d fd=%d errno=%d\n", metadata.kind,
                    metadata.hidden_count, adopted, fd, errno);
        if (imported <= 0) {
            for (int index = 0; index < adopted; ++index) {
                hl_host_process_fd_private_remove(fds[hidden_base + index]);
                close(fds[hidden_base + index]);
            }
            for (uint32_t index = (uint32_t)adopted; index < metadata.hidden_count; ++index)
                close(fds[hidden_base + (int)index]);
        }
        if (imported < 0 && fd >= 0) {
            close(fd);
            fds[metadata.ordinal] = -1;
        }
        close(marker);
        visible = hidden_base;
    }
    return visible;
}

static int cmsg_import_signalfd_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 3) {
        struct hl_cmsg_signalfd_meta metadata;
        int hidden = fds[visible - 2];
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != UINT32_C(0x484c5346))
            break;
        if (metadata.ordinal >= (uint32_t)(visible - 2)) break;
        int fd = fds[metadata.ordinal];
        if (fd >= 0 && fd < HL_NFD) {
            if (metadata.source_pid == (int32_t)getpid() && metadata.source_slot >= 0 &&
                metadata.source_slot < HL_SFD_MAX && g_sfd[metadata.source_slot].refs > 0) {
                g_sigfd_slot[fd] = (uint8_t)(metadata.source_slot + 1);
                g_sfd[metadata.source_slot].refs++;
                close(hidden);
            } else {
                int slot = sfd_alloc();
                int writer = slot >= 0 ? hl_host_process_fd_private_adopt(hidden) : -1;
                if (slot >= 0 && writer >= 0) {
                    g_sfd[slot].rd = fd;
                    g_sfd[slot].wr = writer;
                    g_sfd[slot].mask = metadata.mask;
                    g_sigfd_slot[fd] = (uint8_t)(slot + 1);
                } else {
                    if (writer >= 0) {
                        hl_host_process_fd_private_remove(writer);
                        close(writer);
                    } else
                        close(hidden);
                    if (slot >= 0) g_sfd[slot].refs = 0;
                }
            }
        } else
            close(hidden);
        close(marker);
        visible -= 2;
    }
    return visible;
}

// Peel trailing timerfd markers (LIFO; appended last by cmsg_l2m so imported first), restoring the
// emulation state onto the received fd so its read/poll/gettime route through the timerfd path.
static int cmsg_import_timerfd_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_timerfd_meta m;
        int marker = fds[visible - 1];
        memset(&m, 0, sizeof m);
        if (pread(marker, &m, sizeof m, 0) != (ssize_t)sizeof m || m.magic != HL_CMSG_TIMERFD_MAGIC) break;
        if (m.ordinal >= (uint32_t)(visible - 1)) break;
        int fd = fds[m.ordinal];
        if (fd >= 0 && fd < HL_NFD) {
            if (m.portable) {
                int timer = kqueue();
                if (timer < 0 || dup2(timer, fd) < 0) {
                    if (timer >= 0) close(timer);
                    close(marker);
                    visible--;
                    continue;
                }
                // Register the alias BEFORE the source descriptor goes away. On macOS a dup'd kqueue IS
                // the same queue and this is a no-op; on a Linux host the queue lives in a side table
                // keyed by descriptor, so without this the queue stayed reachable only through `timer`,
                // which is closed on the next line. The EVFILT_TIMER re-arm below then failed EBADF --
                // and its result is deliberately discarded -- so the received timerfd was never armed:
                // it read its pending expirations correctly but never polled ready (compat scm-timerfd).
                if (timer != fd) hl_native_kqueue_duplicate(timer, fd);
                if (timer != fd) close(timer);
            }
            g_timerfd[fd] = 1;
            g_epoll_family_seen = 1;
            g_tfd_deadline[fd] = m.deadline;
            g_tfd_interval[fd] = m.interval;
            g_tfd_clock[fd] = m.clock;
            g_tfd_nb[fd] = (uint8_t)(m.nb != 0);
            g_tfd_first_oneshot[fd] = (uint8_t)(m.first_oneshot != 0);
            // The timer is armed on the host kqueue registry keyed by fd number. Within the sending
            // engine process the received fd is a live dup of that kqueue, so alias it onto the same
            // queue (exactly as dup() does). Across processes the registry is not shared; the pid guard
            // skips the alias, leaving the received fd inert as before (no cross-process regression).
            if (!m.portable && m.source_pid == (int32_t)getpid()) {
                hl_native_kqueue_duplicate(m.source_fd, fd);
            } else {
                struct timerfd_shared_state *state = NULL;
                if (m.shared_state != 0 && (m.source_pid == (int32_t)getpid() || m.restore_shared != 0))
                    state = (struct timerfd_shared_state *)(uintptr_t)m.shared_state;
                if (state == NULL) {
                    state = mmap(NULL, sizeof *state, PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
                    if (state == MAP_FAILED) state = NULL;
                    if (state != NULL) {
                        memset(state, 0, sizeof *state);
                        state->deadline = m.deadline;
                        state->interval = m.interval;
                    }
                }
                if (state == NULL) {
                    close(marker);
                    visible--;
                    continue;
                }
                g_tfd_cslot[fd] = fd + 1;
                g_tfd_object[fd] = m.object ? m.object : ofd_identity_new();
                g_tfd_shared[fd] = state;
                g_tfd_refs[fd]++;
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                timerfd_shared_lock(state);
                int64_t next = state->deadline;
                uint64_t pending = state->pending;
                timerfd_shared_unlock(state);
                g_tfd_deadline[fd] = next;
                g_tfd_interval[fd] = state->interval;
                g_tfd_pending[fd] = pending;
                if (pending != 0 || next > now_ns) {
                    struct kevent event;
                    int64_t delay = pending != 0 ? 1 : next - now_ns;
                    EV_SET(&event, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS, delay, NULL);
                    (void)kevent(fd, &event, 1, NULL, 0, NULL);
                }
            }
        }
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_import_seq_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_seq_meta m;
        int marker = fds[visible - 1];
        memset(&m, 0, sizeof m);
        if (pread(marker, &m, sizeof m, 0) != (ssize_t)sizeof m || m.magic != HL_CMSG_SEQ_MAGIC) break;
        if (m.ordinal >= (uint32_t)(visible - 1) || m.slot >= SEQ_REF_N || m.end > 1) break;
        int fd = fds[m.ordinal];
        uint32_t pending = __atomic_load_n(&g_seq_refs[m.slot].pending[m.end], __ATOMIC_ACQUIRE);
        while (pending != 0 && !__atomic_compare_exchange_n(&g_seq_refs[m.slot].pending[m.end], &pending, pending - 1,
                                                            0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {}
        if (pending == 0) __atomic_add_fetch(&g_seq_refs[m.slot].refs[m.end], 1, __ATOMIC_ACQ_REL);
        if (fd >= 0 && fd < HL_NFD) {
            g_seq_ref[fd] = (uint16_t)(m.slot + 1);
            g_seq_end[fd] = (uint8_t)m.end;
        }
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_fd_is_write_sideband(int fd) {
    if (fd < 0) return 0;
    int fl = fcntl(fd, F_GETFL);
    if (fl < 0) return 0;
    if ((fl & O_ACCMODE) != O_WRONLY) return 0;
    if (!(fl & O_NONBLOCK)) return 0;
    struct stat st;
    if (fstat(fd, &st) != 0) return 0;
    return S_ISFIFO(st.st_mode);
}

static int cmsg_read_eventfd_marker(int fd, struct hl_cmsg_eventfd_meta *m) {
    if (fd < 0 || !m) return 0;
    memset(m, 0, sizeof *m);
    if (pread(fd, m, sizeof *m, 0) != (ssize_t)sizeof *m) return 0;
    return m->magic == HL_CMSG_EVENTFD_MAGIC;
}

static int cmsg_import_eventfd_trailer(int *fds, int nfds) {
    if (!fds || nfds <= 2) return nfds;
    int cap = nfds / 3 + 1;
    int *hidden = calloc((size_t)cap, sizeof(int));
    int *marker_fd = calloc((size_t)cap, sizeof(int));
    struct hl_cmsg_eventfd_meta *metas = calloc((size_t)cap, sizeof(*metas));
    if (!hidden || !marker_fd || !metas) {
        free(hidden);
        free(marker_fd);
        free(metas);
        return nfds;
    }
    int nmeta = 0;
    int visible = nfds;
    while (visible >= 3 && nmeta < cap) {
        int h = fds[visible - 2];
        int marker = fds[visible - 1];
        struct hl_cmsg_eventfd_meta m;
        if (!cmsg_fd_is_write_sideband(h)) break;
        if (!cmsg_read_eventfd_marker(marker, &m)) break;
        hidden[nmeta] = h;
        marker_fd[nmeta] = marker;
        metas[nmeta] = m;
        nmeta++;
        visible -= 2;
    }
    if (!nmeta) {
        free(hidden);
        free(marker_fd);
        free(metas);
        return nfds;
    }
    for (int i = 0; i < nmeta; i++)
        if (metas[i].ordinal >= (uint32_t)visible) {
            free(hidden);
            free(marker_fd);
            free(metas);
            return nfds;
        }
    for (int i = 0; i < nmeta; i++) {
        int h = hidden[i];
        int marker = marker_fd[i];
        struct hl_cmsg_eventfd_meta *m = &metas[i];
        int pub = fds[m->ordinal];
        if (pub >= 0 && pub < HL_NFD) {
            g_eventfd_peer[pub] = h + 1;
            g_eventfd_cslot[pub] = (int)m->slot + 1;
            g_eventfd_sema[pub] = (uint8_t)(m->sema != 0);
            eventfd_guest_nb_set(pub, m->nb != 0); // carry the OFD-shared guest blocking intent
            g_eventfd_refs[m->slot]++;
            // The imported read end must be host-O_NONBLOCK too (internal drains rely on it); the sender set
            // the write-side, but ensure the received public fd is non-blocking regardless of its origin.
            {
                int fl = fcntl(pub, F_GETFL);
                if (fl >= 0 && !(fl & O_NONBLOCK)) fcntl(pub, F_SETFL, fl | O_NONBLOCK);
            }
        } else {
            close(h);
        }
        close(marker);
    }
    free(hidden);
    free(marker_fd);
    free(metas);
    return visible;
}

static void cmsg_note_recv_sock_fd(int fd);

struct cmsg_export {
    int *descriptors;
    int capacity;
    int count;
};

static int cmsg_export_visible(struct cmsg_export *export, const int *fds, int count,
                               struct hl_cmsg_kqueue_meta *kqueue_metadata, int engine_metadata) {
    memset(kqueue_metadata, 0, (size_t)count * sizeof *kqueue_metadata);
    for (int index = 0; index < count; ++index) {
        if (engine_metadata && kqueue_scm_export(fds[index], &kqueue_metadata[index]) > 0) {
            int placeholder = cmsg_kqueue_placeholder();
            if (placeholder < 0) return EMFILE;
            kqueue_metadata[index].magic = UINT32_C(0x484c4b51);
            kqueue_metadata[index].ordinal = (uint32_t)index;
            kqueue_metadata[index].source_pid = (int32_t)getpid();
            kqueue_metadata[index].source_fd = fds[index];
            export->descriptors[export->count++] = placeholder;
            (void)ofd_identity_ensure(fds[index]);
            continue;
        }
        int native = fds[index];
        int borrowed = bound_attachment_borrow(fds[index], &native);
        if (borrowed < 0 || (borrowed > 0 && cmsg_tmpfd_track(native, 1) != 0)) {
            if (borrowed > 0) bound_attachment_release(native);
            return borrowed < 0 ? -borrowed : EMFILE;
        }
        export->descriptors[export->count++] = native;
        (void)ofd_identity_ensure(fds[index]);
        if (engine_metadata && fds[index] >= 0 && fds[index] < HL_NFD && g_seq_ref[fds[index]] && g_cmsg_nseq < 253) {
            uint32_t slot = g_seq_ref[fds[index]] - 1;
            uint32_t end = g_seq_end[fds[index]];
            __atomic_add_fetch(&g_seq_refs[slot].refs[end], 1, __ATOMIC_ACQ_REL);
            __atomic_add_fetch(&g_seq_refs[slot].pending[end], 1, __ATOMIC_ACQ_REL);
            g_cmsg_seq_slot[g_cmsg_nseq] = (uint16_t)slot;
            g_cmsg_seq_end[g_cmsg_nseq++] = (uint8_t)end;
        }
    }
    return 0;
}

static int cmsg_export_sequence_and_event(struct cmsg_export *export, const int *fds, int count) {
    for (int index = 0; index < count; ++index) {
        int fd = fds[index];
        if (fd >= 0 && fd < HL_NFD && g_seq_ref[fd]) {
            struct hl_cmsg_seq_meta metadata = {
                .magic = HL_CMSG_SEQ_MAGIC,
                .ordinal = (uint32_t)index,
                .slot = (uint32_t)(g_seq_ref[fd] - 1),
                .end = (uint32_t)g_seq_end[fd],
            };
            int marker = cmsg_seq_marker(&metadata);
            if (marker < 0) return EMSGSIZE;
            export->descriptors[export->count++] = marker;
        }
    }
    for (int index = 0; index < count; ++index) {
        int fd = fds[index];
        if (fd < 0 || fd >= HL_NFD || !g_eventfd_peer[fd]) continue;
        if (export->count + 2 > export->capacity) return EMSGSIZE;
        int hidden = g_eventfd_peer[fd] - 1;
        int flags = fcntl(hidden, F_GETFL);
        if (flags >= 0) fcntl(hidden, F_SETFL, flags | O_NONBLOCK);
        fcntl(hidden, F_SETFD, FD_CLOEXEC);
        int event_slot = eventfd_counter_slot(fd);
        if (event_slot < 0 || event_slot >= HL_NFD || g_cmsg_nevent >= 253) return EMSGSIZE;
        struct hl_cmsg_eventfd_meta metadata = {
            .magic = HL_CMSG_EVENTFD_MAGIC,
            .ordinal = (uint32_t)index,
            .slot = (uint32_t)event_slot,
            .sema = (uint32_t)(g_eventfd_sema[fd] != 0),
            .nb = (uint32_t)eventfd_guest_nb(fd),
        };
        g_eventfd_refs[event_slot]++;
        g_cmsg_event_slot[g_cmsg_nevent++] = (uint16_t)event_slot;
        int marker = cmsg_eventfd_marker(&metadata);
        if (marker < 0) return EMSGSIZE;
        export->descriptors[export->count++] = hidden;
        export->descriptors[export->count++] = marker;
    }
    return 0;
}

static int cmsg_export_memfd_and_pipe(struct cmsg_export *export, const int *fds, int count) {
    for (int index = 0; index < count; ++index) {
        int fd = fds[index];
        if (!memfd_ensure_fd(fd)) continue;
        if (export->count + 1 > export->capacity) return EMSGSIZE;
        struct hl_cmsg_memfd_meta metadata = {
            .magic = UINT32_C(0x484c4d46),
            .ordinal = (uint32_t)index,
            .seals = g_memfd_seal[fd],
        };
        int marker = cmsg_memfd_marker(&metadata);
        if (marker < 0) return EMSGSIZE;
        export->descriptors[export->count++] = marker;
    }
    for (int index = 0; index < count; ++index) {
        int fd = fds[index];
        if (fd < 0 || fd >= HL_NFD || g_pipe_identity[fd] == 0) continue;
        if (export->count + 1 > export->capacity) return EMSGSIZE;
        struct hl_cmsg_pipe_meta metadata = {
            .magic = UINT32_C(0x484c5049),
            .ordinal = (uint32_t)index,
            .identity = g_pipe_identity[fd],
            .size = g_pipesz[fd],
        };
        int marker = cmsg_pipe_marker(&metadata);
        if (marker < 0) return EMSGSIZE;
        export->descriptors[export->count++] = marker;
    }
    return 0;
}

static int cmsg_export_kqueue(struct cmsg_export *export, const struct hl_cmsg_kqueue_meta *metadata, int count) {
    for (int index = 0; index < count; ++index) {
        struct hl_cmsg_kqueue_meta item = metadata[index];
        if (item.kind == 0) continue;
        int hidden_count = epoll_scm_hidden_export(&item, NULL, 0);
        if (hidden_count < 0 || export->count + hidden_count + 1 > 253) return EMSGSIZE;
        if (export->count + hidden_count + 1 > export->capacity) {
            int expanded = export->count + hidden_count + 1;
            int *replacement = realloc(export->descriptors, (size_t)expanded * sizeof *replacement);
            if (replacement == NULL) return ENOMEM;
            export->descriptors = replacement;
            export->capacity = expanded;
        }
        if (hidden_count != 0 &&
            epoll_scm_hidden_export(&item, export->descriptors + export->count, hidden_count) != hidden_count)
            return EIO;
        export->count += hidden_count;
        int marker = cmsg_kqueue_marker(&item);
        if (marker < 0) return EMSGSIZE;
        export->descriptors[export->count++] = marker;
    }
    return 0;
}

static int cmsg_export_signal_and_timer(struct cmsg_export *export, const int *fds, int count) {
    for (int index = 0; index < count; ++index) {
        int fd = fds[index];
        if (fd < 0 || fd >= HL_NFD || !g_sigfd_slot[fd]) continue;
        int slot = g_sigfd_slot[fd] - 1;
        if (slot < 0 || slot >= HL_SFD_MAX || g_sfd[slot].wr < 0 || export->count + 2 > export->capacity)
            return EMSGSIZE;
        struct hl_cmsg_signalfd_meta metadata = {
            .magic = UINT32_C(0x484c5346),
            .ordinal = (uint32_t)index,
            .source_pid = (int32_t)getpid(),
            .source_slot = slot,
            .mask = g_sfd[slot].mask,
        };
        int marker = cmsg_signalfd_marker(&metadata);
        if (marker < 0) return EMSGSIZE;
        export->descriptors[export->count++] = g_sfd[slot].wr;
        export->descriptors[export->count++] = marker;
    }
    for (int index = 0; index < count; ++index) {
        int fd = fds[index];
        if (fd < 0 || fd >= HL_NFD || !g_timerfd[fd]) continue;
        if (export->count + 1 > export->capacity) return EMSGSIZE;
        struct hl_cmsg_timerfd_meta metadata = {
            .magic = HL_CMSG_TIMERFD_MAGIC,
            .ordinal = (uint32_t)index,
            .first_oneshot = (uint32_t)(g_tfd_first_oneshot[fd] != 0),
            .clock = g_tfd_clock[fd],
            .deadline = g_tfd_deadline[fd],
            .interval = g_tfd_interval[fd],
            .source_fd = fd,
            .source_pid = (int32_t)getpid(),
            .nb = (uint32_t)(g_tfd_nb[fd] != 0),
            .portable = 1,
            .object = g_tfd_object[fd],
            .shared_state = (uint64_t)(uintptr_t)g_tfd_shared[fd],
        };
        struct hl_cmsg_timerfd_meta placeholder_metadata = {0};
        int placeholder = cmsg_timerfd_marker(&placeholder_metadata);
        if (placeholder < 0) return EMSGSIZE;
        export->descriptors[index] = placeholder;
        int marker = cmsg_timerfd_marker(&metadata);
        if (marker < 0) return EMSGSIZE;
        export->descriptors[export->count++] = marker;
    }
    return 0;
}

static int cmsg_export_ofd(struct cmsg_export *export, const int *fds, int count) {
    for (int index = 0; index < count; ++index) {
        struct hl_cmsg_ofd_meta metadata = {
            .magic = HL_CMSG_OFD_MAGIC,
            .ordinal = (uint32_t)index,
            .identity = g_ofd_id[fds[index]],
        };
        int marker = cmsg_ofd_marker(&metadata, NULL);
        if (marker < 0) return EMSGSIZE;
        export->descriptors[export->count++] = marker;
        cmsg_inflight_hold(fds[index], marker);
    }
    return 0;
}

// guest(Linux) control buf -> host(macOS) control buf. Returns host bytes written (<=cap), 0/none,
// or -1 with *errp set. A partial ancillary conversion must never be sent: silently dropping SCM_RIGHTS
// fds leaves higher-level protocols with a successful data message but missing handles.
static ssize_t cmsg_l2m(const uint8_t *g, size_t glen, uint8_t *h, size_t cap, int engine_metadata, int *errp) {
    if (errp) *errp = 0;
    cmsg_tmpfds_close();
    cmsg_seq_finish(0);
    cmsg_event_finish(0);
    cmsg_inflight_finish(0);
    cmsg_inflight_sweep();
    cmsg_inflight_mark();
    size_t go = 0, ho = 0;
    while (go + LX_CMSGHDR <= glen) {
        uint64_t clen = *(const uint64_t *)(g + go); // Linux cmsg_len (8B)
        int lvl = *(const int *)(g + go + 8);
        int typ = *(const int *)(g + go + 12);
        if (clen < LX_CMSGHDR || go + clen > glen) {
            if (errp) *errp = EINVAL;
            return -1;
        }
        size_t dlen = (size_t)clen - LX_CMSGHDR; // payload bytes (e.g. N*4 fds)
        struct cmsg_export export = {0};
        if (lvl == LX_SOL_SOCKET && typ == SCM_RIGHTS && dlen >= sizeof(int)) {
            const int *fds = (const int *)(g + go + LX_CMSGHDR);
            int nfds = (int)(dlen / sizeof(int));
            if (nfds > 253) {
                if (errp) *errp = EINVAL;
                return -1;
            }
            export.capacity = nfds * 6; // visible + OFD/seq/timer markers + eventfd sideband pair
            export.descriptors = malloc((size_t)export.capacity * sizeof *export.descriptors);
            if (!export.descriptors) {
                if (errp) *errp = ENOMEM;
                return -1;
            }
            struct hl_cmsg_kqueue_meta kqueue_metadata[253];
            int error = cmsg_export_visible(&export, fds, nfds, kqueue_metadata, engine_metadata);
            if (!error && engine_metadata) error = cmsg_export_sequence_and_event(&export, fds, nfds);
            if (!error && engine_metadata) error = cmsg_export_memfd_and_pipe(&export, fds, nfds);
            if (!error && engine_metadata) error = cmsg_export_kqueue(&export, kqueue_metadata, nfds);
            if (!error && engine_metadata) error = cmsg_export_signal_and_timer(&export, fds, nfds);
            if (!error && engine_metadata) error = cmsg_export_ofd(&export, fds, nfds);
            if (error) {
                free(export.descriptors);
                if (errp) *errp = error;
                return -1;
            }
            dlen = (size_t)export.count * sizeof(int);
        }
        size_t need = CMSG_SPACE(dlen);
        if (ho + need > cap) {
            free(export.descriptors);
            if (errp) *errp = EMSGSIZE;
            return -1;
        }
        struct cmsghdr ch;
        memset(&ch, 0, sizeof ch);
        ch.cmsg_len = CMSG_LEN(dlen); // macOS 12+dlen
        ch.cmsg_level = cmsg_level_l2m(lvl);
        ch.cmsg_type = typ; // SCM_RIGHTS==1 on both
        memcpy(h + ho, &ch, sizeof ch);
        if (lvl == LX_SOL_SOCKET && typ == SCM_RIGHTS && export.count > 0)
            memcpy(CMSG_DATA((struct cmsghdr *)(h + ho)), export.descriptors, dlen);
        else
            memcpy(CMSG_DATA((struct cmsghdr *)(h + ho)), g + go + LX_CMSGHDR, dlen);
        free(export.descriptors);
        ho += need;
        go += LX_CMSG_ALIGN(clen);
    }
    return (ssize_t)ho;
}

// host(macOS) control buf -> guest(Linux) control buf, appending at `off`. Returns Linux bytes written
// (<=cap; stops at the guest-buffer boundary, leaving the kernel's MSG_CTRUNC in mh->msg_flags to be
// translated).
static ssize_t cmsg_m2l(const struct msghdr *mh, uint8_t *g, size_t cap, size_t off, int *truncp) {
    cmsg_inflight_sweep(); // cheap no-op unless this process has descriptors of its own still in flight
    if (truncp) *truncp = 0;
    size_t go = off;
    for (struct cmsghdr *c = CMSG_FIRSTHDR((struct msghdr *)mh); c; c = CMSG_NXTHDR((struct msghdr *)mh, c)) {
        if (c->cmsg_len < CMSG_LEN(0)) break;
        size_t dlen = (size_t)c->cmsg_len - CMSG_LEN(0); // payload bytes (macOS hdr=12)
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS && dlen >= sizeof(int)) {
            int nfds = (int)(dlen / sizeof(int));
            int *fds = (int *)CMSG_DATA(c);
            int visible = cmsg_import_ofd_trailer(fds, nfds);
            visible = cmsg_import_signalfd_trailer(fds, visible);
            visible = cmsg_import_kqueue_trailer(fds, visible);
            visible = cmsg_import_pipe_trailer(fds, visible);
            visible = cmsg_import_memfd_trailer(fds, visible);
            visible = cmsg_import_timerfd_trailer(fds, visible);
            visible = cmsg_import_eventfd_trailer(fds, visible);
            visible = cmsg_import_seq_trailer(fds, visible);
            for (int i = 0; i < visible; i++) {
                cmsg_note_recv_sock_fd(fds[i]);
            }
            dlen = (size_t)visible * sizeof(int);
        }
        size_t need = LX_CMSG_ALIGN(LX_CMSGHDR + dlen);
        if (go + LX_CMSGHDR + dlen > cap) {
            if (truncp) *truncp = 1;
            // Linux delivers a partial SCM_RIGHTS record with as many whole fds as fit in the
            // remaining control space and closes the fds that did not fit -- it does not drop the
            // whole record. Match that (and never leak the undelivered host fds).
            if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
                int *fds = (int *)CMSG_DATA(c);
                int total = (int)(dlen / sizeof(int));
                size_t room = (go + LX_CMSGHDR <= cap) ? cap - go - LX_CMSGHDR : 0;
                int keep = (int)(room / sizeof(int));
                if (keep > total) keep = total;
                for (int i = keep; i < total; i++)
                    if (fds[i] >= 0) close(fds[i]);
                if (keep > 0) {
                    size_t kb = (size_t)keep * sizeof(int);
                    *(uint64_t *)(g + go) = (uint64_t)(LX_CMSGHDR + kb);
                    *(int *)(g + go + 8) = cmsg_level_m2l(c->cmsg_level);
                    *(int *)(g + go + 12) = c->cmsg_type;
                    memcpy(g + go + LX_CMSGHDR, CMSG_DATA(c), kb);
                    go += LX_CMSG_ALIGN(LX_CMSGHDR + kb);
                }
            }
            break;
        }
        *(uint64_t *)(g + go) = (uint64_t)(LX_CMSGHDR + dlen); // Linux cmsg_len
        *(int *)(g + go + 8) = cmsg_level_m2l(c->cmsg_level);
        *(int *)(g + go + 12) = c->cmsg_type;
#if defined(SCM_CREDENTIALS)
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS && dlen >= 12) {
            const uint32_t *host = (const uint32_t *)CMSG_DATA(c);
            int guest_pid;
            if (hl_linux_pidmap_guest_checked(&g_pidmap, (int32_t)host[0], &guest_pid) != 0) {
                errno = ESRCH;
                return -1;
            }
            if (!hl_linux_pidmap_is_active(&g_pidmap) && g_init_hostpid && guest_pid == g_init_hostpid) guest_pid = 1;
            *(uint32_t *)(g + go + LX_CMSGHDR) = (uint32_t)guest_pid;
            *(uint32_t *)(g + go + LX_CMSGHDR + 4) = (uint32_t)cuid();
            *(uint32_t *)(g + go + LX_CMSGHDR + 8) = (uint32_t)cgid();
            if (dlen > 12) memcpy(g + go + LX_CMSGHDR + 12, CMSG_DATA(c) + 12, dlen - 12);
        } else
#endif
        {
            memcpy(g + go + LX_CMSGHDR, CMSG_DATA(c), dlen);
        }
        go += need;
    }
    return (ssize_t)go;
}

static void cmsg_lx_set_cloexec_fds(uint8_t *g, size_t glen) {
    size_t go = 0;
    while (go + LX_CMSGHDR <= glen) {
        uint64_t clen = *(uint64_t *)(g + go);
        int lvl = *(int *)(g + go + 8);
        int typ = *(int *)(g + go + 12);
        if (clen < LX_CMSGHDR || go + clen > glen) break;
        if (lvl == LX_SOL_SOCKET && typ == SCM_RIGHTS) {
            size_t dlen = (size_t)clen - LX_CMSGHDR;
            int *fds = (int *)(g + go + LX_CMSGHDR);
            for (size_t i = 0; i + sizeof(int) <= dlen; i += sizeof(int)) {
                int fd = fds[i / sizeof(int)];
                if (fd >= 0) fcntl(fd, F_SETFD, FD_CLOEXEC);
            }
        }
        go += LX_CMSG_ALIGN(clen);
    }
}

// Append a synthesized Linux SCM_CREDENTIALS record (SOL_SOCKET / type 2, struct ucred {pid,uid,gid}) at
// offset `off` in the guest control buffer, for a socket with SO_PASSCRED enabled (macOS has neither the
// option nor the auto-attached cmsg). Returns the new offset (8-aligned), or `off` unchanged if there is no
// room -- the caller then flags MSG_CTRUNC. See g_sock_passcred / case 212.
static size_t cmsg_add_cred(uint8_t *g, size_t off, size_t cap, int pid, int uid, int gid) {
    size_t need = LX_CMSGHDR + 12; // ucred = 3 x u32 = 12
    if (off + need > cap) return off;
    *(uint64_t *)(g + off) = (uint64_t)need; // Linux cmsg_len (payload + 16B hdr)
    *(int *)(g + off + 8) = LX_SOL_SOCKET;   // cmsg_level = SOL_SOCKET(1)
    *(int *)(g + off + 12) = 2;              // cmsg_type  = SCM_CREDENTIALS(2)
    *(uint32_t *)(g + off + 16) = (uint32_t)pid;
    *(uint32_t *)(g + off + 20) = (uint32_t)uid;
    *(uint32_t *)(g + off + 24) = (uint32_t)gid;
    return off + LX_CMSG_ALIGN(need);
}

// SOL_SOCKET option name: Linux -> macOS (they differ). -1 = ignore (unsupported here).
/*
 * Windows takes the identity arm with Linux, and that is not an oversight worth
 * re-litigating later: the socket vocabulary this layer calls on that host --
 * src/linux_abi/host_socket.h -- is written in LINUX constants throughout and
 * translates to the neutral host contract itself. Sending it the BSD numbers
 * below would be the classic cross-level aliasing defect rather than a
 * translation: BSD SO_ACCEPTCONN is 0x0002, and 2 is what Linux SO_REUSEADDR
 * already is, so a "translated" SO_REUSEADDR arrives as a request to read a
 * different option -- silently, since setting an option reports nothing.
 * Measured before this arm existed: SO_RCVBUF(8) became 0x1002, which the
 * neutral option table does not name, so a get-after-set reported failure.
 */
static int so_opt_l2m(int o) {
#if defined(__linux__) || defined(_WIN32)
    return o;
#else
    switch (o) {
    // SO_DEBUG
    case 1: return 0x0001;
    // SO_REUSEADDR
    case 2: return 0x0004;
    // SO_ERROR  (async-connect completion!)
    case 4: return 0x1007;
    // SO_DONTROUTE
    case 5: return 0x0010;
    // SO_BROADCAST
    case 6: return 0x0020;
    // SO_SNDBUF
    case 7: return 0x1001;
    // SO_RCVBUF
    case 8: return 0x1002;
    // SO_KEEPALIVE
    case 9: return 0x0008;
    // SO_OOBINLINE
    case 10: return 0x0100;
    // SO_LINGER (struct linger: same layout)
    case 13: return 0x0080;
    // SO_REUSEPORT
    case 15: return 0x0200;
    // SO_ACCEPTCONN
    case 30: return 0x0002;
    // SO_TYPE
    case 3: return 0x1008;
    // SO_RCVTIMEO(20)/SO_SNDTIMEO(21) are handled at the call site (case 208/209: real timeval translation +
    // arming); every other unknown SOL_SOCKET optname -> ignore here.
    default: return -1;
    }
#endif
}

// IPPROTO_TCP optname Linux -> macOS. CRITICAL: these numbers diverge, and a raw pass-through maps
// Linux TCP_KEEPIDLE(4)/TCP_CORK(3) onto macOS TCP_NOPUSH(4)/TCP_NOOPT(3) — TCP_NOPUSH *corks* the
// socket so a server's reply is never delivered until close (breaks redis & every keepalive-setting
// server). Map the known ones; ignore (-1) unknown rather than pass through and accidentally cork.
static int tcp_opt_l2m(int o) {
#if defined(__linux__) || defined(_WIN32)
    return o;
#else
    switch (o) {
    case 1: return 0x01;  // TCP_NODELAY  (same)
    case 2: return 0x02;  // TCP_MAXSEG   (same)
    case 3: return 0x04;  // Linux TCP_CORK     -> macOS TCP_NOPUSH (deliberate; guest asked to cork)
    case 4: return 0x10;  // Linux TCP_KEEPIDLE -> macOS TCP_KEEPALIVE (seconds)
    case 5: return 0x101; // Linux TCP_KEEPINTVL-> macOS TCP_KEEPINTVL
    case 6: return 0x102; // Linux TCP_KEEPCNT  -> macOS TCP_KEEPCNT
    default: return -1;   // unknown -> ignore (never pass a Linux number straight to macOS IPPROTO_TCP)
    }
#endif
}
