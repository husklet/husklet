// SMC self-flush LIVELOCK repro (the chromium/V8 startup wall). A code-generating guest (V8) issues
// `ic ivau` (icache invalidate) constantly at startup -- and crucially it re-flushes ALREADY-TRANSLATED,
// UNCHANGED code lines (a builtin/trampoline flushed as part of a range every call, or a block flushing
// its OWN currently-executing source line). hl's SMC hook responded to EVERY such line-hit with a
// WHOLESALE drop of the entire translation map + IBTC, forcing the whole working set to re-translate --
// so a hot loop that re-flushes one unchanged line pays O(working-set) re-translations PER ITERATION.
// With a large working set that is `translate_block` spinning at 100% CPU forever (RSS flat, no guest
// progress of consequence): the observed `chromium --version` hang.
//
// This reproduces it without V8: an RWX arena holds (a) a self-flushing function `sf` whose body issues
// `ic ivau` on its OWN executing line (dc cvau; dsb; ic ivau; dsb; isb; ret -- the __clear_cache dance,
// hand-emitted so the flush fires from INSIDE the translated block), and (b) a working set of WORK_N
// padded `movz w0,#imm; nop*; ret` functions. Each iteration flushes sf's own (UNCHANGED) line, then
// calls every working-set function. Under the old wholesale-drop hook the self-flush nukes all WORK_N
// translations every iteration -> ITERS*WORK_N re-translations -> multi-minute (harness 25s timeout ->
// FAIL). The fix (content-gated SMC drop: a flush of an UNCHANGED translated line is benign -> skip the
// drop) keeps the single valid translation, so the working set is translated ONCE and every iteration is
// cheap in-cache calls -> ~1-2s. Bytes never change, so the checksum is identical either way (a genuine
// in-place rewrite -- soak_smc/smc2 -- DOES change bytes -> still drops -> still correct). aarch64
// machine code -> Linux/aarch64 only; golden output (deterministic).
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

#define WORK_N 1200        // working-set functions -> the per-flush wholesale-drop re-translation cost
#define PAD 48             // nop padding per function (bigger blocks -> costlier re-translation)
#define FN_WORDS 64        // 64 * 4 = 256B per slot: each function starts on its own 64B line (aligned)
#define ITERS 25000        // hot-loop iterations each re-flushing sf's own unchanged line. Sized so the
                           // bug (wholesale drop = WORK_N re-translations/iter) blows past the harness 25s
                           // timeout (~60s), while the content-gated fix stays a few seconds.

int main(void) {
    size_t slots = (size_t)(WORK_N + 1);
    uint32_t *arena = mmap(NULL, slots * FN_WORDS * sizeof(uint32_t), PROT_READ | PROT_WRITE | PROT_EXEC,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) { perror("mmap"); return 1; }

    // slot 0: sf(x0) -- flush the 64B line at x0 from WITHIN this executing block, then return. We call
    // it with x0 = &sf so it invalidates its OWN (already-translated, never-changing) source line.
    uint32_t *sf = arena;
    sf[0] = 0xD50B7B20u; // dc cvau, x0   (-> nop in hl; harmless data-cache clean)
    sf[1] = 0xD5033B9Fu; // dsb ish
    sf[2] = 0xD50B7520u; // ic ivau, x0   (-> hl R_ICFLUSH: SMC hook fires on line &sf)
    sf[3] = 0xD5033B9Fu; // dsb ish
    sf[4] = 0xD5033FDFu; // isb
    sf[5] = 0xD65F03C0u; // ret

    // slots 1..WORK_N: the working set. fn i returns (i & 0xffff) via movz, padded with nops so each
    // block is nontrivial to (re)translate.
    for (int i = 0; i < WORK_N; i++) {
        uint32_t *f = arena + (size_t)(i + 1) * FN_WORDS;
        uint16_t imm = (uint16_t)(i & 0xffff);
        int w = 0;
        f[w++] = 0x52800000u | ((uint32_t)imm << 5); // movz w0, #imm
        for (int p = 0; p < PAD; p++) f[w++] = 0xD503201Fu; // nop
        f[w++] = 0xD65F03C0u; // ret
    }
    __builtin___clear_cache((char *)arena, (char *)(arena + slots * FN_WORDS)); // publish all code once

    void (*flush)(void *) = (void (*)(void *))sf;
    uint64_t sum = 0;
    // Warm the working set so it is translated before the loop (the first sf() flush then drops it).
    for (int i = 0; i < WORK_N; i++) {
        uint32_t (*f)(void) = (uint32_t (*)(void))(arena + (size_t)(i + 1) * FN_WORDS);
        sum += f();
    }
    // Hot loop: re-flush sf's OWN unchanged translated line, then re-touch the whole working set.
    for (int it = 0; it < ITERS; it++) {
        flush(sf); // ic ivau on line &sf -- already translated, bytes UNCHANGED (benign icache maint.)
        for (int i = 0; i < WORK_N; i++) {
            uint32_t (*f)(void) = (uint32_t (*)(void))(arena + (size_t)(i + 1) * FN_WORDS);
            sum += f(); // under wholesale-drop these re-translate every iteration; under the fix, cached
        }
    }
    munmap(arena, slots * FN_WORDS * sizeof(uint32_t));
    // sum = (ITERS + 1) warm/loop passes * sum_{i=0}^{WORK_N-1}(i & 0xffff). WORK_N<65536 so i&0xffff=i.
    printf("smc selfflush sum=%llu\n", (unsigned long long)sum);
    return 0;
}
