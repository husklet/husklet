#ifndef HL_HOST_SYNC_H
#define HL_HOST_SYNC_H

#include "hl/host_services.h"

typedef struct hl_host_sync_registry hl_host_sync_registry;

hl_status hl_host_sync_registry_create(hl_host_sync_registry **output);
void hl_host_sync_registry_destroy(hl_host_sync_registry *registry);
hl_host_result hl_host_sync_mutex_create(hl_host_sync_registry *registry);
hl_host_result hl_host_sync_mutex_lock(hl_host_sync_registry *registry, hl_host_handle handle);
hl_host_result hl_host_sync_mutex_unlock(hl_host_sync_registry *registry, hl_host_handle handle);
hl_host_result hl_host_sync_mutex_close(hl_host_sync_registry *registry, hl_host_handle handle);
hl_host_result hl_host_sync_fork_prepare(hl_host_sync_registry *registry);
hl_host_result hl_host_sync_fork_complete(hl_host_sync_registry *registry);

/* Address-keyed parking, shared by every host that has an address-keyed wait. The registry owns the
 * waiter records -- who is blocked, on which address, and who has an interruption outstanding --
 * because that bookkeeping is identical everywhere; only the block-and-wake primitive underneath is
 * per-host. See hl_host_sync_services for the contract these implement. */
hl_host_result hl_host_sync_park(hl_host_sync_registry *registry, uint64_t waiter, uint32_t scope, uint64_t key,
                                 const void *address, uint64_t expected, uint32_t compare_size, uint64_t deadline_ns);
hl_host_result hl_host_sync_unpark(hl_host_sync_registry *registry, uint32_t scope, uint64_t key, const void *address,
                                   uint32_t count);
hl_host_result hl_host_sync_interrupt_park(hl_host_sync_registry *registry, uint64_t waiter);
/* Forget every waiter record. Exactly one caller: the child arm of a fork, where no thread but the
 * caller survived and every record the parent left is about a thread that does not exist here. */
void hl_host_sync_park_reset(hl_host_sync_registry *registry);

#endif
