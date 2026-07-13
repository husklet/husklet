// guests/stolen_regs.c -- STOLEN-REGISTER codegen surface (aarch64 only; diffed vs a native run).
// The engine steals x16/x17/x18/x28/x30 from the guest (engine scratch / cpu pointer / host link);
// every guest instruction naming one is rewritten through cpu->x[] (emit_mangled_x18 and friends).
// This case pins the WHOLE mangle surface -- including the steal-mode fast paths (stealfast, which
// use engine-dead host x16/x17 instead of the legacy mscratch spill dance):
//   - data-processing reads/writes of x16/x17/x18 (1..3 distinct stolen regs; madd's 4-field form)
//   - loads/stores with stolen Rt / stolen base / writeback / load-store PAIR of two stolen regs
//   - adr/adrp/ldr-literal materialized INTO a stolen reg
//   - mrs/msr tpidr_el0 (TLS) through stolen and non-stolen regs
//   - cbz/cbnz + tbz/tbnz TESTING a stolen reg (both edges)
// A wrong scratch pick, a missed store-back, or a clobbered neighbour register changes the checksum.
#include <stdio.h>
#include <stdint.h>

static uint64_t lit_pool[2] = {0x1122334455667788ull, 0x99aabbccddeeff00ull};

int main(void) {
    uint64_t acc = 0, a, b, c, d;

    // 1) data-processing through x16/x17/x18: 1..2 distinct stolen regs, read+write fields.
    asm volatile(
        "mov x16, #0x1111\n\t"
        "mov x17, #0x2222\n\t"
        "mov x18, #0x4444\n\t"
        "add x16, x16, x17\n\t"          // 2 stolen sources, stolen dest
        "eor x17, x16, x18\n\t"          // 3 distinct stolen regs in one insn
        "lsl x18, x17, #3\n\t"
        "madd %0, x16, x17, x18\n\t"     // stolen Rn/Rm/Ra, non-stolen Rd
        : "=r"(a) :: "x16", "x17", "x18");
    acc = acc * 31 + a;

    // 2) memory: stolen Rt, stolen base, writeback, and a PAIR of two stolen regs.
    {
        uint64_t buf[4] = {5, 6, 7, 8};
        uint64_t *p = buf;
        asm volatile(
            "mov x16, %1\n\t"            // stolen base
            "ldr x17, [x16], #8\n\t"     // stolen Rt + stolen base + post-index writeback
            "ldr x18, [x16]\n\t"
            "stp x17, x18, [x16, #8]\n\t"// store PAIR of two stolen regs
            "ldp x17, x16, [x16, #8]\n\t"// load pair INTO two stolen regs (incl. the base itself)
            "add %0, x16, x17\n\t"
            : "=r"(b) : "r"(p) : "x16", "x17", "x18", "memory");
        acc = acc * 31 + b + buf[2] + buf[3];
    }

    // 3) adr / ldr-literal into a stolen reg (PC-relative materialization paths).
    asm volatile(
        "adr x16, 1f\n\t"                // adr -> stolen
        "adr x17, 2f\n\t"
        "sub %0, x17, x16\n\t"           // distance is layout-stable (both labels local)
        "1: nop\n\t"
        "2: nop\n\t"
        : "=r"(c) :: "x16", "x17");
    acc = acc * 31 + c;
    asm volatile(
        "ldr x16, %1\n\t"                // stolen Rt from a memory literal
        "mov %0, x16\n\t"
        : "=r"(d) : "m"(lit_pool[0]) : "x16");
    acc = acc * 31 + d;

    // 4) TLS through stolen and non-stolen regs (read + write round-trip, then restore).
    {
        uint64_t t0, t1;
        asm volatile("mrs %0, tpidr_el0" : "=r"(t0));            // non-stolen read
        asm volatile(
            "mrs x16, tpidr_el0\n\t"                             // stolen read
            "mov %0, x16\n\t" : "=r"(t1) :: "x16");
        acc = acc * 31 + (t0 == t1);                             // equal, value itself not printed
        asm volatile(                                            // stolen write + verify + restore
            "mov x17, #0x7a7a\n\t"
            "msr tpidr_el0, x17\n\t"
            "mrs x16, tpidr_el0\n\t"
            "mov %0, x16\n\t"
            "msr tpidr_el0, %1\n\t"
            : "=r"(t1) : "r"(t0) : "x16", "x17");
        acc = acc * 31 + t1;
    }

    // 5) conditional branches TESTING a stolen reg: cbz/cbnz + tbz/tbnz, both edges.
    {
        uint64_t r = 0;
        asm volatile(
            "mov x16, #0\n\t"
            "cbz x16, 1f\n\t"
            "add %0, %0, #100\n\t"       // NOT taken
            "1: add %0, %0, #1\n\t"
            "mov x17, #2\n\t"
            "cbnz x17, 2f\n\t"
            "add %0, %0, #100\n\t"
            "2: add %0, %0, #2\n\t"
            "mov x18, #4\n\t"
            "tbz x18, #2, 3f\n\t"        // bit2 set -> fall through
            "add %0, %0, #4\n\t"
            "3: tbnz x18, #2, 4f\n\t"    // taken
            "add %0, %0, #100\n\t"
            "4: add %0, %0, #8\n\t"
            : "+r"(r) :: "x16", "x17", "x18");
        acc = acc * 31 + r;
    }

    printf("stolen acc=%llu\n", (unsigned long long)acc);
    return 0;
}
