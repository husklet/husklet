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

typedef struct {
    _Atomic uint64_t key;
    _Atomic uint32_t used;
    _Atomic uint64_t head;
    _Atomic uint64_t body;
} census_slot;

/* Process-global rather than thread-local: the census aggregates translation
 * refusals across every worker thread that builds traces. The x86 table is
 * separate because its key is a byte prefix rather than a fixed-width word. */
static census_slot state_census[CENSUS];
static census_slot state_census_x86[CENSUS];

static void census_record(census_slot *table, uint64_t key, int head) {
    uint32_t slot = (uint32_t)((key * UINT64_C(11400714819323198485)) >> 32) % CENSUS;
    for (uint32_t probe = 0; probe < CENSUS; probe++) {
        uint32_t index = (slot + probe) % CENSUS;
        uint32_t expected = 0;
        if (!atomic_load(&table[index].used)) {
            if (!atomic_compare_exchange_strong(&table[index].used, &expected, 1)) {
                if (atomic_load(&table[index].key) != key) continue;
            } else {
                atomic_store(&table[index].key, key);
            }
        } else if (atomic_load(&table[index].key) != key) {
            continue;
        }
        if (head) atomic_fetch_add(&table[index].head, 1);
        else atomic_fetch_add(&table[index].body, 1);
        return;
    }
}

static int census_report(const census_slot *table, uint32_t index, uint64_t *key, uint64_t *head,
                         uint64_t *body) {
    if (index >= CENSUS || !atomic_load(&table[index].used)) return 0;
    if (key != NULL) *key = atomic_load(&table[index].key);
    if (head != NULL) *head = atomic_load(&table[index].head);
    if (body != NULL) *body = atomic_load(&table[index].body);
    return 1;
}

void hl_native_state_untranslatable(uint32_t word, int head) {
    census_record(state_census, word, head);
}

void hl_native_state_untranslatable_x86(const uint8_t *bytes, size_t length, int head) {
    uint64_t key = 0;
    size_t index;

    if (bytes == NULL) return;
    if (length > 8u) length = 8u;
    for (index = 0; index < length; index++) key |= (uint64_t)bytes[index] << (index * 8u);
    census_record(state_census_x86, key, head);
}

__attribute__((destructor)) static void state_census_dump(void) {
    if (getenv("HL_NATIVE_CENSUS") == NULL) return;
    for (uint32_t index = 0; index < CENSUS; index++) {
        uint64_t key = 0, head = 0, body = 0;
        if (!census_report(state_census, index, &key, &head, &body)) continue;
        fprintf(stderr, "CENSUS word=%08x head=%llu body=%llu\n", (unsigned)key,
                (unsigned long long)head, (unsigned long long)body);
    }
    for (uint32_t index = 0; index < CENSUS; index++) {
        uint64_t key = 0, head = 0, body = 0;
        if (!census_report(state_census_x86, index, &key, &head, &body)) continue;
        fprintf(stderr, "CENSUS x86=%016llx head=%llu body=%llu\n", (unsigned long long)key,
                (unsigned long long)head, (unsigned long long)body);
    }
}

int hl_native_state_untranslatable_report(uint32_t index, uint32_t *word, uint64_t *head,
                                          uint64_t *body) {
    uint64_t key = 0;
    if (!census_report(state_census, index, &key, head, body)) return 0;
    if (word != NULL) *word = (uint32_t)key;
    return 1;
}

int hl_native_state_untranslatable_x86_report(uint32_t index, uint64_t *bytes, uint64_t *head,
                                              uint64_t *body) {
    return census_report(state_census_x86, index, bytes, head, body);
}
