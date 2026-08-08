// The patch store has to happen inside the hot loop, so that it is the native
// translation that writes the executable page. A publication that omits the
// executable range leaves the previous translation of the stub in place and the
// call returns a stale constant.
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define ROUNDS 60000

typedef int (*stub_fn)(void);

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    unsigned char *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE | PROT_EXEC,
                               MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    if (code == MAP_FAILED) {
        printf("pubcode mmap=0\n");
        return 1;
    }
#if defined(__aarch64__)
    uint32_t *words = (uint32_t *)code;
    words[1] = 0xd65f03c0u;
    size_t length = 8;
#elif defined(__x86_64__)
    code[0] = 0xb8;
    code[5] = 0xc3;
    size_t length = 6;
#else
#error "unsupported guest ISA"
#endif
    stub_fn stub;
    memcpy(&stub, &code, sizeof stub);
    long stale = 0;
    for (int round = 0; round < ROUNDS; round++) {
        unsigned want = (unsigned)(round & 0xffff);
#if defined(__aarch64__)
        words[0] = 0x52800000u | (want << 5);
#else
        memcpy(code + 1, &want, sizeof want);
#endif
        // Architecturally required on aarch64 and a no-op on x86-64, whose
        // instruction fetch is coherent with stores.
        __builtin___clear_cache((char *)code, (char *)code + length);
        if (stub() != (int)want) {
            stale++;
        }
    }
    munmap(code, (size_t)page);
    printf("pubcode rounds=%d stale=%ld\n", ROUNDS, stale);
    return 0;
}
