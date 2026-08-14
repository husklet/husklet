#ifndef HL_TRANSLATOR_GUEST_X86_64_GUEST_DATA_H
#define HL_TRANSLATOR_GUEST_X86_64_GUEST_DATA_H

#include <stddef.h>
#include <stdint.h>

/*
 * Copy a complete helper operand in architectural guest coordinates.
 *
 * A logical object may cross several independently projected views.  These
 * routines pin every view before moving a byte, so a failed later span cannot
 * leave a partial guest store or a partially restored CPU image.  On failure,
 * *fault_guest is the first byte for which pinning made no forward progress.
 */
int hl_x86_guest_data_read(uint64_t guest, void *destination, size_t length, uint64_t *fault_guest);
int hl_x86_guest_data_write(uint64_t guest, const void *source, size_t length, uint64_t *fault_guest);

#endif
