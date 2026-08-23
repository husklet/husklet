#include "../../process.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <sys/event.h>
#include <sys/proc_info.h>
#include <libproc.h>
#include <bsm/libbsm.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

int hl_host_process_open(pid_t pid) {
    int descriptor = kqueue();
    if (descriptor < 0) return -1;
    struct kevent event;
    EV_SET(&event, (uintptr_t)pid, EVFILT_PROC, EV_ADD, NOTE_EXIT, 0, NULL);
    if (kevent(descriptor, &event, 1, NULL, 0, NULL) != 0) {
        int error = errno;
        close(descriptor);
        errno = error;
        return -1;
    }
    (void)fcntl(descriptor, F_SETFD, FD_CLOEXEC);
    return descriptor;
}

typedef struct hl_host_macos_process_identity {
    uint64_t birth;
    uint64_t unique;
    uint64_t generation;
} hl_host_macos_process_identity;

/* XNU declares flavor 17's layout an API, but current macOS SDKs omit the
 * private header that gives that public kernel contract a spelling. Keep the
 * exact fixed-width wire layout here and fail closed if the running kernel
 * does not implement it. */
typedef struct hl_host_macos_unique_identity {
    uint8_t executable_uuid[16];
    uint64_t unique;
    uint64_t parent_unique;
    int32_t generation;
    int32_t original_parent_generation;
    uint64_t reserved[2];
} hl_host_macos_unique_identity;

enum { HL_MACOS_PROC_PID_UNIQUE_IDENTIFIER_INFO = 17 };

_Static_assert(sizeof(hl_host_macos_unique_identity) == 56, "XNU process identity ABI");

static int hl_host_macos_process_identity_read(pid_t pid, hl_host_macos_process_identity *identity) {
    struct proc_bsdinfo bsd;
    hl_host_macos_unique_identity unique;
    if (identity == NULL || pid <= 0) {
        errno = EINVAL;
        return -1;
    }
    memset(&bsd, 0, sizeof bsd);
    memset(&unique, 0, sizeof unique);
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &bsd, sizeof bsd) != (int)sizeof bsd ||
        proc_pidinfo(pid, HL_MACOS_PROC_PID_UNIQUE_IDENTIFIER_INFO, 0, &unique, sizeof unique) != (int)sizeof unique) {
        if (errno == 0) errno = ESRCH;
        return -1;
    }
    identity->birth =
        (uint64_t)bsd.pbi_start_tvsec * UINT64_C(1000000000) + (uint64_t)bsd.pbi_start_tvusec * UINT64_C(1000);
    identity->unique = unique.unique;
    identity->generation = unique.generation > 0 ? (uint64_t)(uint32_t)unique.generation : 0;
    if (identity->birth == 0 || identity->unique == 0 || identity->generation == 0) {
        errno = ESRCH;
        return -1;
    }
    return 0;
}

int hl_host_process_identity_open(pid_t pid, uint64_t expected_birth, uint64_t expected_generation,
                                  uint64_t *actual_birth, uint64_t *actual_generation) {
    hl_host_macos_process_identity before;
    hl_host_macos_process_identity after;
    struct kevent registration;
    struct kevent observed;
    struct timespec immediate = {0, 0};
    int descriptor = -1;
    int error;
    if (actual_birth == NULL || actual_generation == NULL) {
        errno = EINVAL;
        return -1;
    }
    *actual_birth = 0;
    *actual_generation = 0;
    if (hl_host_macos_process_identity_read(pid, &before) != 0 ||
        (expected_birth != 0 && expected_birth != before.birth) ||
        (expected_generation != 0 && expected_generation != before.generation)) {
        if (errno == 0) errno = ESRCH;
        return -1;
    }
    descriptor = kqueue();
    if (descriptor < 0) return -1;
    EV_SET(&registration, (uintptr_t)pid, EVFILT_PROC, EV_ADD | EV_ENABLE | EV_CLEAR, NOTE_EXIT | NOTE_EXEC, 0, NULL);
    if (kevent(descriptor, &registration, 1, NULL, 0, NULL) != 0 ||
        hl_host_macos_process_identity_read(pid, &after) != 0 || before.birth != after.birth ||
        before.unique != after.unique || before.generation != after.generation ||
        kevent(descriptor, NULL, 0, &observed, 1, &immediate) != 0) {
        error = errno == 0 ? ESRCH : errno;
        (void)close(descriptor);
        errno = error;
        return -1;
    }
    if (fcntl(descriptor, F_SETFD, FD_CLOEXEC) != 0) {
        error = errno;
        (void)close(descriptor);
        errno = error;
        return -1;
    }
    *actual_birth = after.birth;
    *actual_generation = after.generation;
    return descriptor;
}

int hl_host_process_peer_identity_open(int socket_descriptor, uint64_t claimed_pid, uint64_t *actual_pid,
                                       uint64_t *actual_birth, uint64_t *actual_generation) {
    audit_token_t token_before;
    audit_token_t token_after;
    pid_t peer_pid = 0;
    pid_t token_pid;
    int token_generation;
    socklen_t token_size = (socklen_t)sizeof token_before;
    socklen_t pid_size = (socklen_t)sizeof peer_pid;
    int capability;
    if (actual_pid == NULL || actual_birth == NULL || actual_generation == NULL) {
        errno = EINVAL;
        return -1;
    }
    *actual_pid = 0;
    *actual_birth = 0;
    *actual_generation = 0;
    memset(&token_before, 0, sizeof token_before);
    if (socket_descriptor < 0 || claimed_pid == 0 || claimed_pid > INT32_MAX ||
        getsockopt(socket_descriptor, SOL_LOCAL, LOCAL_PEERTOKEN, &token_before, &token_size) != 0 ||
        token_size != (socklen_t)sizeof token_before ||
        getsockopt(socket_descriptor, SOL_LOCAL, LOCAL_PEERPID, &peer_pid, &pid_size) != 0 ||
        pid_size != (socklen_t)sizeof peer_pid)
        return -1;
    token_pid = audit_token_to_pid(token_before);
    token_generation = audit_token_to_pidversion(token_before);
    if (peer_pid <= 0 || token_pid != peer_pid || (uint64_t)peer_pid != claimed_pid || token_generation <= 0) {
        errno = EPERM;
        return -1;
    }
    capability = hl_host_process_identity_open(peer_pid, 0, (uint64_t)(uint32_t)token_generation, actual_birth,
                                               actual_generation);
    if (capability < 0) return -1;
    token_size = (socklen_t)sizeof token_after;
    memset(&token_after, 0, sizeof token_after);
    if (getsockopt(socket_descriptor, SOL_LOCAL, LOCAL_PEERTOKEN, &token_after, &token_size) != 0 ||
        token_size != (socklen_t)sizeof token_after || memcmp(&token_before, &token_after, sizeof token_before) != 0 ||
        *actual_generation != (uint64_t)(uint32_t)token_generation) {
        int error = errno == 0 ? ESRCH : errno;
        (void)close(capability);
        *actual_birth = 0;
        *actual_generation = 0;
        errno = error;
        return -1;
    }
    *actual_pid = (uint64_t)peer_pid;
    return capability;
}
