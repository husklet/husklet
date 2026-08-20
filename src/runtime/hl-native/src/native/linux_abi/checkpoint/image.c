#include <stdarg.h>

enum ckpt_fd_capture_result {
    CKPT_FD_CAPTURE_ERROR = -1,
    CKPT_FD_CAPTURE_NEXT = 0,
    CKPT_FD_CAPTURED = 1,
};

struct ckpt_phase_ledger {
    int enabled;
    const char *isa;
    uint32_t generation;
    int clock_failure;
    int descriptor;
};

_Static_assert(CKPT_ARCH_X86_64 == 1 && CKPT_ARCH_AARCH64 == 2, "checkpoint diagnostic ISA wire values");

static const char *ckpt_phase_isa_name(int architecture) {
    return architecture == CKPT_ARCH_AARCH64 ? "aarch64" : "x86_64";
}

static void ckpt_phase_emit(const struct ckpt_phase_ledger *ledger, const char *format, ...) {
    if (!ledger->enabled || ledger->descriptor < 0) return;
    char record[512];
    va_list arguments;
    va_start(arguments, format);
    int length = vsnprintf(record, sizeof record, format, arguments);
    va_end(arguments);
    if (length <= 0 || (size_t)length >= sizeof record) _exit(70);
    ssize_t written;
    do {
        written = write(ledger->descriptor, record, (size_t)length);
    } while (written < 0 && errno == EINTR);
    if (written != (ssize_t)length) _exit(70);
}

static int ckpt_phase_descriptor(void) {
    const char *value = hl_option_get("HL_DIAGNOSTIC_PORT");
    if (value == NULL || value[0] == 0) return -1;
    char *end = NULL;
    long descriptor = strtol(value, &end, 10);
    return end != value && *end == 0 && descriptor >= 0 && descriptor <= INT_MAX ? (int)descriptor : -1;
}

static uint64_t ckpt_phase_now_us(const struct ckpt_phase_ledger *ledger) {
    if (ledger->clock_failure) return 0;
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * UINT64_C(1000000) + (uint64_t)now.tv_nsec / UINT64_C(1000);
}

static uint64_t ckpt_phase_begin(const struct ckpt_phase_ledger *ledger) {
    return ledger->enabled ? ckpt_phase_now_us(ledger) : UINT64_MAX;
}

static void ckpt_phase_finish(const struct ckpt_phase_ledger *ledger, const char *phase, uint64_t started,
                              uint64_t budget_us) {
    if (!ledger->enabled || started == UINT64_MAX) return;
    uint64_t finished = ckpt_phase_now_us(ledger);
    if (started == 0 || finished == 0) {
        ckpt_phase_emit(
            ledger,
            "checkpoint_phase_ledger\tcomponent=native\tisa=%s\tsession=%u\tattempt=%u\tgeneration=%u\tphase=%s\t"
            "duration_us=0\tbudget_us=%llu\tclock=unavailable\toutcome=progress\tstatus=0\n",
            ledger->isa, ledger->generation, ledger->generation, ledger->generation, phase,
            (unsigned long long)budget_us);
        return;
    }
    ckpt_phase_emit(
        ledger,
        "checkpoint_phase_ledger\tcomponent=native\tisa=%s\tsession=%u\tattempt=%u\tgeneration=%u\tphase=%s\t"
        "duration_us=%llu\tbudget_us=%llu\tclock=ok\toutcome=progress\tstatus=0\n",
        ledger->isa, ledger->generation, ledger->generation, ledger->generation, phase,
        (unsigned long long)(finished >= started ? finished - started : 0), (unsigned long long)budget_us);
}

static void ckpt_phase_terminal(const struct ckpt_phase_ledger *ledger, const char *outcome, int status) {
    if (!ledger->enabled) return;
    const char *clock = ckpt_phase_now_us(ledger) == 0 ? "unavailable" : "ok";
    ckpt_phase_emit(
        ledger,
        "checkpoint_phase_ledger\tcomponent=native\tisa=%s\tsession=%u\tattempt=%u\tgeneration=%u\tphase=terminal\t"
        "duration_us=0\tbudget_us=0\tclock=%s\toutcome=%s\tstatus=%d\n",
        ledger->isa, ledger->generation, ledger->generation, ledger->generation, clock, outcome, status);
}

static _Noreturn void ckpt_phase_exit(const struct ckpt_phase_ledger *ledger, int status) {
    ckpt_phase_terminal(ledger, status == 0 ? "success" : "failure", status);
    _exit(status);
}

/* Why the coordinator refused, as a value the parent can read. The reason TEXT goes to the host over the
   channel; this is the durable, enumerable half of the same answer, carried in the child result's detail. */
enum ckpt_refusal_reason {
    CKPT_REFUSAL_RESOURCES = 1,        /* the coordinator could not allocate what enumeration needs */
    CKPT_REFUSAL_PEER_ENUMERATION = 2, /* the live peer set could not be read */
    CKPT_REFUSAL_PEER_QUIESCENCE = 3,  /* a participant never committed its group */
    CKPT_REFUSAL_SELF_DUMP = 4,        /* the init's own dump failed */
    CKPT_REFUSAL_PROCESS_COUNT = 5,    /* the committed group count is not the membership */
    CKPT_REFUSAL_FOREGROUND_GROUP = 6, /* the tty foreground group is outside the restored namespace */
    CKPT_REFUSAL_DIGEST = 7,           /* the image digest could not be taken */
    CKPT_REFUSAL_MANIFEST = 8,         /* the manifest could not be published */
};

/* Refuse the capture, saying why, and exit.
 *
 * Three things happen here that a bare `ckpt_phase_exit(ledger, 70)` did not do, and the absence of all
 * three is what made every checkpoint defect expensive:
 *
 *  - the reason reaches the HOST. It used to exist only as a `[ckpt] refuse:` line on the engine's stderr,
 *    so the embedder reported HL_STATUS_CORRUPT with detail 0 -- "the coordinator died" -- and named nothing.
 *  - the host FAILS THE CAPTURE NOW. The coordinator aborted only its own group and exited; nothing told
 *    the broker, so peers parked, resumed, and held their channels open, and the client waited out its
 *    entire 30s deadline over a decision taken at 0.3s. The deadline is not the problem and is not touched:
 *    it was being waited out for nothing.
 *  - the parent gets an enumerable status instead of a corrupt-record fallback.
 *
 * Best effort in this order deliberately: the stderr line is written first, so a refusal is still diagnosed
 * when the channel is the thing that broke. */
static _Noreturn void ckpt_coordinator_refuse(const struct ckpt_phase_ledger *ledger, enum ckpt_refusal_reason code,
                                              const char *reason) {
    fprintf(stderr, "[ckpt] refuse: %s\n", reason);
    ckpt_stream_capture_refused(reason);
    hl_engine_child_result_publish(0, HL_STATUS_NOT_SUPPORTED, (uint64_t)code);
    ckpt_phase_exit(ledger, 70);
}

static int ckpt_fd_was_captured(const struct ckpt_fd *records, int count, int fd) {
    for (int prior = 0; prior < count; ++prior)
        if (records[prior].gfd == fd) return 1;
    return 0;
}

static int ckpt_capture_early_emulated_fd(struct ckpt_fd *records, int *count, int fd) {
    struct ckpt_fd r;
    memset(&r, 0, sizeof r);
    r.gfd = fd;
    const char *early_emulated = ckpt_guest_kernel_fd(fd);
    if (early_emulated && strcmp(early_emulated, "socket") == 0 && fd >= 0 && fd < HL_NFD && g_sock_object[fd] != 0) {
        if (sock_internal_checkpoint_admit(fd) != 0) {
            fprintf(stderr,
                    "[ckpt] refuse: socket fd %d inadmissible errno=%d family=%d object=%016llx peer=%016llx "
                    "hidden=%u/%u conn=%u connecting=%u\n",
                    fd, errno, (int)g_sock_fam[fd], (unsigned long long)g_sock_object[fd],
                    (unsigned long long)g_sock_peer_object[fd], (unsigned)g_sock_identity_local_hidden[fd],
                    (unsigned)g_sock_identity_peer_hidden[fd], (unsigned)g_sock_conn[fd],
                    (unsigned)g_sock_connecting[fd]);
            return -1;
        }
        if (g_sock_identity_local_hidden[fd] && g_sock_peer_object[fd] == 0) {
            fprintf(stderr, "[ckpt] refuse: socket fd %d has a hidden local identity and no peer (family=%d)\n", fd,
                    (int)g_sock_fam[fd]);
            /* A refused AF_UNIX connect leaves the real descriptor privately bound so it can be retried.  It
             * has no reciprocal endpoint, and restoring that hidden bind as a guest-visible socket would
             * fabricate topology. */
            errno = ENOTCONN;
            return -1;
        }
        if (g_sock_peer_object[fd] == 0) {
            r.kind = CKF_SOCKET;
            r.flags = fcntl(fd, F_GETFL);
            r.descriptor_flags = fcntl(fd, F_GETFD);
            r.object_id = g_sock_object[fd];
            r.ofd_id = r.object_id;
            snprintf(r.path, sizeof r.path, "socket-state.%016llx", (unsigned long long)r.object_id);
            if (r.flags < 0 || r.descriptor_flags < 0 || ckpt_capture_socket_state(fd, r.object_id, 1) != 0) {
                fprintf(stderr, "[ckpt] refuse: unpaired socket fd %d state capture failed (family=%d conn=%u)\n", fd,
                        (int)g_sock_fam[fd], (unsigned)g_sock_conn[fd]);
                return -1;
            }
            records[(*count)++] = r;
            return CKPT_FD_CAPTURED;
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
            ckpt_capture_socket_queue(fd, r.object_id, (uint32_t)type) != 0) {
            fprintf(stderr, "[ckpt] refuse: paired socket fd %d capture failed (family=%d type=%d peer=%016llx)\n", fd,
                    (int)g_sock_fam[fd], type, (unsigned long long)r.auxiliary);
            return -1;
        }
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (early_emulated && strcmp(early_emulated, "epoll") == 0) {
        r.kind = CKF_EPOLL;
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.object_id = ckpt_epoll_identity(fd);
        r.ofd_id = r.object_id;
        if (r.descriptor_flags < 0 || !r.object_id) return -1;
        snprintf(r.path, sizeof r.path, "epoll.%016llx", (unsigned long long)r.object_id);
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (early_emulated && strcmp(early_emulated, "inotify") == 0) {
        inotify_object_assign(fd);
        r.kind = CKF_INOTIFY;
        r.flags = g_inotify_nb[fd] ? O_NONBLOCK : 0;
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.object_id = g_inotify_object[fd];
        r.ofd_id = r.object_id;
        if (r.descriptor_flags < 0 || !r.object_id) return -1;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    return CKPT_FD_CAPTURE_NEXT;
}

#if defined(HL_NATIVE_TEST_HOOKS)
HL_API int HL_TARGET_LOCAL(unix_identity_capture_test)(int fd) {
    struct ckpt_fd records[2];
    int count = 0;
    errno = 0;
    int status = ckpt_capture_early_emulated_fd(records, &count, fd);
    return status < 0 ? (errno != 0 ? errno : EIO) : 0;
}
#endif

/* Which launch-time standard descriptor, if any, `snapshot` names the same OPEN FILE DESCRIPTION as.
 *
 * The runtime around this engine owns stdin/stdout/stderr, and a restored engine is handed a fresh bridge
 * for them. A guest that duplicates one of those descriptors elsewhere holds the SAME object, not a
 * guest-created pipe: busybox ash's savefd moves the stdout it is about to redirect to the first free
 * descriptor at or above 10 for the duration of `printf x >> file`, so a capture landing inside that
 * window sees the runtime's own stdout pipe at fd 10. Keying the stdio exemption on the descriptor
 * NUMBER refused those captures outright, which is why the refusal was intermittent and load-correlated.
 *
 * Identity is the open file description, so this exempts nothing a guest created: a pipe the guest opened
 * has an OFD of its own and is still refused.
 *
 * Returns the standard descriptor number, or -1 when `snapshot` names a distinct object. */
static int ckpt_stdio_alias(const hl_linux_fd_snapshot *snapshot, int fd) {
    if (snapshot == NULL || fd <= STDERR_FILENO || snapshot->ofd == 0 || g_linux_box == NULL) return -1;
    for (int standard = STDIN_FILENO; standard <= STDERR_FILENO; standard++) {
        hl_linux_fd_snapshot known;
        if (hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)standard, &known) != HL_STATUS_OK) continue;
        if (known.ofd != 0 && known.ofd == snapshot->ofd) return standard;
    }
    return -1;
}

static int ckpt_capture_typed_fd(struct ckpt_fd *records, int *count, int fd) {
    hl_linux_fd_snapshot snapshot;
    if (g_linux_box == NULL || hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &snapshot) != HL_STATUS_OK)
        return CKPT_FD_CAPTURE_NEXT;
    struct ckpt_fd r;
    memset(&r, 0, sizeof r);
    r.gfd = fd;
    hl_host_file_metadata metadata;
    if (snapshot.kind == HL_LINUX_OBJECT_INOTIFY) {
        r.kind = CKF_INOTIFY;
        r.flags = (int32_t)snapshot.status_flags;
        r.descriptor_flags = (int32_t)snapshot.descriptor_flags;
        r.object_id = UINT64_C(0x9000000000000000) | (uint64_t)snapshot.ofd;
        r.ofd_id = r.object_id;
        snprintf(r.path, sizeof r.path, "inotify.%016llx", (unsigned long long)r.object_id);
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (g_linux_box->host == NULL || g_linux_box->host->file == NULL || g_linux_box->host->file->metadata == NULL ||
        g_linux_box->host->file->metadata(g_linux_box->host->context, snapshot.host_handle, &metadata).status !=
            HL_STATUS_OK) {
        if (fcntl(fd, F_GETFD) < 0 && errno == EBADF) {
            proc_fdvis_close(fd);
            return CKPT_FD_CAPTURED;
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
        /* Launch-time stdio is owned by the runtime around this engine.  A restored engine receives a
         * fresh stdin/stdout/stderr bridge, just as it receives a fresh pty; rebuilding those descriptors
         * as an isolated guest pipe would disconnect logs and eventually block the guest. */
        if (fd >= 0 && fd <= STDERR_FILENO) {
            r.kind = CKF_TTY;
            r.offset = 0;
            records[(*count)++] = r;
            return CKPT_FD_CAPTURED;
        }
        int alias = ckpt_stdio_alias(&snapshot, fd);
        if (alias >= 0) {
            /* Restored as a duplicate of the standard descriptor whose open file description it shares,
             * so the guest's own save/restore of that descriptor still names the runtime's bridge. */
            r.kind = CKF_TTY;
            r.offset = 0;
            r.auxiliary |= CKFA_STDIO_ALIAS | ((uint64_t)alias << CKFA_STDIO_ALIAS_SHIFT);
            records[(*count)++] = r;
            return CKPT_FD_CAPTURED;
        }
        fprintf(stderr, "[ckpt] refuse: guest fd %d is a pipe -- shared pipe restore is not yet supported\n", fd);
        return -1;
    }
    if (metadata.type == HL_HOST_FILE_TYPE_SOCKET) {
        fprintf(stderr, "[ckpt] refuse: typed guest fd %d is a socket -- socket restore is not yet supported\n", fd);
        return -1;
    }
    if (metadata.type == HL_HOST_FILE_TYPE_CHARACTER || metadata.type == HL_HOST_FILE_TYPE_BLOCK) {
        char fp[512];
        hl_host_result device_path = g_linux_box->host->file->path(g_linux_box->host->context, snapshot.host_handle,
                                                                   (hl_host_bytes){fp, sizeof(fp) - 1});
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
            if (metadata.type != HL_HOST_FILE_TYPE_REGULAR || ckpt_capture_file_blob(fd, r.path, sizeof r.path) != 0) {
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
    records[(*count)++] = r;
    return CKPT_FD_CAPTURED;
}

#if defined(HL_NATIVE_TEST_HOOKS)
#include "../../bridge/host.h"

// ------------------------------------------- stdio aliasing under capture: behavioral fixture
//
// Drives the REAL ckpt_capture_typed_fd against a REAL guest descriptor table built over REAL kernel
// pipes, in the exact shape the intermittent close failure had: the runtime's own stdout pipe reachable
// at a high guest descriptor because the guest duplicated it there.
//
// busybox ash's savefd moves the stdout it is about to redirect to the first free descriptor at or above
// 10 for the duration of `printf x >> file`. A capture landing in that window used to refuse the whole
// checkpoint with "guest fd 10 is a pipe", because the stdio exemption tested the descriptor NUMBER.
// Scenario 0 is that capture. Scenario 1 is the half that stops the fix from being a widening: a pipe
// the GUEST created, at a descriptor of its own, is still refused -- it prints the refusal, which is the
// expected output of that scenario and not a fixture failure.
struct ckpt_stdio_alias_test_box {
    hl_linux_abi box;
    hl_linux_fd_entry *fds;
    hl_linux_ofd_entry *ofds;
    hl_native_host *host;
    hl_host_services services;
};

static int ckpt_stdio_alias_test_install(struct ckpt_stdio_alias_test_box *fixture, int native_fd, int guest_fd,
                                         uint32_t status_flags) {
    hl_host_result imported = hl_c_bridge_host_import_file((hl_c_bridge_host *)fixture->host, native_fd,
                                                           (status_flags & O_ACCMODE) == O_RDONLY
                                                               ? HL_HOST_FILE_READ
                                                               : HL_HOST_FILE_WRITE);
    if (imported.status != HL_STATUS_OK) return -1;
    if (hl_linux_fd_install_at(&fixture->box, (hl_linux_fd)guest_fd, imported.value, status_flags, 0) !=
        HL_STATUS_OK) {
        (void)fixture->services.file->close(fixture->services.context, imported.value);
        return -1;
    }
    return 0;
}

HL_API int HL_TARGET_LOCAL(checkpoint_stdio_alias_capture_test)(uint32_t scenario) {
    if (scenario > 1) return -22;
    struct ckpt_stdio_alias_test_box fixture;
    memset(&fixture, 0, sizeof fixture);
    hl_linux_abi *saved_box = g_linux_box;
    int runtime_stdio[2] = {-1, -1};
    int guest_pipe[2] = {-1, -1};
    int verdict = 99;

    if (hl_native_host_create(&fixture.host, &fixture.services) != HL_STATUS_OK) return 10;
    fixture.fds = calloc(HL_LINUX_FD_LIMIT, sizeof(*fixture.fds));
    fixture.ofds = calloc(HL_LINUX_OFD_LIMIT, sizeof(*fixture.ofds));
    if (fixture.fds == NULL || fixture.ofds == NULL ||
        hl_linux_abi_init(&fixture.box, &fixture.services, fixture.fds, HL_LINUX_FD_LIMIT, fixture.ofds,
                          HL_LINUX_OFD_LIMIT) != HL_STATUS_OK) {
        verdict = 11;
        goto release;
    }
    g_linux_box = &fixture.box;
    if (pipe(runtime_stdio) != 0 || pipe(guest_pipe) != 0) {
        verdict = 12;
        goto release;
    }
    // The runtime's own stdio bridge: a pipe, exactly as hl-container hands one to a headless launch.
    if (ckpt_stdio_alias_test_install(&fixture, runtime_stdio[0], 0, O_RDONLY) != 0 ||
        ckpt_stdio_alias_test_install(&fixture, runtime_stdio[1], 1, O_WRONLY) != 0 ||
        ckpt_stdio_alias_test_install(&fixture, runtime_stdio[1], 2, O_WRONLY) != 0) {
        verdict = 13;
        goto release;
    }
    // savefd: dup guest fd 1 upward until it lands at 10, then drop the rungs. Every one of these shares
    // fd 1's OPEN FILE DESCRIPTION; fd 2 above holds an independent one over the same kernel pipe, so a
    // fix keying on the pipe object rather than on the description would not satisfy this fixture.
    for (int rung = 3; rung <= 10; ++rung) {
        hl_linux_fd landed = 0;
        if (hl_linux_fd_dup(&fixture.box, 1, 0, &landed) != HL_STATUS_OK || (int)landed != rung) {
            verdict = 14;
            goto release;
        }
    }
    for (int rung = 3; rung < 10; ++rung)
        (void)hl_linux_fd_close(&fixture.box, (hl_linux_fd)rung, NULL);
    // A pipe the guest itself created, at a descriptor of its own.
    if (ckpt_stdio_alias_test_install(&fixture, guest_pipe[1], 11, O_WRONLY) != 0) {
        verdict = 15;
        goto release;
    }

    struct ckpt_fd records[4];
    memset(records, 0, sizeof records);
    int count = 0;
    if (scenario == 0) {
        if (ckpt_capture_typed_fd(records, &count, 10) != CKPT_FD_CAPTURED || count != 1) {
            verdict = 20;
            goto release;
        }
        if (records[0].gfd != 10 || records[0].kind != CKF_TTY) {
            verdict = 21;
            goto release;
        }
        if ((records[0].auxiliary & CKFA_STDIO_ALIAS) == 0 ||
            (int)((records[0].auxiliary >> CKFA_STDIO_ALIAS_SHIFT) & CKFA_STDIO_ALIAS_MASK) != 1) {
            verdict = 22;
            goto release;
        }
        verdict = 0;
    } else {
        if (ckpt_capture_typed_fd(records, &count, 11) != CKPT_FD_CAPTURE_ERROR || count != 0) {
            verdict = 30;
            goto release;
        }
        verdict = 0;
    }

release:
    for (int fd = 0; fd <= 11; ++fd) (void)hl_linux_fd_close(&fixture.box, (hl_linux_fd)fd, NULL);
    g_linux_box = saved_box;
    (void)hl_linux_abi_destroy(&fixture.box);
    free(fixture.fds);
    free(fixture.ofds);
    hl_native_host_destroy(fixture.host);
    for (int index = 0; index < 2; ++index) {
        if (runtime_stdio[index] >= 0) (void)close(runtime_stdio[index]);
        if (guest_pipe[index] >= 0) (void)close(guest_pipe[index]);
    }
    return verdict;
}
#endif

static int ckpt_capture_native_fd(struct ckpt_fd *records, int *count, const struct fdvis_view *view) {
    int fd = view->guest_fd;
    struct ckpt_fd r;
    memset(&r, 0, sizeof r);
    r.gfd = fd;
    hl_host_process_fd detail;
    char path[512];
    size_t path_size = 0;
    if (!hl_host_process_fd_read(getpid(), fd, &detail, path, sizeof(path) - 1, &path_size) ||
        (detail.flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) != 0) {
        if (fcntl(fd, F_GETFD) < 0 && errno == EBADF) {
            proc_fdvis_close(fd);
            return CKPT_FD_CAPTURED;
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
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
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
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
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
        for (int prior = 0; prior < *count; prior++)
            if (records[prior].kind == CKF_TIMERFD && records[prior].object_id == r.object_id) {
                pending = strtoull(records[prior].path + strcspn(records[prior].path, " ") + 1, NULL, 10);
                copied = 1;
                break;
            }
        // kevent() with a zero timeout CONSUMES the timer's pending expirations, so the admission pass must
        // not run it; the record it would fill is discarded anyway.
        if (!copied && !g_ckpt_admission_only) {
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
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (emulated && strcmp(emulated, "inotify") == 0) {
        inotify_object_assign(fd);
        r.kind = CKF_INOTIFY;
        r.flags = g_inotify_nb[fd] ? O_NONBLOCK : 0;
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.object_id = g_inotify_object[fd];
        r.ofd_id = r.object_id;
        if (r.descriptor_flags < 0 || !r.object_id) return -1;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (detail.kind == HL_HOST_FD_PIPE) {
        int flags = fcntl(fd, F_GETFL);
        int descriptor_flags = fcntl(fd, F_GETFD);
        uint64_t identity = view->object ? view->object : g_pipe_identity[fd];
        if (flags < 0 || descriptor_flags < 0 || identity == 0) {
            fprintf(stderr,
                    "[ckpt] refuse: pipe fd %d has invalid metadata (flags=%d descriptor_flags=%d object=%llu "
                    "registered=%llu)\n",
                    fd, flags, descriptor_flags, (unsigned long long)view->object,
                    (unsigned long long)g_pipe_identity[fd]);
            return -1;
        }
        const char *reason = NULL;
        int cause = 0;
        // The capacity gate is a refusal and reads nothing, so it is decided BEFORE the drain. It used to
        // sit after it, which meant a pipe with no readable capacity was emptied and then refused.
        int capacity = ckpt_pipe_capacity(fd);
        if (capacity <= 0) {
            fprintf(stderr, "[ckpt] refuse: pipe fd %d identity %llu has no readable capacity\n", fd,
                    (unsigned long long)identity);
            return -1;
        }
        if (ckpt_pipe_end_drains(flags) && ckpt_capture_pipe_reason(fd, identity, &reason, &cause) != 0) {
            if (cause != 0)
                fprintf(stderr, "[ckpt] refuse: cannot capture pipe fd %d identity %llu: %s (%s)\n", fd,
                        (unsigned long long)identity, reason ? reason : "unknown", strerror(cause));
            else
                fprintf(stderr, "[ckpt] refuse: cannot capture pipe fd %d identity %llu: %s\n", fd,
                        (unsigned long long)identity, reason ? reason : "unknown");
            return -1;
        }
        r.kind = CKF_PIPE;
        r.flags = flags;
        r.descriptor_flags = descriptor_flags;
        r.offset = (int64_t)identity;
        snprintf(r.path, sizeof r.path, "%d", capacity);
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (detail.kind == HL_HOST_FD_SOCKET) {
        if (emulated == NULL) return CKPT_FD_CAPTURED;
        fprintf(stderr, "[ckpt] refuse: native guest fd %d is a socket -- socket restore is not yet supported\n", fd);
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
        r.ofd_id = ckpt_native_ofd_id(records, *count, fd, r.object_id);
        int seals = g_memfd_seal[fd];
        (void)memfd_reg_get_fd(fd, &seals);
        r.auxiliary = (uint64_t)(unsigned)seals;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
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
    r.ofd_id = ckpt_native_ofd_id(records, *count, fd, r.object_id);
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
        /* Guest-owned anonymous objects are classified by ckpt_guest_kernel_fd above.  A descriptor with
         * no guest classification, no path, and no native file type is an engine runtime handle that must
         * be reconstructed by the new engine rather than serialized into the guest image. */
        return CKPT_FD_CAPTURED;
    }
    records[(*count)++] = r;
    return CKPT_FD_CAPTURED;
}

static int ckpt_scan_fds_walk(struct ckpt_fd *recs, int cap, int *out_n) {
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
        if (ckpt_fd_was_captured(recs, n, fd)) continue;
        int result = ckpt_capture_early_emulated_fd(recs, &n, fd);
        if (result == CKPT_FD_CAPTURE_ERROR) {
            fprintf(stderr, "[ckpt] refuse: early emulated fd %d capture failed\n", fd);
            return -1;
        }
        if (result == CKPT_FD_CAPTURED) continue;
        result = ckpt_capture_typed_fd(recs, &n, fd);
        if (result == CKPT_FD_CAPTURE_ERROR) {
            fprintf(stderr, "[ckpt] refuse: typed fd %d capture failed\n", fd);
            return -1;
        }
        if (result == CKPT_FD_CAPTURED) continue;
        if (ckpt_capture_native_fd(recs, &n, &views[index]) == CKPT_FD_CAPTURE_ERROR) {
            fprintf(stderr, "[ckpt] refuse: native fd %d capture failed\n", fd);
            return -1;
        }
    }
    *out_n = n;
    return 0;
}

// Two passes over the descriptor set: prove, then consume. See g_ckpt_admission_only in capture.c for why
// the split exists and why the claim election stays in the second pass. The first pass's records are
// discarded -- they exist only so the arms run their gates against a real record -- and the second pass is
// the one whose output becomes the image.
struct ckpt_scan_request {
    struct ckpt_fd *records;
    int capacity;
    int count;
};

static int ckpt_scan_fds_pass(void *context) {
    struct ckpt_scan_request *request = context;
    memset(request->records, 0, (size_t)request->capacity * sizeof *request->records);
    request->count = 0;
    return ckpt_scan_fds_walk(request->records, request->capacity, &request->count);
}

static int ckpt_scan_fds(struct ckpt_fd *recs, int cap, int *out_n) {
    struct ckpt_scan_request request = {recs, cap, 0};
    int result = ckpt_admit_then_consume(ckpt_scan_fds_pass, &request);
    *out_n = request.count;
    return result;
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
        size_t watch_capacity = HL_NFD + EP_PROVIDER_WATCH_LIMIT + EP_OBJECT_WATCH_LIMIT;
        size_t image_capacity = sizeof(struct ckpt_epoll_header) + watch_capacity * sizeof(struct ckpt_epoll_watch);
        unsigned char *image = malloc(image_capacity);
        if (image == NULL) return -1;
        struct ckpt_epoll_watch *watches = (void *)(image + sizeof(struct ckpt_epoll_header));
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
        struct ckpt_epoll_header header = {CKPT_EPOLL_MAGIC, used, 0};
        memcpy(image, &header, sizeof header);
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
/* A refused page dump names the step and the region it refused at. Every one of these paths used to
 * return -1 in silence, so `ABORT -- see the refusal above` pointed at nothing and the member's real
 * reason never reached the log the failure is observed in. Diagnostic only: no control flow changes. */
static void ckpt_pages_refuse(const char *step, uint64_t address) {
    char message[192];
    snprintf(message, sizeof message, "[ckpt] refuse: cannot %s\n", step);
    fprintf(stderr, message, (unsigned long long)address);
}

static int ckpt_dump_pages(struct ckpt_sink *sink, struct ckpt_sink_stream *f, size_t pagesz, uint64_t *out_n) {
    uint64_t nreg = 0;
    // One host mapping-table read for the whole dump; every region's anonymous-shared lookup is
    // answered from it. A truncated or unreadable scan cannot distinguish a shared anonymous region
    // from a private one, and guessing "private" is exactly the silent per-process copy this exists
    // to stop -- refuse instead.
    ckpt_anon_shared_scan();
    if (g_anon_shared_truncated) {
        fprintf(stderr, "[ckpt] refuse: cannot enumerate this process's shared anonymous mappings; a memory "
                        "region's shared identity would be unrepresentable\n");
        return -1;
    }
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
        // No g_filemap record and not a logical VMA: this is either an ordinary private anonymous
        // region or an ANONYMOUS MAP_SHARED one, and only the kernel can tell them apart (map.c
        // never registered either). See ckpt_anon_shared_object.
        int anon_shared_publisher = 0;
        if (reg.backing_object == 0) {
            uint64_t anon_object = 0, anon_offset = 0;
            if (ckpt_anon_shared_object(addr, glen ? glen : len, &anon_object, &anon_offset)) {
                reg.backing_object = anon_object;
                reg.backing_offset = anon_offset;
                reg.backing_shared = 1;
                reg.backing_emulated = 0;
                reg.backing_anon_shared = 1;
                // ONE publisher for the bytes, elected exactly as the pipe and socket queues are.
                // Nine members holding one 256 MiB PostgreSQL pool would otherwise write 2.3 GiB of
                // identical pages into the image; the losers record the region's topology and no
                // pages, and restore attaches them to the object the winner filled.
                char claim[128];
                snprintf(claim, sizeof claim, "anonshared.%016llx", (unsigned long long)anon_object);
                int claimed = ckpt_sink_claim(sink, claim);
                if (claimed < 0) {
                    fprintf(stderr, "[ckpt] refuse: cannot elect a publisher for anonymous shared region %llx+%llx\n",
                            (unsigned long long)addr, (unsigned long long)(glen ? glen : len));
                    return -1;
                }
                anon_shared_publisher = claimed;
            }
        }
        hl_logical_vma_descriptor logical;
        int is_logical = hl_logical_vma_global_describe(addr, &logical);
        if (is_logical < 0) { ckpt_pages_refuse("describe the logical VMA at %#llx", addr); return -1; }
        if (is_logical == 1) {
            /*
             * gmap tracks the original mmap while mprotect may split the
             * logical ledger. Emit every descriptor in this gmap separately;
             * the next outer entry (if any) skips descriptors it does not own.
             */
            size_t descriptor_count = hl_logical_vma_global_export(NULL, 0);
            hl_logical_vma_descriptor *descriptors =
                descriptor_count ? malloc(descriptor_count * sizeof(*descriptors)) : NULL;
            if (descriptor_count && descriptors == NULL) { ckpt_pages_refuse("allocate the logical VMA descriptors for %#llx", addr); return -1; }
            if (hl_logical_vma_global_export(descriptors, descriptor_count) != descriptor_count) {
                free(descriptors);
                errno = EAGAIN;
                ckpt_pages_refuse("export the logical VMA descriptors for %#llx", addr);
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
                    ckpt_pages_refuse("write a logical region inside %#llx", addr);
                    return -1;
                }
                nreg++;
            }
            free(descriptors);
            continue;
        }
        int64_t header_offset = ckpt_sink_tell(sink, f);
        if (header_offset < 0) { ckpt_pages_refuse("take the stream offset for region %#llx", addr); return -1; }
        if (ckpt_write_region(sink, f, &reg) != 0) { ckpt_pages_refuse("write the region header for %#llx", addr); return -1; }
        if ((!reg.backing_anon_shared || anon_shared_publisher) && ckpt_dump_region_bytes(sink, f, pagesz, &reg) != 0) {
            ckpt_pages_refuse("write the region bytes for %#llx", addr);
            return -1;
        }
        // Patch the region header in place now that npages is known (the streaming equivalent of the
        // old seek-back-and-rewrite).
        if (ckpt_write_region_at(sink, f, (uint64_t)header_offset, &reg) != 0) { ckpt_pages_refuse("patch the region header for %#llx", addr); return -1; }
        nreg++;
    }
    *out_n = nreg;
    return 0;
}

// This process's guest identity (pid / parent / group / session), mapped from host ids to guest space (the
// container init's real host pid/group/session all read back as 1). getppid()==g_init_hostpid means "child
// of init"; a host pgid/sid equal to g_init_hostpid is the container's own group/session (guest 1).
//
// `gpid` is read back from the group this image is being filed under, not recomputed: container_pid() is
// 1 for EVERY launch top (target/{aarch64,x86_64}.c set g_init_hostpid = getpid() per launch and
// container/state.c folds it to 1), so a container exec session recorded self_gpid = 1 while its group was
// named proc.<host pid>, and ckpt_validate_proc_tree rejected the whole image before the first fork.
//
// That same fold makes every g_init_hostpid comparison below fire on an exec session's OWN identity, which
// is why they are gated on `domain_root`: an exec top's pgid and sid are its own, not the container init's.
static int ckpt_self_identity(struct ckpt_meta *m, int gpid) {
    hl_host_process_info process;
    if (gpid <= 0) return -1;
    // A launch top that is not the container init is a container exec session: hl-container forks it out of
    // its own daemon, so its host parent is outside the container and it has no guest parent at all.
    //
    // "IS A LAUNCH TOP" IS ASKED OF THE LAUNCH, NOT OF THE PID. This used to read `container_pid() == 1`,
    // which was a true statement about launch tops only while every launch top folded its own identity to
    // guest 1. Once the identity registry gave each launch its real guest pid, an exec top answered its own
    // number, `domain_root` went false, and the exec top fell through to the parent lookup below -- where
    // its host parent is hl-container's daemon, outside the container and in no pidmap, so `self identity`
    // refused and took the whole capture down with it. `g_init_hostpid` is written exactly once per launch,
    // by the launch top itself (container/state.c:456,561; reached from engine/target/aarch64.c:567 and
    // engine/target/x86_64.c:1117 alike), and is inherited unchanged across fork -- so `it equals getpid()`
    // is true of a launch top and of nothing else, whatever guest pid that launch was handed.
    int domain_root = gpid != 1 && g_init_hostpid != 0 && g_init_hostpid == (int)getpid();
    m->self_gpid = gpid;
    m->domain_root_gpid = domain_root ? 1 : 0;
    if (domain_root) {
        // No parent, and its OWN process group and session. Measured on Docker 29.1.3: an exec session's
        // top process reads pid=7 ppid=0 pgrp=7 sid=7 in the container's pid namespace. Its host group and
        // session belong to the hl-container daemon OUTSIDE the container, so translating them would both
        // leak a host identity into the image and name a session leader no member of the image restores.
        m->ppid_gpid = 0;
        m->pgid_gpid = gpid;
        m->sid_gpid = gpid;
        return 0;
    }
    if (gpid == 1) {
        m->ppid_gpid = 0;
    } else {
        int pp = getppid();
        if (hl_linux_pidmap_guest_checked(&g_pidmap, (int32_t)pp, &m->ppid_gpid) != 0) {
            fprintf(stderr, "[ckpt] refuse: guest %d has no guest identity for its host parent %d\n", gpid, pp);
            return -1;
        }
        if (!hl_linux_pidmap_is_active(&g_pidmap) && g_init_hostpid && pp == g_init_hostpid) m->ppid_gpid = 1;
    }
    int pg = getpgid(0);
    if (hl_linux_pidmap_guest_checked(&g_pgidmap, (int32_t)pg, &m->pgid_gpid) != 0) {
        fprintf(stderr, "[ckpt] refuse: guest %d has no guest identity for its host process group %d\n", gpid, pg);
        return -1;
    }
    if (!hl_linux_pidmap_is_active(&g_pgidmap) && g_init_hostpid && pg == g_init_hostpid) m->pgid_gpid = 1;
    int sd = hl_host_process_read(getpid(), &process) ? (int)process.session : getsid(0);
    if (hl_linux_pidmap_guest_checked(&g_sidmap, (int32_t)sd, &m->sid_gpid) != 0) {
        fprintf(stderr, "[ckpt] refuse: guest %d has no guest identity for its host session %d\n", gpid, sd);
        return -1;
    }
    if (!hl_linux_pidmap_is_active(&g_sidmap) && g_init_hostpid && sd == g_init_hostpid) m->sid_gpid = 1;
    return 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
// ------------------------------------------ exec-session identity: behavioral fixture
//
// Drives the REAL ckpt_self_identity for the two shapes the `domain_root` predicate must tell apart, and
// it is a fixture only depth three can answer: a launch top whose guest pid is 1 reads the same under
// either predicate, so both scenarios use a launch that was handed a guest pid OTHER than 1 and carries a
// registered self identity -- exactly what a container `exec` session is once the identity registry hands
// each launch its own guest pid, and exactly the state in which container_pid() stopped answering 1.
//
//   0  THIS process is the launch top (g_init_hostpid == getpid()) and its registered guest pid is 7.
//      It is a container exec session: no guest parent, its own group, its own session. Under the retired
//      container_pid()==1 predicate this took the descendant path instead and reported its HOST parent,
//      group and session -- which is a different answer, not merely a slower one, so this scenario
//      separates the two designs rather than agreeing with both.
//   1  THIS process is NOT the launch top (g_init_hostpid names another process), with the same guest pid
//      7 registered. It is an ordinary forked guest process and must NOT be treated as a domain root: it
//      keeps a real parent and takes its group and session from the host. This is the direction the fix
//      must not widen -- a predicate that answered "domain root" for everything would pass scenario 0 and
//      fail here.
HL_API int HL_TARGET_LOCAL(checkpoint_launch_identity_test)(uint32_t scenario) {
    if (scenario > 1) return -22;
    int saved_init = g_init_hostpid;
    int saved_self = g_self_gpid;
    struct ckpt_meta m;
    memset(&m, 0, sizeof m);
    g_self_gpid = 7;
    g_init_hostpid = scenario == 0 ? (int)getpid() : (int)getpid() + 1;
    int host_group = (int)getpgid(0);
    int rc = ckpt_self_identity(&m, 7);
    g_init_hostpid = saved_init;
    g_self_gpid = saved_self;
    if (rc != 0) return -1;
    if (m.self_gpid != 7) return -2;
    if (scenario == 0) {
        if (m.domain_root_gpid != 1) return -3;
        if (m.ppid_gpid != 0) return -4;      /* an exec top has no guest parent at all */
        if (m.pgid_gpid != 7) return -5;      /* its own group, never the daemon's outside the container */
        if (m.sid_gpid != 7) return -6;       /* and its own session */
        return 0;
    }
    if (m.domain_root_gpid != 0) return -7;   /* a forked descendant is not a domain root */
    if (m.ppid_gpid == 0) return -8;          /* it has a real guest parent */
    if (m.pgid_gpid != host_group) return -9; /* and takes its group from the host, not from its own pid */
    return 0;
}
#endif

// Dump THIS process (RAM + cpu + fds) into `procdir` (temp dir + rename). Returns 0 on success, -1 on any
// failure or P3 refusal (nothing published on failure).
static struct cpu *g_ckpt_cpu_images;
static int g_ckpt_cpu_count;

static int ckpt_dump_self_locked(struct cpu *c, const char *group);

// Announce this process and its complete executor inventory to the broker while every one of its
// threads is stopped and the registry lock is held. The broker holds the only exact member set for
// the capture; until it acknowledges this process, nothing this process publishes is admissible.
static int ckpt_register_ready(struct cpu **live, int count) {
    size_t payload_size = 8 + (size_t)count * sizeof(uint32_t);
    unsigned char *payload = calloc(1, payload_size);
    if (payload == NULL) return -1;
    uint32_t encoded_count = (uint32_t)count;
    memcpy(payload, &encoded_count, sizeof encoded_count);
    for (int i = 0; i < count; i++) {
        /* A zero tid is the process leader, not a missing thread: the engine reads it as
           container_pid() everywhere else (thread/futex_mapping.c, checkpoint/capture.c), and a
           single-threaded guest process has exactly this one executor. */
        int tid = live[i] != NULL ? (live[i]->tid != 0 ? live[i]->tid : container_pid()) : 0;
        if (tid <= 0) {
            free(payload);
            return -1;
        }
        uint32_t executor = (uint32_t)tid;
        memcpy(payload + 8 + (size_t)i * sizeof executor, &executor, sizeof executor);
    }
    hl_ckpt_reply reply;
    int status = ckpt_stream_call(HL_CKPT_OP_REGISTER_READY, NULL, 0, 0, 0, payload, payload_size, &reply, NULL, 0);
    free(payload);
    if (status == HL_CKPT_STATUS_OK && reply.value != 0) return 0;
    /* A -1 status is a TRANSPORT failure and a >=0 status is the broker's own answer. Printing only the
       number has repeatedly been read as a broker refusal, which is the one thing a -1 is not. */
    if (status < 0) {
        const char *step = hl_ckpt_channel_failure();
        fprintf(stderr,
                "[ckpt] refuse: REGISTER_READY for host process %d never reached the broker: the channel could "
                "not %s\n",
                (int)getpid(), step != NULL ? step : "complete the round trip");
    } else {
        fprintf(stderr, "[ckpt] refuse: REGISTER_READY for host process %d was refused by the broker (status %d, "
                        "member %llu)\n",
                (int)getpid(), status, (unsigned long long)reply.value);
    }
    return -1;
}

// A member that has finished its own group holds its whole-process freeze -- every thread stopped in
// stw_park_handler, the thread registry lock still held -- until the coordinator says what to do with it.
// This is what makes the capture SOUND: without it a peer _exit()s as soon as it is done, so by the time
// another member captures the far end of a shared pipe or socket its owner is already dead, and "both
// owners were stopped and alive at capture" is unprovable.
//
// WHAT THIS LOOP MAY DO is constrained exactly as a fork-critical callback is (AGENTS.md): no allocation,
// no logging, no lock acquisition, no destructor walk. Every peer thread of this process is stopped inside
// a signal handler, so any lock this thread takes here can be one a stopped peer already holds. The loop
// touches a stack `hl_ckpt_reply` and one request/response round trip on this process's private channel,
// which allocates nothing and takes no lock once the channel is bound.
//
// WHY IT CANNOT DEADLOCK: every lock held across the park (g_quiesce_lock, g_stw_reg_lock,
// g_dispatch_gate, g_ckpt_barrier_active) is a per-process static. The coordinator is a different host
// process and cannot acquire any of them, and it never enters a parked member or asks it for work -- a
// shared object is captured by whichever member wins ckpt_sink_claim, and every other holder returns 0
// immediately. The identical lock set is already held for the whole of ckpt_dump_self_locked today; the
// park extends the hold and adds no edge.
//
// CRASH SAFETY has two independent legs, because a member holding g_stw_reg_lock forever is unrecoverable:
//   - the broker owns release. A dead or exited coordinator drops the capture, and a dead BROKER breaks
//     this channel, which reads as RESUME. Release is tied to a descriptor, not to anyone's liveness poll.
//   - the ANSWER, not a clock. The park used to expire after a fixed ~5 s, chosen to match the whole-tree
//     budget the rendezvous no longer has, and that pairing was load-bearing in the worst way: on a busy
//     box the rendezvous legitimately takes longer than 5 s, every member's park then expired and RESUMEd
//     itself, and the tree the coordinator was still assembling came back to life underneath it. Measured
//     on the Linux VM at load ~19: six members committed, all six printed `released: capture abandoned` at
//     ~5 s, and the guest resumed forking -- 607 transients enumerated and kicked over the next 55 s, none
//     of which could ever end the rendezvous, until the embedder's own deadline expired. A member holding
//     the freeze is the whole point of the park, so it must not decide on its own that a live capture has
//     taken too long. A HOLD answer is proof the broker is alive AND that this capture is still running,
//     so the park waits for as long as it keeps getting one. Every way the capture can actually die --
//     coordinator gone, broker gone, channel broken -- fails the round trip instead, and a failed round
//     trip reads as RESUME below, which is the leg that keeps this crash-safe without a clock.
#define CKPT_PARK_POLL_US 2000

static uint64_t ckpt_park_release_state(void) {
    hl_ckpt_reply reply;
    if (ckpt_stream_call(HL_CKPT_OP_RELEASE_WAIT, NULL, 0, 0, 0, NULL, 0, &reply, NULL, 0) != HL_CKPT_STATUS_OK)
        return HL_CKPT_RELEASE_RESUME;
    return reply.value;
}

static uint64_t ckpt_park_until_released(void) {
    for (;;) {
        uint64_t state = ckpt_park_release_state();
        if (state != HL_CKPT_RELEASE_HOLD) return state;
        usleep(CKPT_PARK_POLL_US);
    }
}

/* The step that ended THIS process's own dump, set by ckpt_dump_self_locked before it returns -1.
 * It exists only so the member can name its refusal on the wire; it changes no control flow. */
static const char *g_ckpt_member_refusal;

/* Tell the broker, BY NAME, that this member cannot contribute to the running capture.
 *
 * WHY IT IS NEEDED AT ALL. A member whose dump is refused aborts its group and then parks, exactly as a
 * member whose dump succeeded does -- the park is what keeps every member simultaneously stopped and alive,
 * and a refused member that ran away would break the freeze for everyone still capturing the far end of a
 * shared object it owns half of. But nothing told the broker anything: the coordinator's rendezvous waits
 * for `proc.<gpid>` to be committed by a process that has already decided it never will be, burns the whole
 * peer-quiescence budget, and then refuses with "it did not reach a checkpoint safepoint, OR its dump was
 * refused" -- a disjunction naming neither the process's real reason nor the step that produced it, tens of
 * seconds after the decision was taken. Reporting here converts that into an immediate, correctly-named
 * refusal at the host.
 *
 * WHY IT CANNOT WEAKEN ANYTHING. This runs only on paths that have ALREADY decided the dump is refused, and
 * CAPTURE_REFUSED can only fail a capture, never publish one. A member in any of these states is doomed to
 * refuse the whole capture already: it either never registered -- so it can never be counted by the seal,
 * and it parks alive rather than exiting, so the "gone and never registered" exemption cannot cover it --
 * or it registered and then aborted its group, which the rendezvous refuses for by construction. The only
 * thing that changes is when the host learns, and what it is told.
 *
 * BEST EFFORT, exactly like ckpt_stream_capture_refused itself: on the paths where the channel is what
 * broke, this round trip fails too and the coordinator's own deadline still ends the capture. */
static void ckpt_member_refuse(const char *group, const char *step) {
    char reason[HL_CKPT_STREAM_NAME_MAX];
    snprintf(reason, sizeof reason,
             "member %s (host process %d) refused its own dump: it could not %s; every member's state must be "
             "saved for the capture to be complete",
             group, (int)getpid(), step);
    fprintf(stderr, "[ckpt] refuse: %s\n", reason);
    ckpt_stream_capture_refused(reason);
}

static int ckpt_dump_self(struct cpu *c, const char *procdir, int park) {
    struct cpu *live[THREAD_REG_MAX];
    atomic_store_explicit(&g_ckpt_barrier_active, 1, memory_order_release);
    uint64_t request = stw_checkpoint_arm();
    ckpt_interrupt_threads(c);
    if (stw_checkpoint_wait(request) != 0) {
        fprintf(stderr, "[ckpt] refuse: stop-the-world barrier did not converge\n");
        ckpt_sink_group_abort(ckpt_sink_current(), procdir);
        if (park) ckpt_member_refuse(procdir, "stop every one of its own threads for the capture");
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    int count = stw_checkpoint_cpus(live, THREAD_REG_MAX);
    if (count < 1 || count > THREAD_REG_MAX) {
        fprintf(stderr, "[ckpt] refuse: invalid registered CPU count %d\n", count);
        ckpt_sink_group_abort(ckpt_sink_current(), procdir);
        if (park) ckpt_member_refuse(procdir, "enumerate its own stopped executors");
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    /* Every listed executor is stopped and the registry lock is still held, so this inventory is
       exactly the process's thread set at the instant the broker records it. */
    if (ckpt_register_ready(live, count) != 0) {
        fprintf(stderr, "[ckpt] refuse: participant REGISTER_READY was not acknowledged\n");
        ckpt_sink_group_abort(ckpt_sink_current(), procdir);
        if (park) ckpt_member_refuse(procdir, "prove its membership of the capture to the broker");
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    // Test-only, and the mirror of HL_CKPT_TEST_PEER_EXIT_BEFORE_JOIN: a member that PROVED membership
    // and then died before committing its group. It published objects, so its state is genuinely lost,
    // and the rendezvous exemption must not cover it -- the capture has to refuse. Placed after the
    // registration round trip and gated on `park` so only a peer, never the coordinator, can take it.
    if (park && hl_option_get("HL_CKPT_TEST_PEER_EXIT_AFTER_JOIN") != NULL) _exit(0);
    /* CPU images contain engine pointers to immutable seccomp filter nodes.
       Restoring those addresses would either remove the sandbox or dereference
       stale host memory. Until the filter bytecode is part of the checkpoint
       format, refuse the capture while every task is stopped and publish
       nothing. */
    for (int i = 0; i < count; i++)
        if (live[i]->seccomp_mode != 0 || live[i]->seccomp_filters != NULL) {
            fprintf(stderr, "[ckpt] refuse: CPU %d has unserialized seccomp state (mode=%d filters=%p)\n", i,
                    live[i]->seccomp_mode, (void *)live[i]->seccomp_filters);
            ckpt_sink_group_abort(ckpt_sink_current(), procdir);
            stw_checkpoint_end();
            atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
            return -1;
        }
    struct cpu *images = malloc((size_t)count * sizeof *images);
    if (!images) {
        ckpt_sink_group_abort(ckpt_sink_current(), procdir);
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    for (int i = 0; i < count; i++)
        images[i] = *live[i];
    g_ckpt_cpu_images = images;
    g_ckpt_cpu_count = count;
    int result = ckpt_dump_self_locked(c, procdir);
    if (result != 0) {
        ckpt_sink_group_abort(ckpt_sink_current(), procdir);
        if (park) ckpt_member_refuse(procdir, g_ckpt_member_refusal ? g_ckpt_member_refusal : "complete its dump");
    }
    /* Park BEFORE anything of the freeze is unwound, and park whether or not our own dump succeeded: a
       refused member that ran away would leave the coordinator unable to tell "refused" from "still
       working", and would break the freeze for every member still capturing a shared object it owns half
       of. Nothing between here and the release may allocate, log, or take a lock. */
    if (park) g_ckpt_release_state = ckpt_park_until_released();
    g_ckpt_cpu_images = NULL;
    g_ckpt_cpu_count = 0;
    free(images);
    atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
    stw_checkpoint_end();
    return result;
}

/* A member's dump ends in exactly one place, and the reason it ended there must survive to the log the
 * failure is observed in. Every step that can end the dump names itself, because `ABORT -- see the refusal
 * above` is worse than useless when the step that failed printed nothing: three lanes read the resulting
 * silence as evidence about the broker. The step name is the only diagnostic; it changes no control flow. */
#define CKPT_DUMP_FAIL(step)                                                                                   \
    do {                                                                                                       \
        failed_step = (step);                                                                                  \
        goto done;                                                                                             \
    } while (0)

/* THE STATUSES THE FREEZE CONSUMED.
 *
 * Filled by ckpt_reap_and_record below, drained into this coordinator's own image group by
 * ckpt_dump_self_locked. File-scope because the two are in the same translation unit and the collection has
 * to outlive the rendezvous loop that produces it: the coordinator dumps itself only after quiescence. Empty
 * in every non-coordinating member, which never reaps anybody. */
static struct ckpt_reaped_child *g_ckpt_reaped;
static size_t g_ckpt_reaped_count;

static int ckpt_dump_self_locked(struct cpu *c, const char *group) {
    g_ckpt_member_refusal = NULL; /* one dump, one reason: never report the previous attempt's step */
    // HL_UNTRUSTED routes every host-authority object through the sentry process, so this worker's
    // descriptor table does not describe the guest: ckpt_scan_fds would capture sentry-relative
    // handles that no restore can rebuild. Capturing under the sentry requires the sentry to export
    // its descriptor table, open-file descriptions and connection state across the control ring,
    // which is not implemented. hl-engine classifies this launch as
    // EngineError::CheckpointUnsupportedUnderSandbox before dispatch, so reaching here means the
    // gate was bypassed; refuse with a named cause rather than a bare failure.
    if (g_untrusted) {
        fprintf(stderr, "[ckpt] refuse: checkpoint unsupported under the sentry sandbox policy (HL_UNTRUSTED)\n");
        g_ckpt_member_refusal = "be captured at all under the sentry sandbox policy";
        return -1;
    }
    // fcntl/flock state lives outside the descriptor table, so the fd scan below
    // cannot see it and would publish an image that silently omits it. SysV IPC has
    // the same shape but is now captured (ckpt_sysv_capture, below); the lock domain
    // is not, so the process is admitted only when it holds no lock.
    if (ckpt_admit_ipc_and_lock_state() != 0) {
        g_ckpt_member_refusal = "be admitted: it holds IPC or file-lock state the image cannot carry";
        return -1;
    }
    struct ckpt_sink *sink = ckpt_sink_current();
    struct ckpt_fd *fdrecs = calloc(HL_NFD, sizeof *fdrecs);
    int nfd = 0;
    if (fdrecs == NULL || ckpt_scan_fds(fdrecs, HL_NFD, &nfd) != 0) {
        free(fdrecs);
        g_ckpt_member_refusal = "scan its own descriptor table"; // P3 refusal already reported
        return -1;
    }

    // Open this process's image group. The sink stages it; nothing is visible until group_commit.
    fprintf(stderr, "[ckpt] %s: begin (pid %d)\n", group, (int)getpid());
    if (ckpt_sink_group_begin(sink, group) != 0) {
        free(fdrecs);
        g_ckpt_member_refusal = "open its image group";
        return -1;
    }
    struct ckpt_sink_stream *fp = NULL, *ff = NULL;
    const char *failed_step = NULL;
    int ok = 0;
    size_t pagesz = hl_linux_host_map_granularity();

    struct ckpt_meta m;
    memset(&m, 0, sizeof m);
    m.magic = CKPT_MAGIC;
    m.version = CKPT_VERSION;
    m.arch = G_CKPT_ARCH;
    m.engine_identity = pcache_translator_identity();
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
    m.n_reaped = (uint64_t)g_ckpt_reaped_count;
    if (ckpt_self_identity(&m, ckpt_group_gpid(group)) != 0) CKPT_DUMP_FAIL("self identity"); // common cleanup: frees fdrecs and reports the abort
    snprintf(m.exe_path, sizeof m.exe_path, "%s", g_exe_path ? g_exe_path : "");
    for (int s = 0; s < 65; s++) { // capture this process's guest signal dispositions (restored on thaw)
        m.sig_handler[s] = g_sigact[s].handler;
        m.sig_flags[s] = g_sigact[s].flags;
        m.sig_mask[s] = g_sigact[s].mask;
    }

    if (ckpt_sink_begin(sink, group, "pages", 0, &fp) != 0) CKPT_DUMP_FAIL("open the pages stream");
    if (ckpt_dump_pages(sink, fp, pagesz, &m.n_regions) != 0) CKPT_DUMP_FAIL("dump the memory pages");
    if (ckpt_sink_finish(sink, &fp) != 0) CKPT_DUMP_FAIL("finish the pages stream");

    {
        size_t payload = (size_t)g_ckpt_cpu_count * sizeof *g_ckpt_cpu_images;
        size_t total = sizeof(struct ckpt_cpu_header) + payload;
        struct ckpt_cpu_header *cpu_file = malloc(total);
        if (!cpu_file) CKPT_DUMP_FAIL("allocate the CPU image");
        *cpu_file = (struct ckpt_cpu_header){CKPT_CPU_MAGIC, CKPT_VERSION, G_CKPT_ARCH, (uint64_t)g_ckpt_cpu_count,
                                             sizeof(struct cpu)};
        memcpy(cpu_file + 1, g_ckpt_cpu_images, payload);
        int cpu_rc = ckpt_sink_put(sink, group, "cpu", 0, cpu_file, total);
        free(cpu_file);
        if (cpu_rc != 0) CKPT_DUMP_FAIL("store the CPU image");
    }

    if (ckpt_sink_begin(sink, group, "fds", 0, &ff) != 0) CKPT_DUMP_FAIL("open the fds stream");
    for (int i = 0; i < nfd; i++)
        if (ckpt_sink_write(sink, ff, &fdrecs[i], sizeof fdrecs[i]) != 0) CKPT_DUMP_FAIL("write an fd record");
    if (ckpt_sink_finish(sink, &ff) != 0) CKPT_DUMP_FAIL("finish the fds stream");

    // SysV IPC is container-scoped and reachable from no descriptor, so it is captured here
    // rather than by the fd scan above. A failure to read the registry refuses the dump.
    if (ckpt_sysv_capture(sink, group) != 0) CKPT_DUMP_FAIL("capture SysV IPC");

    for (int i = 0; i < nfd; i++) {
        if (fdrecs[i].kind != CKF_INOTIFY || fdrecs[i].path[0] == 0) continue;
        int duplicate = 0;
        for (int j = 0; j < i; j++)
            if (fdrecs[j].kind == CKF_INOTIFY && fdrecs[j].ofd_id == fdrecs[i].ofd_id) duplicate = 1;
        if (duplicate) continue;
        size_t bytes = 0;
        if (hl_linux_inotify_export(g_linux_box, (hl_linux_fd)fdrecs[i].gfd, NULL, 0, &bytes) != HL_STATUS_OK)
            CKPT_DUMP_FAIL("size the inotify export");
        void *image = malloc(bytes);
        if (image == NULL) CKPT_DUMP_FAIL("allocate the inotify export");
        size_t actual = 0;
        if (hl_linux_inotify_export(g_linux_box, (hl_linux_fd)fdrecs[i].gfd, image, bytes, &actual) != HL_STATUS_OK ||
            actual != bytes) {
            free(image);
            CKPT_DUMP_FAIL("read the inotify export");
        }
        int stored = ckpt_sink_put(sink, group, fdrecs[i].path, 0, image, bytes);
        free(image);
        if (stored != 0) CKPT_DUMP_FAIL("store the inotify export");
    }

    if (ckpt_dump_epoll(sink, group, fdrecs, nfd) != 0) CKPT_DUMP_FAIL("dump the epoll set");
    if (ckpt_dump_inotify(sink, group) != 0) CKPT_DUMP_FAIL("dump the inotify set");
    // The child exit statuses this capture's own reap destroyed (coordinator only; empty everywhere else).
    // A failure here refuses the dump rather than publishing a group whose parent will wait forever.
    if (g_ckpt_reaped_count > 0) {
        size_t records = g_ckpt_reaped_count * sizeof *g_ckpt_reaped;
        size_t total = sizeof(struct ckpt_reaped_header) + records;
        struct ckpt_reaped_header *image = malloc(total);
        if (image == NULL) CKPT_DUMP_FAIL("allocate the reaped-child image");
        image->magic = CKPT_REAPED_MAGIC;
        image->count = (uint64_t)g_ckpt_reaped_count;
        memcpy(image + 1, g_ckpt_reaped, records);
        int stored = ckpt_sink_put(sink, group, "reaped", 0, image, total);
        free(image);
        if (stored != 0) CKPT_DUMP_FAIL("store the child exit statuses the capture consumed");
    }

    if (ckpt_dump_signal_state(sink, group) != 0) CKPT_DUMP_FAIL("dump the signal state");
    if (ckpt_dump_filesystem_state(sink, group) != 0) CKPT_DUMP_FAIL("dump the filesystem state");

    // meta written LAST within the group (it carries the section counts).
    if (ckpt_sink_put(sink, group, "meta", 0, &m, sizeof m) != 0) CKPT_DUMP_FAIL("store the group meta");
    ok = 1;

done:
    if (fp) ckpt_sink_abort(sink, &fp);
    if (ff) ckpt_sink_abort(sink, &ff);
    free(fdrecs);
    if (!ok) {
        fprintf(stderr, "[ckpt] refuse: %s could not %s\n", group, failed_step ? failed_step : "complete its dump");
        fprintf(stderr, "[ckpt] %s: ABORT -- nothing from this process is published\n", group);
        g_ckpt_member_refusal = failed_step ? failed_step : "complete its dump";
        return -1;
    }
    fprintf(stderr, "[ckpt] %s: commit\n", group);
    if (ckpt_sink_group_commit(sink, group) != 0) {
        g_ckpt_member_refusal = "commit its image group";
        return -1;
    }
    return 0;
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
static int ckpt_live_process_peers(hl_host_process_peer *peers, size_t capacity, size_t *count) {
    if (!hl_linux_pidmap_is_active(&g_pidmap)) return hl_host_process_peers(peers, capacity, count);

    size_t mapped_capacity = 0;
    if (hl_linux_pidmap_snapshot_checked(&g_pidmap, NULL, 0, &mapped_capacity) != 0) return 0;
    hl_linux_pidmap_entry *mapped = malloc((mapped_capacity ? mapped_capacity : 1) * sizeof *mapped);
    if (mapped == NULL) return 0;
    size_t mapped_count = 0;
    if (hl_linux_pidmap_snapshot_checked(&g_pidmap, mapped, mapped_capacity, &mapped_count) != 0) {
        free(mapped);
        return 0;
    }
    if (mapped_count > mapped_capacity) {
        free(mapped);
        *count = mapped_count;
        return 1;
    }
    size_t total = 0;
    for (size_t index = 0; index < mapped_count; ++index) {
        int host = mapped[index].host;
        if (host <= 0 || host == (int)getpid()) continue;
        if (kill(host, 0) != 0 && errno == ESRCH) continue;
        if (total < capacity) peers[total].identity = host;
        ++total;
    }
    free(mapped);
    *count = total;
    return 1;
}

/* A peer that vanished before it could ever contribute, and can therefore be dropped from the rendezvous
   without losing any state.
 *
 * THE PROBLEM. `ckpt_live_process_peers` enumerates the tree at one instant. An ordinary guest tree churns
 * across that instant -- a shell's `sleep .05`, a `make` job, any fork that lives tens of milliseconds --
 * so a process can be enumerated, interrupted, and then simply exit on its own before it ever reaches a
 * checkpoint safepoint. The rendezvous loop below waits for that peer's group to be committed, it never
 * will be, and a capture of a perfectly healthy tree burns the whole ~5s budget and then refuses. That is
 * one of the observed intermittent close failures, and it is a false refusal: nothing was lost.
 *
 * WHY LIVENESS ALONE IS THE WRONG DISCRIMINATOR, and why this was deliberately not fixed as a one-liner.
 * "The peer is gone" is also true of a member that registered, published half its objects, and was then
 * killed mid-dump. Dropping THAT peer would publish a manifest whose process count is short one member
 * whose state the user expects back -- a silently incomplete checkpoint, which is strictly worse than a
 * failed close. Liveness cannot tell the two apart.
 *
 * THE DISCRIMINATOR IS "GONE AND NEVER REGISTERED FOR THIS GENERATION", and it is exact rather than
 * heuristic, because it is the complement of the broker's own publication gate: `publishes_capture_bytes`
 * (hl-engine checkpoint/broker.rs) refuses OBJECT_BEGIN/WRITE/FINISH, GROUP_BEGIN/COMMIT, CLAIM and COMMIT
 * from any connection that has not been admitted by REGISTER_READY for the running generation. So a
 * process that never registered has, by construction, zero bytes in this image and zero half-written
 * groups: there is nothing of it to lose. A process that DID register is not exempt here under any
 * circumstance, and the rendezvous still refuses the capture on its behalf.
 *
 * THE ORDER IS LOAD-BEARING AND FAIL-CLOSED. Liveness is tested FIRST and registration second. Once the
 * process is dead its registration record can no longer change, and REGISTER_READY is a synchronous round
 * trip taken while the process is stopped with its thread registry held -- so a dead peer that registered
 * has already been recorded, and a dead peer that reads as unregistered never was. Asking the broker first
 * would leave a window in which a live peer registers after answering 0. Every answer other than a
 * definite "no record" -- a broken channel, a poisoned ledger, a generation out of scope -- counts as
 * registered and does not exempt anyone. */
static int ckpt_peer_never_contributed(long long identity) {
    if (identity <= 0 || identity > INT_MAX) return 0;
    if (kill((pid_t)identity, 0) == 0 || errno != ESRCH) return 0; /* still reachable: it can still commit */
    return ckpt_stream_participant_registered(identity) == 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
// ------------------------------------------------- rendezvous exemption: behavioral fixture
//
// Drives the REAL ckpt_peer_never_contributed -- including the real PARTICIPANT_REGISTERED round trip over
// a real checkpoint channel -- against a REAL host process, once for each of the four answers the
// coordinator can get. The fix is only correct if BOTH halves hold, so both are scenarios here:
//
//   0  the peer is ALIVE and the broker would say "never registered"  -> NOT exempt
//      Liveness is checked first and alone is never the discriminator; a peer that has not registered yet
//      is a peer that still can.
//   1  the peer is GONE and the broker says "never registered"        -> EXEMPT
//      This is the transient `sleep .05` fork child: it published nothing, so nothing is lost.
//   2  the peer is GONE and the broker says "registered"              -> NOT exempt
//      This is a member killed after it published objects and before it committed its group. Its state IS
//      lost, and the capture must still refuse rather than quietly publish a manifest without it.
//   3  the peer is GONE and the broker cannot be reached              -> NOT exempt
//      An unknown answer is not consent. No exemption is ever granted on a guess.
//
// The responder refuses any request that is not PARTICIPANT_REGISTERED naming exactly the peer under test,
// so scenarios 1 and 2 cannot pass by never asking.
static void ckpt_exemption_responder(hl_activation_descriptor broker, long long expected, uint64_t answer) {
    uint64_t announced = 0;
    hl_activation_descriptor channel = hl_ckpt_broker_accept(broker, 500, &announced);
    if (channel == HL_ACTIVATION_DESCRIPTOR_NONE) _exit(0);
    hl_ckpt_request request;
    hl_ckpt_reply reply;
    uint64_t named = 0;
    memset(&reply, 0, sizeof reply);
    reply.magic = HL_CKPT_STREAM_MAGIC_REPLY;
    reply.abi = HL_CKPT_STREAM_ABI;
    reply.status = HL_CKPT_STATUS_ERROR;
    if (read((int)channel, &request, sizeof request) == (ssize_t)sizeof request &&
        request.magic == HL_CKPT_STREAM_MAGIC_REQUEST && request.abi == HL_CKPT_STREAM_ABI &&
        request.op == HL_CKPT_OP_PARTICIPANT_REGISTERED && request.name_size == 0 &&
        request.length == sizeof named && read((int)channel, &named, sizeof named) == (ssize_t)sizeof named &&
        named == (uint64_t)expected) {
        reply.status = HL_CKPT_STATUS_OK;
        reply.value = answer;
    }
    (void)write((int)channel, &reply, sizeof reply);
    _exit(0);
}

// A process that has already exited AND been reaped, so kill(pid, 0) is ESRCH rather than a zombie's 0.
static int ckpt_exemption_departed_peer(void) {
    pid_t gone = hl_host_process_clone_current();
    if (gone < 0) return -1;
    if (gone == 0) _exit(0);
    int status = 0;
    while (waitpid(gone, &status, 0) < 0 && errno == EINTR) {}
    return (int)gone;
}

static int ckpt_exemption_living_peer(void) {
    pid_t alive = hl_host_process_clone_current();
    if (alive < 0) return -1;
    if (alive == 0) { // async-signal-safe only: forked out of a multi-threaded caller
        struct timespec span = {30, 0};
        (void)nanosleep(&span, NULL);
        _exit(0);
    }
    return (int)alive;
}

HL_API int HL_TARGET_LOCAL(checkpoint_rendezvous_test)(uint32_t scenario) {
    if (scenario > 3) return -22;
    int saved_broker = hl_ckpt_channel_broker();
    hl_activation_descriptor parent = HL_ACTIVATION_DESCRIPTOR_NONE;
    hl_activation_descriptor child = HL_ACTIVATION_DESCRIPTOR_NONE;
    pid_t responder = -1;
    int verdict = -1;
    int peer = scenario == 0 ? ckpt_exemption_living_peer() : ckpt_exemption_departed_peer();
    if (peer <= 0) return -1;
    hl_ckpt_channel_forget_for_test();
    if (scenario == 3) {
        hl_ckpt_channel_publish(-1); // no broker: the round trip fails and the answer is unknown
    } else {
        if (hl_ckpt_broker_pair(&parent, &child) != 0) goto done;
        responder = hl_host_process_clone_current();
        if (responder < 0) goto done;
        if (responder == 0) ckpt_exemption_responder(parent, peer, scenario == 2 ? 1 : 0);
        hl_ckpt_channel_publish((int)child);
    }
    verdict = ckpt_peer_never_contributed(peer) == (scenario == 1) ? 0 : -1;
done:
    {
        // Reclaim the channel the predicate's round trip created, if it made one at all. Read with the
        // non-minting accessor deliberately: `hl_ckpt_channel_acquire` would OPEN a connection on the
        // scenarios where the code under test correctly never sent a request.
        int channel = hl_ckpt_channel_current_for_test();
        hl_ckpt_channel_forget_for_test();
        if (channel >= 0) (void)close(channel);
    }
    hl_ckpt_channel_publish(saved_broker);
    if (responder > 0) {
        int status = 0;
        while (waitpid(responder, &status, 0) < 0 && errno == EINTR) {}
    }
    if (parent != HL_ACTIVATION_DESCRIPTOR_NONE) (void)close((int)parent);
    if (child != HL_ACTIVATION_DESCRIPTOR_NONE) (void)close((int)child);
    if (scenario == 0) {
        (void)kill((pid_t)peer, SIGKILL);
        int status = 0;
        while (waitpid((pid_t)peer, &status, 0) < 0 && errno == EINTR) {}
    }
    return verdict;
}
#endif

static void ckpt_coordinator_refuse(const struct ckpt_phase_ledger *phases, uint32_t cause, const char *reason);

/* Record one status the coordinator's reap just took off a child, so restore can hand it back.
 *
 * REFUSING IS THE FALLBACK, NOT THE DESIGN. The status is already destroyed by the time this runs -- waitpid
 * released the zombie -- so a case this cannot record is a case in which a member's state is unsaved, and the
 * only honest answer left is to refuse the capture by name. That is the whole reason the naming is
 * fail-closed on a restored tree: outside a restore a guest pid IS its host pid and there is nothing to
 * resolve, but under an active pid map a host pid with no guest identity is a child this image could not
 * describe even if it carried it. */
static void ckpt_record_reaped_child(const struct ckpt_phase_ledger *phases, pid_t host, int status) {
    int gpid = 0;
    /* Test-only, and it models the ONE way the naming can genuinely fail rather than approximating it: a
     * reaped child with no identity in an active guest pid namespace. No fixture can produce that on
     * purpose -- clone.c publishes a child's guest identity before the child can run, let alone exit -- so
     * without the hook the refusal below is a branch no test can reach, which is exactly the shape that
     * hides a capture reporting success over unsaved state. */
    int unnameable = hl_option_get("HL_CKPT_TEST_REAPED_UNNAMEABLE") != NULL;
    if (unnameable || hl_linux_pidmap_is_active(&g_pidmap)) {
        if (unnameable || hl_linux_pidmap_guest_checked(&g_pidmap, (int32_t)host, &gpid) != 0 || gpid <= 0) {
            char reason[HL_CKPT_STREAM_NAME_MAX];
            snprintf(reason, sizeof reason,
                     "the capture reaped child %lld, destroying the exit status its guest parent had not "
                     "collected, and that child has no identity in the guest pid namespace -- the "
                     "status cannot be carried in the image, so this capture would resume a parent onto a "
                     "wait that can never complete",
                     (long long)host);
            ckpt_coordinator_refuse(phases, CKPT_REFUSAL_PEER_QUIESCENCE, reason);
        }
    } else {
        gpid = (int)host; /* identity namespace outside a restore */
    }
    if (g_ckpt_reaped_count >= CKPT_REAPED_MAX) {
        char reason[HL_CKPT_STREAM_NAME_MAX];
        snprintf(reason, sizeof reason,
                 "the capture reaped more than %d unwaited child exit statuses; the image cannot carry them "
                 "all, and publishing it would resume parents onto waits that can never complete",
                 CKPT_REAPED_MAX);
        ckpt_coordinator_refuse(phases, CKPT_REFUSAL_PEER_QUIESCENCE, reason);
    }
    struct ckpt_reaped_child *grown = realloc(g_ckpt_reaped, (g_ckpt_reaped_count + 1) * sizeof *grown);
    if (grown == NULL)
        ckpt_coordinator_refuse(phases, CKPT_REFUSAL_RESOURCES,
                                "cannot record a child exit status the capture destroyed");
    g_ckpt_reaped = grown;
    g_ckpt_reaped[g_ckpt_reaped_count].gpid = gpid;
    g_ckpt_reaped[g_ckpt_reaped_count].status = status;
    g_ckpt_reaped_count++;
}

/* WHY THE REAP IS STILL HERE.
 *
 * kill(pid, 0) succeeds on a zombie, so an exited transient child reads as reachable for as long as it stays
 * unreaped: without this the exemption below would never fire for it and the rendezvous would wait out its
 * stall window on a corpse before refusing a healthy close. Not reaping at all also leaves the coordinator
 * accumulating zombies for the whole capture. What changes is only that the status is written down before it
 * is released. */
static void ckpt_reap_and_record(const struct ckpt_phase_ledger *phases) {
    for (;;) {
        int status = 0;
        pid_t child = waitpid(-1, &status, WNOHANG);
        if (child <= 0) break;
        ckpt_record_reaped_child(phases, child, status);
    }
}

/* A reaped child that COMMITTED a group is a captured member, not a lost status: it is restored as a live
 * process and its parent's wait for it is answered by that process exiting again. Only the corpses nobody
 * captured need synthesizing. Run once, after quiescence, when the set of committed groups is final. */
static void ckpt_reaped_drop_captured_members(struct ckpt_sink *sink) {
    size_t kept = 0;
    for (size_t index = 0; index < g_ckpt_reaped_count; ++index) {
        char group[64];
        snprintf(group, sizeof group, "proc.%d", g_ckpt_reaped[index].gpid);
        if (ckpt_sink_group_present(sink, group) == 1) continue;
        g_ckpt_reaped[kept++] = g_ckpt_reaped[index];
    }
    g_ckpt_reaped_count = kept;
}

/* Consecutive rescans that must find nothing new, with every known peer finished, before the tree counts
 * as quiescent. Two rather than one, because the first pass is quiet before anything has been adopted and
 * because a fork is only ever visible to a scan taken after it. */
#define CKPT_ENUMERATION_QUIET_PASSES 2

/* THE RENDEZVOUS WAITS ON PROGRESS, NOT ON A CLOCK.
 *
 * This loop used to run at most 500 passes of 10 ms and refuse whatever had not committed by then: a fixed
 * ~5 s WALL budget for the whole tree to reach its safepoints. "Have I waited long enough?" is the wrong
 * question, and the right one -- "is anyone still moving?" -- has a different answer under load. Measured:
 * the same workspace close that passes in 7.5 s at load 8 refused at load ~16 with
 * `participant ... never committed proc.6 (it did not reach a checkpoint safepoint)` at +4906 ms, because
 * a member being descheduled is indistinguishable from a member being wedged when the only thing you look
 * at is a clock. A developer with a busy machine is exactly the person who hits that, and the refusal is a
 * workspace that will not close. Raising the constant only moves the load at which it breaks.
 *
 * So the loop is unbounded in wall time while the tree is PROGRESSING, and gives up only when it provably
 * is not. Progress in a pass is any of: a new member adopted, a member's group committed or exempted, or
 * an outstanding member's consumed host CPU time (user+system) having advanced since the previous pass.
 * The last one is the fine-grained signal, and it is exactly the one a starved-but-running process keeps
 * producing: CPU time is monotonic in work actually performed, so it advances however long the box makes
 * the member wait for a core, and it does NOT advance for a member that will never reach a safepoint --
 * one blocked forever, one that never took the kick. That keeps termination: this is a stall detector, not
 * a deadline, and a member that never moves is still refused, by name, after a bounded window of proven
 * inactivity. It is deliberately generous, because the cost of firing early is a false refusal and the
 * cost of firing late is a slower failure. */
#define CKPT_RENDEZVOUS_STALL_PASSES 500

/* The other way this loop can fail to end, and the one waiting cannot fix: a tree that never stops
 * forking. Quiescence needs a pass that adopts nobody, so a guest process that is still running and still
 * forking keeps the rendezvous honestly busy for as long as it goes on -- every child is a real member and
 * kicking it is the right thing to do. That is a live rendezvous, not a stalled one, so the stall detector
 * above will never end it; it ends when the forking source freezes, which is what its own kick achieves.
 * A source that never freezes therefore has to be refused instead, by name, after this many CONSECUTIVE
 * passes each of which adopted somebody new. Nothing healthy forks continuously for that long after the
 * freeze: an ordinary tree's stragglers are adopted within a pass or two of the trigger. */
#define CKPT_RENDEZVOUS_CHURN_PASSES 500

static void ckpt_coordinate_and_exit(struct cpu *c) {
    const struct ckpt_phase_ledger phases = {
        .enabled = hl_option_get("HL_CHECKPOINT_PHASE_LEDGER") != NULL,
        .isa = ckpt_phase_isa_name(G_CKPT_ARCH),
        .generation = ckpt_request_generation(),
        .clock_failure = hl_option_get("HL_CHECKPOINT_PHASE_CLOCK_FAIL") != NULL,
        .descriptor = ckpt_phase_descriptor(),
    };
    uint64_t phase = ckpt_phase_begin(&phases);
    struct ckpt_sink *sink = ckpt_sink_current();

    /* THE MEMBER SET IS NOT A SNAPSHOT. `ckpt_live_process_peers` reads the tree at one instant, and an
     * ordinary guest tree forks and exits across that instant -- a shell's `while :; do ...; sleep .05;
     * done`, a `make` job, any transient child. A process forked immediately AFTER the scan is a real
     * guest process with real state: it reaches its own safepoint, observes the trigger generation, proves
     * membership and commits its group. Counting it against a set fixed before it existed is what produced
     * `process-count mismatch: expected exactly N committed groups, captured N+1` on a perfectly healthy
     * close. The mirror of the same hole is worse: a process the scan MISSED that never commits is a
     * member whose state is unsaved, and a count derived from the scan reports `checkpoint OK` anyway.
     *
     * So enumeration is demoted to what it can actually do -- find processes that need KICKING to a
     * safepoint -- and it is repeated. Every rendezvous pass rescans and adopts whatever appeared since
     * the last one, because a process that has already frozen cannot fork: each pass that finds nothing
     * new is a pass in which the unfrozen set was empty. The set the MANIFEST is checked against comes
     * from the broker instead (see the seal below), which is the only party that observes membership
     * rather than inferring it. */
    size_t scan_capacity = 512;
    hl_host_process_peer *scan = malloc(scan_capacity * sizeof *scan);
    size_t observed = 0;
    hl_host_process_peer *foll = NULL;
    unsigned char *completed = NULL;
    int nfoll = 0;
    int known_capacity = 0;
    if (scan == NULL)
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_RESOURCES, "cannot allocate the peer enumeration buffer");
    /* Test-only, and they model the two ways enumeration can be wrong about a real process rather than
     * approximating them. HIDDEN_FROM_ENUMERATION withholds one live member from the FIRST scan only,
     * which is indistinguishable downstream from a process that came into existence one instruction after
     * that scan returned. FORGOTTEN_AFTER_KICK lets one member be kicked -- so it really does prove
     * membership to the broker -- and then drops it from the known set and from every later scan, which is
     * the reported blind spot exactly: a peer that registered, exited, and was then enumerated as 0 peers.
     * Only the broker knows that process was ever a member. */
    int hide_first_scan = hl_option_get("HL_CKPT_TEST_PEER_HIDDEN_FROM_ENUMERATION") != NULL;
    int forget_after_kick = hl_option_get("HL_CKPT_TEST_PEER_FORGOTTEN_AFTER_KICK") != NULL;
    long long hidden = 0;
    int ndone = 0;
    int nexempt = 0;
    int quiet = 0;
    int stalled = 0;
    int churning = 0;
    /* Per known peer, the host CPU time it had consumed when it was last seen to advance. Parallel to
     * `foll`/`completed` and grown with them. */
    uint64_t *consumed = NULL;
    for (unsigned long long t = 0;; t++) {
        int settled = 0;
        // Reap BEFORE the liveness test below, not after: an unreaped child of ours is a zombie, and
        // kill(pid, 0) succeeds on a zombie, so an exited transient child would read as still reachable
        // for as long as it stayed unreaped and would never qualify for the exemption. Every status it
        // takes is RECORDED first -- that reap used to destroy the pending child status a guest parent was
        // blocked in wait4 for, and the restored parent then waited forever for a pid that never existed
        // again. See ckpt_record_reaped_child.
        ckpt_reap_and_record(&phases);
        // Rescan and adopt. A peer discovered here is kicked exactly as one found by the first scan is;
        // nothing else distinguishes them, because nothing else should.
        int discovered = 0;
        for (;;) {
            if (!ckpt_live_process_peers(scan, scan_capacity, &observed))
                ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PEER_ENUMERATION, "cannot enumerate the live peer set");
            if (observed <= scan_capacity) break;
            if (observed > (size_t)INT_MAX || observed > SIZE_MAX / sizeof *scan)
                ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PEER_ENUMERATION,
                                        "the live peer set is larger than the coordinator can address");
            hl_host_process_peer *expanded = realloc(scan, observed * sizeof *scan);
            if (expanded == NULL)
                ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_RESOURCES, "cannot grow the peer enumeration buffer");
            scan = expanded;
            scan_capacity = observed;
        }
        for (size_t index = 0; index < observed; index++) {
            if (!ckpt_capture_member(scan[index].identity, getpid())) continue;
            int already = 0;
            for (int i = 0; i < nfoll; i++)
                if (foll[i].identity == scan[index].identity) already = 1;
            if (already) continue;
            if (scan[index].identity == hidden) continue; /* forgotten: this scan never reports it again */
            if (hide_first_scan && t == 0) {
                hide_first_scan = 0;
                fprintf(stderr, "[ckpt] participant %lld withheld from the first enumeration (test hook)\n",
                        (long long)scan[index].identity);
                continue;
            }
            if (nfoll == known_capacity) {
                int grown = known_capacity == 0 ? 16 : known_capacity * 2;
                hl_host_process_peer *peers = realloc(foll, (size_t)grown * sizeof *foll);
                unsigned char *ledger = peers != NULL ? realloc(completed, (size_t)grown) : NULL;
                uint64_t *progress = ledger != NULL ? realloc(consumed, (size_t)grown * sizeof *consumed) : NULL;
                if (peers != NULL) foll = peers;
                if (ledger != NULL) completed = ledger;
                if (progress == NULL)
                    ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_RESOURCES, "cannot grow the rendezvous ledger");
                consumed = progress;
                known_capacity = grown;
            }
            foll[nfoll] = scan[index];
            completed[nfoll] = 0;
            consumed[nfoll] = 0;
            nfoll++;
            discovered = 1;
            // Freeze + dump this peer: the shared trigger generation is already advanced (the requester
            // bumped it), so KICK it with the guest-proof THREAD_INT_SIG to bounce it out of a blocked
            // syscall / chained in-cache loop to its safepoint, where ckpt_poll sees the new generation and
            // dumps proc.<gpid> + _exit()s.
            int kicked = hl_host_process_interrupt(scan[index]);
            fprintf(stderr, "[ckpt] participant %lld %s\n", (long long)scan[index].identity,
                    kicked ? "interrupted" : "NOT interrupted (it cannot reach a safepoint)");
            if (forget_after_kick) { /* kicked, so it can prove membership -- and then never enumerated again */
                forget_after_kick = 0;
                hidden = scan[index].identity;
                nfoll--;
                discovered = 0;
                fprintf(stderr, "[ckpt] participant %lld dropped from the enumeration after its kick (test hook)\n",
                        (long long)hidden);
            }
        }
        for (int i = 0; i < nfoll; i++) {
            if (completed[i]) continue;
            char pd[64];
            snprintf(pd, sizeof pd, "proc.%d", ckpt_peer_gpid(foll[i].identity));
            // Rendezvous through the sink, not through the store: "that peer finished" is defined as
            // "its group was committed", which is exactly what group_commit means for every implementation.
            if (ckpt_sink_group_present(sink, pd) == 1) {
                completed[i] = 1;
                ndone++;
                settled = 1;
                continue;
            }
            // Committed is checked first, so a peer that committed and then exited is counted as the
            // member it is rather than exempted as a transient.
            if (ckpt_peer_never_contributed(foll[i].identity)) {
                fprintf(stderr,
                        "[ckpt] participant %lld exited before joining the capture (no REGISTER_READY for "
                        "generation %u, so it published nothing); not waiting for proc.%d\n",
                        (long long)foll[i].identity, phases.generation, ckpt_peer_gpid(foll[i].identity));
                completed[i] = 1;
                ndone++;
                nexempt++;
                settled = 1;
                continue;
            }
            /* Still outstanding: has it moved? A member that is merely starved keeps burning CPU, however
             * slowly; one that is wedged, or that never took the kick, burns none. Read it fresh every
             * pass -- a member that cannot be read at all (it has just died) is not progress, and it is
             * either exempted above on the next pass or refused by name below. */
            {
                hl_host_process_info info;
                uint64_t spent;
                if (hl_host_process_read(foll[i].identity, &info)) {
                    spent = info.user_time_ns + info.system_time_ns;
                    if (spent > consumed[i]) {
                        consumed[i] = spent;
                        settled = 1;
                    }
                }
            }
        }
        /* Quiescent means everything known has finished AND a rescan found nothing new -- and it takes
         * CKPT_ENUMERATION_QUIET_PASSES consecutive such passes, never one. One is not enough because the
         * very first pass is trivially quiet before anything has been adopted, and because a process
         * forked microseconds before the scan that missed it is discovered by the NEXT scan, not that one.
         * Settling on a single quiet pass is exactly the hole this loop exists to close: it would seal the
         * membership while an unfrozen guest process was still on its way to a safepoint. */
        quiet = ndone == nfoll && !discovered ? quiet + 1 : 0;
        if (quiet >= CKPT_ENUMERATION_QUIET_PASSES) break;
        /* `discovered` and `settled` cover the coarse milestones; `settled` also carries the CPU-time
         * advance recorded above. A pass in which none of them moved is a pass in which the whole
         * outstanding set stood still. */
        stalled = discovered || settled ? 0 : stalled + 1;
        if (stalled >= CKPT_RENDEZVOUS_STALL_PASSES) break;
        churning = discovered ? churning + 1 : 0;
        if (churning >= CKPT_RENDEZVOUS_CHURN_PASSES) break;
        usleep(10000);
    }
    free(scan);
    free(consumed);
    fprintf(stderr, "[ckpt] coordinator pid=%d found %d peer(s), %d exempt\n", getpid(), nfoll, nexempt);
    if (churning >= CKPT_RENDEZVOUS_CHURN_PASSES) {
        char reason[HL_CKPT_STREAM_NAME_MAX];
        snprintf(reason, sizeof reason,
                 "the guest tree never stopped forking: a new process was adopted in every one of the last "
                 "%d ms, so no enumeration ever found the tree quiescent and the set of members cannot be "
                 "closed; %d peer(s) were enumerated and %d exempted",
                 CKPT_RENDEZVOUS_CHURN_PASSES * 10, nfoll, nexempt);
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PEER_QUIESCENCE, reason);
    }
    if (ndone != nfoll) {
        // Name every participant still outstanding at the rendezvous deadline: "the group never committed"
        // is otherwise indistinguishable from "nothing was ever asked to commit". The host is told about
        // the first one by name, because a reason it cannot act on is barely better than no reason.
        char reason[HL_CKPT_STREAM_NAME_MAX];
        int named = 0;
        for (int i = 0; i < nfoll; i++)
            if (!completed[i]) {
                fprintf(stderr,
                        "[ckpt] participant %lld never committed proc.%d and stopped making progress "
                        "towards a checkpoint safepoint (no host CPU time consumed for %d ms); refusing "
                        "incomplete manifest\n",
                        (long long)foll[i].identity, ckpt_peer_gpid(foll[i].identity),
                        CKPT_RENDEZVOUS_STALL_PASSES * 10);
                if (!named) {
                    named = 1;
                    snprintf(reason, sizeof reason,
                             "%d of %d participants never committed their group; the first is process %lld "
                             "(proc.%d), which stopped making progress towards a checkpoint safepoint -- it "
                             "consumed no host CPU time for %d ms -- or had its dump refused",
                             nfoll - ndone, nfoll, (long long)foll[i].identity, ckpt_peer_gpid(foll[i].identity),
                             CKPT_RENDEZVOUS_STALL_PASSES * 10);
                }
            }
        if (!named) snprintf(reason, sizeof reason, "a participant never committed its group");
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PEER_QUIESCENCE, reason);
    }
    ckpt_phase_finish(&phases, "peer_quiescence", phase, 0);

    // Dump ourselves (the init) last. The statuses the freeze consumed go into THIS group: waitpid(-1)
    // reaps only this process's own children, and an orphan reparents to this process, so the coordinator is
    // the parent of every corpse it collected -- by construction, on both ISAs.
    ckpt_reaped_drop_captured_members(sink);
    phase = ckpt_phase_begin(&phases);
    if (ckpt_dump_self(c, "proc.1", 0) != 0)
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_SELF_DUMP,
                                "the container init's own dump failed; the checkpoint would be incomplete");
    ckpt_phase_finish(&phases, "serialization", phase, 0);

    // Publish the MANIFEST last: its presence == a complete, restorable checkpoint.
    //
    // SEAL FIRST, THEN COUNT, AND COUNT AGAINST THE SEAL. The expected process set is fixed at exactly one
    // instant, here, and it comes from the broker's REGISTER_READY ledger rather than from anything this
    // process enumerated. That matters in both directions, and both have been observed in production:
    //
    //   - a process forked after the coordinator's scan is a genuine member that commits a genuine group.
    //     Measured against an enumeration it postdates it reads as a surplus group and refuses a healthy
    //     close; measured against the ledger it registered in, it is simply one of the members.
    //   - a process the coordinator never enumerated, or enumerated and lost, that DID register is a
    //     member whose state is unsaved. An enumeration-derived count cannot see it at all -- that is the
    //     shape that published `checkpoint OK: 1 process(es)` with a registered member missing. A sealed
    //     count is higher than the committed groups and refuses.
    //
    // The seal runs after this coordinator's own dump, so every member including the init is in the ledger,
    // and after the rendezvous, so no unfrozen guest process remains that could still fork or register. A
    // registration arriving after it is refused by the broker rather than admitted, and the late member
    // refuses its own dump instead of publishing into an image that is already being counted.
    phase = ckpt_phase_begin(&phases);
    uint64_t sealed = 0;
    if (ckpt_stream_seal_membership(&sealed) != 0)
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PROCESS_COUNT,
                                "the broker could not seal this capture's membership, so the set of processes the "
                                "manifest must contain is unknown");
    int nproc = ckpt_sink_group_count(sink, "proc.");
    ckpt_phase_finish(&phases, "settlement", phase, 0);
    if (nproc < 0 || sealed > (uint64_t)INT_MAX || nproc != (int)sealed) {
        char reason[HL_CKPT_STREAM_NAME_MAX];
        snprintf(reason, sizeof reason,
                 "process-count mismatch: %llu process(es) proved membership of this capture and exactly that "
                 "many groups must be committed, but %d were; %d peer(s) were enumerated and %d exempted",
                 (unsigned long long)sealed, nproc, nfoll, nexempt);
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PROCESS_COUNT, reason);
    }
    struct ckpt_manifest man;
    phase = ckpt_phase_begin(&phases);
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
        if (fgh <= 0)
            man.fg_pgid_gpid = 0;
        else if (hl_linux_pidmap_guest_checked(&g_pgidmap, (int32_t)fgh, &man.fg_pgid_gpid) != 0) {
            ckpt_ctty_close(tf);
            ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_FOREGROUND_GROUP,
                                    "the terminal's foreground process group is outside the restored namespace");
        } else if (!hl_linux_pidmap_is_active(&g_pgidmap) && g_init_hostpid && fgh == g_init_hostpid)
            man.fg_pgid_gpid = 1;
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
        char reason[HL_CKPT_STREAM_NAME_MAX];
        snprintf(reason, sizeof reason, "cannot hash the checkpoint image: %s", strerror(errno));
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_DIGEST, reason);
    }
    // Explicit completion: the only signal that the image is complete.
    if (ckpt_sink_commit(sink, &man, sizeof man) != 0) {
        char reason[HL_CKPT_STREAM_NAME_MAX];
        snprintf(reason, sizeof reason, "cannot publish the checkpoint manifest: %s", strerror(errno));
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_MANIFEST, reason);
    }
    ckpt_phase_finish(&phases, "manifest_publication", phase, 0);
    fprintf(stderr, "[ckpt] checkpoint OK: %d process(es)\n", nproc);
    int st;
    phase = ckpt_phase_begin(&phases);
    // Final reap, deliberately NOT recording: the manifest is already published, so a status destroyed here
    // could not be carried anyway. Nothing unrecorded can reach it -- the rendezvous ended quiescent, every
    // member is parked inside its own freeze and cannot run guest code, and therefore cannot fork or exit,
    // until the host releases it after this point. This collects the corpses of members that already exited
    // during the capture, whose groups are in the image.
    while (waitpid(-1, &st, WNOHANG) > 0) {} // final reap
    ckpt_phase_finish(&phases, "native_reap", phase, 0);
    hl_engine_child_result_publish(0, HL_STATUS_OK, 0);
    ckpt_phase_exit(&phases, 0);
}

// ================================= RESTORE =================================
