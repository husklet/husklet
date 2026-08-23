#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "../../system.h"

#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <sys/stat.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

static int hl_linux_cpu_line(const char *line, hl_host_cpu_ticks *ticks) {
    unsigned long long user;
    unsigned long long nice;
    unsigned long long system;
    unsigned long long idle;
    unsigned long long wait = 0;
    if (sscanf(line, "%*s %llu %llu %llu %llu %llu", &user, &nice, &system, &idle, &wait) < 4) return 0;
    ticks->user = user;
    ticks->nice = nice;
    ticks->system = system;
    ticks->idle = idle + wait;
    return 1;
}

static void hl_linux_memory(hl_host_system_info *info) {
    FILE *file = fopen("/proc/meminfo", "r");
    char line[256];
    if (file == NULL) return;
    while (fgets(line, sizeof line, file) != NULL) {
        unsigned long long kib = 0;
        if (sscanf(line, "MemTotal: %llu kB", &kib) == 1)
            info->memory_total = kib * 1024;
        else if (sscanf(line, "MemFree: %llu kB", &kib) == 1)
            info->memory_free = kib * 1024;
        else if (sscanf(line, "MemAvailable: %llu kB", &kib) == 1)
            info->memory_available = kib * 1024;
        else if (sscanf(line, "Cached: %llu kB", &kib) == 1)
            info->memory_cached = kib * 1024;
    }
    fclose(file);
}

int hl_host_system_read(hl_host_system_info *info, hl_host_cpu_ticks *cores, size_t core_capacity) {
    FILE *file;
    char line[512];
    uint32_t core_count = 0;
    if (info == NULL || (core_capacity != 0 && cores == NULL)) return 0;
    file = fopen("/proc/stat", "r");
    if (file == NULL) return 0;
    memset(info, 0, sizeof *info);
    while (fgets(line, sizeof line, file) != NULL) {
        if (strncmp(line, "cpu ", 4) == 0) {
            (void)hl_linux_cpu_line(line, &info->aggregate);
        } else if (strncmp(line, "cpu", 3) == 0 && line[3] >= '0' && line[3] <= '9') {
            if (core_count < core_capacity) (void)hl_linux_cpu_line(line, &cores[core_count]);
            core_count++;
        } else {
            unsigned long long boot;
            if (sscanf(line, "btime %llu", &boot) == 1) info->boot_time_seconds = boot;
        }
    }
    fclose(file);
    info->reported_cores = core_count;
    long online = sysconf(_SC_NPROCESSORS_ONLN);
    info->online_cpus = online > 0 ? (uint32_t)online : (core_count != 0 ? core_count : 1);
    if (info->boot_time_seconds == 0) info->boot_time_seconds = (uint64_t)time(NULL);
    hl_linux_memory(info);
    return 1;
}

static uint64_t hl_linux_boot_time(void) {
    static uint64_t cached = 0;
    FILE *file;
    char line[512];
    if (cached != 0) return cached;
    file = fopen("/proc/stat", "r");
    if (file == NULL) return 0;
    while (fgets(line, sizeof line, file) != NULL) {
        unsigned long long boot;
        if (sscanf(line, "btime %llu", &boot) == 1) {
            cached = boot;
            break;
        }
    }
    fclose(file);
    return cached;
}

int hl_host_process_read(int64_t pid, hl_host_process_info *info) {
    char path[64];
    char line[8192];
    FILE *file;
    char *left;
    char *right;
    char *cursor;
    char *save = NULL;
    uint64_t fields[25] = {0};
    long ticks;
    long page_size;
    uint64_t boot_time_seconds;
    if (info == NULL || pid <= 0) return 0;
    snprintf(path, sizeof path, "/proc/%lld/stat", (long long)pid);
    file = fopen(path, "r");
    if (file == NULL) return 0;
    if (fgets(line, sizeof line, file) == NULL) {
        fclose(file);
        return 0;
    }
    fclose(file);
    left = strchr(line, '(');
    right = strrchr(line, ')');
    if (left == NULL || right == NULL || right <= left || right[1] != ' ' || right[2] == '\0') return 0;
    memset(info, 0, sizeof *info);
    size_t name_size = (size_t)(right - left - 1);
    if (name_size >= sizeof info->name) name_size = sizeof info->name - 1;
    memcpy(info->name, left + 1, name_size);
    info->name[name_size] = '\0';
    info->state = right[2];
    cursor = right + 3;
    for (size_t field = 4; field <= 24; ++field) {
        char *token = strtok_r(field == 4 ? cursor : NULL, " ", &save);
        char *end = NULL;
        if (token == NULL) return 0;
        errno = 0;
        fields[field] = strtoull(token, &end, 10);
        if (errno != 0 || end == token) return 0;
    }
    info->parent_pid = (int64_t)fields[4];
    info->process_group = (int64_t)fields[5];
    info->session = (int64_t)fields[6];
    info->terminal_device = (int64_t)fields[7];
    info->foreground_group = (int64_t)fields[8];
    info->threads = fields[20] > UINT32_MAX ? UINT32_MAX : (uint32_t)fields[20];
    ticks = sysconf(_SC_CLK_TCK);
    if (ticks <= 0) ticks = 100;
    info->user_time_ns = fields[14] * UINT64_C(1000000000) / (uint64_t)ticks;
    info->system_time_ns = fields[15] * UINT64_C(1000000000) / (uint64_t)ticks;
    boot_time_seconds = hl_linux_boot_time();
    if (boot_time_seconds != 0) {
        info->start_time_seconds = boot_time_seconds + fields[22] / (uint64_t)ticks;
        info->start_time_ns =
            boot_time_seconds * UINT64_C(1000000000) + fields[22] * UINT64_C(1000000000) / (uint64_t)ticks;
    }
    info->virtual_bytes = fields[23];
    page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0) page_size = 4096;
    info->resident_bytes = fields[24] * (uint64_t)page_size;
    return 1;
}

int hl_host_process_resource_read(hl_host_process_resource_snapshot *snapshot) {
    struct rlimit nofile;
    struct rlimit nproc;
    char children[4096];
    ssize_t children_size;
    int child_descriptor;
    size_t descriptor_count = 0;
    hl_host_process_info process;
    if (snapshot == NULL) return 0;
    memset(snapshot, 0, sizeof *snapshot);
    snapshot->open_descriptors = -1;
    snapshot->threads = -1;
    snapshot->caller_children = -1;
    snapshot->nofile_status = getrlimit(RLIMIT_NOFILE, &nofile);
    if (snapshot->nofile_status == 0) {
        snapshot->nofile_current = nofile.rlim_cur;
        snapshot->nofile_maximum = nofile.rlim_max;
    }
    snapshot->nproc_status = getrlimit(RLIMIT_NPROC, &nproc);
    if (snapshot->nproc_status == 0) {
        snapshot->nproc_current = nproc.rlim_cur;
        snapshot->nproc_maximum = nproc.rlim_max;
    }
    if (hl_host_process_fds(getpid(), NULL, 0, &descriptor_count)) {
        if (descriptor_count > 0) --descriptor_count; /* exclude the enumeration DIR itself */
        snapshot->open_descriptors = descriptor_count > INT32_MAX ? INT32_MAX : (int32_t)descriptor_count;
    }
    if (hl_host_process_read(getpid(), &process))
        snapshot->threads = process.threads > INT32_MAX ? INT32_MAX : (int32_t)process.threads;
    child_descriptor = open("/proc/thread-self/children", O_RDONLY | O_CLOEXEC);
    if (child_descriptor >= 0) {
        do
            children_size = read(child_descriptor, children, sizeof children);
        while (children_size < 0 && errno == EINTR);
        (void)close(child_descriptor);
        if (children_size >= 0) {
            int count = 0;
            int in_number = 0;
            for (ssize_t index = 0; index < children_size; ++index) {
                int digit = children[index] >= '0' && children[index] <= '9';
                if (digit && !in_number) ++count;
                in_number = digit;
            }
            snapshot->caller_children = count;
            snapshot->children_truncated = children_size == (ssize_t)sizeof children;
        }
    }
    return 1;
}

static uint32_t hl_linux_fd_kind(const char *target) {
    if (strncmp(target, "pipe:[", 6) == 0) return HL_HOST_FD_PIPE;
    if (strncmp(target, "socket:[", 8) == 0) return HL_HOST_FD_SOCKET;
    return target[0] == '/' ? HL_HOST_FD_FILE : HL_HOST_FD_OTHER;
}

int hl_host_process_fd_read(int64_t pid, int32_t descriptor, hl_host_process_fd *entry, char *path,
                            size_t path_capacity, size_t *path_size) {
    char link[96];
    char target[PATH_MAX + 1];
    ssize_t length;
    uint64_t start_time_ns;
    if (entry == NULL || path_size == NULL || pid <= 0 || descriptor < 0 || (path_capacity != 0 && path == NULL))
        return 0;
    /* Only the privacy classification below needs anything from the process, and it needs only the
     * identity token. Reading the whole /proc/<pid>/stat record here cost a file open, read and field
     * parse for EVERY inspected descriptor. */
    if (!hl_host_process_start_time_ns(pid, &start_time_ns)) return 0;
    snprintf(link, sizeof link, "/proc/%lld/fd/%d", (long long)pid, descriptor);
    length = readlink(link, target, sizeof target - 1);
    if (length < 0 || (size_t)length >= sizeof target) return 0;
    target[length] = '\0';
    entry->descriptor = descriptor;
    entry->kind = hl_linux_fd_kind(target);
    entry->flags =
        hl_host_process_fd_private_is(pid, start_time_ns, descriptor) ? HL_HOST_PROCESS_FD_ENGINE_PRIVATE : 0;
    entry->reserved = 0;
    entry->stable_device = 0;
    entry->stable_object = 0;
    struct stat status;
    if (stat(link, &status) == 0) {
        entry->stable_device = (uint64_t)status.st_dev;
        entry->stable_object = (uint64_t)status.st_ino;
    }
    *path_size = 0;
    if (entry->kind == HL_HOST_FD_FILE) {
        size_t copied = (size_t)length < path_capacity ? (size_t)length : path_capacity;
        if (copied != 0) memcpy(path, target, copied);
        *path_size = copied;
    }
    return 1;
}

int hl_host_process_fds(int64_t pid, hl_host_process_fd *entries, size_t capacity, size_t *count) {
    char path[64];
    DIR *directory;
    struct dirent *item;
    size_t total = 0;
    uint64_t start_time_ns;
    if (count == NULL || pid <= 0 || (capacity != 0 && entries == NULL)) return 0;
    if (!hl_host_process_start_time_ns(pid, &start_time_ns)) return 0;
    snprintf(path, sizeof path, "/proc/%lld/fd", (long long)pid);
    directory = opendir(path);
    if (directory == NULL) return 0;
    item = readdir(directory);
    while (item != NULL) {
        char *end = NULL;
        long descriptor;
        errno = 0;
        descriptor = strtol(item->d_name, &end, 10);
        if (errno == 0 && end != item->d_name && *end == '\0' && descriptor >= 0 && descriptor <= INT32_MAX) {
            if (total < capacity) {
                entries[total].descriptor = (int32_t)descriptor;
                entries[total].kind = HL_HOST_FD_OTHER;
                entries[total].flags = hl_host_process_fd_private_is(pid, start_time_ns, (int)descriptor)
                                           ? HL_HOST_PROCESS_FD_ENGINE_PRIVATE
                                           : 0;
                entries[total].reserved = 0;
                entries[total].stable_device = 0;
                entries[total].stable_object = 0;
            }
            total++;
        }
        item = readdir(directory);
    }
    closedir(directory);
    *count = total;
    return 1;
}

int hl_host_process_fds_collect(int64_t pid, hl_host_process_fd **entries, size_t *count) {
    char path[64];
    DIR *directory;
    struct dirent *item;
    uint64_t start_time_ns;
    hl_host_process_fd *listing = NULL;
    size_t capacity = 0;
    size_t total = 0;
    if (entries == NULL || count == NULL || pid <= 0) return 0;
    *entries = NULL;
    *count = 0;
    if (!hl_host_process_start_time_ns(pid, &start_time_ns)) return 0;
    snprintf(path, sizeof path, "/proc/%lld/fd", (long long)pid);
    directory = opendir(path);
    if (directory == NULL) return 0;
    item = readdir(directory);
    while (item != NULL) {
        char *end = NULL;
        long descriptor;
        errno = 0;
        descriptor = strtol(item->d_name, &end, 10);
        if (errno == 0 && end != item->d_name && *end == '\0' && descriptor >= 0 && descriptor <= INT32_MAX) {
            if (total == capacity) {
                size_t grown = capacity == 0 ? 64 : capacity * 2;
                hl_host_process_fd *resized;
                if (grown <= capacity || grown > SIZE_MAX / sizeof *listing) {
                    free(listing);
                    closedir(directory);
                    return 0;
                }
                resized = realloc(listing, grown * sizeof *listing);
                if (resized == NULL) {
                    free(listing);
                    closedir(directory);
                    return 0;
                }
                listing = resized;
                capacity = grown;
            }
            listing[total].descriptor = (int32_t)descriptor;
            listing[total].kind = HL_HOST_FD_OTHER;
            listing[total].flags = hl_host_process_fd_private_is(pid, start_time_ns, (int)descriptor)
                                       ? HL_HOST_PROCESS_FD_ENGINE_PRIVATE
                                       : 0;
            listing[total].reserved = 0;
            listing[total].stable_device = 0;
            listing[total].stable_object = 0;
            total++;
        }
        item = readdir(directory);
    }
    closedir(directory);
    *entries = listing;
    *count = total;
    return 1;
}

/* Every live engine process on this host, identified by running the same engine executable.
 *
 * MEMBERSHIP IS NOT A SESSION. This used to also require `info.session == self_info.session`, and that
 * filter silently emptied the result on the workloads it mattered most for: the engine emulates the
 * guest's setsid(2) with the host's, so a guest process that becomes a session leader IS one on the
 * host, and its host session id is its own pid. Measured on a live PostgreSQL cluster -- twelve peers,
 * every one reporting `session == its own pid` against a coordinator in its own session, so the
 * coordinator found ZERO peers, interrupted nobody, waited for nobody and published a manifest naming
 * one process while eight others were still assembling their groups. Those eight then arrived at the
 * broker after the capture had left `Active` and were correctly refused as non-members.
 *
 * The caller narrows this set to its own descendants (`ckpt_is_descendant`), which is both tighter than
 * a session and immune to the guest re-parenting or re-sessioning itself.
 */
int hl_host_process_peers(hl_host_process_peer *entries, size_t capacity, size_t *count) {
    char self_path[PATH_MAX + 1];
    char path[PATH_MAX + 1];
    char link[64];
    DIR *directory;
    struct dirent *item;
    ssize_t self_length;
    size_t total = 0;
    if (count == NULL || (capacity != 0 && entries == NULL)) return 0;
    self_length = readlink("/proc/self/exe", self_path, PATH_MAX);
    if (self_length <= 0 || (size_t)self_length >= sizeof self_path) return 0;
    self_path[self_length] = '\0';
    directory = opendir("/proc");
    if (directory == NULL) return 0;
    item = readdir(directory);
    while (item != NULL) {
        char *end = NULL;
        long long value;
        ssize_t length;
        errno = 0;
        value = strtoll(item->d_name, &end, 10);
        if (errno == 0 && end != item->d_name && *end == '\0' && value > 0 && value != getpid()) {
            snprintf(link, sizeof link, "/proc/%lld/exe", value);
            length = readlink(link, path, PATH_MAX);
            if (length == self_length && memcmp(path, self_path, (size_t)self_length) == 0) {
                if (total < capacity) entries[total].identity = value;
                total++;
            }
        }
        item = readdir(directory);
    }
    closedir(directory);
    *count = total;
    return 1;
}

#if defined(HL_NATIVE_TEST_HOOKS)
#include "hl/base.h"
/* 1 when `pid` is enumerated as a live engine peer of this process, 0 when it is not.
   Exists because peer enumeration is only ever exercised by a coordinator against a REAL forked
   process tree: an in-memory fake cannot express "this peer became a session leader", which is the
   condition that silently emptied the result for every setsid-using guest. */
HL_API int32_t hl_c_backend_host_process_peer_enumerated_test(int32_t pid);

HL_API int32_t hl_c_backend_host_process_peer_enumerated_test(int32_t pid) {
    hl_host_process_peer peers[512];
    size_t count = 0;
    if (!hl_host_process_peers(peers, sizeof peers / sizeof *peers, &count)) return -1;
    if (count > sizeof peers / sizeof *peers) count = sizeof peers / sizeof *peers;
    for (size_t index = 0; index < count; ++index)
        if (peers[index].identity == (int64_t)pid) return 1;
    return 0;
}
#endif

int hl_host_process_interrupt(hl_host_process_peer peer) {
    /* SIGRTMIN+7 is the engine's own THREAD_INT_SIG (native_compat.h aliases it as SIGINFO; see
     * src/linux_abi/thread.c), the ONE signal every engine process installs a permanent handler for.
     * It must not be a guest-reachable signal: the engine installs a host handler for one of those only
     * when the guest itself calls rt_sigaction on it, so kicking with SIGURG -- whose default action is
     * *ignore* -- silently failed to interrupt any guest that does not handle SIGURG. An interactive shell
     * parked in read(2) on its pty was therefore never bounced to its safepoint and never entered capture;
     * the whole tree then waited for it and the embedder saw only a deadline. */
    return peer.identity > 0 && peer.identity <= INT32_MAX && kill((pid_t)peer.identity, SIGRTMIN + 7) == 0;
}
