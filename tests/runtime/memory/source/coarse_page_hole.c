/* Coarse-host-page memory semantics under the soft-memory TLB.
 * A guest 4 KiB munmap inside a 16 KiB host page cannot be honoured by the
 * host, so the backing stays mapped and the logical hole is enforced in
 * software.  Pins: the surviving neighbours stay readable/writable with the
 * right bytes, and the hole faults. */
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <setjmp.h>
#include <sys/mman.h>
#include <unistd.h>

static sigjmp_buf jump;
static volatile int caught;
static void on_segv(int signal_number) {
    (void)signal_number;
    caught = 1;
    siglongjmp(jump, 1);
}

#define PAGE 4096u
#define SPAN (16u * PAGE)

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    unsigned char *p = mmap(NULL, SPAN, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) { printf("mmap failed\n"); return 1; }
    for (unsigned i = 0; i < SPAN; ++i) p[i] = (unsigned char)(i & 0xff);
    /* Sub-host-page unmap of exactly one guest page. */
    if (munmap(p + PAGE, PAGE) != 0) { printf("munmap failed\n"); return 1; }

    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_handler = on_segv;
    sigaction(SIGSEGV, &action, NULL);
    sigaction(SIGBUS, &action, NULL);

    /* Many live pages touched in one block: the single-entry TLB thrashed here. */
    unsigned long sum = 0;
    for (int round = 0; round < 64; ++round)
        for (unsigned page = 0; page < 16; ++page) {
            if (page == 1) continue;
            unsigned char *q = p + page * PAGE + (unsigned)round;
            sum += *q;
            *q = (unsigned char)((page * PAGE + (unsigned)round) & 0xff);
            sum += *q;
        }
    printf("live sum=%lu\n", sum);

    /* Every surviving byte must still read its written value. */
    unsigned bad = 0;
    for (unsigned page = 0; page < 16; ++page) {
        if (page == 1) continue;
        for (unsigned offset = 128; offset < PAGE; offset += 512)
            if (p[page * PAGE + offset] != (unsigned char)((page * PAGE + offset) & 0xff)) ++bad;
    }
    printf("bad=%u\n", bad);

    /* Unaligned width crossing two LIVE pages still resolves. */
    unsigned long long wide = 0;
    caught = 0;
    if (sigsetjmp(jump, 1) == 0) {
        memcpy(&wide, p + 3 * PAGE - 3, sizeof wide);
        printf("cross_live=%llx\n", (unsigned long long)wide);
    } else {
        printf("cross_live=FAULT\n");
    }

    /* The unmapped guest page must fault, both read and write. */
    caught = 0;
    if (sigsetjmp(jump, 1) == 0) {
        volatile unsigned char v = p[PAGE + 17];
        (void)v;
        printf("hole_read=NOFAULT\n");
    } else {
        printf("hole_read=FAULT\n");
    }
    caught = 0;
    if (sigsetjmp(jump, 1) == 0) {
        p[PAGE + 2000] = 7;
        printf("hole_write=NOFAULT\n");
    } else {
        printf("hole_write=FAULT\n");
    }
    /* A page above the hole is still live after the faults. */
    printf("after=%u\n", (unsigned)p[3 * PAGE + 5]);
    return 0;
}
