#include "guest_data.h"

#include <stdint.h>
#include <string.h>

#include "../../guest_memory.h"

/* The largest helper-owned image is the 512-byte legacy FXSAVE area.  A
   maximal pin span may be one byte, so 512 retained pins is the exact finite
   upper bound and avoids allocation in the execution path. */
#define HL_X86_GUEST_DATA_MAX 512u

typedef struct {
    hl_guest_memory_pin pin;
    size_t offset;
    size_t length;
} hl_x86_guest_data_span;

typedef struct {
    hl_x86_guest_data_span spans[HL_X86_GUEST_DATA_MAX];
    size_t count;
} hl_x86_guest_data_pins;

static void guest_data_release(hl_x86_guest_data_pins *pins) {
    while (pins->count != 0) {
        pins->count--;
        hl_guest_memory_unpin_data(&pins->spans[pins->count].pin);
    }
}

static int guest_data_pin(uint64_t guest, size_t length, hl_guest_memory_access access,
                          hl_x86_guest_data_pins *pins, uint64_t *fault_guest) {
    if (fault_guest != NULL) *fault_guest = guest;
    if (length == 0 || length > HL_X86_GUEST_DATA_MAX || guest > UINT64_MAX - length) return -1;
    size_t offset = 0;
    while (offset < length) {
        if (pins->count == HL_X86_GUEST_DATA_MAX) {
            guest_data_release(pins);
            return -1;
        }
        hl_x86_guest_data_span *span = &pins->spans[pins->count];
        int result = hl_guest_memory_pin_data(guest + offset, length - offset, access, &span->pin);
        if (result < 0 || span->pin.host == NULL || span->pin.contiguous == 0) {
            hl_guest_memory_unpin_data(&span->pin);
            if (fault_guest != NULL) *fault_guest = guest + offset;
            guest_data_release(pins);
            return -1;
        }
        span->offset = offset;
        span->length = span->pin.contiguous;
        if (span->length > length - offset) span->length = length - offset;
        pins->count++;
        offset += span->length;
    }
    return 0;
}

int hl_x86_guest_data_read(uint64_t guest, void *destination, size_t length, uint64_t *fault_guest) {
    if (destination == NULL) return -1;
    hl_x86_guest_data_pins pins = {0};
    if (guest_data_pin(guest, length, HL_GUEST_MEMORY_READ, &pins, fault_guest) != 0) return -1;
    for (size_t index = 0; index < pins.count; ++index) {
        const hl_x86_guest_data_span *span = &pins.spans[index];
        memcpy((uint8_t *)destination + span->offset, span->pin.host, span->length);
    }
    guest_data_release(&pins);
    return 0;
}

int hl_x86_guest_data_write(uint64_t guest, const void *source, size_t length, uint64_t *fault_guest) {
    if (source == NULL) return -1;
    hl_x86_guest_data_pins pins = {0};
    if (guest_data_pin(guest, length, HL_GUEST_MEMORY_WRITE, &pins, fault_guest) != 0) return -1;
    for (size_t index = 0; index < pins.count; ++index) {
        const hl_x86_guest_data_span *span = &pins.spans[index];
        memcpy(span->pin.host, (const uint8_t *)source + span->offset, span->length);
    }
    guest_data_release(&pins);
    hl_guest_memory_store_observe(guest, length);
    return 0;
}
