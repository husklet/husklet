#define _GNU_SOURCE
#include <stdint.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#if defined(__x86_64__)
static sigjmp_buf fault_pad;
static volatile sig_atomic_t faulted;

static void fault_handler(int signal_) {
    (void)signal_;
    faulted = 1;
    siglongjmp(fault_pad, 1);
}

static int unchanged(const unsigned char *bytes, size_t first, size_t last, unsigned char expected) {
    for (size_t index = first; index < last; ++index)
        if (bytes[index] != expected) return 0;
    return 1;
}

int main(void) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    unsigned char *area = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (area == MAP_FAILED) return 2;
    memset(area, 0xa5, 832);
    __asm__ volatile("fld1\n\tpcmpeqd %%xmm0, %%xmm0" : : : "xmm0", "memory");
    __asm__ volatile("xsave64 (%0)" : : "r"(area), "a"(3u), "d"(0u) : "memory");
    uint64_t loaded_bv;
    memcpy(&loaded_bv, area + 512, sizeof(loaded_bv));
    volatile uint64_t bv = loaded_bv;
    volatile int saved_fcw = area[0] == 0x7f && area[1] == 0x03;
    volatile int saved_bv = (bv & 3u) == 3u;
    volatile int saved = saved_fcw && saved_bv;
    volatile int selective = unchanged(area, 416, 512, 0xa5) && unchanged(area, 513, 832, 0xa5);

    unsigned char fx[512] __attribute__((aligned(16)));
    memset(fx, 0, sizeof(fx));
    __asm__ volatile("fxsave64 %0" : "=m"(fx) : : "memory");
    volatile int neighbor = fx[0] == 0x7f && fx[1] == 0x03;

    struct sigaction action = {0};
    action.sa_handler = fault_handler;
    sigemptyset(&action.sa_mask);
    sigaction(SIGSEGV, &action, NULL);
    faulted = 0;
    if (sigsetjmp(fault_pad, 1) == 0) __asm__ volatile("xsave64 (%0)" : : "r"(area + 1), "a"(3u), "d"(0u) : "memory");
    volatile int alignment = faulted != 0;

    unsigned char *edge = area + page - 512;
    memset(edge, 0xa5, 512);
    if (mprotect(area + page, page, PROT_NONE) != 0) return 3;
    faulted = 0;
    if (sigsetjmp(fault_pad, 1) == 0) __asm__ volatile("xsave64 (%0)" : : "r"(edge), "a"(3u), "d"(0u) : "memory");
    int atomic_fault = faulted != 0 && unchanged(edge, 0, 512, 0xa5);

    printf("x86-projection-xsave saved=%d selective=%d aligned-fault=%d atomic-fault=%d neighbor=%d\n", saved,
           selective, alignment, atomic_fault, neighbor);
    return (!saved_fcw ? 1 : 0) | (!selective ? 2 : 0) | (!alignment ? 4 : 0) | (!atomic_fault ? 8 : 0) |
           (!neighbor ? 16 : 0) | (!saved_bv ? 32 : 0);
}
#else
int main(void) {
    return 0;
}
#endif
