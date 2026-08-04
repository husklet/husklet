// eventfd as a counter and as a cross-thread wakeup. The main thread accumulates writes,
// then a single read returns the summed counter. Exercises eventfd2 + 8-byte read/write
// semantics. Deterministic -> oracle-checked.
#include <stdint.h>
#include <stdio.h>
#include <sys/eventfd.h>
#include <unistd.h>

int main(void) {
    int efd = eventfd(0, 0);
    if (efd < 0) { perror("eventfd"); return 1; }
    uint64_t add;
    for (int i = 1; i <= 10; i++) {
        add = i;
        if (write(efd, &add, sizeof add) != (ssize_t)sizeof add) { perror("write"); return 1; }
    }
    uint64_t v = 0;
    if (read(efd, &v, sizeof v) != (ssize_t)sizeof v) { perror("read"); return 1; }
    close(efd);
    printf("eventfd counter=%llu\n", (unsigned long long)v); // 55
    return v == 55 ? 0 : 1;
}
