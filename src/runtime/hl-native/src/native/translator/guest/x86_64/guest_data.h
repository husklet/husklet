#ifndef HL_TRANSLATOR_GUEST_X86_64_GUEST_DATA_H
#define HL_TRANSLATOR_GUEST_X86_64_GUEST_DATA_H

#include <stddef.h>
#include <stdint.h>

#include "../../guest_memory.h"

#define HL_X86_GUEST_DATA_MAX 512u

typedef struct {
    hl_guest_memory_pin pin;
    size_t offset;
    size_t length;
} hl_x86_guest_data_span;

typedef struct {
    hl_x86_guest_data_span spans[HL_X86_GUEST_DATA_MAX];
    size_t count;
    uint64_t guest;
    size_t length;
    hl_guest_memory_access access;
    int commit_started;
} hl_x86_guest_data_pins;

/*
 * Copy a complete helper operand in architectural guest coordinates.
 *
 * A logical object may cross several independently projected views.  These
 * routines pin every view before moving a byte, so a failed later span cannot
 * leave a partial guest store or a partially restored CPU image.  On failure,
 * *fault_guest is the first byte for which pinning made no forward progress.
 *
 * Preparation proves projection and requested guest permissions only. A host
 * mapping can still fault afterward (for example, SIGBUS after file truncation).
 * A caller that can recover from host faults must retain this pin set in its
 * landing authority and call release there; commit_started tells a write-side
 * landing whether conservative whole-range store publication is owed.
 */
int hl_x86_guest_data_read(uint64_t guest, void *destination, size_t length, uint64_t *fault_guest);
int hl_x86_guest_data_write(uint64_t guest, const void *source, size_t length, uint64_t *fault_guest);
int hl_x86_guest_data_prepare(hl_x86_guest_data_pins *pins, uint64_t guest, size_t length,
                              hl_guest_memory_access access, uint64_t *fault_guest);
void hl_x86_guest_data_copy_from(hl_x86_guest_data_pins *pins, void *destination);
void hl_x86_guest_data_copy_to(hl_x86_guest_data_pins *pins, const void *source);
void hl_x86_guest_data_release(hl_x86_guest_data_pins *pins);
void hl_x86_guest_data_abandon(hl_x86_guest_data_pins *pins);

#endif
