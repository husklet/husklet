// #281 regression: LDAPR / LDAPRH / LDAPRB (Load-Acquire RCpc) on a NON-PIE image's low absolute data.
//
// C++20 std::atomic<T>::load(memory_order_acquire) on FEAT_LSE2/RCPC hardware lowers to LDAPR, not LDAR.
// clickhouse (and other modern aarch64 binaries) stream these against their own .data/.bss, which for a
// non-PIE ET_EXEC live at a LOW link address the loader biases high. Two engine paths must serve LDAPR:
//   * the translate-time bias-fold (emit_fold_mem) — LDAPR aliases the LSE atomic encoding box, so it
//     must be re-based like any other single-base load, and
//   * the SIGSEGV fallback (nonpie_fixup) — reached when the fold is skipped (LDAPR inside an exclusive
//     region, or the NOGUESTFOLD kill-switch). nonpie_fixup formerly matched LDAPR as an LSE atomic-RMW
//     (opc==4/o3==1) and returned 0 → a hard SIGSEGV. Both must now yield the same byte value as native.
//
// Built -static -no-pie (ET_EXEC) so g_nonpie_lo is armed. Deterministic output, diffed vs the native
// aarch64 oracle. LDAPR needs FEAT_RCPC; the src_nopie compile targets the host, which has it.
#include <stdio.h>
#include <stdint.h>

// low absolute globals (non-PIE .data): distinct bit patterns per width so a wrong width/offset shows.
static uint64_t g_q[4] = {0x1122334455667788ull, 0x0fedcba987654321ull, 0xffffffffffffffffull, 0};
static uint32_t g_w[4] = {0xdeadbeefu, 0x00c0ffeeu, 0x80000001u, 0};
static uint16_t g_h[4] = {0xabcd, 0x0001, 0x8000, 0};
static uint8_t  g_b[8]  = {0x5a, 0xff, 0x01, 0x80, 0, 0, 0, 0};

// ldapr* wrappers. `.arch armv8.3-a` enables FEAT_RCPC for the assembler so the exact RCpc opcode is
// emitted even when the compile's default -march (no per-test flags in the harness) predates it.
static uint64_t ld_q(const uint64_t *p){ uint64_t v; __asm__ volatile(".arch armv8.3-a\n\tldapr %0,[%1]" :"=r"(v):"r"(p):"memory"); return v; }
static uint32_t ld_w(const uint32_t *p){ uint32_t v; __asm__ volatile(".arch armv8.3-a\n\tldapr %w0,[%1]":"=r"(v):"r"(p):"memory"); return v; }
static uint32_t ld_h(const uint16_t *p){ uint32_t v; __asm__ volatile(".arch armv8.3-a\n\tldaprh %w0,[%1]":"=r"(v):"r"(p):"memory"); return v; }
static uint32_t ld_b(const uint8_t  *p){ uint32_t v; __asm__ volatile(".arch armv8.3-a\n\tldaprb %w0,[%1]":"=r"(v):"r"(p):"memory"); return v; }

int main(void){
    uint64_t acc = 0;
    for (int i = 0; i < 4; i++) acc = acc * 1000003 + ld_q(&g_q[i]);
    for (int i = 0; i < 4; i++) acc = acc * 1000003 + ld_w(&g_w[i]);
    for (int i = 0; i < 4; i++) acc = acc * 1000003 + ld_h(&g_h[i]);
    for (int i = 0; i < 8; i++) acc = acc * 1000003 + ld_b(&g_b[i]);
    // spot values prove per-width zero/sign handling (ldapr* zero-extend to X)
    printf("ldapr q0=%016llx w0=%08x h0=%04x b3=%02x acc=%016llx\n",
        (unsigned long long)ld_q(&g_q[0]), ld_w(&g_w[0]),
        (unsigned)ld_h(&g_h[0]), (unsigned)ld_b(&g_b[3]),
        (unsigned long long)acc);
    return 0;
}
