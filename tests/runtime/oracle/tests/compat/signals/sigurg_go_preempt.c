// sigurg_go_preempt.c -- BUG-A regression: the engine suppresses Go's async-preempt SIGURG (23) for EVERY
// aarch64 Go image, not only the cgo (CGO_ENABLED=1 / runtime.iscgo==1) class. The internal-linked toolchain
// children `go build` forks (compile/asm/link) crash identically when SIGURG is delivered -- a SIGURG taken in
// sysmon's runtime.usleep SIGSEGVs (addr=0x0) and, under build parallelism, corrupts thread startup so
// clone/newosproc returns EAGAIN. `GODEBUG=asyncpreemptoff=1` fixes it, which is exactly what dropping SIGURG
// for a Go image emulates (os/linux/elf.c elf_is_go_image -> signal.c g_go_image -> maybe_deliver_signal drop).
//
// This binary is detected as a Go image ONLY by the linker's Go build-info magic ("\xff Go buildinf:"), which
// it carries in a .rodata blob below. It deliberately has NO "CGO_ENABLED=1" modinfo, so the OLD (cgo-only)
// detector left it UNSUPPRESSED: a hammer thread tgkill()ing SIGURG at the spinning main thread ran the handler
// (delivered>=1). The fixed engine drops every SIGURG for this Go-magic image, so the handler NEVER runs and
// the deterministic checksum below is byte-identical every run. Golden = engine behavior (delivered=0), which
// differs from a native kernel (native always delivers) -- exactly like sigurg_preempt.c's engine-behavior
// golden. Kept aarch64-only: g_go_image is set only by the aarch64 loader.
#define _GNU_SOURCE
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

// Go linker build-info blob: 14-byte magic, ptrSize byte, flags byte (0 == no inline strings -> the loader's
// detector matches on the magic alone). Marked used+volatile-referenced so it survives into the loaded image.
__attribute__((used)) static const unsigned char go_buildinfo[] = {
    0xff, ' ', 'G', 'o', ' ', 'b', 'u', 'i', 'l', 'd', 'i', 'n', 'f', ':', // magic
    0x08, 0x00,                                                            // ptrSize=8, flags=0
};

static volatile sig_atomic_t g_hits = 0;
static volatile int g_run = 1;
static int g_target_tid;

static void on_sigurg(int s) {
    (void)s;
    g_hits++;
}

static uint64_t __attribute__((noinline)) mix(uint64_t a, uint64_t b) {
    return (a ^ (a >> 27)) * 0x9E3779B97F4A7C15ull + b + 0x100000001B3ull;
}

static void *hammer(void *arg) {
    (void)arg;
    int pid = getpid();
    while (__atomic_load_n(&g_run, __ATOMIC_RELAXED)) {
        syscall(SYS_tgkill, pid, g_target_tid, SIGURG); // thread-directed async preempt at the main thread
        for (volatile int k = 0; k < 64; k++) {
        }
    }
    return NULL;
}

int main(void) {
    // touch the blob so the linker keeps it in the image the loader inspects
    __asm__ __volatile__("" ::"r"(go_buildinfo) : "memory");

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_sigurg;
    sa.sa_flags = SA_RESTART;
    sigaction(SIGURG, &sa, NULL);

    g_target_tid = (int)syscall(SYS_gettid);
    pthread_t th;
    pthread_create(&th, NULL, hammer, NULL);

    // Spin a bounded, JIT'd deterministic loop the storm can preempt at any block boundary.
    uint64_t acc = 0xCAFEF00DBAADF00Dull;
    for (int round = 0; round < 4000; round++)
        for (uint64_t i = 0; i < 20000; i++)
            acc = mix(acc, i);

    g_run = 0;
    pthread_join(th, NULL);

    // delivered=0 on a fixed engine (SIGURG dropped for the Go image); delivered=1 on the old cgo-only engine.
    printf("sigurg-go: delivered=%d csum=%016llx\n", g_hits > 0 ? 1 : 0, (unsigned long long)acc);
    return g_hits > 0 ? 1 : 0;
}
