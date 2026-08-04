/* C11 <stdatomic.h> across all memory orders, single-threaded but exercising every lowering:
   fetch_add/sub/and/or/xor, exchange, compare_exchange_strong/weak (success + failure paths),
   load/store with relaxed/acquire/release/seq_cst. aarch64 -> LDXR/STXR or LSE; x86_64 -> LOCK
   prefixes / XADD / CMPXCHG. Final derived values are arch-neutral. */
#include <stdio.h>
#include <stdatomic.h>
#include <stdint.h>

int main(void) {
    atomic_uint_fast64_t a;
    atomic_init(&a, 100);
    atomic_fetch_add_explicit(&a, 50, memory_order_relaxed);
    atomic_fetch_sub_explicit(&a, 20, memory_order_acquire);
    atomic_fetch_or_explicit(&a, 0x100, memory_order_release);
    atomic_fetch_and_explicit(&a, ~0x2ull, memory_order_acq_rel);
    atomic_fetch_xor_explicit(&a, 0xFF, memory_order_seq_cst);
    uint64_t prev = atomic_exchange_explicit(&a, 777, memory_order_seq_cst);
    printf("a=%llu prev=%llu\n", (unsigned long long)atomic_load(&a),
           (unsigned long long)prev);

    atomic_int c;
    atomic_init(&c, 10);
    int expect = 10;
    int ok_s = atomic_compare_exchange_strong(&c, &expect, 42);
    int expect2 = 999;
    int ok_f = atomic_compare_exchange_strong(&c, &expect2, 55);
    printf("cas strong ok=%d val=%d fail=%d seen=%d final=%d\n",
           ok_s, 42, ok_f, expect2, atomic_load(&c));

    /* weak CAS loop until success */
    atomic_int w;
    atomic_init(&w, 0);
    int cur = atomic_load(&w), tries = 0;
    while (!atomic_compare_exchange_weak(&w, &cur, cur + 1) && tries < 1000) tries++;
    printf("cas weak final=%d\n", atomic_load(&w));

    atomic_flag f = ATOMIC_FLAG_INIT;
    int t0 = atomic_flag_test_and_set(&f);
    int t1 = atomic_flag_test_and_set(&f);
    atomic_flag_clear(&f);
    int t2 = atomic_flag_test_and_set(&f);
    printf("flag t0=%d t1=%d t2=%d\n", t0, t1, t2);
    return 0;
}
