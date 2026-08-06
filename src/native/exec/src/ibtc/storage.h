#ifndef HL_NATIVE_IBTC_STORAGE_H
#define HL_NATIVE_IBTC_STORAGE_H

#include <stdint.h>

typedef struct hl_native_ibtc_entry {
    uint64_t target;
    void *body;
} hl_native_ibtc_entry;

#define HL_NATIVE_IBTC_COUNT (1u << 16)

hl_native_ibtc_entry *hl_native_ibtc_storage_create(void);
void hl_native_ibtc_storage_destroy(hl_native_ibtc_entry *);

#endif
