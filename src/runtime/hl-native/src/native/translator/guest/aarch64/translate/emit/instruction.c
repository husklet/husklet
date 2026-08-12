// translator/guest/aarch64 -- the aarch64-Linux -> arm64-host transliterator. Same-ISA: copy
// most instructions verbatim; MANGLE only stolen-register (x18/x28/x30) users. Optimizations: LSE
// atomic upgrade, §B shadow-return prediction (depth-gated), tier-2 purity gate. See OPTIMIZATIONS.md.

#include <assert.h>
#include "../../../../guest_fetch.h"
#include "../../../../../linux_abi/logical_vma.h"

static uint32_t a64_fetch_instruction(uint64_t guest, int *ok) {
    uint32_t instruction = 0;
    int success = hl_guest_fetch_u32(guest, &instruction) == 0;
    if (ok != NULL) *ok = success;
    return success ? instruction : 0;
}

// Non-PIE ET_EXEC link span + high-map bias. Really defined (and set by load_elf) in os/linux/container/
// vfs.c and os/linux/elf.c, both compiled LATER in the same unity TU; forward-declared here (static, so
// it merges into the single later definition) so adr/adrp can un-bias the PC. 0 for PIE/static-PIE.
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias;

// PC-relative base for adr/adrp materialization. A non-PIE ET_EXEC maps HIGH (the low 4GB is reserved),
// so the dispatcher biases the guest PC to the high mapping before translate_block -> gpc here is HIGH.
// But the image's baked absolute data pointers are LOW (non-PIE => no dynamic relocations), and Go/gcc
// compare an adr/adrp-computed pointer against such a stored pointer for identity; a HIGH result then
// mismatches the LOW baked pointer (gcc ICEs in set_static_spec; cc1 hits an invalid free()). Materialize
// adr/adrp against the LOW (un-biased) PC so the produced value matches the baked pointers; the
// nonpie_fixup SIGSEGV handler transparently serves the resulting LOW data access from the real high
// mapping (+bias). Branch/stitch/dispatch logic keeps the HIGH gpc -- only the *address value* adr/adrp
// produces becomes LOW. Inert for PIE/static-PIE (g_nonpie_lo == 0, the only state the test matrix sees).
static uint64_t pcrel_base(uint64_t gpc) {
    if (g_nonpie_lo && gpc >= g_nonpie_lo + g_nonpie_bias && gpc < g_nonpie_hi + g_nonpie_bias)
        return gpc - g_nonpie_bias;
    return gpc;
}

// ---- x18 stealing ----
// macOS asynchronously zeroes the real x18 (it is reserved on Apple platforms), but a
// Linux guest uses x18 as a normal GP register. So guest x18 must NEVER live in the real
// x18: it lives in cpu->x[18], and any guest instruction that names x18 is rewritten to
// use a scratch loaded from / stored back to cpu->x[18].
//
// gpr_field_mask: which of the 4 register fields are GP registers for this instruction.
//   bit0 = [4:0] (Rd/Rt)   bit1 = [9:5] (Rn)   bit2 = [20:16] (Rm/Rs)   bit3 = [14:10] (Rt2/Ra)
static int gpr_field_mask(uint32_t in) {
    uint32_t op = (in >> 25) & 0xF;
    // Data-processing immediate
    if (op == 8 || op == 9) {
        //   adr/adrp: Rd
        if ((in & 0x1F000000u) == 0x10000000u) return 1;
        //   move wide: Rd (imm in [20:5])
        if ((in & 0x1F800000u) == 0x12800000u) return 1;
        //   extr: Rd,Rn,Rm
        if ((in & 0x1F800000u) == 0x13800000u) return 1 | 2 | 4;
        //   add/sub-imm, logical-imm, bitfield: Rd,Rn
        return 1 | 2;
    }
    // Branches/Exception/System
    if (op == 0xA || op == 0xB) {
        //   mrs/msr <-> Rt
        if ((in & 0xFFD00000u) == 0xD5100000u) return 1;
        //   branches: handled as block enders
        return 0;
    }
    // Loads and Stores
    if ((in & 0x0A000000u) == 0x08000000u) {
        //   Rn[9:5] base is GP
        int v = (in >> 26) & 1, m = 2;
        //   Rt[4:0] GP unless SIMD/FP
        if (!v) m |= 1;
        //   register offset: Rm[20:16]
        if ((in & 0x3B200C00u) == 0x38200800u) m |= 4;
        //   LSE atomic memory ops (LDADD/LDCLR/LDEOR/LDSET/LDSMAX.../SWP): value operand Rs[20:16].
        //   Same encoding box as register-offset but bits[11:10]==00; without this a stolen Rs (x16/x17)
        //   would be emitted verbatim on the generic decode path and read the engine-private host reg.
        if ((in & 0x3B200C00u) == 0x38200000u) m |= 4;
        //   AdvSIMD load/store STRUCTURE, register post-index: the increment operand Rm[20:16] is a GPR
        //   (immediate post-index sets Rm==31/xzr, harmless to flag). Rt here is a vector list (bit26 keeps
        //   it unflagged) so only the base was marked; without this a stolen Rm stride (e.g. the generic
        //   decode path for `ld1 {v0.16b},[sp],x16`, where emit_fold_advsimd_struct is skipped on an SP base)
        //   would be emitted verbatim and advance the base by the engine-private host x16/x17.
        if ((in & 0xBE800000u) == 0x0C800000u) m |= 4;
        //   load/store pair: Rt2[14:10] (GP only)
        if ((in & 0x3A000000u) == 0x28000000u && !v) m |= 8;
        //   exclusive: Rs[20:16], Rt2[14:10]
        if ((in & 0x3F000000u) == 0x08000000u) m |= 4 | 8;
        return m;
    }
    // Data-processing register
    if ((in & 0x0E000000u) == 0x0A000000u) {
        //   3-source: Rd,Rn,Rm,Ra
        if ((in & 0x1F000000u) == 0x1B000000u) return 1 | 2 | 4 | 8;
        //   1-source: Rd,Rn (Rm field is opcode)
        if ((in & 0x5FE00000u) == 0x5AC00000u) return 1 | 2;
        if ((in & 0x1FE00000u) == 0x1A400000u)
            // ccmp/ccmn: [4:0]=nzcv; imm -> Rn only
            return (in & 0x800u) ? 2 : (2 | 4);
        //   logical/addsub-reg/cond-sel/2-source
        return 1 | 2 | 4;
    }
    // Scalar FP <-> integer / fixed-point conversions read or write a GENERAL-PURPOSE register even though
    // they sit in the scalar-FP encoding box (so the data-processing-register test above misses them).
    // Without flagging that GPR operand, a conversion naming a stolen reg (e.g. `fcvtzs w28,d0`) would be
    // emitted verbatim and clobber the engine's reserved x28=cpu pointer. The box is bits[30:24]==0011110;
    // a conversion is bit21==0 (fixed-point) OR bit21==1 with bits[15:10]==0 (integer) -- the only scalar-FP
    // forms with a zero opcode field there (FADD/FMOV/FCMP/... all have nonzero [15:10] when bit21==1).
    if ((in & 0x5F000000u) == 0x1E000000u && (!(in & 0x200000u) || !(in & 0xFC00u))) {
        int opcode = (in >> 16) & 7;
        // SCVTF/UCVTF (010/011) and FMOV-from-GPR (111) take the GPR as Rn[9:5]; every FP->GPR convert and
        // FMOV-to-GPR (110) takes it as Rd[4:0].
        return (opcode == 2 || opcode == 3 || opcode == 7) ? 2 : 1;
    }
    // AdvSIMD "copy" group also crosses the SIMD/GPR boundary: UMOV/SMOV write a GENERAL-PURPOSE Rd from a
    // vector lane, and DUP(general)/INS(general) read a GENERAL-PURPOSE Rn into a vector. They live in the
    // SIMD box (so the test below would miss them); naming a stolen reg there -- e.g. glibc's `dup v31.2d,x28`
    // -- would be emitted verbatim and read/clobber the engine's reserved x28=cpu pointer (silent data
    // corruption). Group: bit31==0, bits[28:24]==01110, bit21==0, bit15==0, bit10==1; op==bit29, imm4==[14:11].
    if ((in & 0x9F208400u) == 0x0E000400u) {
        int op = (in >> 29) & 1, imm4 = (in >> 11) & 0xF;
        if (!op && (imm4 == 5 || imm4 == 7)) return 1; // SMOV/UMOV: GPR is Rd[4:0]
        if (!op && (imm4 == 1 || imm4 == 3)) return 2; // DUP(general)/INS(general): GPR is Rn[9:5]
        return 0;                                      // DUP/INS(element): vector only
    }
    // SIMD/FP data: V registers only
    return 0;
}

static int field_is(uint32_t in, int bit, int shift) {
    return is_stolen((in >> shift) & 0x1F) && bit;
}

// "uses a STOLEN reg" (x18 / x28 [/ x30 in Stage B])
static int uses_x18(uint32_t in, int mask) {
    return field_is(in, mask & 1, 0) || field_is(in, mask & 2, 5) || field_is(in, mask & 4, 16) ||
           field_is(in, mask & 8, 10);
}

/* FEAT_I8MM matrix multiply is not present on every Apple Silicon generation.
   A Linux guest may still select it from its own build target, so copying these
   same-ISA opcodes verbatim would turn a supported guest instruction into a
   host SIGILL.  Lower the three integer matrix forms to baseline AdvSIMD.

   The architectural 2x8 by 8x2 operation treats each source's low/high eight
   bytes as the two rows.  Each output lane is one eight-element dot product.
   Widening the byte products to halfwords, pairwise widening to words, and
   ADDV therefore preserves the exact 32-bit modular accumulation semantics.
   USMMLA is U8*S8: reinterpret (u8 - 128) as s8, then add 128*sum(s8) for
   each right-hand row. */
// FEAT_DotProd, by contrast, IS advertised (cpu.h, AT_HWCAP bit 20) and needs no probe or lowering here:
// it is mandatory from Armv8.4-A, so SDOT/UDOT -- vector and by-element alike, neither of which touches a
// stolen GPR -- reach the verbatim emit at the bottom of the loop and land on silicon that has them. That is
// the same host assumption the already-advertised FEAT_LSE/AES/SHA2/CRC32 bits make.
// FEAT_I8MM / FEAT_BF16 are OPTIONAL, and the engine's CPU model does not advertise them, so a guest that
// uses them is already reaching past the contract. BFCVT is always lowered because hosts disagree on NaN
// canonicalization; the other instructions pass through when the host supports them. This matters for BFDOT:
// its architectural definition adds both bf16 products and the addend with a SINGLE rounding and forced
// FZ/DN, and it raises no FP exceptions -- properties the widen/fmul/pairwise-add decomposition cannot
// reproduce (the differential ISA fuzzer, tests/fuzz/isa/aarch64, showed 1-ulp results, wrong NaN payloads
// and a spurious FPSR.UFC). Probed once, before any translation runs.
static int g_host_i8mm, g_host_bf16;
// Both probes ask the HOST CPU for the extension, so both are gated on an AArch64 host: AT_HWCAP2 exists
// everywhere and its bits are architecture-defined, so on x86_64 bit 13 is not HWCAP2_I8MM and the translator
// would copy I8MM/BF16 opcodes verbatim on the strength of it. 0 selects baseline lowerings. Include
// cpu.h rather than trusting the unity TU: an undefined macro drops the probe on a REAL host.
#include "../../../../../host/cpu.h"
#if defined(__linux__) && defined(HL_HOST_CPU_AARCH64)
#include <sys/auxv.h>
#ifndef HWCAP2_I8MM
#define HWCAP2_I8MM (1u << 13)
#endif
#ifndef HWCAP2_BF16
#define HWCAP2_BF16 (1u << 14)
#endif
__attribute__((constructor)) static void hl_detect_host_matrix_ext(void) {
    unsigned long h2 = getauxval(AT_HWCAP2);
    g_host_i8mm = (h2 & HWCAP2_I8MM) ? 1 : 0;
    g_host_bf16 = (h2 & HWCAP2_BF16) ? 1 : 0;
}
#elif defined(__APPLE__) && defined(HL_HOST_CPU_AARCH64)
#include <sys/sysctl.h>

static int hl_sysctl_flag(const char *name) {
    int v = 0;
    size_t n = sizeof v;
    return sysctlbyname(name, &v, &n, NULL, 0) == 0 && v;
}

__attribute__((constructor)) static void hl_detect_host_matrix_ext(void) {
    g_host_i8mm = hl_sysctl_flag("hw.optional.arm.FEAT_I8MM");
    g_host_bf16 = hl_sysctl_flag("hw.optional.arm.FEAT_BF16");
}
#endif

static int is_i8mm_mmla(uint32_t in) {
    if (g_host_i8mm) return 0;
    uint32_t op = in & ~(0x1Fu | (0x1Fu << 5) | (0x1Fu << 16));
    return op == 0x4E80A400u || op == 0x6E80A400u || op == 0x4E80AC00u;
}

static uint32_t v3(uint32_t base, int d, int n, int m) {
    return base | ((uint32_t)m << 16) | ((uint32_t)n << 5) | (uint32_t)d;
}

static void emit_i8mm_dot(int out, int lane, int product, int left, int right, int is_unsigned) {
    /* SMULL/UMULL vProduct.8h, vLeft.8b, vRight.8b. */
    emit32(v3(is_unsigned ? 0x2E20C000u : 0x0E20C000u, product, left, right));
    /* [SU]ADDLP vProduct.4s, vProduct.8h; ADDV sProduct,vProduct.4s. */
    emit32((is_unsigned ? 0x6E602800u : 0x4E602800u) | ((uint32_t)product << 5) | (uint32_t)product);
    emit32(0x4EB1B800u | ((uint32_t)product << 5) | (uint32_t)product);
    /* INS vOut.s[lane],vProduct.s[0]. */
    emit32(0x6E040400u | ((uint32_t)lane << 19) | ((uint32_t)product << 5) | (uint32_t)out);
}

static void emit_i8mm_mmla(uint32_t in) {
    int d = (int)(in & 31u), n = (int)((in >> 5) & 31u), m = (int)((in >> 16) & 31u);
    int scratch = 0;
    /* Three operands cannot intersect every aligned group of four registers. */
    for (; scratch < 32; scratch += 4)
        if ((d < scratch || d >= scratch + 4) && (n < scratch || n >= scratch + 4) && (m < scratch || m >= scratch + 4))
            break;
    int out = scratch, product = scratch + 1, left = scratch + 2, right = scratch + 3;
    int is_unsigned = (in & 0x20000000u) != 0;
    int is_mixed = (in & 0x00000800u) != 0;

    /* Preserve scratch registers in their canonical cpu->v slots.  Async host
       signals resume this straight-line sequence and observe restored state at
       the next dispatcher boundary; no guest-SP window is introduced. */
    e_stp_q(out, product, CPUREG, OFF_V + out * 16);
    e_stp_q(left, right, CPUREG, OFF_V + left * 16);

    emit32(0x4F000400u | (uint32_t)out);  /* MOVI vOut.4s,#0 */
    emit32(v3(0x4EA01C00u, left, n, n));  /* MOV vLeft.16b,vN.16b */
    emit32(v3(0x4EA01C00u, right, m, m)); /* MOV vRight.16b,vM.16b */
    if (is_mixed) {
        /* U8 left -> signed (left-128), without widening. */
        emit32(0x4F04E400u | (uint32_t)product); /* MOVI vProduct.16b,#0x80 */
        emit32(v3(0x6E201C00u, left, left, product));
    }

    emit_i8mm_dot(out, 0, product, left, right, is_unsigned && !is_mixed);
    emit32(v3(0x6E000000u | (8u << 11), right, right, right)); /* EXT right rows by eight bytes */
    emit_i8mm_dot(out, 1, product, left, right, is_unsigned && !is_mixed);
    emit32(v3(0x6E000000u | (8u << 11), left, left, left)); /* EXT left rows by eight bytes */
    emit_i8mm_dot(out, 3, product, left, right, is_unsigned && !is_mixed);
    emit_i8mm_dot(out, 2, product, left, m, is_unsigned && !is_mixed);

    if (is_mixed) {
        /* vM row sums become [lo,hi,lo,hi], then multiply by 128. */
        emit32(0x4E202800u | ((uint32_t)m << 5) | (uint32_t)product);       /* SADDLP 8h */
        emit32(0x4E602800u | ((uint32_t)product << 5) | (uint32_t)product); /* SADDLP 4s */
        emit32(v3(0x4EA0BC00u, product, product, product));                 /* ADDP 4s */
        emit32(0x4F275400u | ((uint32_t)product << 5) | (uint32_t)product); /* SHL #7 */
        emit32(v3(0x4EA08400u, out, out, product));
    }
    emit32(v3(0x4EA08400u, d, d, out)); /* architectural accumulate */

    e_ldp_q(out, product, CPUREG, OFF_V + out * 16);
    e_ldp_q(left, right, CPUREG, OFF_V + left * 16);
}

/* FEAT_BF16 is likewise optional on Apple Silicon.  BFCVT's scalar result is
   the rounded high half of the IEEE binary32 encoding; BFDOT can be expressed
   exactly with baseline widening, shifts, FP multiply, and pairwise add. */
static int is_bf16_bfcvt(uint32_t in) {
    return (in & ~(0x1Fu | (0x1Fu << 5))) == 0x1E634000u;
}

static int is_bf16_bfdot(uint32_t in) {
    if (g_host_bf16) return 0;
    return (in & ~(0x1Fu | (0x1Fu << 5) | (0x1Fu << 16))) == 0x6E40FC00u;
}

static void emit_bf16_bfcvt(uint32_t in) {
    int d = (int)(in & 31u), n = (int)((in >> 5) & 31u);
    e_str(15, CPUREG, 15 * 8);
    if (!g_steal1617) e_stp(16, 17, CPUREG, 16 * 8);

    emit32(0x1E260000u | ((uint32_t)n << 5) | 16u);               /* FMOV w16,sN */
    emit32(0x2A1003EFu);                                          /* MOV w15,w16 (retain NaN test bits) */
    emit32(0x53107C00u | ((uint32_t)16 << 5) | 17u);              /* LSR w17,w16,#16 */
    emit32(0x12000000u | (1u << 10) | ((uint32_t)17 << 5) | 17u); /* AND w17,w17,#1 */
    emit32(0x0B000000u | ((uint32_t)17 << 16) | ((uint32_t)16 << 5) | 16u);
    emit32(0x528FFFE0u | 17u); /* MOV w17,#0x7fff */
    emit32(0x0B000000u | ((uint32_t)17 << 16) | ((uint32_t)16 << 5) | 16u);
    emit32(0x53107C00u | ((uint32_t)16 << 5) | 16u); /* LSR w16,w16,#16 */

    /* FPConvertBF returns the one default BF16 NaN. Select it branchlessly and
       preserve integer NZCV, which scalar BFCVT does not modify. */
    emit32(0x531779F1u);                                     /* UBFX w17,w15,#23,#8 */
    emit32(0x52001E31u);                                     /* EOR w17,w17,#0xff */
    emit32(0x5AC01231u);                                     /* CLZ w17,w17 */
    emit32(0x53057E31u);                                     /* LSR w17,w17,#5: exponent was all ones */
    emit32(0x120059EFu);                                     /* AND w15,w15,#0x7fffff */
    emit32(0x5AC011EFu);                                     /* CLZ w15,w15 */
    emit32(0x53057DEFu);                                     /* LSR w15,w15,#5 */
    emit32(0x520001EFu);                                     /* EOR w15,w15,#1: mantissa was nonzero */
    emit32(0x0A0F0231u);                                     /* AND w17,w17,w15: source was NaN */
    emit32(0x4B1103F1u);                                     /* NEG w17,w17: selection mask */
    emit32(v3(0x0A200000u, 16, 16, 17));                     /* BIC w16,w16,w17 */
    emit32(0x528FF80Fu);                                     /* MOV w15,#0x7fc0 */
    emit32(v3(0x0A000000u, 15, 15, 17));                     /* AND w15,w15,w17 */
    emit32(v3(0x2A000000u, 16, 16, 15));                     /* ORR w16,w16,w15 */
    emit32(0x1E270000u | ((uint32_t)16 << 5) | (uint32_t)d); /* FMOV sD,w16 */

    if (!g_steal1617) e_ldp(16, 17, CPUREG, 16 * 8);
    e_ldr(15, CPUREG, 15 * 8);
}

static void emit_bf16_bfdot(uint32_t in) {
    int d = (int)(in & 31u), n = (int)((in >> 5) & 31u), m = (int)((in >> 16) & 31u);
    int scratch = 0;
    for (; scratch < 32; scratch += 4)
        if ((d < scratch || d >= scratch + 4) && (n < scratch || n >= scratch + 4) && (m < scratch || m >= scratch + 4))
            break;
    int lo = scratch, hi = scratch + 1, rhs = scratch + 2, spare = scratch + 3;
    e_stp_q(lo, hi, CPUREG, OFF_V + lo * 16);
    e_stp_q(rhs, spare, CPUREG, OFF_V + rhs * 16);

    emit32(0x2F10A400u | ((uint32_t)n << 5) | (uint32_t)lo);  /* UXTL low N */
    emit32(0x4F305400u | ((uint32_t)lo << 5) | (uint32_t)lo); /* SHL #16 */
    emit32(0x2F10A400u | ((uint32_t)m << 5) | (uint32_t)rhs);
    emit32(0x4F305400u | ((uint32_t)rhs << 5) | (uint32_t)rhs);
    emit32(v3(0x6E20DC00u, lo, lo, rhs)); /* FMUL low pairs */

    emit32(0x6F10A400u | ((uint32_t)n << 5) | (uint32_t)hi); /* UXTL2 high N */
    emit32(0x4F305400u | ((uint32_t)hi << 5) | (uint32_t)hi);
    emit32(0x6F10A400u | ((uint32_t)m << 5) | (uint32_t)rhs);
    emit32(0x4F305400u | ((uint32_t)rhs << 5) | (uint32_t)rhs);
    emit32(v3(0x6E20DC00u, hi, hi, rhs)); /* FMUL high pairs */

    emit32(v3(0x6E20D400u, lo, lo, hi)); /* FADDP -> four dot products */
    emit32(v3(0x4E20D400u, d, d, lo));   /* accumulate */

    e_ldp_q(lo, hi, CPUREG, OFF_V + lo * 16);
    e_ldp_q(rhs, spare, CPUREG, OFF_V + rhs * 16);
}

// ---- steal-mode stolen-reg FAST PATHS (perf: the mangle machinery measured ~20% of CPython wall) ----
// Under the default x16/x17 steal (g_steal1617), host x16/x17 are ENGINE-PRIVATE at every point inside a
// block body: the prologue never loads them, chained entries keep them dead, and every IBTC probe/irq
// poll clobbers them freely (emit_set_x30 and emit_irq_check already rely on exactly this). So stolen-reg
// traffic does not need the legacy mscratch spill/restore dance (which cost 4+ extra memory ops per
// mangled instruction) or the 3-insn TLS-based cpu reload of x18_prolog (x28 IS the cpu pointer,
// maintained for the whole block): load cpu->x[stolen] straight into host x16/x17, run the rewritten
// instruction, store back. Sampled attribution on the CPython eval loop showed the mscratch dance +
// cpu->x[] traffic at ~20% of total run time (PLT stubs -- adrp x16/ldr x17/add x16/br x17, all-stolen --
// alone were 19% of samples), so this is the single biggest engine tax on call-heavy aarch64 guests.
static int stealfast_on(void) {
    return g_steal1617;
}

// Emit a guest insn that references stolen reg(s): for each, a scratch S = cpu->x[stolen]; run the
// insn with the stolen field(s) replaced by scratch(es); store back. Real x28 = cpu is the base;
// scratch originals are spilled to cpu->mscratch (NOT the stack -- that would collide with the
// guest's own stp/ldp frame stores + writeback). An instruction has up to FOUR register fields
// (Rd/Rn/Rm/Ra of the 3-source madd/msub family), so up to four DISTINCT stolen regs can appear in
// one insn (e.g. `madd x16,x17,x18,x28`); size both arrays for that (mscratch[8] backs the spill).
// Undersizing them to 2 overflowed the stack on such an insn -> __stack_chk_fail abort (cc1/libc).
static void emit_mangled_x18(uint32_t in, int mask) {
    static const int shifts[4] = {0, 5, 16, 10}, mbits[4] = {1, 2, 4, 8};
    int read_mask = mask, write_mask = mask;
    uint32_t op = (in >> 25) & 0xF;
    if (op == 8 || op == 9) {
        if ((in & 0x1F000000u) == 0x10000000u) {
            read_mask = 0; // ADR/ADRP
            write_mask = mask & 1;
        } else if ((in & 0x1F800000u) == 0x12800000u) {
            write_mask = mask & 1; // MOVN/Z/K
            read_mask = ((in >> 29) & 3) == 3 ? write_mask : 0;
        } else if ((in & 0x1F800000u) == 0x13800000u) {
            read_mask = mask & (2 | 4); // EXTR
            write_mask = mask & 1;
        } else if ((in & 0x1F800000u) == 0x13000000u) {
            // SBFM/UBFM read Rn and overwrite Rd. BFM merges selected bits
            // into the old Rd value, so it reads Rd as well as writing it.
            read_mask = mask & 2;
            if (((in >> 29) & 3) == 1) read_mask |= mask & 1;
            write_mask = mask & 1;
        } else {
            read_mask = mask & 2;
            write_mask = mask & 1;
        }
    } else if ((in & 0x0E000000u) == 0x0A000000u) {
        read_mask = mask & (2 | 4 | 8);
        write_mask = mask & 1;
    } else if (!(in & 0x04000000u) && ((in & 0x3B000000u) == 0x39000000u || (in & 0x3B200000u) == 0x38000000u ||
                                       (in & 0x3B200C00u) == 0x38200800u)) {
        // Ordinary integer single load/store.  Atomics/exclusives occupy a
        // different encoding box and deliberately stay conservative.
        int opc = (int)((in >> 22) & 3);
        int size = (int)((in >> 30) & 3);
        if (!(size == 3 && opc == 2)) { // PRFM encodes an operation, not a GPR Rt
            int mode = (int)((in >> 10) & 3);
            int writeback = (in & 0x3B200000u) == 0x38000000u && (mode == 1 || mode == 3);
            int base_and_index = mask & (2 | 4);
            if (opc == 0) { // store: Rt/base/index inputs
                read_mask = mask & (1 | 2 | 4);
                write_mask = writeback ? (mask & 2) : 0;
            } else { // load: address inputs, Rt (+ writeback base) outputs
                read_mask = base_and_index;
                write_mask = (mask & 1) | (writeback ? (mask & 2) : 0);
            }
        }
    }
    // Ordinary integer pair transfers have directional operands.  Treating
    // every stolen field as read/write needlessly loaded an LDP destination
    // before the instruction overwrote it and stored an STP source back
    // unchanged afterwards.  This is especially costly for the ubiquitous
    // x29/x30 frame pair.  Keep exclusive/CASP and SIMD pairs on the fully
    // conservative path; their operand rules are handled elsewhere.
    if ((in & 0x3A000000u) == 0x28000000u && !(in & 0x04000000u)) {
        int data = mask & (1 | 8);
        int base = mask & 2;
        int mode = (int)((in >> 23) & 3);
        int writeback = mode == 1 || mode == 3;
        if (in & (1u << 22)) { // LDP: data outputs, base input (+ output on writeback)
            read_mask = base;
            write_mask = data | (writeback ? base : 0);
        } else { // STP: data/base inputs, only a writeback base is an output
            read_mask = data | base;
            write_mask = writeback ? base : 0;
        }
    }
    int stolen[4], ns = 0, used = 0;
    for (int k = 0; k < 4; k++)
        if (mask & mbits[k]) {
            int rf = (in >> shifts[k]) & 0x1F;
            used |= 1 << rf;
            if (is_stolen(rf)) {
                int seen = 0;
                for (int j = 0; j < ns; j++)
                    if (stolen[j] == rf) seen = 1;
                if (!seen) stolen[ns++] = rf;
            }
        }
    // FAST PATH (stealfast): host x16/x17 are engine-dead here, so a mangle with <= 2 distinct stolen
    // regs needs no mscratch spill/restore. Same loads, same rewritten insn, same store-backs as the
    // legacy path below -- ONLY the scratch registers differ (engine-dead x16/x17 instead of spilled
    // guest regs), so guest-visible state is identical by construction. >2 distinct stolen regs (a
    // 3-source madd naming three of x16/x17/x18/x28/x30 -- vanishingly rare) falls to the legacy path
    // (host x30 is NOT usable as a third scratch: §B, when enabled, keeps a live host return address
    // in it across the block body).
    if (stealfast_on() && ns <= 2) {
        static const int hsc[2] = {16, 17};
        for (int i = 0; i < ns; i++) {
            int read = 0;
            for (int k = 0; k < 4; k++)
                if ((read_mask & mbits[k]) && (int)((in >> shifts[k]) & 31) == stolen[i]) read = 1;
            if (read)
                // scratch = cpu->x[stolen]
                e_ldr(hsc[i], CPUREG, stolen[i] * 8);
            else
                emit32(0xD503201Fu); // preserve established block layout without the dead load
        }
        uint32_t m = in;
        for (int k = 0; k < 4; k++)
            if (mask & mbits[k]) {
                int rf = (m >> shifts[k]) & 0x1F;
                if (is_stolen(rf)) {
                    int s = hsc[0];
                    for (int i = 0; i < ns; i++)
                        if (stolen[i] == rf) s = hsc[i];
                    m = (m & ~(0x1Fu << shifts[k])) | ((unsigned)s << shifts[k]);
                }
            }
        emit32(m);
        for (int i = 0; i < ns; i++) {
            int written = 0;
            for (int k = 0; k < 4; k++)
                if ((write_mask & mbits[k]) && (int)((in >> shifts[k]) & 31) == stolen[i]) written = 1;
            if (written)
                // cpu->x[stolen] = scratch
                e_str(hsc[i], CPUREG, stolen[i] * 8);
            else
                emit32(0xD503201Fu); // preserve established block layout without the dead store
        }
        return;
    }
    int sc[4], nsc = 0;
    for (int r = 0; r <= 27 && nsc < ns; r++)
        if (!(used & (1 << r)) && !is_stolen(r)) sc[nsc++] = r;
    for (int i = 0; i < ns; i++)
        // spill scratch -> cpu->mscratch
        e_str(sc[i], CPUREG, (int)OFF_MSCRATCH + 8 * i);
    for (int i = 0; i < ns; i++) {
        int read = 0;
        for (int k = 0; k < 4; k++)
            if ((read_mask & mbits[k]) && (int)((in >> shifts[k]) & 31) == stolen[i]) read = 1;
        if (read)
            // scratch = cpu->x[stolen]
            e_ldr(sc[i], CPUREG, stolen[i] * 8);
        else
            emit32(0xD503201Fu);
    }
    uint32_t m = in;
    for (int k = 0; k < 4; k++)
        if (mask & mbits[k]) {
            int rf = (m >> shifts[k]) & 0x1F;
            if (is_stolen(rf)) {
                int s = sc[0];
                for (int i = 0; i < ns; i++)
                    if (stolen[i] == rf) s = sc[i];
                m = (m & ~(0x1Fu << shifts[k])) | ((unsigned)s << shifts[k]);
            }
        }
    emit32(m);
    for (int i = 0; i < ns; i++) {
        int written = 0;
        for (int k = 0; k < 4; k++)
            if ((write_mask & mbits[k]) && (int)((in >> shifts[k]) & 31) == stolen[i]) written = 1;
        if (written)
            // cpu->x[stolen] = scratch
            e_str(sc[i], CPUREG, stolen[i] * 8);
        else
            emit32(0xD503201Fu);
    }
    for (int i = 0; i < ns; i++)
        // restore scratch
        e_ldr(sc[i], CPUREG, (int)OFF_MSCRATCH + 8 * i);
}

// ---- CASP/CASPA/CASPL/CASPAL: paired compare-and-swap (DWCAS / __int128 lock-free) ----
// The Rs pair (Xs,Xs+1) is expected-value IN / old-value OUT; the Rt pair (Xt,Xt+1) is new-value IN;
// base is [Xn]. gpr_field_mask flags the NAMED Rs/Rt fields, but emit_mangled_x18 only substitutes the
// named field -- it does NOT relocate the IMPLICIT pair partner (Xs+1 / Xt+1). So when a pair member is a
// stolen reg (x16/x17 -- the default steal pair -- or x18/x28/x30), that partner reads/writes the
// engine-private host register instead of the guest value, silently corrupting the atomic (a stolen Xs
// makes the compare see garbage in the high half -> the swap spuriously fails, or a stolen Xs writeback
// loses the observed old value). Relocate each guest pair into a free even/odd host pair, run the CASP,
// and write the Rs pair result back. Encoding: (in & 0xBFA07C00) == 0x08207C00 (bit23==0 excludes CAS).
static int is_casp(uint32_t in) {
    return (in & 0xBFA07C00u) == 0x08207C00u;
}

static int casp_uses_stolen(uint32_t in) {
    int Rs = (in >> 16) & 31, Rt = in & 31, Rn = (in >> 5) & 31;
    return is_stolen(Rs) || is_stolen((Rs + 1) & 31) || is_stolen(Rt) || is_stolen((Rt + 1) & 31) || is_stolen(Rn);
}

static void emit_casp_mangled(uint32_t in, int override_base) {
    int Rs = (in >> 16) & 31, Rt = in & 31, Rn = (in >> 5) & 31;
    int touch[5] = {Rs, (Rs + 1) & 31, Rt, (Rt + 1) & 31, override_base >= 0 ? override_base : Rn};
    // Two DISTINCT free even host pairs (P = Rs role, Q = Rt role): neither member stolen, neither a guest
    // reg the op names. Register 31 in a CASP field means xzr (not SP), so it is safe to leave in place.
    int P = -1, Q = -1, Nr = -1;
    for (int r = 0; r <= 26; r += 2) {
        int bad = is_stolen(r) || is_stolen(r + 1);
        for (int k = 0; k < 5; k++)
            if (touch[k] == r || touch[k] == r + 1) bad = 1;
        if (bad) continue;
        if (P < 0)
            P = r;
        else {
            Q = r;
            break;
        }
    }
    // Base scratch only when Xn itself is stolen (a non-stolen Xn -- including SP=31 -- stays in the op).
    if (override_base < 0 && is_stolen(Rn))
        for (int r = 0; r <= 27; r++) {
            if (is_stolen(r) || r == P || r == P + 1 || r == Q || r == Q + 1) continue;
            int bad = 0;
            for (int k = 0; k < 5; k++)
                if (touch[k] == r) bad = 1;
            if (!bad) {
                Nr = r;
                break;
            }
        }
    // Spill the host scratch originals (live guest values) to cpu->mscratch[0..].
    int spill[5], nsp = 0;
    spill[nsp++] = P;
    spill[nsp++] = P + 1;
    spill[nsp++] = Q;
    spill[nsp++] = Q + 1;
    if (Nr >= 0) spill[nsp++] = Nr;
    for (int i = 0; i < nsp; i++)
        e_str(spill[i], CPUREG, (int)OFF_MSCRATCH + 8 * i);
    // Load guest pair values (a stolen member lives in its cpu slot; else in the live host reg).
#define CASP_LOADG(dst, g)                                                                                             \
    do {                                                                                                               \
        if (is_stolen(g))                                                                                              \
            e_ldr((dst), CPUREG, (g) * 8);                                                                             \
        else                                                                                                           \
            e_movr((dst), (g));                                                                                        \
    } while (0)
    CASP_LOADG(P, Rs);
    CASP_LOADG(P + 1, (Rs + 1) & 31);
    CASP_LOADG(Q, Rt);
    CASP_LOADG(Q + 1, (Rt + 1) & 31);
#undef CASP_LOADG
    if (Nr >= 0) e_ldr(Nr, CPUREG, Rn * 8);
    int base = override_base >= 0 ? override_base : (Nr >= 0) ? Nr : Rn;
    uint32_t m =
        (in & ~((0x1Fu << 16) | (0x1Fu << 5) | 0x1Fu)) | ((uint32_t)P << 16) | ((uint32_t)base << 5) | (uint32_t)Q;
    emit32(m);
    // CASP wrote the old memory pair into P,P+1 (the Rs pair) -> store back to guest Rs,Rs+1.
    if (is_stolen(Rs))
        e_str(P, CPUREG, Rs * 8);
    else
        e_movr(Rs, P);
    if (is_stolen((Rs + 1) & 31))
        e_str(P + 1, CPUREG, ((Rs + 1) & 31) * 8);
    else
        e_movr((Rs + 1) & 31, P + 1);
    for (int i = 0; i < nsp; i++)
        e_ldr(spill[i], CPUREG, (int)OFF_MSCRATCH + 8 * i);
}

// ---- guest_base bias-fold for non-PIE ET_EXEC images ----
// A non-PIE image maps HIGH (+g_nonpie_bias) but its baked absolute pointers stay LOW (link vaddr); a
// guest load/store through such a pointer would hit the unmapped low address and trap (one SIGSEGV per
// access -> cc1 ~400s). Instead, fold the bias into the effective address at translate time: if the access
// targets a LOW image address, add g_nonpie_bias so it lands directly in the high mapping. Stack/heap/mmap
// pointers are real HIGH addresses (>= 4GiB, above the engine's 4GiB __PAGEZERO), so the discriminator is
// "EA < 4GiB" <=> image. The common single-base + register-offset + writeback forms are folded; the
// monitor-exclusive pair, AdvSIMD load/store structures, DC-ZVA, and the LSE-upgraded atomic loops fall
// through to nonpie_fixup, the safety net (still correct, just a per-access fault). Inert for PIE.

// Is `in` a base-register memory op whose effective address we fold? We fold ONLY the forms with a single
// base register Xn[9:5] + a (possibly absent) immediate, so the "is this a LOW image address" test on Xn
// is sound: a LOW Xn means an image access, regardless of the small immediate. Excluded (left to the
// nonpie_fixup safety net -- still correct, just a per-access fault):
//   - ldr-literal (PC-relative; already materialized HIGH)
//   - writeback (pre/post-index)
//   - the exclusive-MONITOR pair (a scratch spill between ldxr/stxr clears the monitor)
//   - AdvSIMD load/store structures, DC ZVA.
// The register-offset form [Xn,Xm{,ext}] HAS two address registers (EA = Xn + extend(Xm)); it is folded
// too, but by computing the full EA first and testing THAT (biasing Xn alone is wrong when the pointer is
// the high Xm and Xn a small index -- that corrupted glibc/ld.so). See emit_fold_mem.
static int is_foldable_mem(uint32_t in) {
    if ((in & 0x0A000000u) != 0x08000000u) return 0; // not in the loads/stores major group
    if ((in & 0x3B000000u) == 0x18000000u) return 0; // ldr (literal): handled separately, maps HIGH
    if ((in & 0x3B000000u) == 0x39000000u) return 1; // LDR/STR unsigned-offset (int + SIMD): no WB
    if ((in & 0x3B200000u) == 0x38000000u)
        return 1;                                    // unscaled / unpriv / post / pre (single base Xn; WB
                                                     // handled by emit_fold_mem -- post/pre are the hot form)
    if ((in & 0x3B200C00u) == 0x38200800u) return 1; // register-offset [Xn,Xm{,ext}]: full-EA fold below
    if ((in & 0x3A000000u) == 0x28000000u) {         // LDP/STP family
        return 1;                                    /* no-alloc, offset, post-index, and pre-index */
    }
    if ((in & 0x3F000000u) == 0x08000000u)           // exclusive / ordered / CAS group ([Xn] base)
        return (in & 0x00800000u) != 0;              // bit23: 1=LDAR/STLR/CAS (single) -> fold; 0=monitor pair
    if ((in & 0x3B200C00u) == 0x38200000u) return 1; // LSE atomic memory ops (LDADD/SWP/...): [Xn]
    return 0;
}

static int is_prfm_register_or_immediate(uint32_t in) {
    if ((in & 0x04000000u) != 0) return 0; /* SIMD/FP */
    if (((in >> 30) & 3) != 3 || ((in >> 22) & 3) != 2) return 0;
    return (in & 0x3B000000u) == 0x39000000u || (in & 0x3B200000u) == 0x38000000u || (in & 0x3B200C00u) == 0x38200800u;
}

static uint64_t a64_mem_bytes(uint32_t in) {
    if (is_casp(in)) return (in & (1u << 30)) ? 16 : 8;
    int pair = (in & 0x3A000000u) == 0x28000000u;
    int vector = (in >> 26) & 1;
    unsigned size = (in >> 30) & 3;
    uint64_t bytes;
    if (pair)
        bytes = vector ? (UINT64_C(4) << size) : (size == 2 ? UINT64_C(8) : UINT64_C(4));
    else if (!vector)
        bytes = UINT64_C(1) << size;
    else {
        unsigned scale = ((((in >> 22) & 3u) >> 1) << 2) | size;
        bytes = UINT64_C(1) << scale; /* B/H/S/D/Q scalar or vector */
    }
    if ((in & 0x3F000000u) == 0x08000000u && !(in & (1u << 23)) && (in & (1u << 21))) bytes *= 2; /* LDXP/STXP */
    return pair ? bytes * 2 : bytes;
}

static uint32_t a64_mem_required(uint32_t in) {
    /* LSE RMW and compare-and-swap instructions both read and write. */
    if ((in & 0x3B200C00u) == 0x38200000u || is_casp(in) || ((in & 0x3FA07C00u) == 0x08A07C00u))
        return HL_LOGICAL_VMA_READ | HL_LOGICAL_VMA_WRITE;
    return (in & (1u << 22)) ? HL_LOGICAL_VMA_READ : HL_LOGICAL_VMA_WRITE;
}

/* Byte displacement of the access performed by a foldable memory opcode.
   The copied opcode is de-indexed after this displacement is folded into Sb,
   so the BUS query and the native access use exactly the same address. */
static int64_t a64_fold_mem_offset(uint32_t in, int wb) {
    if ((in & 0x3a000000u) == 0x28000000u) {
        if (wb == 2) return 0; /* pair post-index accesses before writeback */
        int64_t element = (int64_t)(a64_mem_bytes(in) / 2);
        return sext((in >> 15) & 0x7f, 7) * element;
    }
    if (wb == 2) return 0; /* post-index accesses before writeback */
    if (wb == 1) return sext((in >> 12) & 0x1ff, 9);
    if ((in & 0x3b000000u) == 0x39000000u) {
        uint64_t bytes = a64_mem_bytes(in);
        return (int64_t)((in >> 10) & 0xfff) << __builtin_ctzll(bytes);
    }
    if ((in & 0x3b200000u) == 0x38000000u) return sext((in >> 12) & 0x1ff, 9);
    return 0; /* register-offset already materialized; atomics use [Xn] */
}

/*
 * Sparse logical-VMA software-TLB guard.
 *
 * Mapping activation retires pre-guard translations.  Mapping mutation then
 * clears soft_page for every CPU while peers are stopped, before an immutable
 * snapshot/backing can be reclaimed.  A hit consequently needs no global
 * generation read.  All instructions below are flag-free: in particular, a
 * guard between LDXR and STXR cannot perturb guest NZCV or perform a store
 * that would destroy the host exclusive monitor.
 *
 * The caller computes the architectural guest EA in `ea`, emits the native
 * operation against that register between begin/end, and supplies two
 * engine-owned temporaries.  On a hit `ea` becomes its canonical host
 * address.  Misses and discontinuous page spans exit before the operation and
 * retry the same guest PC after dispatcher handling.
 */
