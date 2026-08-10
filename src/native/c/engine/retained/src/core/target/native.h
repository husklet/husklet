#ifndef HL_CORE_TARGET_NATIVE_H
#define HL_CORE_TARGET_NATIVE_H

#include "hl/config.h"
#include "hl/engine.h"
#include "hl/host_services.h"
#include "../engine_backend.h"

#if defined(__APPLE__)
#define HL_NATIVE_HOST_NAME "macos"
#elif defined(_WIN32)
#define HL_NATIVE_HOST_NAME "windows"
#elif defined(__linux__)
#define HL_NATIVE_HOST_NAME "linux"
#else
#error "hl engine has no native host backend for this platform"
#endif

typedef struct hl_native_host hl_native_host;

hl_status hl_native_host_create(hl_native_host **host, hl_host_services *services);
void hl_native_host_destroy(hl_native_host *host);
int hl_native_host_bind(hl_native_host **native, hl_host_services *bound, const hl_host_services *injected);
int hl_native_result_store(const hl_host_services *services, const char *path, const hl_launch_result *result);
int hl_native_engine_run(uint32_t guest_isa, const char *rootfs, uint32_t argc, char *const argv[],
                         const hl_options *options, const char *result_path);

#endif
