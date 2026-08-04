#define _GNU_SOURCE
#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>
#include <stdint.h>

static sigjmp_buf jump;
static volatile sig_atomic_t armed;

static void fault(int signo) {
    (void)signo;
    if (armed) siglongjmp(jump, 1);
}

static int read_fault(volatile unsigned char *p) {
    armed = 1;
    if (sigsetjmp(jump, 1) == 0) {
        volatile unsigned char value = *p;
        (void)value;
        armed = 0;
        return 0;
    }
    armed = 0;
    return 1;
}

static int write_fault(volatile unsigned char *p) {
    armed = 1;
    if (sigsetjmp(jump, 1) == 0) {
        *p = 0x5a;
        armed = 0;
        return 0;
    }
    armed = 0;
    return 1;
}

static void emit_return(unsigned char *p, int value) {
#if defined(__aarch64__)
    uint32_t *w = (uint32_t *)p;
    w[0] = 0x52800000u | ((uint32_t)(value & 0xffff) << 5);
    w[1] = 0xd65f03c0u;
#elif defined(__x86_64__)
    p[0] = 0xb8;
    p[1] = (unsigned char)value;
    p[2] = (unsigned char)(value >> 8);
    p[3] = (unsigned char)(value >> 16);
    p[4] = (unsigned char)(value >> 24);
    p[5] = 0xc3;
#endif
}

static int smc_toggle(const size_t page) {
    unsigned char *p = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) return 0;
    int ok = 1;
    for (int i = 1; i <= 8; i++) {
        ok &= mprotect(p, page, PROT_READ | PROT_WRITE) == 0;
        emit_return(p, 100 + i);
        __builtin___clear_cache((char *)p, (char *)p + page);
        ok &= mprotect(p, page, PROT_READ | PROT_EXEC) == 0;
        int (*fn)(void) = (int (*)(void))p;
        ok &= fn() == 100 + i;
    }
    munmap(p, page);
    return ok;
}

int main(void) {
    struct sigaction sa = {0};
    const size_t page = 4096;
    int none_faults = 0, read_write_faults = 0, recovered = 1, toggles = 32;
    sa.sa_handler = fault;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);

    volatile unsigned char *a = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    volatile unsigned char *b = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (a == MAP_FAILED || b == MAP_FAILED) return 2;
    a[0] = 0x11;
    a[page - 1] = 0x22;
    b[0] = 0x33;

    errno = 0;
    int unaligned = mprotect((void *)(a + 1), page, PROT_NONE) == -1 && errno == EINVAL;
    for (int i = 0; i < toggles; i++) {
        recovered &= mprotect((void *)a, 1, PROT_NONE) == 0;
        none_faults += read_fault(a);
        recovered &= mprotect((void *)a, page, PROT_READ) == 0;
        recovered &= a[0] == 0x11 && a[page - 1] == 0x22 && b[0] == 0x33;
        read_write_faults += write_fault(a);
        recovered &= mprotect((void *)a, page, PROT_READ | PROT_WRITE) == 0;
        a[0] = 0x11;
    }
    printf("mprotect enforce none=%d ro=%d recover=%d peer=%d unaligned=%d smc=%d\n", none_faults == toggles,
           read_write_faults == toggles, recovered, b[0] == 0x33, unaligned, smc_toggle(page));
    return 0;
}
