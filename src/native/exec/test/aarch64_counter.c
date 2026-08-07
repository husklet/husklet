#include "../src/arch/aarch64/system.h"
#include "../src/arch/aarch64/trace.h"
#include "../src/state.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "counter:%d: %s\n", __LINE__, #x); return 1; } } while (0)

/* The interpreter projects cntvct_el0 as host monotonic nanoseconds against a
 * fixed 1GHz cntfrq_el0, so emitting the host mrs would hand the guest raw
 * 24MHz generic-timer ticks and desynchronise the two execution paths. */
int main(void) {
    const uint32_t counters[] = {
        0xd53be000u, /* mrs x0,cntfrq_el0 */
        0xd53be040u, /* mrs x0,cntvct_el0 */
        0xd53be053u, /* mrs x19,cntvct_el0 */
        0xd53be020u, /* mrs x0,cntpct_el0 */
    };
    uint8_t storage[HL_A64_TRACE_MAX_BYTES];
    for (size_t index = 0; index < sizeof(counters) / sizeof(counters[0]); index++) {
        hl_a64_assembler assembler;
        CHECK(hl_a64_assembler_begin(&assembler, storage, storage, sizeof(storage)));
        CHECK(hl_a64_system_body(&assembler, counters[index]) == 0);
        CHECK(hl_a64_assembler_begin(&assembler, storage, storage, sizeof(storage)));
        CHECK(hl_a64_system_emit(&assembler, counters[index], 0x6000) == 0);
    }

    uint32_t word = 0;
    uint64_t head = 0, body = 0;
    hl_native_state_untranslatable(0xd53be040u, 1);
    hl_native_state_untranslatable(0xd53be040u, 1);
    hl_native_state_untranslatable(0xd53be040u, 0);
    for (uint32_t index = 0;; index++) {
        CHECK(index < 512);
        if (!hl_native_state_untranslatable_report(index, &word, &head, &body)) continue;
        if (word != 0xd53be040u) continue;
        CHECK(head == 2);
        CHECK(body == 1);
        break;
    }
    return 0;
}
