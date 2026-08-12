#include "address_projection.h"
#include <stddef.h>
int32_t hl_native_address_projection_init(hl_native_address_projection *p, uint64_t start, uint64_t end,
                                          uint64_t storage) {
    if (!p || end <= start || storage < start) return -1;
    uint64_t bias = storage - start;
    if (bias > UINT64_MAX - end) return -1;
    *p = (hl_native_address_projection){HL_NATIVE_ADDRESS_PROJECTION_ABI, (uint32_t)sizeof(*p),
                                        bias ? HL_NATIVE_ADDRESS_PROJECTION_DISPLACED : 0, 0, start, end, bias};
    return 0;
}
int32_t hl_native_address_projection_storage(const hl_native_address_projection *p, uint64_t address,
                                             uint64_t *output) {
    if (!hl_native_address_projection_valid(p) || !output) return -1;
    *output = hl_native_address_projection_storage_unchecked(p, address);
    return 0;
}
int32_t hl_native_address_projection_guest(const hl_native_address_projection *p, uint64_t address,
                                           uint64_t *output) {
    if (!hl_native_address_projection_valid(p) || !output) return -1;
    *output = hl_native_address_projection_guest_unchecked(p, address);
    return 0;
}
