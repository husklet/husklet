#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "../../process.h"

#include <errno.h>
#include <sys/syscall.h>
#include <unistd.h>

int hl_host_process_open(pid_t pid) {
#ifdef SYS_pidfd_open
    return (int)syscall(SYS_pidfd_open, pid, 0u);
#else
    (void)pid;
    errno = ENOSYS;
    return -1;
#endif
}
static hl_host_result hl_linux_process_spawn_mode(void *context, hl_host_process_entry entry, void *entry_context,
                                                  int prepared) {
    hl_host_linux *host = context;
    hl_host_result result;
    pid_t pid;
    int fork_error;
    int private_prepared = 0;
    if (entry == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!prepared && pthread_mutex_lock(&host->fork_gate) != 0)
        return hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (hl_host_process_fd_private_fork_prepare() != 0) {
        if (!prepared) (void)pthread_mutex_unlock(&host->fork_gate);
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    private_prepared = 1;
    pid = fork();
    fork_error = errno;
    int private_status = private_prepared ? hl_host_process_fd_private_fork_complete(pid == 0) : 0;
    if (prepared) {
        /* The fork child has no watcher threads.  Reset inherited subscription
         * records there instead of applying the parent's completion path: a
         * later close must never pthread_join a thread that vanished at fork. */
        result = pid == 0 ? hl_linux_fork_child(host) : hl_linux_fork_complete(host);
    } else {
        result = pthread_mutex_unlock(&host->fork_gate) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                             : hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    }
    if (pid < 0) {
        errno = fork_error;
        return result.status == HL_STATUS_OK ? hl_linux_errno_result() : result;
    }
    if (private_status != 0 && result.status == HL_STATUS_OK) result = hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    if (result.status != HL_STATUS_OK) {
        if (pid > 0) {
            int status;
            kill(pid, SIGKILL);
            while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        }
        if (pid == 0) _exit(255);
        return result;
    }
    if (pid == 0) _exit(entry(entry_context) & 255);
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_PROCESS, pid, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) {
        int status;
        kill(pid, SIGKILL);
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
    }
    return result;
}

static hl_host_result hl_linux_process_spawn(void *context, hl_host_process_entry entry, void *entry_context) {
    return hl_linux_process_spawn_mode(context, entry, entry_context, 0);
}

static hl_host_result hl_linux_process_spawn_prepared(void *context, hl_host_process_entry entry, void *entry_context) {
    return hl_linux_process_spawn_mode(context, entry, entry_context, 1);
}

static hl_host_result hl_linux_process_wait(void *context, hl_host_handle handle, uint64_t deadline_ns) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pid_t pid;
    pid_t waited;
    int status;
    int options;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    if (entry == NULL || host->destroying) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    entry->process_waiters++;
    while (entry != NULL && entry->process_waiting && !entry->process_reaped) {
        if (deadline_ns == 0 ||
            (deadline_ns != HL_HOST_DEADLINE_INFINITE && hl_linux_monotonic_value() >= deadline_ns)) {
            entry->process_waiters--;
            pthread_cond_broadcast(&host->process_changed);
            pthread_mutex_unlock(&host->lock);
            return hl_linux_result(HL_STATUS_WOULD_BLOCK, 0, 0);
        }
        hl_linux_process_changed_wait(host, deadline_ns);
        entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    }
    if (entry != NULL && entry->process_reaped) {
        hl_host_result result = hl_linux_result(HL_STATUS_OK, entry->process_exit_value, entry->process_exit_kind);
        entry->process_waiters--;
        pthread_cond_broadcast(&host->process_changed);
        pthread_mutex_unlock(&host->lock);
        return result;
    }
    pid = entry != NULL ? entry->descriptor : -1;
    if (entry != NULL) entry->process_waiting = 1;
    pthread_mutex_unlock(&host->lock);
    if (pid < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    options = deadline_ns == HL_HOST_DEADLINE_INFINITE ? 0 : WNOHANG;
    for (;;) {
        do {
            waited = waitpid(pid, &status, options);
        } while (waited < 0 && errno == EINTR);
        if (waited != 0) break;
        if (deadline_ns == 0 || hl_linux_monotonic_value() >= deadline_ns) break;
        hl_linux_sleep_until(deadline_ns);
    }
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    if (entry != NULL) {
        entry->process_waiting = 0;
        entry->process_waiters--;
    }
    if (waited > 0 && entry != NULL) {
        entry->process_reaped = 1;
        entry->process_exit_kind = WIFEXITED(status) ? HL_HOST_PROCESS_EXIT_CODE : HL_HOST_PROCESS_EXIT_SIGNAL;
        entry->process_exit_value = WIFEXITED(status) ? (uint32_t)WEXITSTATUS(status) : (uint32_t)WTERMSIG(status);
    }
    pthread_cond_broadcast(&host->process_changed);
    pthread_mutex_unlock(&host->lock);
    if (waited == 0) return hl_linux_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    if (waited < 0) return hl_linux_errno_result();
    if (WIFEXITED(status))
        return hl_linux_result(HL_STATUS_OK, (uint64_t)WEXITSTATUS(status), HL_HOST_PROCESS_EXIT_CODE);
    if (WIFSIGNALED(status))
        return hl_linux_result(HL_STATUS_OK, (uint64_t)WTERMSIG(status), HL_HOST_PROCESS_EXIT_SIGNAL);
    return hl_linux_result(HL_STATUS_CORRUPT, 0, (uint64_t)(uint32_t)status);
}

static hl_host_result hl_linux_process_terminate(void *context, hl_host_handle handle, uint32_t reason) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pid_t pid;
    if (reason != HL_HOST_PROCESS_TERMINATE_INTERRUPT && reason != HL_HOST_PROCESS_TERMINATE_FORCE &&
        (reason <= HL_HOST_PROCESS_TERMINATE_SIGNAL || reason > HL_HOST_PROCESS_TERMINATE_SIGNAL + 64))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    pid = entry != NULL && !entry->process_reaped && !host->destroying ? entry->descriptor : -1;
    pthread_mutex_unlock(&host->lock);
    if (pid < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int signal_number = reason == HL_HOST_PROCESS_TERMINATE_INTERRUPT ? SIGINT
                        : reason == HL_HOST_PROCESS_TERMINATE_FORCE   ? SIGKILL
                                                                    : (int)(reason - HL_HOST_PROCESS_TERMINATE_SIGNAL);
    if (kill(pid, signal_number) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_process_close(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (host->destroying || !entry->process_reaped || entry->process_waiting || entry->process_waiters != 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_BUSY, 0, 0);
    }
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->descriptor = -1;
    entry->process_reaped = 0;
    entry->process_exit_kind = 0;
    entry->process_exit_value = 0;
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

/* --- terminal ---------------------------------------------------------------------------
 *
 * The device, and only the device. Everything the guest's terminal vocabulary is made of -- the
 * attribute structure, its control characters, canonical buffering, line editing, signal
 * generation, minimum-and-timeout reads, output post-processing -- is above this and is not
 * reachable from here on purpose.
 *
 * The five mode bits are the abstract capabilities the contract names, mapped onto the five native
 * flags that carry the same meaning. Nothing else in the native attributes is read or written, so
 * a caller that sets a mode gets exactly the change it named and no adjacent policy it did not.
 */
