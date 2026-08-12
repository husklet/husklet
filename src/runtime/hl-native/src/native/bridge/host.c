#include "host.h"

#if defined(_WIN32)
#include "hl/windows.h"
typedef hl_host_windows hl_c_bridge_platform_host;
#define HL_C_HOST_CREATE hl_host_windows_create
#define HL_C_HOST_IMPORT_FILE hl_host_windows_import_file
#define HL_C_HOST_DESTROY hl_host_windows_destroy
#elif defined(__APPLE__)
#include "hl/macos.h"
typedef hl_host_macos hl_c_bridge_platform_host;
#define HL_C_HOST_CREATE hl_host_macos_create
#define HL_C_HOST_IMPORT_FILE hl_host_macos_import_file
#define HL_C_HOST_DESTROY hl_host_macos_destroy
#elif defined(__linux__)
#include "hl/linux.h"
typedef hl_host_linux hl_c_bridge_platform_host;
#define HL_C_HOST_CREATE hl_host_linux_create
#define HL_C_HOST_IMPORT_FILE hl_host_linux_import_file
#define HL_C_HOST_DESTROY hl_host_linux_destroy
#else
#error "hl-native has no host-services adapter for this platform"
#endif

hl_status hl_c_bridge_host_create(hl_c_bridge_host **out_host, hl_host_services *out_services) {
    return HL_C_HOST_CREATE((hl_c_bridge_platform_host **)out_host, out_services);
}

hl_host_result hl_c_bridge_host_import_file(hl_c_bridge_host *host, int32_t descriptor, uint32_t access) {
#if defined(_WIN32)
    return HL_C_HOST_IMPORT_FILE((hl_c_bridge_platform_host *)host, descriptor, access);
#else
    (void)access;
    return HL_C_HOST_IMPORT_FILE((hl_c_bridge_platform_host *)host, descriptor);
#endif
}

void hl_c_bridge_host_destroy(hl_c_bridge_host *host) {
    HL_C_HOST_DESTROY((hl_c_bridge_platform_host *)host);
}
