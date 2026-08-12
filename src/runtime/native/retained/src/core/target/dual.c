#include "hl/host_services.h"
#include "hl/linux_abi.h"

/* Compatibility entry for the legacy config-file launcher.  The typed engine
 * API selects either embedded backend by config.guest_isa; this untyped entry
 * retains the native AArch64 default used by the macOS command launcher. */
int hl_aarch64_run_linux_guest(const hl_host_services *host, hl_linux_abi *box, const char *rootfs,
                               hl_host_handle executable, const void *executable_image, size_t executable_size,
                               uint32_t argc, char *const argv[]);

int hl_run_linux_guest(const hl_host_services *host, hl_linux_abi *box, const char *rootfs, hl_host_handle executable,
                       const void *executable_image, size_t executable_size, uint32_t argc, char *const argv[]) {
    return hl_aarch64_run_linux_guest(host, box, rootfs, executable, executable_image, executable_size, argc, argv);
}
