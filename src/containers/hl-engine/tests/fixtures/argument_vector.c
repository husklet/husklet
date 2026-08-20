// Reports whether the loader handed this process the COMPLETE argument vector.
// argv[1] carries the expected argc as a decimal string; every remaining argument
// is its own index, so a truncated, reordered, or overrun vector is detectable
// from inside the guest without trusting the engine's own accounting.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 2) return 10;
    long expected = strtol(argv[1], NULL, 10);
    if (expected != argc) {
        fprintf(stderr, "argc=%d expected=%ld\n", argc, expected);
        return 11;
    }
    for (int index = 2; index < argc; index++) {
        char slot[32];
        snprintf(slot, sizeof slot, "%d", index);
        if (strcmp(argv[index], slot) != 0) {
            fprintf(stderr, "argv[%d]=%s\n", index, argv[index]);
            return 12;
        }
    }
    return 0;
}
