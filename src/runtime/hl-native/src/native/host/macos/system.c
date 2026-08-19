#include "../system.h"

#include <libproc.h>
#include <mach/host_info.h>
#include <mach/mach_host.h>
#include <mach/machine.h>
#include <mach/processor_info.h>
#include <mach/vm_map.h>
#include <mach/vm_statistics.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <sys/proc_info.h>
#include <sys/resource.h>
#include <sys/sysctl.h>
#include <time.h>
#include <unistd.h>

static uint64_t hl_macos_boot_time(void) {
    struct timeval value = {0};
    size_t size = sizeof value;
    int mib[2] = {CTL_KERN, KERN_BOOTTIME};
    if (sysctl(mib, 2, &value, &size, NULL, 0) == 0 && value.tv_sec > 0) return (uint64_t)value.tv_sec;
    return (uint64_t)time(NULL);
}

static uint32_t hl_macos_online_cpus(void) {
    static const char *names[] = {"hw.activecpu", "hw.logicalcpu", "hw.ncpu"};
    for (size_t index = 0; index < sizeof names / sizeof names[0]; ++index) {
        int value = 0;
        size_t size = sizeof value;
        if (sysctlbyname(names[index], &value, &size, NULL, 0) == 0 && value > 0) return (uint32_t)value;
    }
    long value = sysconf(_SC_NPROCESSORS_ONLN);
    return value > 0 ? (uint32_t)value : 1;
}

int hl_host_system_read(hl_host_system_info *info, hl_host_cpu_ticks *cores, size_t core_capacity) {
    host_cpu_load_info_data_t aggregate = {0};
    mach_msg_type_number_t count = HOST_CPU_LOAD_INFO_COUNT;
    uint64_t memory_size = 0;
    size_t memory_size_size = sizeof memory_size;
    vm_size_t page_size = 4096;
    vm_statistics64_data_t memory = {0};
    mach_msg_type_number_t memory_count = HOST_VM_INFO64_COUNT;
    processor_info_array_t values = NULL;
    mach_msg_type_number_t value_count = 0;
    natural_t cpu_count = 0;
    processor_cpu_load_info_t loads = NULL;
    if (info == NULL || (core_capacity != 0 && cores == NULL)) return 0;
    memset(info, 0, sizeof *info);
    info->boot_time_seconds = hl_macos_boot_time();
    info->online_cpus = hl_macos_online_cpus();
    info->reported_cores = info->online_cpus;
    if (host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO, (host_info_t)&aggregate, &count) == KERN_SUCCESS) {
        info->aggregate.user = (uint64_t)aggregate.cpu_ticks[CPU_STATE_USER];
        info->aggregate.nice = (uint64_t)aggregate.cpu_ticks[CPU_STATE_NICE];
        info->aggregate.system = (uint64_t)aggregate.cpu_ticks[CPU_STATE_SYSTEM];
        info->aggregate.idle = (uint64_t)aggregate.cpu_ticks[CPU_STATE_IDLE];
    }
    (void)sysctlbyname("hw.memsize", &memory_size, &memory_size_size, NULL, 0);
    info->memory_total = memory_size;
    (void)host_page_size(mach_host_self(), &page_size);
    if (host_statistics64(mach_host_self(), HOST_VM_INFO64, (host_info_t)&memory, &memory_count) == KERN_SUCCESS) {
        uint64_t free_bytes = ((uint64_t)memory.free_count + memory.speculative_count) * page_size;
        uint64_t cached_bytes = (uint64_t)memory.inactive_count * page_size;
        info->memory_free = free_bytes;
        info->memory_cached = cached_bytes;
        info->memory_available = free_bytes + cached_bytes;
    }
    if (core_capacity != 0 && host_processor_info(mach_host_self(), PROCESSOR_CPU_LOAD_INFO, &cpu_count, &values,
                                                  &value_count) == KERN_SUCCESS) {
        loads = (processor_cpu_load_info_t)values;
        info->reported_cores = (uint32_t)cpu_count;
        size_t limit = cpu_count < core_capacity ? cpu_count : core_capacity;
        for (size_t index = 0; index < limit; ++index) {
            cores[index].user = (uint64_t)loads[index].cpu_ticks[CPU_STATE_USER];
            cores[index].nice = (uint64_t)loads[index].cpu_ticks[CPU_STATE_NICE];
            cores[index].system = (uint64_t)loads[index].cpu_ticks[CPU_STATE_SYSTEM];
            cores[index].idle = (uint64_t)loads[index].cpu_ticks[CPU_STATE_IDLE];
        }
        vm_deallocate(mach_task_self(), (vm_address_t)values, value_count * sizeof(integer_t));
    }
    return 1;
}

int hl_host_process_read(int64_t pid, hl_host_process_info *info) {
    struct proc_bsdinfo bsd;
    struct proc_taskinfo task;
    if (info == NULL || pid <= 0 || pid > INT32_MAX ||
        proc_pidinfo((int)pid, PROC_PIDTBSDINFO, 0, &bsd, sizeof bsd) != (int)sizeof bsd)
        return 0;
    memset(info, 0, sizeof *info);
    info->parent_pid = bsd.pbi_ppid;
    info->process_group = bsd.pbi_pgid;
    info->session = getsid((pid_t)pid);
    info->terminal_device = bsd.e_tdev;
    info->foreground_group = bsd.e_tpgid;
    info->start_time_seconds = bsd.pbi_start_tvsec;
    info->start_time_ns =
        (uint64_t)bsd.pbi_start_tvsec * UINT64_C(1000000000) + (uint64_t)bsd.pbi_start_tvusec * UINT64_C(1000);
    info->threads = 1;
    switch (bsd.pbi_status) {
    case 2: info->state = 'R'; break;
    case 4: info->state = 'T'; break;
    case 5: info->state = 'Z'; break;
    default: info->state = 'S'; break;
    }
    snprintf(info->name, sizeof info->name, "%s", bsd.pbi_comm);
    if (proc_pidinfo((int)pid, PROC_PIDTASKINFO, 0, &task, sizeof task) == (int)sizeof task) {
        info->resident_bytes = task.pti_resident_size;
        info->virtual_bytes = task.pti_virtual_size;
        info->user_time_ns = task.pti_total_user;
        info->system_time_ns = task.pti_total_system;
        if (task.pti_threadnum > 0) info->threads = (uint32_t)task.pti_threadnum;
    }
    return 1;
}

int hl_host_process_resource_read(hl_host_process_resource_snapshot *snapshot) {
    struct rlimit nofile;
    struct rlimit nproc;
    pid_t children[1024];
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
    if (hl_host_process_read(getpid(), &process)) snapshot->threads = (int32_t)process.threads;
    int descriptor_bytes = proc_pidinfo(getpid(), PROC_PIDLISTFDS, 0, NULL, 0);
    if (descriptor_bytes > 0) snapshot->open_descriptors = descriptor_bytes / (int)sizeof(struct proc_fdinfo);
    int child_count = proc_listchildpids(getpid(), children, (int)sizeof children);
    if (child_count >= 0) {
        snapshot->caller_children = child_count;
        snapshot->children_truncated = child_count >= (int)(sizeof children / sizeof children[0]);
    }
    return 1;
}

static uint32_t hl_macos_fd_kind(uint32_t kind) {
    if (kind == PROX_FDTYPE_VNODE) return HL_HOST_FD_FILE;
    if (kind == PROX_FDTYPE_PIPE) return HL_HOST_FD_PIPE;
    if (kind == PROX_FDTYPE_SOCKET) return HL_HOST_FD_SOCKET;
    return HL_HOST_FD_OTHER;
}

static void hl_macos_pipe_identity(int pid, int descriptor, hl_host_process_fd *entry) {
    struct pipe_fdinfo info;
    if (proc_pidfdinfo(pid, descriptor, PROC_PIDFDPIPEINFO, &info, sizeof info) != (int)sizeof info) return;
    uint64_t handle = info.pipeinfo.pipe_handle;
    uint64_t peer = info.pipeinfo.pipe_peerhandle;
    entry->stable_device = info.pipeinfo.pipe_stat.vst_dev;
    entry->stable_object = handle == 0 ? peer : peer == 0 ? handle : handle < peer ? handle : peer;
}

int hl_host_process_fds(int64_t pid, hl_host_process_fd *entries, size_t capacity, size_t *count) {
    int bytes;
    int received;
    struct proc_fdinfo *native;
    size_t total;
    hl_host_process_info process;
    if (count == NULL || pid <= 0 || pid > INT32_MAX || (capacity != 0 && entries == NULL)) return 0;
    if (!hl_host_process_read(pid, &process)) return 0;
    bytes = proc_pidinfo((int)pid, PROC_PIDLISTFDS, 0, NULL, 0);
    if (bytes <= 0) return 0;
    native = malloc((size_t)bytes);
    if (native == NULL) return 0;
    received = proc_pidinfo((int)pid, PROC_PIDLISTFDS, 0, native, bytes);
    if (received < 0) {
        free(native);
        return 0;
    }
    total = (size_t)received / sizeof *native;
    for (size_t index = 0; index < total && index < capacity; ++index) {
        entries[index].descriptor = native[index].proc_fd;
        entries[index].kind = hl_macos_fd_kind(native[index].proc_fdtype);
        entries[index].flags = hl_host_process_fd_private_is(pid, process.start_time_ns, native[index].proc_fd)
                                   ? HL_HOST_PROCESS_FD_ENGINE_PRIVATE
                                   : 0;
        entries[index].reserved = 0;
        entries[index].stable_device = 0;
        entries[index].stable_object = 0;
        if (entries[index].kind == HL_HOST_FD_PIPE)
            hl_macos_pipe_identity((int)pid, native[index].proc_fd, &entries[index]);
    }
    free(native);
    *count = total;
    return 1;
}

/* Darwin's PROC_PIDLISTFDS sizing call is not the pathological one Linux's /proc readdir is, so this
 * keeps the size-then-list shape and over-allocates against the same open-in-between race the two-call
 * form always had. Callers see the single-pass contract either way. */
int hl_host_process_fds_collect(int64_t pid, hl_host_process_fd **entries, size_t *count) {
    hl_host_process_fd *listing;
    size_t need = 0;
    size_t capacity;
    size_t got = 0;
    if (entries == NULL || count == NULL) return 0;
    *entries = NULL;
    *count = 0;
    if (!hl_host_process_fds(pid, NULL, 0, &need)) return 0;
    capacity = need <= SIZE_MAX - 32 ? need + 32 : need;
    if (capacity > SIZE_MAX / sizeof *listing) return 0;
    listing = capacity != 0 ? malloc(capacity * sizeof *listing) : NULL;
    if (capacity != 0 && listing == NULL) return 0;
    if (!hl_host_process_fds(pid, listing, capacity, &got) || got > capacity) {
        free(listing);
        return 0;
    }
    *entries = listing;
    *count = got;
    return 1;
}

int hl_host_process_fd_read(int64_t pid, int32_t descriptor, hl_host_process_fd *entry, char *path,
                            size_t path_capacity, size_t *path_size) {
    struct vnode_fdinfowithpath info;
    hl_host_process_fd *entries;
    size_t count, capacity;
    uint32_t kind = HL_HOST_FD_OTHER;
    int found = 0;
    hl_host_process_info process;
    if (entry == NULL || path_size == NULL || pid <= 0 || descriptor < 0 || (path_capacity != 0 && path == NULL) ||
        pid > INT32_MAX)
        return 0;
    if (!hl_host_process_read(pid, &process)) return 0;
    if (proc_pidfdinfo((int)pid, descriptor, PROC_PIDFDVNODEPATHINFO, &info, sizeof info) == (int)sizeof info &&
        info.pvip.vip_path[0] != '\0') {
        size_t length = strnlen(info.pvip.vip_path, sizeof info.pvip.vip_path);
        if (length > path_capacity) length = path_capacity;
        if (length != 0) memcpy(path, info.pvip.vip_path, length);
        entry->descriptor = descriptor;
        entry->kind = HL_HOST_FD_FILE;
        entry->flags = hl_host_process_fd_private_is(pid, process.start_time_ns, descriptor)
                           ? HL_HOST_PROCESS_FD_ENGINE_PRIVATE
                           : 0;
        entry->reserved = 0;
        entry->stable_device = info.pvip.vip_vi.vi_stat.vst_dev;
        entry->stable_object = info.pvip.vip_vi.vi_stat.vst_ino;
        *path_size = length;
        return 1;
    }
    if (!hl_host_process_fds(pid, NULL, 0, &count)) return 0;
    capacity = count;
    entries = capacity != 0 ? malloc(capacity * sizeof *entries) : NULL;
    if (capacity != 0 && entries == NULL) return 0;
    if (!hl_host_process_fds(pid, entries, capacity, &count)) {
        free(entries);
        return 0;
    }
    if (count > capacity) count = capacity;
    for (size_t index = 0; index < count; ++index)
        if (entries[index].descriptor == descriptor) {
            kind = entries[index].kind;
            found = 1;
            break;
        }
    free(entries);
    if (!found) return 0;
    entry->descriptor = descriptor;
    entry->kind = kind;
    entry->flags =
        hl_host_process_fd_private_is(pid, process.start_time_ns, descriptor) ? HL_HOST_PROCESS_FD_ENGINE_PRIVATE : 0;
    entry->reserved = 0;
    entry->stable_device = 0;
    entry->stable_object = 0;
    if (entry->kind == HL_HOST_FD_PIPE) hl_macos_pipe_identity((int)pid, descriptor, entry);
    *path_size = 0;
    return 1;
}

int hl_host_process_peers(hl_host_process_peer *entries, size_t capacity, size_t *count) {
    char self_path[PROC_PIDPATHINFO_MAXSIZE];
    struct kinfo_proc *processes;
    int mib[3] = {CTL_KERN, KERN_PROC, KERN_PROC_ALL};
    pid_t self = getpid();
    pid_t session = getsid(0);
    size_t bytes = 0;
    size_t total = 0;
    if (count == NULL || (capacity != 0 && entries == NULL) || proc_pidpath(self, self_path, sizeof self_path) <= 0 ||
        session < 0)
        return 0;
    if (sysctl(mib, 3, NULL, &bytes, NULL, 0) != 0 || bytes == 0) return 0;
    bytes += 16 * sizeof *processes;
    processes = malloc(bytes);
    if (processes == NULL) return 0;
    if (sysctl(mib, 3, processes, &bytes, NULL, 0) != 0) {
        free(processes);
        return 0;
    }
    for (size_t index = 0; index < bytes / sizeof *processes; ++index) {
        char path[PROC_PIDPATHINFO_MAXSIZE];
        pid_t pid = processes[index].kp_proc.p_pid;
        if (pid <= 0 || pid == self || getsid(pid) != session || proc_pidpath(pid, path, sizeof path) <= 0 ||
            strcmp(path, self_path) != 0)
            continue;
        if (total < capacity) entries[total].identity = pid;
        total++;
    }
    free(processes);
    *count = total;
    return 1;
}

int hl_host_process_interrupt(hl_host_process_peer peer) {
    return peer.identity > 0 && peer.identity <= INT32_MAX && kill((pid_t)peer.identity, SIGINFO) == 0;
}
