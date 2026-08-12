#include "rep.h"

#include "cpu.h"
#include "flags.h"
#include "operand.h"

#include <stddef.h>
#include <stdint.h>
#include <string.h>

static uint64_t guest_to_host(uint64_t address, uint64_t low, uint64_t high, uint64_t bias) {
    return low != 0 && address >= low && address < high ? address + bias : address;
}

// ---- W4-C: rep cmps/scas idiom (R_REPSTR) -------------------------------------------------
// One C round-trip does the entire (possibly REP/REPE/REPNE) compare/scan, then writes the exact
// x86 end-state (RCX/RSI/RDI and ZF/SF/CF/OF) back to the cpu struct. The descriptor (cpu->divop)
// carries the direction flag (DF, bit 11), taken from the runtime cpu->df when not statically known:
// DF=0 scans low->high (fast host memcmp/memchr paths), DF=1 (after `std`/popfq) scans high->low via
// the generic per-element loop.
// width-w (a-b) flags -> ARM NZCV, in the engine's borrow convention (stored C = NOT x86 CF),
// byte-identical to what do_alu()/SUBS produces for a normal cmp of the same width.
void hl_x86_rep_compare(struct cpu *c, uint64_t nonpie_low, uint64_t nonpie_high, uint64_t nonpie_bias) {
    uint64_t d = c->divop;
    int w = (int)(d & 0xff);
    uint64_t stride = (uint64_t)w;
    int isscas = (d >> 8) & 1, isrepne = (d >> 9) & 1, isrep = (d >> 10) & 1, df = (d >> 11) & 1;
    uint64_t n = isrep ? c->r[RCX] : 1; // REP uses RCX; a bare cmps/scas does exactly one step
    if (n == 0) return;                 // REP with RCX==0: no element executed, flags+pointers UNCHANGED
    // W6A/non-PIE: a biased ET_EXEC guest pointer may still hold a low LINK address -- e.g. rdi loaded via
    // `mov edi,imm32` pointing at a baked .rodata string (node20's non-PIE argv/flag parser emits
    // `mov edi,<flagstr>; rep cmpsb`). This helper dereferences rsi/rdi DIRECTLY from C (memcmp/memchr/typed
    // loads), so a low link address must be rebased to the real high mapping first -- the single-access fault
    // path nonpie_fixup cannot serve a bulk memcmp. rep movs/stos already do this (repstr_g2h); cmps/scas did
    // not -> the low deref SIGSEGV'd (node:20 x86 `node --version`). Bias ONLY the dereferenced pointers;
    // the RSI/RDI write-back below advances the GUEST (unbiased) values. Inert for PIE/static-PIE (g_nonpie_lo
    // == 0 makes repstr_g2h the identity), so no behavior change for the common case.
    uint64_t gsi = c->r[RSI], gdi = c->r[RDI]; // guest (unbiased) pointers for the write-back
    uint64_t rsi = guest_to_host(gsi, nonpie_low, nonpie_high, nonpie_bias);
    uint64_t rdi = guest_to_host(gdi, nonpie_low, nonpie_high, nonpie_bias);
    int64_t step = df ? -(int64_t)w : (int64_t)w; // DF=1 (std) scans high->low, DF=0 low->high
    uint64_t wmask = (w == 8) ? ~0ull : ((1ull << (8 * w)) - 1);
    uint64_t acc = c->r[RAX] & wmask; // scas accumulator (AL/AX/EAX/RAX)
    int stop_on_equal = isrepne;      // REPNE stops at first equal; REPE stops at first not-equal
    uint64_t k = 0, av = 0, bv = 0;
    if (!df && isrep && w == 1) { // ---- fast host scan (the lever; forward only) ----
        if (!isscas) {            // rep cmps byte  -> memcmp-style first-difference scan
            const uint8_t *pa = (const uint8_t *)rsi, *pb = (const uint8_t *)rdi;
            if (!stop_on_equal) { // REPE: stop at first diff -> memcmp tests equality fast,
                if (memcmp(pa, pb, n) == 0)
                    k = n; // then a bounded scan locates the mismatch byte.
                else {
                    size_t i = 0;
                    while (pa[i] == pb[i])
                        i++;
                    k = i + 1;
                }
            } else { // REPNE: stop at first equal (rare)
                size_t i = 0;
                while (i < n && pa[i] != pb[i])
                    i++;
                k = (i < n) ? i + 1 : n;
            }
            av = pa[k - 1];
            bv = pb[k - 1];
        } else { // scas byte: REPNE -> memchr (strlen/memchr), REPE -> run-scan
            const uint8_t *pb = (const uint8_t *)rdi;
            uint8_t cc = (uint8_t)acc;
            if (stop_on_equal) {
                const uint8_t *hit = memchr(pb, cc, n);
                k = hit ? (uint64_t)(hit - pb) + 1 : n;
            } else {
                size_t i = 0;
                while (i < n && pb[i] == cc)
                    i++;
                k = (i < n) ? i + 1 : n;
            }
            av = acc;
            bv = pb[k - 1];
        }
    } else if (!df && isrep && !isscas) { // rep cmps word/dword/qword (forward)
        if (!stop_on_equal) {             // REPE: memcmp tests equality fast, then locate the element
            if (memcmp((void *)rsi, (void *)rdi, n * (size_t)w) == 0)
                k = n;
            else {
                size_t i = 0;
                while (hl_x86_operand_read(rsi + i * stride, w) == hl_x86_operand_read(rdi + i * stride, w))
                    i++;
                k = i + 1;
            }
        } else {
            size_t i = 0;
            while (i < n && hl_x86_operand_read(rsi + i * stride, w) != hl_x86_operand_read(rdi + i * stride, w))
                i++;
            k = (i < n) ? i + 1 : n;
        }
        av = hl_x86_operand_read(rsi + (k - 1) * stride, w);
        bv = hl_x86_operand_read(rdi + (k - 1) * stride, w);
    } else if (!df && isrep) { // rep scas word/dword/qword: typed loop (forward)
        size_t i = 0;
        if (stop_on_equal)
            while (i < n && (hl_x86_operand_read(rdi + i * stride, w) & wmask) != acc)
                i++;
        else
            while (i < n && (hl_x86_operand_read(rdi + i * stride, w) & wmask) == acc)
                i++;
        k = (i < n) ? i + 1 : n;
        av = acc;
        bv = hl_x86_operand_read(rdi + (k - 1) * stride, w);
    } else {                                   // generic per-element loop: NOREPCMP oracle, bare
        for (;;) {                             // cmps/scas, OR any DF=1 (backward) scan
            uint64_t off = k * (uint64_t)step; // signed stride (forward +w, backward -w), modular
            if (isscas) {
                av = acc;
                bv = hl_x86_operand_read(rdi + off, w);
            } else {
                av = hl_x86_operand_read(rsi + off, w);
                bv = hl_x86_operand_read(rdi + off, w);
            }
            k++;
            int eq = ((av & wmask) == (bv & wmask));
            if (k >= n) break;
            if (stop_on_equal ? eq : !eq) break;
        }
    }
    if (isrep) c->r[RCX] = n - k;
    if (!isscas) c->r[RSI] = gsi + k * (uint64_t)step; // advance the GUEST pointers (not the host-biased ones)
    c->r[RDI] = gdi + k * (uint64_t)step;
    c->nzcv = hl_x86_sub_nzcv(av, bv, w);
}
