static hl_host_result hl_linux_clock(int clock_id) {
    struct timespec time;
    if (clock_gettime(clock_id, &time) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, (uint64_t)time.tv_sec * UINT64_C(1000000000) + (uint64_t)time.tv_nsec, 0);
}

static hl_host_result hl_linux_monotonic(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_MONOTONIC);
}

static hl_host_result hl_linux_realtime(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_REALTIME);
}

static hl_host_result hl_linux_raw_monotonic(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_MONOTONIC_RAW);
}

static hl_host_result hl_linux_process_cpu(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_PROCESS_CPUTIME_ID);
}

static hl_host_result hl_linux_thread_cpu(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_THREAD_CPUTIME_ID);
}

/* Reports the tick RATE of the CPU's free-running counter, which the translator bakes into a tick->ns
 * multiplier to serve guest clock reads inline.  x86-64's TSC frequency is not architecturally readable
 * (CPUID.15H is absent or lies on most parts) and is not calibrated here; NOT_SUPPORTED is a first-class
 * answer, and s1_calibrate then clears its fast-clock flag. */
static hl_host_result hl_linux_architectural_counter(void *context) {
    (void)context;
#if defined(HL_HOST_CPU_AARCH64)
    uint64_t frequency;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(frequency));
    if (frequency != 0) return hl_linux_result(HL_STATUS_OK, frequency, 0);
#endif
    return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

static hl_host_result hl_linux_backoff(void *context, uint64_t interval_ns) {
    struct timespec remaining;
    (void)context;
    remaining.tv_sec = (time_t)(interval_ns / UINT64_C(1000000000));
    remaining.tv_nsec = (long)(interval_ns % UINT64_C(1000000000));
    while (nanosleep(&remaining, &remaining) != 0) {
        if (errno != EINTR) return hl_linux_errno_result();
    }
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_clock_sleep_until(void *context, uint32_t clock_kind, uint64_t deadline_ns) {
    clockid_t clock_id;
    struct timespec deadline;
    int error;
    (void)context;
    switch (clock_kind) {
    case HL_HOST_CLOCK_MONOTONIC: clock_id = CLOCK_MONOTONIC; break;
    case HL_HOST_CLOCK_REALTIME: clock_id = CLOCK_REALTIME; break;
    case HL_HOST_CLOCK_PROCESS_CPU: clock_id = CLOCK_PROCESS_CPUTIME_ID; break;
    default: return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
    deadline.tv_sec = (time_t)(deadline_ns / UINT64_C(1000000000));
    deadline.tv_nsec = (long)(deadline_ns % UINT64_C(1000000000));
    error = clock_nanosleep(clock_id, TIMER_ABSTIME, &deadline, NULL);
    if (error != 0) {
        errno = error;
        return hl_linux_errno_result();
    }
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

