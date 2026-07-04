// ARMv8.1 LSE atomics (CAS/SWP/LDADD/LDSET/LDCLR/LDEOR, in relaxed/acquire/release/acq_rel orderings).
// Ubuntu's armv8.2 baseline glibc executes these (via the outline-atomic helpers) for malloc/locks/refcounts
// whenever HWCAP advertises ATOMICS; Debian bookworm (armv8.0) took the ldxr/stxr path. If the engine
// mistranslates ANY LSE variant, glibc state under gpgv corrupts -> observed wrong keyblock selection.
// Direct LSE is forced via -march=...+lse -mno-outline-atomics. Self-checking against expected results.
#include <stdio.h>
#include <stdint.h>
#include <stddef.h>

#define CHK(cond, msg) do { if (!(cond)) { printf("FAIL %s\n", msg); fails++; } } while (0)

int main(void) {
    int fails = 0;

    // ---- CAS family (compare-and-swap), all orderings ----
    for (int mo = 0; mo < 5; mo++) {
        uint64_t v = 0x1122334455667788ull, exp = v, des = 0xDEADBEEFCAFEF00Dull;
        int ok = __atomic_compare_exchange_n(&v, &exp, des, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        CHK(ok && v == des, "cas success");
        uint64_t v2 = 5, e2 = 9;
        int ok2 = __atomic_compare_exchange_n(&v2, &e2, 42, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        CHK(!ok2 && v2 == 5 && e2 == 5, "cas fail keeps value, reloads expected");
    }
    // 32-bit CAS (subregister)
    { uint32_t v = 0xAABBCCDD, e = v; int ok = __atomic_compare_exchange_n(&v, &e, 0x01020304u, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
      CHK(ok && v == 0x01020304u, "cas32"); }

    // ---- SWP ----
    { uint64_t v = 0x11, old = __atomic_exchange_n(&v, 0x99, __ATOMIC_ACQ_REL); CHK(old == 0x11 && v == 0x99, "swp64"); }
    { uint32_t v = 0x7, old = __atomic_exchange_n(&v, 0x5A5A5A5Au, __ATOMIC_ACQUIRE); CHK(old == 7 && v == 0x5A5A5A5Au, "swp32"); }

    // ---- LDADD (fetch_add) ----
    { uint64_t v = 100, old = __atomic_fetch_add(&v, 23, __ATOMIC_RELEASE); CHK(old == 100 && v == 123, "ldadd64"); }
    { uint32_t v = 0xFFFFFFF0u, old = __atomic_fetch_add(&v, 0x20, __ATOMIC_RELAXED); CHK(old == 0xFFFFFFF0u && v == 0x10u, "ldadd32 wrap"); }

    // ---- LDSET (fetch_or), LDCLR (fetch_and via ~), LDEOR (fetch_xor) ----
    { uint64_t v = 0xF0F0, old = __atomic_fetch_or(&v, 0x0F0F, __ATOMIC_ACQ_REL); CHK(old == 0xF0F0 && v == 0xFFFF, "ldset"); }
    { uint64_t v = 0xFFFF, old = __atomic_fetch_and(&v, 0xF0F0, __ATOMIC_ACQ_REL); CHK(old == 0xFFFF && v == 0xF0F0, "ldclr"); }
    { uint64_t v = 0xFFFF, old = __atomic_fetch_xor(&v, 0x0FF0, __ATOMIC_ACQ_REL); CHK(old == 0xFFFF && v == 0xF00F, "ldeor"); }

    // ---- realistic: a refcount / spinlock-ish loop (glibc pattern) ----
    { uint64_t rc = 1;
      for (int i = 0; i < 1000; i++) __atomic_fetch_add(&rc, 1, __ATOMIC_ACQ_REL);
      for (int i = 0; i < 1000; i++) __atomic_fetch_sub(&rc, 1, __ATOMIC_ACQ_REL);
      CHK(rc == 1, "refcount roundtrip"); }
    // CAS-based lock acquire/release cycle
    { uint32_t lock = 0;
      for (int i = 0; i < 500; i++) {
        uint32_t e = 0; while (!__atomic_compare_exchange_n(&lock, &e, 1, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) e = 0;
        __atomic_store_n(&lock, 0, __ATOMIC_RELEASE);
      }
      CHK(lock == 0, "cas lock cycle"); }

    printf("lse-atomics %s (fails=%d)\n", fails == 0 ? "OK" : "CORRUPT", fails);
    return fails ? 1 : 0;
}
