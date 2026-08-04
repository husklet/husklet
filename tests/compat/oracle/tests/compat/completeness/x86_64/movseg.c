// MOV r/m, Sreg (0x8C) and MOV Sreg, r/m16 (0x8E) -- #183. 64-bit userspace segment-selector reads:
// ES/DS/FS/GS=0, CS=0x33, SS=0x2b (Linux process start, matched by qemu-user). Covers the 64-bit-reg,
// 16-bit-reg (upper-bits-preserved) and memory destination forms, plus a MOV-to-Sreg (accept/discard).
// Oracle: qemu-x86_64 -- passes iff the emitted selector constants equal qemu's.
#include <stdio.h>
#include <stdint.h>

int main(void) {
    uint64_t ds = ~0ULL, es = ~0ULL, ss = ~0ULL, cs = ~0ULL, fs = ~0ULL, gs = ~0ULL;
    __asm__ volatile("mov %%ds, %0" : "=r"(ds)); // 0x8C, REX.W dest -> zero-extended selector
    __asm__ volatile("mov %%es, %0" : "=r"(es));
    __asm__ volatile("mov %%ss, %0" : "=r"(ss));
    __asm__ volatile("mov %%cs, %0" : "=r"(cs));
    __asm__ volatile("mov %%fs, %0" : "=r"(fs));
    __asm__ volatile("mov %%gs, %0" : "=r"(gs));

    // 16-bit register destination: write low 16, preserve bits 63:16.
    uint64_t base = 0xAAAAAAAAAAAAAAAAULL;
    __asm__ volatile("mov %%cs, %w0" : "+r"(base));

    // memory destination (16-bit selector store).
    uint16_t m = 0xBEEF;
    __asm__ volatile("mov %%ss, %0" : "=m"(m));

    // MOV to Sreg: accepted+discarded; must not fault. Reload of ES with its own selector is a no-op.
    uint16_t sel = (uint16_t)es;
    __asm__ volatile("mov %0, %%es" : : "r"(sel));
    uint64_t es2 = ~0ULL;
    __asm__ volatile("mov %%es, %0" : "=r"(es2));

    printf("seg ds=%llu es=%llu ss=%llu cs=%llu fs=%llu gs=%llu w16=%llx mem=%u es2=%llu\n",
           (unsigned long long)ds, (unsigned long long)es, (unsigned long long)ss, (unsigned long long)cs,
           (unsigned long long)fs, (unsigned long long)gs, (unsigned long long)base, (unsigned)m,
           (unsigned long long)es2);
    return 0;
}
