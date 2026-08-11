#ifndef HL_PRODUCTION_HOST_CLOCK_H
#define HL_PRODUCTION_HOST_CLOCK_H

#include "hl/host_services.h"

#include <time.h>

enum {
    HL_PRODUCTION_CLOCK_REALTIME = 1,
    HL_PRODUCTION_CLOCK_MONOTONIC = 2,
    HL_PRODUCTION_CLOCK_RAW_MONOTONIC = 3,
    HL_PRODUCTION_CLOCK_PROCESS_CPU = 4,
    HL_PRODUCTION_CLOCK_THREAD_CPU = 5
};

int hl_production_clock_gettime(const hl_host_services *services, int clock_id, struct timespec *output);
int hl_production_clock_nanoseconds(const hl_host_services *services, int clock_id, uint64_t *output);
int hl_production_clock_sleep_until(const hl_host_services *services, int clock_id, uint64_t deadline_ns);

#endif
