#include "native.h"

#if defined(__APPLE__)
#include "hl/macos.h"
#elif defined(_WIN32)
#include "hl/windows.h"
#elif defined(__linux__)
#include "hl/linux.h"
#endif

hl_status hl_native_host_create(hl_native_host **host, hl_host_services *services) {
#if defined(__APPLE__)
    return hl_host_macos_create((hl_host_macos **)(void *)host, services);
#elif defined(_WIN32)
    return hl_host_windows_create((hl_host_windows **)(void *)host, services);
#else
    return hl_host_linux_create((hl_host_linux **)(void *)host, services);
#endif
}

void hl_native_host_destroy(hl_native_host *host) {
#if defined(__APPLE__)
    hl_host_macos_destroy((hl_host_macos *)(void *)host);
#elif defined(_WIN32)
    hl_host_windows_destroy((hl_host_windows *)(void *)host);
#else
    hl_host_linux_destroy((hl_host_linux *)(void *)host);
#endif
}

int hl_native_host_bind(hl_native_host **native, hl_host_services *bound, const hl_host_services *injected) {
    if (native == NULL || bound == NULL) return -1;
    if (bound->abi != 0) {
        if (injected != NULL)
            return injected->context == bound->context && injected->memory == bound->memory &&
                           injected->clock == bound->clock
                       ? 0
                       : -1;
        return *native != NULL ? 0 : -1;
    }
    if (injected != NULL)
        *bound = *injected;
    else if (hl_native_host_create(native, bound) != HL_STATUS_OK)
        return -1;
    return hl_host_services_validate(bound, HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_CODE_MAPPING) ==
                   HL_STATUS_OK
               ? 0
               : -1;
}
