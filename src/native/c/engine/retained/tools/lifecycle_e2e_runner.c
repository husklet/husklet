#include "hl/engine.h"
#include "hl/log.h"
#if defined(HL_TEST_HOST_LINUX)
#include "hl/linux.h"
#include "../src/host/linux/probe.h"
typedef hl_host_linux lifecycle_host;
#define lifecycle_host_create hl_host_linux_create
#define lifecycle_host_destroy hl_host_linux_destroy
#define lifecycle_active_mappings hl_host_linux_active_mappings
#else
#include "hl/macos.h"
#include "../src/host/macos/probe.h"
typedef hl_host_macos lifecycle_host;
#define lifecycle_host_create hl_host_macos_create
#define lifecycle_host_destroy hl_host_macos_destroy
#define lifecycle_active_mappings hl_host_macos_active_mappings
#endif

#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <sched.h>
#include <string.h>
#include <sys/mman.h>

#ifndef HL_LIFECYCLE_SCENARIO
#define HL_LIFECYCLE_SCENARIO "generic"
#endif
__attribute__((used)) static const char lifecycle_scenario_identity[] = HL_LIFECYCLE_SCENARIO;

#ifndef HL_TEST_GUEST_ISA
#error HL_TEST_GUEST_ISA is required
#endif

typedef struct lifecycle_run {
    hl_engine *engine;
    int argc;
    const char *const *argv;
    hl_engine_exit *result;
    hl_status status;
} lifecycle_run;

static const hl_host_clock_services *native_clock;
static uint32_t *clock_calls;
static uint32_t *realtime_calls;
static int injected_clock;
static const hl_host_memory_services *native_memory;
static uint32_t fatal_events;
static uint32_t fatal_tag;
static char fatal_message[96];
static const hl_host_stream_services *native_stream;
static int result_fault;

static hl_host_result fail_publish(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size) {
    (void)context;
    (void)mapping;
    (void)offset;
    (void)size;
    return (hl_host_result){HL_STATUS_PLATFORM_FAILURE, 0, 0, 0};
}

static void capture_fatal(void *context, uint32_t tag, const char *message, size_t size) {
    size_t copy = size < sizeof(fatal_message) - 1u ? size : sizeof(fatal_message) - 1u;
    (void)context;
    fatal_events++;
    fatal_tag = tag;
    memcpy(fatal_message, message, copy);
    fatal_message[copy] = 0;
}

static hl_host_result result_write(void *context, hl_host_handle stream, hl_host_const_bytes input) {
    if (result_fault == 1) return (hl_host_result){HL_STATUS_OK, 0, input.size, 0};
    if (result_fault == 2) {
        size_t half = input.size / 2u;
        hl_host_result written = native_stream->write(context, stream, (hl_host_const_bytes){input.data, half});
        return written.status == HL_STATUS_OK && written.value == half
                   ? (hl_host_result){HL_STATUS_OK, 0, input.size, 0}
                   : written;
    }
    if (result_fault == 3 && input.size != 0) {
        unsigned char copy[64];
        size_t size = input.size < sizeof(copy) ? input.size : sizeof(copy);
        memcpy(copy, input.data, size);
        copy[0] ^= 0xffu;
        return native_stream->write(context, stream, (hl_host_const_bytes){copy, size});
    }
    return native_stream->write(context, stream, input);
}

static hl_host_result spy_monotonic_ns(void *context) {
    (void)__atomic_add_fetch(clock_calls, UINT32_C(1), __ATOMIC_RELAXED);
    return native_clock->monotonic_ns(context);
}

static hl_host_result spy_realtime_ns(void *context) {
    (void)__atomic_add_fetch(realtime_calls, UINT32_C(1), __ATOMIC_RELAXED);
    if (injected_clock) return (hl_host_result){HL_STATUS_OK, 0, UINT64_C(123456789987654321), 0};
    return native_clock->realtime_ns(context);
}

static void *run_guest(void *opaque) {
    lifecycle_run *run = opaque;
    run->status = hl_engine_run(run->engine, run->argc, run->argv, run->result);
    return NULL;
}

int main(int argc, char **argv) {
    lifecycle_host *host = NULL;
    hl_host_services services;
    hl_host_clock_services clock;
    hl_host_memory_services memory;
    hl_host_log_services log;
    hl_host_stream_services stream;
    hl_engine_config config;
    hl_engine_exit result;
    hl_engine *engine = NULL;
    hl_status status;
    int force_stop = argc > 1 && strcmp(argv[1], "--force-stop") == 0;
    int fail_code_publish = argc > 1 && strcmp(argv[1], "--fail-publish") == 0;
    int bad_result = argc > 1 && strncmp(argv[1], "--result-", 9) == 0;
    int expect_exit = argc > 2 && strcmp(argv[1], "--expect-exit") == 0 ? atoi(argv[2]) : -1;
    int expect_signal = argc > 2 && strcmp(argv[1], "--expect-signal") == 0 ? atoi(argv[2]) : -1;
    int guest_index = 1;
    injected_clock = argc > 1 && strcmp(argv[1], "--clock-spy") == 0;
    if (injected_clock || fail_code_publish || bad_result) guest_index = 2;
    if (expect_exit >= 0 || expect_signal >= 0) guest_index = 3;
    if (argc < 2) {
        fprintf(stderr, "usage: lifecycle-e2e-runner GUEST [args...]\n");
        return 64;
    }
    status = lifecycle_host_create(&host, &services);
    if (status != HL_STATUS_OK) return 70;
    if (lifecycle_active_mappings(host) != 0) return 82;
    clock_calls = mmap(NULL, 2 * sizeof(*clock_calls), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (clock_calls == MAP_FAILED) return 76;
    realtime_calls = clock_calls + 1;
    native_clock = services.clock;
    clock = *native_clock;
    clock.monotonic_ns = spy_monotonic_ns;
    clock.realtime_ns = spy_realtime_ns;
    services.clock = &clock;
    if (bad_result) {
        result_fault = strcmp(argv[1], "--result-missing") == 0     ? 1
                       : strcmp(argv[1], "--result-truncated") == 0 ? 2
                       : strcmp(argv[1], "--result-malformed") == 0 ? 3
                                                                    : 0;
        if (result_fault == 0) return 80;
        native_stream = services.stream;
        stream = *native_stream;
        stream.write = result_write;
        services.stream = &stream;
    }
    if (fail_code_publish) {
        native_memory = services.memory;
        memory = *native_memory;
        memory.publish_code = fail_publish;
        services.memory = &memory;
        log = *services.log;
        log.emit = capture_fatal;
        services.log = &log;
    }
    memset(&config, 0, sizeof config);
    config.abi = HL_ENGINE_ABI;
    config.size = sizeof config;
    config.guest_isa = HL_TEST_GUEST_ISA;
    status = hl_engine_create(&config, &services, &engine);
    if (status != HL_STATUS_OK) {
        fprintf(stderr, "lifecycle: engine create failed: %d\n", status);
        return 71;
    }
    memset(&result, 0, sizeof result);
    result.abi = HL_ENGINE_ABI;
    result.size = sizeof result;
    if (force_stop) {
        lifecycle_run run = {engine, argc - 2, (const char *const *)(argv + 2), &result, HL_STATUS_BUSY};
        pthread_t thread;
        uint32_t attempts;
        if (argc < 3 || pthread_create(&thread, NULL, run_guest, &run) != 0) return 73;
        for (attempts = 0; attempts < UINT32_C(1000000); ++attempts) {
            status = hl_engine_request(engine, HL_ENGINE_REQUEST_FORCE_STOP, NULL, 0);
            if (status != HL_STATUS_BUSY) break;
            sched_yield();
        }
        if (status != HL_STATUS_OK || pthread_join(thread, NULL) != 0) return 74;
        status = run.status;
    } else {
        status = hl_engine_run(engine, argc - guest_index, (const char *const *)(argv + guest_index), &result);
    }
    hl_engine_destroy(engine);
    if (lifecycle_active_mappings(host) != 0) return 83;
    lifecycle_host_destroy(host);
    if (fail_code_publish)
        return status == HL_STATUS_PLATFORM_FAILURE && result.kind == HL_ENGINE_EXIT_ENGINE_ERROR &&
                       result.guest_status == HL_STATUS_PLATFORM_FAILURE && result.detail == 0 && fatal_events == 0
                   ? 0
                   : 79;
    if (bad_result)
        return status == HL_STATUS_CORRUPT && result.kind == HL_ENGINE_EXIT_ENGINE_ERROR &&
                       result.guest_status == HL_STATUS_CORRUPT
                   ? 0
                   : 81;
    if (!force_stop && __atomic_load_n(clock_calls, __ATOMIC_RELAXED) == 0) return 77;
    if (injected_clock && __atomic_load_n(realtime_calls, __ATOMIC_RELAXED) == 0) return 78;
    (void)munmap(clock_calls, 2 * sizeof(*clock_calls));
    if (force_stop)
        return status == HL_STATUS_OK && result.kind == HL_ENGINE_EXIT_SIGNAL && result.guest_status == 9 ? 0 : 75;
    if (expect_exit >= 0)
        return status == HL_STATUS_OK && result.kind == HL_ENGINE_EXIT_CODE && result.guest_status == expect_exit ? 0
                                                                                                                  : 84;
    if (expect_signal >= 0) {
        if (status == HL_STATUS_OK && result.kind == HL_ENGINE_EXIT_SIGNAL && result.guest_status == expect_signal)
            return 0;
        fprintf(stderr, "lifecycle: expected signal=%d status=%d kind=%u guest=%d detail=%llu\n", expect_signal, status,
                result.kind, result.guest_status, (unsigned long long)result.detail);
        return 85;
    }
    if (status != HL_STATUS_OK || result.kind != HL_ENGINE_EXIT_CODE) {
        fprintf(stderr, "lifecycle: status=%d kind=%u guest=%d detail=%llu\n", status, result.kind, result.guest_status,
                (unsigned long long)result.detail);
        return 72;
    }
    return result.guest_status;
}
