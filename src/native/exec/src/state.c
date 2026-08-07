#include "state.h"

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>

static _Thread_local const char *state_invariant;

hl_native_status hl_native_state_record(const char *invariant) {
    state_invariant = invariant;
    return HL_NATIVE_STATE;
}

const char *hl_native_state_invariant(void) {
    return state_invariant == NULL ? "unclassified" : state_invariant;
}

#define CENSUS 512

/* Process-global rather than thread-local: the census aggregates translation
 * refusals across every worker thread that builds traces. */
static struct {
    _Atomic uint32_t word;
    _Atomic uint32_t used;
    _Atomic uint64_t head;
    _Atomic uint64_t body;
} state_census[CENSUS];

void hl_native_state_untranslatable(uint32_t word, int head) {
    uint32_t slot = (word * UINT32_C(2654435761)) % CENSUS;
    for (uint32_t probe = 0; probe < CENSUS; probe++) {
        uint32_t index = (slot + probe) % CENSUS;
        uint32_t expected = 0;
        if (!atomic_load(&state_census[index].used)) {
            if (!atomic_compare_exchange_strong(&state_census[index].used, &expected, 1)) {
                if (atomic_load(&state_census[index].word) != word) continue;
            } else {
                atomic_store(&state_census[index].word, word);
            }
        } else if (atomic_load(&state_census[index].word) != word) {
            continue;
        }
        if (head) atomic_fetch_add(&state_census[index].head, 1);
        else atomic_fetch_add(&state_census[index].body, 1);
        return;
    }
}

__attribute__((destructor)) static void state_census_dump(void) {
    if (getenv("HL_NATIVE_CENSUS") == NULL) return;
    for (uint32_t index = 0; index < CENSUS; index++) {
        if (!atomic_load(&state_census[index].used)) continue;
        fprintf(stderr, "CENSUS word=%08x head=%llu body=%llu\n",
                atomic_load(&state_census[index].word),
                (unsigned long long)atomic_load(&state_census[index].head),
                (unsigned long long)atomic_load(&state_census[index].body));
    }
}

int hl_native_state_untranslatable_report(uint32_t index, uint32_t *word, uint64_t *head,
                                          uint64_t *body) {
    if (index >= CENSUS || !atomic_load(&state_census[index].used)) return 0;
    if (word != NULL) *word = atomic_load(&state_census[index].word);
    if (head != NULL) *head = atomic_load(&state_census[index].head);
    if (body != NULL) *body = atomic_load(&state_census[index].body);
    return 1;
}
