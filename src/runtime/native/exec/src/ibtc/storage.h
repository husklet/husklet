#ifndef HL_NATIVE_IBTC_STORAGE_H
#define HL_NATIVE_IBTC_STORAGE_H

#include <stddef.h>
#include <stdatomic.h>
#include <stdint.h>

typedef struct hl_native_ibtc_entry {
    uint64_t target;
    void *body;
} hl_native_ibtc_entry;

/* Dormant authenticated-ingress storage.  It is deliberately distinct from
 * the live 16-byte IBTC so existing readers cannot reinterpret its stride. */
typedef struct hl_native_ibtc_authenticated_entry {
    _Alignas(32) uint64_t target;
    uint64_t authenticated_ingress;
    uint64_t target_identity;
    _Atomic uint64_t sequence;
} hl_native_ibtc_authenticated_entry;

#define HL_NATIVE_IBTC_COUNT (1u << 16)
#define HL_NATIVE_IBTC_AUTHENTICATED_BYTES \
    ((size_t)HL_NATIVE_IBTC_COUNT * sizeof(hl_native_ibtc_authenticated_entry))

hl_native_ibtc_entry *hl_native_ibtc_storage_create(void);
void hl_native_ibtc_storage_destroy(hl_native_ibtc_entry *);
int hl_native_ibtc_authenticated_storage_bytes(size_t, size_t *);
hl_native_ibtc_authenticated_entry *hl_native_ibtc_authenticated_storage_create(void);
void hl_native_ibtc_authenticated_storage_clear(hl_native_ibtc_authenticated_entry *);
void hl_native_ibtc_authenticated_storage_destroy(hl_native_ibtc_authenticated_entry *);

#endif
