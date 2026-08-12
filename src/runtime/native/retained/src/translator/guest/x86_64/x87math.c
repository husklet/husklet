#include "x87math.h"
#include <math.h>
#include <stdint.h>
#include "cpu.h"
#include "x87state.h" // cpu->fptop's tag bits: a TOP move must preserve them and retag the slot

// TOP +- delta, keeping the tag bits a plain `(fptop + d) & 7` would drop.
static void top_add(struct cpu *cpu, int delta) {
    cpu->fptop = (cpu->fptop & ~UINT64_C(7)) | ((cpu->fptop + (uint64_t)(int64_t)delta) & 7);
}

static void push(struct cpu *cpu, double value) {
    top_add(cpu, -1);
    hl_x87_phys_mark(&cpu->fptop, (int)(cpu->fptop & 7), 0);
    cpu->st[cpu->fptop & 7] = value;
}

static void pop(struct cpu *cpu) {
    hl_x87_phys_mark(&cpu->fptop, (int)(cpu->fptop & 7), 1);
    top_add(cpu, 1);
}

// |Q| mod 8, exactly and at ANY magnitude. Rounding ST0/ST1 and taking its low bits loses them above 2^53;
// fmod against 8*|ST1| is exact, and dividing THAT by |ST1| lands in [0,8). FPREM1's quotient is
// round-to-NEAREST, one more than the truncating one exactly when the IEEE remainder changed sign.
static unsigned quotient_low3(double st0, double st1, int ieee) {
    double a = fabs(st0), b = fabs(st1), scaled, reduced;
    unsigned magnitude;
    if (!isfinite(a) || !isfinite(b) || b == 0.0) return 0;
    scaled = scalbn(b, 3);
    reduced = scaled > a ? a : fmod(a, scaled); // scaled==inf (b huge) also lands here: a mod inf == a
    magnitude = (unsigned)(reduced / b);
    if (magnitude > 7u) magnitude = 7u; // the division may round up to exactly 8.0
    if (ieee && signbit(remainder(a, b))) magnitude++;
    return magnitude & 7u;
}

// FPREM/FPREM1 are EXACT by definition -- the remainder is a subset of ST0's bits -- so they raise NOTHING;
// measured exc=00 on hardware for every case. The emitted f64 sequence this replaces raised a spurious #P,
// because deriving the quotient bits divides, hence the hold/release around everything.
//
// C2=1 means "PARTIAL remainder, call me again", and hardware genuinely iterates once the operand exponents
// differ by 64 or more (measured: `1e300 fmod 1e-300` takes 22 steps). glibc's remainderl loops on C2, so
// always reporting 0 -- which the emitted single fused step did -- is a silent lie about a value the loop
// then consumes. What is NOT reproducible is hardware's per-step quantum ("up to 63 quotient bits", measured
// exponent deltas 32..68), so the step COUNT differs; the architectural contract -- iterate while C2, then an
// exact remainder and |Q| mod 8 -- is what this reproduces. fmod against a SCALED ST1 makes each partial step
// exact in the double carrier and drops the exponent difference by at least 64, so the loop terminates.
static void x87_remainder(struct cpu *cpu, int ieee) {
    double st0 = cpu->st[cpu->fptop & 7];
    double st1 = cpu->st[(cpu->fptop + 1) & 7];
    unsigned invalid;
    unsigned held;
    if (hl_x87_phys_empty(cpu->fptop, (int)(cpu->fptop & 7)) ||
        hl_x87_phys_empty(cpu->fptop, (int)((cpu->fptop + 1) & 7))) {
        hl_x87_exceptions_raise(1u); // #IS underflow: IE|SF, C1 = 0
        cpu->fpsw = (cpu->fpsw & ~(UINT64_C(1) << 9)) | UINT64_C(0x40);
        hl_x87_phys_mark(&cpu->fptop, (int)(cpu->fptop & 7), 0);
        cpu->st[cpu->fptop & 7] = hl_x87_indefinite();
        return;
    }
    held = hl_x87_exceptions_get();
    if (isfinite(st0) && isfinite(st1) && st0 != 0.0 && st1 != 0.0) {
        int spread = ilogb(st0) - ilogb(st1);
        if (spread >= 64) {
            cpu->st[cpu->fptop & 7] = fmod(st0, scalbn(st1, spread - 63));
            hl_x87_exceptions_set(held);
            cpu->fpsw |= UINT64_C(0x400); // C2: partial. The quotient bits read 0 on hardware.
            return;
        }
    }
    invalid = !isnan(st0) && !isnan(st1) && (isinf(st0) || st1 == 0.0);
    cpu->st[cpu->fptop & 7] = ieee ? remainder(st0, st1) : fmod(st0, st1);
    {
        unsigned magnitude = quotient_low3(st0, st1, ieee);
        hl_x87_exceptions_set(held | invalid);
        // BOTH flavours publish |Q|'s low three bits as C1/C3/C0; the old lowering cleared them for FPREM1.
        cpu->fpsw |= (uint64_t)((magnitude >> 2) & 1u) << 8 | (uint64_t)(magnitude & 1u) << 9 |
                     (uint64_t)((magnitude >> 1) & 1u) << 14;
    }
}

void hl_x86_x87_math(struct cpu *cpu) {
    double st0 = cpu->st[cpu->fptop & 7];
    double st1 = cpu->st[(cpu->fptop + 1) & 7];
    // x87 invalid operations deliver the QNaN indefinite with the sign bit SET (0xFFF8000000000000);
    // the host libm/ARM default NaN has it CLEAR. Only a GENERATED NaN may be stamped -- a NaN
    // propagated from an operand keeps that operand's sign -- so remember whether either input was
    // already a NaN and fix the sign of the (single) result slot afterwards. Same rule, and the same
    // "result NaN AND no input NaN" test, as emit_x87_dnan_post / emit_dnan_post in translate.c.
    int clean_in = !isnan(st0) && !isnan(st1);
    unsigned top_in = (unsigned)(cpu->fptop & 7);
    cpu->fpsw &= ~UINT64_C(0x4700);
    switch (cpu->x87_ea) {
    case X87_F2XM1: cpu->st[cpu->fptop & 7] = exp2(st0) - 1.0; break;
    case X87_FYL2X:
        cpu->st[(cpu->fptop + 1) & 7] = st1 * log2(st0);
        pop(cpu);
        break;
    case X87_FPATAN:
        cpu->st[(cpu->fptop + 1) & 7] = atan2(st1, st0);
        pop(cpu);
        break;
    case X87_FYL2XP1:
        cpu->st[(cpu->fptop + 1) & 7] = st1 * log2(st0 + 1.0);
        pop(cpu);
        break;
    case X87_FPTAN:
        if (fabs(st0) >= 0x1p63) {
            cpu->fpsw |= UINT64_C(0x400);
            break;
        }
        cpu->st[cpu->fptop & 7] = tan(st0);
        push(cpu, 1.0);
        break;
    case X87_FSINCOS:
        if (fabs(st0) >= 0x1p63) {
            cpu->fpsw |= UINT64_C(0x400);
            break;
        }
        cpu->st[cpu->fptop & 7] = sin(st0);
        push(cpu, cos(st0));
        break;
    case X87_FSIN:
        if (fabs(st0) >= 0x1p63)
            cpu->fpsw |= UINT64_C(0x400);
        else
            cpu->st[cpu->fptop & 7] = sin(st0);
        break;
    case X87_FCOS:
        if (fabs(st0) >= 0x1p63)
            cpu->fpsw |= UINT64_C(0x400);
        else
            cpu->st[cpu->fptop & 7] = cos(st0);
        break;
    case X87_FPREM: x87_remainder(cpu, 0); break;
    case X87_FPREM1: x87_remainder(cpu, 1); break;
    }
    if (clean_in) {
        // Neither input was a NaN, so any NaN now sitting in a slot this op could have written was
        // GENERATED by it -> give it x86's negative indefinite sign. The candidate slots are the two
        // the op reads/writes in place plus the slot a push created.
        unsigned slot[3] = {top_in & 7, (top_in + 1) & 7, cpu->fptop & 7};
        for (int i = 0; i < 3; i++) {
            double v = cpu->st[slot[i]];
            if (isnan(v) && !signbit(v)) cpu->st[slot[i]] = -v;
        }
    }
}
