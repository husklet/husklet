// longjmp out of deep nested loops, then re-enter the same region -- repeatedly. Non-local exit
// unwinds the guest stack past many chained blocks; re-entering re-drives translation of the same
// blocks with a freshly restored context. A DBT that mishandles the sp/pc restore or leaves stale
// chained state produces a wrong count or crashes. Bounded rounds, deterministic checksum.
#include <setjmp.h>
#include <stdint.h>
#include <stdio.h>

static jmp_buf env;
static volatile uint64_t sink;

static void deep(uint64_t seed, int depth) {
    volatile uint64_t local[8];
    for (int i = 0; i < 8; i++) local[i] = seed ^ (uint64_t)(depth * 131 + i);
    if (depth > 40) {
        sink = local[seed & 7];
        longjmp(env, (int)((seed & 0x7fffffff) | 1)); // non-local exit from depth ~40
    }
    for (uint64_t k = 0; k < 3; k++) deep(seed + k + (uint64_t)depth, depth + 1);
}

int main(void) {
    uint64_t acc = 0;
    for (uint64_t round = 0; round < 200000ULL; round++) {
        int rc = setjmp(env);
        if (rc == 0) {
            deep(round * 0x9e3779b9ULL + 1, 0); // dives, then longjmps back here
        } else {
            acc = acc * 1000003ULL + (uint64_t)rc + sink; // re-entry path
        }
    }
    printf("longjmp-reenter acc=%llu\n", (unsigned long long)acc);
    return 0;
}
