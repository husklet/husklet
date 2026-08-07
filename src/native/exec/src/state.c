#include "state.h"

static _Thread_local const char *state_invariant;

hl_native_status hl_native_state_record(const char *invariant) {
    state_invariant = invariant;
    return HL_NATIVE_STATE;
}

const char *hl_native_state_invariant(void) {
    return state_invariant == NULL ? "unclassified" : state_invariant;
}
