    struct timespec remaining;
    (void)context;
    remaining.tv_sec = (time_t)(interval_ns / UINT64_C(1000000000));
    remaining.tv_nsec = (long)(interval_ns % UINT64_C(1000000000));
    while (nanosleep(&remaining, &remaining) != 0) {
        if (errno != EINTR) return hl_macos_errno();
    }
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static void hl_macos_precise_sleep_begin(void) {
    mach_timebase_info_data_t timebase;
    thread_time_constraint_policy_data_t policy;
    double nanoseconds_to_ticks;
    if (mach_timebase_info(&timebase) != KERN_SUCCESS || timebase.numer == 0) return;
    nanoseconds_to_ticks = (double)timebase.denom / (double)timebase.numer;
    policy.period = (uint32_t)(500000.0 * nanoseconds_to_ticks);
    policy.computation = (uint32_t)(100000.0 * nanoseconds_to_ticks);
    policy.constraint = (uint32_t)(500000.0 * nanoseconds_to_ticks);
    policy.preemptible = 1;
    (void)thread_policy_set(mach_thread_self(), THREAD_TIME_CONSTRAINT_POLICY, (thread_policy_t)&policy,
                            THREAD_TIME_CONSTRAINT_POLICY_COUNT);
}

static void hl_macos_precise_sleep_end(void) {
    thread_standard_policy_data_t policy = {0};
    (void)thread_policy_set(mach_thread_self(), THREAD_STANDARD_POLICY, (thread_policy_t)&policy,
                            THREAD_STANDARD_POLICY_COUNT);
}

static hl_host_result hl_macos_clock_sleep_until(void *context, uint32_t clock_kind, uint64_t deadline_ns) {
    clockid_t clock_id;
    struct timespec now, delay;
    uint64_t now_ns, remaining;
    (void)context;
    switch (clock_kind) {
    case HL_HOST_CLOCK_MONOTONIC: clock_id = CLOCK_MONOTONIC; break;
    case HL_HOST_CLOCK_REALTIME: clock_id = CLOCK_REALTIME; break;
    case HL_HOST_CLOCK_PROCESS_CPU: clock_id = CLOCK_PROCESS_CPUTIME_ID; break;
    default: return hl_macos_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
    for (;;) {
        if (clock_gettime(clock_id, &now) != 0) return hl_macos_errno();
        now_ns = (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
        if (now_ns >= deadline_ns) return hl_macos_result(HL_STATUS_OK, 0, 0);
        remaining = deadline_ns - now_ns;
        /* Recheck non-monotonic clocks periodically so realtime adjustments and process-CPU progress
         * change the effective absolute deadline instead of becoming one stale wall-clock delay. */
        if (clock_kind != HL_HOST_CLOCK_MONOTONIC && remaining > UINT64_C(10000000)) remaining = UINT64_C(10000000);
        delay.tv_sec = (time_t)(remaining / UINT64_C(1000000000));
        delay.tv_nsec = (long)(remaining % UINT64_C(1000000000));
        /* Match Linux high-resolution timer wakeups without leaking a Darwin scheduler policy into linux_abi. */
        hl_macos_precise_sleep_begin();
        if (nanosleep(&delay, NULL) != 0) {
            hl_host_result result = hl_macos_errno();
            hl_macos_precise_sleep_end();
            return result;
        }
        hl_macos_precise_sleep_end();
        if (clock_kind == HL_HOST_CLOCK_MONOTONIC) return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
}

