// Opt-in, failure-only fork tracing. This is included after bound_fork_state so
// records can expose the exact prepared snapshot without exporting ABI-private
// state. Diagnostics preserve errno and never execute on the successful path.

typedef struct fork_diagnostic_route {
    const char *name;
    int worker_pid;
    int sentry_pid;
    int guest_children;
    int worker_threads;
    int ring;
} fork_diagnostic_route;

static _Thread_local fork_diagnostic_route g_fork_diagnostic_route = {
    .name = "local", .worker_pid = -1, .sentry_pid = -1, .guest_children = -1, .worker_threads = -1, .ring = -1};

static fork_diagnostic_route fork_diagnostic_route_enter(const char *name, int worker_pid, int sentry_pid,
                                                          int guest_children, int worker_threads, int ring) {
    fork_diagnostic_route previous = g_fork_diagnostic_route;
    g_fork_diagnostic_route = (fork_diagnostic_route){.name = name,
                                                       .worker_pid = worker_pid,
                                                       .sentry_pid = sentry_pid,
                                                       .guest_children = guest_children,
                                                       .worker_threads = worker_threads,
                                                       .ring = ring};
    return previous;
}

static void fork_diagnostic_route_leave(fork_diagnostic_route previous) {
    g_fork_diagnostic_route = previous;
}

static void fork_diagnostic_close_descriptor(int *descriptor) {
    if (*descriptor < 0) return;
    (void)close(*descriptor);
    *descriptor = -1;
}

static void fork_diagnostic_close_pair(int descriptors[2]) {
    fork_diagnostic_close_descriptor(&descriptors[0]);
    fork_diagnostic_close_descriptor(&descriptors[1]);
}

static int fork_diagnostic_pids_total(void) {
    if (g_acct == NULL) {
        int local = atomic_load_explicit(&g_pids_cur, memory_order_relaxed);
        return local > 0 ? local : 1;
    }
    int self = (int)getpid();
    int total = 0;
    for (int index = 0; index < HL_ACCT_SLOTS; ++index) {
        int process = atomic_load_explicit(&g_acct[index].pid, memory_order_relaxed);
        if (process == 0 || (process != self && !acct_pid_live(process))) continue;
        int tasks = atomic_load_explicit(&g_acct[index].tasks, memory_order_relaxed);
        total += tasks > 0 ? tasks : 1;
    }
    return total > 0 ? total : 1;
}

static void fork_diagnostic_emit(struct cpu *c, uint64_t nr, uint64_t flags, const char *stage, int failure,
                                 int pids_total, const bound_fork_state *state) {
    if (hl_option_get("HL_C_DIAGNOSTICS") == NULL) return;
    int saved_errno = errno;
    if (pids_total < 0) pids_total = fork_diagnostic_pids_total();
    hl_host_process_resource_snapshot host_snapshot;
    int host_snapshot_status = hl_host_process_resource_read(&host_snapshot);
    if (!host_snapshot_status) {
        memset(&host_snapshot, 0, sizeof host_snapshot);
        host_snapshot.nofile_status = -1;
        host_snapshot.nproc_status = -1;
        host_snapshot.open_descriptors = -1;
        host_snapshot.threads = -1;
        host_snapshot.caller_children = -1;
    }
    int local_tasks = atomic_load_explicit(&g_pids_cur, memory_order_relaxed);
    uint32_t ofd_count = state != NULL ? state->plan.count : 0;
    uint32_t ofd_capacity = state != NULL ? state->plan.capacity : 0;
    size_t watch_count = state != NULL ? state->watch_plan.count : 0;
    size_t fdvis_count = state != NULL ? state->fdvis_plan.count : 0;
    const char *snapshot_stage = state != NULL && state->diagnostic_stage != NULL ? state->diagnostic_stage : "none";
    uint32_t ofd_watermark = g_linux_box != NULL ? g_linux_box->ofd_watermark : 0;
    uint32_t reserved_fds = g_linux_box != NULL ? g_linux_box->reserved_fds : 0;
    size_t ofd_bytes = ofd_count <= SIZE_MAX / sizeof(hl_linux_fork_record)
                           ? (size_t)ofd_count * sizeof(hl_linux_fork_record)
                           : SIZE_MAX;
    size_t watch_bytes = watch_count <= SIZE_MAX / sizeof(hl_linux_watch_fork_record)
                             ? watch_count * sizeof(hl_linux_watch_fork_record)
                             : SIZE_MAX;
    size_t fdvis_bytes = state != NULL && fdvis_count <= SIZE_MAX / sizeof(*state->fdvis_plan.entries)
                             ? fdvis_count * sizeof(*state->fdvis_plan.entries)
                             : SIZE_MAX;
    size_t ofd_capacity_bytes = state != NULL && state->plan.capacity <= SIZE_MAX / sizeof(hl_linux_fork_record)
                                    ? (size_t)state->plan.capacity * sizeof(hl_linux_fork_record)
                                    : SIZE_MAX;
    size_t watch_capacity = state != NULL ? state->watch_plan.capacity : 0;
    size_t watch_capacity_bytes = watch_capacity <= SIZE_MAX / sizeof(hl_linux_watch_fork_record)
                                      ? watch_capacity * sizeof(hl_linux_watch_fork_record)
                                      : SIZE_MAX;
    char line[1536];
    int length = snprintf(
        line, sizeof line,
        "hl-fork-failure: stage=%s result_errno=%d ambient_errno=%d syscall=%llu flags=%#llx guest_pc=%#llx guest_sp=%#llx "
        "guest_tid=%d host_pid=%d host_ppid=%d route=%s worker_pid=%d sentry_pid=%d guest_children=%d "
        "worker_threads=%d ring=%d host_snapshot_status=%d host_threads=%d host_children=%d children_truncated=%d "
        "local_tasks=%d pids_total=%d pids_max=%llu open_fds=%d "
        "nofile_cur=%llu nofile_max=%llu nofile_status=%d nproc_cur=%llu nproc_max=%llu nproc_status=%d "
        "mem_charged=%llu mem_max=%llu snapshot_stage=%s ofd_count=%u ofd_bytes=%zu ofd_capacity=%u "
        "ofd_capacity_bytes=%zu ofd_watermark=%u reserved_fds=%u watch_count=%zu watch_bytes=%zu "
        "watch_capacity=%zu watch_capacity_bytes=%zu fdvis_count=%zu fdvis_bytes=%zu "
        "watch_prepared=%d private_prepared=%d fdvis_prepared=%d seq_prepared=%d\n",
        stage, failure, saved_errno, (unsigned long long)nr, (unsigned long long)flags, (unsigned long long)G_PC(c),
        (unsigned long long)G_SP(c), cpu_tid(c), (int)getpid(), (int)getppid(), g_fork_diagnostic_route.name,
        g_fork_diagnostic_route.worker_pid, g_fork_diagnostic_route.sentry_pid,
        g_fork_diagnostic_route.guest_children, g_fork_diagnostic_route.worker_threads,
        g_fork_diagnostic_route.ring, host_snapshot_status, host_snapshot.threads, host_snapshot.caller_children,
        host_snapshot.children_truncated, local_tasks, pids_total, (unsigned long long)g_pids_max,
        host_snapshot.open_descriptors, (unsigned long long)host_snapshot.nofile_current,
        (unsigned long long)host_snapshot.nofile_maximum, host_snapshot.nofile_status,
        (unsigned long long)host_snapshot.nproc_current, (unsigned long long)host_snapshot.nproc_maximum,
        host_snapshot.nproc_status,
        (unsigned long long)atomic_load_explicit(&g_mem_charged, memory_order_relaxed),
        (unsigned long long)g_mem_max, snapshot_stage, ofd_count, ofd_bytes, ofd_capacity, ofd_capacity_bytes,
        ofd_watermark, reserved_fds, watch_count, watch_bytes, watch_capacity, watch_capacity_bytes, fdvis_count,
        fdvis_bytes, state != NULL && state->watch_prepared,
        state != NULL && state->private_prepared, state != NULL && state->fdvis_prepared,
        state != NULL && state->seq_prepared);
    if (length > 0) {
        size_t size = (size_t)length < sizeof line ? (size_t)length : sizeof line - 1;
        ssize_t written;
        do
            written = write(STDERR_FILENO, line, size);
        while (written < 0 && errno == EINTR);
        (void)written;
    }
    errno = saved_errno;
}
