/* The past-EOF SIGBUS ledger arms and disarms as mappings come and go, and the
   translator emits its memory guards from that state.  Code translated while
   the ledger is empty carries no guard, so a later arm must invalidate it: this
   drives the ledger through several arm/disarm cycles and re-uses ONE hot,
   already-translated accessor across each transition.

   touch_byte() is deliberately noinline and address-taken so every call reaches
   the same guest block.  It is made hot against ordinary anonymous memory while
   the ledger is empty, and is then pointed at a genuinely past-EOF page.  If
   the arm did not discard the unguarded translation, the load returns zero
   instead of raising SIGBUS and `rearm` reads 0. */
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CYCLES 4
#define WARM 20000

static sigjmp_buf jump;
static volatile unsigned char *fault_address;
static volatile sig_atomic_t metadata_ok;
static volatile sig_atomic_t bus_count;

static void on_bus(int number, siginfo_t *info, void *context) {
    (void)number;
    (void)context;
    bus_count++;
    if (info != NULL && info->si_code == BUS_ADRERR && info->si_addr == (void *)fault_address) metadata_ok++;
    siglongjmp(jump, 1);
}

__attribute__((noinline)) static unsigned touch_byte(volatile unsigned char *address) {
    unsigned total = 0;
    total += *address;
    return total;
}

static unsigned (*const touch)(volatile unsigned char *) = touch_byte;

static int catches(volatile unsigned char *address) {
    fault_address = address;
    if (sigsetjmp(jump, 1) == 0) {
        (void)touch(address);
        return 0;
    }
    return 1;
}

static int quiet(volatile unsigned char *address, unsigned char expected) {
    fault_address = address;
    if (sigsetjmp(jump, 1) == 0) return touch(address) == expected;
    return 0;
}

/* Make the accessor's block hot -- and, while the ledger is empty, translated
   without a memory guard. */
static unsigned warm(volatile unsigned char *scratch) {
    unsigned total = 0;
    for (int i = 0; i < WARM; ++i)
        total += touch(scratch + (i & 0xfff));
    return total;
}

int main(void) {
    char path[] = "/tmp/hl-bus-rearm-XXXXXX";
    unsigned char source[5000];
    struct sigaction action = {0};
    int rearm = 0, disarmed_quiet = 0, guarded_quiet = 0, cycles = 0;
    int fd = mkstemp(path);
    if (fd < 0) return 1;
    memset(source, 0x27, sizeof source);
    if (write(fd, source, sizeof source) != (ssize_t)sizeof source) return 2;
    action.sa_sigaction = on_bus;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGBUS, &action, NULL) != 0) return 3;

    unsigned char *scratch = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (scratch == MAP_FAILED) return 4;

    rearm = 1;
    disarmed_quiet = 1;
    guarded_quiet = 1;
    for (int cycle = 0; cycle < CYCLES; ++cycle) {
        /* Ledger empty here: the accessor is (re)translated without a guard. */
        (void)warm(scratch);
        if (!quiet(scratch + 4096, 0)) disarmed_quiet = 0;

        /* Arm: a fresh past-EOF file mapping.  This must invalidate the
           unguarded translation above. */
        unsigned char *map = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
        if (map == MAP_FAILED) return 5;
        if (!quiet(map, 0x27)) guarded_quiet = 0;
        if (!catches(map + 8192)) rearm = 0;

        /* Park the tail, then unpark it: the guarded translation must still
           fault through the same hot block after the round trip. */
        if (mprotect(map + 8192, 8192, PROT_NONE) != 0) return 6;
        if (!quiet(scratch, 0)) disarmed_quiet = 0;
        if (mprotect(map + 8192, 8192, PROT_READ | PROT_WRITE) != 0) return 7;
        if (!catches(map + 8192)) rearm = 0;

        /* Disarm: dropping the only past-EOF mapping empties the ledger. */
        if (munmap(map, 65536) != 0) return 8;
        if (!quiet(scratch + 8192, 0)) disarmed_quiet = 0;
        cycles++;
    }

    printf("bus-rearm cycles=%d rearm=%d disarmed-quiet=%d guarded-quiet=%d metadata=%d signals=%d\n", cycles, rearm,
           disarmed_quiet, guarded_quiet, metadata_ok == bus_count, (int)bus_count);
    munmap(scratch, 65536);
    close(fd);
    unlink(path);
    return cycles == CYCLES && rearm && disarmed_quiet && guarded_quiet && metadata_ok == bus_count &&
                   bus_count == CYCLES * 2
               ? 0
               : 9;
}
