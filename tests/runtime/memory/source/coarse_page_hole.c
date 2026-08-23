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

/* An access that BEGINS in a live page and RUNS INTO the hole must fault too.
 * The host page is still mapped underneath (macOS 16 KiB granule), so nothing
 * but the software ledger can refuse it; on native Linux the page is genuinely
 * gone and the same transcript falls out of the hardware, which is what makes
 * this a portable oracle.  Both directions (live->hole across the low edge and
 * hole->live across the high edge) and several widths, as a load and a store. */
#define CROSS_LOAD(label, address, type, warm)                                                                         \
    do {                                                                                                               \
        caught = 0;                                                                                                    \
        if (warm) sink += *(const volatile unsigned char *)(warm); /* warm the start page's guard entry */             \
        if (sigsetjmp(jump, 1) == 0) {                                                                                 \
            type value;                                                                                                \
            memcpy(&value, (const void *)(address), sizeof value);                                                     \
            sink += (unsigned long long)value;                                                                         \
            printf(label "=NOFAULT\n");                                                                                \
        } else {                                                                                                       \
            printf(label "=FAULT\n");                                                                                  \
        }                                                                                                              \
    } while (0)

#define CROSS_STORE(label, address, type, warm)                                                                        \
    do {                                                                                                               \
        caught = 0;                                                                                                    \
        if (warm) sink += *(const volatile unsigned char *)(warm); /* warm the start page's guard entry */             \
        if (sigsetjmp(jump, 1) == 0) {                                                                                 \
            type value = (type)0x5a5a5a5a5a5a5a5aULL;                                                                  \
            memcpy((void *)(address), &value, sizeof value);                                                           \
            printf(label "=NOFAULT\n");                                                                                \
        } else {                                                                                                       \
            printf(label "=FAULT\n");                                                                                  \
        }                                                                                                              \
    } while (0)

/* The compiler is free to lower an 8-byte constant store as two 4-byte stores
 * (gcc does exactly that for the repeated-byte pattern above), so the C cases
 * do not by themselves pin a single ARCHITECTURAL access that crosses the
 * boundary -- the shape the guard's span/bounce paths exist for.  Spell those
 * in assembly so the width in the instruction is the width the guard sees. */
static unsigned long long cross_load8(const volatile void *address) {
    unsigned long long value;
#if defined(__aarch64__)
    __asm__ __volatile__("ldr %x0,[%1]" : "=r"(value) : "r"(address) : "memory");
#elif defined(__x86_64__)
    __asm__ __volatile__("movq (%1),%0" : "=r"(value) : "r"(address) : "memory");
#else
    memcpy(&value, (const void *)address, sizeof value);
#endif
    return value;
}

static void cross_store8(volatile void *address, unsigned long long value) {
#if defined(__aarch64__)
    __asm__ __volatile__("str %x0,[%1]" : : "r"(value), "r"(address) : "memory");
#elif defined(__x86_64__)
    __asm__ __volatile__("movq %0,(%1)" : : "r"(value), "r"(address) : "memory");
#else
    memcpy((void *)address, &value, sizeof value);
#endif
}

#define ONE_INSTRUCTION(label, body)                                                                                   \
    do {                                                                                                               \
        caught = 0;                                                                                                    \
        if (sigsetjmp(jump, 1) == 0) {                                                                                 \
            body;                                                                                                      \
            printf(label "=NOFAULT\n");                                                                                \
        } else {                                                                                                       \
            printf(label "=FAULT\n");                                                                                  \
        }                                                                                                              \
    } while (0)

static sigjmp_buf jump;
static volatile int caught;
static volatile unsigned long long sink;

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
    if (p == MAP_FAILED) {
        printf("mmap failed\n");
        return 1;
    }
    for (unsigned i = 0; i < SPAN; ++i)
        p[i] = (unsigned char)(i & 0xff);
    /* Sub-host-page unmap of exactly one guest page. */
    if (munmap(p + PAGE, PAGE) != 0) {
        printf("munmap failed\n");
        return 1;
    }

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
    /* live -> hole: the access starts in page 0 and ends inside the hole. */
    CROSS_LOAD("low_load2", p + PAGE - 1, unsigned short, p);
    CROSS_LOAD("low_load4", p + PAGE - 2, unsigned int, p);
    CROSS_LOAD("low_load8", p + PAGE - 4, unsigned long long, p);
    CROSS_STORE("low_store2", p + PAGE - 1, unsigned short, p);
    CROSS_STORE("low_store4", p + PAGE - 2, unsigned int, p);
    CROSS_STORE("low_store8", p + PAGE - 4, unsigned long long, p);
    /* hole -> live: the access starts inside the hole and ends in page 2. */
    CROSS_LOAD("high_load2", p + 2 * PAGE - 1, unsigned short, NULL);
    CROSS_LOAD("high_load4", p + 2 * PAGE - 2, unsigned int, NULL);
    CROSS_LOAD("high_load8", p + 2 * PAGE - 4, unsigned long long, NULL);
    CROSS_STORE("high_store2", p + 2 * PAGE - 1, unsigned short, NULL);
    CROSS_STORE("high_store4", p + 2 * PAGE - 2, unsigned int, NULL);
    CROSS_STORE("high_store8", p + 2 * PAGE - 4, unsigned long long, NULL);

    /* One instruction, eight bytes, straddling the edge: the guard's span path
       sees the whole width at once and grants ONE host delta for it, so the
       ledger has to be consulted for the complete access. */
    sink += *(const volatile unsigned char *)p;
    ONE_INSTRUCTION("edge_load8", sink += cross_load8(p + PAGE - 4));
    sink += *(const volatile unsigned char *)p;
    ONE_INSTRUCTION("edge_store8", cross_store8(p + PAGE - 4, 0x0102030405060708ULL));
    sink += *(const volatile unsigned char *)(p + 2 * PAGE);
    ONE_INSTRUCTION("edge_high_load8", sink += cross_load8(p + 2 * PAGE - 4));
    sink += *(const volatile unsigned char *)(p + 2 * PAGE);
    ONE_INSTRUCTION("edge_high_store8", cross_store8(p + 2 * PAGE - 4, 0x0102030405060708ULL));

    /* A page that was LIVE long enough to leave a guard entry behind, then
       unmapped sub-host-page: the standing grant must not outlive the unmap. */
    sink += *(const volatile unsigned char *)(p + 5 * PAGE + 8);
    p[5 * PAGE + 8] = 3;
    if (munmap(p + 5 * PAGE, PAGE) != 0) {
        printf("munmap of the warmed page failed\n");
        return 1;
    }
    ONE_INSTRUCTION("warmed_read", sink += *(const volatile unsigned char *)(p + 5 * PAGE + 8));
    ONE_INSTRUCTION("warmed_write", p[5 * PAGE + 8] = 4);
    ONE_INSTRUCTION("warmed_load8", sink += cross_load8(p + 5 * PAGE + 8));
    ONE_INSTRUCTION("warmed_store8", cross_store8(p + 5 * PAGE + 8, 0x0102030405060708ULL));

    /* A page above the hole is still live after the faults. */
    printf("after=%u\n", (unsigned)p[3 * PAGE + 5]);
    return 0;
}
