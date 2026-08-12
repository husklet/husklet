#ifndef HL_LINUX_GMAP_H
#define HL_LINUX_GMAP_H

#include "../../limits.h"
#include "hl/host_services.h"

#include <stddef.h>
#include <stdint.h>

typedef struct hl_gmap_entry {
    uint64_t address;
    uint64_t length;
    uint64_t guest_length;
    uint64_t physical_address;
    uint64_t physical_length;
} hl_gmap_entry;

void hl_gmap_bind_limits(const hl_limit_table *limits);
void hl_gmap_bind_host(const hl_host_services *host);
hl_status hl_gmap_map_anonymous(uint64_t requested_address, uint64_t length, uint32_t protection, uint32_t flags,
                                uint64_t *address);
size_t hl_gmap_count(void);
int hl_gmap_get(size_t index, hl_gmap_entry *entry);
void hl_gmap_add(uint64_t address, uint64_t length);
void hl_gmap_add_physical(uint64_t address, uint64_t length, uint64_t physical_address, uint64_t physical_length);
void hl_gmap_set_guest_length(uint64_t address, uint64_t guest_length);
int hl_gmap_find_physical(uint64_t address, uint64_t *physical_address, uint64_t *physical_length);
void hl_gmap_remove(uint64_t address);
uint64_t hl_gmap_find_length(uint64_t address);
uint64_t hl_gmap_find_guest_length(uint64_t address);
int hl_gmap_contains(uint64_t address, uint64_t length);
void hl_gmap_unmap_range(uint64_t start, uint64_t end);
/* Split the registry around a MAP_FIXED that replaced the range, without releasing any host mapping. */
void hl_gmap_supersede_range(uint64_t start, uint64_t end);
void hl_gmap_reset(void);

/* Loader allocations have opaque host ownership independent of guest VMA splits. */
int hl_exec_mapping_add(uint64_t address, uint64_t length, hl_host_handle host_mapping);
void hl_exec_mapping_discard_range(uint64_t address, uint64_t length);
void hl_exec_mapping_reset(void);

void hl_gmap_lock_remove(uint64_t address, uint64_t length);
void hl_gmap_lock_add(uint64_t address, uint64_t length);
void hl_gmap_lock_reset(void);
int hl_gmap_lock_wire_current(void);
void hl_gmap_lock_unwire_all(void);
uint64_t hl_gmap_lock_region_bytes(uint64_t low, uint64_t high);
uint64_t hl_gmap_lock_total_bytes(void);
int hl_gmap_lock_limit_range(uint64_t address, uint64_t length);
int hl_gmap_lock_limit_all(void);
int hl_gmap_lock_future(void);
void hl_gmap_lock_all(int future);

#endif
