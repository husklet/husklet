/* mprotect(PROT_NONE) makes a past-EOF file mapping unreachable, so Linux can
   no longer raise SIGBUS there.  Restoring an accessible protection restores
   the SIGBUS contract, and it must survive every way the range can be
   re-covered in between (anonymous overmap, file shrink, file regrowth). */
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

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

static int catches_load(volatile unsigned char *address) {
    fault_address = address;
    if (sigsetjmp(jump, 1) == 0) {
        (void)*address;
        return 0;
    }
    return 1;
}

static int reads_quietly(volatile unsigned char *address, unsigned char expected) {
    fault_address = address;
    if (sigsetjmp(jump, 1) == 0) return *address == expected;
    return 0;
}

int main(void) {
    char path[] = "/tmp/hl-protnone-bus-XXXXXX";
    unsigned char source[5000];
    struct sigaction action = {0};
    int fd = mkstemp(path);
    if (fd < 0) return 1;
    memset(source, 0x3c, sizeof source);
    if (write(fd, source, sizeof source) != (ssize_t)sizeof source) return 2;
    action.sa_sigaction = on_bus;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGBUS, &action, NULL) != 0) return 3;

    unsigned char *map = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    if (map == MAP_FAILED) return 4;

    /* The past-EOF tail SIGBUSes to begin with. */
    int armed = catches_load(map + 8192);

    /* PROT_NONE, then back: the bytes are still past EOF, so SIGBUS returns. */
    if (mprotect(map + 8192, 8192, PROT_NONE) != 0) return 5;
    if (mprotect(map + 8192, 8192, PROT_READ | PROT_WRITE) != 0) return 6;
    int restored = catches_load(map + 8192);

    /* A partial restore re-arms only the part it covers; the rest stays
       unreachable and is re-armed by its own later restore. */
    if (mprotect(map + 8192, 8192, PROT_NONE) != 0) return 7;
    if (mprotect(map + 8192, 4096, PROT_READ) != 0) return 8;
    int partial_low = catches_load(map + 8192);
    if (mprotect(map + 12288, 4096, PROT_READ) != 0) return 9;
    int partial_high = catches_load(map + 12288);

    /* An anonymous overmap of a parked range is ordinary memory: restoring an
       accessible protection over it must NOT resurrect the old verdict. */
    if (mprotect(map + 16384, 8192, PROT_NONE) != 0) return 10;
    if (mmap(map + 16384, 8192, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) != map + 16384)
        return 11;
    if (mprotect(map + 16384, 8192, PROT_READ | PROT_WRITE) != 0) return 12;
    map[16384] = 0x5a;
    int overmapped = reads_quietly(map + 16384, 0x5a) && reads_quietly(map + 20480, 0);

    /* Growing the file back over a parked range makes it valid again. */
    if (mprotect(map + 24576, 8192, PROT_NONE) != 0) return 13;
    if (ftruncate(fd, 28672) != 0) return 14;
    if (mprotect(map + 24576, 8192, PROT_READ | PROT_WRITE) != 0) return 15;
    int regrown = reads_quietly(map + 24576, 0);
    int beyond = catches_load(map + 28672);

    printf("protnone-bus armed=%d restored=%d partial-low=%d partial-high=%d overmapped=%d regrown=%d beyond=%d "
           "metadata=%d signals=%d\n",
           armed, restored, partial_low, partial_high, overmapped, regrown, beyond, metadata_ok == bus_count,
           (int)bus_count);
    munmap(map, 65536);
    close(fd);
    unlink(path);
    return armed && restored && partial_low && partial_high && overmapped && regrown && beyond &&
                   metadata_ok == bus_count && bus_count == 5
               ? 0
               : 16;
}
