#include "hl/engine.h"

#include <stdint.h>

_Static_assert(HL_ENGINE_ABI == 5u, "the public engine ABI generation changed");
_Static_assert(sizeof(((hl_engine_config *)0)->abi) == sizeof(uint32_t), "ABI field width changed");
_Static_assert(sizeof(((hl_engine_config *)0)->size) == sizeof(uint32_t), "size field width changed");

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
