/*
 * hl_host_clock_services on Win32.
 *
 * The cheapest group after log, but three of the nine rows have a real fidelity
 * note attached and they are recorded at the callback rather than in a summary.
 */
#include "internal.h"

/* 100 ns ticks between 1601-01-01 (the FILETIME epoch) and 1970-01-01. */
#define HL_WINDOWS_UNIX_EPOCH_TICKS UINT64_C(116444736000000000)

static uint64_t hl_windows_filetime_ticks(FILETIME time) {
    return ((uint64_t)time.dwHighDateTime << 32) | (uint64_t)time.dwLowDateTime;
}

/* Applied to durations, and to a wall-clock reading only after it has been
 * rebased to the Unix epoch: scaling a raw 1601-based FILETIME first would put a
 * present-day timestamp within a factor of two of overflowing 64 bits. */
static uint64_t hl_windows_ticks_ns(uint64_t ticks) {
    return ticks * UINT64_C(100);
}

/* counter * 1e9 / frequency without the overflow that the obvious spelling takes
 * once the counter passes a few hundred seconds at 10 MHz. */
static uint64_t hl_windows_scale_ticks(uint64_t counter, uint64_t frequency) {
    if (frequency == 0) return 0;
    return (counter / frequency) * UINT64_C(1000000000) + (counter % frequency) * UINT64_C(1000000000) / frequency;
}

static uint64_t hl_windows_performance_frequency(void) {
    LARGE_INTEGER frequency;
    if (!QueryPerformanceFrequency(&frequency) || frequency.QuadPart <= 0) return 0;
    return (uint64_t)frequency.QuadPart;
}

static uint64_t hl_windows_performance_counter(void) {
    LARGE_INTEGER counter;
    if (!QueryPerformanceCounter(&counter) || counter.QuadPart < 0) return 0;
    return (uint64_t)counter.QuadPart;
}

/* CLOCK_MONOTONIC excludes time the machine spent suspended, and the unbiased
 * interrupt time is the only Windows clock that also excludes it.
 * QueryPerformanceCounter would be wrong across sleep on some HALs. */
static uint64_t hl_windows_unbiased_ns(const hl_windows_kernelbase *api) {
    ULONGLONG ticks = 0;
    if (api->query_unbiased_interrupt_time_precise != NULL) {
        api->query_unbiased_interrupt_time_precise(&ticks);
        return (uint64_t)ticks * UINT64_C(100);
    }
    if (api->query_unbiased_interrupt_time != NULL && api->query_unbiased_interrupt_time(&ticks))
        return (uint64_t)ticks * UINT64_C(100);
    /* Last resort: the tick count is also unbiased, at ~15.6 ms resolution. */
    return (uint64_t)GetTickCount64() * UINT64_C(1000000);
}

static hl_host_result hl_windows_clock_monotonic(void *context) {
    hl_host_windows *host = context;
    return hl_windows_result(HL_STATUS_OK, hl_windows_unbiased_ns(&host->api), 0);
}

/* 100 ns resolution rather than 1 ns, which a guest comparing consecutive
 * clock_gettime results can observe. */
static hl_host_result hl_windows_clock_realtime(void *context) {
    FILETIME now;
    uint64_t ticks;
    (void)context;
    GetSystemTimePreciseAsFileTime(&now);
    ticks = hl_windows_filetime_ticks(now);
    /* A system clock set before 1970 cannot be expressed by the unsigned
     * nanosecond result the contract returns. */
    if (ticks < HL_WINDOWS_UNIX_EPOCH_TICKS) return hl_windows_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return hl_windows_result(HL_STATUS_OK, hl_windows_ticks_ns(ticks - HL_WINDOWS_UNIX_EPOCH_TICKS), 0);
}

/* CLOCK_MONOTONIC_RAW is "undisciplined by NTP", and that is exactly what the
 * performance counter is. */
static hl_host_result hl_windows_clock_raw_monotonic(void *context) {
    const uint64_t frequency = hl_windows_performance_frequency();
    (void)context;
    if (frequency == 0) return hl_windows_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return hl_windows_result(HL_STATUS_OK, hl_windows_scale_ticks(hl_windows_performance_counter(), frequency), 0);
}

static hl_host_result hl_windows_clock_process_cpu(void *context) {
    FILETIME created;
    FILETIME exited;
    FILETIME kernel;
    FILETIME user;
    (void)context;
    if (!GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user)) return hl_windows_last_error_result();
    return hl_windows_result(
        HL_STATUS_OK, hl_windows_ticks_ns(hl_windows_filetime_ticks(kernel) + hl_windows_filetime_ticks(user)), 0);
}

/* Real accuracy loss, and unavoidable: GetThreadTimes accumulates at scheduler
 * tick granularity (~15.6 ms by default) against Linux's nanosecond accounting.
 * QueryThreadCycleTime is precise but reports cycles, and cycles->ns needs a TSC
 * frequency this host cannot read. The coarse value is reported rather than a
 * fabricated precise one. */
static hl_host_result hl_windows_clock_thread_cpu(void *context) {
    FILETIME created;
    FILETIME exited;
    FILETIME kernel;
    FILETIME user;
    (void)context;
    if (!GetThreadTimes(GetCurrentThread(), &created, &exited, &kernel, &user)) return hl_windows_last_error_result();
    return hl_windows_result(
        HL_STATUS_OK, hl_windows_ticks_ns(hl_windows_filetime_ticks(kernel) + hl_windows_filetime_ticks(user)), 0);
}

static hl_host_result hl_windows_clock_sleep_until(void *context, uint32_t clock_kind, uint64_t deadline_ns) {
    hl_host_windows *host = context;
    LARGE_INTEGER due;
    HANDLE timer;
    DWORD waited;
    switch (clock_kind) {
    case HL_HOST_CLOCK_REALTIME:
        /* SetWaitableTimer's positive due time IS system time, so it tracks clock
         * steps the way clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME) must. */
        due.QuadPart = (LONGLONG)(deadline_ns / UINT64_C(100) + HL_WINDOWS_UNIX_EPOCH_TICKS);
        break;
    case HL_HOST_CLOCK_MONOTONIC: {
        /* No absolute monotonic form exists, so the remaining interval is passed
         * as a negative (relative) due time. That loses immunity to a clock step
         * occurring during the sleep, and nothing else. */
        const uint64_t now = hl_windows_unbiased_ns(&host->api);
        if (now >= deadline_ns) return hl_windows_result(HL_STATUS_OK, 0, 0);
        due.QuadPart = -(LONGLONG)((deadline_ns - now) / UINT64_C(100) + 1u);
        break;
    }
    /* Windows has no CPU-time timer at all. */
    default: return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
    timer = CreateWaitableTimerExW(NULL, NULL, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
    if (timer == NULL) timer = CreateWaitableTimerExW(NULL, NULL, 0, TIMER_ALL_ACCESS);
    if (timer == NULL) return hl_windows_last_error_result();
    if (!SetWaitableTimer(timer, &due, 0, NULL, NULL, FALSE)) {
        hl_host_result failure = hl_windows_last_error_result();
        CloseHandle(timer);
        return failure;
    }
    /* A non-alertable wait cannot be interrupted, so HL_STATUS_INTERRUPTED --
     * the EINTR the contract mentions -- never occurs on this host. */
    waited = WaitForSingleObject(timer, INFINITE);
    CloseHandle(timer);
    if (waited != WAIT_OBJECT_0) return hl_windows_result(HL_STATUS_PLATFORM_FAILURE, 0, (uint64_t)waited);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* Reports the tick RATE of the CPU's free-running counter, which the translator
 * bakes into a tick->ns multiplier to serve guest clock reads inline. x86-64's
 * TSC frequency is not architecturally readable (CPUID.15H is absent or lies on
 * most parts) and is not calibrated here; NOT_SUPPORTED is a first-class answer
 * and the translator then clears its fast-clock flag. */
static hl_host_result hl_windows_clock_architectural_counter(void *context) {
    (void)context;
    return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

/*
 * The contract forbids allocation, locking and early return here, and demands
 * immutable state. That rules out a high-resolution waitable timer, which needs
 * a HANDLE and therefore either per-thread storage or a process-wide object no
 * two waiters could share. It also rules out timeBeginPeriod(1), whose effect is
 * process-wide and has power-consumption consequences that should be an explicit
 * engine decision rather than a side effect of a backoff call.
 *
 * So: spin below a millisecond, and above it sleep in coarse steps against a
 * performance-counter deadline, never returning before the deadline passes.
 */
static hl_host_result hl_windows_clock_backoff(void *context, uint64_t interval_ns) {
    const uint64_t frequency = hl_windows_performance_frequency();
    uint64_t deadline;
    (void)context;
    if (frequency == 0) return hl_windows_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (interval_ns == 0) return hl_windows_result(HL_STATUS_OK, 0, 0);
    deadline = hl_windows_performance_counter() + (interval_ns / UINT64_C(1000000000)) * frequency +
               (interval_ns % UINT64_C(1000000000)) * frequency / UINT64_C(1000000000);
    for (;;) {
        const uint64_t now = hl_windows_performance_counter();
        uint64_t remaining_ms;
        if (now >= deadline) return hl_windows_result(HL_STATUS_OK, 0, 0);
        remaining_ms = hl_windows_scale_ticks(deadline - now, frequency) / UINT64_C(1000000);
        if (remaining_ms == 0) {
            YieldProcessor();
            continue;
        }
        /* Sleep's granularity is ~15.6 ms, so it is only ever asked for the whole
         * millisecond remainder and the residue is spun out above. */
        Sleep((DWORD)(remaining_ms > UINT64_C(1000) ? UINT64_C(1000) : remaining_ms));
    }
}

const hl_host_clock_services hl_windows_clock_services = {.abi = HL_HOST_CLOCK_ABI,
                                                          .size = sizeof(hl_host_clock_services),
                                                          .monotonic_ns = hl_windows_clock_monotonic,
                                                          .realtime_ns = hl_windows_clock_realtime,
                                                          .raw_monotonic_ns = hl_windows_clock_raw_monotonic,
                                                          .process_cpu_ns = hl_windows_clock_process_cpu,
                                                          .thread_cpu_ns = hl_windows_clock_thread_cpu,
                                                          .sleep_until = hl_windows_clock_sleep_until,
                                                          .architectural_counter_hz =
                                                              hl_windows_clock_architectural_counter,
                                                          .backoff_ns = hl_windows_clock_backoff};
