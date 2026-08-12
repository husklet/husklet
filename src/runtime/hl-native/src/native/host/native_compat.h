#ifndef HL_HOST_NATIVE_COMPAT_H
#define HL_HOST_NATIVE_COMPAT_H

#include "system.h"

#include <fcntl.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <time.h>

#if defined(__APPLE__)
#include <sys/event.h>
#include <sys/socket.h>
#include <sys/xattr.h>

#define HL_NATIVE_RENAME_NOREPLACE RENAME_EXCL
#define HL_NATIVE_RENAME_EXCHANGE RENAME_SWAP
#define HL_NATIVE_SEEK_DATA SEEK_DATA
#define HL_NATIVE_SEEK_HOLE SEEK_HOLE

static inline void hl_native_kqueue_close(int descriptor) {
    (void)descriptor;
}

static inline void hl_native_kqueue_duplicate(int source, int destination) {
    (void)source;
    (void)destination;
}

/* A kqueue() descriptor here is a real kernel object, so dup2()/F_DUPFD move it with no bookkeeping. */
static inline void hl_native_kqueue_relocate(int source, int destination) {
    (void)source;
    (void)destination;
}

static inline int hl_native_fd_path(int descriptor, char *path, size_t capacity) {
    (void)capacity;
    return fcntl(descriptor, F_GETPATH, path);
}

static inline int hl_native_open_watch(const char *path) {
    return open(path, O_EVTONLY);
}

static inline int hl_native_set_no_sigpipe(int descriptor) {
    int enabled = 1;
    return setsockopt(descriptor, SOL_SOCKET, SO_NOSIGPIPE, &enabled, sizeof(enabled));
}

static inline int hl_native_birthtime(const struct stat *status, struct timespec *time) {
    *time = status->st_birthtimespec;
    return 0;
}

static inline int hl_native_setxattr(const char *path, const char *name, const void *value, size_t size, int position,
                                     int options) {
    return setxattr(path, name, value, size, position, options);
}

static inline int hl_native_fsetxattr(int fd, const char *name, const void *value, size_t size, int position,
                                      int options) {
    return fsetxattr(fd, name, value, size, position, options);
}

static inline ssize_t hl_native_getxattr(const char *path, const char *name, void *value, size_t size, int position,
                                         int options) {
    return getxattr(path, name, value, size, position, options);
}

static inline ssize_t hl_native_fgetxattr(int fd, const char *name, void *value, size_t size, int position,
                                          int options) {
    return fgetxattr(fd, name, value, size, position, options);
}

static inline ssize_t hl_native_listxattr(const char *path, char *list, size_t size, int options) {
    return listxattr(path, list, size, options);
}

static inline int hl_native_removexattr(const char *path, const char *name, int options) {
    return removexattr(path, name, options);
}
#elif defined(__linux__)
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <limits.h>
#include <pthread.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/inotify.h>
#include <sys/timerfd.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/statfs.h>
#include <sys/syscall.h>
#include <sys/xattr.h>
#include <time.h>
#include <unistd.h>

/* renameat2 consumes the Linux guest flag values directly.  Darwin's
   renameatx_np uses different values, so callers must use these host-native
   constants instead of carrying the Darwin spelling into common code. */
#define HL_NATIVE_RENAME_NOREPLACE 1u
#define HL_NATIVE_RENAME_EXCHANGE 2u
#define HL_NATIVE_SEEK_DATA SEEK_DATA
#define HL_NATIVE_SEEK_HOLE SEEK_HOLE

/* Temporary shape-compatible event seam for the production unity runtime.
   Linux-native epoll/inotify/timerfd implementations will replace these calls;
   keeping the Darwin vocabulary here prevents it leaking into target roots. */
struct kevent {
    uintptr_t ident;
    int16_t filter;
    uint16_t flags;
    uint32_t fflags;
    intptr_t data;
    void *udata;
};

#define EVFILT_READ (-1)
#define EVFILT_WRITE (-2)
#define EVFILT_VNODE (-4)
#define EVFILT_TIMER (-7)
#define EVFILT_USER (-10)
#define EV_ADD UINT16_C(0x0001)
#define EV_DELETE UINT16_C(0x0002)
#define EV_ENABLE UINT16_C(0x0004)
#define EV_ONESHOT UINT16_C(0x0010)
#define EV_CLEAR UINT16_C(0x0020)
#define EV_EOF UINT16_C(0x8000)
#define EV_ERROR UINT16_C(0x4000)
#define NOTE_DELETE UINT32_C(0x0001)
#define NOTE_WRITE UINT32_C(0x0002)
#define NOTE_EXTEND UINT32_C(0x0004)
#define NOTE_ATTRIB UINT32_C(0x0008)
#define NOTE_LINK UINT32_C(0x0010)
#define NOTE_RENAME UINT32_C(0x0020)
#define NOTE_REVOKE UINT32_C(0x0040)
#define NOTE_TRIGGER UINT32_C(0x01000000)
#define NOTE_NSECONDS UINT32_C(0x00000004)
// macOS-only hint (minimal power-aware timer coalescing); inert in this Linux shim, whose timerfd is
// already an exact hrtimer.
#define NOTE_CRITICAL UINT32_C(0x00000020)

#define EV_SET(event, identifier, event_filter, event_flags, event_fflags, event_data, event_udata)                    \
    (*(event) = (struct kevent){(uintptr_t)(identifier), (int16_t)(event_filter), (uint16_t)(event_flags),             \
                                (uint32_t)(event_fflags), (intptr_t)(event_data), (void *)(event_udata)})

typedef struct hl_native_kregistration {
    uint64_t queue;
    uint64_t token;
    int target;
    int wake;
    int16_t filter;
    uint32_t read;
    uint32_t write;
    uint16_t flags;
    void *udata;
    struct hl_native_kregistration *next;
} hl_native_kregistration;

static pthread_mutex_t hl_native_klock = PTHREAD_MUTEX_INITIALIZER;
static hl_native_kregistration *hl_native_kregistrations;

typedef struct hl_native_kalias {
    int descriptor;
    uint64_t queue;
    struct hl_native_kalias *next;
} hl_native_kalias;

static hl_native_kalias *hl_native_kaliases;
static uint64_t hl_native_knext = 1;
static uint64_t hl_native_ktoken_next = 1;

static inline hl_native_kalias *hl_native_kalias_find(int descriptor) {
    for (hl_native_kalias *alias = hl_native_kaliases; alias != NULL; alias = alias->next)
        if (alias->descriptor == descriptor) return alias;
    return NULL;
}

static inline hl_native_kregistration *hl_native_kfind(uint64_t queue, int target) {
    hl_native_kregistration *entry;
    for (entry = hl_native_kregistrations; entry != NULL; entry = entry->next)
        if (entry->queue == queue && entry->target == target) return entry;
    return NULL;
}

static inline hl_native_kregistration *hl_native_ktoken_find(uint64_t token) {
    for (hl_native_kregistration *entry = hl_native_kregistrations; entry != NULL; entry = entry->next)
        if (entry->token == token) return entry;
    return NULL;
}

static inline void hl_native_kqueue_release_locked(int descriptor) {
    hl_native_kalias **alias_cursor = &hl_native_kaliases;
    while (*alias_cursor != NULL && (*alias_cursor)->descriptor != descriptor)
        alias_cursor = &(*alias_cursor)->next;
    if (*alias_cursor == NULL) return;
    hl_native_kalias *alias = *alias_cursor;
    uint64_t queue = alias->queue;
    *alias_cursor = alias->next;
    free(alias);
    for (hl_native_kalias *survivor = hl_native_kaliases; survivor != NULL; survivor = survivor->next)
        if (survivor->queue == queue) return;
    hl_native_kregistration **cursor = &hl_native_kregistrations;
    while (*cursor != NULL) {
        hl_native_kregistration *entry = *cursor;
        if (entry->queue != queue) {
            cursor = &entry->next;
            continue;
        }
        *cursor = entry->next;
        if (entry->wake >= 0) {
            hl_host_process_fd_private_remove(entry->wake);
            close(entry->wake);
        }
        free(entry);
    }
}

static inline void hl_native_kqueue_close(int descriptor) {
    pthread_mutex_lock(&hl_native_klock);
    hl_native_kqueue_release_locked(descriptor);
    pthread_mutex_unlock(&hl_native_klock);
}

/* A duplicated descriptor names the same epoll open-file description.  Keep a
   stable identity and retain registrations until the last alias closes. */
static inline void hl_native_kqueue_duplicate(int source, int destination) {
    if (source < 0 || destination < 0 || source == destination) return;
    pthread_mutex_lock(&hl_native_klock);
    hl_native_kalias *original = hl_native_kalias_find(source);
    if (original != NULL) {
        uint64_t queue = original->queue;
        hl_native_kqueue_release_locked(destination);
        hl_native_kalias *alias = calloc(1, sizeof(*alias));
        if (alias != NULL) {
            alias->descriptor = destination;
            alias->queue = queue;
            alias->next = hl_native_kaliases;
            hl_native_kaliases = alias;
        }
    }
    pthread_mutex_unlock(&hl_native_klock);
}

/* Move a shim kqueue's identity between descriptor numbers.  The queue lives only in the alias table above,
   keyed by descriptor NUMBER: an untold relocation leaves the new number unknown (kevent() -> EBADF) and the
   old one a stale entry another descriptor could inherit.  Register `destination` before dropping `source`. */
static inline void hl_native_kqueue_relocate(int source, int destination) {
    if (source < 0 || destination < 0 || source == destination) return;
    hl_native_kqueue_duplicate(source, destination);
    hl_native_kqueue_close(source);
}

static inline int hl_native_kevent_rehome(int descriptor, int old_target, int new_target) {
    hl_native_kregistration *entry;
    struct epoll_event event = {0};
    pthread_mutex_lock(&hl_native_klock);
    hl_native_kalias *alias = hl_native_kalias_find(descriptor);
    entry = alias == NULL ? NULL : hl_native_kfind(alias->queue, old_target);
    if (entry == NULL) {
        pthread_mutex_unlock(&hl_native_klock);
        errno = ENOENT;
        return -1;
    }
    event.events = (entry->read ? EPOLLIN : 0u) | (entry->write ? EPOLLOUT : 0u);
    if ((entry->flags & EV_CLEAR) != 0) event.events |= EPOLLET;
    if ((entry->flags & EV_ONESHOT) != 0) event.events |= EPOLLONESHOT;
    event.data.u64 = entry->token;
    if (epoll_ctl(descriptor, EPOLL_CTL_DEL, old_target, NULL) != 0 ||
        epoll_ctl(descriptor, EPOLL_CTL_ADD, new_target, &event) != 0) {
        pthread_mutex_unlock(&hl_native_klock);
        return -1;
    }
    entry->target = new_target;
    pthread_mutex_unlock(&hl_native_klock);
    return 0;
}

static inline int kqueue(void) {
    int descriptor = epoll_create1(EPOLL_CLOEXEC);
    if (descriptor < 0) return descriptor;
    hl_native_kalias *alias = calloc(1, sizeof(*alias));
    if (alias == NULL) {
        close(descriptor);
        errno = ENOMEM;
        return -1;
    }
    pthread_mutex_lock(&hl_native_klock);
    alias->descriptor = descriptor;
    alias->queue = hl_native_knext++;
    alias->next = hl_native_kaliases;
    hl_native_kaliases = alias;
    pthread_mutex_unlock(&hl_native_klock);
    return descriptor;
}

static inline int kevent(int descriptor, const struct kevent *changes, int change_count, struct kevent *events,
                         int event_count, const struct timespec *timeout) {
    int index;
    if (change_count < 0 || event_count < 0 || (change_count != 0 && changes == NULL) ||
        (event_count != 0 && events == NULL)) {
        errno = EINVAL;
        return -1;
    }
    pthread_mutex_lock(&hl_native_klock);
    hl_native_kalias *alias = hl_native_kalias_find(descriptor);
    if (alias == NULL) {
        pthread_mutex_unlock(&hl_native_klock);
        errno = EBADF;
        return -1;
    }
    uint64_t queue = alias->queue;
    for (index = 0; index < change_count; ++index) {
        const struct kevent *change = &changes[index];
        hl_native_kregistration *entry;
        struct epoll_event event = {0};
        int operation;
        int target;
        if (change->filter == EVFILT_VNODE) {
            pthread_mutex_unlock(&hl_native_klock);
            errno = ENOTSUP;
            return -1;
        }
        if (change->filter != EVFILT_READ && change->filter != EVFILT_WRITE && change->filter != EVFILT_USER &&
            change->filter != EVFILT_TIMER) {
            pthread_mutex_unlock(&hl_native_klock);
            errno = ENOTSUP;
            return -1;
        }
        target = change->filter == EVFILT_USER ? -1 : change->filter == EVFILT_TIMER ? -2 : (int)change->ident;
        entry = hl_native_kfind(queue, target);
        if (change->filter == EVFILT_USER && (change->fflags & NOTE_TRIGGER) != 0) {
            uint64_t one = 1;
            if (entry == NULL || write(entry->wake, &one, sizeof(one)) != (ssize_t)sizeof(one)) {
                pthread_mutex_unlock(&hl_native_klock);
                errno = entry == NULL ? ENOENT : errno;
                return -1;
            }
            continue;
        }
        if ((change->flags & EV_DELETE) != 0) {
            if (entry == NULL) {
                pthread_mutex_unlock(&hl_native_klock);
                errno = ENOENT;
                return -1;
            }
            if (change->filter == EVFILT_READ)
                entry->read = 0;
            else if (change->filter == EVFILT_WRITE)
                entry->write = 0;
            else
                entry->read = 0;
        } else if ((change->flags & EV_ADD) != 0) {
            if (entry == NULL) {
                entry = calloc(1, sizeof(*entry));
                if (entry == NULL) {
                    pthread_mutex_unlock(&hl_native_klock);
                    errno = ENOMEM;
                    return -1;
                }
                entry->queue = queue;
                entry->token = hl_native_ktoken_next++;
                entry->target = target;
                entry->wake = -1;
                entry->filter = change->filter;
                entry->next = hl_native_kregistrations;
                hl_native_kregistrations = entry;
            }
            entry->flags = change->flags;
            entry->udata = change->udata;
            if (change->filter == EVFILT_READ)
                entry->read = 1;
            else if (change->filter == EVFILT_WRITE)
                entry->write = 1;
            else {
                if (entry->wake < 0) {
                    int wake = change->filter == EVFILT_TIMER
                                   ? timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK)
                                   : eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
                    if (wake >= 0) {
                        int adopted = hl_host_process_fd_private_adopt(wake);
                        if (adopted < 0) {
                            close(wake);
                            errno = -adopted;
                            wake = -1;
                        } else {
                            wake = adopted;
                        }
                    }
                    entry->wake = wake;
                }
                if (entry->wake < 0) {
                    pthread_mutex_unlock(&hl_native_klock);
                    return -1;
                }
                entry->read = 1;
                if (change->filter == EVFILT_TIMER) {
                    struct itimerspec setting = {0};
                    int64_t nanoseconds = change->data;
                    if ((change->fflags & NOTE_NSECONDS) == 0 || nanoseconds < 0) {
                        pthread_mutex_unlock(&hl_native_klock);
                        errno = ENOTSUP;
                        return -1;
                    }
                    if (nanoseconds == 0) nanoseconds = 1;
                    setting.it_value.tv_sec = (time_t)(nanoseconds / INT64_C(1000000000));
                    setting.it_value.tv_nsec = (long)(nanoseconds % INT64_C(1000000000));
                    if ((change->flags & EV_ONESHOT) == 0) setting.it_interval = setting.it_value;
                    if (timerfd_settime(entry->wake, 0, &setting, NULL) != 0) {
                        pthread_mutex_unlock(&hl_native_klock);
                        return -1;
                    }
                }
            }
        } else {
            continue;
        }
        // Always request EPOLLRDHUP on the read side so a peer half-close (shutdown SHUT_WR) surfaces as
        // EV_EOF, matching macOS kqueue's native EVFILT_READ EV_EOF-on-half-close. Without it Linux epoll
        // reports a half-close only as a plain EPOLLIN (readable-at-EOF) and never sets EV_EOF, so the engine
        // could not deliver EPOLLRDHUP to a guest that registered it.
        event.events = (entry->read ? (EPOLLIN | EPOLLRDHUP) : 0u) | (entry->write ? EPOLLOUT : 0u);
        if ((entry->flags & EV_CLEAR) != 0) event.events |= EPOLLET;
        if ((entry->flags & EV_ONESHOT) != 0) event.events |= EPOLLONESHOT;
        event.data.u64 = entry->token;
        operation = event.events == 0 ? EPOLL_CTL_DEL : EPOLL_CTL_MOD;
        if (entry->wake >= 0) target = entry->wake;
        int control = epoll_ctl(descriptor, operation, target, operation == EPOLL_CTL_DEL ? NULL : &event);
        if (control != 0 && operation == EPOLL_CTL_MOD && errno == ENOENT) {
            operation = EPOLL_CTL_ADD;
            control = epoll_ctl(descriptor, operation, target, &event);
        }
        if (control != 0 && operation == EPOLL_CTL_ADD && errno == EEXIST) control = 0;
        if (control != 0 && !(operation == EPOLL_CTL_DEL && errno == ENOENT)) {
            pthread_mutex_unlock(&hl_native_klock);
            return -1;
        }
        if (operation == EPOLL_CTL_DEL && entry->filter == EVFILT_TIMER && entry->wake >= 0) {
            hl_host_process_fd_private_remove(entry->wake);
            close(entry->wake);
            entry->wake = -1;
        }
    }
    pthread_mutex_unlock(&hl_native_klock);
    if (event_count == 0) return 0;
    {
        struct epoll_event native_events[256];
        int timeout_ms = -1;
        int count;
        if (event_count > 256) event_count = 256;
        if (timeout != NULL) {
            int64_t milliseconds = (int64_t)timeout->tv_sec * 1000 + (timeout->tv_nsec + 999999) / 1000000;
            timeout_ms = milliseconds > INT_MAX ? INT_MAX : (int)milliseconds;
        }
        for (;;) {
            count = epoll_wait(descriptor, native_events, event_count, timeout_ms);
            if (count < 0) return -1;
            int delivered = 0;
            for (index = 0; index < count; ++index) {
                pthread_mutex_lock(&hl_native_klock);
                hl_native_kregistration *entry = hl_native_ktoken_find(native_events[index].data.u64);
                if (entry == NULL) {
                    pthread_mutex_unlock(&hl_native_klock);
                    continue; /* stale readiness raced the last alias close */
                }
                uint32_t ready = native_events[index].events;
                if (entry->filter == EVFILT_TIMER) {
                    // The underlying timerfd is TFD_NONBLOCK and, when the guest kqueue (a host epoll fd) is
                    // shared across fork(2), it backs BOTH the parent's and the child's inherited timer fd.
                    // Level-triggered epoll can therefore report readiness to one waiter after the other has
                    // already drained the accumulated expirations. A drained read yields EAGAIN / a short
                    // read (0 expirations) -- Linux's timerfd read() NEVER surfaces a count of 0, so treat
                    // this as a spurious wake: skip the stale event instead of delivering data=0. The fd is
                    // now at 0 (level-triggered), so a re-wait below blocks until the next real expiry.
                    uint64_t expirations = 0;
                    if (read(entry->wake, &expirations, sizeof(expirations)) != (ssize_t)sizeof(expirations) ||
                        expirations == 0) {
                        pthread_mutex_unlock(&hl_native_klock);
                        continue;
                    }
                    events[delivered] = (struct kevent){(uintptr_t)(entry->target < 0 ? descriptor : entry->target),
                                                        EVFILT_TIMER,
                                                        0,
                                                        0,
                                                        (intptr_t)expirations,
                                                        entry->udata};
                    delivered++;
                    pthread_mutex_unlock(&hl_native_klock);
                    continue;
                }
                int16_t filter = (ready & (EPOLLIN | EPOLLPRI | EPOLLRDHUP)) != 0 ? EVFILT_READ : EVFILT_WRITE;
                events[delivered] = (struct kevent){
                    (uintptr_t)(entry->target < 0 ? descriptor : entry->target), filter, 0, 0, 0, entry->udata};
                if ((ready & (EPOLLHUP | EPOLLRDHUP)) != 0) events[delivered].flags |= EV_EOF;
                if ((ready & EPOLLERR) != 0) events[delivered].flags |= EV_ERROR;
                if (entry->target < 0) {
                    uint64_t value;
                    ssize_t consumed = read(entry->wake, &value, sizeof(value));
                    if (consumed < 0 && errno != EAGAIN && errno != EWOULDBLOCK) {
                        events[delivered].flags |= EV_ERROR;
                        events[delivered].data = errno;
                    }
                    events[delivered].filter = EVFILT_USER;
                }
                delivered++;
                pthread_mutex_unlock(&hl_native_klock);
            }
            // A blocking wait (timeout==NULL) that saw only stale-drained timer wakeups must keep waiting for a
            // real expiration rather than returning 0 events (which the timerfd read path maps to a spurious
            // EAGAIN on a blocking fd). Any events delivered, a finite/zero timeout, or an idle wakeup (count==0
            // -> timeout elapsed) all return as-is.
            if (delivered == 0 && count > 0 && timeout == NULL) continue;
            return delivered;
        }
    }
}

#define st_atimespec st_atim
#define st_mtimespec st_mtim
#define st_ctimespec st_ctim

static inline int hl_native_fd_path(int descriptor, char *path, size_t capacity) {
    char link[64];
    ssize_t count;
    int length;
    if (path == NULL || capacity == 0) {
        errno = EINVAL;
        return -1;
    }
    length = snprintf(link, sizeof(link), "/proc/self/fd/%d", descriptor);
    if (length < 0 || (size_t)length >= sizeof(link)) {
        errno = EOVERFLOW;
        return -1;
    }
    count = readlink(link, path, capacity - 1);
    if (count < 0) return -1;
    if ((size_t)count >= capacity - 1) {
        path[0] = '\0';
        errno = ENAMETOOLONG;
        return -1;
    }
    path[count] = '\0';
    return 0;
}

static inline int hl_native_open_watch(const char *path) {
    (void)path;
    errno = ENOTSUP;
    return -1;
}

static inline int hl_native_set_no_sigpipe(int descriptor) {
    (void)descriptor;
    return 0; /* Linux callers suppress SIGPIPE per send with MSG_NOSIGNAL. */
}

static inline int hl_native_birthtime(const struct stat *status, struct timespec *time) {
    (void)status;
    time->tv_sec = 0;
    time->tv_nsec = 0;
    errno = ENOTSUP;
    return -1;
}

static inline int hl_native_setxattr(const char *path, const char *name, const void *value, size_t size, int position,
                                     int options) {
    (void)position;
    return options != 0 ? lsetxattr(path, name, value, size, 0) : setxattr(path, name, value, size, 0);
}

static inline int hl_native_fsetxattr(int fd, const char *name, const void *value, size_t size, int position,
                                      int options) {
    (void)position;
    (void)options;
    return fsetxattr(fd, name, value, size, 0);
}

static inline ssize_t hl_native_getxattr(const char *path, const char *name, void *value, size_t size, int position,
                                         int options) {
    (void)position;
    return options != 0 ? lgetxattr(path, name, value, size) : getxattr(path, name, value, size);
}

static inline ssize_t hl_native_fgetxattr(int fd, const char *name, void *value, size_t size, int position,
                                          int options) {
    (void)position;
    (void)options;
    return fgetxattr(fd, name, value, size);
}

static inline ssize_t hl_native_listxattr(const char *path, char *list, size_t size, int options) {
    return options != 0 ? llistxattr(path, list, size) : listxattr(path, list, size);
}

static inline int hl_native_removexattr(const char *path, const char *name, int options) {
    return options != 0 ? lremovexattr(path, name) : removexattr(path, name);
}

#define XATTR_NOFOLLOW 1
#ifndef ENOATTR
#define ENOATTR ENODATA
#endif
#ifndef O_SYMLINK
/* macOS O_SYMLINK opens the SYMLINK NODE itself (not its target). Linux has no such flag: an O_PATH
 * open follows a final symlink to its target, and only O_PATH|O_NOFOLLOW yields a handle that names
 * the link node (so readlinkat(fd,"",..) / fstatat(fd,"",AT_EMPTY_PATH) operate on the link). The
 * container-vfs open path uses O_SYMLINK for exactly the O_PATH|O_NOFOLLOW-on-a-symlink case, so the
 * Linux emulation must carry O_NOFOLLOW alongside O_PATH -- otherwise the bound/overlay open follows
 * the link and the empty-path readlink names the target. */
#define O_SYMLINK (O_PATH | O_NOFOLLOW)
#endif

static inline int renameatx_np(int old_directory, const char *old_path, int new_directory, const char *new_path,
                               unsigned flags) {
#ifdef SYS_renameat2
    return (int)syscall(SYS_renameat2, old_directory, old_path, new_directory, new_path, flags);
#else
    if (flags == 0) return renameat(old_directory, old_path, new_directory, new_path);
    errno = ENOSYS;
    return -1;
#endif
}

#ifndef SIGEMT
#define SIGEMT (SIGRTMIN + 6)
#endif
#ifndef SIGINFO
#define SIGINFO (SIGRTMIN + 7)
#endif
#elif defined(_WIN32)
/* ===================== Windows (mingw-w64 / UCRT) =========================
 *
 * Third arm of the same fork, and it exists for the same reason the Linux arm
 * does: Darwin is this tree's lingua franca, so the vocabulary above -- struct
 * kevent, the EVFILT_/EV_/NOTE_ constants, the hl_native_* wrappers -- is what
 * every caller compiles against unconditionally.  A host that lacks the
 * underlying kernel object still has to present the words, or the spelling
 * leaks upward into target roots.  That is the file's whole job.
 *
 * Three kinds of entry live below and they are labelled where they appear:
 *   REAL      -- a genuine Windows implementation of the same operation.
 *   SHAPE     -- a synthesized type or constant with no behaviour of its own,
 *                present so the callers compile (exactly what the Linux arm
 *                does for kqueue).
 *   REFUSAL   -- returns -1 with a specific errno.  Never a quiet success, and
 *                never a constant chosen to let a call compile and then do
 *                something other than what its name says.
 *
 * Deliberately NOT here: the POSIX descriptor vocabulary (fcntl/F_*, openat and
 * the *at family, AT_FDCWD, O_CLOEXEC/O_DIRECTORY, mmap/PROT_*, the socket and
 * poll constants).  Both other arms get those from the system headers, not from
 * this file; inventing them here would be a new layer wearing this one's name,
 * and an honest version of most of them needs the Windows backend's
 * descriptor->HANDLE table, which a header included by the guest-target TU must
 * not reach into. */
#include <errno.h>
#include <io.h>
#include <limits.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

/* Win32 entry points are declared by hand rather than by including <windows.h>.
 * This header is pulled into the guest-target unity TU, and <windows.h> would
 * drop macros named ERROR, IN, OUT, min and max -- plus a second file-status
 * vocabulary -- into the same TU that defines the guest ABI.  Only the five
 * functions the bodies below actually call are named, with the ABI types
 * spelled out (HANDLE is void *, DWORD is unsigned long, BOOL is int, WCHAR is
 * wchar_t on this target).  Skipped when <windows.h> did get included first, so
 * the two declarations can never drift apart. */
#ifndef _WINDOWS_
__declspec(dllimport) unsigned long __stdcall GetLastError(void);
__declspec(dllimport) unsigned long __stdcall GetFinalPathNameByHandleW(void *file, wchar_t *path,
                                                                        unsigned long capacity, unsigned long flags);
__declspec(dllimport) int __stdcall MoveFileExW(const wchar_t *existing, const wchar_t *replacement,
                                                unsigned long flags);
__declspec(dllimport) int __stdcall MultiByteToWideChar(unsigned int page, unsigned long flags, const char *bytes,
                                                        int byte_count, wchar_t *wide, int wide_count);
__declspec(dllimport) int __stdcall WideCharToMultiByte(unsigned int page, unsigned long flags, const wchar_t *wide,
                                                        int wide_count, char *bytes, int byte_count,
                                                        const char *fallback, int *used_fallback);
#endif

#define HL_NATIVE_WIN_UTF8 65001u
#define HL_NATIVE_WIN_REPLACE_EXISTING 0x1ul

/* Win32 status -> errno.  Small on purpose: only the codes the three REAL
 * bodies below can actually produce are listed, and anything else becomes EIO
 * rather than a guess.  The backend keeps its own, wider map because it also
 * reports the raw status in hl_host_result.detail; nothing here has a detail
 * channel to report into, so precision stops at the errno. */
static inline int hl_native_win_errno(unsigned long status) {
    switch (status) {
    case 2ul:
    case 3ul: return ENOENT;
    case 5ul: return EACCES;
    case 6ul: return EBADF;
    case 8ul:
    case 14ul: return ENOMEM;
    case 17ul: return EXDEV;
    case 32ul: return EBUSY;
    case 80ul:
    case 183ul: return EEXIST;
    case 87ul: return EINVAL;
    case 122ul:
    case 206ul: return ENAMETOOLONG;
    case 145ul: return ENOTEMPTY;
    default: return EIO;
    }
}

/* Paths cross this boundary as UTF-8 and Win32 wants UTF-16.  The ...A entry
 * points would do the conversion themselves but through the process ANSI code
 * page, which is only UTF-8 when a manifest says so -- a link-time property no
 * header can assume.  Converting explicitly makes the encoding a property of
 * this call instead. */
static inline wchar_t *hl_native_win_widen(const char *text) {
    int count = MultiByteToWideChar(HL_NATIVE_WIN_UTF8, 0ul, text, -1, NULL, 0);
    wchar_t *wide;
    if (count <= 0) {
        errno = EILSEQ;
        return NULL;
    }
    wide = (wchar_t *)malloc((size_t)count * sizeof(*wide));
    if (wide == NULL) {
        errno = ENOMEM;
        return NULL;
    }
    if (MultiByteToWideChar(HL_NATIVE_WIN_UTF8, 0ul, text, -1, wide, count) != count) {
        free(wide);
        errno = EILSEQ;
        return NULL;
    }
    return wide;
}

/* renameat2's flag values, same as the Linux arm: renameatx_np below consumes
 * them directly, so callers keep passing the guest's numbering and never learn
 * that Darwin's RENAME_SWAP/RENAME_EXCL spell them differently. */
#define HL_NATIVE_RENAME_NOREPLACE 1u
#define HL_NATIVE_RENAME_EXCHANGE 2u

/* SHAPE.  Windows has no sparse-file whence: allocated-range discovery is
 * FSCTL_QUERY_ALLOCATED_RANGES, not an lseek mode.  These carry the Linux
 * numbering so a later FSCTL-backed implementation needs no respelling, and
 * because the UCRT's _lseeki64 validates origin against {SET,CUR,END} and
 * returns EINVAL for anything else -- a call that reaches the CRT with one of
 * these fails loudly instead of seeking somewhere plausible and wrong. */
#define HL_NATIVE_SEEK_DATA 3
#define HL_NATIVE_SEEK_HOLE 4

/* SHAPE.  Verbatim from the Linux arm, and for the identical reason: the event
 * callers in linux_abi/syscall/event.c are compiled unconditionally and speak
 * kqueue.  Windows has no kqueue and no epoll; its readiness engine is the host
 * backend's pollset (an IOCP plus a wake event and a timer queue), which is
 * keyed by hl_host_handle rather than by an int descriptor.  Binding kqueue()
 * to it needs the backend's descriptor->handle table, so the two functions
 * below refuse rather than approximate -- but the vocabulary still has to
 * exist here or every caller stops compiling. */
struct kevent {
    uintptr_t ident;
    int16_t filter;
    uint16_t flags;
    uint32_t fflags;
    intptr_t data;
    void *udata;
};

#define EVFILT_READ (-1)
#define EVFILT_WRITE (-2)
#define EVFILT_VNODE (-4)
#define EVFILT_TIMER (-7)
#define EVFILT_USER (-10)
#define EV_ADD UINT16_C(0x0001)
#define EV_DELETE UINT16_C(0x0002)
#define EV_ENABLE UINT16_C(0x0004)
#define EV_ONESHOT UINT16_C(0x0010)
#define EV_CLEAR UINT16_C(0x0020)
#define EV_EOF UINT16_C(0x8000)
#define EV_ERROR UINT16_C(0x4000)
#define NOTE_DELETE UINT32_C(0x0001)
#define NOTE_WRITE UINT32_C(0x0002)
#define NOTE_EXTEND UINT32_C(0x0004)
#define NOTE_ATTRIB UINT32_C(0x0008)
#define NOTE_LINK UINT32_C(0x0010)
#define NOTE_RENAME UINT32_C(0x0020)
#define NOTE_REVOKE UINT32_C(0x0040)
#define NOTE_TRIGGER UINT32_C(0x01000000)
#define NOTE_NSECONDS UINT32_C(0x00000004)
// macOS-only hint (minimal power-aware timer coalescing); inert here, as it is in the Linux arm.
#define NOTE_CRITICAL UINT32_C(0x00000020)

#define EV_SET(event, identifier, event_filter, event_flags, event_fflags, event_data, event_udata)                    \
    (*(event) = (struct kevent){(uintptr_t)(identifier), (int16_t)(event_filter), (uint16_t)(event_flags),             \
                                (uint32_t)(event_fflags), (intptr_t)(event_data), (void *)(event_udata)})

/* REFUSAL. */
static inline int kqueue(void) {
    errno = ENOTSUP;
    return -1;
}

/* REFUSAL.  Unreachable in practice -- kqueue() never yields a descriptor, so
 * every caller has already failed by the time it would call this -- but a
 * definition has to exist, and EBADF is the honest answer to a descriptor that
 * cannot name a queue. */
static inline int kevent(int descriptor, const struct kevent *changes, int change_count, struct kevent *events,
                         int event_count, const struct timespec *timeout) {
    (void)descriptor;
    (void)changes;
    (void)change_count;
    (void)events;
    (void)event_count;
    (void)timeout;
    errno = EBADF;
    return -1;
}

/* No queue exists to alias, relocate or release, so these have nothing to do.
 * That is the same reasoning the Darwin arm gives for its empty bodies (there
 * the descriptor is a real kernel object dup2 moves by itself); only the Linux
 * arm needs bookkeeping, because only it keeps a side table. */
static inline void hl_native_kqueue_close(int descriptor) {
    (void)descriptor;
}

static inline void hl_native_kqueue_duplicate(int source, int destination) {
    (void)source;
    (void)destination;
}

static inline void hl_native_kqueue_relocate(int source, int destination) {
    (void)source;
    (void)destination;
}

/* REAL.  GetFinalPathNameByHandleW is the Windows answer to Darwin's
 * F_GETPATH and Linux's /proc/self/fd readlink: it walks the open file object
 * back to a path.  VOLUME_NAME_DOS|FILE_NAME_NORMALIZED is flags 0, and the
 * result always carries the \\?\ prefix, which is a Win32 parsing marker rather
 * than part of the name -- callers here compare and re-open these strings, so
 * it is stripped (and the \\?\UNC\ form folded back to \\server\share). */
static inline int hl_native_fd_path(int descriptor, char *path, size_t capacity) {
    wchar_t inline_buffer[512];
    wchar_t *buffer = inline_buffer;
    wchar_t *start;
    void *handle;
    unsigned long count;
    int written;
    int capped;
    if (path == NULL || capacity == 0) {
        errno = EINVAL;
        return -1;
    }
    handle = (void *)(intptr_t)_get_osfhandle(descriptor);
    if (handle == (void *)(intptr_t)-1 || handle == NULL) {
        errno = EBADF;
        return -1;
    }
    count =
        GetFinalPathNameByHandleW(handle, buffer, (unsigned long)(sizeof(inline_buffer) / sizeof(*inline_buffer)), 0ul);
    if (count >= sizeof(inline_buffer) / sizeof(*inline_buffer)) {
        buffer = (wchar_t *)malloc(((size_t)count + 1u) * sizeof(*buffer));
        if (buffer == NULL) {
            errno = ENOMEM;
            return -1;
        }
        count = GetFinalPathNameByHandleW(handle, buffer, count + 1ul, 0ul);
    }
    if (count == 0ul) {
        int saved = hl_native_win_errno(GetLastError());
        if (buffer != inline_buffer) free(buffer);
        errno = saved;
        return -1;
    }
    start = buffer;
    if (count >= 4ul && start[0] == L'\\' && start[1] == L'\\' && start[2] == L'?' && start[3] == L'\\') {
        if (count >= 8ul && (start[4] == L'U' || start[4] == L'u') && (start[5] == L'N' || start[5] == L'n') &&
            (start[6] == L'C' || start[6] == L'c') && start[7] == L'\\') {
            start += 6;
            start[0] = L'\\';
            start[1] = L'\\';
        } else {
            start += 4;
        }
    }
    capped = capacity > (size_t)INT_MAX ? INT_MAX : (int)capacity;
    written = WideCharToMultiByte(HL_NATIVE_WIN_UTF8, 0ul, start, -1, path, capped, NULL, NULL);
    if (written <= 0) {
        unsigned long status = GetLastError();
        if (buffer != inline_buffer) free(buffer);
        path[0] = '\0';
        errno = status == 122ul ? ENAMETOOLONG : EILSEQ;
        return -1;
    }
    if (buffer != inline_buffer) free(buffer);
    return 0;
}

/* REFUSAL.  Darwin's O_EVTONLY is an open mode that grants no data access and
 * so does not keep the file from being unlinked; Windows has no open mode with
 * that property, and its change notification is a directory HANDLE driving
 * ReadDirectoryChangesW -- an object the backend owns and which is not a
 * descriptor at all.  Same answer the Linux arm gives, for the same reason. */
static inline int hl_native_open_watch(const char *path) {
    (void)path;
    errno = ENOTSUP;
    return -1;
}

/* REAL, and the real answer is "nothing to do".  Windows has no SIGPIPE: a send
 * on a connection the peer has reset returns WSAECONNRESET to the caller rather
 * than raising a signal, so there is no per-socket suppression to install and 0
 * is the truthful result, not a stub that skipped the work. */
static inline int hl_native_set_no_sigpipe(int descriptor) {
    (void)descriptor;
    return 0;
}

/* REAL.  st_ctime in the UCRT's struct stat is the file's CREATION time, not
 * the POSIX inode-change time -- the one field where the Windows spelling is
 * closer to Darwin's st_birthtimespec than to POSIX's. Whole seconds only, so
 * tv_nsec is 0 rather than fabricated.  NTFS records it; a FAT volume leaves it
 * zero, which is what the filesystem itself reports. */
static inline int hl_native_birthtime(const struct stat *status, struct timespec *time) {
    time->tv_sec = (time_t)status->st_ctime;
    time->tv_nsec = 0;
    return 0;
}

/* REFUSAL, whole family.  NTFS does have extended attributes -- the EA stream
 * behind NtSetEaFile/NtQueryEaFile -- but they are not reachable from the CRT
 * and their shape is not Linux's: names are upper-cased, capped at 254 bytes,
 * the whole set must fit one buffer, and there is no user./trusted./security.
 * prefix rule for a guest to key off.  Alternate data streams are the usual
 * suggestion and are the wrong object entirely: a stream is a file, unbounded
 * in size, enumerated by a different API, and it survives a copy under
 * different rules than an attribute does.
 *
 * ENOTSUP is not a placeholder here.  It is exactly what Linux returns for a
 * filesystem carrying no extended-attribute support, so a guest that gets it
 * has been told something true and already knows how to handle it -- which a
 * silent empty listing would not be. */
static inline int hl_native_setxattr(const char *path, const char *name, const void *value, size_t size, int position,
                                     int options) {
    (void)path;
    (void)name;
    (void)value;
    (void)size;
    (void)position;
    (void)options;
    errno = ENOTSUP;
    return -1;
}

static inline int hl_native_fsetxattr(int fd, const char *name, const void *value, size_t size, int position,
                                      int options) {
    (void)fd;
    (void)name;
    (void)value;
    (void)size;
    (void)position;
    (void)options;
    errno = ENOTSUP;
    return -1;
}

static inline ssize_t hl_native_getxattr(const char *path, const char *name, void *value, size_t size, int position,
                                         int options) {
    (void)path;
    (void)name;
    (void)value;
    (void)size;
    (void)position;
    (void)options;
    errno = ENOTSUP;
    return -1;
}

static inline ssize_t hl_native_fgetxattr(int fd, const char *name, void *value, size_t size, int position,
                                          int options) {
    (void)fd;
    (void)name;
    (void)value;
    (void)size;
    (void)position;
    (void)options;
    errno = ENOTSUP;
    return -1;
}

static inline ssize_t hl_native_listxattr(const char *path, char *list, size_t size, int options) {
    (void)path;
    (void)list;
    (void)size;
    (void)options;
    errno = ENOTSUP;
    return -1;
}

static inline int hl_native_removexattr(const char *path, const char *name, int options) {
    (void)path;
    (void)name;
    (void)options;
    errno = ENOTSUP;
    return -1;
}

#define XATTR_NOFOLLOW 1
#ifndef ENOATTR
#define ENOATTR ENODATA
#endif

/* O_SYMLINK is deliberately NOT defined on this arm, and the two call sites in
 * linux_abi/syscall/fs.c will keep failing to compile until a Windows open path
 * exists.  That is the point: the flag means "open the link node itself", the
 * only Windows expression of it is FILE_FLAG_OPEN_REPARSE_POINT on
 * CreateFile/NtCreateFile, and the UCRT's open() has no bit for it.  Any value
 * put here would either be ignored by _open -- silently following the symlink,
 * which is precisely the bug the flag exists to prevent -- or rejected, which a
 * constant cannot signal.  A compile error is the only honest refusal available
 * to a macro. */

/* REAL for the two reachable cases, REFUSAL for the third.
 *
 * A negative directory descriptor is the AT_FDCWD sentinel on every host this
 * tree targets (-2 on Darwin, -100 on Linux); a non-negative one names an open
 * directory, and resolving that to a path needs the backend's handle table.
 * Exchange has no Windows primitive at all -- NTFS offers replace-or-not, never
 * swap -- so it refuses rather than emulating a swap through a temporary name,
 * which would not be atomic and would leave debris on a crash.
 *
 * MoveFileExW without MOVEFILE_REPLACE_EXISTING already fails with
 * ERROR_ALREADY_EXISTS when the target is present, so NOREPLACE is native here
 * rather than a check-then-act. */
static inline int renameatx_np(int old_directory, const char *old_path, int new_directory, const char *new_path,
                               unsigned flags) {
    wchar_t *old_wide;
    wchar_t *new_wide;
    int moved;
    unsigned long status;
    if (old_directory >= 0 || new_directory >= 0 || (flags & HL_NATIVE_RENAME_EXCHANGE) != 0) {
        errno = ENOSYS;
        return -1;
    }
    old_wide = hl_native_win_widen(old_path);
    if (old_wide == NULL) return -1;
    new_wide = hl_native_win_widen(new_path);
    if (new_wide == NULL) {
        free(old_wide);
        return -1;
    }
    moved = MoveFileExW(old_wide, new_wide,
                        (flags & HL_NATIVE_RENAME_NOREPLACE) != 0 ? 0ul : HL_NATIVE_WIN_REPLACE_EXISTING);
    status = moved != 0 ? 0ul : GetLastError();
    free(old_wide);
    free(new_wide);
    if (moved != 0) return 0;
    errno = hl_native_win_errno(status);
    return -1;
}

/* SHAPE.  These two name host signals the engine sends to its OWN threads --
 * cache.c's stop-the-world and thread.c's blocking-syscall interrupt -- picked
 * on Darwin because the guest signal map never targets 7 or 29.  Windows has no
 * signal delivery to a thread at all; the equivalent mechanism is
 * SuspendThread/QueueUserAPC in the host backend.
 *
 * The numbers are above NSIG (23) ON PURPOSE.  The UCRT's signal() and
 * winpthreads' pthread_kill both validate the signal number against NSIG, so
 * every call reaching the CRT with one of these fails with EINVAL instead of
 * landing on some other signal's disposition.  A value inside the valid range
 * would compile the same and quietly hijack SIGABRT or SIGBREAK. */
#ifndef SIGEMT
#define SIGEMT 32
#endif
#ifndef SIGINFO
#define SIGINFO 33
#endif

/* SHAPE.  The POSIX type codes for a symlink and a socket.  0xA000 and 0xC000
 * are the two values _S_IFMT (0xF000) leaves unused by the UCRT, whose st_mode
 * only ever carries FIFO/CHR/DIR/BLK/REG -- so adopting the POSIX numbering
 * costs nothing and collides with nothing.
 *
 * Read the limit plainly: the predicate is exactly as good as whatever filled
 * st_mode, and the UCRT's own stat() never sets either bit.  It reports a
 * reparse point as a regular file.  So on today's tree every S_ISLNK() here
 * answers false, and a Windows stat path that wants symlink awareness has to
 * set S_IFLNK itself -- these definitions are the place for it to put the
 * answer, not a claim that the answer is already correct. */
#ifndef S_IFLNK
#define S_IFLNK 0xA000
#endif
#ifndef S_IFSOCK
#define S_IFSOCK 0xC000
#endif
#ifndef S_ISLNK
#define S_ISLNK(mode) (((mode) & S_IFMT) == S_IFLNK)
#endif
#ifndef S_ISSOCK
#define S_ISSOCK(mode) (((mode) & S_IFMT) == S_IFSOCK)
#endif

/* REAL, and the choice between two right-looking numbers matters.  Windows has
 * a 4 KiB PAGE size and a 64 KiB ALLOCATION granularity, and they are used for
 * different things: protection changes act on pages, but a reservation can only
 * be placed, split or replaced on a 64 KiB boundary.
 *
 * Every caller of getpagesize() in this tree means the second one -- it is the
 * host mmap reconciliation unit (mem.c's mmap/mprotect/mincore alignment,
 * checkpoint.c's image page size), which is why an Apple Silicon host already
 * answers 16384 here while the guest ABI stays at 4096.  Returning 4096 would
 * let those paths compute placements the Windows mapper must then reject.
 *
 * 65536 matches the granularity the Windows backend asserts at startup.  One
 * caller is merely coarsened rather than corrected by it: the translator's
 * self-modifying-code guard protects a page at a time and would work at 4096
 * too, so it over-protects and takes some extra traps.  That is a cost, not a
 * correctness break, and it is the right direction to be wrong in. */
static inline int getpagesize(void) {
    return 65536;
}

/* REAL.  Pure byte swaps, and that is the whole point of defining them here:
 * their only declaration on Windows is in <winsock2.h>, which would pull the
 * entire Winsock struct vocabulary (sockaddr, its own fd_set, timeval) into a
 * TU whose job is marshalling the GUEST's ABI structures of the same names.
 * Four inline swaps cost nothing and foreclose that collision.  Guarded on the
 * Winsock include markers so a TU that legitimately needs sockets keeps the
 * real declarations and never sees a conflicting one. */
#if !defined(_WINSOCK2API_) && !defined(_WINSOCKAPI_)
static inline unsigned short htons(unsigned short value) {
    return __builtin_bswap16(value);
}

static inline unsigned short ntohs(unsigned short value) {
    return __builtin_bswap16(value);
}

static inline unsigned int htonl(unsigned int value) {
    return __builtin_bswap32(value);
}

static inline unsigned int ntohl(unsigned int value) {
    return __builtin_bswap32(value);
}
#endif

/* st_atimespec / st_mtimespec / st_ctimespec are deliberately NOT aliased.
 * The Linux arm can do it with three #defines because glibc's struct stat
 * carries st_atim as a real struct timespec; the UCRT's carries st_atime as a
 * bare time_t and has no sub-second field, and no macro can rename a scalar
 * member into one with .tv_sec and .tv_nsec.  st_blocks and st_blksize are
 * absent for the same reason -- the UCRT's struct stat has neither.
 *
 * So the guest-stat encoder keeps failing to compile on this arm, and it should:
 * the fix is a Windows file-status path that fills a structure with the fields
 * the encoder needs (the information exists -- FILE_BASIC_INFORMATION carries
 * 100 ns timestamps and FILE_STANDARD_INFORMATION the allocated size), not a
 * rename that would silently truncate every timestamp to whole seconds. */
#endif

#endif
