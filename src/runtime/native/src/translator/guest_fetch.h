#ifndef HL_TRANSLATOR_GUEST_FETCH_H
#define HL_TRANSLATOR_GUEST_FETCH_H

#include <stddef.h>
#include <stdint.h>

/*
 * Copies executable guest bytes without assuming that the guest virtual
 * address is also the host address.  Returns zero on success and -1 when the
 * logical mapping rejects instruction fetch.
 *
 * The implementation deliberately walks one 4 KiB guest page at a time.
 * x86 instructions can straddle a page boundary, and two adjacent logical
 * VMAs need not be contiguous in the canonical host backing.
 */
int hl_guest_fetch_exec(uint64_t guest, void *destination, size_t length);
int hl_guest_fetch_u32(uint64_t guest, uint32_t *instruction);
typedef int (*hl_guest_fetch_direct_validator)(uint64_t, size_t);
void hl_guest_fetch_set_direct_validator(hl_guest_fetch_direct_validator);

#endif
