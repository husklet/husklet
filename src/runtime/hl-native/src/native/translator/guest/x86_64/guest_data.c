#include "guest_data.h"

#include <stdint.h>
#include <string.h>

struct guest_data_una8 {
    uint8_t value;
} __attribute__((packed));

struct guest_data_una16 {
    uint16_t value;
} __attribute__((packed));

struct guest_data_una32 {
    uint32_t value;
} __attribute__((packed));

struct guest_data_una64 {
    uint64_t value;
} __attribute__((packed));

static void guest_data_copy_indivisible(void *destination, const void *source, size_t length) {
    switch (length) {
    case 1: *(struct guest_data_una8 *)destination = *(const struct guest_data_una8 *)source; return;
    case 2: *(struct guest_data_una16 *)destination = *(const struct guest_data_una16 *)source; return;
    case 4: *(struct guest_data_una32 *)destination = *(const struct guest_data_una32 *)source; return;
    case 8: *(struct guest_data_una64 *)destination = *(const struct guest_data_una64 *)source; return;
    default: memcpy(destination, source, length); return;
    }
}

/* The largest helper-owned image is XSAVE's 576-byte standard-format area. A
   maximal pin span may be one byte, so 576 retained pins is the finite upper
   bound and avoids allocation in the execution path. */
void hl_x86_guest_data_release(hl_x86_guest_data_pins *pins) {
    while (pins->count != 0) {
        pins->count--;
        hl_guest_memory_unpin_data(&pins->spans[pins->count].pin);
    }
    if (pins->transaction_active) {
        pins->transaction_active = 0;
        hl_guest_memory_transaction_end();
    }
}

void hl_x86_guest_data_abandon(hl_x86_guest_data_pins *pins) {
    if (pins == NULL) return;
    if (pins->access == HL_GUEST_MEMORY_WRITE && pins->commit_started)
        hl_guest_memory_store_observe(pins->guest, pins->length);
    hl_x86_guest_data_release(pins);
}

int hl_x86_guest_data_prepare(hl_x86_guest_data_pins *pins, uint64_t guest, size_t length,
                              hl_guest_memory_access access, uint64_t *fault_guest) {
    if (pins == NULL) return -1;
    *pins = (hl_x86_guest_data_pins){0};
    pins->guest = guest;
    pins->length = length;
    pins->access = access;
    if (fault_guest != NULL) *fault_guest = guest;
    if (length == 0 || length > HL_X86_GUEST_DATA_MAX || guest > UINT64_MAX - length) return -1;
    size_t offset = 0;
    while (offset < length) {
        if (pins->count == HL_X86_GUEST_DATA_MAX) {
            hl_x86_guest_data_release(pins);
            return -1;
        }
        hl_x86_guest_data_span *span = &pins->spans[pins->count];
        int result = hl_guest_memory_pin_data(guest + offset, length - offset, access, &span->pin);
        if (result < 0 || span->pin.host == NULL || span->pin.contiguous == 0) {
            hl_guest_memory_unpin_data(&span->pin);
            if (fault_guest != NULL) *fault_guest = guest + offset;
            hl_x86_guest_data_release(pins);
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

int hl_x86_guest_data_prepare_transaction(hl_x86_guest_data_pins *pins, uint64_t guest, size_t length,
                                          hl_guest_memory_access access, uint64_t *fault_guest) {
    hl_guest_memory_transaction_begin();
    if (hl_x86_guest_data_prepare(pins, guest, length, access, fault_guest) != 0) {
        hl_guest_memory_transaction_end();
        return -1;
    }
    pins->transaction_active = 1;
    return 0;
}

void hl_x86_guest_data_copy_from(hl_x86_guest_data_pins *pins, void *destination) {
    if (pins->count == 1) {
        guest_data_copy_indivisible(destination, pins->spans[0].pin.host, pins->length);
        return;
    }
    for (size_t index = 0; index < pins->count; ++index) {
        const hl_x86_guest_data_span *span = &pins->spans[index];
        memcpy((uint8_t *)destination + span->offset, span->pin.host, span->length);
    }
}

void hl_x86_guest_data_copy_to(hl_x86_guest_data_pins *pins, const void *source) {
    pins->commit_started = 1;
    if (pins->count == 1) {
        guest_data_copy_indivisible(pins->spans[0].pin.host, source, pins->length);
        return;
    }
    for (size_t index = 0; index < pins->count; ++index) {
        const hl_x86_guest_data_span *span = &pins->spans[index];
        memcpy(span->pin.host, (const uint8_t *)source + span->offset, span->length);
    }
}

int hl_x86_guest_data_read(uint64_t guest, void *destination, size_t length, uint64_t *fault_guest) {
    if (destination == NULL) return -1;
    hl_x86_guest_data_pins pins = {0};
    if (hl_x86_guest_data_prepare(&pins, guest, length, HL_GUEST_MEMORY_READ, fault_guest) != 0) return -1;
    hl_x86_guest_data_copy_from(&pins, destination);
    hl_x86_guest_data_release(&pins);
    return 0;
}

int hl_x86_guest_data_write(uint64_t guest, const void *source, size_t length, uint64_t *fault_guest) {
    if (source == NULL) return -1;
    hl_x86_guest_data_pins pins = {0};
    if (hl_x86_guest_data_prepare(&pins, guest, length, HL_GUEST_MEMORY_WRITE, fault_guest) != 0) return -1;
    hl_x86_guest_data_copy_to(&pins, source);
    hl_x86_guest_data_release(&pins);
    hl_guest_memory_store_observe(guest, length);
    return 0;
}
