#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <sys/wait.h>
#include <ucontext.h>

static sigjmp_buf jump;
static volatile sig_atomic_t bus_count;
static volatile sig_atomic_t metadata_ok;
static volatile unsigned char *expected_address;
static volatile sig_atomic_t check_canaries;
static volatile sig_atomic_t canary_context_ok;

static void on_bus(int signal_number, siginfo_t *info, void *context) {
    (void)signal_number;
    if (info != NULL && info->si_code == BUS_ADRERR && info->si_addr == (void *)expected_address) metadata_ok++;
#if defined(__aarch64__)
    if (check_canaries) {
        ucontext_t *uc = context;
        if (uc->uc_mcontext.regs[0] == UINT64_C(0x101) && uc->uc_mcontext.regs[1] == UINT64_C(0x202) &&
            uc->uc_mcontext.regs[2] == UINT64_C(0x303))
            canary_context_ok = 1;
    }
#else
    if (check_canaries) canary_context_ok = 1;
#endif
    (void)context;
    bus_count++;
    siglongjmp(jump, 1);
}

static int guarded_canary_load(volatile unsigned char *base) {
#if defined(__aarch64__)
    unsigned result;
    __asm__ volatile("mov x17, %1\n"
                     "mov x0, #0x101\n"
                     "mov x1, #0x202\n"
                     "mov x2, #0x303\n"
                     "ldr w3, [x17, #16]\n"
                     "mov x4, #0x101\n"
                     "cmp x0, x4\n"
                     "mov x4, #0x202\n"
                     "ccmp x1, x4, #0, eq\n"
                     "mov x4, #0x303\n"
                     "ccmp x2, x4, #0, eq\n"
                     "cset %w0, eq\n"
                     : "=r"(result)
                     : "r"(base)
                     : "x0", "x1", "x2", "x3", "x4", "x17", "cc", "memory");
    return (int)result;
#else
    return base[16] == 0x3c;
#endif
}

static int expect_bus_load(volatile unsigned char *address) {
    expected_address = address;
    if (sigsetjmp(jump, 1) == 0) {
        (void)*address;
        return 0;
    }
    return 1;
}

static int expect_bus_store(volatile unsigned char *address) {
    expected_address = address;
    if (sigsetjmp(jump, 1) == 0) {
        *address = 1;
        return 0;
    }
    return 1;
}

int main(void) {
    struct sigaction action = {0};
    char path[] = "/tmp/hl-eof-bus-XXXXXX";
    unsigned char source[5000];
    int descriptor = mkstemp(path);
    memset(source, 0x3c, sizeof source);
    if (descriptor < 0 || write(descriptor, source, sizeof source) != (ssize_t)sizeof source) return 1;
    action.sa_sigaction = on_bus;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGBUS, &action, NULL) != 0) return 2;
    unsigned char *mapping = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE, descriptor, 0);
    if (mapping == MAP_FAILED) return 3;
    int partial_zero = mapping[4999] == 0x3c && mapping[5000] == 0 && mapping[8191] == 0;
    int canary_miss = guarded_canary_load(mapping + 4096);
    expected_address = mapping + 8192;
    check_canaries = 1;
    int canary_hit = 0;
    if (sigsetjmp(jump, 1) == 0)
        (void)guarded_canary_load(mapping + 8176);
    else
        canary_hit = canary_context_ok;
    check_canaries = 0;
    int load_bus = expect_bus_load(mapping + 8192);
    int far_bus = expect_bus_load(mapping + 12288);
    int store_bus = expect_bus_store(mapping + 8192);
    expected_address = mapping + 8192;
    int crossing_bus = 0;
    if (sigsetjmp(jump, 1) == 0)
        (void)*(volatile uint32_t *)(void *)(mapping + 8190);
    else
        crossing_bus = 1;
    expected_address = mapping + 8192;
    int atomic_bus = 0;
    if (sigsetjmp(jump, 1) == 0)
        (void)atomic_exchange_explicit((_Atomic uint32_t *)(void *)(mapping + 8192), UINT32_C(0xabcdef01),
                                       memory_order_seq_cst);
    else
        atomic_bus = 1;
    pid_t child = fork();
    if (child == 0) {
        signal(SIGBUS, SIG_DFL);
        (void)*(volatile unsigned char *)(mapping + 12288);
        _exit(9);
    }
    int child_status = 0;
    if (child < 0 || waitpid(child, &child_status, 0) != child) return 6;
    int default_bus = WIFSIGNALED(child_status) && WTERMSIG(child_status) == SIGBUS;
    if (mmap(mapping + 8192, 49152, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) !=
        mapping + 8192)
        return 4;
    mapping[8192] = 0x71;
    mapping[57343] = 0x72;
    _Atomic uint32_t *word = (_Atomic uint32_t *)(void *)(mapping + 8192);
    uint32_t old = atomic_exchange_explicit(word, UINT32_C(0x12345678), memory_order_seq_cst);
    int replacement = old == 0x71 && atomic_load_explicit(word, memory_order_relaxed) == UINT32_C(0x12345678);
    int edges = mapping[4096] == 0x3c && mapping[8191] == 0 && mapping[57343] == 0x72;
    int suffix_bus = expect_bus_load(mapping + 57344);
    printf("eof-bus partial=%d load=%d far=%d store=%d crossing=%d atomic=%d metadata=%d default=%d replacement=%d "
           "edges=%d suffix=%d canary-miss=%d canary-hit=%d signals=%d\n",
           partial_zero, load_bus, far_bus, store_bus, crossing_bus, atomic_bus, metadata_ok == bus_count, default_bus,
           replacement, edges, suffix_bus, canary_miss, canary_hit, (int)bus_count);
    munmap(mapping, 65536);
    close(descriptor);
    unlink(path);
    return partial_zero && load_bus && far_bus && store_bus && crossing_bus && atomic_bus && metadata_ok == bus_count &&
                   default_bus && replacement && edges && suffix_bus && canary_miss && canary_hit && bus_count == 7
               ? 0
               : 5;
}
