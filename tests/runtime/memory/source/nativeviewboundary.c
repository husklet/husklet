// Drives the edge of a projection view from a natively translated hot loop.
// The walk probes report how far past a mapping the loop got before faulting,
// and the alias probe reports whether stores just past the mapping reached the
// neighbour's own backing, so a view widened past its region is visible either
// as a fault that arrives a page late or as a store that missed the alias.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static sigjmp_buf escape;
static volatile unsigned long offset;
static volatile unsigned long sink;

static void onfault(int signal) {
    (void)signal;
    siglongjmp(escape, 1);
}

// Warmed by repeated in-bounds passes so the loop body is translated before the
// pass that runs off the end; a single straight-line walk never qualifies.
static unsigned long walk_writes(volatile unsigned char *base, unsigned long limit) {
    if (sigsetjmp(escape, 1) == 0) {
        for (offset = 0; offset < limit; offset++) {
            base[offset] = (unsigned char)(offset & 0xff);
        }
        return limit;
    }
    return offset;
}

static unsigned long walk_reads(volatile unsigned char *base, unsigned long limit) {
    if (sigsetjmp(escape, 1) == 0) {
        unsigned long total = 0;
        for (offset = 0; offset < limit; offset++) {
            total += base[offset];
        }
        sink = total;
        return limit;
    }
    return offset;
}

static int walk_probes(unsigned long page) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_handler = onfault;
    action.sa_flags = SA_NODEFER;
    if (sigaction(SIGSEGV, &action, NULL) != 0 || sigaction(SIGBUS, &action, NULL) != 0) {
        printf("sigaction failed\n");
        return 1;
    }
    // An unmapped hole rather than a reprotected page, so the projection view
    // and not the host protection is what has to stop the loop.
    unsigned char *write_base = mmap(NULL, 4 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (write_base == MAP_FAILED || munmap(write_base + page, 3 * page) != 0) {
        printf("write mapping failed\n");
        return 1;
    }
    for (int warm = 0; warm < 64; warm++) {
        if (walk_writes(write_base, page) != page) {
            printf("warm write walk faulted early at %lu\n", offset);
            return 1;
        }
    }
    printf("write reach %lu pages\n", walk_writes(write_base, 4 * page) / page);

    unsigned char *read_base = mmap(NULL, 4 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (read_base == MAP_FAILED || munmap(read_base + page, 3 * page) != 0) {
        printf("read mapping failed\n");
        return 1;
    }
    for (int warm = 0; warm < 64; warm++) {
        if (walk_reads(read_base, page) != page) {
            printf("warm read walk faulted early at %lu\n", offset);
            return 1;
        }
    }
    printf("read reach %lu pages\n", walk_reads(read_base, 4 * page) / page);
    return 0;
}

// A shared page immediately after a private one, aliased by a second independent
// mapping. A widened private view resolves the shared page through the private
// arena, so the alias never observes the stores.
static int alias_probe(unsigned long page) {
    int fd = (int)syscall(SYS_memfd_create, "hl-view-boundary", 0);
    if (fd < 0 || ftruncate(fd, (long)page) != 0) {
        printf("memfd failed\n");
        return 1;
    }
    unsigned char *base = mmap(NULL, 3 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) {
        printf("alias reserve failed\n");
        return 1;
    }
    memset(base, 0x11, 3 * page);
    unsigned char *shared = mmap(base + page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, 0);
    unsigned char *alias = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (shared != base + page || alias == MAP_FAILED) {
        printf("alias mapping failed\n");
        return 1;
    }
    memset(shared, 0x22, page);

    // Straddle the boundary in one hot body so the private page keeps the loop
    // warm while the last four stores land in the shared page.
    volatile unsigned char *straddle = base + page - 4;
    for (unsigned long i = 0; i < 20000; i++) {
        for (unsigned k = 0; k < 8; k++) {
            straddle[k] = (unsigned char)(0x50 + k);
        }
    }
    printf("alias %02x %02x %02x %02x\n", alias[0], alias[1], alias[2], alias[3]);
    printf("private %02x %02x\n", base[page - 1], base[page - 2]);
    return 0;
}

// Stores wholly inside a shared page from a hot native body, read back through a
// second mapping of the same object. Only write publication reconciles the shared
// backing, so dropping it leaves the alias reading the pre-loop bytes.
static int shared_probe(unsigned long page) {
    int fd = (int)syscall(SYS_memfd_create, "hl-view-shared", 0);
    if (fd < 0 || ftruncate(fd, (long)page) != 0) {
        printf("shared memfd failed\n");
        return 1;
    }
    unsigned char *mapped = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    unsigned char *alias = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mapped == MAP_FAILED || alias == MAP_FAILED) {
        printf("shared mapping failed\n");
        return 1;
    }
    memset(mapped, 0x33, page);
    volatile unsigned char *store = mapped;
    for (unsigned long i = 0; i < 20000; i++) {
        for (unsigned k = 0; k < 64; k++) {
            store[k] = (unsigned char)(0x60 + k);
        }
    }
    printf("shared %02x %02x %02x\n", alias[0], alias[1], alias[63]);
    return 0;
}

int main(void) {
    unsigned long page = (unsigned long)sysconf(_SC_PAGESIZE);
    if (walk_probes(page) != 0) { return 1; }
    if (alias_probe(page) != 0) { return 1; }
    return shared_probe(page);
}
