/*
 * The host system/process snapshot group on Windows.
 *
 * What this group answers on the POSIX hosts is "what does /proc (or the Mach
 * task port) say about this machine and this process". The machine half has a
 * direct Windows answer and is REAL below. The per-process half does not, and
 * the reason is worth stating precisely rather than being filed under "not
 * ported yet", because it does not go away with effort:
 *
 *   - hl_host_process_read() wants a parent pid, a process group, a session, a
 *     resident and virtual size, user and system CPU time, a start time, a
 *     thread count and a state character, FOR AN ARBITRARY PID. Windows can
 *     supply most of those for a process it can open, but it has no process
 *     GROUP and no SESSION in the POSIX sense at all -- a Windows session is a
 *     login/desktop session, an entirely different object that would be a
 *     wrong answer rather than an approximation. The two fields exist here
 *     because the guest's /proc/[pid]/stat has them.
 *
 *   - hl_host_process_fds() and hl_host_process_fd_read() enumerate another
 *     process's open descriptors. There is no supported API for that. The
 *     handle table is reachable only through NtQuerySystemInformation's
 *     SystemHandleInformation, an undocumented interface whose record layout
 *     has changed between Windows releases, and even then a HANDLE is not a
 *     descriptor -- the CRT's fd-to-HANDLE mapping is private to the target
 *     process's own CRT instance. This is the same gap host_mman.h records for
 *     a file-backed mmap, one level further out.
 *
 * So those three are REFUSALS: they report "no such process / nothing
 * enumerated" rather than inventing a plausible process. A caller that acts on
 * a fabricated /proc entry -- a guest ps, a container's pid reaper -- gets a
 * consistent lie instead of an absence, and an absence is what a pid outside
 * the container looks like on Linux too.
 */

#include "../system.h"
#include "win32.h"

#include <string.h>

/*
 * REAL. GlobalMemoryStatusEx and GetSystemTimes are the machine-wide answers,
 * and both are exact rather than derived.
 *
 * Two mappings need stating because the vocabulary differs:
 *   - "free" and "available" are the same number here. Windows reports
 *     ullAvailPhys, which already accounts for reclaimable standby pages -- it
 *     is MemAvailable's meaning, not MemFree's. Reporting a smaller MemFree
 *     would require the standby list size, which needs the same undocumented
 *     interface the process half refuses to use.
 *   - the tick unit is Linux's USER_HZ (100 Hz), because the caller feeds these
 *     straight into /proc/stat, whose unit the guest assumes. Windows reports
 *     100-nanosecond units, so the conversion is a divide by 100000.
 *
 * GetSystemTimes' idle time is INCLUDED in its kernel time, which is the one
 * place a naive mapping is wrong: kernel time must have idle subtracted or
 * every core reads as fully busy in system mode.
 */
static uint64_t hl_windows_filetime_ticks(const FILETIME *time) {
    uint64_t raw = ((uint64_t)time->dwHighDateTime << 32) | (uint64_t)time->dwLowDateTime;
    return raw / UINT64_C(100000); /* 100ns units -> 1/100 s */
}

int hl_host_system_read(hl_host_system_info *info, hl_host_cpu_ticks *cores, size_t core_capacity) {
    MEMORYSTATUSEX memory;
    SYSTEM_INFO system;
    FILETIME idle, kernel, user;
    ULONGLONG uptime_ms;
    if (info == NULL) return 0;
    memset(info, 0, sizeof(*info));

    memory.dwLength = sizeof(memory);
    if (!GlobalMemoryStatusEx(&memory)) return 0;
    info->memory_total = (uint64_t)memory.ullTotalPhys;
    info->memory_free = (uint64_t)memory.ullAvailPhys;
    info->memory_available = (uint64_t)memory.ullAvailPhys;
    info->memory_cached = 0;

    GetSystemInfo(&system);
    info->online_cpus = (uint32_t)system.dwNumberOfProcessors;
    info->reported_cores = (uint32_t)system.dwNumberOfProcessors;

    /* Boot time as a wall-clock second, from "now minus uptime". The interrupt
     * time count is the monotonic one and does not move when the clock is
     * stepped, so the subtraction is stable across a time change. */
    uptime_ms = GetTickCount64();
    {
        FILETIME now;
        uint64_t now_100ns;
        GetSystemTimeAsFileTime(&now);
        now_100ns = ((uint64_t)now.dwHighDateTime << 32) | (uint64_t)now.dwLowDateTime;
        /* FILETIME epoch is 1601-01-01; Unix epoch is 11644473600 seconds later. */
        if (now_100ns >= UINT64_C(116444736000000000)) {
            uint64_t unix_seconds = (now_100ns - UINT64_C(116444736000000000)) / UINT64_C(10000000);
            uint64_t uptime_seconds = (uint64_t)uptime_ms / UINT64_C(1000);
            info->boot_time_seconds = unix_seconds > uptime_seconds ? unix_seconds - uptime_seconds : 0;
        }
    }

    if (GetSystemTimes(&idle, &kernel, &user)) {
        uint64_t idle_ticks = hl_windows_filetime_ticks(&idle);
        uint64_t kernel_ticks = hl_windows_filetime_ticks(&kernel);
        info->aggregate.user = hl_windows_filetime_ticks(&user);
        info->aggregate.nice = 0; /* Windows has no separate low-priority accounting bucket. */
        info->aggregate.system = kernel_ticks > idle_ticks ? kernel_ticks - idle_ticks : 0;
        info->aggregate.idle = idle_ticks;
    }

    /* Per-core counters need NtQuerySystemInformation's
     * SystemProcessorPerformanceInformation. Zeroing rather than repeating the
     * aggregate: a caller that divides the aggregate by the core count gets a
     * number that looks measured and is not. */
    if (cores != NULL && core_capacity > 0) memset(cores, 0, core_capacity * sizeof(*cores));
    return 1;
}

/* REFUSAL -- see the header note. Zero is "no such process", which is what a
 * pid outside the container reads as on every host. */
int hl_host_process_read(int64_t pid, hl_host_process_info *info) {
    (void)pid;
    if (info != NULL) memset(info, 0, sizeof(*info));
    return 0;
}

/* REFUSAL. Reports zero descriptors AND fails, so a caller cannot read the
 * empty enumeration as "this process has nothing open". */
int hl_host_process_fds(int64_t pid, hl_host_process_fd *entries, size_t capacity, size_t *count) {
    (void)pid;
    (void)entries;
    (void)capacity;
    if (count != NULL) *count = 0;
    return 0;
}

int hl_host_process_fd_read(int64_t pid, int32_t descriptor, hl_host_process_fd *entry, char *path,
                            size_t path_capacity, size_t *path_size) {
    (void)pid;
    (void)descriptor;
    (void)path;
    (void)path_capacity;
    if (entry != NULL) memset(entry, 0, sizeof(*entry));
    if (path_size != NULL) *path_size = 0;
    return 0;
}

/*
 * REFUSAL. Peer enumeration exists so several engine instances in one session
 * can find each other and interrupt one another at a safepoint. Both halves
 * need the process-enumeration surface refused above, and the interrupt half
 * additionally needs a cross-process signalling mechanism this host has not
 * chosen yet -- a named event or an APC, not a signal.
 */
int hl_host_process_peers(hl_host_process_peer *entries, size_t capacity, size_t *count) {
    (void)entries;
    (void)capacity;
    if (count != NULL) *count = 0;
    return 0;
}

int hl_host_process_interrupt(hl_host_process_peer peer) {
    (void)peer;
    return 0;
}
