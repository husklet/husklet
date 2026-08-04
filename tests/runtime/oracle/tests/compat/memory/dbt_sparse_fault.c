// Large sparse anonymous mapping: reserve 1 GiB, then touch one word per page at a large prime stride
// so pages fault in a scattered order (demand-paging + page-table churn under the DBT's guest-memory
// shadow). Every touched word is written with a position-derived value and summed back, so a dropped
// or aliased fault surfaces as a wrong checksum. Bounded, deterministic.
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    size_t page = 4096;
    size_t pages = 262144;          // 1 GiB reservation
    size_t len = pages * page;
    unsigned char *m = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) {
        perror("mmap");
        return 1;
    }
    uint64_t sum = 0;
    size_t idx = 1;
    const size_t stride = 49157; // prime, coprime with pages -> visits every page once
    for (size_t n = 0; n < pages; n++) {
        idx = (idx + stride) % pages;
        volatile uint64_t *slot = (volatile uint64_t *)(m + idx * page);
        uint64_t v = (uint64_t)idx * 0x9e3779b97f4a7c15ULL + 1;
        *slot = v;         // fault the page in on write
        sum += *slot ^ v;  // must read back exactly what was written -> contributes 0 if coherent
        sum = sum * 1000003ULL + (*slot >> 17);
    }
    munmap(m, len);
    printf("sparse-fault sum=%llu\n", (unsigned long long)sum);
    return 0;
}
