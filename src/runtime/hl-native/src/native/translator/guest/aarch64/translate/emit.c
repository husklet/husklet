// translator/guest/aarch64 -- the aarch64-Linux -> arm64-host transliterator. Same-ISA: copy
// most instructions verbatim; MANGLE only stolen-register (x18/x28/x30) users. Optimizations: LSE
// atomic upgrade, §B shadow-return prediction (depth-gated), tier-2 purity gate. See OPTIMIZATIONS.md.

#include <assert.h>
#include "../../../guest_fetch.h"
#include "../../../../linux_abi/logical_vma.h"

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
#include "../../../../host/cpu.h"
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
struct a64_soft_guard {
    uint32_t *miss[6];
    uint32_t *direct[2];
    int miss_bit[6]; /* -1 = CBNZ; otherwise TBZ bit */
    int miss_reg[6];
    unsigned nmiss;
    unsigned ndirect;
    int ea;
    int tmp;
    int tmp2;
    uint64_t bytes;
    uint32_t required;
    uint64_t pc;
    uint8_t *native;
    uint8_t *metadata;
    int shared;
    int active;
    int profile_sample;
    int restore_reg[4];
    int restore_offset[4];
    unsigned nrestore;
};

#define SOFT_STUB_PATCH_MAX 65536
static uint32_t *g_soft_stub_patches[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_stub_patch_count;
static uint32_t *g_soft_legacy_stub_patches[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_legacy_stub_patch_count;
static uint32_t *g_soft_resolver_patches[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_resolver_bytes[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_resolver_required[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_resolver_patch_count;

static void emit_a64_bus_guard(int, uint64_t, uint64_t);
static void patch_adr(uint32_t *, uint8_t *, unsigned);
static int shadowgate(void);
static void emit_prof_bump(void *);

static int soft_profile_sample(uint64_t pc) {
    return g_prof && ((((pc >> 2) * UINT64_C(0x9e3779b97f4a7c15)) >> 58) == 0);
}

static uint32_t a64_cbnz_x(int reg, int64_t words) {
    return 0xB5000000u | (((uint32_t)words & 0x7ffffu) << 5) | (unsigned)reg;
}

static uint32_t a64_tbz_x(int reg, unsigned bit, int64_t words) {
    return 0x36000000u | ((bit & 0x20u) << 26) | ((bit & 0x1fu) << 19) | (((uint32_t)words & 0x3fffu) << 5) |
           (unsigned)reg;
}

static struct a64_soft_guard emit_a64_soft_guard_begin(int ea, int tmp, int tmp2, uint64_t bytes, uint32_t required,
                                                       uint64_t pc) {
    struct a64_soft_guard guard = {.ea = ea, .tmp = tmp, .tmp2 = tmp2, .bytes = bytes, .required = required, .pc = pc};
    int resume_ea = ea;
    if (!jit_guest_soft_active()) return guard;
    guard.active = 1;
    guard.profile_sample = soft_profile_sample(pc);
    if (guard.profile_sample) g_prof_soft_sites_sampled++;
    assert(ea != tmp && ea != tmp2 && tmp != tmp2);
    assert(bytes != 0 && bytes <= 4096);
    /*
     * With the shadow-RAS disabled x30 carries no live engine return link.
     * Use it as the resolver's per-site continuation, normalize every EA in
     * x16, and share the complete interval/permission check once per block.
     * Shadow-enabled builds retain the proven inline guard below.
     */
    guard.shared = shadowgate() < 0 && !g_tier2_build && !guard.profile_sample;
    if (guard.shared) {
        if (ea != 16) e_movr(16, ea);
        guard.ea = 16;
        guard.tmp = 17;
        guard.tmp2 = 18;
        ea = 16;
        tmp = 17;
        tmp2 = 18;
    }

    /*
     * Most accesses in a process with one sparse 4 KiB alias still target
     * ordinary identity-mapped stack/heap pages. Reject those against the
     * conservative logical-VMA hull before consulting the per-page software
     * TLB. All arithmetic and branches are flag-free, preserving guest NZCV
     * and host exclusive-monitor state.
     *
     * Below-hull: first - (ea+bytes) has bit 63 clear when the access ends at
     * or below first. Above-hull: ea-last has bit 63 clear when ea >= last.
     * Equality is intentionally classified direct; overlap remains on the
     * guarded path.
     */
    e_ldr(tmp, CPUREG, OFF_SOFT_FILTER_FIRST);
    e_movconst(tmp2, bytes);
    emit32(0x8B000000u | ((unsigned)tmp2 << 16) | ((unsigned)ea << 5) | (unsigned)tmp2);
    emit32(0xCB000000u | ((unsigned)tmp2 << 16) | ((unsigned)tmp << 5) | (unsigned)tmp2);
    emit32(0xD37FFC00u | ((unsigned)tmp2 << 5) | (unsigned)tmp2);
    guard.direct[guard.ndirect++] = (uint32_t *)g_cp;
    emit32(0); /* tbz tmp2,#0,direct */

    e_ldr(tmp, CPUREG, OFF_SOFT_FILTER_LAST);
    emit32(0xCB000000u | ((unsigned)tmp << 16) | ((unsigned)ea << 5) | (unsigned)tmp2);
    emit32(0xD37FFC00u | ((unsigned)tmp2 << 5) | (unsigned)tmp2);
    guard.direct[guard.ndirect++] = (uint32_t *)g_cp;
    emit32(0); /* tbz tmp2,#0,direct */

    if (guard.shared) {
        /*
         * x17 points at fixed metadata and a plain branch enters the shared
         * resolver. The native continuation immediately follows metadata:
         *   [pc:u64, miss_delta:i32, pad:u32]
         */
        uint32_t *metadata_address = (uint32_t *)g_cp;
        emit32(0); /* adr x17,metadata */
        if (g_soft_resolver_patch_count >= SOFT_STUB_PATCH_MAX) {
            static const char message[] = "too many shared soft-memory guards in one translated block";
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            _exit(70);
        }
        uint32_t resolver_index = g_soft_resolver_patch_count++;
        g_soft_resolver_patches[resolver_index] = (uint32_t *)g_cp;
        g_soft_resolver_bytes[resolver_index] = (uint32_t)bytes;
        g_soft_resolver_required[resolver_index] = required;
        emit32(0); /* b shared_soft_resolver */
        guard.metadata = g_cp;
        patch_adr(metadata_address, guard.metadata, 17);
        memcpy(g_cp, &pc, sizeof pc);
        g_cp += sizeof pc;
        memset(g_cp, 0, 4); /* miss displacement, filled by guard end */
        g_cp += 4;
        uint16_t narrow_bytes = (uint16_t)bytes;
        memcpy(g_cp, &narrow_bytes, sizeof narrow_bytes);
        g_cp += sizeof narrow_bytes;
        *g_cp++ = (uint8_t)required;
        *g_cp++ = 0;
        guard.native = g_cp;
        if (resume_ea != 16) e_movr(resume_ea, 16);
        return guard;
    }

    /* Width-independent cached interval hit, using sign bits of non-setting
       subtracts. Linux userspace canonical addresses are below 2^63, so an
       unsigned underflow is exactly the high-bit test here. */
    e_ldr(tmp, CPUREG, OFF_SOFT_PAGE); /* inclusive first */
    emit32(0xCB000000u | ((unsigned)tmp << 16) | ((unsigned)ea << 5) | (unsigned)tmp2);
    emit32(0xD37FFC00u | ((unsigned)tmp2 << 5) | (unsigned)tmp2); /* lsr tmp2,tmp2,#63 */
    guard.miss[guard.nmiss++] = (uint32_t *)g_cp;
    guard.miss_bit[guard.nmiss - 1] = -1;
    guard.miss_reg[guard.nmiss - 1] = tmp2;
    emit32(0);

    e_ldr(tmp, CPUREG, OFF_SOFT_LIMIT); /* exclusive end */
    e_movconst(tmp2, bytes);
    emit32(0x8B000000u | ((unsigned)tmp2 << 16) | ((unsigned)ea << 5) | (unsigned)tmp2);
    emit32(0xCB000000u | ((unsigned)tmp2 << 16) | ((unsigned)tmp << 5) | (unsigned)tmp2);
    emit32(0xD37FFC00u | ((unsigned)tmp2 << 5) | (unsigned)tmp2);
    guard.miss[guard.nmiss++] = (uint32_t *)g_cp;
    guard.miss_bit[guard.nmiss - 1] = -1;
    guard.miss_reg[guard.nmiss - 1] = tmp2;
    emit32(0);

    e_ldr(tmp, CPUREG, OFF_SOFT_PROTECTION);
    if (required & HL_LOGICAL_VMA_READ) {
        guard.miss[guard.nmiss++] = (uint32_t *)g_cp;
        guard.miss_bit[guard.nmiss - 1] = 0;
        guard.miss_reg[guard.nmiss - 1] = tmp;
        emit32(0); /* tbz tmp,#0,miss */
    }
    if (required & HL_LOGICAL_VMA_WRITE) {
        guard.miss[guard.nmiss++] = (uint32_t *)g_cp;
        guard.miss_bit[guard.nmiss - 1] = 1;
        guard.miss_reg[guard.nmiss - 1] = tmp;
        emit32(0); /* tbz tmp,#1,miss */
    }
    e_ldr(tmp, CPUREG, OFF_SOFT_DELTA);
    emit32(0x8B000000u | ((unsigned)tmp << 16) | ((unsigned)ea << 5) | (unsigned)ea); /* add ea,ea,tmp */
    if (guard.profile_sample) emit_prof_bump(&g_prof_soft_cached_sampled);
    guard.native = g_cp;
    return guard;
}

static void a64_soft_guard_restore(struct a64_soft_guard *guard, int reg, int offset) {
    assert(guard->nrestore < 4);
    guard->restore_reg[guard->nrestore] = reg;
    guard->restore_offset[guard->nrestore] = offset;
    guard->nrestore++;
}

/*
 * A soft-TLB miss is cold, but the old lowering put a complete architectural
 * spill and block-return sequence at every guest memory instruction.  A
 * memory-heavy straight-line region consequently spent hundreds of bytes per
 * access on identical code which almost never ran.
 *
 * Keep only the site-specific work inline: preserve the guest EA, restore
 * translator scratch registers, and point x17 at immutable metadata adjacent
 * to the site.  All miss sites in the translated block branch to one shared
 * spill/exit stub.  x16/x17 are engine-owned in this ABI and emit_spill()
 * deliberately preserves them, exactly as for the shared BUS stub.
 */
static void emit_a64_soft_exit_site(const struct a64_soft_guard *guard) {
    assert(g_steal1617);
    e_str(guard->ea, CPUREG, OFF_SOFT_EA);
    for (unsigned index = 0; index < guard->nrestore; ++index)
        e_ldr(guard->restore_reg[index], CPUREG, guard->restore_offset[index]);
    uint32_t *metadata_address = (uint32_t *)g_cp;
    emit32(0); /* adr x17,immutable_site_metadata */
    if (g_soft_legacy_stub_patch_count >= SOFT_STUB_PATCH_MAX) {
        static const char message[] = "too many soft-memory guards in one translated block";
        (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
        _exit(70);
    }
    g_soft_legacy_stub_patches[g_soft_legacy_stub_patch_count++] = (uint32_t *)g_cp;
    emit32(0); /* b shared_soft_stub */
    uint8_t *metadata = g_cp;
    patch_adr(metadata_address, metadata, 17);
    memcpy(g_cp, &guard->bytes, sizeof(guard->bytes));
    g_cp += sizeof(guard->bytes);
    uint64_t required = guard->required;
    memcpy(g_cp, &required, sizeof(required));
    g_cp += sizeof(required);
    memcpy(g_cp, &guard->pc, sizeof(guard->pc));
    g_cp += sizeof(guard->pc);
}

static void emit_a64_soft_guard_end(struct a64_soft_guard *guard) {
    if (!guard->active) return;
    if (guard->shared) {
        uint32_t *skip = (uint32_t *)g_cp;
        emit32(0); /* b resume */
        uint8_t *miss = g_cp;
        for (unsigned index = 0; index < guard->nrestore; ++index)
            e_ldr(guard->restore_reg[index], CPUREG, guard->restore_offset[index]);
        if (g_soft_stub_patch_count >= SOFT_STUB_PATCH_MAX) {
            static const char message[] = "too many soft-memory restore stubs in one translated block";
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            _exit(70);
        }
        g_soft_stub_patches[g_soft_stub_patch_count++] = (uint32_t *)g_cp;
        emit32(0); /* b shared_soft_exit */
        uint8_t *resume = g_cp;
        *skip = 0x14000000u | ((uint32_t)((resume - (uint8_t *)skip) / 4) & 0x03ffffffu);
        int64_t miss_delta = miss - guard->native;
        if (miss_delta < INT32_MIN || miss_delta > INT32_MAX) {
            static const char message[] = "soft-memory site miss displacement out of range";
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            _exit(70);
        }
        int32_t narrow_delta = (int32_t)miss_delta;
        memcpy(guard->metadata + 8, &narrow_delta, sizeof narrow_delta);
        for (unsigned i = 0; i < guard->ndirect; ++i)
            *guard->direct[i] = a64_tbz_x(guard->tmp2, 0, (guard->native - (uint8_t *)guard->direct[i]) / 4);
        return;
    }
    uint32_t *skip = (uint32_t *)g_cp;
    emit32(0); /* b resume */
    uint8_t *miss = g_cp;
    emit_a64_soft_exit_site(guard);
    uint8_t *resume = g_cp;
    *skip = 0x14000000u | ((uint32_t)((resume - (uint8_t *)skip) / 4) & 0x03ffffffu);
    for (unsigned i = 0; i < guard->nmiss; ++i) {
        if (guard->miss_bit[i] < 0)
            *guard->miss[i] = a64_cbnz_x(guard->miss_reg[i], (miss - (uint8_t *)guard->miss[i]) / 4);
        else
            *guard->miss[i] =
                a64_tbz_x(guard->miss_reg[i], (unsigned)guard->miss_bit[i], (miss - (uint8_t *)guard->miss[i]) / 4);
    }
    uint8_t *direct = guard->profile_sample ? g_cp : guard->native;
    for (unsigned i = 0; i < guard->ndirect; ++i)
        *guard->direct[i] = a64_tbz_x(guard->tmp2, 0, (direct - (uint8_t *)guard->direct[i]) / 4);
    if (guard->profile_sample) {
        emit_prof_bump(&g_prof_soft_hull_sampled);
        uint32_t *back = (uint32_t *)g_cp;
        int64_t words = (guard->native - (uint8_t *)back) / 4;
        emit32(0x14000000u | ((uint32_t)words & 0x03ffffffu));
    }
}

static void aarch64_soft_filter_refresh(struct cpu *c) {
    uint64_t first = UINT64_MAX, last = 0;
    hl_logical_vma_snapshot *snapshot =
        atomic_load_explicit(hl_logical_vma_global_snapshot_source(), memory_order_acquire);
    if (snapshot != NULL && snapshot->count != 0) {
        first = snapshot->views[0].guest_first;
        last = snapshot->views[snapshot->count - 1].guest_last;
    }
    c->soft_filter_first = first;
    c->soft_filter_last = last;
}

static void emit_a64_soft_stub(void) {
    if (!g_soft_stub_patch_count && !g_soft_resolver_patch_count && !g_soft_legacy_stub_patch_count) return;
    if (g_soft_resolver_patch_count) {
        uint32_t *cold_miss_patches[1024];
        int cold_miss_bits[1024]; /* -1 = CBNZ x18, otherwise TBZ x18,bit */
        unsigned cold_miss_count = 0;
        for (;;) {
            uint32_t first = 0;
            while (first < g_soft_resolver_patch_count && g_soft_resolver_patches[first] == NULL)
                ++first;
            if (first == g_soft_resolver_patch_count) break;
            uint32_t bytes = g_soft_resolver_bytes[first];
            uint32_t required = g_soft_resolver_required[first];
            uint8_t *resolver = g_cp;
            for (uint32_t i = first; i < g_soft_resolver_patch_count; ++i) {
                if (g_soft_resolver_patches[i] == NULL || g_soft_resolver_bytes[i] != bytes ||
                    g_soft_resolver_required[i] != required)
                    continue;
                int64_t displacement = (resolver - (uint8_t *)g_soft_resolver_patches[i]) / 4;
                if (displacement < -(INT64_C(1) << 25) || displacement >= (INT64_C(1) << 25)) {
                    static const char message[] = "soft-memory resolver branch out of range";
                    (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
                    _exit(70);
                }
                *g_soft_resolver_patches[i] = 0x14000000u | ((uint32_t)displacement & 0x03ffffffu);
                g_soft_resolver_patches[i] = NULL;
            }

            /* x16 = guest EA, x17 = immutable site metadata. Only x18 is
               scratch; x30 remains untouched for precise host-link state. */
            e_ldr(18, CPUREG, OFF_SOFT_PAGE);
            emit32(0xCB000000u | (18u << 16) | (16u << 5) | 18u);
            emit32(0xD37FFC00u | (18u << 5) | 18u);
            assert(cold_miss_count < sizeof cold_miss_patches / sizeof cold_miss_patches[0]);
            cold_miss_patches[cold_miss_count] = (uint32_t *)g_cp;
            cold_miss_bits[cold_miss_count++] = -1;
            emit32(0);

            e_ldr(18, CPUREG, OFF_SOFT_LIMIT);
            if (bytes == 4096)
                emit32(0xD1400000u | (1u << 10) | (18u << 5) | 18u);
            else
                e_subi(18, 18, bytes);
            emit32(0xCB000000u | (16u << 16) | (18u << 5) | 18u);
            emit32(0xD37FFC00u | (18u << 5) | 18u);
            assert(cold_miss_count < sizeof cold_miss_patches / sizeof cold_miss_patches[0]);
            cold_miss_patches[cold_miss_count] = (uint32_t *)g_cp;
            cold_miss_bits[cold_miss_count++] = -1;
            emit32(0);

            e_ldr(18, CPUREG, OFF_SOFT_PROTECTION);
            if (required & HL_LOGICAL_VMA_READ) {
                assert(cold_miss_count < sizeof cold_miss_patches / sizeof cold_miss_patches[0]);
                cold_miss_patches[cold_miss_count] = (uint32_t *)g_cp;
                cold_miss_bits[cold_miss_count++] = 0;
                emit32(0); /* tbz x18,#READ,miss */
            }
            if (required & HL_LOGICAL_VMA_WRITE) {
                assert(cold_miss_count < sizeof cold_miss_patches / sizeof cold_miss_patches[0]);
                cold_miss_patches[cold_miss_count] = (uint32_t *)g_cp;
                cold_miss_bits[cold_miss_count++] = 1;
                emit32(0); /* tbz x18,#WRITE,miss */
            }
            e_ldr(18, CPUREG, OFF_SOFT_DELTA);
            emit32(0x8B000000u | (18u << 16) | (16u << 5) | 16u);
            e_addi(17, 17, 16);
            e_br(17);
        }
        uint8_t *resolver_miss = g_cp;
        for (unsigned i = 0; i < cold_miss_count; ++i) {
            uint32_t *patch = cold_miss_patches[i];
            int64_t displacement = (resolver_miss - (uint8_t *)patch) / 4;
            *patch = cold_miss_bits[i] < 0 ? a64_cbnz_x(18, displacement)
                                           : a64_tbz_x(18, (unsigned)cold_miss_bits[i], displacement);
        }
        e_str(16, CPUREG, OFF_SOFT_EA);
        emit32(0x79400000u | (6u << 10) | (17u << 5) | 18u); /* ldrh w18,[meta,#12] */
        e_str(18, CPUREG, OFF_SOFT_BYTES);
        emit32(0x39400000u | (14u << 10) | (17u << 5) | 18u); /* ldrb w18,[meta,#14] */
        e_str(18, CPUREG, OFF_SOFT_REQUIRED);
        e_ldr(18, 17, 0);
        e_str(18, CPUREG, OFF_SOFT_PC);
        e_str(18, CPUREG, OFF_PC);
        emit32(0xB9800000u | (2u << 10) | (17u << 5) | 18u); /* ldrsw x18,[meta,#8] */
        e_addi(17, 17, 16);
        emit32(0x8B000000u | (18u << 16) | (17u << 5) | 18u);
        e_br(18);
    }

    if (g_soft_stub_patch_count) {
        uint8_t *stub = g_cp;
        for (uint32_t i = 0; i < g_soft_stub_patch_count; ++i) {
            int64_t displacement = (stub - (uint8_t *)g_soft_stub_patches[i]) / 4;
            if (displacement < -(INT64_C(1) << 25) || displacement >= (INT64_C(1) << 25)) {
                static const char message[] = "soft-memory stub branch out of range";
                (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
                _exit(70);
            }
            *g_soft_stub_patches[i] = 0x14000000u | ((uint32_t)displacement & 0x03ffffffu);
        }
        emit_spill();
        e_movconst(9, R_SOFTMISS);
        e_str(9, 0, OFF_RSN);
        emit_blockret(9);
        e_br(9);
    }
    if (g_soft_legacy_stub_patch_count) {
        uint8_t *stub = g_cp;
        for (uint32_t i = 0; i < g_soft_legacy_stub_patch_count; ++i) {
            int64_t displacement = (stub - (uint8_t *)g_soft_legacy_stub_patches[i]) / 4;
            if (displacement < -(INT64_C(1) << 25) || displacement >= (INT64_C(1) << 25)) {
                static const char message[] = "legacy soft-memory stub branch out of range";
                (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
                _exit(70);
            }
            *g_soft_legacy_stub_patches[i] = 0x14000000u | ((uint32_t)displacement & 0x03ffffffu);
        }
        emit_spill();
        e_ldr(9, 17, 0);
        e_str(9, 0, OFF_SOFT_BYTES);
        e_ldr(9, 17, 8);
        e_str(9, 0, OFF_SOFT_REQUIRED);
        e_ldr(9, 17, 16);
        e_str(9, 0, OFF_SOFT_PC);
        e_str(9, 0, OFF_PC);
        e_movconst(9, R_SOFTMISS);
        e_str(9, 0, OFF_RSN);
        emit_blockret(9);
        e_br(9);
    }
}

/* A discontinuous-view retry executes against cpu->soft_bounce.  Force one
   cold dispatcher crossing after the architectural operation so stores can
   be scattered before the following guest instruction observes them. */
static void emit_a64_soft_bounce_commit(uint64_t next_pc) {
    if (!jit_guest_soft_active()) return;
    e_ldr(16, CPUREG, OFF_SOFT_BOUNCE_PENDING);
    uint32_t *clear = (uint32_t *)g_cp;
    emit32(0); /* cbz x16,resume */
    emit_exit_const(next_pc, R_SOFTCOMMIT);
    uint8_t *resume = g_cp;
    *clear = 0xB4000000u | (((uint32_t)((resume - (uint8_t *)clear) / 4) & 0x7ffffu) << 5) | 16u;
}

static void emit_a64_soft_exclusive(uint32_t in) {
    int base = (int)((in >> 5) & 31u);
    if (base == 31)
        e_mov_from_sp(16);
    else if (is_stolen(base))
        e_ldr(16, CPUREG, base * 8);
    else
        e_movr(16, base);
    emit_a64_bus_guard(16, a64_mem_bytes(in), g_emit_gpc);

    int mask = gpr_field_mask(in);
    unsigned used = 0;
    static const int shifts[4] = {0, 5, 16, 10}, mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; ++k)
        if (mask & mbits[k]) used |= 1u << ((in >> shifts[k]) & 31u);
    if (is_casp(in)) {
        used |= 1u << ((((in >> 16) & 31u) + 1u) & 31u);
        used |= 1u << (((in & 31u) + 1u) & 31u);
    }
    int ea = 0;
    while ((used & (1u << ea)) || is_stolen(ea))
        ++ea;
    e_str(ea, CPUREG, (int)OFF_MSCRATCH + 32);
    e_movr(ea, 16);
    struct a64_soft_guard soft =
        emit_a64_soft_guard_begin(ea, 17, 18, a64_mem_bytes(in), a64_mem_required(in), g_emit_gpc);
    a64_soft_guard_restore(&soft, ea, (int)OFF_MSCRATCH + 32);
    if (is_casp(in)) {
        emit_casp_mangled(in, ea);
    } else {
        uint32_t rebased = (in & ~(31u << 5)) | ((uint32_t)ea << 5);
        mask &= ~2;
        if (uses_x18(in, mask))
            emit_mangled_x18(rebased, mask);
        else
            emit32(rebased);
    }
    emit_a64_soft_guard_end(&soft);
    e_ldr(ea, CPUREG, (int)OFF_MSCRATCH + 32);
}

/*
 * Every BUS-active memory site retains its compact force/page-filter fast
 * path, but all sites in a translated block share one cold spill/query/reload
 * stub. Large runtimes can keep BUS tracking active for their entire lifetime;
 * duplicating that cold path at every memory operation needlessly exhausts the
 * code cache even though almost every filter probe misses.
 */
#define BUS_STUB_PATCH_MAX 65536
static uint32_t *g_bus_stub_patches[BUS_STUB_PATCH_MAX];
static uint32_t g_bus_stub_patch_count;

static void patch_adr(uint32_t *instruction, uint8_t *target, unsigned reg) {
    int64_t displacement = target - (uint8_t *)instruction;
    if (displacement < -(INT64_C(1) << 20) || displacement >= (INT64_C(1) << 20)) {
        static const char message[] = "BUS metadata address out of range";
        (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
        _exit(70);
    }
    uint64_t immediate = (uint64_t)displacement & UINT64_C(0x1fffff);
    *instruction =
        0x10000000u | (uint32_t)((immediate & 3u) << 29) | (uint32_t)(((immediate >> 2) & 0x7ffffu) << 5) | reg;
}

static void emit_a64_bus_guard_saved(uint64_t bytes, uint64_t pc) {
    /* x16 carries the state loaded by the caller. */
    uint32_t *force_slow = (uint32_t *)g_cp;
    emit32(0); /* tbnz w16,#1,slow */
    /* Use only engine-reserved x16/x17. A live guest register cannot be
       parked in shared per-thread scratch here: an asynchronous signal may
       re-enter translated code and run another guard before this one resumes. */
    e_ldr(17, CPUREG, OFF_BUS_EA);
    emit32(0xD34CFC00u | (17u << 5) | 17u);                            /* lsr x17,x17,#12: page */
    emit32(0xD3400000u | (6u << 16) | (15u << 10) | (17u << 5) | 16u); /* ubfx x16,x17,#6,#10 */
    e_ldr(17, CPUREG, OFF_BUS_FILTER);
    emit32(0x8B000000u | (16u << 16) | (3u << 10) | (17u << 5) | 16u); /* add x16,x17,x16,lsl#3 */
    e_ldr(16, 16, 0);
    e_ldr(17, CPUREG, OFF_BUS_EA);
    emit32(0xD34CFC00u | (17u << 5) | 17u);
    emit32(0x9AD12610u); /* lsrv x16,x16,x17 */
    uint32_t *filter_miss = (uint32_t *)g_cp;
    emit32(0); /* tbz x18,#0,resume */
    uint8_t *slow = g_cp;
    *force_slow = 0x37000000u | (1u << 19) | (((uint32_t)((slow - (uint8_t *)force_slow) / 4) & 0x3FFFu) << 5) | 16u;
    /*
     * Carry only engine-reserved registers into the shared stub. emit_spill()
     * deliberately preserves x16/x17, so an asynchronous signal cannot
     * overwrite site metadata in per-thread mutable scratch.
     */
    e_ldr(16, CPUREG, OFF_BUS_EA);
    uint32_t *metadata_address = (uint32_t *)g_cp;
    emit32(0); /* adr x17,immutable_site_metadata */
    if (g_bus_stub_patch_count >= BUS_STUB_PATCH_MAX) {
        static const char message[] = "too many BUS guards in one translated block";
        (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
        _exit(70);
    }
    g_bus_stub_patches[g_bus_stub_patch_count++] = (uint32_t *)g_cp;
    emit32(0); /* b shared_bus_stub */
    uint8_t *metadata = g_cp;
    patch_adr(metadata_address, metadata, 17);
    memcpy(g_cp, &bytes, sizeof(bytes));
    g_cp += sizeof(bytes);
    memcpy(g_cp, &pc, sizeof(pc));
    g_cp += sizeof(pc);
    uint8_t *resume_slot = g_cp;
    g_cp += sizeof(uint64_t);
    uint8_t *resume_fast = g_cp;
    uint64_t resume_rx = (uint64_t)J_RX(resume_fast);
    memcpy(resume_slot, &resume_rx, sizeof(resume_rx));
    *filter_miss = 0x36000000u | (((uint32_t)((resume_fast - (uint8_t *)filter_miss) / 4) & 0x3FFFu) << 5) | 16u;
}

static void emit_a64_bus_stub(void) {
    if (!g_bus_stub_patch_count) return;
    uint8_t *stub = g_cp;
    for (uint32_t i = 0; i < g_bus_stub_patch_count; i++) {
        int64_t displacement = (stub - (uint8_t *)g_bus_stub_patches[i]) / 4;
        if (displacement < -(INT64_C(1) << 25) || displacement >= (INT64_C(1) << 25)) {
            static const char message[] = "BUS stub branch out of range";
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            _exit(70);
        }
        *g_bus_stub_patches[i] = 0x14000000u | ((uint32_t)displacement & 0x03ffffffu);
    }
    emit_spill();
    e_movr(19, 17); /* callee-saved immutable metadata pointer across query */
    e_movr(0, 16);
    e_ldr(1, 19, 0);
    emit_busfaultptr(16);
    emit32(0xD63F0000u | (16u << 5)); /* blr x16 */
    uint32_t *clear = (uint32_t *)g_cp;
    emit32(0); /* cbz x0,clear */
    e_str(0, CPUREG, OFF_FAULT_ADDR);
    e_ldr(9, 19, 8);
    e_str(9, CPUREG, OFF_PC);
    e_movconst(9, R_BUS);
    e_str(9, CPUREG, OFF_RSN);
    e_movr(0, CPUREG);
    emit_blockret(9);
    e_br(9);
    uint8_t *resume = g_cp;
    *clear = 0xB4000000u | (((uint32_t)((resume - (uint8_t *)clear) / 4) & 0x7ffffu) << 5);
    e_ldr(16, 19, 16);
    e_ldr(9, CPUREG, OFF_SP);
    e_mov_sp_from(9);
    e_ldr(9, CPUREG, OFF_NZCV);
    emit32(0xD51B4200u | 9);
    for (int t = 0; t < 32; t += 2)
        e_ldp_q(t, t + 1, CPUREG, OFF_V + t * 16);
    for (int r = 1; r <= 30; r++)
        if (!is_stolen(r)) e_ldr(r, CPUREG, r * 8);
    e_ldr(0, CPUREG, 0);
    e_br(16);
}

static void emit_a64_bus_guard(int ea, uint64_t bytes, uint64_t pc) {
    if (!jit_guest_bus_active()) return;
    /* The inline BUS ABI reserves x16/x17 as engine registers. Target
       initialization fixes g_steal1617 on and exposes no legacy override. */
    assert(g_steal1617);
    e_str(ea, CPUREG, OFF_BUS_EA);
    e_ldr(16, CPUREG, OFF_BUS_FORCE);
    emit32(0xB9400000u | (16u << 5) | 16u);
    uint32_t *inactive_fast = (uint32_t *)g_cp;
    emit32(0);
    emit_a64_bus_guard_saved(bytes, pc);
    uint8_t *resume_inactive = g_cp;
    e_ldr(ea, CPUREG, OFF_BUS_EA);
    *inactive_fast =
        0x36000000u | (((uint32_t)((resume_inactive - (uint8_t *)inactive_fast) / 4) & 0x3FFFu) << 5) | 16u;
}

static void emit_a64_bus_guard_base(int base, int64_t offset, uint64_t bytes, uint64_t pc) {
    if (!jit_guest_bus_active()) return;
    if (base == 31)
        e_mov_from_sp(16);
    else if (is_stolen(base))
        e_ldr(16, CPUREG, base * 8);
    else
        e_movr(16, base);
    if (offset < 0)
        e_subi(16, 16, (unsigned)(-offset));
    else if (offset > 0)
        e_addi(16, 16, (unsigned)offset);
    emit_a64_bus_guard(16, bytes, pc);
}

/* Compute and guard the architectural guest EA while preserving the original
   memory opcode. BUS observation must not broaden non-PIE bias folding. */
static void emit_a64_bus_guard_instruction(uint32_t in, uint64_t pc) {
    int base = (int)((in >> 5) & 31u);
    int regoff = (in & 0x3B200C00u) == 0x38200800u;
    if (base == 31)
        e_mov_from_sp(16);
    else if (is_stolen(base))
        e_ldr(16, CPUREG, base * 8);
    else
        e_movr(16, base);
    if (regoff) {
        int rm = (int)((in >> 16) & 31u), opt = (int)((in >> 13) & 7u);
        int vector = (int)((in >> 26) & 1u);
        int size = vector ? (int)((((in >> 22) & 3u) >> 1) << 2) | (int)((in >> 30) & 3u) : (int)((in >> 30) & 3u);
        int amount = ((in >> 12) & 1u) ? size : 0;
        if (is_stolen(rm))
            e_ldr(17, CPUREG, rm * 8);
        else
            e_movr(17, rm);
        emit32(0x8B200000u | (17u << 16) | ((unsigned)opt << 13) | ((unsigned)(amount & 7) << 10) | (16u << 5) | 16u);
    } else {
        int64_t offset = a64_fold_mem_offset(in, 0);
        if (((in >> 27) & 7u) == 7u && !((in >> 24) & 1u)) {
            int mode = (int)((in >> 10) & 3u);
            if (mode == 1) offset = 0;
        }
        if (offset != 0) {
            uint64_t magnitude = (uint64_t)(offset < 0 ? -offset : offset);
            e_movconst(17, magnitude);
            emit32((offset < 0 ? 0xCB000000u : 0x8B000000u) | (17u << 16) | (16u << 5) | 16u);
        }
    }
    emit_a64_bus_guard(16, a64_mem_bytes(in), pc);
}

// Scratch-slot assignment for a folded memory op. Picks the non-stolen host GP registers whose live guest
// values emit_fold_mem spills to cpu->mscratch[4..7] (Sb,T,T2,Tm). Factored out so fault-time register
// reconstruction (sigframe_capture_fault) uses the EXACT same slot mapping the emitter chose. Fills
// slots[0..n-1] with the chosen register numbers (Sb=slots[0], T=slots[1], T2=slots[2], Tm=slots[3] for the
// register-offset form) and returns the count: 4 for register-offset, else 3. Mirrors gpr_field_mask + the
// LSE-Rs (bit2) fixup so the "used" set matches the emitter exactly.
static int fold_mem_scratch(uint32_t insn, int slots[4]) {
    int mask = gpr_field_mask(insn);
    if ((insn & 0x3B200C00u) == 0x38200000u) mask |= 4; // LSE atomic value operand Rs[20:16]
    int regoff = (insn & 0x3B200C00u) == 0x38200800u;
    int used = 0;
    static const int shifts[4] = {0, 5, 16, 10}, mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if (mask & mbits[k]) used |= 1u << ((insn >> shifts[k]) & 31);
    int need = regoff ? 4 : 3, n = 0;
    for (int r = 0; r <= 30 && n < need; r++)
        if (!(used & (1u << r)) && !is_stolen(r)) slots[n++] = r;
    return n;
}

// Emit a folded memory op: compute the guest effective address into a scratch Sb, add g_nonpie_bias iff
// that address is a LOW image address (< 4GiB; everything else -- stack/heap/mmap/libs -- is >= the
// engine's 4GiB __PAGEZERO), then the access re-pointed at Sb. Flag-free (loads/stores must not disturb the
// guest NZCV): only mov/ldr/add/lsr/cbnz. Scratch originals are spilled to cpu->mscratch (NOT the stack:
// the fold runs on every memory op, where an async host signal would clobber a red-zone slot). For the
// register-offset form the full EA (Xn + extend(Xm)) is materialized and the access is de-indexed to a
// plain [Sb] (unscaled, #0) so the single < 4GiB test is on the real target. Pre/post-index writeback is
// de-indexed too: the access runs against the biased Sb, then the writeback updates the LOW guest base. Any
// stolen Rt/Rt2/Rs is handled by reusing emit_mangled_x18 on the re-based instruction (base field -> Sb).
