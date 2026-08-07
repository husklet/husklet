#ifndef HL_NATIVE_STATE_H
#define HL_NATIVE_STATE_H

#include "executor.h"

/* Records which invariant produced HL_NATIVE_STATE so the status is reportable
 * rather than an unclassified code. */
hl_native_status hl_native_state_record(const char *invariant);

#define HL_STATE(invariant) hl_native_state_record(invariant)

#endif
