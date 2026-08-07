#ifndef HL_NATIVE_STATE_H
#define HL_NATIVE_STATE_H

#include "executor.h"

/* Records which invariant produced HL_NATIVE_STATE so the status is reportable
 * rather than an unclassified code. */
hl_native_status hl_native_state_record(const char *invariant);

#define HL_STATE(invariant) hl_native_state_record(invariant)

/* Census of guest words that refused lowering, split by whether the word sat on
 * a trace head (blocks the whole trace) or in a body (truncates it). */
void hl_native_state_untranslatable(uint32_t word, int head);
int hl_native_state_untranslatable_report(uint32_t index, uint32_t *word, uint64_t *head,
                                          uint64_t *body);

/* Same census for x86, keyed by the first eight bytes of the refused instruction
 * including its prefixes, since guest instructions there are variable length. */
void hl_native_state_untranslatable_x86(const uint8_t *bytes, size_t length, int head);
int hl_native_state_untranslatable_x86_report(uint32_t index, uint64_t *bytes, uint64_t *head,
                                              uint64_t *body);

#endif
