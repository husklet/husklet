
// x87 -- the D8..DF ESC space.
//
// STATE MODEL. `double st[8]`, `fptop`, `fpsw`, `fpcw` in struct cpu are a THREE-WAY ABI -- AArch64
// emitters bake the offsets, checkpoints record sizeof(struct cpu), signal.c projects fpcw into the xsave
// area -- so this file may not change its SHAPE. Two consequences, decided deliberately:
//
//  * TAG STATE (needed for #IS stack overflow/underflow, FXAM's "empty", FFREE, and FNSTENV/FXSAVE's tag
//    word) is stored in cpu->fptop's unused high bits rather than in a new field, so sizeof(struct cpu) --
//    the checkpoint format -- does not move. valid/zero/special ARE derived from the stored double;
//    emptiness is not derivable (FFREE punches an arbitrary hole, `fst %st(3)` fills an arbitrary slot,
//    both measured), so exactly one bit per slot is stored. See x87state.h for the encoding, the ARMED
//    gate, and why every byte the AArch64 backend produces is unchanged.
//  * PRECISION. The carrier is a C double = 53 significand bits. FCW.PC=53 is therefore exact and PC=24 is
//    reproduced by narrowing the 53-bit result (innocuous double rounding: 53 >= 2*24+2). PC=64, the
//    FNINIT DEFAULT, is the model's one irreducible shortfall -- 11 bits of long-double tail -- and needs a
//    genuinely 80-bit register file, i.e. a wider st[] plus an f64<->ext80 conversion on every JIT x87
//    load/store. Not attempted here; it is a format change AND a lower/x87.c rewrite.
//
// On x86-64 the C double arithmetic below runs on SSE2 scalars, so the QNaN indefinite already carries
// x86's SET sign and FCOM/FUCOM's #IA distinction falls out of COMISD/UCOMISD. FCW.RC is projected into
// MXCSR around every rounding operation (arithmetic and the narrowing f32 store), which the AArch64
// backend does not do.
//
// ESC decode trap: /digit depends on the opcode byte AND on mod, the operand type rides on the opcode not
// a prefix, and the register forms of DC/DE SWAP the reverse-subtract/divide digits.

// The stack is architecturally EMPTY until the first x87 instruction, which is where the tag model arms
// itself; before that cpu->fptop reads as "no tag information" (see x87state.h) and no #IS is possible.
static void interp_x87_arm(struct cpu *cpu) {
    if (hl_x87_tags_modelled() && !(cpu->fptop & HL_X87_ARMED)) cpu->fptop |= HL_X87_ARMED | HL_X87_EMPTY_ALL;
}

// TOP +- delta with the tag bits intact (FDECSTP/FINCSTP: rotate only, tags untouched -- measured).
static void interp_x87_top_add(struct cpu *cpu, int delta) {
    cpu->fptop = (cpu->fptop & ~UINT64_C(7)) | ((cpu->fptop + (uint64_t)(int64_t)delta) & 7);
}

static int interp_x87_empty(const struct cpu *cpu, int index) {
    return hl_x87_phys_empty(cpu->fptop, (int)((cpu->fptop + (uint64_t)(unsigned)index) & 7));
}

// ST(i) relative to fptop; the stack grows DOWNWARD.
static double interp_x87_get(const struct cpu *cpu, int index) {
    return cpu->st[(cpu->fptop + (uint64_t)(unsigned)index) & 7];
}

// Any write fills the slot: `fst %st(3)` into an empty ST(3) is legal and tags it valid, no #IS (measured).
static void interp_x87_set(struct cpu *cpu, int index, double value) {
    unsigned slot = (unsigned)((cpu->fptop + (uint64_t)(unsigned)index) & 7);
    hl_x87_phys_mark(&cpu->fptop, (int)slot, 0);
    cpu->st[slot] = value;
}

// #IS. C1 tells the two apart: 1 = OVERFLOW (a push onto a non-empty slot), 0 = UNDERFLOW (a read of an
// empty one). Masked by default (FCW.IM), so the guest sees only the sticky IE|SF -- which is exactly the
// x87 stack-depth probe real code runs (`fld1` until IE), and which a tag-less model reports as success.
static void interp_fp_raise(unsigned flags);

static void interp_x87_stack_fault(struct cpu *cpu, unsigned overflow) {
    interp_fp_raise(1u); // IE
    cpu->fpsw = (cpu->fpsw & ~(UINT64_C(1) << 9)) | UINT64_C(0x40) | (overflow ? UINT64_C(1) << 9 : 0);
}

// Reading an empty ST(a) (and ST(b), when b >= 0) is #IS underflow; the caller then writes the QNaN
// indefinite into its DESTINATION and leaves the empty source empty (measured: `fld1; fld %st(1)` tags the
// pushed slot special and leaves the source slot empty). 1 = operands live, proceed normally.
static int interp_x87_live(struct cpu *cpu, int a, int b) {
    if (!interp_x87_empty(cpu, a) && (b < 0 || !interp_x87_empty(cpu, b))) return 1;
    interp_x87_stack_fault(cpu, 0);
    return 0;
}

// A push onto a NON-EMPTY slot is #IS overflow: TOP still decrements and the destination is overwritten
// with the indefinite, destroying what was there (measured: 9x `fld1` from FNINIT leaves fsw=3a41 and
// ST(0) = ffffc000000000000000).
static void interp_x87_push(struct cpu *cpu, double value) {
    int overflow = hl_x87_phys_live(cpu->fptop, (int)((cpu->fptop - 1) & 7));
    interp_x87_top_add(cpu, -1);
    if (overflow) {
        interp_x87_stack_fault(cpu, 1);
        value = hl_x87_indefinite();
    }
    hl_x87_phys_mark(&cpu->fptop, (int)(cpu->fptop & 7), 0);
    cpu->st[cpu->fptop & 7] = value;
}

static void interp_x87_pop(struct cpu *cpu) {
    hl_x87_phys_mark(&cpu->fptop, (int)(cpu->fptop & 7), 1);
    interp_x87_top_add(cpu, 1);
}

// cpu->fpsw holds ONLY the condition codes and SF (0x4740); the other exception flags live in the host FPSW.
static void interp_x87_condition(struct cpu *cpu, unsigned c0, unsigned c1, unsigned c2, unsigned c3) {
    uint64_t status = cpu->fpsw & ~UINT64_C(0x4700);
    if (c0) status |= UINT64_C(1) << 8;
    if (c1) status |= UINT64_C(1) << 9;
    if (c2) status |= UINT64_C(1) << 10;
    if (c3) status |= UINT64_C(1) << 14;
    cpu->fpsw = status;
}

// C1 (FSW bit 9) is visible via FNSTSW/FXSAVE. Arithmetic ops write 0: no exact unrounded result.
static void interp_x87_c1(struct cpu *cpu, unsigned value) {
    if (value)
        cpu->fpsw |= UINT64_C(1) << 9;
    else
        cpu->fpsw &= ~(UINT64_C(1) << 9);
}

// C1 after a rounding op means "the SIGNIFICAND was rounded up", i.e. the MAGNITUDE grew -- x87 is
// sign-magnitude, so this is not `result > original`. Measured across FRNDINT/FISTP: -2.5 under RC=nearest
// gives -2 with C1=0, and under RC=down gives -3 with C1=1.
//
static unsigned interp_fp_hold(void);
static void interp_fp_release(unsigned held);

// C1 is DERIVED, so deriving it must raise nothing: COMISD signals #IA on a NaN and #D on a subnormal, and
// widening a narrowed subnormal float back to double signals #D too (measured: `fstp m32` of 1e-45 reports
// UE|PE on hardware, and reported UE|PE|DE here). Hold the sticky flags across the whole comparison.
static unsigned interp_x87_rounded_up(double result, double original) {
    unsigned held = interp_fp_hold();
    unsigned up = isnan(result) || isnan(original) ? 0u : (unsigned)(fabs(result) > fabs(original));
    interp_fp_release(held);
    return up;
}

// Not rint/nearbyint: those follow the live HOST rounding mode.
static double interp_round_half_even(double value) {
    double truncated;
    double fraction;
    double magnitude;
    if (!isfinite(value)) return value;
    truncated = trunc(value);
    fraction = value - truncated; // |value| >= 2^52 is already integral
    magnitude = fabs(fraction);
    if (magnitude > 0.5 || (magnitude == 0.5 && fmod(truncated, 2.0) != 0.0)) truncated += fraction > 0.0 ? 1.0 : -1.0;
    return truncated;
}

// STMXCSR/LDMXCSR as volatile asm, not the _mm_*csr intrinsics: the intrinsics are ordinary function-like
// builtins the compiler may sink or hoist, which silently defeats every RC projection below.
#if defined(HL_HOST_CPU_X86_64)
static unsigned interp_fp_getcsr(void) {
    unsigned value;
    __asm__ volatile("stmxcsr %0" : "=m"(value));
    return value;
}

static void interp_fp_setcsr(unsigned value) {
    __asm__ volatile("ldmxcsr %0" : : "m"(value));
}
#endif

// Only for exceptions the host FP unit cannot raise itself: the FIST/FISTP out-of-range #IA.
static void interp_fp_raise(unsigned flags) {
#if defined(HL_HOST_CPU_X86_64)
    interp_fp_setcsr(interp_fp_getcsr() | (flags & 0x3fu));
#else
    (void)flags; // no host FP status word here
#endif
}

// x87 FSW and MXCSR exception bits are the SAME host bits here, so FNCLEX also clears the SSE flags.
static void interp_fp_clear_exceptions(void) {
#if defined(HL_HOST_CPU_X86_64)
    interp_fp_setcsr(interp_fp_getcsr() & ~0x3fu);
#endif
}

// Snapshot/restore the sticky exception flags around a computation that is only used to DERIVE something
// (FPREM's quotient bits): the host raises #P on a division the architecture never performs.
static unsigned interp_fp_hold(void) {
#if defined(HL_HOST_CPU_X86_64)
    return interp_fp_getcsr() & 0x3fu;
#else
    return 0;
#endif
}

static void interp_fp_release(unsigned held) {
#if defined(HL_HOST_CPU_X86_64)
    interp_fp_setcsr((interp_fp_getcsr() & ~0x3fu) | held);
#else
    (void)held;
#endif
}

// FCW.RC (bits 11:10) uses the SAME encoding as MXCSR.RC (bits 14:13), and the x87 arithmetic here runs on
// host SSE2 scalars, so RC has to be projected in for the duration of every rounding operation. Measured:
// RC=up on `1/3` stored to m64 is 3fd5555555555556 on hardware and was ...555 here, and `fstp m32` of 1/3
// under RC=down is 3eaaaaaa, not 3eaaaaab. Returns the MXCSR to hand back (0 = nothing to do); the release
// keeps flags raised in between, so this is transparent to the sticky state.
static unsigned interp_x87_round_enter(const struct cpu *cpu) {
#if defined(HL_HOST_CPU_X86_64)
    unsigned saved = interp_fp_getcsr();
    unsigned want = (unsigned)((cpu->fpcw >> 10) & 3u) << 13;
    if ((saved & 0x6000u) == want) return 0;
    interp_fp_setcsr((saved & ~0x6000u) | want);
    return saved | 0x10000u; // marker: a restore is owed
#else
    (void)cpu;
    return 0;
#endif
}

static void interp_x87_round_leave(unsigned saved) {
#if defined(HL_HOST_CPU_X86_64)
    if (saved) interp_fp_setcsr((interp_fp_getcsr() & ~0x6000u) | (saved & 0x6000u));
#else
    (void)saved;
#endif
}

// FCW.PC (bits 9:8): 00 = 24 significand bits, 10 = 53, 11 = 64 (the FNINIT default). The carrier is a
// double, so PC=53 is already exact and PC=64 is the model's known shortfall (see the header). PC=24 IS
// exact here: re-rounding a 53-bit result to 24 bits is innocuous double rounding because 53 >= 2*24+2, so
// hardware's answer follows from the double one. x87 keeps the full 15-bit exponent range at PC=24, so this
// masks the significand rather than casting through float; a double SUBNORMAL is a normal 80-bit value the
// carrier has already lost, so it is left alone.
static double interp_x87_narrow(const struct cpu *cpu, double value) {
    uint64_t bits, dropped, kept;
    unsigned rc;
    int negative;
    if ((cpu->fpcw & 0x0300u) != 0 || !isfinite(value)) return value;
    memcpy(&bits, &value, sizeof bits);
    if (((bits >> 52) & 0x7ff) == 0) return value; // zero or subnormal
    dropped = bits & ((UINT64_C(1) << 29) - 1);    // the 29 fraction bits a 24-bit significand cannot hold
    if (!dropped) return value;
    kept = bits - dropped;
    negative = (int)(bits >> 63);
    rc = (unsigned)((cpu->fpcw >> 10) & 3u);
    // Rounding away from zero == +1 in the last KEPT bit; a carry out of the significand lands in the
    // exponent field, which is exactly the "1.111.. -> 10.000.." renormalise, and 0x7ff => infinity.
    if (rc == 1   ? negative
        : rc == 2 ? !negative
        : rc == 3 ? 0
                  : dropped > (UINT64_C(1) << 28) || (dropped == (UINT64_C(1) << 28) && (kept & (UINT64_C(1) << 29))))
        kept += UINT64_C(1) << 29;
    interp_fp_raise(0x20u); // #P: inexact at 24 bits even when it was exact at 53
    memcpy(&value, &kept, sizeof value);
    return value;
}

// TOP at 13:11, ES(7)/B(15) when a raised exception is UNMASKED per FCW. Must match hl_x86_fxsave.
static uint16_t interp_x87_status_word(const struct cpu *cpu) {
    uint16_t status = (uint16_t)((cpu->fpsw & 0x4740) | ((cpu->fptop & 7) << 11));
#if defined(HL_HOST_CPU_X86_64)
    uint16_t raised = (uint16_t)(interp_fp_getcsr() & 0x3fu); // same bit positions as MXCSR
    status |= raised;
    if (raised & (uint16_t)(~cpu->fpcw & 0x3f)) status |= (uint16_t)0x8080; // ES(7) + B(15)
#endif
    return status;
}

// COMISD raises #IE for ANY NaN, UCOMISD only for an sNaN -- the FCOM/FUCOM distinction. FSW codes are
// (C0,C2,C3) = (CF,PF,ZF), hence the EFLAGS shape.
static void interp_x87_compare_flags(double left, double right, int signalling, unsigned char *zf, unsigned char *pf,
                                     unsigned char *cf) {
#if defined(HL_HOST_CPU_X86_64)
    __m128d a = _mm_set_sd(left);
    __m128d b = _mm_set_sd(right);
    if (signalling)
        __asm__ volatile("comisd %[b], %[a]\n\tsetz %[z]\n\tsetp %[p]\n\tsetc %[c]"
                         : [z] "=r"(*zf), [p] "=r"(*pf), [c] "=r"(*cf)
                         : [a] "x"(a), [b] "x"(b)
                         : "cc");
    else
        __asm__ volatile("ucomisd %[b], %[a]\n\tsetz %[z]\n\tsetp %[p]\n\tsetc %[c]"
                         : [z] "=r"(*zf), [p] "=r"(*pf), [c] "=r"(*cf)
                         : [a] "x"(a), [b] "x"(b)
                         : "cc");
#else
    // No host compare: the signalling-vs-quiet #IE distinction is lost.
    (void)signalling;
    int unordered = isunordered(left, right);
    *pf = (unsigned char)(unordered ? 1 : 0);
    *zf = (unsigned char)((unordered || left == right) ? 1 : 0);
    *cf = (unsigned char)((unordered || left < right) ? 1 : 0);
#endif
}

static void interp_x87_compare_fpsw(struct cpu *cpu, double left, double right, int signalling) {
    unsigned char zf, pf, cf;
    interp_x87_compare_flags(left, right, signalling, &zf, &pf, &cf);
    interp_x87_condition(cpu, cf, 0, pf, zf);
}

static void interp_x87_compare_eflags(struct cpu *cpu, double left, double right, int signalling) {
    unsigned char zf, pf, cf;
    interp_x87_compare_flags(left, right, signalling, &zf, &pf, &cf);
    interp_flags_nzcv(cpu, 0, zf, cf, 0);
    cpu->pf = pf ? 0u : 1u; // EVEN parity of this byte is x86 PF
    cpu->af = 0;
    interp_x87_c1(cpu, 0); // FSW codes untouched; C1 is defined as 0
}

// Under x87 RC (FCW 11:10), not the live host mode.
static double interp_x87_round_integral(const struct cpu *cpu, double value) {
    unsigned rc = (unsigned)((cpu->fpcw >> 10) & 3u);
    if (!isfinite(value)) return value;
    switch (rc) {
    case 1: return floor(value);                   // -inf
    case 2: return ceil(value);                    // +inf
    case 3: return trunc(value);                   // zero
    default: return interp_round_half_even(value); // RC=00 default
    }
}

// Out of range (NaN and infinity included: all comparisons below false) gives INTEGER INDEFINITE.
static uint64_t interp_x87_to_integer(double value, int bytes, int *invalid) {
    double low = bytes == 2 ? -32768.0 : bytes == 4 ? -2147483648.0 : -9223372036854775808.0;
    double high = bytes == 2 ? 32767.0 : bytes == 4 ? 2147483647.0 : 9223372036854775808.0;
    // EXCLUSIVE: 2^63 is a double but not an int64.
    int in_range = bytes == 8 ? (value >= low && value < high) : (value >= low && value <= high);
    if (!in_range) {
        *invalid = 1;
        return bytes == 2 ? UINT64_C(0x8000) : bytes == 4 ? UINT64_C(0x80000000) : (UINT64_C(1) << 63);
    }
    *invalid = 0;
    return (uint64_t)(int64_t)value;
}

// SSE arithmetic is PURE to the compiler, so it will happily hoist an ADDSD/DIVSD above the LDMXCSR that
// establishes the guest's rounding mode -- measured: `fstp m32` under RC=down kept rounding to nearest until
// the conversion became volatile asm. Volatile asm statements keep their order relative to each other, so
// every operation whose result depends on RC is emitted this way. kind 0/1/2/3 = add/mul/sub/div.
#if defined(HL_HOST_CPU_X86_64)
static double interp_x87_sse2(int kind, double a, double b) {
    switch (kind) {
    case 0: __asm__ volatile("addsd %1, %0" : "+x"(a) : "x"(b)); break;
    case 1: __asm__ volatile("mulsd %1, %0" : "+x"(a) : "x"(b)); break;
    case 2: __asm__ volatile("subsd %1, %0" : "+x"(a) : "x"(b)); break;
    default: __asm__ volatile("divsd %1, %0" : "+x"(a) : "x"(b)); break;
    }
    return a;
}

static double interp_x87_sse_sqrt(double a) {
    double r;
    __asm__ volatile("sqrtsd %1, %0" : "=x"(r) : "x"(a));
    return r;
}

static float interp_x87_sse_narrow32(double a) {
    float r;
    __asm__ volatile("cvtsd2ss %1, %0" : "=x"(r) : "x"(a));
    return r;
}
#else
static double interp_x87_sse2(int kind, double a, double b) {
    return kind == 0 ? a + b : kind == 1 ? a * b : kind == 2 ? a - b : a / b;
}

static double interp_x87_sse_sqrt(double a) {
    return sqrt(a);
}

static float interp_x87_sse_narrow32(double a) {
    return (float)a;
}
#endif

// FSUBR/FDIVR reversed, so the DC/DE digit swap is one decision. Rounded under FCW's RC and PC, not the
// live host mode -- FDIV was the case an earlier audit caught (RC=up on 1/3 gave ...555, hardware ...556).
static double interp_x87_arith(const struct cpu *cpu, int kind, double destination, double source) {
    unsigned saved = interp_x87_round_enter(cpu);
    double result;
    switch (kind) {
    case 0: result = interp_x87_sse2(0, destination, source); break;  // FADD
    case 1: result = interp_x87_sse2(1, destination, source); break;  // FMUL
    case 4: result = interp_x87_sse2(2, destination, source); break;  // FSUB
    case 5: result = interp_x87_sse2(2, source, destination); break;  // FSUBR
    case 6: result = interp_x87_sse2(3, destination, source); break;  // FDIV
    default: result = interp_x87_sse2(3, source, destination); break; // FDIVR
    }
    result = interp_x87_narrow(cpu, result);
    interp_x87_round_leave(saved);
    return result;
}

static double interp_x87_sqrt(const struct cpu *cpu, double value) {
    unsigned saved = interp_x87_round_enter(cpu);
    double result = interp_x87_narrow(cpu, interp_x87_sse_sqrt(value));
    interp_x87_round_leave(saved);
    return result;
}

// FXAM {C3,C2,C0}: zero=100, NaN=001, Inf=011, denormal=110, normal=010, EMPTY=101; C1 sign. Never faults:
// this is the instruction that reports emptiness, and 101 is what the tag word buys (measured: 4100).
static void interp_x87_classify(struct cpu *cpu) {
    uint64_t bits;
    double value = interp_x87_get(cpu, 0);
    if (interp_x87_empty(cpu, 0)) {
        interp_x87_condition(cpu, 1, (unsigned)!!signbit(value), 0, 1); // signbit, not `< 0`: no COMISD on a NaN
        return;
    }
    memcpy(&bits, &value, sizeof bits);
    {
        unsigned sign = (unsigned)(bits >> 63);
        unsigned exponent_max = (unsigned)(((bits >> 52) & 0x7ff) == 0x7ff);
        unsigned exponent_zero = (unsigned)(((bits >> 52) & 0x7ff) == 0);
        unsigned mantissa_zero = (unsigned)((bits & ((UINT64_C(1) << 52) - 1)) == 0);
        unsigned is_zero = exponent_zero & mantissa_zero;
        unsigned is_nan = exponent_max & (unsigned)!mantissa_zero;
        interp_x87_condition(cpu, exponent_max, sign, (unsigned)!(is_zero | is_nan), exponent_zero);
    }
}

// ST0 <- unbiased exponent, then the significand (in [1,2), ST0's sign) is pushed. Like the JIT, no
// -inf/+inf/NaN for zero or non-finite.
static void interp_x87_extract(struct cpu *cpu) {
    uint64_t bits;
    double value = interp_x87_get(cpu, 0);
    double exponent;
    double significand;
    memcpy(&bits, &value, sizeof bits);
    exponent = (double)((int64_t)((bits >> 52) & 0x7ff) - 1023);
    bits = (bits & ~(UINT64_C(0x7ff) << 52)) | (UINT64_C(1023) << 52);
    memcpy(&significand, &bits, sizeof significand);
    interp_x87_set(cpu, 0, exponent);
    interp_x87_push(cpu, significand);
}

// Identity for non-finite or zero ST0 (scalbn's rule); the BIASED exponent clamps to [0,2047].
static double interp_x87_scale(double value, double exponent) {
    double power;
    int64_t biased;
    uint64_t bits;
    if (!isfinite(value) || value == 0.0) return value;
    if (isnan(exponent)) return value + exponent; // propagate ST1's NaN
    if (exponent > 4096.0)
        exponent = 4096.0;
    else if (exponent < -4096.0)
        exponent = -4096.0;
    biased = (int64_t)trunc(exponent) + 1023;
    if (biased > 2047)
        biased = 2047;
    else if (biased < 0)
        biased = 0;
    bits = (uint64_t)biased << 52;
    memcpy(&power, &bits, sizeof power);
    return value * power;
}

// |Q| mod 8, exactly and at ANY magnitude. Rounding ST0/ST1 and taking its low bits loses them above 2^53;
// fmod against 8*|ST1| is exact, and dividing THAT by |ST1| lands in [0,8). FPREM1's quotient is
// round-to-NEAREST, one more than the truncating one exactly when the IEEE remainder changed sign.
static unsigned interp_x87_quotient_low3(double st0, double st1, int ieee) {
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
// measured exc=00 on hardware for every case. This raised a spurious #P because deriving the quotient bits
// divided on the host FP unit, hence interp_fp_hold.
//
// C2=1 means "PARTIAL remainder, call me again", and hardware genuinely iterates once the operand exponents
// differ by 64 or more (measured: `1e300 fmod 1e-300` takes 22 steps, `1e300 fmod 3` takes 3). glibc's
// remainderl loops on C2, so always reporting 0 is a silent lie about a value the loop then consumes.
// What is NOT reproducible is hardware's per-step partial remainder: it reduces "up to 63 quotient bits" per
// step by an unspecified quantum (measured exponent deltas of 32..68), so the step COUNT differs. The
// architectural contract -- iterate while C2, then an exact remainder and |Q| mod 8 -- is what this
// reproduces. fmod against a SCALED ST1 makes each partial step exact in the double carrier, and drops the
// exponent difference by at least 64, so the loop terminates and the final remainder is the true one.
static void interp_x87_remainder(struct cpu *cpu, int ieee) {
    double st0 = interp_x87_get(cpu, 0);
    double st1 = interp_x87_get(cpu, 1);
    unsigned held = interp_fp_hold();
    if (isfinite(st0) && isfinite(st1) && st0 != 0.0 && st1 != 0.0) {
        int spread = ilogb(st0) - ilogb(st1);
        if (spread >= 64) {
            interp_x87_set(cpu, 0, fmod(st0, scalbn(st1, spread - 63)));
            interp_fp_release(held);
            interp_x87_condition(cpu, 0, 0, 1 /*C2: partial*/, 0); // quotient bits read 0 on hardware
            return;
        }
    }
    interp_x87_set(cpu, 0, ieee ? remainder(st0, st1) : fmod(st0, st1));
    {
        unsigned magnitude = interp_x87_quotient_low3(st0, st1, ieee);
        // The remainder itself may legitimately have raised #IA (ST1 zero, ST0 infinite); keep that.
        interp_fp_release(held | (interp_fp_hold() & 1u));
        // BOTH flavours publish |Q|'s low three bits as C1/C3/C0; lower/x87.c and qemu wrongly clear them
        // for FPREM1.
        interp_x87_condition(cpu, (magnitude >> 2) & 1u, magnitude & 1u, 0 /*complete*/, (magnitude >> 1) & 1u);
    }
}

// FCW@0, FSW@4, FTW@8, FIP@12, FCS+FOP@16, FDP@20, FDS@24 -- the 28-byte 32-bit protected-mode image.
// FNSTENV writes ALL 28 bytes including the upper half of each 16-bit word, which hardware fills with
// 0xffff (2-byte stores would leave the guest's own prior bytes there).
//
// STILL UNMODELLED, and therefore zero: FIP/FCS/FOP/FDP/FDS, the "last FP instruction" pointer group.
// Hardware gives e.g. FIP=<insn addr>, FCS=0x0033, FOP=((op0&7)<<8)|modrm, FDP=<operand addr>, FDS=0x002b.
// Reproducing them needs two more 64-bit fields in struct cpu, i.e. the format change the tag word was
// specifically arranged to avoid; and the group is only read by 16/32-bit unmasked-#FPU exception handlers,
// which cannot exist under this ABI. Writing the two SELECTORS alone would be a plausible-looking lie next
// to a zero FIP, so the whole group stays honestly zero.
static void interp_x87_store_environment(struct cpu *cpu, uint64_t address) {
    interp_store(address + 0, 4, 0xffff0000u | (cpu->fpcw & 0xffff));
    interp_store(address + 4, 4, 0xffff0000u | interp_x87_status_word(cpu));
    interp_store(address + 8, 4, 0xffff0000u | hl_x87_tag_word(cpu->fptop, cpu->st));
    interp_store(address + 12, 4, 0);
    interp_store(address + 16, 4, 0);
    interp_store(address + 20, 4, 0);
    interp_store(address + 24, 4, 0xffff0000u);
    cpu->fpcw |= 0x3f; // FNSTENV masks every exception afterwards (measured: fcw 0300 -> 037f)
}

static void interp_x87_load_environment(struct cpu *cpu, uint64_t address) {
    uint64_t control = interp_load(address + 0, 2);
    uint64_t status = interp_load(address + 4, 2);
    uint64_t tags = interp_load(address + 8, 2);
    cpu->fpcw = HL_X87_FCW(control);
    cpu->fpsw = (cpu->fpsw & ~UINT64_C(0x4740)) | (status & 0x4740); // condition codes + SF
    cpu->fptop = (cpu->fptop & HL_X87_STATE_BITS) | ((status >> 11) & 7);
    // Tags are restored per PHYSICAL register, and the data registers are NOT touched -- so re-tagging a
    // slot valid makes its old value readable again, which is what hardware does (measured: FNINIT then
    // FLDENV of a saved env reads back the pre-FNINIT ST(0)).
    interp_x87_arm(cpu);
    for (int slot = 0; slot < 8; ++slot)
        hl_x87_phys_mark(&cpu->fptop, slot, ((tags >> (2 * slot)) & 3u) == 3u);
}

// FNSAVE/FRSTOR m108: the 28-byte environment above followed by the eight registers as 10-byte ext80. The
// register area is TOP-RELATIVE (slot i is ST(i)) while the tag word is physical -- the same asymmetry
// FXSAVE has, measured the same way. FNSAVE then reinitialises the FPU exactly as FNINIT does.
static void interp_x87_save(struct cpu *cpu, uint64_t address) {
    interp_x87_store_environment(cpu, address);
    for (int index = 0; index < 8; ++index) {
        uint8_t image[10];
        hl_x86_ext80_store(interp_x87_get(cpu, index), image);
        interp_store_bytes(address + 28 + (uint64_t)index * 10, image, sizeof image);
    }
    cpu->fptop = HL_X87_EMPTY_ALL | (cpu->fptop & HL_X87_ARMED);
    cpu->fpsw = 0;
    cpu->fpcw = 0x037f;
    interp_fp_clear_exceptions();
}

static void interp_x87_restore(struct cpu *cpu, uint64_t address) {
    interp_x87_load_environment(cpu, address);
    for (int index = 0; index < 8; ++index) {
        uint8_t image[10];
        interp_load_bytes(address + 28 + (uint64_t)index * 10, image, sizeof image);
        cpu->st[(cpu->fptop + (unsigned)index) & 7] = hl_x86_ext80_load(image);
    }
}

// Bit patterns: what a real FPU's 80-bit constant narrows to.
static const uint64_t g_x87_constants[8] = {
    UINT64_C(0x3FF0000000000000), // FLD1
    UINT64_C(0x400A934F0979A371), // FLDL2T
    UINT64_C(0x3FF71547652B82FE), // FLDL2E
    UINT64_C(0x400921FB54442D18), // FLDPI
    UINT64_C(0x3FD34413509F79FF), // FLDLG2
    UINT64_C(0x3FE62E42FEFA39EF), // FLDLN2
    UINT64_C(0x0000000000000000), // FLDZ
    UINT64_C(0x0000000000000000), // no EF form
};

// FLD m32/m64 of a SUBNORMAL raises #D at LOAD time -- the widening to the register's exponent range is what
// hardware calls a denormal operand (measured: `fldl` of 5e-324 leaves exc=02; the m80 form raises nothing).
// The loaders are also the D8/DC operand path, so `faddl` of a subnormal reports DE|PE as hardware does.
static void interp_x87_load_denormal(uint64_t bits, uint64_t exponent_mask, uint64_t fraction_mask) {
    if (!(bits & exponent_mask) && (bits & fraction_mask)) interp_fp_raise(2u); // DE
}

static double interp_x87_load_f32(uint64_t address) {
    uint32_t bits = (uint32_t)interp_load(address, 4);
    float value;
    interp_x87_load_denormal(bits, UINT64_C(0x7f800000), UINT64_C(0x007fffff));
    memcpy(&value, &bits, sizeof value);
    return (double)value;
}

static double interp_x87_load_f64(uint64_t address) {
    uint64_t bits = interp_load(address, 8);
    double value;
    interp_x87_load_denormal(bits, UINT64_C(0x7ff) << 52, (UINT64_C(1) << 52) - 1);
    memcpy(&value, &bits, sizeof value);
    return value;
}

// The one x87 store that can round, so the one that needs FCW.RC (measured: `fstp m32` of 1/3 is 3eaaaaab
// under RC=nearest and 3eaaaaaa under RC=down/zero).
static unsigned interp_x87_store_f32(const struct cpu *cpu, uint64_t address, double value) {
    unsigned saved = interp_x87_round_enter(cpu);
    float narrowed = interp_x87_sse_narrow32(value);
    uint32_t bits;
    unsigned held, up;
    interp_x87_round_leave(saved);
    memcpy(&bits, &narrowed, sizeof bits);
    interp_store(address, 4, bits);
    // Widening the narrowed value back for C1 is itself an FP op: CVTSS2SD of a SUBNORMAL single raises #D,
    // which `fstp m32` of 1e-45 must not report (hardware: UE|PE only). Do it inside the hold.
    held = interp_fp_hold();
    up = interp_x87_rounded_up((double)narrowed, value);
    interp_fp_release(held);
    return up;
}

static void interp_x87_store_f64(uint64_t address, double value) {
    uint64_t bits;
    memcpy(&bits, &value, sizeof bits);
    interp_store(address, 8, bits); // exact: the ST carrier IS a double
}

// The D9 F0-FF transcendentals are computed in x87math.c after a block exit, which happens too late to
// report a stack fault -- so screen their operands here. 1 = live, take the exit; 0 = #IS already raised and
// the instruction's stack effect applied on the indefinite.
static void interp_x87_push(struct cpu *cpu, double value);

static int interp_x87_transcendental(struct cpu *cpu, int selector) {
    int two = selector == X87_FYL2X || selector == X87_FPATAN || selector == X87_FYL2XP1;
    if (interp_x87_live(cpu, 0, two ? 1 : -1)) return 1;
    interp_x87_set(cpu, 0, hl_x87_indefinite());
    if (two)
        interp_x87_pop(cpu); // FYL2X/FPATAN/FYL2XP1 write ST(1) then pop
    else if (selector == X87_FPTAN || selector == X87_FSINCOS)
        interp_x87_push(cpu, hl_x87_indefinite());
    return 0;
}

// A masked #IS on a store delivers the INTEGER or REAL indefinite of the destination width, and the form
// still pops (measured: `fstp m64` from an empty stack writes fff8000000000000 and TOP still advances).
static void interp_x87_store_indefinite(uint64_t address, int bytes, int integral) {
    if (integral) {
        interp_store(address, (unsigned)bytes,
                     bytes == 2   ? UINT64_C(0x8000)
                     : bytes == 4 ? UINT64_C(0x80000000)
                                  : (UINT64_C(1) << 63));
    } else if (bytes == 10) {
        uint8_t image[10];
        hl_x86_ext80_store(hl_x87_indefinite(), image);
        interp_store_bytes(address, image, sizeof image);
    } else {
        interp_store(address, (unsigned)bytes, bytes == 4 ? UINT64_C(0xffc00000) : HL_X87_INDEFINITE_BITS);
    }
}

// An empty ST(0) or ST(i) makes a compare UNORDERED as well as raising #IS: C3:C2:C0 = 111 for FCOM/FUCOM,
// ZF=PF=CF=1 for FCOMI/FUCOMI (measured).
static void interp_x87_compare_unordered_fpsw(struct cpu *cpu) {
    interp_x87_condition(cpu, 1, 0, 1, 1);
}

static void interp_x87_compare_unordered_eflags(struct cpu *cpu) {
    interp_flags_nzcv(cpu, 0, 1, 1, 0);
    cpu->pf = 0u; // even parity of 0 -> PF = 1
    cpu->af = 0;
}

// D8/DC m32/m64 float, DA/DE m32/m16 SIGNED int; destination always ST0.
static int interp_x87_memory_arith(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next, double source) {
    int kind = insn->reg & 7;
    int live = interp_x87_live(cpu, 0, -1);
    double st0 = interp_x87_get(cpu, 0);
    if (kind == 2 || kind == 3) { // FCOM / FCOMP: signalling
        if (live)
            interp_x87_compare_fpsw(cpu, st0, source, 1);
        else
            interp_x87_compare_unordered_fpsw(cpu);
        if (kind == 3) interp_x87_pop(cpu);
        cpu->rip = next;
        return STEP_NEXT;
    }
    (void)pc;
    interp_x87_set(cpu, 0, live ? interp_x87_arith(cpu, kind, st0, source) : hl_x87_indefinite());
    if (live) interp_x87_c1(cpu, 0); // see interp_x87_c1
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_step_x87_memory(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    // Neither the /digit nor the ST(i) selector is REX-extended: x87 has eight slots.
    int reg = insn->reg & 7;
    int rm = insn->rm & 7;
    interp_x87_arm(cpu);

    {
        uint64_t address = interp_ea(cpu, insn, next);
        switch (op) {
        case 0xD8: return interp_x87_memory_arith(cpu, insn, pc, next, interp_x87_load_f32(address));
        case 0xDC: return interp_x87_memory_arith(cpu, insn, pc, next, interp_x87_load_f64(address));
        case 0xDA: return interp_x87_memory_arith(cpu, insn, pc, next, (double)(int32_t)interp_load(address, 4));
        case 0xDE: return interp_x87_memory_arith(cpu, insn, pc, next, (double)(int16_t)interp_load(address, 2));

        case 0xD9:
            switch (reg) {
            case 0: // FLD m32 -- C1 first: a stack-overflow push sets it to 1
                interp_x87_c1(cpu, 0);
                interp_x87_push(cpu, interp_x87_load_f32(address));
                break;
            case 2: // FST m32
            case 3: // FSTP m32
                if (interp_x87_live(cpu, 0, -1))
                    interp_x87_c1(cpu, interp_x87_store_f32(cpu, address, interp_x87_get(cpu, 0)));
                else
                    interp_x87_store_indefinite(address, 4, 0);
                if (reg == 3) interp_x87_pop(cpu);
                break;
            case 4: interp_x87_load_environment(cpu, address); break;       // FLDENV m28
            case 5: cpu->fpcw = HL_X87_FCW(interp_load(address, 2)); break; // FLDCW m16
            case 6: interp_x87_store_environment(cpu, address); break;      // FNSTENV m28
            case 7: interp_store(address, 2, cpu->fpcw & 0xffff); break;    // FNSTCW m16
            default: return interp_undefined(cpu, insn, pc, "x87 D9 memory form");
            }
            cpu->rip = next;
            return STEP_NEXT;

        case 0xDB:
            switch (reg) {
            case 0: // FILD m32
                interp_x87_c1(cpu, 0);
                interp_x87_push(cpu, (double)(int32_t)interp_load(address, 4));
                break;
            case 1:   // FISTTP m32
            case 2:   // FIST m32
            case 3: { // FISTP m32
                int invalid = 0;
                // FISTTP truncates; FIST/FISTP round per FCW.
                double value = interp_x87_get(cpu, 0);
                double rounded;
                if (!interp_x87_live(cpu, 0, -1)) {
                    interp_x87_store_indefinite(address, 4, 1);
                    if (reg != 2) interp_x87_pop(cpu);
                    break;
                }
                rounded = reg == 1 ? trunc(value) : interp_x87_round_integral(cpu, value);
                uint64_t stored = interp_x87_to_integer(rounded, 4, &invalid);
                if (invalid) interp_fp_raise(1u /*IE*/);
                interp_store(address, 4, stored);
                interp_x87_c1(cpu, interp_x87_rounded_up(rounded, value));
                if (reg != 2) interp_x87_pop(cpu);
                break;
            }
            case 5: { // FLD m80 -- shared 80-bit converter
                uint8_t image[10];
                interp_load_bytes(address, image, sizeof image);
                interp_x87_c1(cpu, 0);
                interp_x87_push(cpu, hl_x86_ext80_load(image));
                break;
            }
            case 7: // FSTP m80
                if (interp_x87_live(cpu, 0, -1)) {
                    uint8_t image[10];
                    hl_x86_ext80_store(interp_x87_get(cpu, 0), image);
                    interp_store_bytes(address, image, sizeof image);
                    interp_x87_c1(cpu, 0);
                } else {
                    interp_x87_store_indefinite(address, 10, 0);
                }
                interp_x87_pop(cpu);
                break;
            default: return interp_undefined(cpu, insn, pc, "x87 DB memory form");
            }
            cpu->rip = next;
            return STEP_NEXT;

        case 0xDD:
            switch (reg) {
            case 0: // FLD m64
                interp_x87_c1(cpu, 0);
                interp_x87_push(cpu, interp_x87_load_f64(address));
                break;
            case 1: { // FISTTP m64
                int invalid = 0;
                double value = interp_x87_get(cpu, 0);
                double rounded;
                if (!interp_x87_live(cpu, 0, -1)) {
                    interp_x87_store_indefinite(address, 8, 1);
                    interp_x87_pop(cpu);
                    break;
                }
                rounded = trunc(value);
                uint64_t stored = interp_x87_to_integer(rounded, 8, &invalid);
                if (invalid) interp_fp_raise(1u);
                interp_store(address, 8, stored);
                interp_x87_c1(cpu, interp_x87_rounded_up(rounded, value));
                interp_x87_pop(cpu);
                break;
            }
            case 2: // FST m64
            case 3: // FSTP m64
                if (interp_x87_live(cpu, 0, -1)) {
                    interp_x87_store_f64(address, interp_x87_get(cpu, 0));
                    interp_x87_c1(cpu, 0);
                } else {
                    interp_x87_store_indefinite(address, 8, 0);
                }
                if (reg == 3) interp_x87_pop(cpu);
                break;
            case 4: interp_x87_restore(cpu, address); break;                      // FRSTOR m108
            case 6: interp_x87_save(cpu, address); break;                         // FNSAVE m108
            case 7: interp_store(address, 2, interp_x87_status_word(cpu)); break; // FNSTSW m16
            default: return interp_undefined(cpu, insn, pc, "x87 DD memory form");
            }
            cpu->rip = next;
            return STEP_NEXT;

        case 0xDF:
            switch (reg) {
            case 0: // FILD m16
                interp_x87_c1(cpu, 0);
                interp_x87_push(cpu, (double)(int16_t)interp_load(address, 2));
                break;
            case 1:   // FISTTP m16
            case 2:   // FIST m16
            case 3: { // FISTP m16
                int invalid = 0;
                double value = interp_x87_get(cpu, 0);
                double rounded;
                if (!interp_x87_live(cpu, 0, -1)) {
                    interp_x87_store_indefinite(address, 2, 1);
                    if (reg != 2) interp_x87_pop(cpu);
                    break;
                }
                rounded = reg == 1 ? trunc(value) : interp_x87_round_integral(cpu, value);
                uint64_t stored = interp_x87_to_integer(rounded, 2, &invalid);
                if (invalid) interp_fp_raise(1u);
                interp_store(address, 2, stored);
                interp_x87_c1(cpu, interp_x87_rounded_up(rounded, value));
                if (reg != 2) interp_x87_pop(cpu);
                break;
            }
            // FILD m64: the only x87 load that can round -- beyond 2^53 the carrier loses bits. C1 = 0.
            case 5:
                interp_x87_c1(cpu, 0);
                interp_x87_push(cpu, (double)(int64_t)interp_load(address, 8));
                break;
            case 7: { // FISTP m64
                int invalid = 0;
                double value = interp_x87_get(cpu, 0);
                double rounded;
                if (!interp_x87_live(cpu, 0, -1)) {
                    interp_x87_store_indefinite(address, 8, 1);
                    interp_x87_pop(cpu);
                    break;
                }
                rounded = interp_x87_round_integral(cpu, value);
                uint64_t stored = interp_x87_to_integer(rounded, 8, &invalid);
                if (invalid) interp_fp_raise(1u);
                interp_store(address, 8, stored);
                interp_x87_c1(cpu, interp_x87_rounded_up(rounded, value));
                interp_x87_pop(cpu);
                break;
            }
            default:
                // FBLD/FBSTP m80: packed BCD, unemitted.
                return interp_undefined(cpu, insn, pc, "x87 packed-BCD FBLD/FBSTP (DF /4,/6)");
            }
            cpu->rip = next;
            return STEP_NEXT;

        default: return interp_undefined(cpu, insn, pc, "x87 memory form");
        }
    }
}

static void interp_x87_register_arithmetic(struct cpu *cpu, uint8_t op, int reg, int rm) {
    int kind = reg;
    int target = op == 0xD8 ? 0 : rm;
    int live = interp_x87_live(cpu, 0, rm);
    double destination = interp_x87_get(cpu, target);
    double source = op == 0xD8 ? interp_x87_get(cpu, rm) : interp_x87_get(cpu, 0);
    if (op != 0xD8 && kind >= 4) kind ^= 1;
    interp_x87_set(cpu, target, live ? interp_x87_arith(cpu, kind, destination, source) : hl_x87_indefinite());
    if (live) interp_x87_c1(cpu, 0);
    if (op == 0xDE) interp_x87_pop(cpu);
}

static int interp_x87_register_status(struct cpu *cpu, struct insn *insn, int reg, int rm, uint64_t pc) {
    if (reg == 4 && rm == 0)
        interp_reg_write(cpu, insn, RAX, 2, interp_x87_status_word(cpu));
    else if (reg == 5 || reg == 6) {
        if (interp_x87_live(cpu, 0, rm))
            interp_x87_compare_eflags(cpu, interp_x87_get(cpu, 0), interp_x87_get(cpu, rm), reg == 6);
        else
            interp_x87_compare_unordered_eflags(cpu);
        interp_x87_pop(cpu);
    } else
        return interp_undefined(cpu, insn, pc, "x87 DF register form");
    return STEP_NEXT;
}

static void interp_x87_register_compare(struct cpu *cpu, uint8_t op, int reg, int rm) {
    if (interp_x87_live(cpu, 0, rm))
        interp_x87_compare_fpsw(cpu, interp_x87_get(cpu, 0), interp_x87_get(cpu, rm), 1);
    else
        interp_x87_compare_unordered_fpsw(cpu);
    if (op == 0xDE && rm == 1) interp_x87_pop(cpu);
    if (reg == 3) interp_x87_pop(cpu);
}

static int interp_x87_advance(struct cpu *cpu, uint64_t next) {
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_step_x87_register(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    int reg = insn->reg & 7;
    int rm = insn->rm & 7;
    interp_x87_arm(cpu);
    switch (op) {
    case 0xD8:
    case 0xDC:
    case 0xDE:
        if (reg == 2 || reg == 3) { // FCOM / FCOMP; DE D9 = FCOMPP, pops TWICE
            interp_x87_register_compare(cpu, op, reg, rm);
            cpu->rip = next;
            return STEP_NEXT;
        }
        interp_x87_register_arithmetic(cpu, op, reg, rm);
        return interp_x87_advance(cpu, next);

    case 0xD9:
        interp_x87_c1(cpu, 0);
        switch (reg) {
        case 0: // FLD ST(i): an empty source underflows, and the PUSHED slot takes the indefinite
            interp_x87_push(cpu, interp_x87_live(cpu, rm, -1) ? interp_x87_get(cpu, rm) : hl_x87_indefinite());
            break;
        case 1: { // FXCH ST(i): the exchange happens either way, then ST0 takes the indefinite (measured:
                  // `fld1; fxch %st(1)` leaves ST0 = the indefinite and ST1 = 1.0, ST1 tagged valid)
            double st0 = interp_x87_get(cpu, 0);
            int live = interp_x87_live(cpu, 0, rm);
            interp_x87_set(cpu, 0, live ? interp_x87_get(cpu, rm) : hl_x87_indefinite());
            interp_x87_set(cpu, rm, st0);
            break;
        }
        case 2:
            if (rm != 0) return interp_undefined(cpu, insn, pc, "x87 D9 /2 (only FNOP is defined)");
            break; // FNOP
        case 4:
            if (rm == 5) {
                interp_x87_classify(cpu);
                break;
            }
            if (rm == 0 || rm == 1 || rm == 4) {
                int live = interp_x87_live(cpu, 0, -1);
                double value = live ? interp_x87_get(cpu, 0) : hl_x87_indefinite();
                if (rm == 4) { // FTST
                    if (live)
                        interp_x87_compare_fpsw(cpu, value, 0.0, 1);
                    else
                        interp_x87_compare_unordered_fpsw(cpu);
                } else {
                    interp_x87_set(cpu, 0, !live ? value : rm == 0 ? -value : fabs(value)); // FCHS / FABS
                }
                break;
            }
            return interp_undefined(cpu, insn, pc, "x87 D9 /4 (reserved encoding)");
        case 5: { // FLD1 .. FLDZ
            double value;
            if (rm == 7) return interp_undefined(cpu, insn, pc, "x87 D9 EF (no such constant)");
            memcpy(&value, &g_x87_constants[rm], sizeof value);
            interp_x87_push(cpu, value);
            break;
        }
        case 6:
            if (rm <= 3) {
                static const int selector[4] = {X87_F2XM1, X87_FYL2X, X87_FPTAN, X87_FPATAN};
                if (!interp_x87_transcendental(cpu, selector[rm])) break;
                cpu->x87_ea = (uint64_t)selector[rm];
                return interp_exit(cpu, next, R_X87FUNC);
            }
            if (rm == 4) { // FXTRACT: reads ST0, writes it AND a push
                if (interp_x87_live(cpu, 0, -1)) {
                    interp_x87_extract(cpu);
                } else {
                    interp_x87_set(cpu, 0, hl_x87_indefinite());
                    interp_x87_push(cpu, hl_x87_indefinite());
                }
            } else if (rm == 5) { // FPREM1
                if (interp_x87_live(cpu, 0, 1))
                    interp_x87_remainder(cpu, 1);
                else
                    interp_x87_set(cpu, 0, hl_x87_indefinite());
            } else if (rm == 6) {
                interp_x87_top_add(cpu, -1); // FDECSTP: rotate top only, tags untouched, never faults
            } else {
                interp_x87_top_add(cpu, 1); // FINCSTP
            }
            break;
        case 7:
            switch (rm) {
            case 0: // FPREM
                if (interp_x87_live(cpu, 0, 1))
                    interp_x87_remainder(cpu, 0);
                else
                    interp_x87_set(cpu, 0, hl_x87_indefinite());
                break;
            case 2: // FSQRT
                interp_x87_set(cpu, 0,
                               interp_x87_live(cpu, 0, -1) ? interp_x87_sqrt(cpu, interp_x87_get(cpu, 0))
                                                           : hl_x87_indefinite());
                break;
            case 4: { // FRNDINT: per FCW.RC, direction in C1
                double value = interp_x87_get(cpu, 0);
                double rounded;
                if (!interp_x87_live(cpu, 0, -1)) {
                    interp_x87_set(cpu, 0, hl_x87_indefinite());
                    break;
                }
                rounded = interp_x87_round_integral(cpu, value);
                interp_x87_set(cpu, 0, rounded);
                interp_x87_c1(cpu, interp_x87_rounded_up(rounded, value));
                break;
            }
            case 5: // FSCALE
                interp_x87_set(cpu, 0,
                               interp_x87_live(cpu, 0, 1)
                                   ? interp_x87_scale(interp_x87_get(cpu, 0), interp_x87_get(cpu, 1))
                                   : hl_x87_indefinite());
                break;
            default: { // F9 FYL2XP1, FB FSINCOS, FE FSIN, FF FCOS
                static const int selector[8] = {0, X87_FYL2XP1, 0, X87_FSINCOS, 0, 0, X87_FSIN, X87_FCOS};
                if (!interp_x87_transcendental(cpu, selector[rm])) break;
                cpu->x87_ea = (uint64_t)selector[rm];
                return interp_exit(cpu, next, R_X87FUNC);
            }
            }
            break;
        default: return interp_undefined(cpu, insn, pc, "x87 D9 register form");
        }
        return interp_x87_advance(cpu, next);

    case 0xDA:
        if (reg <= 3) {
            static const int condition[4] = {2 /*B*/, 4 /*E*/, 6 /*BE*/, 10 /*U (P)*/};
            if (interp_cond(cpu, condition[reg])) interp_x87_set(cpu, 0, interp_x87_get(cpu, rm));
        } else if (reg == 5 && rm == 1) { // FUCOMPP: pop twice
            if (interp_x87_live(cpu, 0, 1))
                interp_x87_compare_fpsw(cpu, interp_x87_get(cpu, 0), interp_x87_get(cpu, 1), 0);
            else
                interp_x87_compare_unordered_fpsw(cpu);
            interp_x87_pop(cpu);
            interp_x87_pop(cpu);
        } else {
            return interp_undefined(cpu, insn, pc, "x87 DA register form");
        }
        cpu->rip = next;
        return STEP_NEXT;

    case 0xDB:
        if (reg <= 3) {
            static const int condition[4] = {3 /*NB*/, 5 /*NE*/, 7 /*NBE*/, 11 /*NU (NP)*/};
            if (interp_cond(cpu, condition[reg])) interp_x87_set(cpu, 0, interp_x87_get(cpu, rm));
        } else if (reg == 4 && rm == 2) { // FNCLEX: sticky exception flags only
            interp_fp_clear_exceptions();
        } else if (reg == 4 && rm == 3) { // FNINIT: TOP=0 and every slot EMPTY; st[] itself is untouched,
                                          // which is why a later FLDENV can re-tag and read the old values
            cpu->fptop = HL_X87_EMPTY_ALL | (cpu->fptop & HL_X87_ARMED);
            cpu->fpsw = 0;
            cpu->fpcw = 0x037f; // nearest, 64-bit, all masked
            interp_fp_clear_exceptions();
        } else if (reg == 4) { // FNENI / FNDISI / FNSETPM: 8087 no-ops
            /* nothing */
        } else if (reg == 5 || reg == 6) { // FUCOMI /5 quiet, FCOMI /6 signalling
            if (interp_x87_live(cpu, 0, rm))
                interp_x87_compare_eflags(cpu, interp_x87_get(cpu, 0), interp_x87_get(cpu, rm), reg == 6);
            else
                interp_x87_compare_unordered_eflags(cpu);
        } else {
            return interp_undefined(cpu, insn, pc, "x87 DB register form");
        }
        cpu->rip = next;
        return STEP_NEXT;

    case 0xDD:
        switch (reg) {
        case 0: // FFREE ST(i): tag it empty WITHOUT moving TOP, punching a hole no depth model can express
            hl_x87_phys_mark(&cpu->fptop, (int)((cpu->fptop + (unsigned)rm) & 7), 1);
            break;
        case 2: // FST ST(i)
        case 3: // FSTP ST(i)
            if (interp_x87_live(cpu, 0, -1)) {
                interp_x87_set(cpu, rm, interp_x87_get(cpu, 0));
                interp_x87_c1(cpu, 0);
            } else {
                interp_x87_set(cpu, rm, hl_x87_indefinite());
            }
            if (reg == 3) interp_x87_pop(cpu);
            break;
        case 4: // FUCOM: quiet, only sNaN raises #IA
        case 5: // FUCOMP ST(i)
            if (interp_x87_live(cpu, 0, rm))
                interp_x87_compare_fpsw(cpu, interp_x87_get(cpu, 0), interp_x87_get(cpu, rm), 0);
            else
                interp_x87_compare_unordered_fpsw(cpu);
            if (reg == 5) interp_x87_pop(cpu);
            break;
        default: return interp_undefined(cpu, insn, pc, "x87 DD register form");
        }
        cpu->rip = next;
        return STEP_NEXT;

    case 0xDF: {
        int result = interp_x87_register_status(cpu, insn, reg, rm, pc);
        if (result != STEP_NEXT) return result;
    }
        cpu->rip = next;
        return STEP_NEXT;

    default: return interp_undefined(cpu, insn, pc, "x87 register form");
    }
}

static int interp_step_x87(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    return insn->is_mem ? interp_step_x87_memory(cpu, insn, pc, next) : interp_step_x87_register(cpu, insn, pc, next);
}

