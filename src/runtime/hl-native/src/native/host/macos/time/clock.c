static hl_host_result hl_macos_clock(clockid_t clock_id) {
    struct timespec value;
    if (clock_gettime(clock_id, &value) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec, 0);
}

static hl_host_result hl_macos_monotonic(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_MONOTONIC);
}

static hl_host_result hl_macos_realtime(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_REALTIME);
}

static hl_host_result hl_macos_raw_monotonic(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_MONOTONIC_RAW);
}

static hl_host_result hl_macos_process_cpu(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_PROCESS_CPUTIME_ID);
}

static hl_host_result hl_macos_thread_cpu(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_THREAD_CPUTIME_ID);
}

static hl_host_result hl_macos_architectural_counter(void *context) {
    mach_timebase_info_data_t timebase = {0, 0};
    uint64_t frequency;
    (void)context;
    if (mach_timebase_info(&timebase) != KERN_SUCCESS || timebase.numer == 0 || timebase.denom == 0)
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    frequency = (uint64_t)(((unsigned __int128)UINT64_C(1000000000) * timebase.denom) / timebase.numer);
    if (frequency == 0) return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return hl_macos_result(HL_STATUS_OK, frequency, 0);
}

static hl_host_result hl_macos_backoff(void *context, uint64_t interval_ns) {
