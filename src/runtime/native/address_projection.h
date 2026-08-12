#ifndef HL_NATIVE_ADDRESS_PROJECTION_H
#define HL_NATIVE_ADDRESS_PROJECTION_H
#include <stdint.h>
#define HL_NATIVE_ADDRESS_PROJECTION_ABI 1u
#define HL_NATIVE_ADDRESS_PROJECTION_DISPLACED 1u
typedef struct hl_native_address_projection {
    uint32_t abi, size, flags, reserved;
    uint64_t guest_start, guest_end, storage_bias;
} hl_native_address_projection;
int32_t hl_native_address_projection_init(hl_native_address_projection *, uint64_t, uint64_t, uint64_t);
int32_t hl_native_address_projection_storage(const hl_native_address_projection *, uint64_t, uint64_t *);
int32_t hl_native_address_projection_guest(const hl_native_address_projection *, uint64_t, uint64_t *);
static inline int hl_native_address_projection_valid(const hl_native_address_projection *p) {
    if (!p || p->abi != HL_NATIVE_ADDRESS_PROJECTION_ABI || p->size < sizeof(*p) || p->reserved ||
        (p->flags & ~HL_NATIVE_ADDRESS_PROJECTION_DISPLACED) || p->guest_end <= p->guest_start)
        return 0;
    return p->storage_bias <= UINT64_MAX - p->guest_end &&
           ((p->storage_bias != 0) == ((p->flags & HL_NATIVE_ADDRESS_PROJECTION_DISPLACED) != 0));
}
static inline uint64_t hl_native_address_projection_storage_unchecked(const hl_native_address_projection *p,
                                                                      uint64_t guest) {
    return guest >= p->guest_start && guest < p->guest_end ? guest + p->storage_bias : guest;
}
static inline uint64_t hl_native_address_projection_guest_unchecked(const hl_native_address_projection *p,
                                                                    uint64_t storage) {
    uint64_t start = p->guest_start + p->storage_bias, end = p->guest_end + p->storage_bias;
    return storage >= start && storage < end ? storage - p->storage_bias : storage;
}
#endif
