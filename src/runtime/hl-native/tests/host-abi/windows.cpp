/* Public Windows C++ ABI integration fixture owned by hl-native. */
#include "hl/engine.h"
#include "hl/windows.h"

#include <cstddef>
#include <cstdint>
#include <type_traits>

static_assert(sizeof(hl_engine_main_image_plan) == 48, "Win64 C++ main-image plan layout changed");
static_assert(sizeof(hl_engine_config) == 96, "Win64 C++ engine configuration layout changed");
static_assert(offsetof(hl_engine_config, main_image_plan) == 88, "Win64 C++ engine plan offset changed");

using engine_abi_signature = std::uint32_t (*)();
using windows_create_signature = hl_status (*)(hl_host_windows **, hl_host_services *);
using windows_import_signature = hl_host_result (*)(hl_host_windows *, int, std::uint32_t);
using windows_destroy_signature = void (*)(hl_host_windows *);

static_assert(std::is_same_v<decltype(&hl_engine_abi), engine_abi_signature>);
static_assert(std::is_same_v<decltype(&hl_host_windows_create), windows_create_signature>);
static_assert(std::is_same_v<decltype(&hl_host_windows_import_file), windows_import_signature>);
static_assert(std::is_same_v<decltype(&hl_host_windows_destroy), windows_destroy_signature>);

extern "C" HL_API std::uint32_t hl_engine_abi(void);
extern "C" HL_API hl_status hl_host_windows_create(hl_host_windows **, hl_host_services *);
extern "C" HL_API hl_host_result hl_host_windows_import_file(hl_host_windows *, int, std::uint32_t);
extern "C" HL_API void hl_host_windows_destroy(hl_host_windows *);
