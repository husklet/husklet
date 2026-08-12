#include "../src/state.h"

#include <stdio.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "tally:%d: %s\n", __LINE__, #x); return 1; } } while (0)

/* Worked example of the supported native counter path: tally against a string literal from
 * anywhere in the C engine and read it back, instead of an fprintf that fd 2 redirection eats. */
static uint64_t total(const char *wanted) {
    uint64_t sum = 0;
    for (uint32_t index = 0;; index++) {
        const char *name = NULL;
        uint64_t count = 0;
        if (!hl_native_tally_report(index, &name, &count)) break;
        if (name == wanted) sum += count;
    }
    return sum;
}

int main(void) {
    static const char *const fallback = "trace.fallback";
    static const char *const refusal = "trace.refusal";

    CHECK(total(fallback) == 0);
    hl_native_tally(fallback);
    hl_native_tally(fallback);
    hl_native_tally(refusal);
    hl_native_tally(NULL);
    CHECK(total(fallback) == 2);
    CHECK(total(refusal) == 1);
    return 0;
}
