// frontend/x86_64/x86_ops.c -- x86-only block-exit helpers the dispatcher invokes: cpuid emulation
// and the 80-bit x87 load/store (which need a C round-trip, not inline codegen).
#include <math.h> // x87 transcendentals (x87_func) computed via host libm

// ---- W4-C: rep cmps/scas idiom (R_REPSTR) -------------------------------------------------
// One C round-trip does the entire (possibly REP/REPE/REPNE) compare/scan, then writes the exact
// x86 end-state (RCX/RSI/RDI and ZF/SF/CF/OF) back to the cpu struct. The descriptor (cpu->divop)
// carries the translate-time direction flag (DF, bit 11): DF=0 scans low->high (fast host
// memcmp/memchr paths), DF=1 (after `std`) scans high->low via the generic per-element loop.
static uint64_t repstr_rd(uint64_t p, int w) {
    switch (w) {
    case 1: return *(uint8_t *)p;
    case 2: return *(uint16_t *)p;
    case 4: return *(uint32_t *)p;
    default: return *(uint64_t *)p;
    }
}
// width-w (a-b) flags -> ARM NZCV, in the engine's borrow convention (stored C = NOT x86 CF),
// byte-identical to what do_alu()/SUBS produces for a normal cmp of the same width.
static uint64_t repstr_nzcv(uint64_t a, uint64_t b, int w) {
    int bits = 8 * w;
    uint64_t mask = (w == 8) ? ~0ull : ((1ull << bits) - 1);
    uint64_t ua = a & mask, ub = b & mask, r = (ua - ub) & mask;
    uint64_t N = r >> (bits - 1);
    uint64_t Z = (r == 0);
    uint64_t C = (ua >= ub); // ARM carry = NO borrow
    // signed overflow of (a-b): sign(a)!=sign(b) AND sign(r)!=sign(a). Bitwise -> works at all widths
    // incl. 64-bit (a naive INT64 negation form was the one bug the qemu oracle caught -- see W4-C md §5).
    uint64_t V = (((ua ^ ub) & (ua ^ r)) >> (bits - 1)) & 1;
    return (N << 31) | (Z << 30) | (C << 29) | (V << 28);
}
static void do_repstr(struct cpu *c) {
    uint64_t d = c->divop;
    int w = (int)(d & 0xff);
    int isscas = (d >> 8) & 1, isrepne = (d >> 9) & 1, isrep = (d >> 10) & 1, df = (d >> 11) & 1;
    uint64_t n = isrep ? c->r[RCX] : 1; // REP uses RCX; a bare cmps/scas does exactly one step
    if (n == 0) return;                 // REP with RCX==0: no element executed, flags+pointers UNCHANGED
    uint64_t rsi = c->r[RSI], rdi = c->r[RDI];
    int64_t step = df ? -(int64_t)w : (int64_t)w; // DF=1 (std) scans high->low, DF=0 low->high
    uint64_t wmask = (w == 8) ? ~0ull : ((1ull << (8 * w)) - 1);
    uint64_t acc = c->r[RAX] & wmask;     // scas accumulator (AL/AX/EAX/RAX)
    int stop_on_equal = isrepne;          // REPNE stops at first equal; REPE stops at first not-equal
    uint64_t k = 0, av = 0, bv = 0;
    if (!df && !norepcmp() && isrep && w == 1) {  // ---- fast host scan (the lever; forward only) ----
        if (!isscas) {                     // rep cmps byte  -> memcmp-style first-difference scan
            const uint8_t *pa = (const uint8_t *)rsi, *pb = (const uint8_t *)rdi;
            if (!stop_on_equal) {          // REPE: stop at first diff -> memcmp tests equality fast,
                if (memcmp(pa, pb, n) == 0) k = n; // then a bounded scan locates the mismatch byte.
                else { size_t i = 0; while (pa[i] == pb[i]) i++; k = i + 1; }
            } else {                       // REPNE: stop at first equal (rare)
                size_t i = 0; while (i < n && pa[i] != pb[i]) i++;
                k = (i < n) ? i + 1 : n;
            }
            av = pa[k - 1]; bv = pb[k - 1];
        } else {                           // scas byte: REPNE -> memchr (strlen/memchr), REPE -> run-scan
            const uint8_t *pb = (const uint8_t *)rdi;
            uint8_t cc = (uint8_t)acc;
            if (stop_on_equal) {
                const uint8_t *hit = memchr(pb, cc, n);
                k = hit ? (uint64_t)(hit - pb) + 1 : n;
            } else {
                size_t i = 0;
                while (i < n && pb[i] == cc) i++;
                k = (i < n) ? i + 1 : n;
            }
            av = acc; bv = pb[k - 1];
        }
    } else if (!df && !norepcmp() && isrep && !isscas) { // rep cmps word/dword/qword (forward)
        if (!stop_on_equal) {              // REPE: memcmp tests equality fast, then locate the element
            if (memcmp((void *)rsi, (void *)rdi, n * (size_t)w) == 0) k = n;
            else { size_t i = 0; while (repstr_rd(rsi + i * w, w) == repstr_rd(rdi + i * w, w)) i++; k = i + 1; }
        } else {
            size_t i = 0; while (i < n && repstr_rd(rsi + i * w, w) != repstr_rd(rdi + i * w, w)) i++;
            k = (i < n) ? i + 1 : n;
        }
        av = repstr_rd(rsi + (k - 1) * w, w); bv = repstr_rd(rdi + (k - 1) * w, w);
    } else if (!df && !norepcmp() && isrep) {     // rep scas word/dword/qword: typed loop (forward)
        size_t i = 0;
        if (stop_on_equal) while (i < n && (repstr_rd(rdi + i * w, w) & wmask) != acc) i++;
        else               while (i < n && (repstr_rd(rdi + i * w, w) & wmask) == acc) i++;
        k = (i < n) ? i + 1 : n;
        av = acc; bv = repstr_rd(rdi + (k - 1) * w, w);
    } else {                                       // generic per-element loop: NOREPCMP oracle, bare
        for (;;) {                                 // cmps/scas, OR any DF=1 (backward) scan
            uint64_t off = k * (uint64_t)step;     // signed stride (forward +w, backward -w), modular
            if (isscas) { av = acc; bv = repstr_rd(rdi + off, w); }
            else        { av = repstr_rd(rsi + off, w); bv = repstr_rd(rdi + off, w); }
            k++;
            int eq = ((av & wmask) == (bv & wmask));
            if (k >= n) break;
            if (stop_on_equal ? eq : !eq) break;
        }
    }
    if (isrep) c->r[RCX] = n - k;
    if (!isscas) c->r[RSI] = rsi + k * (uint64_t)step;
    c->r[RDI] = rdi + k * (uint64_t)step;
    c->nzcv = repstr_nzcv(av, bv, w);
    g_repstr_n++;
    g_repstr_elems += k;
}

// ---- RCL/RCR by CL (R_RCL) ---------------------------------------------------------------
// Rotate-through-carry with a runtime count. Descriptor in cpu->divop:
//   [0:8]=w(1/2/4/8)  bit8=rcr  bit9=is_mem  bit10=hi8(byte high reg AH/CH/DH/BH)  [16:21]=r/m reg.
// A memory operand's already-host-biased effective address is in cpu->x87_ea. Carry-in/out use cpu->nzcv
// in the engine's borrow convention (stored ARM C = NOT x86 CF) -- byte-identical to the inline
// emit_rcl_rcr() constant-count path and the membank flag ABI, so the following block reloads flags
// verbatim. x86: count is masked to 5 bits (operand size 8/16/32) or 6 bits (64), then MOD (width+1);
// a masked count of 0 changes NO flags; OF is defined only when the masked count is exactly 1.
static void do_rcl(struct cpu *c) {
    uint64_t d = c->divop;
    int w = (int)(d & 0xff), rcr = (d >> 8) & 1, is_mem = (d >> 9) & 1, hi8 = (d >> 10) & 1;
    int reg = (int)((d >> 16) & 0x1f);
    int W = 8 * w;
    uint64_t mask = (w == 8) ? ~0ull : ((1ull << W) - 1);
    uint64_t ea = c->x87_ea;
    uint64_t val = is_mem ? repstr_rd(ea, w) : (hi8 ? ((c->r[reg] >> 8) & 0xff) : (c->r[reg] & mask));
    int countMask = (w == 8) ? 0x3f : 0x1f;
    int masked = (int)(c->r[RCX] & 0xff) & countMask;
    int ec = masked % (W + 1);
    int cf = !((c->nzcv >> 29) & 1); // x86 CF carry-in = NOT stored ARM C
    // ec==0 (incl. a nonzero masked count that is a multiple of width+1, e.g. CL=9 for a byte) is a no-op:
    // x86 leaves the operand AND all flags unchanged. Only ec!=0 rotates; OF is written only for masked==1.
    if (ec != 0) {
        uint64_t res;
        int newcf;
        if (!rcr) { // RCL: (W+1)-bit left rotate of {CF, val} by ec
            res = (ec < W) ? ((val << ec) & mask) : 0;
            res |= (ec == 1) ? (uint64_t)cf : (((uint64_t)cf << (ec - 1)) & mask);
            if (ec >= 2) res |= (val >> (W + 1 - ec)) & mask; // top (ec-1) operand bits wrap in below CF
            newcf = (val >> (W - ec)) & 1;
        } else { // RCR: (W+1)-bit right rotate
            res = (ec < W) ? (val >> ec) : 0;
            res |= ((uint64_t)cf << (W - ec)) & mask;         // carry-in lands at bit (W-ec)
            if (ec >= 2) res |= (val << (W - ec + 1)) & mask; // low (ec-1) operand bits wrap to the top
            newcf = (val >> (ec - 1)) & 1;
        }
        cf = newcf;
        val = res & mask;
        c->nzcv = (c->nzcv & ~(1ull << 29)) | ((uint64_t)(cf ? 0 : 1) << 29); // stored C = NOT x86 CF
        if (masked == 1) {                                                    // OF defined only for a 1-bit rotate
            int of = !rcr ? (int)(((val >> (W - 1)) & 1) ^ (uint64_t)cf)      // RCL: MSB(result) XOR newCF
                          : (int)(((val >> (W - 1)) & 1) ^ ((val >> (W - 2)) & 1)); // RCR: top two result bits
            c->nzcv = (c->nzcv & ~(1ull << 28)) | ((uint64_t)of << 28);
        }
    }
    if (is_mem) {
        switch (w) {
        case 1: *(uint8_t *)ea = (uint8_t)val; break;
        case 2: *(uint16_t *)ea = (uint16_t)val; break;
        case 4: *(uint32_t *)ea = (uint32_t)val; break;
        default: *(uint64_t *)ea = val; break;
        }
    } else if (hi8)
        c->r[reg] = (c->r[reg] & ~0xff00ull) | ((val & 0xff) << 8);
    else if (w == 1)
        c->r[reg] = (c->r[reg] & ~0xffull) | (val & 0xff);
    else if (w == 2)
        c->r[reg] = (c->r[reg] & ~0xffffull) | (val & 0xffff);
    else
        c->r[reg] = val & mask; // w==4 zero-extends (upper 32 cleared); w==8 full
}

// CPUID emulation. We advertise EXACTLY the feature set the engine actually translates (legacy-SSE in
// emit.c + the 0F38/0F3A SSSE3/SSE4/AES/PCLMUL/SHA/CRC32/MOVBE and BMI lanes in avx.c do_sse3b/do_avx),
// mirroring a real x86-64 baseline. We deliberately do NOT advertise AVX/AVX2/FMA/F16C/XSAVE/OSXSAVE: those
// are gated on YMM state being enabled in XCR0, but our xgetbv reports only x87+SSE (translate.c), so a
// conformant guest would correctly decline them anyway -- advertising them would only mislead.
static void do_cpuid(struct cpu *c) {
    uint32_t leaf = (uint32_t)c->r[RAX], a = 0, b = 0, cc = 0, d = 0;
    switch (leaf) {
    case 0:
        a = 7;
        b = 0x756e6547;
        d = 0x49656e69;
        cc = 0x6c65746e;
        break; // max-leaf=7, "GenuineIntel"
    case 1:
        // Report a pre-AVX Intel Westmere (family 6, model 0x2c). Rationale: dd deliberately withholds
        // AVX (xgetbv reports only x87+SSE), so guest glibc takes its SSE path for memcpy/memmove. glibc's
        // memmove ifunc, when AVX is unusable, picks `__memmove_ssse3` (NO rep movsb) UNLESS the internal
        // `Fast_Unaligned_Copy` preferred bit is set -- and glibc only sets that bit for Nehalem/Westmere
        // models (Haswell+ is assumed to have AVX, so its SSE fast-copy heuristic was never wired). A
        // Haswell id (old value 0x000306c3) therefore left memcpy on the non-rep ssse3 path; Westmere makes
        // glibc select `__memmove_sse2_unaligned_erms` -> `rep movsb` above threshold, which dd lowers to a
        // single host memcpy. All features dd advertises (SSE4.2/AES-NI/PCLMUL/POPCNT below, BMI2/SHA/ERMS/
        // FSRM in leaf 7) are consumed by guests via their CPUID FEATURE bits, which stay set regardless of
        // this model id -- the model only steers glibc's SSE-vs-rep tuning toward the dd-fast rep path.
        a = 0x000206c2; // Intel Westmere (fam 6, model 0x2c) -- trips glibc Fast_Unaligned_Copy (SSE memcpy -> rep movsb)
        d = (1u << 0) | (1u << 4) | (1u << 8) | (1u << 11) | (1u << 13) | (1u << 15) | (1u << 19) | (1u << 23) |
            (1u << 24) | (1u << 25) | (1u << 26); // FPU,TSC,CX8,SEP,PGE,CMOV,CLFSH,MMX,FXSR,SSE,SSE2
        // SSE3, PCLMULQDQ, SSSE3, CMPXCHG16B, SSE4.1, SSE4.2, MOVBE, POPCNT, AES-NI -- all backed by the
        // legacy/0F38/0F3A lowerings; none need YMM state (XMM only, covered by xgetbv's SSE bit).
        cc = (1u << 0) | (1u << 1) | (1u << 9) | (1u << 13) | (1u << 19) | (1u << 20) | (1u << 22) | (1u << 23) |
             (1u << 25);
        break;
    case 7:
        if ((uint32_t)c->r[RCX] == 0) {                     // subleaf 0
            b = (1u << 3) | (1u << 8) | (1u << 9) | (1u << 29); // BMI1, BMI2, ERMS, SHA
            // ERMS (EBX[9]) + FSRM (EDX[4]): "REP MOVSB/STOSB is fast" + "fast for short lengths". We
            // advertise these because dd lowers `rep movs`/`rep stos` to ONE bit-exact host memcpy/memset
            // (translate/repstr.c: forward, all widths/alignments/0-length, forward-overlap smear replayed
            // element-granular; DF=1/backward falls back to the exact per-element scalar loop). glibc's
            // memcpy/memmove/memset ERMS paths only ever issue `rep movsb/stosb` in the FORWARD direction
            // (they take the backward SSE loop for dst>src overlap), which is exactly the case dd's fast
            // path serves -- so routing bulk libc copies here is byte-exact and avoids translating glibc's
            // SSE/AVX copy loops (which also hit the SSSE3/SSE4 softmulator exits). max-subleaf stays 0
            // (a==0): FSRM lives in subleaf 0, so no subleaf-1 emulation is needed.
            d = (1u << 4); // FSRM (Fast Short REP MOV); only bit 4 set -- no AVX512/security bits implied
        }
        break;
    case 0x80000000: a = 0x80000001; break;
    case 0x80000001:
        d = (1u << 11) | (1u << 29);
        cc = (1u << 0);
        break;                          // SYSCALL, LM(64-bit), LAHF
    case 0x80000008: a = 0x3027; break; // 48-bit phys, 39-bit virt
    default: break;
    }
    c->r[RAX] = a;
    c->r[RBX] = b;
    c->r[RCX] = cc;
    c->r[RDX] = d;
}

// x87 80-bit extended <-> double conversion (done in C for reliability; libm-free).
// We emulate the ST stack at double precision, so this loses the 80-bit mantissa tail.
static void x87_fld_m80(struct cpu *c) {
    uint8_t *ea = (uint8_t *)c->x87_ea;
    uint64_t sig;
    uint16_t se;
    memcpy(&sig, ea, 8);
    memcpy(&se, ea + 8, 2);
    int s = se >> 15, e = se & 0x7fff;
    double d;
    if (sig == 0 && e == 0)
        d = 0.0;
    else {
        d = (double)sig;        // ~2^63 (ucvtf)
        int k = e - 16383 - 63; // scale exponent
        uint64_t db;
        memcpy(&db, &d, 8);
        int de = (int)((db >> 52) & 0x7ff) + k;
        if (de <= 0)
            d = 0.0;
        else if (de >= 0x7ff) {
            db = (db & (1ull << 63)) | (0x7ffull << 52);
            memcpy(&d, &db, 8);
        } else {
            db = (db & ~(0x7ffull << 52)) | ((uint64_t)de << 52);
            memcpy(&d, &db, 8);
        }
        if (s) d = -d;
    }
    c->fptop = (c->fptop - 1) & 7;
    c->st[c->fptop & 7] = d;
}
static void x87_fstp_m80(struct cpu *c) {
    uint8_t *ea = (uint8_t *)c->x87_ea;
    double d = c->st[c->fptop & 7];
    c->fptop = (c->fptop + 1) & 7;
    uint64_t db;
    memcpy(&db, &d, 8);
    int s = (int)(db >> 63), de = (int)((db >> 52) & 0x7ff);
    uint64_t dm = db & ((1ull << 52) - 1);
    uint64_t sig;
    uint16_t se;
    if (de == 0) {
        sig = 0;
        se = (uint16_t)(s ? 0x8000 : 0);
    } else {
        sig = (1ull << 63) | (dm << 11);
        int e80 = de - 1023 + 16383;
        se = (uint16_t)((s << 15) | (e80 & 0x7fff));
    }
    memcpy(ea, &sig, 8);
    memcpy(ea + 8, &se, 2);
}

// x87 transcendentals (R_X87FUNC): the D9 F0-FF subset has no ARM/SSE counterpart, so it is computed
// here on the double-precision ST stack via host libm. cpu->x87_ea carries the X87_* selector. We
// track no tag bits, so C1 (stack over/underflow) is cleared on success; C2 (argument out of range,
// |x| >= 2^63) is set for the trig ops exactly as the hardware does, leaving the operand untouched.
static void x87_push_d(struct cpu *c, double v) {
    c->fptop = (c->fptop - 1) & 7;
    c->st[c->fptop & 7] = v;
}
static void x87_func(struct cpu *c) {
    double st0 = c->st[c->fptop & 7];
    double st1 = c->st[(c->fptop + 1) & 7];
    c->fpsw &= ~0x4700ull; // clear C0/C1/C2/C3 (bits 8/9/10/14)
    switch (c->x87_ea) {
    case X87_F2XM1: // ST0 = 2^ST0 - 1
        c->st[c->fptop & 7] = exp2(st0) - 1.0;
        break;
    case X87_FYL2X: // ST1 = ST1 * log2(ST0); pop -> result in ST0
        c->st[(c->fptop + 1) & 7] = st1 * log2(st0);
        c->fptop = (c->fptop + 1) & 7;
        break;
    case X87_FPATAN: // ST1 = atan2(ST1, ST0); pop
        c->st[(c->fptop + 1) & 7] = atan2(st1, st0);
        c->fptop = (c->fptop + 1) & 7;
        break;
    case X87_FYL2XP1: // ST1 = ST1 * log2(ST0 + 1); pop
        c->st[(c->fptop + 1) & 7] = st1 * log2(st0 + 1.0);
        c->fptop = (c->fptop + 1) & 7;
        break;
    case X87_FPTAN: // ST0 = tan(ST0); push 1.0
        if (fabs(st0) >= 0x1p63) {
            c->fpsw |= 0x400;
            break;
        }
        c->st[c->fptop & 7] = tan(st0);
        x87_push_d(c, 1.0);
        break;
    case X87_FSINCOS: // ST0 = sin(ST0); push cos -> ST0=cos, ST1=sin
        if (fabs(st0) >= 0x1p63) {
            c->fpsw |= 0x400;
            break;
        }
        c->st[c->fptop & 7] = sin(st0);
        x87_push_d(c, cos(st0));
        break;
    case X87_FSIN:
        if (fabs(st0) >= 0x1p63) {
            c->fpsw |= 0x400;
            break;
        }
        c->st[c->fptop & 7] = sin(st0);
        break;
    case X87_FCOS:
        if (fabs(st0) >= 0x1p63) {
            c->fpsw |= 0x400;
            break;
        }
        c->st[c->fptop & 7] = cos(st0);
        break;
    }
}
