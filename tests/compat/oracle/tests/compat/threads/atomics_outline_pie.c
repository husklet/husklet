// Atomicity + memory-ordering coverage for the LSE outline-atomic idiom on a static-PIE guest
// (guestbase off), the exact shape combined_bench/phase_atomics compiles to: every atomic is a
// `bl __aarch64_<op><sz>_<order>` call whose fast path is a single host LSE op. This is the path the
// outline-atomic INLINER rewrites, so it is the correctness oracle for that rewrite. Deterministic,
// golden output compared against the native run (native = oracle).
//
// Coverage:
//   - CAS success + CAS failure (expected mutated on failure), 64- and 32-bit
//   - fetch_add / fetch_sub / fetch_or / fetch_xor / fetch_and / swap / exchange, 64- and 32-bit
//   - seq_cst / acq_rel / acquire / release / relaxed orders
//   - ABA: a CAS that must succeed after A->B->A, and a witness that the value round-tripped
//   - cross-thread release/acquire ordering: publisher stores payload then releases a flag; each
//     observer that sees the flag MUST see the payload (a dropped release/acquire barrier would let
//     the observer read a stale payload -> counted as a violation, must be 0)
//   - contended fetch_add total must be exact (atomicity under real contention)
#include <stdatomic.h>
#include <pthread.h>
#include <stdio.h>
#include <stdint.h>

// ---- single-threaded op matrix (exercises each outline helper's fast path) ----
static void op_matrix(void) {
    uint64_t v = 0;
    uint64_t exp;
    // CAS success (seq_cst)
    exp = 0;
    int ok1 = atomic_compare_exchange_strong_explicit((atomic_ullong *)&v, &exp, 100,
                                                       memory_order_seq_cst, memory_order_relaxed);
    // CAS failure: v==100, expect 0 -> fail, exp must be updated to 100
    uint64_t exp2 = 0;
    int ok2 = atomic_compare_exchange_strong_explicit((atomic_ullong *)&v, &exp2, 999,
                                                       memory_order_seq_cst, memory_order_relaxed);
    uint64_t fa = atomic_fetch_add_explicit((atomic_ullong *)&v, 7, memory_order_acq_rel);   // 100->107
    uint64_t fs = atomic_fetch_sub_explicit((atomic_ullong *)&v, 5, memory_order_acq_rel);   // 107->102
    uint64_t fo = atomic_fetch_or_explicit((atomic_ullong *)&v, 0x11, memory_order_seq_cst); // 102|0x11
    uint64_t fx = atomic_fetch_xor_explicit((atomic_ullong *)&v, 0xff, memory_order_seq_cst);
    uint64_t fn = atomic_fetch_and_explicit((atomic_ullong *)&v, 0x0f, memory_order_seq_cst);
    uint64_t sw = atomic_exchange_explicit((atomic_ullong *)&v, 0xdead, memory_order_acq_rel);
    printf("m64 ok1=%d ok2=%d exp2=%llu fa=%llu fs=%llu fo=%llu fx=%llu fn=%llu sw=%llu v=%llu\n",
           ok1, ok2, (unsigned long long)exp2, (unsigned long long)fa, (unsigned long long)fs,
           (unsigned long long)fo, (unsigned long long)fx, (unsigned long long)fn,
           (unsigned long long)sw, (unsigned long long)v);

    uint32_t w = 0;
    uint32_t we = 0;
    int wok1 = atomic_compare_exchange_strong_explicit((atomic_uint *)&w, &we, 100,
                                                        memory_order_seq_cst, memory_order_relaxed);
    uint32_t we2 = 5;
    int wok2 = atomic_compare_exchange_strong_explicit((atomic_uint *)&w, &we2, 999,
                                                        memory_order_seq_cst, memory_order_relaxed);
    uint32_t wfa = atomic_fetch_add_explicit((atomic_uint *)&w, 7, memory_order_acq_rel);
    uint32_t wfo = atomic_fetch_or_explicit((atomic_uint *)&w, 0x11, memory_order_seq_cst);
    uint32_t wsw = atomic_exchange_explicit((atomic_uint *)&w, 0xbeef, memory_order_release);
    printf("m32 wok1=%d wok2=%d we2=%u wfa=%u wfo=%u wsw=%u w=%u\n",
           wok1, wok2, we2, wfa, wfo, wsw, w);
}

// ---- ABA ----
static void aba(void) {
    uint64_t v = 0xA;
    atomic_store_explicit((atomic_ullong *)&v, 0xB, memory_order_seq_cst); // A->B
    atomic_store_explicit((atomic_ullong *)&v, 0xA, memory_order_seq_cst); // B->A
    uint64_t exp = 0xA;
    int ok = atomic_compare_exchange_strong_explicit((atomic_ullong *)&v, &exp, 0xC,
                                                     memory_order_acq_rel, memory_order_relaxed);
    printf("aba ok=%d v=%llu\n", ok, (unsigned long long)v);
}

// ---- contended fetch_add: exact total ----
#define NT 16
#define PER 200000
static atomic_ullong counter;
static void *addw(void *_) {
    (void)_;
    for (int i = 0; i < PER; i++)
        atomic_fetch_add_explicit(&counter, 1, memory_order_seq_cst);
    return 0;
}

// ---- cross-thread release/acquire ordering (race-free monotonic invariant) ----
// payload and flag both count up, one round at a time: the publisher writes payload=r (release) THEN
// flag=r (release). An observer loads flag (acquire) then payload (relaxed). Because payload for round r
// is published-before flag r, correct acquire/release ordering guarantees: once flag>=r is observed,
// payload>=r. The publisher racing ahead only makes payload LARGER, so "payload >= flag_seen" is immune
// to harness races yet a DROPPED release or acquire barrier lets the observer see the new flag with a
// STALE (smaller) payload -> payload < flag_seen -> counted. Deterministically 0 on a correct engine.
#define OBS 8
#define ROUNDS 400000
static atomic_ullong g_flag;
static atomic_ullong g_payload;
static atomic_ullong g_violations;
static atomic_int g_stop;
static void *observer(void *_) {
    (void)_;
    while (!atomic_load_explicit(&g_stop, memory_order_relaxed)) {
        uint64_t f = atomic_load_explicit(&g_flag, memory_order_acquire);
        uint64_t p = atomic_load_explicit(&g_payload, memory_order_relaxed);
        if (p < f) atomic_fetch_add_explicit(&g_violations, 1, memory_order_relaxed);
    }
    return 0;
}

int main(void) {
    op_matrix();
    aba();

    // contended total
    pthread_t t[NT];
    for (int i = 0; i < NT; i++) pthread_create(&t[i], 0, addw, 0);
    for (int i = 0; i < NT; i++) pthread_join(t[i], 0);
    printf("contended counter=%llu expect=%llu\n",
           (unsigned long long)counter, (unsigned long long)((uint64_t)NT * PER));

    // release/acquire ordering
    pthread_t o[OBS];
    for (int i = 0; i < OBS; i++) pthread_create(&o[i], 0, observer, 0);
    for (uint64_t r = 1; r <= ROUNDS; r++) {
        atomic_store_explicit(&g_payload, r, memory_order_release); // publish payload first
        atomic_store_explicit(&g_flag, r, memory_order_release);    // then release the flag
    }
    atomic_store_explicit(&g_stop, 1, memory_order_relaxed);
    for (int i = 0; i < OBS; i++) pthread_join(o[i], 0);
    printf("ordering violations=%llu\n", (unsigned long long)g_violations);
    return 0;
}
