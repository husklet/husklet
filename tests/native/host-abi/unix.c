#include "hl/engine.h"

#if defined(__APPLE__)
#include "hl/macos.h"
typedef hl_host_macos hl_ci_host;
#define HL_CI_HOST_CREATE hl_host_macos_create
#define HL_CI_HOST_IMPORT hl_host_macos_import_file
#define HL_CI_HOST_DESTROY hl_host_macos_destroy
#elif defined(__linux__)
#include "hl/linux.h"
typedef hl_host_linux hl_ci_host;
#define HL_CI_HOST_CREATE hl_host_linux_create
#define HL_CI_HOST_IMPORT hl_host_linux_import_file
#define HL_CI_HOST_DESTROY hl_host_linux_destroy
#else
#error "the Unix ABI fixture requires Linux or macOS"
#endif

#include <stddef.h>
#include <stdint.h>

_Static_assert(sizeof(void *) == 8, "the public Unix ABI requires LP64 pointers");
_Static_assert(sizeof(hl_engine_main_image_plan) == 48, "LP64 main-image plan layout changed");
_Static_assert(sizeof(hl_engine_config) == 96, "LP64 engine configuration layout changed");
_Static_assert(sizeof(hl_engine_exit) == 24, "LP64 engine-exit layout changed");
_Static_assert(offsetof(hl_engine_config, payload) == 32, "LP64 engine payload offset changed");
_Static_assert(offsetof(hl_engine_config, main_image_plan) == 88, "LP64 engine plan offset changed");

static void hl_ci_signatures(void) {
    uint32_t (*engine_abi)(void) = hl_engine_abi;
    hl_status (*host_create)(hl_ci_host **, hl_host_services *) = HL_CI_HOST_CREATE;
    hl_host_result (*host_import)(hl_ci_host *, int) = HL_CI_HOST_IMPORT;
    void (*host_destroy)(hl_ci_host *) = HL_CI_HOST_DESTROY;
    (void)engine_abi;
    (void)host_create;
    (void)host_import;
    (void)host_destroy;
}

int main(void) {
    hl_ci_signatures();
    return hl_engine_abi() == HL_ENGINE_ABI ? 0 : 1;
}
