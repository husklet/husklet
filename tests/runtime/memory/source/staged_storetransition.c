#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/* A translated store site caches the projected host address of its window. When a
   MAP_FIXED transition rebinds that guest address to a different backing, the
   generation guard is what forces the site to re-resolve; without it the site keeps
   the retired delta and every later store lands in the previous backing. */

#define PAGE 4096u
#define SENTINEL UINT64_C(0x5add1ed5add1ed5a)
/* Native admission needs the same site observed twice at one token version, and a
   straight-line probe never qualifies. The counted walk in front of the probe is what
   carries the body past NATIVE_SOLO_BUDGET and gets it translated. */
#define WALK 64
#define WARM_ITERATIONS 500u
#define REBIND_ITERATIONS 1000u
#define OVERLAP_ITERATIONS 100u
#define SETTLE_ITERATIONS 500u

static volatile uint64_t *window;
static _Atomic unsigned long iterations;
static _Atomic unsigned long rebinds;
static _Atomic int running = 1;
static _Atomic int rebind_failed;
static int backing;
static uint64_t scratch[WALK];

/* Rebinds the same guest address between the two backings while the store site is
   hot, so invalidation and staged stores overlap instead of being separated. */
static void *flipper(void *unused) {
    (void)unused;
    unsigned long n = 0;
    const struct timespec scheduling_gap = {.tv_nsec = 50000};
    while (n < REBIND_ITERATIONS) {
        off_t offset = (n & 1u) ? (off_t)PAGE : (off_t)0;
        if (mmap((void *)window, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, backing, offset) !=
            (void *)window) {
            atomic_store_explicit(&rebind_failed, 1, memory_order_release);
            break;
        }
        ++n;
        atomic_store_explicit(&rebinds, n, memory_order_release);
        (void)nanosleep(&scheduling_gap, NULL);
    }
    return NULL;
}

static void *storer(void *unused) {
    (void)unused;
    volatile int walk = WALK;
    while (atomic_load_explicit(&running, memory_order_acquire)) {
        /* Counted warm-up walk: keeps the block hot enough to translate. */
        uint64_t sum = 0;
        for (int i = 0; i < walk; ++i) {
            scratch[i] = scratch[i] + 1u;
            sum += scratch[i];
        }
        /* The probe under test: a store through the window that a peer remaps. */
        window[0] = SENTINEL;
        window[1] = SENTINEL ^ sum;
        window[2] = SENTINEL;
        atomic_fetch_add_explicit(&iterations, 1u, memory_order_release);
    }
    return NULL;
}

static void advance(unsigned long count) {
    unsigned long start = atomic_load_explicit(&iterations, memory_order_acquire);
    while (atomic_load_explicit(&iterations, memory_order_acquire) - start < count) {}
}

static int all_zero(const unsigned char *page) {
    for (unsigned i = 0; i < PAGE; ++i)
        if (page[i] != 0u) return 0;
    return 1;
}

int main(void) {
    int fd = (int)syscall(SYS_memfd_create, "staged-store-transition", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)(PAGE * 2)) != 0) return 2;

    /* Stable observers of each backing page, independent of the racing window. */
    unsigned char *first = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    unsigned char *second = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)PAGE);
    void *slot = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (first == MAP_FAILED || second == MAP_FAILED || slot == MAP_FAILED) return 3;
    window = slot;
    backing = fd;

    pthread_t thread, flip;
    if (pthread_create(&thread, NULL, storer, NULL) != 0) return 4;

    /* Let the store site translate and latch the first backing. */
    advance(WARM_ITERATIONS);

    /* Race a fixed number of rebinds against the hot store site.  Basing the
       remap lifetime on store progress creates a circular starvation loop:
       every MAP_FIXED transition stops that same storer, so an unbounded
       flipper can prevent the progress used to stop it. */
    unsigned long overlap_start = atomic_load_explicit(&iterations, memory_order_acquire);
    if (pthread_create(&flip, NULL, flipper, NULL) != 0) return 5;
    pthread_join(flip, NULL);
    unsigned long overlap_end = atomic_load_explicit(&iterations, memory_order_acquire);
    if (atomic_load_explicit(&rebind_failed, memory_order_acquire) ||
        atomic_load_explicit(&rebinds, memory_order_acquire) != REBIND_ITERATIONS)
        return 6;
    if (overlap_end - overlap_start < OVERLAP_ITERATIONS) return 11;
    if (mmap(slot, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, (off_t)PAGE) != slot) return 7;

    /* Well after the last transition, so no in-flight store is still ambiguous. */
    advance(SETTLE_ITERATIONS);
    memset(first, 0, PAGE);
    atomic_thread_fence(memory_order_seq_cst);

    /* Every store from here must reach the second backing only. */
    advance(SETTLE_ITERATIONS);
    int retired_quiet = all_zero(first);
    int live_written = ((volatile uint64_t *)second)[0] == SENTINEL;

    atomic_store_explicit(&running, 0, memory_order_release);
    pthread_join(thread, NULL);

    printf("staged-store-transition retired-quiet=%d live-written=%d\n", retired_quiet, live_written);
    return retired_quiet && live_written ? 0 : 1;
}
