#include "clock.h"

#include <errno.h>

int hl_production_clock_gettime(const hl_host_services *services, int clock_id, struct timespec *output) {
    uint64_t nanoseconds;
    if (output == NULL || hl_production_clock_nanoseconds(services, clock_id, &nanoseconds) != 0) return -1;
    output->tv_sec = (time_t)(nanoseconds / UINT64_C(1000000000));
    output->tv_nsec = (long)(nanoseconds % UINT64_C(1000000000));
    return 0;
}

int hl_production_clock_nanoseconds(const hl_host_services *services, int clock_id, uint64_t *output) {
    hl_host_result result;
    if (services == NULL || output == NULL || services->clock == NULL) {
        errno = EINVAL;
        return -1;
    }
    if (clock_id == HL_PRODUCTION_CLOCK_MONOTONIC)
        result = services->clock->monotonic_ns(services->context);
    else if (clock_id == HL_PRODUCTION_CLOCK_REALTIME)
        result = services->clock->realtime_ns(services->context);
    else if (clock_id == HL_PRODUCTION_CLOCK_RAW_MONOTONIC)
        result = services->clock->raw_monotonic_ns(services->context);
    else if (clock_id == HL_PRODUCTION_CLOCK_PROCESS_CPU)
        result = services->clock->process_cpu_ns(services->context);
    else if (clock_id == HL_PRODUCTION_CLOCK_THREAD_CPU)
        result = services->clock->thread_cpu_ns(services->context);
    else {
        errno = EINVAL;
        return -1;
    }
    if (result.status != HL_STATUS_OK) {
        errno = result.status == HL_STATUS_INVALID_ARGUMENT ? EINVAL : EIO;
        return -1;
    }
    *output = result.value;
    return 0;
}

int hl_production_clock_sleep_until(const hl_host_services *services, int clock_id, uint64_t deadline_ns) {
    hl_host_result result;
    uint32_t kind;
    if (services == NULL || services->clock == NULL || services->clock->sleep_until == NULL) {
        errno = EINVAL;
        return -1;
    }
    switch (clock_id) {
    case HL_PRODUCTION_CLOCK_MONOTONIC: kind = HL_HOST_CLOCK_MONOTONIC; break;
    case HL_PRODUCTION_CLOCK_REALTIME: kind = HL_HOST_CLOCK_REALTIME; break;
    case HL_PRODUCTION_CLOCK_RAW_MONOTONIC: kind = HL_HOST_CLOCK_RAW_MONOTONIC; break;
    case HL_PRODUCTION_CLOCK_PROCESS_CPU: kind = HL_HOST_CLOCK_PROCESS_CPU; break;
    default: errno = EINVAL; return -1;
    }
    result = services->clock->sleep_until(services->context, kind, deadline_ns);
    if (result.status == HL_STATUS_OK) return 0;
    if (result.status == HL_STATUS_INTERRUPTED)
        errno = EINTR;
    else if (result.status == HL_STATUS_INVALID_ARGUMENT || result.status == HL_STATUS_NOT_SUPPORTED)
        errno = EINVAL;
    else
        errno = EIO;
    return -1;
}
