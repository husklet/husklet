/* Public C++ ABI integration fixture owned by hl-native. */
#include "hl/engine.h"

#if defined(__APPLE__)
#include "hl/macos.h"
using hl_ci_host = hl_host_macos;
#define HL_CI_HOST_CREATE hl_host_macos_create
#define HL_CI_HOST_IMPORT hl_host_macos_import_file
#define HL_CI_HOST_DESTROY hl_host_macos_destroy
#elif defined(__linux__)
#include "hl/linux.h"
using hl_ci_host = hl_host_linux;
#define HL_CI_HOST_CREATE hl_host_linux_create
#define HL_CI_HOST_IMPORT hl_host_linux_import_file
#define HL_CI_HOST_DESTROY hl_host_linux_destroy
#else
#error "the Unix ABI fixture requires Linux or macOS"
#endif

#include <cstddef>
#include <cstdint>
#include <type_traits>

static_assert(sizeof(hl_engine_config) == 96, "LP64 C++ engine configuration layout changed");
static_assert(offsetof(hl_engine_config, main_image_plan) == 88, "LP64 C++ engine plan offset changed");
static_assert(std::is_same_v<decltype(&hl_engine_abi), std::uint32_t (*)()>);
static_assert(std::is_same_v<decltype(&HL_CI_HOST_CREATE), hl_status (*)(hl_ci_host **, hl_host_services *)>);
static_assert(std::is_same_v<decltype(&HL_CI_HOST_IMPORT), hl_host_result (*)(hl_ci_host *, int)>);
static_assert(std::is_same_v<decltype(&HL_CI_HOST_DESTROY), void (*)(hl_ci_host *)>);

extern "C" HL_API std::uint32_t hl_engine_abi(void);

int main() {
    return hl_engine_abi() == HL_ENGINE_ABI ? 0 : 1;
}
