// A guest store into its own read-only PT_LOAD must raise SIGSEGV/SEGV_ACCERR at the exact byte, and the
// bytes must survive. The loader registers those segments read-only; if it does not also PROTECT them the
// store lands on a writable page and the guest is told its const data changed when it did not -- which is
// how every W^X / const-correctness self-test silently passes. Scalar and bulk stores are both checked
// because they take different paths through the engine (direct store vs the string-op helper).
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static const unsigned char ro_page[8192] __attribute__((aligned(4096))) = {0x11, 0x22, 0x33, 0x44};

static sigjmp_buf pad;
static volatile int hit, code;
static volatile uintptr_t addr;
// The compiler folds a load from a const object; route every access through a pointer it cannot trace.
static const unsigned char *volatile opaque = ro_page;

static void onfault(int signal_number, siginfo_t *info, void *context) {
    (void)signal_number;
    (void)context;
    hit = 1;
    code = info->si_code;
    addr = (uintptr_t)info->si_addr;
    siglongjmp(pad, 1);
}

static void arm(void) {
    hit = 0;
    code = 0;
    addr = 0;
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = onfault;
    action.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigaction(SIGSEGV, &action, NULL);
    sigaction(SIGBUS, &action, NULL);

    const unsigned char *base = opaque;
    printf("read %02x%02x%02x%02x\n", base[0], base[1], base[2], base[3]);

    arm();
    if (sigsetjmp(pad, 1) == 0) *(volatile unsigned char *)(base + 4096) = 0xAA;
    printf("scalar sig=%d code=%d off=%ld\n", hit, code, hit ? (long)(addr - (uintptr_t)base) : -1L);

    // Bulk stores land inside [5120,5376) but which byte faults first depends on the libc's vectorization,
    // so report containment rather than the exact offset.
    arm();
    if (sigsetjmp(pad, 1) == 0) memset((void *)(base + 5120), 0xBB, 256);
    printf("memset sig=%d code=%d in=%d\n", hit, code,
           hit && addr >= (uintptr_t)base + 5120 && addr < (uintptr_t)base + 5376);

    static unsigned char source[256];
    memset(source, 0xCC, sizeof source);
    arm();
    if (sigsetjmp(pad, 1) == 0) memcpy((void *)(base + 6144), source, sizeof source);
    printf("memcpy sig=%d code=%d in=%d\n", hit, code,
           hit && addr >= (uintptr_t)base + 6144 && addr < (uintptr_t)base + 6400);

    unsigned dirty = 0;
    for (size_t i = 4096; i < sizeof ro_page; i++)
        dirty += opaque[i] != 0;
    printf("dirty %u\n", dirty);

    // The page is read-only, not broken: the guest's own mprotect still opens it.
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    int opened = mprotect((void *)(base + 4096), page, PROT_READ | PROT_WRITE) == 0;
    arm();
    if (sigsetjmp(pad, 1) == 0) *(volatile unsigned char *)(base + 4096) = 0xAA;
    printf("reopen ok=%d sig=%d value=%02x\n", opened, hit, opaque[4096]);
    return 0;
}
