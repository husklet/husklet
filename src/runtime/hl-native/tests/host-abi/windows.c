/* Public Windows C ABI integration fixture owned by hl-native. */
#include "hl/engine.h"
#include "hl/windows.h"

#include <stddef.h>
#include <stdint.h>

_Static_assert(HL_ENGINE_ABI == 5u, "the public engine ABI generation changed");
_Static_assert(sizeof(hl_engine_main_image_plan) == 48, "Win64 main-image plan layout changed");
_Static_assert(sizeof(hl_engine_config) == 96, "Win64 engine configuration layout changed");
_Static_assert(sizeof(hl_engine_exit) == 24, "Win64 engine-exit layout changed");
_Static_assert(offsetof(hl_engine_config, payload) == 32, "Win64 engine payload offset changed");
_Static_assert(offsetof(hl_engine_config, main_image_plan) == 88, "Win64 engine plan offset changed");

#if defined(HL_ABI_COMPILE_CONTRACT)
void hl_ci_compile_contract(void) {
    uint32_t (*engine_abi_signature)(void) = hl_engine_abi;
    hl_status (*windows_create_signature)(hl_host_windows **, hl_host_services *) = hl_host_windows_create;
    hl_host_result (*windows_import_signature)(hl_host_windows *, int, uint32_t) = hl_host_windows_import_file;
    void (*windows_destroy_signature)(hl_host_windows *) = hl_host_windows_destroy;
    (void)engine_abi_signature;
    (void)windows_create_signature;
    (void)windows_import_signature;
    (void)windows_destroy_signature;
}
#endif

#if defined(HL_ABI_FIXTURE_EXPORT)
__declspec(dllexport) uint32_t hl_ci_engine_abi(void) {
    return HL_ENGINE_ABI;
}
#else
__declspec(dllimport) uint32_t hl_ci_engine_abi(void);

int main(void) {
    return hl_ci_engine_abi() == HL_ENGINE_ABI ? 0 : 1;
}
#endif
