// translator/guest/aarch64 -- the aarch64-Linux -> arm64-host transliterator. Same-ISA: copy
// most instructions verbatim; MANGLE only stolen-register (x18/x28/x30) users. Optimizations: LSE
// atomic upgrade, §B shadow-return prediction (depth-gated), tier-2 purity gate. See OPTIMIZATIONS.md.

#include <assert.h>
#include "../../guest_fetch.h"
#include "../../../linux_abi/logical_vma.h"

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
// host_cpu.h rather than trusting the unity TU: an undefined macro drops the probe on a REAL host.
#include "../../../host/host_cpu.h"
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
    assert(ea != tmp && ea != tmp2 && tmp != tmp2);
    assert(bytes != 0 && bytes <= 4096);
    /*
     * With the shadow-RAS disabled x30 carries no live engine return link.
     * Use it as the resolver's per-site continuation, normalize every EA in
     * x16, and share the complete interval/permission check once per block.
     * Shadow-enabled builds retain the proven inline guard below.
     */
    guard.shared = shadowgate() < 0 && !g_tier2_build;
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
    for (unsigned i = 0; i < guard->ndirect; ++i)
        *guard->direct[i] = a64_tbz_x(guard->tmp2, 0, (guard->native - (uint8_t *)guard->direct[i]) / 4);
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
static void emit_fold_mem(uint32_t in, int emit_bus_guard) {
    int mask = gpr_field_mask(in), base = (in >> 5) & 31;
    // LSE atomic memory ops (LDADD/SWP/...) carry an operand register Rs at [20:16] that gpr_field_mask
    // does not flag; mark it (bit2) so the scratch picker never aliases Rs and a stolen Rs is mangled.
    if ((in & 0x3B200C00u) == 0x38200000u) mask |= 4;
    int regoff = (in & 0x3B200C00u) == 0x38200800u;
    int wb = 0; // single-register writeback: 1 = pre-index, 2 = post-index
    int64_t wbimm = 0;
    if (((in >> 27) & 7) == 7 && !((in >> 24) & 1)) {
        int o = (in >> 10) & 3;
        wb = (o == 3) ? 1 : (o == 1) ? 2 : 0;
        if (wb) wbimm = sext((uint64_t)((in >> 12) & 0x1FF), 9);
    } else if ((in & 0x3A000000u) == 0x28000000u) {
        int o = (in >> 23) & 3;
        wb = o == 3 ? 1 : o == 1 ? 2 : 0;
        if (wb) wbimm = sext((uint64_t)((in >> 15) & 0x7f), 7) * (int64_t)(a64_mem_bytes(in) / 2);
    }
    int sc[4];
    fold_mem_scratch(in, sc); // shared with fault-time reconstruction: same slot mapping
    int Sb = sc[0], T = sc[1], T2 = sc[2], Tm = regoff ? sc[3] : -1;
    // Spill scratch originals to cpu->mscratch[4..7], NOT the stack: the fold runs on EVERY guest memory op,
    // and an async host signal (e.g. Go's SIGURG preemption) would clobber a [sp,#-N] red-zone slot. This
    // mirrors emit_mangled_x18 (it uses mscratch[0..3]); the slots are disjoint so the nested call is safe.
    int M = (int)OFF_MSCRATCH;
    e_str(Sb, CPUREG, M + 32);
    e_str(T, CPUREG, M + 40);
    e_str(T2, CPUREG, M + 48);
    if (regoff) e_str(Tm, CPUREG, M + 56);
    if (base == 31)
        e_mov_from_sp(Sb);
    else if (is_stolen(base))
        e_ldr(Sb, CPUREG, base * 8); // guest base from cpu->x[base]
    else
        e_movr(Sb, base); // guest base from the live host reg
    if (regoff) {
        int rm = (in >> 16) & 31, opt = (in >> 13) & 7, S = (in >> 12) & 1, v = (in >> 26) & 1;
        int sz = v ? ((((in >> 22) & 3) >> 1) << 2) | ((in >> 30) & 3) : (in >> 30) & 3;
        int amt = S ? sz : 0, mreg = rm;
        if (is_stolen(rm)) {
            e_ldr(Tm, CPUREG, rm * 8); // index from cpu->x[rm]
            mreg = Tm;
        }
        // Sb = Xn + extend(Xm)  (extended-register add; option/amount mirror the load's index extend)
        emit32(0x8B200000u | (mreg << 16) | (opt << 13) | ((unsigned)(amt & 7) << 10) | (Sb << 5) | Sb);
    }
    int64_t access_off = regoff ? 0 : a64_fold_mem_offset(in, wb);
    if (access_off) {
        e_movconst(T, (uint64_t)(access_off < 0 ? -access_off : access_off));
        emit32((access_off < 0 ? 0xCB000000u : 0x8B000000u) | ((unsigned)T << 16) | ((unsigned)Sb << 5) |
               (unsigned)Sb); /* sub/add Sb,Sb,T */
    }
    if (guestbase_on()) {
        // Bias iff the EA falls in THIS image's span [g_nonpie_lo, g_nonpie_hi). Fast path: a >= 4GiB address is
        // never the low non-PIE image (stack/heap/mmap/ld.so/libc all live above the 4GiB __PAGEZERO) -> skip
        // with no flag traffic (the common case). For a < 4GiB EA, do the exact two-sided range test.
        emit32(0xD360FC00u | (Sb << 5) | T); // lsr T, Sb, #32
        uint32_t *p_hi = (uint32_t *)g_cp;
        emit32(0);
        emit32(0xD53B4200u | T2); // mrs T2,nzcv
        e_movconst(T, g_nonpie_lo);
        emit32(0xEB000000u | (T << 16) | (Sb << 5) | 31);
        uint32_t *p_lo1 = (uint32_t *)g_cp;
        emit32(0);
        e_movconst(T, g_nonpie_hi);
        emit32(0xEB000000u | (T << 16) | (Sb << 5) | 31);
        uint32_t *p_lo2 = (uint32_t *)g_cp;
        emit32(0);
        e_movconst(T, g_nonpie_bias);
        emit32(0x8B000000u | (T << 16) | (Sb << 5) | Sb);
        uint8_t *Llo = g_cp;
        emit32(0xD51B4200u | T2);
        uint8_t *Lhi = g_cp;
        *p_hi = 0xB5000000u | (((uint32_t)(((uint8_t *)Lhi - (uint8_t *)p_hi) / 4) & 0x7FFFF) << 5) | T;
        *p_lo1 = 0x54000000u | (((uint32_t)(((uint8_t *)Llo - (uint8_t *)p_lo1) / 4) & 0x7FFFF) << 5) | 3;
        *p_lo2 = 0x54000000u | (((uint32_t)(((uint8_t *)Llo - (uint8_t *)p_lo2) / 4) & 0x7FFFF) << 5) | 2;
    }
    uint32_t m;
    int emask = mask;
    if (regoff) {
        // de-index: register-offset -> unscaled [Sb,#0]; keep size/V/opc/Rt, clear bits[21:10] + base->Sb
        m = (in & ~0x003FFC00u & ~(0x1Fu << 5)) | ((unsigned)Sb << 5);
        emask &= ~4; // Rm now folded into Sb -> drop it from the mangle set
    } else if (wb) {
        // The architectural access address is already in Sb.  Convert the
        // pre/post-index opcode to an unscaled zero-offset access; writeback is
        // applied separately to the original guest base below.
        if ((in & 0x3A000000u) == 0x28000000u) {
            m = (in & ~(3u << 23) & ~(0x7fu << 15)) | (2u << 23);
        } else {
            m = in & ~(0x3u << 10) & ~(0x1FFu << 12);
        }
        m = (m & ~(0x1Fu << 5)) | ((unsigned)Sb << 5);
    } else {
        m = (in & ~(0x1Fu << 5)) | ((unsigned)Sb << 5);
        if ((in & 0x3b000000u) == 0x39000000u)
            m &= ~(0xfffu << 10); /* unsigned immediate */
        else if ((in & 0x3b200000u) == 0x38000000u)
            m &= ~(0x1ffu << 12); /* unscaled immediate */
        else if ((in & 0x3a000000u) == 0x28000000u)
            m &= ~(0x7fu << 15); /* pair immediate */
    }
    if (emit_bus_guard && jit_guest_bus_active()) {
        /* Preserve the exact EA, then restore scratch registers before
           emit_spill captures the architectural register file.  Spilling Sb,
           T, T2, or Tm while they contain translator temporaries silently
           corrupts guest registers on every guarded miss. */
        e_str(Sb, CPUREG, OFF_BUS_EA);
        e_ldr(16, CPUREG, OFF_BUS_FORCE);
        emit32(0xB9400000u | (16u << 5) | 16u);
        uint32_t *inactive_fast = (uint32_t *)g_cp;
        emit32(0);
        if (regoff) e_ldr(Tm, CPUREG, M + 56);
        e_ldr(Sb, CPUREG, M + 32);
        e_ldr(T, CPUREG, M + 40);
        e_ldr(T2, CPUREG, M + 48);
        emit_a64_bus_guard_saved(a64_mem_bytes(in), g_emit_gpc);
        e_ldr(Sb, CPUREG, OFF_BUS_EA);
        uint8_t *resume_inactive = g_cp;
        *inactive_fast =
            0x36000000u | (((uint32_t)((resume_inactive - (uint8_t *)inactive_fast) / 4) & 0x3FFFu) << 5) | 16u;
    }
    struct a64_soft_guard soft =
        emit_a64_soft_guard_begin(Sb, T, T2, a64_mem_bytes(in), a64_mem_required(in), g_emit_gpc);
    a64_soft_guard_restore(&soft, Sb, M + 32);
    a64_soft_guard_restore(&soft, T, M + 40);
    a64_soft_guard_restore(&soft, T2, M + 48);
    if (regoff) a64_soft_guard_restore(&soft, Tm, M + 56);
    if (uses_x18(m, emask))
        emit_mangled_x18(m, emask); // stolen Rt/Rt2/Rs (base now names non-stolen Sb)
    else
        emit32(m);
    emit_a64_soft_guard_end(&soft);
    if (wb) { // writeback updates the LOW guest base (Rt != base for loads -> safe to do after the access)
        unsigned a = (unsigned)(wbimm < 0 ? -wbimm : wbimm);
        if (is_stolen(base)) {
            e_ldr(T, CPUREG, base * 8);
            if (wbimm < 0)
                e_subi(T, T, a);
            else
                e_addi(T, T, a);
            e_str(T, CPUREG, base * 8);
        } else if (wbimm < 0)
            e_subi(base, base, a);
        else
            e_addi(base, base, a);
    }
    if (regoff) e_ldr(Tm, CPUREG, M + 56); // restore scratch originals
    e_ldr(Sb, CPUREG, M + 32);
    e_ldr(T, CPUREG, M + 40);
    e_ldr(T2, CPUREG, M + 48);
    emit_a64_soft_bounce_commit(g_emit_gpc + 4);
}

// ---- AdvSIMD load/store STRUCTURE bias-fold (ld1/st1 .. ld4/st4, single & multiple, ld1r/ld2r/...) ----
// is_foldable_mem deliberately omits these (their effective address is a bare base Xn with no offset or
// index), so without this they fall to the nonpie_fixup safety net -- a SIGSEGV per access. glibc's NEON
// strlen/memcpy stream the image's LOW absolute pointers through `ld1 {v0.16b},[x1]`, which then traps once
// per 16 bytes (gcc -shared spins). Fold them exactly like emit_fold_mem: materialize the base in a scratch,
// add g_nonpie_bias iff it lands in the image span [lo,hi), and run the access against the biased scratch.
// The EA of a structure op IS the base (no immediate, no index), so the < image-span test on Xn is exact.
// Identifier: bit31=0, bits[29:25]=00110; bit24 = single(1)/multiple(0), bit23 = post-index writeback. Rt is
// a V register (no GP mangle needed) -- only the base Xn[9:5] and a register post-index Rm[20:16] are GP.
static int is_advsimd_struct(uint32_t in) {
    return (in & 0xBE000000u) == 0x0C000000u;
}

// Bytes transferred by an AdvSIMD structure op -- the implicit increment of an immediate post-index (Rm==31).
static int advsimd_struct_bytes(uint32_t in) {
    int q = (in >> 30) & 1;
    if (!((in >> 24) & 1)) { // load/store MULTIPLE structures: register count from opcode[15:12]
        int regs;
        switch ((in >> 12) & 0xF) {
        case 0x0:
        case 0x2: regs = 4; break; // LD4/ST4, LD1 x4
        case 0x4:
        case 0x6: regs = 3; break; // LD3/ST3, LD1 x3
        case 0x8:
        case 0xA: regs = 2; break; // LD2/ST2, LD1 x2
        case 0x7: regs = 1; break; // LD1 x1
        default: regs = 0; break;  // unallocated (never reached: the access would have faulted)
        }
        return regs * (q ? 16 : 8);
    }
    // load/store SINGLE structure: selem consecutive elements, each (1<<scale) bytes.
    int opcode = (in >> 13) & 7, R = (in >> 21) & 1, size = (in >> 10) & 3;
    int scale = (opcode >> 1) & 3, selem = (((opcode & 1) << 1) | R) + 1;
    if (scale == 3) scale = size; // LD#R replicate: element width is `size`
    return selem * (1 << scale);
}

// Fold an AdvSIMD load/store structure op (see is_advsimd_struct). Mirrors emit_fold_mem's range-gated bias
// (flag-safe: NZCV saved across the compares) but is simpler -- the EA is just the base, and Rt names a V
// register so it never needs mangling. The access is rebased onto the biased scratch as the no-offset form;
// any post-index writeback (immediate or register increment) is then applied to the LOW guest base, matching
// nonpie_fixup's writeback semantics. Caller gates on guestbase_on() && !in_excl && base != SP.
static void emit_fold_advsimd_struct(uint32_t in) {
    int base = (in >> 5) & 31, post = (in >> 23) & 1;
    int rm = post ? (int)((in >> 16) & 31) : 31; // post-index increment register (31 = immediate form)
    // Scratch set: Sb (biased base / effective addr), T (compares + temps), T2 (saved NZCV / wb temp). The
    // only GP operands to avoid are the base and, for a register post-index, Rm.
    unsigned usedmask = (1u << base) | (rm != 31 ? (1u << rm) : 0u);
    int sc[3], n = 0;
    for (int r = 0; r <= 30 && n < 3; r++)
        if (!(usedmask & (1u << r)) && !is_stolen(r)) sc[n++] = r;
    int Sb = sc[0], T = sc[1], T2 = sc[2];
    // Spill scratch originals to cpu->mscratch[4..6] (disjoint from emit_mangled_x18's [0..3]); NOT the stack,
    // since the fold runs on a hot memory op where an async host signal would clobber a [sp,#-N] red-zone slot.
    int M = (int)OFF_MSCRATCH;
    e_str(Sb, CPUREG, M + 32);
    e_str(T, CPUREG, M + 40);
    e_str(T2, CPUREG, M + 48);
    if (base == 31)
        e_mov_from_sp(Sb);
    else if (is_stolen(base))
        e_ldr(Sb, CPUREG, base * 8); // guest base from cpu->x[base]
    else
        e_movr(Sb, base); // guest base from the live host reg
    // Bias iff Sb is in [g_nonpie_lo, g_nonpie_hi); a >= 4GiB base is never the low image -> skip with no flag
    // traffic. The compares clobber NZCV, so save/restore the guest flags. (Same discriminator as emit_fold_mem.)
    if (guestbase_on()) {
        emit32(0xD360FC00u | (Sb << 5) | T); // lsr T, Sb, #32
        uint32_t *p_hi = (uint32_t *)g_cp;
        emit32(0);                // cbnz T, Lhi   (>= 4GiB -> skip, flags untouched)
        emit32(0xD53B4200u | T2); // mrs T2, nzcv  (save guest flags)
        e_movconst(T, g_nonpie_lo);
        emit32(0xEB000000u | (T << 16) | (Sb << 5) | 31); // cmp Sb, lo
        uint32_t *p_lo1 = (uint32_t *)g_cp;
        emit32(0); // b.lo Llo   (Sb < lo -> not image)
        e_movconst(T, g_nonpie_hi);
        emit32(0xEB000000u | (T << 16) | (Sb << 5) | 31); // cmp Sb, hi
        uint32_t *p_lo2 = (uint32_t *)g_cp;
        emit32(0); // b.hs Llo   (Sb >= hi -> not image)
        e_movconst(T, g_nonpie_bias);
        emit32(0x8B000000u | (T << 16) | (Sb << 5) | Sb); // add Sb, Sb, bias
        uint8_t *Llo = g_cp;
        emit32(0xD51B4200u | T2); // msr nzcv, T2  (restore guest flags)
        uint8_t *Lhi = g_cp;
        *p_hi = 0xB5000000u | (((uint32_t)(((uint8_t *)Lhi - (uint8_t *)p_hi) / 4) & 0x7FFFF) << 5) | T;
        *p_lo1 = 0x54000000u | (((uint32_t)(((uint8_t *)Llo - (uint8_t *)p_lo1) / 4) & 0x7FFFF) << 5) | 3; // b.lo
        *p_lo2 = 0x54000000u | (((uint32_t)(((uint8_t *)Llo - (uint8_t *)p_lo2) / 4) & 0x7FFFF) << 5) | 2; // b.hs
    }
    // De-index to the no-offset form against Sb: clear post-index (bit23) and Rm[20:16], rebase Xn -> Sb. The
    // V-register list, opcode, R, and size fields are untouched, so the transfer is identical -- only its
    // address is now the biased high pointer.
    emit_a64_bus_guard(Sb, (uint64_t)advsimd_struct_bytes(in), g_emit_gpc);
    struct a64_soft_guard soft =
        emit_a64_soft_guard_begin(Sb, T, T2, (uint64_t)advsimd_struct_bytes(in),
                                  (in & (1u << 22)) ? HL_LOGICAL_VMA_READ : HL_LOGICAL_VMA_WRITE, g_emit_gpc);
    a64_soft_guard_restore(&soft, Sb, M + 32);
    a64_soft_guard_restore(&soft, T, M + 40);
    a64_soft_guard_restore(&soft, T2, M + 48);
    emit32((in & ~(1u << 23) & ~(0x1Fu << 16) & ~(0x1Fu << 5)) | ((unsigned)Sb << 5));
    emit_a64_soft_guard_end(&soft);
    if (post) { // writeback the LOW guest base: Xn += (Rm==31 ? bytes transferred : Xm)
        if (rm == 31) {
            unsigned inc = (unsigned)advsimd_struct_bytes(in);
            if (is_stolen(base)) {
                e_ldr(T, CPUREG, base * 8);
                e_addi(T, T, inc);
                e_str(T, CPUREG, base * 8);
            } else
                e_addi(base, base, inc);
        } else {
            int idx = rm;
            if (is_stolen(rm)) {
                e_ldr(T, CPUREG, rm * 8); // increment from cpu->x[rm]
                idx = T;
            }
            if (is_stolen(base)) { // T2's original is spilled -> free as the base temp (T2 != idx always)
                e_ldr(T2, CPUREG, base * 8);
                emit32(0x8B000000u | ((unsigned)idx << 16) | (T2 << 5) | T2); // add T2, T2, idx
                e_str(T2, CPUREG, base * 8);
            } else if (base == 31) {
                e_mov_from_sp(T2);
                emit32(0x8B000000u | ((unsigned)idx << 16) | (T2 << 5) | T2);
                e_mov_sp_from(T2);
            } else
                emit32(0x8B000000u | ((unsigned)idx << 16) | (base << 5) | base); // add base, base, idx
        }
    }
    e_ldr(Sb, CPUREG, M + 32); // restore scratch originals
    e_ldr(T, CPUREG, M + 40);
    e_ldr(T2, CPUREG, M + 48);
    emit_a64_soft_bounce_commit(g_emit_gpc + 4);
}

// For instructions that WRITE a stolen reg via a legacy special path (adr/adrp/mrs): save x0,x1
// to cpu->mscratch, x1 := cpu. The case then computes a value into x0 and stores it to cpu->x[stolen];
// x18_epilog restores x0,x1. Literal loads do not use this path: target
// initialization permanently reserves x16/x17, so their guarded address
// materialization has one supported implementation.
//
// Spill target is cpu->mscratch[0..1], NOT the guest [sp,#-N] "red zone": AArch64 has NO architectural
// red zone, so a store below the guest SP faults whenever the page under SP is unmapped (a shallow guest
// stack) -- the exact crash class 6d38d96c/7de3a17a closed for the steal path. x16/x17 are NOT free here
// (NOSTEAL1617 keeps them as guest values), so this mirrors the mscratch spill in emit_mangled_x18 /
// emit_fold_mem. x28 (CPUREG) = cpu is the whole-block invariant (prologue `mov x28,x0`; guest x28 is
// stolen), so it reaches mscratch on every path. Slots [0..1] are free during the bracket: this helper
// wraps a SINGLE special-write whose body only touches cpu->x[stolen] + guest memory -- never mscratch,
// never a nested mangle/fold -- so it cannot alias the saved x0/x1.
static void x18_prolog(void) {
    e_str(0, CPUREG, (int)OFF_MSCRATCH);
    e_str(1, CPUREG, (int)OFF_MSCRATCH + 8);
    e_load_cpu(1);
}

static void x18_epilog(void) {
    e_ldr(1, CPUREG, (int)OFF_MSCRATCH + 8);
    e_ldr(0, CPUREG, (int)OFF_MSCRATCH);
}

// A3 §B instrumentation (PROF=1 only): bump a 64-bit global counter from emitted code. Self-contained:
// stashes x9/x10 in cpu->mscratch[0..1] (NOT the [sp,#-N] red zone -- AArch64 has none, and this runs at
// a §B shadow push/ret point that can be reached with a shallow guest SP, so a below-SP store would fault
// exactly as the pixman self-loop did in 6d38d96c). mscratch[0..1] is free at all three call sites: the
// shadow-push call precedes that helper's own mscratch spill, and both shadow-ret calls follow its
// x0..x3 restore. x28 (CPUREG) = cpu holds for the whole block on every path. Gated on g_prof, so the
// non-PROF codegen is byte-identical to baseline (zero steady-state cost).
static void emit_prof_bump(void *ctr) {
    e_str(9, CPUREG, (int)OFF_MSCRATCH);
    e_str(10, CPUREG, (int)OFF_MSCRATCH + 8);
    e_adrp_add(9, (uint64_t)ctr); // x9 = &counter (plain RW data; adrp+add reaches it)
    e_ldr(10, 9, 0);
    e_addi(10, 10, 1);
    e_str(10, 9, 0);
    e_ldr(9, CPUREG, (int)OFF_MSCRATCH);
    e_ldr(10, CPUREG, (int)OFF_MSCRATCH + 8);
}

// §B: store a constant to cpu->x[30] (the stolen guest link reg). x28=cpu.
// IBSLIM: with x16/x17 stolen (A1 default) x16 is engine-private scratch here, so the legacy
// x0 spill/restore dance through cpu->mscratch (5 insns, 3 memory ops PER GUEST CALL: bl and blr
// both pass through this) collapses to movconst+str (typically 3 insns, 1 store). Guest x16 is
// untouched -- its value lives only in cpu->x[16] under the steal. NOIBSLIM=1 restores the exact
// legacy sequence for A/B.
static void emit_set_x30(uint64_t val) {
    if (g_steal1617 && !g_noibslim) {
        e_movconst(16, val);
        e_str(16, CPUREG, 30 * 8);
        return;
    }
    e_str(0, CPUREG, (int)OFF_MSCRATCH);
    e_movconst(0, val);
    e_str(0, CPUREG, 30 * 8);
    e_ldr(0, CPUREG, (int)OFF_MSCRATCH);
}

// §B shadow push: cpu->x[30] = gpc+4; sstk[ssp&1023] = (gpc+4, &Lcont); ssp++. x0..x2 spilled to
// cpu->mscratch (all guest regs are live across the call). Returns the `adr x1,Lcont` to backpatch.
static uint32_t *emit_shadow_push(uint64_t gpc) {
    int M = (int)OFF_MSCRATCH;
    if (g_prof) emit_prof_bump(&g_prof_shpush); // A3: count §B shadow pushes executed (PROF only)
    e_stp(0, 1, CPUREG, M);
    // spill x0..x3 -> mscratch (paired: 2 stp not 4 str)
    e_stp(2, 3, CPUREG, M + 16);
    // guest_ret is the guest-VISIBLE link value (spilled to the guest stack + matched on the ret),
    // so use the UN-BIASED (low) link vaddr for non-PIE; pcrel_base is identity for PIE.
    e_movconst(0, pcrel_base(gpc) + 4);
    // x0 = guest_ret; cpu->x[30] = guest_ret (ALWAYS)
    e_str(0, CPUREG, 30 * 8);
    // x1 = ssp (capped at 1024)
    e_ldr(1, CPUREG, (int)OFF_SSP);
    uint32_t *p_full = (uint32_t *)g_cp;
    // tbnz x1, #10, Lskip (ssp==1024 -> overflow; no flags)
    emit32(0);
    e_addlsl4(2, CPUREG, 1);
    // x2 = C + idx*16 + OFF_SSTK = &sstk[2*ssp]
    e_addi(2, 2, (unsigned)OFF_SSTK);
    uint32_t *p_adr = (uint32_t *)g_cp;
    // adr x3, Lcont (host_ret; backpatched)
    emit32(0);
    // sstk[2*ssp] = (guest_ret, host_ret=&Lcont)
    e_stp(0, 3, 2, 0);
    e_mov_from_sp(3);
    e_addlsl3(2, CPUREG, 1);
    // gsp[ssp] = current guest SP (frame disambiguator)
    e_str(3, 2, (int)OFF_GSP);
    e_addi(1, 1, 1);
    // ssp++
    e_str(1, CPUREG, (int)OFF_SSP);
    uint8_t *Lskip = g_cp;
    *p_full = 0x37000000u | (10u << 19) | (((uint32_t)(((uint8_t *)Lskip - (uint8_t *)p_full) / 4) & 0x3FFF) << 5) |
              // tbnz x1,#10
              1;
    e_ldp(0, 1, CPUREG, M);
    // restore x0..x3 (paired: 2 ldp not 4 ldr)
    e_ldp(2, 3, CPUREG, M + 16);
    return p_adr;
}

// §B profile gate: scan the target's entry block. A LEAF function (reaches `ret` with no bl/blr
// first) gains nothing from the shadow-RAS -- its monomorphic return is predicted by the per-site IC
// -- so paying the per-bl shadow push is pure overhead (floatk: sqrt/sin/pow). Only non-leaf targets
// (depth -> the hardware RAS predicts nested returns: stringk/recursion) get §B. Static, no profiling
// overhead; the ret auto-adapts (no frame pushed -> classify falls to the IC return).
// A3 §B depth-gate tuning. The baseline gate (scan_calls + target_is_leaf) is depth-2 static and
// MISCLASSIFIES two important cases as "leaf" (-> withholds §B -> the return falls to the IBTC):
//   (1) a function LARGER than the 64-insn scan window whose calls live past it (fib at -O2, most of
//       sqlite's VDBE helpers) -- the scan exhausts having seen no bl and reports "leaf";
//   (2) a "shallow" helper that calls only leaves but is itself called from MANY sites -- its single
//       return site is polymorphic, so the per-site IC thrashes (exactly what the RAS fixes).
// §B is self-validating (emit_shadow_ret checks guest_ret AND guest_sp; a wrong guess -> IBTC, never a
// misland), so the gate only ever trades cycles, never correctness. MEASUREMENT (see arm-a3.md) shows
// §B is NET-NEGATIVE on every return-heavy workload tested -- sqlite, qsort, AND the ideal polymorphic
// deep-recursion cases (longfib 2x, deepcall 1.4x) -- because the shadow push (~19 insn + sstk stores)
// plus the shadow-ret validate (~22 insn: guest_ret AND guest_sp compares) cost FAR more than the IBTC
// return path they replace (a monomorphic per-site IC hit, or even a thrashing shared-hash probe). The
// host RAS's 1-cycle `ret` is buried under ~40 insn of software bookkeeping. So the right tune is the
// OPPOSITE of "widen": DISABLE §B and return every ret through the proven IBTC. Levels (env, once):
//   -1 (DEFAULT)      -> §B OFF: no shadow push; every ret -> bare IBTC (IC + shared hash). The win.
//   -2 SHADOWGATE=-2  -> §B OFF on the push side, but ret keeps the shadow-ret stub (empty -> IBTC).
//    0 NOSHADOWTUNE=1 -> EXACT original §B-on gate (byte-identical baseline codegen). A/B kill switch.
//    1 SHADOWGATE=1   -> widen-fix: window-exhaustion = large/complex fn -> DEEP not leaf (measured: worse).
//    2 SHADOWGATE=2   -> widen more: ANY direct call -> §B (measured: worse / no better).
static int shadowgate(void) {
    return -1;
}

// Scan target's straight-line extent (bounded by forward-branch reach). Returns -1 if a blr (unknown
// callee) -- or, when tuned, if the scan window is exhausted with no clean terminal (large/complex fn,
// treat as deep) -- else the count of direct-call (bl) targets, writing up to `max` of them to calls[].
static int scan_calls(uint64_t target, uint64_t calls[], int max) {
    int64_t reach = 0;
    int n = 0;
    for (int i = 0; i < 64; i++) {
        uint32_t in = a64_fetch_instruction(target + (uint64_t)i * 4, NULL);
        // blr -> unknown callee
        if ((in & 0xFFFFFC1Fu) == 0xD63F0000u) return -1;
        if ((in & 0xFC000000u) == 0x94000000u) {
            if (n < max) calls[n] = target + (uint64_t)i * 4 + ((uint64_t)sext(in & 0x3FFFFFF, 26) << 2);
            n++;
            // bl
        }
        int64_t off = 0;
        int isb = 0;
        if ((in & 0xFF000010u) == 0x54000000u) {
            off = sext((in >> 5) & 0x7FFFF, 19);
            isb = 1;
            // b.cond
        } else if ((in & 0x7E000000u) == 0x34000000u) {
            off = sext((in >> 5) & 0x7FFFF, 19);
            isb = 1;
            // cbz/cbnz
        } else if ((in & 0x7E000000u) == 0x36000000u) {
            off = sext((in >> 5) & 0x3FFF, 14);
            isb = 1;
            // tbz/tbnz
        } else if ((in & 0xFC000000u) == 0x14000000u) {
            off = sext(in & 0x3FFFFFF, 26);
            isb = 1;
            // b
        }
        if (isb && off > 0 && i + off < 64 && i + off > reach) reach = i + off;
        if ((in & 0xFFFFFC1Fu) == 0xD65F0000u || (in & 0xFC000000u) == 0x14000000u || (in & 0xFFFFFC1Fu) == 0xD61F0000u)
            // terminal past all branches
            if (i >= reach) return n;
    }
    // window exhausted with no clean terminal: a function larger than the scan window. Baseline reported
    // whatever bl count it happened to see (usually 0 -> "leaf"); tuned treats it as deep/unknown.
    return shadowgate() ? -1 : n;
}

static int is_leaf0(uint64_t t) {
    uint64_t c[1];
    return scan_calls(t, c, 0) == 0;
    // no calls at all
}

// §B helps only on DEPTH (the RAS predicts nested returns). A leaf or a depth-2 "shallow" function
// (all its calls go to leaves: sqrt/sin/pow's helpers) gains nothing -> keep cheap Stage-B. Only a
// function that calls a NON-leaf (or recurses, or calls indirectly) is deep enough to pay the push.
static int target_is_leaf(uint64_t target) {
    uint64_t calls[16];
    int n = scan_calls(target, calls, 16);
    // SHADOWGATE=-1: never §B (every bl -> leaf path, every ret -> IBTC). Floor experiment.
    if (shadowgate() < 0) return 1;
    // indirect callee / large-complex fn (tuned) -> assume deep -> §B
    if (n < 0) return 0;
    // true leaf (no calls at all: sqrt/sin/pow) -> Stage-B, regardless of level
    if (n == 0) return 1;
    // L2/L3: ANY direct call -> §B (covers multiply-called shallow helpers whose ret site is polymorphic)
    if (shadowgate() >= 2) return 0;
    for (int i = 0; i < n && i < 16; i++)
        // calls a non-leaf -> deep -> §B
        if (!is_leaf0(calls[i])) return 0;
    // all-leaf-calls (shallow) -> Stage-B
    return 1;
}

#define CTX_INLINE_DEPTH 3
#define CTX_INLINE_INSNS 64

static int context_clone_candidate(uint64_t target, const uint64_t ancestors[], int depth, uint64_t *retpc, int *cost) {
    if (depth >= CTX_INLINE_DEPTH) return 0;
    for (int i = 0; i < depth; i++)
        if (ancestors[i] == target) return 0;
    uint64_t anc[CTX_INLINE_DEPTH];
    for (int i = 0; i < depth; i++)
        anc[i] = ancestors[i];
    anc[depth] = target;
    int total = 0;
    /*
     * A candidate is decoded sequentially inside one VM region.  Query that
     * containing region once instead of asking the host kernel about the same
     * page before every instruction (mach_vm_region is especially costly).
     * Retain the old boundary behavior by rejecting an instruction whose last
     * byte is outside the containing readable region.
     */
    hl_host_region region;
    if (!hl_host_region_query((uintptr_t)target, &region) || target < region.address ||
        !(region.protection & HL_HOST_REGION_READ))
        return 0;
    uint64_t region_offset = target - region.address;
    if (region_offset >= region.size) return 0;
    uint64_t remaining = region.size - region_offset;
    for (int i = 0; i < CTX_INLINE_INSNS; i++) {
        uint64_t offset = (uint64_t)i * 4;
        if (offset > remaining || remaining - offset < 4 || offset > UINT64_MAX - target) return 0;
        uint64_t pc = target + offset;
        uint32_t in = a64_fetch_instruction(pc, NULL);
        total++;
        if ((in & 31u) == 30u && (in & 0xFC000000u) != 0x94000000u && (in & 0xFFFFFC1Fu) != 0xD65F0000u) return 0;
        if (in == 0xD4000001u || (in & 0xFC000000u) == 0x14000000u || (in & 0xFFFFFC1Fu) == 0xD61F0000u ||
            (in & 0xFFFFFC1Fu) == 0xD63F0000u)
            return 0;
        if ((in & 0xFC000000u) == 0x94000000u) {
            int64_t off = sext(in & 0x3FFFFFF, 26) << 2;
            uint64_t child_ret;
            int child_cost;
            if (!context_clone_candidate(pc + off, anc, depth + 1, &child_ret, &child_cost)) return 0;
            total += child_cost;
            if (total > CTX_INLINE_INSNS * CTX_INLINE_DEPTH) return 0;
        }
        if ((in & 0xFFFFFC1Fu) == 0xD65F0000u) {
            if (((in >> 5) & 31) != 30) return 0;
            *retpc = pc;
            *cost = total;
            return 1;
        }
    }
    return 0;
}

// §B guest bl: push shadow, host `bl body(target)` (RAS pushes &Lcont), Lcont continues at gpc+4.
static void emit_bl_ras(uint64_t gpc, uint64_t target) {
    /*
     * Shadow returns and host BL edges retain host PCs in otherwise unrelated
     * blocks.  Once SMC is active use the dispatcher-only leaf path so a
     * targeted source invalidation has no hidden ingress to the old body.
     */
    if (smc_seen() || target_is_leaf(target)) {
        if (g_prof) g_prof_bl_leaf++; // A3: depth-gate steered this bl to the cheap leaf Stage-B path
        // Guest LR is a guest-VISIBLE value (the guest spills it to its stack), so it must be the
        // UN-BIASED (low) link vaddr -- non-PIE runs gpc HIGH; the dispatcher re-biases low->high on
        // the ret. pcrel_base is identity for PIE (no codegen change for the matrix).
        emit_set_x30(pcrel_base(gpc) + 4);
        emit_chain_exit(target);
        return;
        // leaf -> cheap Stage-B (IC return)
    }
    if (g_prof) g_prof_bl_shadow++; // A3: depth-gate steered this bl to §B (shadow push + RAS ret)
    uint32_t *p_adr = emit_shadow_push(gpc);
    void *body = map_body(target);
    uint32_t *slot = (uint32_t *)g_cp;
    if (body) {
        int64_t d = ((uint8_t *)body - (uint8_t *)slot) / 4;
        emit32(0x94000000u | ((uint32_t)d & 0x3FFFFFFu));
        // host bl body (RAS pushes &Lcont)
    } else {
        add_pend2(slot, target, 1);
        emit_exit_const(target, R_BRANCH);
        // not translated yet: spill-exit (slot patched to `bl body`)
    }
    // host ret lands here
    uint8_t *Lcont = g_cp;
    int64_t ao = Lcont - (uint8_t *)p_adr;
    *p_adr = 0x10000000u | ((uint32_t)(ao & 3) << 29) | (((uint32_t)((ao >> 2) & 0x7FFFF)) << 5) | 3u;
    // after the call returns -> gpc+4
    emit_chain_exit(gpc + 4);
}

// §B guest ret: if cpu->x[30] == shadow-top guest_ret, pop + real x30=host_ret + host `ret`
// (hardware-RAS predicted). Else fall back to the dispatcher reading cpu->x[30]. Never lands wrong.
static void emit_shadow_ret(void) {
    int M = (int)OFF_MSCRATCH;
    e_stp(0, 1, CPUREG, M);
    // spill x0..x3 (paired)
    e_stp(2, 3, CPUREG, M + 16);
    // x0 = ssp
    e_ldr(0, CPUREG, (int)OFF_SSP);
    uint32_t *p_cbz = (uint32_t *)g_cp;
    // cbz x0, Lfb (empty shadow)
    emit32(0);
    // x0 = ssp-1 = idx (ssp<=1024 -> no wrap)
    e_subi(0, 0, 1);
    e_addlsl4(1, CPUREG, 0);
    // x1 = &sstk[2*idx]
    e_addi(1, 1, (unsigned)OFF_SSTK);
    // x2 = guest_ret, x3 = host_ret
    e_ldp(2, 3, 1, 0);
    // x1 = cpu->x[30] (guest return target)
    e_ldr(1, CPUREG, 30 * 8);
    // sub x1, x2, x1 (guest_ret - x30; no flags)
    emit32(0xCB000000u | (1 << 16) | (2 << 5) | 1);
    uint32_t *p_cb1 = (uint32_t *)g_cp;
    // cbnz x1, Lfb (foreign/longjmp)
    emit32(0);
    e_addlsl3(1, CPUREG, 0);
    // x2 = gsp[idx] (guest SP captured at the bl)
    e_ldr(2, 1, (int)OFF_GSP);
    // x1 = current guest SP
    e_mov_from_sp(1);
    // sub x1, x1, x2 (sp - gsp; no flags)
    emit32(0xCB000000u | (2 << 16) | (1 << 5) | 1);
    uint32_t *p_cb2 = (uint32_t *)g_cp;
    // cbnz x1, Lfb (guest_ret matched but wrong frame -> slow)
    emit32(0);
    // FAST: ssp-- (pop)
    e_str(0, CPUREG, (int)OFF_SSP);
    // real x30 = host_ret
    e_movr(30, 3);
    e_ldp(0, 1, CPUREG, M);
    // restore x0..x3 (paired)
    e_ldp(2, 3, CPUREG, M + 16);
    if (g_prof) emit_prof_bump(&g_prof_shret_hit); // A3: §B predicted-return FAST hit (PROF only)
    // host ret -> &Lcont (hardware-RAS predicted)
    e_hret();
    uint8_t *Lfb = g_cp;
    *p_cbz = 0xB4000000u | (((uint32_t)(((uint8_t *)Lfb - (uint8_t *)p_cbz) / 4) & 0x7FFFF) << 5) | 0;
    // cbnz x1
    *p_cb1 = 0xB5000000u | (((uint32_t)(((uint8_t *)Lfb - (uint8_t *)p_cb1) / 4) & 0x7FFFF) << 5) | 1;
    // cbnz x1
    *p_cb2 = 0xB5000000u | (((uint32_t)(((uint8_t *)Lfb - (uint8_t *)p_cb2) / 4) & 0x7FFFF) << 5) | 1;
    e_ldp(0, 1, CPUREG, M);
    // restore x0..x3 (paired)
    e_ldp(2, 3, CPUREG, M + 16);
    if (g_prof) emit_prof_bump(&g_prof_shret_fb); // A3: §B return fell to the IBTC fallback (PROF only)
    // UNWIND/FOREIGN -> IBTC (per-site IC + hash), NOT the dispatcher
    emit_ibranch(30);
}

// ---------------- the translator ----------------
// Translate the basic block at guest address gpc; returns host entry pointer.
// re-target a cond branch to offset d (instrs)
static uint32_t recode_cond(uint32_t in, int64_t d) {
    // cbz/cbnz
    if ((in & 0x7E000000u) == 0x34000000u) return (in & 0xFF00001Fu) | ((uint32_t)(d & 0x7FFFF) << 5);
    // b.cond
    if ((in & 0xFF000010u) == 0x54000000u) return (in & 0xFF00000Fu) | ((uint32_t)(d & 0x7FFFF) << 5);
    // tbz/tbnz
    return (in & 0xFFF8001Fu) | ((uint32_t)(d & 0x3FFF) << 5);
}

// W4E tier-2: emit the in-cache back-edge hotness counter for a hot-candidate self-loop. Runs on the
// TAKEN (loop) edge in tier-1. Flag-free (sub-imm + cbnz never touch NZCV, so the guest's condition
// flags are preserved across the back-edge -- mandatory for bit-exactness when the loop body does not
// itself re-set the tested flags). Counts DOWN from g_t2thresh; on reaching zero it exits R_TIER2 so the
// dispatcher promotes the block, after which this stub is dead.
//
// SCRATCH: two host regs for the counter pointer + value. Under the x16/x17 steal (g_steal1617, the
// aarch64 default) those two host regs are ENGINE-PRIVATE at this block-boundary back-edge -- the exact
// invariant emit_irq_check already relies on to poll cpu->irq with no guest-reg stash -- so use them
// DIRECTLY with no memory spill. The legacy (NOSTEAL1617) path has no free host reg here, so it falls
// back to stashing x9/x10 in the [sp,#-16/-24] slots.
//
// Why the steal path must NOT use the [sp,#-N] slots: AArch64 has NO architectural red zone, so a store
// below the guest SP is only safe if that memory happens to be mapped+writable. A hot pixman NEON fill
// self-loop (`st1 {v0-v3},[x2],#32; subs; b.ge`) reaches this counter with the guest SP shallow on its
// stack; the page just under SP is an untouched anon page that faults on write (EXC_BAD_ACCESS), so the
// old unconditional `stur x9,[sp,#-16]` here crashed GTK4's software/pixman render. The engine already
// established this principle elsewhere -- emit_fold_mem / emit_mangled_x18 spill to cpu->mscratch rather
// than a [sp,#-N] slot precisely because a below-SP slot is not a safe scratch -- and this brings the
// tier-2 counter in line. (The counter never runs concurrently with a fold, and x9/x10 stay LIVE in
// their host regs on the steal path, so the R_TIER2 exit's emit_spill captures the correct guest values.)
static void emit_t2_counter(int slot, uint64_t start, void *body) {
    int rp = g_steal1617 ? 16 : 9;  // counter-pointer scratch
    int rv = g_steal1617 ? 17 : 10; // counter-value scratch
    if (!g_steal1617) {
        e_stur(9, 31, -16);
        e_stur(10, 31, -24);
    }
    // rp = &g_t2cnt[slot] (plain RW data; adrp+add reaches it)
    emit_t2cntptr(rp, slot); // recorded &g_t2cnt[slot] bake (fixed 4-insn + reloc when g_pcache)
    e_ldr(rv, rp, 0);
    // --count (sub immediate: flag-free)
    e_subi(rv, rv, 1);
    e_str(rv, rp, 0);
    uint32_t *p_cbnz = (uint32_t *)g_cp;
    // cbnz rv, Lcont (still counting -> keep looping; flag-free)
    emit32(0);
    // reached 0 -> restore scratch (legacy) + exit to the dispatcher to promote (pc = loop start)
    if (!g_steal1617) {
        e_ldur(9, 31, -16);
        e_ldur(10, 31, -24);
    }
    emit_exit_const(start, R_TIER2);
    uint8_t *Lcont = g_cp;
    *p_cbnz = 0xB5000000u | (((uint32_t)(((uint8_t *)Lcont - (uint8_t *)p_cbnz) / 4) & 0x7FFFF) << 5) | (unsigned)rv;
    if (!g_steal1617) {
        e_ldur(9, 31, -16);
        e_ldur(10, 31, -24);
    }
    // b body  (the loop back-edge, in-cache)
    int64_t d = ((uint8_t *)body - (uint8_t *)g_cp) / 4;
    emit32(0x14000000u | ((uint32_t)d & 0x3FFFFFFu));
}

// W4E tier-2: store-to-load-forwarding hazard guard. Folding the back-edge tightens the loop enough that
// a store immediately followed by a load of the SAME address (e.g. a volatile / aliased RMW of one stack
// slot every iteration) starts hitting an Apple-Silicon store-forwarding replay -- measured as a ~3.7x
// slowdown on a `volatile` counter loop, while the extra tier-1 trampoline branch happened to mask it. So
// if the loop body contains a store whose (size,base,offset) a later load reuses, leave the loop on tier-1
// (no counter, no fold). Pure-store, load-only, and distinct-address load+store loops are NOT flagged and
// still tier up (measured wins). Scans the guest body [start, endpc).
static int loop_has_rmw_hazard(uint64_t start, uint64_t endpc) {
    uint64_t stores[32];
    int ns = 0;
    for (uint64_t p = start; p < endpc; p += 4) {
        uint32_t in = a64_fetch_instruction(p, NULL);
        uint64_t key = 0;
        int opc = -1;
        // load/store unsigned imm12
        if ((in & 0x3B000000u) == 0x39000000u) {
            opc = (in >> 22) & 3;
            key = ((uint64_t)((in >> 30) & 3) << 24) | (((in >> 5) & 31) << 12) | ((in >> 10) & 0xFFF);
        }
        // STUR/LDUR unscaled imm9
        else if ((in & 0x3B200C00u) == 0x38000000u) {
            opc = (in >> 22) & 3;
            key = (1ull << 40) | ((uint64_t)((in >> 30) & 3) << 24) | (((in >> 5) & 31) << 12) | ((in >> 12) & 0x1FF);
        }
        if (opc == 0) {
            if (ns < 32) stores[ns++] = key; // a store
        } else if (opc > 0) {
            for (int i = 0; i < ns; i++)
                if (stores[i] == key) return 1; // a load reusing a stored address -> hazard
        }
    }
    return 0;
}

// W4E tier-2: when a hot self-loop's FIRST instruction writes a vector register (the memcpy/memcmp
// stream loops), the once-per-region cpu->vdirty-set store lands at the loop top and, under the folded
// back-edge, re-executes every iteration (measured ~1.7x on a 1 MiB memcpy vs native). The store is
// idempotent -- vdirty is a sticky flag cleared only by a full spill -- so it need run only ONCE per
// loop ENTRY. During the tier-2 recompile the store is hoisted above a fresh async poll placed at the
// loop top; this holds the loop re-entry point (after the store, at that poll) so emit_selfloop folds
// the back-edge to it instead of to `body`. NULL when no hoist applies (fall back to `body`).
static uint8_t *g_t2_loop_top;
static uint32_t *g_t2_irq_patch;

// W4E tier-2: emit a single-block self-loop's terminating conditional (taken target == block start).
//   tier-1 build: cond -> Lcnt (counter) ; fall-through = loop exit. The counter promotes when hot.
//   tier-2 build: cond -> body DIRECTLY (the fold) ; fall-through = loop exit. One taken branch/iter
//                 instead of tier-1's `b.cond Ltaken; b body` -- native-equivalent. Bit-identical control
//                 flow (same condition, same taken target = loop top). `body` is always a few insns above,
//                 well inside the conditional's imm19/imm14 reach.
static void emit_selfloop(uint32_t in, uint64_t start, uint64_t fall, void *body, int slot) {
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    emit_chain_exit(fall);
    if (g_tier2_build) {
        // Fold the back-edge past the hoisted vdirty store when one was emitted at the loop top
        // (g_t2_loop_top); the store then runs once per entry, never per iteration.
        uint8_t *tgt = g_t2_loop_top ? g_t2_loop_top : (uint8_t *)body;
        int64_t d = (tgt - (uint8_t *)patch) / 4;
        *patch = recode_cond(in, d);
        return;
    }
    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
    *patch = recode_cond(in, d);
    emit_t2_counter(slot, start, body);
}

// ---- LSE atomics idiom upgrade ----
// Distro binaries are built ARMv8.0-baseline, so every atomic is an ldxr/stxr retry
// loop. Apple Silicon has FEAT_LSE: recognize the loop and emit a single atomic op
// (2.29x faster, and it removes the load/store-exclusive monitor region that
// complicates the translator). AL ordering is always safe.
// Emit ONE instruction of an LSE-atomic-loop rewrite, applying the same non-PIE bias-fold / stolen-reg
// mangling the main decode loop would. `is_mem`: the access (swp/ldadd/.../casal) -- a non-PIE LOW [Xn]
// gets +bias (emit_fold_mem, which also mangles stolen Rt/Rn/Rs and re-derives its own field mask, incl
// the atomic value operand Rs). `mask` is used only off the fold path (PIE, or SP-based): the gpr fields
// to mangle if they name a stolen reg. CRUCIAL: the original ldxr/stxr monitor fallback is UNUSABLE when
// an operand is stolen or the base is a low non-PIE pointer -- the per-insn ldr/str it injects between the
// load- and store-exclusive clear the monitor so stxr retries forever. Each rewritten LSE op is a SINGLE
// instruction (no monitor), so the injected spill/fill is harmless, making this the correct path. The
// common clean PIE case still lowers to the bare op -> byte-identical to before.
static void emit_atomic_part(uint32_t in, int mask, int is_mem) {
    if (is_mem && (guestbase_on() || jit_guest_soft_active()) && (jit_guest_soft_active() || ((in >> 5) & 31) != 31)) {
        if (jit_guest_bus_active()) emit_a64_bus_guard_instruction(in, g_emit_gpc);
        emit_fold_mem(in, 0);
    } else if (uses_x18(in, mask))
        emit_mangled_x18(in, mask);
    else
        emit32(in);
}

// The rewritten LSE op replaces a whole ldxr/stxr RETRY LOOP, and that loop only ever falls out of
// `cbnz Ws, loop` with the store-exclusive status register Ws == 0. A single LSE instruction never
// touches Ws, so without this the guest keeps the stale pre-loop value of Ws -- an architectural
// divergence the differential ISA fuzzer (tests/fuzz/isa/aarch64) caught directly. Emit it LAST: in
// the original loop the stxr writes Ws after every other operand, so a Ws that aliases Wt/Ws2/Wm
// must also end up zero.
static void emit_lse_status_zero(int Ws) {
    if (Ws == 31) return;                               /* stxr wzr: the status is architecturally discarded */
    emit_atomic_part(0x2A1F03E0u | (uint32_t)Ws, 1, 0); /* mov Ws, wzr */
}

// Returns bytes consumed (12 or 16) if a known atomic loop at gpc was rewritten, else 0.
static int try_lse_atomic(uint64_t gpc) {
    uint32_t i0 = a64_fetch_instruction(gpc, NULL);
    // load-exclusive?
    if ((i0 & 0x3F400000u) != 0x08400000u) return 0;
    int sz = (i0 >> 30) & 3;
    // word/dword only
    if (sz < 2) return 0;
    // non-pair
    if (((i0 >> 16) & 0x1F) != 0x1F || ((i0 >> 10) & 0x1F) != 0x1F) return 0;
    int Wt = i0 & 31, Xn = (i0 >> 5) & 31;
    uint32_t i1 = a64_fetch_instruction(gpc + 4, NULL);

    // SWP:  ldxr Wt,[Xn]; stxr Ws,Wv,[Xn]; cbnz Ws,loop
    if ((i1 & 0x3F400000u) == 0x08000000u && ((i1 >> 30) & 3) == sz && ((i1 >> 10) & 0x1F) == 0x1F &&
        ((i1 >> 5) & 31) == Xn) {
        int Ws = (i1 >> 16) & 31, Wv = i1 & 31;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL);
        if ((i2 & 0xFF000000u) == 0x35000000u && (i2 & 31) == Ws &&
            (gpc + 8 + (uint64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
            // A bare `swpal` in place of this swap loop is a deterministic lost-wakeup for multithreaded
            // musl: node's V8 workers park forever in __unlock's `a_swap(l,0)==2 && __wake` because the
            // swp'd old value doesn't drive the wake (node:alpine hung >400s; the exclusive pair completes
            // in 0.28s, matching the docker oracle). The `ldadd*`/`casal` idioms below are unaffected. So
            // upgrade ONLY when the exclusive-pair fallback is UNUSABLE -- i.e. when translating it verbatim
            // would inject a monitor-clearing ldr/str between the ldxr and stxr (a stolen operand needs a
            // cpu-slot mangle, or a non-PIE low base needs a bias-fold), which would spin the stxr forever.
            // The common clean-PIE case (no stolen operand, no fold) keeps the proven exclusive pair.
            if (guestbase_on() || is_stolen(Wt) || is_stolen(Xn) || is_stolen(Ws) || is_stolen(Wv)) {
                // swpal Wv, Wt, [Xn] (a single LSE op; emit_atomic_part folds/mangles the corner cases).
                emit_atomic_part(0xB8E08000u | (sz == 3 ? 0x40000000u : 0) | (Wv << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 12;
            }
        }
    }
    // LDADD/LDSET/LDEOR/LDCLR/LDADD-neg:  ldxr Wt,[Xn]; <op> Ws2,Wt,Wm; stxr Ws,Ws2,[Xn]; cbnz Ws,loop
    // 0 add 1 orr 2 eor 3 and 4 sub
    int op = -1;
    if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0x8B000000u : 0x0B000000u))
        op = 0;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xAA000000u : 0x2A000000u))
        op = 1;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xCA000000u : 0x4A000000u))
        op = 2;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0x8A000000u : 0x0A000000u))
        op = 3;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xCB000000u : 0x4B000000u))
        op = 4;
    if (op >= 0) {
        int Ws2 = i1 & 31, n = (i1 >> 5) & 31, m = (i1 >> 16) & 31, Wm = -1;
        if (op == 4) {
            if (n == Wt) Wm = m;
            // sub: not commutative, Rn must be Wt
        } else {
            if (n == Wt)
                Wm = m;
            else if (m == Wt)
                Wm = n;
        }
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        if (Wm >= 0 && (i2 & 0x3F400000u) == 0x08000000u && ((i2 >> 30) & 3) == sz && (i2 & 31) == Ws2 &&
            ((i2 >> 5) & 31) == Xn && ((i2 >> 10) & 0x1F) == 0x1F) {
            int Ws = (i2 >> 16) & 31;
            if ((i3 & 0xFF000000u) == 0x35000000u && (i3 & 31) == Ws &&
                (gpc + 12 + (uint64_t)(sext((i3 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
                // op>=3 borrows Ws as a scratch holding ~Wm / -Wm across two ops -> it must not alias Wm.
                if (op >= 3 && Wm == Ws) return 0;
                uint32_t szb = sz == 3 ? 0x40000000u : 0, szd = sz == 3 ? 0x80000000u : 0;
                if (op <= 2) {
                    uint32_t lse = op == 0 ? 0xB8E00000u : op == 1 ? 0xB8E03000u : 0xB8E02000u;
                    // ldaddal/ldsetal/ldeoral Wm, Wt, [Xn]
                    emit_atomic_part(lse | szb | (Wm << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                } else if (op == 3) {
                    // fetch_and: *Xn &= Wm  ==  ldclr ~Wm:  mvn Ws,Wm (orn Ws,wzr,Wm); ldclral Ws, Wt, [Xn]
                    emit_atomic_part(0x2A200000u | szd | (Wm << 16) | (31 << 5) | Ws, 1 | 4, 0);
                    emit_atomic_part(0xB8E01000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                } else {
                    // fetch_sub: *Xn -= Wm  ==  ldadd -Wm:  neg Ws,Wm (sub Ws,wzr,Wm); ldaddal Ws, Wt, [Xn]
                    emit_atomic_part(0x4B000000u | szd | (Wm << 16) | (31 << 5) | Ws, 1 | 4, 0);
                    emit_atomic_part(0xB8E00000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                }
                // reconstruct the new value (re-emit the original op) for any following guest code
                emit_atomic_part(i1, gpr_field_mask(i1), 0);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 16;
            }
        }
    }
    // LDADD immediate (fetch_add of a constant -- the headline refcount/counter case):
    //   ldxr Wt,[Xn]; add Ws2,Wt,#imm (sh=0); stxr Ws,Ws2,[Xn]; cbnz Ws,loop
    uint32_t addib = sz == 3 ? 0x91000000u : 0x11000000u;
    if ((i1 & 0xFFC00000u) == addib && ((i1 >> 5) & 31) == Wt) {
        int Ws2 = i1 & 31;
        unsigned imm = (i1 >> 10) & 0xFFF;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        if ((i2 & 0x3F400000u) == 0x08000000u && ((i2 >> 30) & 3) == sz && (i2 & 31) == Ws2 && ((i2 >> 5) & 31) == Xn &&
            ((i2 >> 10) & 0x1F) == 0x1F) {
            int Ws = (i2 >> 16) & 31;
            if ((i3 & 0xFF000000u) == 0x35000000u && (i3 & 31) == Ws &&
                (gpc + 12 + (uint64_t)(sext((i3 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
                uint32_t szb = sz == 3 ? 0x40000000u : 0;
                // Ws (dead status reg) = imm  (movz Ws, #imm; e_movz always uses the 64-bit form)
                emit_atomic_part(0xD2800000u | ((imm & 0xFFFFu) << 5) | Ws, 1, 0);
                // ldaddal Ws, Wt, [Xn]
                emit_atomic_part(0xB8E00000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                // re-emit add Ws2, Wt, #imm (reconstruct the new value)
                emit_atomic_part(i1, gpr_field_mask(i1), 0);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 16;
            }
        }
    }
    // CAS:  ldxr Wt,[Xn]; cmp Wt,Wexp; b.ne out; stxr Ws,Wnew,[Xn]; cbnz Ws,loop; out:
    // subs wzr, Wt, Wexp (cmp)
    uint32_t subsb = sz == 3 ? 0xEB00001Fu : 0x6B00001Fu;
    if ((i1 & 0xFFE0FC1Fu) == subsb && ((i1 >> 5) & 31) == Wt) {
        int Wexp = (i1 >> 16) & 31;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        uint32_t i4 = a64_fetch_instruction(gpc + 16, NULL);
        // b.ne
        if ((i2 & 0xFF00001Fu) == 0x54000001u && (i3 & 0x3F400000u) == 0x08000000u && ((i3 >> 30) & 3) == sz &&
            ((i3 >> 10) & 0x1F) == 0x1F && ((i3 >> 5) & 31) == Xn && (i4 & 0xFF000000u) == 0x35000000u &&
            (i4 & 31) == ((i3 >> 16) & 31) &&
            // cbnz -> loop
            (gpc + 16 + (uint64_t)(sext((i4 >> 5) & 0x7FFFF, 19) << 2)) == gpc
            // b.ne -> out
            && (gpc + 8 + (uint64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2)) == gpc + 20) {
            int Wnew = i3 & 31;
            // casal carries the compare/old value in Wt, so Wt must differ from Wexp (a stolen Wt flows
            // through its cpu slot across the three ops). The bare ldxr/stxr fallback would spin on a stolen
            // operand / low non-PIE [Xn], so route every part through emit_atomic_part.
            if (Wt == Wexp) return 0;
            uint32_t szd = sz == 3 ? 0x80000000u : 0;
            // mov Wt, Wexp (orr Wt, wzr, Wexp): Rd=Wt[0], Rm=Wexp[16]
            emit_atomic_part(0x2A000000u | szd | (Wexp << 16) | (31 << 5) | Wt, 1 | 4, 0);
            // casal Wt, Wnew, [Xn]; Wt = old:  Rs=Wt[16], Rn=Xn[5], Rt=Wnew[0]
            emit_atomic_part((sz == 3 ? 0xC8E0FC00u : 0x88E0FC00u) | (Wt << 16) | (Xn << 5) | Wnew, 1 | 2 | 4 | 8, 1);
            // cmp Wt, Wexp (reproduce NZCV): subs wzr, Wt, Wexp -> Rn=Wt[5], Rm=Wexp[16]
            emit_atomic_part(0x6B00001Fu | szd | (Wexp << 16) | (Wt << 5), 2 | 4, 0);
            // The guest loop reaches `stxr Ws` only when the compare matched; on the b.ne-out path Ws
            // keeps its pre-loop value. Reproduce both with a csel off the NZCV just recomputed.
            {
                int Ws = (i3 >> 16) & 31;
                // 64-bit csel: the not-taken path must preserve the FULL guest register (a 32-bit csel
                // would zero the top half of an untouched Ws); the taken path selects xzr, and stxr's
                // W-sized status write is zero-extending anyway, so 0 is right for it too.
                if (Ws != 31)
                    emit_atomic_part(0x9A800000u | ((uint32_t)Ws << 16) | (31u << 5) | (uint32_t)Ws, 1 | 2 | 4, 0);
            }
            g_lse_n++;
            return 20;
        }
    }
    return 0;
}

// ---- LSE outline-atomic call inlining ----
// GCC/LLVM emit every C atomic as a `bl __aarch64_<op><sz>_<order>` outline helper (the distro/musl and
// -mno-outline-atomics-ignored toolchains still do). The helper is a fixed 5-insn leaf:
//     adrp x16,#page ; ldrb w16,[x16,#off] ; cbz w16, Lfallback ; <host LSE op> ; ret   Lfallback: ldxr/stxr..
// The gated byte is __aarch64_have_lse_atomics -- ALWAYS 1 here (we advertise HWCAP_ATOMICS and the host
// has FEAT_LSE), so the fast-path single LSE op IS the architectural effect of the call. Inline that one
// op at the call site: elide the bl + adrp/ldrb/cbz + ret AND the block-split/return dispatch (the call
// idiom is ~2 helper round-trips per atomic in tight code -- the dominant hl-vs-native atomics tax, since
// the LSE op itself already lowers 1:1). The op reads/writes guest memory with its native [Xn] base, so
// inline ONLY when an in-stream copy of that op would be emitted verbatim too: guestbase off (PIE/static-
// PIE) and BUS inactive. Otherwise fall through to the normal call (the helper still runs correctly).
// Returns 1 if it inlined (caller advances past the bl and keeps the block going), else 0.
static int try_inline_outline_atomic(uint64_t gpc, uint64_t target) {
    /*
     * This optimization embeds an instruction read from an out-of-line helper
     * in the caller translation.  The initial SMC prime removes all such
     * pre-SMC callers; do not create new hidden source dependencies once map
     * entries are individually invalidatable.
     */
    if (smc_seen() || guestbase_on() || jit_guest_bus_active()) return 0;
    uint32_t i0 = a64_fetch_instruction(target, NULL), i1 = a64_fetch_instruction(target + 4, NULL);
    uint32_t i2 = a64_fetch_instruction(target + 8, NULL), i3 = a64_fetch_instruction(target + 12, NULL);
    uint32_t i4 = a64_fetch_instruction(target + 16, NULL);
    // adrp x16, #page
    if ((i0 & 0x9F00001Fu) != 0x90000010u) return 0;
    // ldrb w16, [x16, #imm12]
    if ((i1 & 0xFFC003FFu) != 0x39400210u) return 0;
    // cbz w16, Lfallback  (byte==0 -> ldxr/stxr fallback; byte!=0 falls through to the LSE op i3)
    if ((i2 & 0xFF00001Fu) != 0x34000010u) return 0;
    // the cbz must jump PAST i3 (forward, skipping the fast-path op) so the fall-through reaches i3
    if ((int64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2) < 8) return 0;
    // i4 = ret x30 (the helper is a leaf; x30 is preserved across it)
    if (i4 != 0xD65F03C0u) return 0;
    // i3 must be a single-[Xn]-base LSE atomic memory op (LDADD/SWP/LDSET/...) or a CAS (single).
    int is_lse = (i3 & 0x3F200C00u) == 0x38200000u;
    int is_cas = (i3 & 0x3FA07C00u) == 0x08A07C00u;
    if (!is_lse && !is_cas) return 0;
    // no stolen operand (Rs[20:16], Rn[9:5], Rt[4:0]) -> the op is safe to copy verbatim
    if (is_stolen(i3 & 31) || is_stolen((i3 >> 5) & 31) || is_stolen((i3 >> 16) & 31)) return 0;
    // architectural x30 after the (elided) bl+ret is the return address; set it so a later reader / signal
    // / unwinder sees exactly what the real call would have left. (Un-biased low vaddr for non-PIE; identity
    // for PIE -- but guestbase is off here anyway.)
    emit_set_x30(pcrel_base(gpc) + 4);
    emit32(i3);
    g_lse_n++;
    return 1;
}

// ---- tier-2 substrate: the purity gate (the analyze() of trace_pipeline.c) ----
// Given a formed trace's instructions, return 1 only if it is safe to MEMOIZE:
// no syscall (svc) and no memory access at all -- so the result is fully determined
// by the input registers and there are no side effects. Conservative by construction:
// any load/store or syscall -> impure -> emit unoptimized (side effects must run).
// This is the gate that refuses the impure region in the pipeline (a wrong gate here
// is a miscompile). Linear in trace length, run once on promotion. Verified by
// TIER2_SELFTEST; wired into specialization when trace formation (the "form trace"
// step) lands -- the remaining substrate brick.
static int region_pure(const uint32_t *code, int n) {
    for (int i = 0; i < n; i++) {
        uint32_t in = code[i];
        // svc -> side effect
        if (in == 0xD4000001u) return 0;
        // any load/store -> not register-determined
        if ((in & 0x0A000000u) == 0x08000000u) return 0;
    }
    // pure: register-to-register computation only
    return 1;
}

// ---- §B shadow-stack return prediction: the validated mechanism (PoC: shadow_stack.c) ----
// At a guest `bl`, record the guest return address. At a guest `ret`, classify the guest's x30:
//   FAST    -> matches the top of the shadow stack: the normal return; take a host `ret` (the
//              hardware RAS predicts it in ~1 insn instead of the ~14-insn ret-IBTC).
//   UNWIND  -> matches a deeper frame (longjmp / multi-frame return): pop to it, still correct.
//   FOREIGN -> not on the shadow (computed/tail return): fall back to the IBTC.
// Conservative: ONLY the FAST path takes the host ret; UNWIND/FOREIGN fall back, so a return can
// never land at the wrong target. The codegen that emits host bl/ret + the x30 steal wires onto
// this (the one subtlety past the PoC is x30's dual role: host return address vs guest-visible
// link value -- handled by keeping guest x30 in cpu->x[30] and validating here).
enum { SS_FAST, SS_UNWIND, SS_FOREIGN };

static inline void shadow_push(struct cpu *c, uint64_t guest_ret, uint64_t host_ret) {
    if (c->ssp < 1024) {
        c->sstk[2 * c->ssp] = guest_ret;
        c->sstk[2 * c->ssp + 1] = host_ret;
        c->ssp++;
    }
}

// matches on guest_ret (even index)
static int shadow_classify(struct cpu *c, uint64_t guest_x30) {
    if (c->ssp > 0 && c->sstk[2 * (c->ssp - 1)] == guest_x30) {
        c->ssp--;
        return SS_FAST;
    }
    for (uint64_t d = 2; d <= c->ssp && d <= 64; d++)
        if (c->sstk[2 * (c->ssp - d)] == guest_x30) {
            c->ssp -= d;
            return SS_UNWIND;
        }
    return SS_FOREIGN;
}

// ---- opt4: greedy superblock / trace formation ----
// Follow unconditional `b` edges INLINE, and lay conditional fall-through successors INLINE
// (inverting the guest condition so the TAKEN side becomes a tiny out-of-line chain-exit).
// A region is bounded to TRACE_MAX_BYTES / TRACE_MAX_BLK; intermediate guest block-starts are
// deliberately NOT registered in g_map -- any edge that later enters mid-region self-heals by
// re-translating a fresh (always-correct) duplicate, wired up through the existing
// add_pend/patch_links_to back-patch path. NOSTITCH=1 -> g_stitch=0 -> exact single-block
// baseline (env read once; set-once + idempotent under the JIT lock).
#define TRACE_MAX_BLK 16
#define TRACE_MAX_BYTES (16 * 1024)
static int g_stitch = -1;

static int seen_has(const uint64_t *seen, int n, uint64_t v) {
    for (int i = 0; i < n; i++)
        if (seen[i] == v) return 1;
    return 0;
}

// Lay a conditional's fall-through inline: `inv` is the branch insn with its condition/op
// already inverted, so when the guest would NOT take it we keep falling through. Emit the
// inverted branch (skips the taken-side exit), the taken chain-exit, then patch the branch to
// jump just past it. The patched offset is always tiny (the taken exit is ~1 insn if chained,
// ~30 if it spills) -> in range even for tbz/tbnz's 14-bit field.
static void stitch_cond(uint32_t inv, uint64_t taken) {
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    emit_chain_exit(taken);
    *patch = recode_cond(inv, ((uint8_t *)g_cp - (uint8_t *)patch) / 4);
}

static int smc_disabled(void) {
    return 0;
}

static void *translate_block(uint64_t gpc);
static uint64_t g_last_guest_start;
static uint64_t g_last_guest_end;

static void smc_queue_line(struct cpu *c, uint64_t address) {
    /*
     * ET_EXEC code is mapped at a high collision-avoidance bias while its
     * architectural pointers remain link-time-low.  Translation-map source
     * intervals use the real executable address, so normalize an ic-ivau
     * operand exactly like instruction dispatch does before classifying it.
     */
    if (g_nonpie_lo && address >= g_nonpie_lo && address < g_nonpie_hi) address += g_nonpie_bias;
    uint64_t start = address & ~UINT64_C(63), end = start + 64;
    for (uint32_t i = 0; i < c->smc_range_count; i++) {
        if (end < c->smc_ranges[i][0] || start > c->smc_ranges[i][1]) continue;
        if (start < c->smc_ranges[i][0]) c->smc_ranges[i][0] = start;
        if (end > c->smc_ranges[i][1]) c->smc_ranges[i][1] = end;
        return;
    }
    if (c->smc_range_count == SMC_RANGE_CAP) {
        c->smc_range_overflow = 1;
        return;
    }
    c->smc_ranges[c->smc_range_count][0] = start;
    c->smc_ranges[c->smc_range_count][1] = end;
    c->smc_range_count++;
}

/*
 * Syscall copy-to-user writes bypass translated store sites.  Queue the
 * guest-visible destination now; logical executable aliases are added by the
 * immutable alias visitor when present, while architectural publication still
 * occurs at the guest's ic-ivau/isb boundary.
 */
static void aarch64_smc_queue_range(uint64_t first, uint64_t last, void *opaque) {
    struct cpu *c = opaque;
    for (uint64_t line = first & ~UINT64_C(63); line < last;) {
        smc_queue_line(c, line);
        if (line > UINT64_MAX - 64) break;
        line += 64;
    }
}

static void aarch64_smc_copyout(uint64_t first, uint64_t last) {
    if (last <= first) return;
    struct cpu *c = pthread_getspecific(g_cpu_key);
    if (c == NULL) return;
    aarch64_smc_queue_range(first, last, c);
    hl_logical_vma_visit_exec_aliases(first, last, aarch64_smc_queue_range, c);
}

/* Return 1 to retry the instruction, 0 for a guest protection fault. */
static int aarch64_soft_tlb_miss(struct cpu *c) {
    void *host = NULL;
    size_t contiguous = 0;
    uint32_t protection = HL_LOGICAL_VMA_READ | HL_LOGICAL_VMA_WRITE | HL_LOGICAL_VMA_EXEC;
    int resolved = hl_logical_vma_resolve_data(c->soft_ea, 1, (uint32_t)c->soft_required, &host, &contiguous);
    if (resolved < 0) return 0;
    uint64_t first, last;
    if (resolved) {
        hl_logical_vma_snapshot *snapshot =
            atomic_load_explicit(hl_logical_vma_global_snapshot_source(), memory_order_acquire);
        const hl_logical_vma_view *view = NULL;
        if (snapshot != NULL)
            for (size_t index = 0; index < snapshot->count; ++index)
                if (c->soft_ea >= snapshot->views[index].guest_first &&
                    c->soft_ea < snapshot->views[index].guest_last) {
                    view = &snapshot->views[index];
                    break;
                }
        if (view == NULL) return 0;
        first = view->guest_first;
        last = view->guest_last;
        protection = view->protection;
    } else {
#if !defined(__APPLE__)
        if (!hl_host_range_mapped((uintptr_t)c->soft_ea, 1)) return 0;
#endif
        first = c->soft_ea & ~UINT64_C(4095);
        last = first + UINT64_C(4096);
    }
    if (c->soft_bytes == 0 || c->soft_bytes > last - first || c->soft_ea > last - c->soft_bytes) {
        c->reason = R_SOFTSPAN;
        return 1;
    }
    c->soft_page = first;
    c->soft_limit = last;
    c->soft_delta = resolved ? (uint64_t)(uintptr_t)host - c->soft_ea : 0;
    c->soft_protection = protection;
    c->reason = R_BRANCH;
    return 1;
}

/*
 * Return 1 when the complete cross-page access has one host delta and can be
 * retried natively, 0 for a protection fault, and -1 when valid adjacent
 * guest pages have discontinuous canonical storage (the architectural split
 * slow path must handle that case).
 */
static int aarch64_soft_span_copy(struct cpu *c, int to_guest, int copy_bytes) {
    uint64_t cursor = c->soft_ea;
    size_t done = 0;
    while (done < c->soft_bytes) {
        void *host = NULL;
        size_t contiguous = 0;
        uint32_t required = to_guest ? HL_LOGICAL_VMA_WRITE : HL_LOGICAL_VMA_READ;
        int resolved = hl_logical_vma_resolve_data(cursor, 1, required, &host, &contiguous);
        if (resolved < 0) return 0;
        if (!resolved) {
            size_t page_left = 4096u - (size_t)(cursor & 4095u);
            if (!hl_host_range_mapped((uintptr_t)cursor, 1)) return 0;
            host = (void *)(uintptr_t)cursor;
            contiguous = page_left;
        }
        size_t take = (size_t)c->soft_bytes - done;
        if (take > contiguous) take = contiguous;
        if (!take) return 0;
        if (copy_bytes) {
            if (to_guest)
                memcpy(host, c->soft_bounce + done, take);
            else
                memcpy(c->soft_bounce + done, host, take);
        }
        cursor += take;
        done += take;
    }
    return 1;
}

static int aarch64_soft_prepare_bounce(struct cpu *c) {
    int ok = 0;
    uint32_t in = a64_fetch_instruction(c->soft_pc, &ok);
    int single = (in & 0x3B000000u) == 0x39000000u || (in & 0x3B200000u) == 0x38000000u;
    int pair = (in & 0x3A000000u) == 0x28000000u;
    int structure = is_advsimd_struct(in);
    int literal = (in & 0xBF000000u) == 0x18000000u || (in & 0xFF000000u) == 0x98000000u ||
                  ((in & 0x3F000000u) == 0x1C000000u && ((in >> 30) & 3) != 3);
    int atomic = (in & 0x3B200C00u) == 0x38200000u || (in & 0x3F000000u) == 0x08000000u || is_casp(in);
    if (!ok || !(single || pair || structure || literal) || atomic || c->soft_bytes == 0 ||
        c->soft_bytes > sizeof(c->soft_bounce))
        return -1;
    int write = (c->soft_required & HL_LOGICAL_VMA_WRITE) != 0;
    if (!aarch64_soft_span_copy(c, write, 0)) return 0; /* validate every span first */
    if (!write && !aarch64_soft_span_copy(c, 0, 1)) return 0;
    /*
     * No asynchronous signal may observe the architectural store after it
     * changed the bounce but before scatter.  This is a cold discontinuous
     * path: block host delivery across the single-instruction retry and
     * restore the exact prior mask in R_SOFTCOMMIT.  Synchronous faults remain
     * deliverable, but the validated aligned bounce access cannot fault.
     */
    sigset_t all;
    sigfillset(&all);
    _Static_assert(sizeof(sigset_t) <= sizeof(c->soft_bounce_host_mask), "soft bounce host-mask storage is too small");
    if (pthread_sigmask(SIG_BLOCK, &all, (sigset_t *)(void *)c->soft_bounce_host_mask) != 0) return 0;
    c->soft_bounce_write = (uint64_t)write;
    c->soft_bounce_pending = 1;
    c->soft_page = c->soft_ea;
    c->soft_limit = c->soft_ea + c->soft_bytes;
    c->soft_delta = (uint64_t)(uintptr_t)c->soft_bounce - c->soft_ea;
    c->soft_protection = c->soft_required;
    c->reason = R_BRANCH;
    return 1;
}

static int aarch64_soft_bounce_commit(struct cpu *c) {
    if (!c->soft_bounce_pending) return 1;
    int ok = !c->soft_bounce_write || aarch64_soft_span_copy(c, 1, 1);
    if (ok && c->soft_bounce_write) aarch64_smc_copyout(c->soft_ea, c->soft_ea + c->soft_bytes);
    c->soft_bounce_pending = 0;
    c->soft_page = UINT64_MAX;
    c->soft_protection = 0;
    (void)pthread_sigmask(SIG_SETMASK, (const sigset_t *)(const void *)c->soft_bounce_host_mask, NULL);
    return ok;
}

static int aarch64_soft_tlb_span(struct cpu *c) {
    if (c->soft_bytes == 0 || c->soft_ea > UINT64_MAX - c->soft_bytes) return 0;
    uint64_t cursor = c->soft_ea, last = cursor + c->soft_bytes, delta = 0;
    int have_delta = 0;
    while (cursor < last) {
        void *host = NULL;
        size_t contiguous = 0;
        int resolved = hl_logical_vma_resolve_data(cursor, 1, (uint32_t)c->soft_required, &host, &contiguous);
        if (resolved < 0) return 0;
        uint64_t part_delta = resolved ? (uint64_t)(uintptr_t)host - cursor : 0;
        if (have_delta && part_delta != delta) return aarch64_soft_prepare_bounce(c);
        delta = part_delta;
        have_delta = 1;
        uint64_t step = UINT64_C(4096) - (cursor & UINT64_C(4095));
        if (resolved && contiguous < step) step = contiguous;
        if (last - cursor < step) step = last - cursor;
        if (step == 0) return 0;
        cursor += step;
    }
    c->soft_page = c->soft_ea;
    c->soft_limit = c->soft_ea + c->soft_bytes;
    c->soft_delta = delta;
    c->soft_protection = c->soft_required;
    c->reason = R_BRANCH;
    return 1;
}

static int smc_commit(struct cpu *c) {
    pthread_mutex_lock(&g_jit_lock);
    txln_activate();                // arm eager line recording; may request a priming wholesale drop
    int force_whole = g_txln_prime; // first SMC after lazy activation: no lines recorded -> can't classify
    g_txln_prime = 0;
    if (!force_whole && !c->smc_range_count && !c->smc_range_overflow) {
        pthread_mutex_unlock(&g_jit_lock);
        return 1;
    }
    __atomic_store_n(&g_smc_seen, 1, __ATOMIC_RELEASE);
    if (!c->smc_range_overflow && !force_whole) {
        uint32_t retained = 0;
        for (uint32_t i = 0; i < c->smc_range_count; i++) {
            uint64_t dirty_start = UINT64_MAX, dirty_end = 0;
            for (uint64_t line = c->smc_ranges[i][0]; line < c->smc_ranges[i][1]; line += 64) {
                int classification = txln_flush_class(line);
                int warm_source = 0;
                if (classification == 0 && g_pcache_loaded) {
                    for (uint32_t n = 0; n < g_live_map_count; n++) {
                        uint32_t index = g_live_map_indices[n];
                        if (map_live(index) && map_source_overlaps(index, line, line + 64)) {
                            warm_source = 1;
                            break;
                        }
                    }
                }
                if (classification == 1 || warm_source) {
                    if (dirty_start == UINT64_MAX) dirty_start = line;
                    dirty_end = line + 64;
                }
            }
            if (dirty_start != UINT64_MAX) {
                c->smc_ranges[retained][0] = dirty_start;
                c->smc_ranges[retained][1] = dirty_end;
                retained++;
            }
        }
        c->smc_range_count = retained;
        if (!retained) {
            pthread_mutex_unlock(&g_jit_lock);
            c->smc_range_count = 0;
            c->smc_range_overflow = 0;
            return 1;
        }
    }
    pthread_mutex_unlock(&g_jit_lock);
    /*
     * Do not rewrite live map entries in place here.  Besides leaving several
     * independent ingress paths to the old body (direct chains, shadow
     * returns and per-site ICs), recompiling every overlapping entry can emit
     * an unbounded amount of code during one dispatcher crossing.  Large JITs
     * such as Julia reached the end of the writable alias and the assembler
     * then faulted on the adjacent RX alias.
     *
     * All peers are brought to a dispatcher boundary, so invalidating every
     * lookup/chain is both simpler and coherent.  The old bytes remain mapped
     * and untouched until the ordinary capacity rotation retires them; no
     * executing host PC is invalidated.  Subsequent entries translate the
     * modified guest bytes on demand.
     */
    stw_mapping_begin();
    uint32_t removed;
    if (force_whole || c->smc_range_overflow) {
        removed = g_live_map_count;
        map_clear();
        memset(g_ibtc, 0, sizeof g_ibtc);
        txpg_clear();
    } else {
        removed = map_invalidate_source_ranges((const uint64_t (*)[2])c->smc_ranges, c->smc_range_count);
    }
    pend_reset();
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_relaxed) && g_stw_threads[i].cpu)
            g_stw_threads[i].cpu->ssp = 0;
    HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT, "smc invalidate mode=%s ranges=%u removed=%u retained=%u",
            (force_whole || c->smc_range_overflow) ? "whole" : "targeted", c->smc_range_count, removed,
            g_live_map_count);
    g_smc_flushes++;
    stw_mapping_end();
    c->smc_range_count = 0;
    c->smc_range_overflow = 0;
    return 1;
}

// A guest `ic ivau` reached the dispatcher (R_ICFLUSH): the guest is about to execute code it just rewrote,
// so every gpc->host translation may be stale. Drop the whole block map + IBTC + pending chains (mirrors the
// x86 smc_on_write flush). We deliberately do NOT reset g_cp: the just-exited block's host code stays intact
// and is reclaimed by the normal wholesale flush; stale entries are simply re-emitted on demand. The §B
// shadow stack is left alone -- its host_rets point at old code that is still present in g_cp (valid targets).
// g_smc_seen latches so indirect branches stop populating the per-site IC (see G_IBTC_FILL): that literal
// lives in the unmodified CALLER block, which this flush cannot reach.
static void smc_icflush(struct cpu *c, uint64_t va) {
    // The guest issued `ic ivau` -> it generates/patches code -> the per-site monomorphic IC stays disabled
    // (its literal lives in an unmodified caller block this flush can't reach). Latch this unconditionally,
    // even when the precise gate below skips, so a code-modifying guest never trusts the per-site IC.
    __atomic_store_n(&g_smc_seen, 1, __ATOMIC_RELEASE);
    // PRECISE GATE: if the invalidated bytes were never translated, there is nothing stale to drop. A
    // code-generating guest flushes each freshly-written line as it grows its code space -> almost always
    // brand-new bytes -> this turns the catastrophic per-flush wholesale invalidation (which re-translated
    // the entire working set on every `ic ivau`) into a no-op. Gate at CACHE-LINE (64B) granularity -- the
    // exact unit `ic ivau, Xt` invalidates -- NOT at 4KB page granularity. BeamAsm (Erlang/OTP's
    // arm64 JIT) packs many compiled functions per page, so appending a NEW function onto a page that
    // already holds a translated one makes a page-granular gate fire a wholesale drop even though no
    // translated byte changed. Re-translating the whole working set on that spurious drop -- and, before the
    // thread-safety fix below, doing it unlocked -- crashed the heavily-threaded emulator. The line gate
    // makes those same-page appends a no-op (measured: 100% of BeamAsm's page-hit drops are line-misses),
    // while a genuine in-place overwrite (V8 patching a jump) still overlaps a translated line -> real drop.
    // pcache warm-load restores blocks with page info but no line info (see pcache.c), so for a restored
    // arena fall back to the coarse page gate -- conservative (may over-drop) but never misses stale code.
    // CONTENT GATE: classify the flush by whether the invalidated
    // translated line's bytes actually CHANGED. A benign re-flush of unchanged already-translated code
    // (a builtin/trampoline flushed as part of a range each call, or a block flushing its OWN executing
    // source line -- exactly what V8 does thousands of times at startup) must NOT trigger the wholesale
    // drop, or the entire working set re-translates on every flush and translate_block spins forever.
    //   class 0 -> line never translated: nothing stale (fall back to the coarse pcache page gate below).
    //   class 2 -> translated but UNCHANGED: benign icache maintenance -> keep the valid translation, skip.
    //   class 1 -> translated AND (first flush | genuinely rewritten): take the real drop (soak_smc/V8 patch).
    /*
     * Queue only.  smc_commit owns activation, membership/content
     * classification and hash mutation under g_jit_lock.  Besides avoiding a
     * race with translation, this ensures a changed line is classified once
     * rather than immediately looking unchanged on a second observation.
     */
    // ---- a GENUINE in-place modification of already-translated guest code (the line WAS a source line) ----
    // (BeamAsm SIGSEGV) coherence. smc_icflush runs from the dispatcher's post-run reason handler, which
    // has ALREADY released g_jit_lock (engine/dispatch.c: the unlock precedes G_DISPATCH_REASON), so a peer
    // guest thread may be executing translated code concurrently. A wholesale drop memsets g_map/g_ibtc that a
    // peer reads lock-free AND forces a re-translation of the modified bytes -- and there is no way to make
    // that coherent while other threads run. Two approaches were measured and BOTH fault BeamAsm:
    //   * stop-the-world + fresh cache (jit_flush_to_fresh): parked peers resume in the RETIRED cache running
    //     the STALE translation while freshly-dispatched threads run the RE-translated code -> two live
    //     versions of the modified function at once.
    //   * stop-the-world + in-place drop (keep g_cp): the old arena stays mapped, so a resuming peer follows
    //     baked-in direct chains straight into stale old blocks -> same two-version split.
    // The split is what an async/dirty scheduler thread trips over the instant it re-enters a modified region.
    // The coherent choice under live peers is to keep the SINGLE existing translation for EVERY thread and NOT
    // re-translate: g_smc_seen (latched above) already disables the per-site monomorphic IC so a code-
    // modifying guest never trusts a baked body, and the guest re-synchronizes through the shared indirect
    // dispatch. This matches hl's long-standing NOSMC fallback and is exactly what lets Erlang/OTP + Elixir
    // (BeamAsm) run to completion, including external-program ports (os:cmd / open_port {spawn,...}) whose
    // forker relies on the emulator staying alive. Fully coherent re-translation of a multithreaded
    // in-place patch would need precise per-block recompile+redirect with all peers rendezvoused at a
    // safepoint (the tier2_promote bounce, generalized) -- out of scope here; a guest that depends on such a
    // patch keeps running the prior version instead of crashing. The LINE-granular gate above keeps genuine
    // in-place hits rare (a code-generator that merely APPENDS onto a shared page never reaches here), so this
    // fallback is taken only on a true overwrite of executed code.
    // Single-threaded (incl. all peers exited): the wholesale in-place drop IS coherent -- one thread, no
    // split -- so re-translate for correct self-modification (a single-isolate V8, the soak_smc test).
    smc_queue_line(c, va);
}

static void emit_smc_queue(int va_register) {
    assert(g_steal1617);
    if (is_stolen(va_register))
        e_ldr(16, CPUREG, va_register * 8);
    else
        e_movr(16, va_register);
    emit32(0x927AE610u); /* and x16,x16,#-64 */
    e_str(16, CPUREG, OFF_SMCVA);
    e_ldr(16, CPUREG, OFF_SMC_RANGE_COUNT);
    uint32_t *empty = (uint32_t *)g_cp;
    emit32(0); /* cbz x16,append */
    e_subi(16, 16, 1);
    e_addlsl4(17, CPUREG, 16);
    unsigned offset = (unsigned)OFF_SMC_RANGES;
    if (offset >= 4096) {
        emit32(0x91400000u | (((offset >> 12) & 0xfffu) << 10) | (17u << 5) | 17u);
        offset &= 0xfffu;
    }
    if (offset) e_addi(17, 17, offset);
    e_ldr(17, 17, 8);
    e_ldr(16, CPUREG, OFF_SMCVA);
    emit32(0xCB110211u); /* sub x17,x16,x17 */
    uint32_t *not_adjacent = (uint32_t *)g_cp;
    emit32(0); /* cbnz x17,append */
    e_ldr(16, CPUREG, OFF_SMC_RANGE_COUNT);
    e_subi(16, 16, 1);
    e_addlsl4(17, CPUREG, 16);
    offset = (unsigned)OFF_SMC_RANGES;
    if (offset >= 4096) {
        emit32(0x91400000u | (((offset >> 12) & 0xfffu) << 10) | (17u << 5) | 17u);
        offset &= 0xfffu;
    }
    if (offset) e_addi(17, 17, offset);
    e_ldr(16, CPUREG, OFF_SMCVA);
    e_addi(16, 16, 64);
    e_str(16, 17, 8);
    uint32_t *extended = (uint32_t *)g_cp;
    emit32(0); /* b done */

    uint8_t *append = g_cp;
    e_ldr(16, CPUREG, OFF_SMC_RANGE_COUNT);
    e_subi(17, 16, SMC_RANGE_CAP);
    uint32_t *overflow = (uint32_t *)g_cp;
    emit32(0); /* cbz x17,overflow */
    e_addlsl4(17, CPUREG, 16);
    offset = (unsigned)OFF_SMC_RANGES;
    if (offset >= 4096) {
        emit32(0x91400000u | (((offset >> 12) & 0xfffu) << 10) | (17u << 5) | 17u);
        offset &= 0xfffu;
    }
    if (offset) e_addi(17, 17, offset);
    e_ldr(16, CPUREG, OFF_SMCVA);
    e_str(16, 17, 0);
    e_addi(16, 16, 64);
    e_str(16, 17, 8);
    e_ldr(16, CPUREG, OFF_SMC_RANGE_COUNT);
    e_addi(16, 16, 1);
    e_str(16, CPUREG, OFF_SMC_RANGE_COUNT);
    uint32_t *skip = (uint32_t *)g_cp;
    emit32(0); /* b done */
    uint8_t *overflow_body = g_cp;
    e_movconst(16, 1);
    e_str(16, CPUREG, OFF_SMC_RANGE_OVERFLOW);
    uint8_t *done = g_cp;
    *empty = 0xB4000000u | (((uint32_t)((append - (uint8_t *)empty) / 4) & 0x7ffffu) << 5) | 16u;
    *not_adjacent = 0xB5000000u | (((uint32_t)((append - (uint8_t *)not_adjacent) / 4) & 0x7ffffu) << 5) | 17u;
    *extended = 0x14000000u | ((uint32_t)((done - (uint8_t *)extended) / 4) & 0x03ffffffu);
    *overflow = 0xB4000000u | (((uint32_t)((overflow_body - (uint8_t *)overflow) / 4) & 0x7ffffu) << 5) | 17u;
    *skip = 0x14000000u | ((uint32_t)((done - (uint8_t *)skip) / 4) & 0x03ffffffu);
}

// async-interrupt poll: emit a CHEAP flag-free check of cpu->irq at the block body entry (the target
// of every fall-through, direct chain `b body`, self-loop fold, tier-1 back-edge, and IBTC hit). When irq
// is set (a caught async guest signal became pending while spinning in-cache with no syscalls), exit the
// block to the dispatcher at a safe boundary -- all guest regs are live in host regs here, so the standard
// emit_exit_const spill materializes consistent guest state and maybe_deliver_signal builds the sigframe
// exactly as the syscall-boundary path does. Fast path is ldr+cbz (2 insns); cbz never touches NZCV, so a
// self-loop back-edge that lands here keeps the guest condition flags. x16 is engine scratch (dead at body
// entry when x16/x17 are stolen -- the default), so no guest reg is disturbed; the legacy NOSTEAL1617 path
// spills x9 to the red zone instead. `gpc` is the block start = the guest pc to resume at.
// IRQSLIM: when active (g_fwdskip == 8; aarch64 steal-mode default), the poll is emitted as a FIXED
// 2-insn header (ldr + cbnz to an out-of-line exit stub at the end of the block), so a forward direct
// chain can enter at body+8 and skip it -- every cycle still polls through its backward or indirect
// edge (see the g_fwdskip invariant note in cache.c). g_irq_patch carries the cbnz to the end-of-block
// stub emitter (emit_irq_stub). NOIRQSLIM=1 -> the legacy inline poll on every entry, chains to body+0.
static uint32_t *g_irq_patch;

static void emit_irq_check(uint64_t gpc) {
    (void)gpc;
    if (g_fwdskip) {
        e_ldr(16, CPUREG, OFF_IRQ); // ldr x16, [x28, #irq]
        g_irq_patch = (uint32_t *)g_cp;
        emit32(0); // cbnz x16, Lirq (the out-of-line exit stub; patched by emit_irq_stub)
        return;
    }
    if (g_steal1617) {
        e_ldr(16, CPUREG, OFF_IRQ); // ldr x16, [x28, #irq]
        uint32_t *p = (uint32_t *)g_cp;
        emit32(0); // cbz x16, Lcont  (patched below)
        emit_exit_const(gpc, R_BRANCH);
        uint8_t *cont = g_cp;
        *p = 0xB4000000u | (((uint32_t)(((uint8_t *)cont - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16;
    } else {
        e_stur(9, 31, -16); // save guest x9 to the red zone
        e_ldr(9, CPUREG, OFF_IRQ);
        uint32_t *p = (uint32_t *)g_cp;
        emit32(0);          // cbz x9, Lcont
        e_ldur(9, 31, -16); // restore guest x9 before the exit (emit_spill saves the real value)
        emit_exit_const(gpc, R_BRANCH);
        uint8_t *cont = g_cp;
        *p = 0xB4000000u | (((uint32_t)(((uint8_t *)cont - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 9;
        e_ldur(9, 31, -16); // Lcont: restore guest x9 and fall into the body
    }
}

// (SIMD-clean syscall exit): SOUND over-approximation of "this guest instruction WRITES a vector
// (V) register." Over-marks read-only vector ops (vector stores, FCMP), the GPR-destination FP conversions
// (FCVTZS/FMOV-to-GPR), and UMOV/SMOV -> that only ever costs the optimization (a full spill), never
// correctness. It covers every V-writing form: the SIMD&FP data-processing box (scalar FP + AdvSIMD, bits
// [27:25]=111), SIMD&FP loads/stores (the V bit, [26]=1, in the load/store box), and AdvSIMD load/store
// STRUCTURES (LD1..LD4). A block containing any of these must take the full V spill on its syscall exit.
static int insn_touches_vreg(uint32_t in) {
    if ((in & 0x0E000000u) == 0x0E000000u) return 1;                     // SIMD&FP data-processing
    if ((in & 0x0A000000u) == 0x08000000u && ((in >> 26) & 1)) return 1; // SIMD&FP load/store (V=1)
    if ((in & 0xBE000000u) == 0x0C000000u) return 1;                     // AdvSIMD load/store structures
    return 0;
}

static int x28_alu_window_classify(uint32_t in, int *mask_out, int *read_out, int *write_out) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    int mask = gpr_field_mask(in), read = 0, write = 0;
    uint32_t op = (in >> 25) & 0xFu;
    if (op == 8 || op == 9) {
        if ((in & 0x1F000000u) == 0x10000000u) return 0;
        if ((in & 0x1F800000u) == 0x12800000u) {
            int opc = (in >> 29) & 3;
            if (opc == 1) return 0;
            write = mask & 1;
            if (opc == 3) read = write;
        } else if ((in & 0x1F800000u) == 0x13800000u) {
            read = mask & (2 | 4);
            write = mask & 1;
        } else if ((in & 0x1F000000u) == 0x11000000u || (in & 0x1F800000u) == 0x12000000u ||
                   (in & 0x1F800000u) == 0x13000000u) {
            read = mask & 2;
            write = mask & 1;
        } else {
            return 0;
        }
    } else if ((in & 0x0E000000u) == 0x0A000000u) {
        read = mask & (2 | 4 | 8);
        write = mask & 1;
    } else {
        return 0;
    }
    for (int k = 0; k < 4; k++)
        if ((mask & mbits[k]) && is_stolen((in >> shifts[k]) & 31) && ((in >> shifts[k]) & 31) != 28) return 0;
    *mask_out = mask;
    *read_out = read;
    *write_out = write;
    return 1;
}

static int x28_alu_window_field(uint32_t in, int fields) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if ((fields & mbits[k]) && ((in >> shifts[k]) & 31) == 28) return 1;
    return 0;
}

static uint32_t x28_alu_window_rewrite(uint32_t in, int mask) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if ((mask & mbits[k]) && ((in >> shifts[k]) & 31) == 28) in = (in & ~(31u << shifts[k])) | (17u << shifts[k]);
    return in;
}

/*
 * Diagnostic ceiling for forwarding the two dominant stolen values through a
 * short straight-line run.  Guest x16 uses the otherwise engine-private host
 * x16 and guest x28 uses host x17; x28 remains CPUREG outside the rewritten
 * instructions.  Writes are published immediately, rather than only at the
 * end of the window, so signal reconstruction and a fault on a later guest
 * instruction always see architecturally committed state in cpu->x[].
 *
 * Deliberately admitted:
 *   - the integer ALU forms audited by x28_alu_window_classify;
 *   - ordinary scalar integer load/store, unsigned-immediate, unscaled,
 *     pre/post-indexed, or register-offset.
 * Deliberately rejected: x17/x18 operands (scratch collision), pairs,
 * atomics/exclusives, SIMD, branches/system instructions, guest-base/folded
 * addressing, tier 2, and active guest-bus/SMC-special translation modes.
 */
static int stolen_forward_classify(uint32_t in, int *mask_out, int *read_out, int *write_out, int *fault_out) {
    int mask, read, write;
    if (x28_alu_window_classify(in, &mask, &read, &write)) {
        *mask_out = mask;
        *read_out = read;
        *write_out = write;
        *fault_out = 0;
        return 1;
    }

    /* Same audited integer-ALU set, but admit guest x16 as well as x28. */
    uint32_t op = (in >> 25) & 0xFu;
    mask = gpr_field_mask(in);
    if (op == 8 || op == 9) {
        if ((in & 0x1F000000u) == 0x10000000u) return 0;
        if ((in & 0x1F800000u) == 0x12800000u) {
            int opc = (in >> 29) & 3;
            if (opc == 1) return 0;
            write = mask & 1;
            read = opc == 3 ? write : 0;
        } else if ((in & 0x1F800000u) == 0x13800000u) {
            read = mask & (2 | 4);
            write = mask & 1;
        } else if ((in & 0x1F000000u) == 0x11000000u || (in & 0x1F800000u) == 0x12000000u ||
                   (in & 0x1F800000u) == 0x13000000u) {
            read = mask & 2;
            write = mask & 1;
        } else {
            goto try_memory;
        }
    } else if ((in & 0x0E000000u) == 0x0A000000u) {
        read = mask & (2 | 4 | 8);
        write = mask & 1;
    } else {
        goto try_memory;
    }
    {
        static const int shifts[4] = {0, 5, 16, 10};
        static const int mbits[4] = {1, 2, 4, 8};
        for (int k = 0; k < 4; k++) {
            if (!(mask & mbits[k])) continue;
            int r = (in >> shifts[k]) & 31;
            if (is_stolen(r) && r != 16 && r != 28) return 0;
        }
    }
    *mask_out = mask;
    *read_out = read;
    *write_out = write;
    *fault_out = 0;
    return 1;

try_memory:
    /* Scalar ordinary single-register loads/stores only. */
    if (in & 0x04000000u) return 0;
    int unsigned_imm = (in & 0x3B000000u) == 0x39000000u;
    int unscaled = (in & 0x3B200000u) == 0x38000000u;
    int regoff = (in & 0x3B200C00u) == 0x38200800u;
    if (!unsigned_imm && !unscaled && !regoff) return 0;
    if (unscaled) {
        int mode = (in >> 10) & 3;
        if (mode == 2) return 0; /* unprivileged: keep uncommon form baseline */
    }

    mask = gpr_field_mask(in);
    int opc = (in >> 22) & 3;
    int size = (in >> 30) & 3;
    if (size == 3 && opc == 2) return 0; /* PRFM */
    int writeback = unscaled && (((in >> 10) & 3) == 1 || ((in >> 10) & 3) == 3);
    int base_index = mask & (2 | 4);
    if (opc == 0) {
        read = mask & (1 | 2 | 4);
        write = writeback ? (mask & 2) : 0;
    } else {
        read = base_index;
        write = (mask & 1) | (writeback ? (mask & 2) : 0);
    }

    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++) {
        if (!(mask & mbits[k])) continue;
        int r = (in >> shifts[k]) & 31;
        if (is_stolen(r) && r != 16 && r != 28) return 0;
    }
    *mask_out = mask;
    *read_out = read;
    *write_out = write;
    *fault_out = 1;
    return 1;
}

static int stolen_forward_field(uint32_t in, int fields, int reg) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if ((fields & mbits[k]) && (int)((in >> shifts[k]) & 31) == reg) return 1;
    return 0;
}

static uint32_t stolen_forward_rewrite(uint32_t in, int mask) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if ((mask & mbits[k]) && ((in >> shifts[k]) & 31) == 28) in = (in & ~(31u << shifts[k])) | (17u << shifts[k]);
    return in;
}

static void emit_cpu_model_value(int rd, uint64_t value) {
    if (is_stolen(rd)) {
        if (stealfast_on()) {
            e_movconst(16, value);
            e_str(16, CPUREG, rd * 8);
        } else {
            x18_prolog();
            e_movconst(0, value);
            e_str(0, 1, rd * 8);
            x18_epilog();
        }
    } else {
        e_movconst(rd, value);
    }
}

// Recognize the compiler's otherwise redundant frame around a direct tail call:
//
//   stp x29,x30,[sp,#-16]!; small register-only setup; ldp x29,x30,[sp],#16; b target
//
// x17 is engine-private in steal mode, so it can carry the architectural LR
// between the real stack store/load.  The stack accesses remain native and at
// their original guest PCs (fault/unwind semantics are unchanged); this only
// avoids repeatedly round-tripping x30 through cpu->x[30] in the generic
// stolen-register mangler.  Keep the recognizer intentionally exact.
static uint64_t scan_tail_x30_carry(uint64_t pc) {
    if (!g_steal1617 || g_noibslim || jit_guest_bus_active()) return 0;
    // The recognizer may inspect the eight setup instructions plus the tail
    // instruction following the restoring LDP.  Refuse to speculate across an
    // unmapped guest boundary.
    if (!hl_host_range_mapped((uintptr_t)pc, 10 * sizeof(uint32_t))) return 0;
    if (a64_fetch_instruction(pc, NULL) != 0xA9BF7BFDu) return 0;
    for (int i = 1; i <= 8; i++) {
        uint32_t in = a64_fetch_instruction(pc + (uint64_t)i * 4, NULL);
        if (in == 0xA8C17BFDu) {
            uint32_t tail = a64_fetch_instruction(pc + (uint64_t)(i + 1) * 4, NULL);
            return (tail & 0xFC000000u) == 0x14000000u ? pc + (uint64_t)i * 4 : 0;
        }
        // No memory, control flow, system, SIMD, PC-relative operation, or
        // stolen guest register may occur while host x17 carries the LR.
        if ((in & 0x0A000000u) == 0x08000000u || (in & 0x7C000000u) == 0x14000000u ||
            (in & 0x1C000000u) == 0x10000000u || (in & 0x0E000000u) == 0x0E000000u || (in & 0xFFC00000u) == 0xD5000000u)
            return 0;
        int mask = gpr_field_mask(in);
        if (uses_x18(in, mask)) return 0;
    }
    return 0;
}

static void *translate_block(uint64_t gpc) {
    /* Observe writes made through another MAP_SHARED alias before decoding
       an executable view backed by an emulated host-page snapshot. */
    uint64_t source_page = gpc & ~UINT64_C(0xfff);
    filemap_refresh_emulated(source_page, source_page + UINT64_C(0x1000));
    // W4E tier-2: read NOTIER2 / TIER2_THRESHOLD once (idempotent) before any self-loop detection.
    tier2_env_init();
    // gpc is mutated by the decode loop; key the cache by START
    uint64_t start = gpc;
    chain_exit_dedup_reset();
    g_bus_stub_patch_count = 0;
    g_soft_stub_patch_count = 0;
    g_soft_legacy_stub_patch_count = 0;
    g_soft_resolver_patch_count = 0;
    uint64_t guest_start = gpc;
    uint64_t guest_end = gpc + 4;
    uint64_t tail_carry_ldp = scan_tail_x30_carry(gpc);
    g_blk_vdirty = 0;     // reset per block; set below when a V-writing insn is emitted
    g_t2_loop_top = NULL; // reset per block; set only in the tier-2 vdirty-hoist path below
    g_t2_irq_patch = NULL;
    void *host = g_cp;
    emit_prologue();
    // Keep the hot chained/IBTC entry stable independently of prologue size.
    // Cold dispatcher entry runs this padding once; hot entries target `body`
    // below and skip it.
    while ((uintptr_t)g_cp & 15)
        emit32(0xD503201Fu);
    // chained jumps land here (regs already live)
    void *body = g_cp;
    // poll cpu->irq at the body entry so a caught async signal reaches a no-syscall guest loop.
    emit_irq_check(start);
    // ldxr/ldaxr..stxr/stlxr exclusive regions must stay in ONE block with no injected
    // memory ops between them, else the monitor clears and stxr retries forever. While
    // inside such a region, conditional branches are emitted inline and their exits are
    // deferred to stubs after the store-exclusive.
    int in_excl = 0;

    struct {
        uint32_t *patch;
        uint64_t target;
        uint32_t in;
    } defer[64];

    int ndefer = 0;
    uint64_t provenance_host = 0;
    uint64_t provenance_guest = 0;
    int provenance_fault_capable = 0;
    // opt4 region state: guest block-starts inlined into this region + a block budget. The
    // region STOPS (falls to the baseline single-block exit) at any dispatcher-mediated edge
    // (indirect br/blr, bl/call, ret, svc/syscall), inside an exclusive monitor region, or on
    // hitting the 16-block / 16 KB bound -- "when unsure, end the region".
    if (g_stitch < 0) g_stitch = 1;
    uint64_t seen[TRACE_MAX_BLK];
    int nseen = 0, trace_blk = 0;
    // opt4 conditional-stitch budget: each conditional fall-through laid inline is a SPECULATION -- the
    // guest may instead take the (chain-exit) branch, leaving the inlined tail dead. Deadness compounds
    // per conditional passed (measured on sqlite: depth-1 fall-throughs 28% never-executed, rising to
    // >85% by the 6th). Unconditional `b` edges follow the guaranteed path and are NOT budgeted, so
    // straight-line/loop-body traces still stitch freely; only chains of hard-to-predict conditionals are
    // cut. Ending a region early is always semantics-preserving: intermediate block-starts are never
    // registered in g_map, so the truncated successor self-heals as an on-demand fresh translation via the
    // ordinary chain-exit path (identical to the NOSTITCH baseline, just re-anchored deeper).
    int ncond = 0;

    struct {
        uint64_t target, resume, retpc, expected_x30;
    } ctx[CTX_INLINE_DEPTH];

    int nctx = 0;
#ifndef STITCH_MAX_COND
#define STITCH_MAX_COND 3
#endif
    // SMC precise gate (line-granular source set): record every 64B guest line this block is actually
    // decoded from, AS WE DECODE, instead of marking the whole contiguous [start,guest_end) hull after the
    // loop (see txpg_mark). For an opt4-stitched superblock the hull also spans the address GAPS between
    // the scattered inlined sub-blocks -- lines that hold no translated code -- so the post-loop hull
    // marked ~15x more lines than the block truly sourced (measured ~29 vs ~2 per block on sqlite), and
    // each txln_put is a cache miss into the 16MB line set. Marking only the decoded lines is a strict
    // subset of the hull yet still a complete superset of the REAL source lines (every translated byte came
    // from a decoded instruction), so txln_flush_class stays correct -- it can never miss a genuinely
    // self-modified source line. Skipped under g_tier2_build (the promoter never marks), matching the
    // post-loop guard.
    uint64_t tx_last_line = ~UINT64_C(0);
#define STITCH_OK                                                                                                      \
    (g_stitch && !smc_seen() && !in_excl && trace_blk < TRACE_MAX_BLK - 1 && ncond < STITCH_MAX_COND &&                \
     (g_cp - (uint8_t *)host) < TRACE_MAX_BYTES)
    for (;;) {
        // A basic block is not necessarily small: generated programs can contain tens of thousands of
        // straight-line instructions before their first control-flow edge.  File-backed BUS guards can
        // expand each guest memory operation into hundreds of host bytes.  Bound normal regions by emitted
        // size so the dispatcher's CACHE_EMIT_HEADROOM admission guarantee remains true.  Splitting at an
        // arbitrary instruction boundary is equivalent to an ordinary chain exit; exclusive sequences are
        // exempt because an injected dispatcher edge would clear the architectural monitor.
        if (!in_excl && ((size_t)(g_cp - (uint8_t *)host) >= CACHE_EMIT_HEADROOM / 2 ||
                         g_bus_stub_patch_count >= BUS_STUB_PATCH_MAX - 4)) {
            emit_chain_exit(gpc);
            break;
        }
        int fetch_ok;
        uint32_t in = a64_fetch_instruction(gpc, &fetch_ok);
        if (!fetch_ok) {
            e_movconst(9, gpc);
            e_str(9, CPUREG, OFF_FAULT_ADDR);
            emit_exit_const(gpc, R_FETCHFAULT);
            break;
        }
        if (stealfast_on() && !g_tier2_build && !in_excl && !guestbase_on() && !jit_guest_bus_active() &&
            !g_nonpie_lo) {
            int mask, read, write, fault;
            if (stolen_forward_classify(in, &mask, &read, &write, &fault) &&
                (stolen_forward_field(in, read, 16) || stolen_forward_field(in, read, 28))) {
                int count = 0, touches = 0, last_touch = -1;
                int need16 = 0, need28 = 0;
                for (; count < 12; count++) {
                    uint32_t win = a64_fetch_instruction(gpc + (uint64_t)count * 4, NULL);
                    int wm, wr, ww, wf;
                    if (!stolen_forward_classify(win, &wm, &wr, &ww, &wf)) break;
                    int r16 = stolen_forward_field(win, wr, 16);
                    int r28 = stolen_forward_field(win, wr, 28);
                    if (r16 || r28) {
                        touches++;
                        last_touch = count;
                        need16 |= r16;
                        need28 |= r28;
                    }
                }
                if (touches >= 3) {
                    int window = last_touch + 1;
                    if (provenance_fault_capable)
                        jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
                    /* Load x16 first: the second load still needs real x28. */
                    if (need16) e_ldr(16, CPUREG, 16 * 8);
                    if (need28) e_ldr(17, CPUREG, 28 * 8);
                    for (int i = 0; i < window; i++) {
                        uint64_t pc = gpc + (uint64_t)i * 4;
                        uint32_t win = a64_fetch_instruction(pc, NULL);
                        int wm, wr, ww, wf;
                        int ok = stolen_forward_classify(win, &wm, &wr, &ww, &wf);
                        assert(ok);
                        uint64_t hstart = (uint64_t)g_cp;
                        emit32(stolen_forward_rewrite(win, wm));
                        if (stolen_forward_field(win, ww, 16)) e_str(16, CPUREG, 16 * 8);
                        if (stolen_forward_field(win, ww, 28)) e_str(17, CPUREG, 28 * 8);
                        if (wf) jit_instruction_map_put(hstart, (uint64_t)g_cp, pc);
                    }
                    if (g_txln_active) {
                        uint64_t last = (gpc + (uint64_t)window * 4 - 1) >> 6;
                        for (uint64_t line = gpc >> 6; line <= last; line++)
                            txln_put(line);
                        tx_last_line = last;
                    }
                    if (gpc < guest_start) guest_start = gpc;
                    gpc += (uint64_t)window * 4;
                    if (gpc > guest_end) guest_end = gpc;
                    provenance_fault_capable = 0;
                    continue;
                }
            }
        }
        if (stealfast_on() && !g_tier2_build && !in_excl && !guestbase_on() && !jit_guest_bus_active() &&
            !g_nonpie_lo) {
            int mask, read, write;
            if (x28_alu_window_classify(in, &mask, &read, &write) && x28_alu_window_field(in, read)) {
                int count = 0, reads = 0, last_read = -1;
                for (; count < 12; count++) {
                    uint32_t win = a64_fetch_instruction(gpc + (uint64_t)count * 4, NULL);
                    int wm, wr, ww;
                    if (!x28_alu_window_classify(win, &wm, &wr, &ww)) break;
                    if (x28_alu_window_field(win, wr)) {
                        reads++;
                        last_read = count;
                    }
                }
                if (reads >= 3) {
                    int window = last_read + 1;
                    if (provenance_fault_capable)
                        jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
                    e_ldr(17, CPUREG, 28 * 8);
                    for (int i = 0; i < window; i++) {
                        uint64_t pc = gpc + (uint64_t)i * 4;
                        uint32_t win = a64_fetch_instruction(pc, NULL);
                        int wm, wr, ww;
                        int ok = x28_alu_window_classify(win, &wm, &wr, &ww);
                        assert(ok);
                        emit32(x28_alu_window_rewrite(win, wm));
                        if (x28_alu_window_field(win, ww)) e_str(17, CPUREG, 28 * 8);
                    }
                    if (g_txln_active) {
                        uint64_t last = (gpc + (uint64_t)window * 4 - 1) >> 6;
                        for (uint64_t line = gpc >> 6; line <= last; line++)
                            txln_put(line);
                        tx_last_line = last;
                    }
                    if (gpc < guest_start) guest_start = gpc;
                    gpc += (uint64_t)window * 4;
                    if (gpc > guest_end) guest_end = gpc;
                    provenance_fault_capable = 0;
                    continue;
                }
            }
        }
        if (!g_tier2_build && g_txln_active) {
            uint64_t tx_ln = gpc >> 6;
            if (tx_ln != tx_last_line) {
                txln_put(tx_ln);
                tx_last_line = tx_ln;
            }
        }
        if (gpc < guest_start) guest_start = gpc;
        if (gpc + 4 > guest_end) guest_end = gpc + 4;
        if (provenance_fault_capable) jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
        provenance_host = (uint64_t)g_cp;
        provenance_guest = gpc;
        uint32_t provenance_major = (in >> 25) & 0xFu;
        provenance_fault_capable =
            ((in & 0x0A000000u) == 0x08000000u) || provenance_major == 0xA || provenance_major == 0xB;
        g_emit_gpc = gpc; // IRQSLIM: tag the current guest PC for the forward/backward edge test in emit_chain_exit
        // at the FIRST vector-touching instruction of the region, store the (nonzero) cpu pointer
        // into cpu->vdirty so a later (possibly chained-to) syscall exit takes the full V spill. Emitted
        // once per region (g_blk_vdirty latch); flag-neutral `str` runs before the vector write. Regions are
        // linear (taken branches exit, only fall-through continues), so the first write dominates all later
        // vector writes -> one store covers every path. Zero cost on vector-free (integer/syscall) blocks.
        if (!g_blk_vdirty && insn_touches_vreg(in)) {
            e_str(CPUREG, CPUREG, (int)OFF_VDIRTY);
            g_blk_vdirty = 1;
            // W4E tier-2 vdirty hoist: only in the promoter recompile, and only when this V-writing
            // insn is the block's FIRST (== the self-loop top). Emit a fresh inline async poll right
            // after the store and record its address so the folded back-edge lands here -- skipping the
            // idempotent store while still polling cpu->irq every iteration (IRQSLIM back-edge invariant).
            // Every block ENTRY still runs the store first: the header poll path (body+0) falls straight
            // in, and a forward chain (body+g_fwdskip) lands exactly on the store. Non-self-loop V-first
            // blocks harmlessly gain one extra entry poll; g_t2_loop_top then goes unused.
            if (g_tier2_build && gpc == start) {
                g_t2_loop_top = g_cp;
                e_ldr(16, CPUREG, OFF_IRQ); // ldr x16, [cpu, #irq]
                g_t2_irq_patch = (uint32_t *)g_cp;
                emit32(0); // cbnz x16, shared out-of-line Lirq
            }
        }

        if (!in_excl) {
            if (tail_carry_ldp && gpc == start) {
                e_ldr(17, CPUREG, 30 * 8);
                emit32((in & ~(31u << 10)) | (17u << 10));
                gpc += 4;
                continue;
            }
            if (tail_carry_ldp && gpc == tail_carry_ldp) {
                emit32((in & ~(31u << 10)) | (17u << 10));
                e_str(17, CPUREG, 30 * 8);
                gpc += 4;
                continue;
            }
            int n = try_lse_atomic(gpc);
            if (n) {
                // try_lse_atomic consumes n bytes (a whole ldxr..stxr sequence) without re-entering the
                // loop top, so mark every source line it spans -- not just gpc's -- keeping the line set a
                // complete superset of the decoded bytes.
                if (!g_tier2_build && g_txln_active)
                    for (uint64_t ll = gpc >> 6; ll <= (gpc + n - 1) >> 6; ll++)
                        txln_put(ll);
                gpc += n;
                continue;
            }
            // ldxr/stxr loop -> LSE
        }
        if (is_i8mm_mmla(in)) {
            emit_i8mm_mmla(in);
            gpc += 4;
            continue;
        }
        if (is_bf16_bfcvt(in)) {
            emit_bf16_bfcvt(in);
            gpc += 4;
            continue;
        }
        if (is_bf16_bfdot(in)) {
            emit_bf16_bfdot(in);
            gpc += 4;
            continue;
        }
        // Load/store-exclusive family is bits[29:24]=001000. The o2 bit (bit23) distinguishes the
        // EXCLUSIVE monitor variants (o2=0: LDXR/LDAXR/STXR/STLXR/LDXP/STXP) from the merely ORDERED
        // load-acquire/store-release (o2=1: LDAR/LDLAR/STLR/STLLR), which are NOT part of an exclusive
        // pair. Masking bit23 in (0x3FC00000) keeps a bare LDAR -- ubiquitous in C++ std::atomic and
        // glibc -- from opening the region and leaving in_excl stuck on. L (bit22) selects load vs store.
        // CASP/CASPA/CASPL/CASPAL share this encoding box (o2=0, and A reuses the L bit), so the plain
        // mask alone reads CASPA/CASPAL as a load-exclusive and CASP/CASPL as a store-exclusive. A CASPA
        // then latches in_excl ON for the rest of the block, which disables the non-PIE bias fold for
        // every following memory op -- a fatal SIGSEGV on the next low image access. Exclude CASP here;
        // it is a single self-contained instruction and opens no monitor region.
        if (!is_casp(in)) {
            if ((in & 0x3FC00000u) == 0x08400000u)
                // load-exclusive (o2=0, L=1)
                in_excl = 1;
            else if (in_excl && (in & 0x3FC00000u) == 0x08000000u)
                // store-exclusive (o2=0, L=0)
                in_excl = 0;
        }
        // Defensive: the deferred-branch table is fixed-size. If a region ever fills it (pathological
        // or mis-decoded -- a real LDXR..STXR pair never holds this many conditional branches), end the
        // region here so the branches below take the normal exit path instead of overflowing defer[].
        if (in_excl && ndefer >= (int)(sizeof defer / sizeof defer[0])) in_excl = 0;

        // svc #0
        if (in == 0xD4000001u) {
            emit_exit_const(gpc, R_SYSCALL);
            break;
        }
        // b
        if ((in & 0xFC000000u) == 0x14000000u) {
            int64_t off = sext(in & 0x3FFFFFF, 26) << 2;
            uint64_t tgt = gpc + off;
            // opt4: follow the unconditional edge INLINE if its target is a fresh block (not the
            // region head, not already inlined, not already translated) -> the inter-block `b`
            // disappears. Otherwise chain normally (existing block / loop back-edge).
            if (STITCH_OK && tgt != start && !seen_has(seen, nseen, tgt) && !map_body(tgt)) {
                seen[nseen++] = tgt;
                trace_blk++;
                gpc = tgt;
                continue;
            }
            emit_chain_exit(tgt);
            break;
        }
        // bl
        if ((in & 0xFC000000u) == 0x94000000u) {
            int64_t off = sext(in & 0x3FFFFFF, 26) << 2;
            // Fuse a direct call to the exact canonical four-insn PLT veneer:
            //   adrp x16,page; ldr x17,[x16,#got]; add x16,x16,#lo; br x17
            // Preserve every architectural effect and the real fault-capable GOT
            // load; only the extra translated-block hop and its entry poll vanish.
            uint64_t plt = gpc + off;
            if (smc_seen()) goto no_bl_plt_fuse;
            if (!hl_host_range_mapped((uintptr_t)plt, 16)) goto no_bl_plt_fuse;
            uint32_t p0 = a64_fetch_instruction(plt, NULL);
            uint32_t p1 = a64_fetch_instruction(plt + 4, NULL);
            uint32_t p2 = a64_fetch_instruction(plt + 8, NULL);
            uint32_t p3 = a64_fetch_instruction(plt + 12, NULL);
            if (!guestbase_on() && !jit_guest_bus_active() && (p0 & 0x9F00001Fu) == 0x90000010u &&
                (p1 & 0xFFC003FFu) == 0xF9400211u && (p2 & 0xFFC003FFu) == 0x91000210u && p3 == 0xD61F0220u) {
                int64_t pimm = sext((((p0 >> 5) & 0x7FFFF) << 2) | ((p0 >> 29) & 3), 21) << 12;
                uint64_t page = (pcrel_base(plt) & ~0xFFFull) + pimm;
                emit_set_x30(pcrel_base(gpc) + 4);
                if (!emit_guest_adrp_page(16, page)) e_movconst(16, page);
                e_str(16, CPUREG, 16 * 8);
                uint64_t load_host = (uint64_t)g_cp;
                emit32(p1);
                pcache_record_provenance(load_host, (uint64_t)g_cp, plt + 4);
                e_str(17, CPUREG, 17 * 8);
                emit32(p2);
                e_str(16, CPUREG, 16 * 8);
                txpg_mark(plt, plt + 16);
                if (g_txln_active)
                    for (uint64_t line = plt >> 6; line <= (plt + 15) >> 6; line++)
                        txln_put(line);
                emit_ibranch_ip2_ready(17, 1);
                break;
            }
        no_bl_plt_fuse:
            // Inline an LSE outline-atomic helper call to a single host atomic op (elides the call +
            // return dispatch, the dominant atomics tax); only fires in the verbatim-safe regime.
            if (try_inline_outline_atomic(gpc, gpc + off)) {
                gpc += 4;
                continue;
            }
            uint64_t ancestors[CTX_INLINE_DEPTH];
            for (int i = 0; i < nctx; i++)
                ancestors[i] = ctx[i].target;
            uint64_t clone_ret;
            int clone_cost;
            /*
             * A BUS-active generation expands every cloned memory operation
             * with a runtime guard. Cloning then duplicates both hot guards
             * and cold stubs, accelerating cache rotation while removing only
             * a call/return pair. Keep ordinary context cloning unchanged, but
             * use the normal RAS call path while BUS observation is active.
             */
            if (!smc_seen() && !jit_guest_bus_active() && nctx < CTX_INLINE_DEPTH &&
                context_clone_candidate(gpc + off, ancestors, nctx, &clone_ret, &clone_cost) &&
                (g_cp - (uint8_t *)host) + clone_cost * 16 < TRACE_MAX_BYTES) {
                emit_set_x30(pcrel_base(gpc) + 4);
                ctx[nctx].target = gpc + off;
                ctx[nctx].resume = gpc + 4;
                ctx[nctx].retpc = clone_ret;
                ctx[nctx].expected_x30 = pcrel_base(gpc) + 4;
                nctx++;
                gpc += off;
                continue;
            }
            emit_bl_ras(gpc, gpc + off);
            // §B: shadow push + host bl (RAS) + Lcont continuation
            break;
        }
        // ret xN
        if ((in & 0xFFFFFC1Fu) == 0xD65F0000u) {
            int rrn = (in >> 5) & 31;
            if (rrn == 30 && nctx > 0 && gpc == ctx[nctx - 1].retpc) {
                e_movr(16, 30);
                e_movconst(17, ctx[nctx - 1].expected_x30);
                emit32(0xCB000000u | (17u << 16) | (16u << 5) | 16u);
                uint32_t *p_zero = (uint32_t *)g_cp;
                emit32(0);
                emit_ibranch(30);
                uint8_t *resume_host = g_cp;
                *p_zero =
                    0xB4000000u | (((uint32_t)(((uint8_t *)resume_host - (uint8_t *)p_zero) / 4) & 0x7FFFF) << 5) | 16;
                gpc = ctx[nctx - 1].resume;
                nctx--;
                continue;
            }
            if (rrn == 30)
                // A3: §B OFF (default) -> bare IBTC return (no shadow-ret preamble); §B ON -> shadow ret
                // (FAST host-ret on guest_ret+guest_sp match, else IBTC fallback).
                shadowgate() == -1 ? emit_ibranch(30) : emit_shadow_ret();
            else
                // ret xN via another reg -> ordinary indirect branch
                emit_ibranch(rrn);
            break;
        }
        // br
        if ((in & 0xFFFFFC1Fu) == 0xD61F0000u) {
            int brn = (in >> 5) & 31;
            if (g_steal1617 && !g_noibslim && !is_stolen(brn) && is_interp_dispatch_br(gpc, brn))
                // IBSLIM: a recognized interpreter-dispatch site (megamorphic by construction) --
                // skip the dead per-site IC, go straight to the shared hash.
                emit_hash_tail(brn);
            else
                emit_ibranch(brn);
            break;
        }
        // blr
        if ((in & 0xFFFFFC1Fu) == 0xD63F0000u) {
            // guest x30 lives in cpu->x[30] (stolen); RAS push needs a host blr. The link value is
            // guest-visible (spilled to the guest stack), so store the UN-BIASED (low) return vaddr
            // for non-PIE; the dispatcher re-biases on the ret. pcrel_base is identity for PIE.
            int blrn = (in >> 5) & 31;
            if (blrn == 30) {
                // BLR x30 reads the old guest x30 as its target, then writes
                // the return address to x30. Preserve that ordering now that
                // guest x30 is resident in cpu->x[30].
                // emit_set_x30 uses x16 as its store scratch, so keep the
                // branch target in the other engine-private IP register.
                e_ldr(17, CPUREG, 30 * 8);
                emit_set_x30(pcrel_base(gpc) + 4);
                emit_ibranch_ip2_ready(17, 1);
            } else {
                emit_set_x30(pcrel_base(gpc) + 4);
                emit_ibranch(blrn);
            }
            //   (Section 3) -- deferred; Stage-B IBTC for the function-ptr return
            break;
        }
        // b.cond
        if ((in & 0xFF000010u) == 0x54000000u) {
            int cond = in & 0xF;
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            uint64_t taken = gpc + off, fall = gpc + 4;
            if (in_excl) {
                defer[ndefer].patch = (uint32_t *)g_cp;
                defer[ndefer].target = taken;
                defer[ndefer].in = in;
                ndefer++;
                emit32(0);
                gpc += 4;
                continue;
            }
            // W4E tier-2: single-block self-loop (taken back-edge == block start). Intercept BEFORE the
            // opt4 stitch so the redundant back-edge trampoline can be folded; non-self-loops (taken !=
            // start) fall through to opt4 unchanged. NOTIER2 -> skipped (exact committed-opt4 baseline).
            if (taken == start && !g_notier2 && !loop_has_rmw_hazard(start, gpc)) {
                int slot = g_tier2_build ? 0 : t2_slot(start);
                if (g_tier2_build || slot >= 0) {
                    emit_selfloop(in, start, fall, body, slot);
                    break;
                }
            }
            // opt4: lay the fall-through inline; invert the condition so TAKEN is the exit. This inverts by
            // flipping cond bit0 (stitch_cond: in ^ 1) -- valid ONLY for a genuinely conditional branch.
            // The condition field 0b111x is special: 0b1110 = AL and 0b1111 = NV BOTH mean "always execute"
            // in A64 (NV is not "never"; it is a reserved alias of AL). So a guest `b.al` is an
            // UNCONDITIONAL branch with a DEAD fall-through, and flipping its bit0 yields NV -- still
            // "always" -- so the "inverted" branch never actually diverts: the stitched superblock would
            // then ALWAYS fall into the (guest-unreachable) fall-through and NEVER reach the guest's real
            // always-taken target. HotSpot's generated interpreter emits `b.al` as a plain jump, so this
            // silently mislowered its dispatch -> an infinite spin (#186). Exclude cond >= 0xE from the fold;
            // an always-branch takes the ordinary b.cond emit below, which chains straight to `taken`.
            if (STITCH_OK && cond < 0xE && fall != start && !seen_has(seen, nseen, fall) && !map_body(fall)) {
                stitch_cond(in ^ 1u, taken);
                seen[nseen++] = fall;
                trace_blk++;
                ncond++;
                gpc = fall;
                continue;
            }
            uint32_t *patch = (uint32_t *)g_cp;
            // b.cond -> taken (backpatched)
            emit32(0);
            emit_chain_exit(fall);
            int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
            *patch = 0x54000000u | ((uint32_t)(d & 0x7FFFF) << 5) | cond;
            emit_chain_exit(taken);
            break;
        }
        // cbz / cbnz
        if ((in & 0x7E000000u) == 0x34000000u) {
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            uint64_t taken = gpc + off, fall = gpc + 4;
            int sf = in >> 31, op = (in >> 24) & 1, rt = in & 31;
            if (in_excl) {
                defer[ndefer].patch = (uint32_t *)g_cp;
                defer[ndefer].target = taken;
                defer[ndefer].in = in;
                ndefer++;
                emit32(0);
                gpc += 4;
                continue;
            }
            // A backward CBZ/CBNZ whose loop body is only polling memory is the canonical AArch64
            // contended-lock wait loop (glibc/musl spin locks and several runtime locks use it).  The
            // translated thread otherwise consumes its entire host timeslice repeatedly loading the
            // held word, which can starve the translated owner that must run to release it.  Preserve
            // the guest-visible semantics but emit the architectural YIELD hint before retrying.  Keep
            // this deliberately narrow: both the instruction immediately before the branch and the
            // branch target must be scalar loads, so compute loops and ordinary backward branches remain
            // byte-for-byte unchanged.
            uint32_t prev = gpc >= start + 4 ? a64_fetch_instruction(gpc - 4, NULL) : 0;
            uint32_t first = taken < gpc ? a64_fetch_instruction(taken, NULL) : 0;
            int prev_load = (prev & 0x0A000000u) == 0x08000000u && ((prev >> 22) & 1u);
            int first_load = (first & 0x0A000000u) == 0x08000000u && ((first >> 22) & 1u);
            if (taken < gpc && prev_load && first_load) emit32(0xD503203Fu); // yield
            // W4E tier-2: single-block self-loop (non-stolen tested reg). Before opt4; NOTIER2 -> skipped.
            if (taken == start && !g_notier2 && !is_stolen(rt) && !loop_has_rmw_hazard(start, gpc)) {
                int slot = g_tier2_build ? 0 : t2_slot(start);
                if (g_tier2_build || slot >= 0) {
                    emit_selfloop(in, start, fall, body, slot);
                    break;
                }
            }
            // opt4: lay the fall-through inline (non-stolen test reg only); invert op (cbz<->cbnz)
            if (STITCH_OK && !is_stolen(rt) && fall != start && !seen_has(seen, nseen, fall) && !map_body(fall)) {
                stitch_cond(in ^ (1u << 24), taken);
                seen[nseen++] = fall;
                trace_blk++;
                ncond++;
                gpc = fall;
                continue;
            }
            // tested reg stolen -> test cpu->x[rt] via a saved scratch
            if (is_stolen(rt)) {
                // stealfast: x16 is engine-dead across both successor edges -> no spill/restore at all
                if (stealfast_on()) {
                    e_ldr(16, CPUREG, rt * 8);
                    uint32_t *patch = (uint32_t *)g_cp;
                    // cbz/cbnz x16 -> taken
                    emit32(0);
                    emit_chain_exit(fall);
                    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                    *patch =
                        0x34000000u | ((unsigned)sf << 31) | ((unsigned)op << 24) | ((uint32_t)(d & 0x7FFFF) << 5) | 16;
                    emit_chain_exit(taken);
                    break;
                }
                int S = 0;
                e_str(S, CPUREG, (int)OFF_MSCRATCH);
                e_ldr(S, CPUREG, rt * 8);
                uint32_t *patch = (uint32_t *)g_cp;
                // cbz/cbnz S -> taken
                emit32(0);
                e_ldr(S, CPUREG, (int)OFF_MSCRATCH);
                // fall: restore S
                emit_chain_exit(fall);
                int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                *patch = 0x34000000u | ((unsigned)sf << 31) | ((unsigned)op << 24) | ((uint32_t)(d & 0x7FFFF) << 5) | S;
                e_ldr(S, CPUREG, (int)OFF_MSCRATCH);
                emit_chain_exit(taken);
                // taken: restore S
                break;
            }
            uint32_t *patch = (uint32_t *)g_cp;
            // cbz/cbnz rt -> taken
            emit32(0);
            emit_chain_exit(fall);
            int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
            *patch = 0x34000000u | ((unsigned)sf << 31) | ((unsigned)op << 24) | ((uint32_t)(d & 0x7FFFF) << 5) | rt;
            emit_chain_exit(taken);
            break;
        }
        // tbz / tbnz
        if ((in & 0x7E000000u) == 0x36000000u) {
            int b40 = (in >> 19) & 0x1F, bit5 = (in >> 31) & 1;
            int64_t off = sext((in >> 5) & 0x3FFF, 14) << 2;
            uint64_t taken = gpc + off, fall = gpc + 4;
            int op = (in >> 24) & 1, rt = in & 31;
            if (in_excl) {
                defer[ndefer].patch = (uint32_t *)g_cp;
                defer[ndefer].target = taken;
                defer[ndefer].in = in;
                ndefer++;
                emit32(0);
                gpc += 4;
                continue;
            }
            // W4E tier-2: single-block self-loop (non-stolen tested reg). Before opt4; NOTIER2 -> skipped.
            if (taken == start && !g_notier2 && !is_stolen(rt) && !loop_has_rmw_hazard(start, gpc)) {
                int slot = g_tier2_build ? 0 : t2_slot(start);
                if (g_tier2_build || slot >= 0) {
                    emit_selfloop(in, start, fall, body, slot);
                    break;
                }
            }
            // opt4: lay the fall-through inline (non-stolen test reg only); invert op (tbz<->tbnz)
            if (STITCH_OK && !is_stolen(rt) && fall != start && !seen_has(seen, nseen, fall) && !map_body(fall)) {
                stitch_cond(in ^ (1u << 24), taken);
                seen[nseen++] = fall;
                trace_blk++;
                ncond++;
                gpc = fall;
                continue;
            }
            // tested reg stolen -> test cpu->x[rt] via a saved scratch
            if (is_stolen(rt)) {
                // stealfast: x16 is engine-dead across both successor edges -> no spill/restore at all
                if (stealfast_on()) {
                    e_ldr(16, CPUREG, rt * 8);
                    uint32_t *patch = (uint32_t *)g_cp;
                    // tbz/tbnz x16,#bit -> taken
                    emit32(0);
                    emit_chain_exit(fall);
                    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                    *patch = 0x36000000u | ((unsigned)bit5 << 31) | ((unsigned)op << 24) | ((unsigned)b40 << 19) |
                             ((uint32_t)(d & 0x3FFF) << 5) | 16;
                    emit_chain_exit(taken);
                    break;
                }
                int S = 0;
                e_str(S, CPUREG, (int)OFF_MSCRATCH);
                e_ldr(S, CPUREG, rt * 8);
                uint32_t *patch = (uint32_t *)g_cp;
                // tbz/tbnz S,#bit -> taken
                emit32(0);
                e_ldr(S, CPUREG, (int)OFF_MSCRATCH);
                // fall: restore S
                emit_chain_exit(fall);
                int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                *patch = 0x36000000u | ((unsigned)bit5 << 31) | ((unsigned)op << 24) | ((unsigned)b40 << 19) |
                         ((uint32_t)(d & 0x3FFF) << 5) | S;
                e_ldr(S, CPUREG, (int)OFF_MSCRATCH);
                emit_chain_exit(taken);
                // taken: restore S
                break;
            }
            uint32_t *patch = (uint32_t *)g_cp;
            // tbz/tbnz rt,#bit -> taken
            emit32(0);
            emit_chain_exit(fall);
            int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
            *patch = 0x36000000u | ((unsigned)bit5 << 31) | ((unsigned)op << 24) | ((unsigned)b40 << 19) |
                     ((uint32_t)(d & 0x3FFF) << 5) | rt;
            emit_chain_exit(taken);
            break;
        }

        // --- TLS: the whole point. mrs/msr tpidr_el0 become a single NATIVE
        //     load/store from cpu->tls. No trap, no Mach round-trip. ---
        // mrs xN, tpidr_el0  (TLS read, hot: CPython reads its thread state through this constantly).
        // stealfast: x28 IS the cpu pointer for the whole block, so the read is ONE ldr (legacy paid a
        // 3-insn TLS-based cpu reload via e_load_cpu, or the full x18_prolog dance for a stolen rd).
        if ((in & 0xFFFFFFE0u) == 0xD53BD040u) {
            int n = in & 31;
            if (is_stolen(n)) {
                if (stealfast_on()) {
                    e_ldr(16, CPUREG, OFF_TLS);
                    e_str(16, CPUREG, n * 8);
                } else {
                    x18_prolog();
                    e_ldr(0, 1, OFF_TLS);
                    e_str(0, 1, n * 8);
                    x18_epilog();
                }
            } else if (stealfast_on()) {
                e_ldr(n, CPUREG, OFF_TLS);
            } else {
                e_load_cpu(n);
                e_ldr(n, n, OFF_TLS);
            }
            gpc += 4;
            continue;
        }
        // msr tpidr_el0, xN  (TLS write, rare)
        if ((in & 0xFFFFFFE0u) == 0xD51BD040u) {
            int n = in & 31, t = (n == 16) ? 15 : 16;
            if (is_stolen(n)) {
                if (stealfast_on()) {
                    e_ldr(16, CPUREG, n * 8);
                    e_str(16, CPUREG, OFF_TLS);
                } else {
                    x18_prolog();
                    e_ldr(0, 1, n * 8);
                    e_str(0, 1, OFF_TLS);
                    x18_epilog();
                }
            } else if (stealfast_on()) {
                e_str(n, CPUREG, OFF_TLS);
            } else {
                // NOSTEALFAST/NOSTEAL1617 only: park scratch `t` in cpu->mscratch[0] (x28=cpu holds), NOT
                // [sp,#-16] -- a below-SP store faults on a shallow guest stack (the 6d38d96c crash class).
                // t (15/16) != n (non-stolen) and != CPUREG, so no operand is clobbered.
                e_str(t, CPUREG, (int)OFF_MSCRATCH);
                e_load_cpu(t);
                e_str(n, t, OFF_TLS);
                e_ldr(t, CPUREG, (int)OFF_MSCRATCH);
            }
            gpc += 4;
            continue;
        }

        // --- SMC prerequisite: mrs Xt, ctr_el0 (cache-type register) ---
        // __clear_cache reads CTR_EL0 to size its dc/ic strides. Reading it from EL0 FAULTS for the JIT'd
        // guest on macOS (SCTLR_EL1.UCT is not enabled for it), so the verbatim mrs crashed every guest that
        // flushes its icache. Materialize a synthetic value describing the DBT's coherence model instead:
        //   IminLine/DminLine = 4 -> 64-byte I/D lines        L1Ip = PIPT
        //   IDC (bit28) = 1 -> "DC clean to PoU not required": TRUE here, the host re-translates the page by
        //                      reading the SAME coherent memory the guest wrote, so __clear_cache skips DC.
        //   DIC (bit29) = 0 -> "IC invalidate IS required": keeps the guest issuing `ic ivau`, our SMC hook.
        if ((in & 0xFFFFFFE0u) == 0xD53B0020u) {
            int rd = in & 31;
            emit_cpu_model_value(rd, g_aarch64_cpu_model.ctr_el0);
            gpc += 4;
            continue;
        }
        if ((in & 0xFFFFFFE0u) == 0xD53B00E0u) {
            emit_cpu_model_value(in & 31, g_aarch64_cpu_model.dczid_el0);
            gpc += 4;
            continue;
        }
        // HWCAP_CPUID is absent: EL1 ID-register families are inaccessible to EL0 and must not leak host IDs.
        if ((in & 0xFFFF0000u) == 0xD5380000u && !g_aarch64_cpu_model.user_id_registers) {
            emit32(0);
            gpc += 4;
            continue;
        }

        /* DC ZVA is defined by the guest's DCZID_EL0, not by the host CPU.
           Apple hosts may use a different zero-block size; copying the opcode
           verbatim then clears bytes outside the 64-byte block advertised to
           the guest.  Managed runtimes place live metadata immediately after
           such blocks, so the mismatch surfaces later as pointer corruption.
           Lower to four exact stores at the guest-model-aligned address. */
        if ((in & 0xFFFFFFE0u) == 0xD50B7420u) {
            int source = (int)(in & 31u);
            if (is_stolen(source))
                e_ldr(16, CPUREG, source * 8);
            else
                e_movr(16, source);
            emit32(0x927AE610u); /* and x16,x16,#-64 */
            if (jit_guest_bus_active()) emit_a64_bus_guard(16, 64, gpc);
            struct a64_soft_guard soft = emit_a64_soft_guard_begin(16, 17, 18, 64, HL_LOGICAL_VMA_WRITE, gpc);
            for (unsigned offset = 0; offset < 64; offset += 16)
                emit32(0xA9000000u | (((offset / 8) & 0x7Fu) << 15) | (31u << 10) | (16u << 5) |
                       31u); /* stp xzr,xzr,[x16,#offset] */
            emit_a64_soft_guard_end(&soft);
            gpc += 4;
            continue;
        }
        // --- SMC: dc cvau, Xt (data-cache clean to PoU) -> nop ---
        // A pure no-op for a DBT: the host never instruction-fetches guest pages, so the guest's data writes
        // need no clean for our re-translation (which is a normal coherent data read). Standard __clear_cache
        // already skips DC via IDC=1 above; this also covers callers that issue it unconditionally and avoids
        // any EL0 trap on the instruction. (NOSMC keeps it -- it is unrelated to the stale-translation A/B.)
        if ((in & 0xFFFFFFE0u) == 0xD50B7B20u) {
            emit32(0xD503201Fu); // nop
            gpc += 4;
            continue;
        }
        // --- SMC: ic ivau, Xt (instruction-cache invalidate by VA to PoU) ---
        // A code-generating guest issues this (the __clear_cache / dc;dsb;ic;dsb;isb dance) before running
        // freshly-written bytes. The host never instruction-fetches guest pages (we execute the TRANSLATED
        // copy), so emitting `ic ivau` verbatim is a no-op for our cache -> the guest would re-run the STALE
        // translation. Instead end the block here and exit R_ICFLUSH: the dispatcher drops the stale gpc->host
        // map + IBTC (smc_icflush) and the modified bytes re-translate. pc resumes PAST the ic ivau. Gated by
        // NOSMC; the dc cvau / isb in the same dance run verbatim (harmless: they touch real data memory).
        if ((in & 0xFFFFFFE0u) == 0xD50B7520u && !smc_disabled()) {
            emit_smc_queue((int)(in & 31));
            gpc += 4;
            continue;
        }
        if (in == 0xD5033FDFu && !smc_disabled()) {
            emit_exit_const(gpc + 4, R_ICCOMMIT);
            break;
        }

        // --- non-branch, PC-relative: rewrite to materialize the (relocated) addr ---
        // adr
        if ((in & 0x9F000000u) == 0x10000000u) {
            int rd = in & 31;
            int64_t imm = sext((((in >> 5) & 0x7FFFF) << 2) | ((in >> 29) & 3), 21);
            uint64_t v = pcrel_base(gpc) + imm;
            if (is_stolen(rd)) {
                // stealfast: host x16 is engine-dead -> movconst + one store (no red-zone stash, no
                // TLS-based cpu reload; x28 = cpu). adrp x16 is the PLT-stub head, so this is HOT.
                if (stealfast_on()) {
                    e_movconst(16, v);
                    e_str(16, CPUREG, rd * 8);
                } else {
                    x18_prolog();
                    e_movconst(0, v);
                    e_str(0, 1, rd * 8);
                    x18_epilog();
                }
            } else
                e_movconst(rd, v);
            gpc += 4;
            continue;
        }
        // adrp
        if ((in & 0x9F000000u) == 0x90000000u) {
            int rd = in & 31;
            int64_t imm = sext((((in >> 5) & 0x7FFFF) << 2) | ((in >> 29) & 3), 21) << 12;
            uint64_t v = (pcrel_base(gpc) & ~0xFFFull) + imm;
            // Exact canonical AArch64 PLT veneer:
            //   adrp x16,page; ldr x17,[x16,#got]; add x16,x16,#lo; br x17
            if (!hl_host_range_mapped((uintptr_t)gpc, 16)) goto no_adrp_plt_fuse;
            uint32_t p1 = a64_fetch_instruction(gpc + 4, NULL);
            uint32_t p2 = a64_fetch_instruction(gpc + 8, NULL);
            uint32_t p3 = a64_fetch_instruction(gpc + 12, NULL);
            if (!guestbase_on() && !jit_guest_bus_active() && (in & 0x9F00001Fu) == 0x90000010u &&
                (p1 & 0xFFC003FFu) == 0xF9400211u && (p2 & 0xFFC003FFu) == 0x91000210u && p3 == 0xD61F0220u) {
                if (!emit_guest_adrp_page(16, v)) e_movconst(16, v);
                e_str(16, CPUREG, 16 * 8);
                uint64_t load_host = (uint64_t)g_cp;
                emit32(p1);
                pcache_record_provenance(load_host, (uint64_t)g_cp, gpc + 4);
                e_str(17, CPUREG, 17 * 8);
                emit32(p2);
                e_str(16, CPUREG, 16 * 8);
                if (!g_tier2_build && g_txln_active) {
                    uint64_t last = (gpc + 12) >> 6;
                    for (uint64_t line = gpc >> 6; line <= last; line++)
                        txln_put(line);
                    tx_last_line = last;
                }
                if (gpc + 16 > guest_end) guest_end = gpc + 16;
                emit_ibranch_ip2_ready(17, 1);
                break;
            }
        no_adrp_plt_fuse:
            if (is_stolen(rd)) {
                if (stealfast_on()) {
                    if (!emit_guest_adrp_page(16, v)) e_movconst(16, v);
                    e_str(16, CPUREG, rd * 8);
                } else {
                    x18_prolog();
                    if (!emit_guest_adrp_page(0, v)) e_movconst(0, v);
                    e_str(0, 1, rd * 8);
                    x18_epilog();
                }
            } else if (!emit_guest_adrp_page(rd, v))
                e_movconst(rd, v);
            gpc += 4;
            continue;
        }
        // ldr (literal) 32/64
        if ((in & 0xBF000000u) == 0x18000000u) {
            int rt = in & 31, is64 = (in >> 30) & 1;
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            if (is_stolen(rt)) {
                e_movconst(16, gpc + off);
                emit_a64_bus_guard(16, is64 ? 8 : 4, gpc);
                struct a64_soft_guard soft =
                    emit_a64_soft_guard_begin(16, 17, 18, is64 ? 8 : 4, HL_LOGICAL_VMA_READ, gpc);
                if (is64)
                    e_ldr(16, 16, 0);
                else
                    emit32(0xB9400000u | (16 << 5) | 16);
                emit_a64_soft_guard_end(&soft);
                e_str(16, CPUREG, rt * 8);
                emit_a64_soft_bounce_commit(gpc + 4);
            } else {
                e_movconst(rt, gpc + off);
                emit_a64_bus_guard(rt, is64 ? 8 : 4, gpc);
                struct a64_soft_guard soft =
                    emit_a64_soft_guard_begin(rt, 16, 17, is64 ? 8 : 4, HL_LOGICAL_VMA_READ, gpc);
                if (is64)
                    e_ldr(rt, rt, 0);
                else
                    emit32(0xB9400000u | (rt << 5) | rt);
                emit_a64_soft_guard_end(&soft);
                emit_a64_soft_bounce_commit(gpc + 4);
            }
            gpc += 4;
            continue;
        }
        // ldrsw (literal): opc=10, V=0 -> top byte 0x98 (unique: bits[29:27]=011, bits[25:24]=00, bit26=0).
        // The integer ldr-literal above masks 0xBF (only bit30), so opc=10 does NOT match it and this
        // sign-extending 32->64 word literal load would fall through to the verbatim emit -- executing
        // PC-relative from the HOST code cache and loading a garbage word (then sign-extended into Xt).
        // Compilers emit LDRSW-literal for switch/jump tables (sign-extended word offsets). Same hazard
        // and same fix as the integer/SIMD forms: materialize the GUEST literal address and LDRSW from it,
        // so the value is correct regardless of host arena placement or a warm pcache load.
        if ((in & 0xFF000000u) == 0x98000000u) {
            int rt = in & 31;
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            if (is_stolen(rt)) {
                e_movconst(16, gpc + off); // x16 = guest literal address
                emit_a64_bus_guard(16, 4, gpc);
                struct a64_soft_guard soft = emit_a64_soft_guard_begin(16, 17, 18, 4, HL_LOGICAL_VMA_READ, gpc);
                emit32(0xB9800000u | (16 << 5) | 16); // ldrsw x16, [x16]
                emit_a64_soft_guard_end(&soft);
                e_str(16, CPUREG, rt * 8);
                emit_a64_soft_bounce_commit(gpc + 4);
            } else {
                e_movconst(rt, gpc + off);
                emit_a64_bus_guard(rt, 4, gpc);
                struct a64_soft_guard soft = emit_a64_soft_guard_begin(rt, 16, 17, 4, HL_LOGICAL_VMA_READ, gpc);
                emit32(0xB9800000u | (rt << 5) | rt); // ldrsw xt, [xt]
                emit_a64_soft_guard_end(&soft);
                emit_a64_soft_bounce_commit(gpc + 4);
            }
            gpc += 4;
            continue;
        }
        // ldr (literal), SIMD&FP: `ldr St/Dt/Qt, [pc, #imm]`. The integer ldr-literal above only matches
        // V=0; the SIMD/FP form (V=1, bit26) would otherwise fall through to the verbatim emit and execute
        // PC-relative from the HOST code cache -- loading garbage instead of the guest literal pool. LuaJIT
        // trace mcode loads its double constants this way (e.g. `ldr d15,[pc,#-N]`), so a verbatim emit
        // corrupts the trace's FP constants (intermittent crashes once a bad value reaches a Lua value).
        // Rewrite it like the integer case: materialize the guest literal ADDRESS into a scratch GPR and
        // load the V register from it. opc[31:30]: 00=S(32b) 01=D(64b) 10=Q(128b); 11 is PRFM (no data reg).
        if ((in & 0x3F000000u) == 0x1C000000u && ((in >> 30) & 3) != 3) {
            int vt = in & 31, sz = (in >> 30) & 3;
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            // ldr (V), [Xn] unsigned-offset #0 base forms, Rn=x0: S=0xBD400000 D=0xFD400000 Q=0x3DC00000
            uint32_t ld = sz == 0 ? 0xBD400000u : (sz == 1 ? 0xFD400000u : 0x3DC00000u);
            e_movconst(16, gpc + off); // x16 = guest literal address
            emit_a64_bus_guard(16, UINT64_C(4) << sz, gpc);
            struct a64_soft_guard soft =
                emit_a64_soft_guard_begin(16, 17, 18, UINT64_C(4) << sz, HL_LOGICAL_VMA_READ, gpc);
            emit32(ld | (16u << 5) | (uint32_t)vt); // ldr St/Dt/Qt, [x16]
            emit_a64_soft_guard_end(&soft);
            emit_a64_soft_bounce_commit(gpc + 4);
            gpc += 4;
            continue;
        }
        // prfm (literal): opc=11, V=0 -> top byte 0xD8. A prefetch HINT that reads its target address
        // PC-relative from the guest literal pool. It has no destination register and never faults, but a
        // verbatim emit would prefetch a host-PC-relative (garbage) address -- useless work, never the
        // intended guest line. Prefetch is architecturally optional, so honoring it as "no prefetch" is
        // always legal: drop it to a nop. This completes the PC-relative literal-load family (0x18/0x58
        // LDR-lit, 0x98 LDRSW-lit, 0x1C/0x5C/0x9C LDR-lit-SIMD, 0xD8 PRFM-lit) -- every form rewritten.
        if ((in & 0xFF000000u) == 0xD8000000u) {
            emit32(0xD503201Fu); // nop
            gpc += 4;
            continue;
        }
        /* Register/immediate PRFM is also an architecturally optional,
           non-faulting hint.  Sending it through the ordinary fold would
           incorrectly require WRITE permission and could synthesize a guest
           fault.  Drop it exactly like PRFM-literal. */
        if (is_prfm_register_or_immediate(in)) {
            emit32(0xD503201Fu);
            gpc += 4;
            continue;
        }

        // pointer authentication (ubuntu 24.04 -mbranch-protection): we don't enforce PAC, and signing
        // x30 on the PAC-capable host would corrupt the §B shadow-stack return match (it expects an
        // UNSIGNED guest x30) -> wild branch to a signed address. Neutralize PAC (hardening, not
        // semantics): paci*/auti* hints -> nop (x30 stays unsigned); retaa/retab -> a plain x30 ret.
        // paciasp/autiasp/paci?z/... -> nop
        if ((in & 0xFFFFFF1Fu) == 0xD503231Fu) {
            emit32(0xD503201Fu);
            gpc += 4;
            continue;
        }
        // retaa/retab -> shadow ret (x30)
        if ((in & 0xFFFFFBFFu) == 0xD65F0BFFu) {
            shadowgate() == -1 ? emit_ibranch(30) : emit_shadow_ret();
            break;
        }

        // guest_base bias-fold: a non-PIE image's LOW absolute load/store -> +bias (the high mapping), no
        // fault. Only outside an exclusive monitor region (the fold spills scratch to memory, which would
        // clear the monitor) and only for a non-SP base (the stack is always high). The AdvSIMD load/store
        // structure family (ld1/st1.., ld1r) has no offset/index so is_foldable_mem omits it -- fold it via
        // its own emitter (else glibc's NEON strlen/memcpy trap once per access on the image). Inert for PIE.
        if ((guestbase_on() || jit_guest_soft_active()) && !in_excl &&
            (jit_guest_soft_active() || ((in >> 5) & 31) != 31)) {
            if (is_foldable_mem(in)) {
                if (jit_guest_bus_active()) emit_a64_bus_guard_instruction(in, gpc);
                emit_fold_mem(in, 0);
                gpc += 4;
                continue;
            }
            if (is_advsimd_struct(in)) {
                emit_fold_advsimd_struct(in);
                gpc += 4;
                continue;
            }
        }
        if (jit_guest_bus_active()) {
            if (!guestbase_on() && !in_excl && is_foldable_mem(in)) emit_a64_bus_guard_instruction(in, gpc);
            if (!guestbase_on() && !in_excl && is_advsimd_struct(in))
                emit_a64_bus_guard_base((in >> 5) & 31, 0, (uint64_t)advsimd_struct_bytes(in), gpc);
            /* Pair pre/post-index forms are deliberately not bias-folded.  Guard
               their architectural access address, then preserve the native
               writeback opcode verbatim below. */
            if ((in & 0x3A000000u) == 0x28000000u) {
                int mode = (in >> 23) & 3;
                if (mode == 1 || mode == 3) {
                    uint64_t total = a64_mem_bytes(in);
                    int64_t imm = sext((in >> 15) & 0x7f, 7) * (int64_t)(total / 2);
                    emit_a64_bus_guard_base((in >> 5) & 31, mode == 3 ? imm : 0, total, gpc);
                }
            }
            /* Guard once at the load-exclusive edge.  No call is injected
               between LDXR/LDXP and STXR/STXP, which would clear the host
               exclusive monitor; activation cannot publish a new BUS range
               until this thread reaches the dispatcher. */
            if ((in & 0x3FC00000u) == 0x08400000u && !is_casp(in)) {
                uint64_t bytes = UINT64_C(1) << ((in >> 30) & 3);
                if ((in >> 21) & 1) bytes *= 2;
                emit_a64_bus_guard_base((in >> 5) & 31, 0, bytes, gpc);
            }
        }
        if (jit_guest_soft_active() && (((in & 0x3F000000u) == 0x08000000u) || is_casp(in))) {
            emit_a64_soft_exclusive(in);
            gpc += 4;
            continue;
        }
        // CASP paired compare-and-swap: the mangle machinery only substitutes NAMED register fields, so a
        // stolen pair partner (Xs+1 / Xt+1) would slip through verbatim. Relocate both pairs when any member
        // is stolen; otherwise (the common case) emit verbatim -- byte-identical to before.
        if (is_casp(in)) {
            if (casp_uses_stolen(in))
                emit_casp_mangled(in, -1);
            else
                emit32(in);
            gpc += 4;
            continue;
        }
        // Exact ORR-alias MOV from a stolen source into a live guest host reg.
        if ((in & 0x7FE0FFE0u) == 0x2A0003E0u) {
            int rd = in & 31, rm = (in >> 16) & 31;
            if (rd != 31 && !is_stolen(rd) && is_stolen(rm)) {
                if (in >> 31)
                    e_ldr(rd, CPUREG, rm * 8);
                else
                    emit32(0xB9400000u | ((unsigned)(rm * 2) << 10) | ((unsigned)CPUREG << 5) | (unsigned)rd);
                gpc += 4;
                continue;
            }
        }
        // Exact MOVZ Wd/Xd,#0,LSL#0.
        if (stealfast_on() && (in & 0x7FFFFFE0u) == 0x52800000u && is_stolen(in & 31)) {
            e_str(31, CPUREG, (int)(in & 31) * 8);
            gpc += 4;
            continue;
        }
        // everything else: verbatim,
        int mask = gpr_field_mask(in);
        if (uses_x18(in, mask))
            // unless it names x18 -> mangle
            emit_mangled_x18(in, mask);
        else
            emit32(in);
        gpc += 4;
    }
    if (provenance_fault_capable) jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
    // emit the deferred exit stubs for branches taken inside an exclusive region
    for (int i = 0; i < ndefer; i++) {
        int64_t d = ((uint8_t *)g_cp - (uint8_t *)defer[i].patch) / 4;
        *defer[i].patch = recode_cond(defer[i].in, d);
        emit_chain_exit(defer[i].target);
    }
    chain_exit_dedup_finish();
    // IRQSLIM: the out-of-line poll exit stub the body-entry cbnz targets (irq set -> exit to
    // the dispatcher at the block start, exactly like the legacy inline poll).
    if (g_irq_patch || g_t2_irq_patch) {
        int pad_removed_t2_exit = g_t2_irq_patch != NULL;
        uint8_t *stub = g_cp;
        if (g_irq_patch) {
            uint32_t *p = g_irq_patch;
            g_irq_patch = NULL;
            *p = 0xB5000000u | (((uint32_t)((stub - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16;
        }
        if (g_t2_irq_patch) {
            uint32_t *p = g_t2_irq_patch;
            g_t2_irq_patch = NULL;
            *p = 0xB5000000u | (((uint32_t)((stub - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16;
        }
        uint8_t *exit_begin = g_cp;
        emit_exit_const(start, R_BRANCH);
        size_t exit_bytes = (size_t)(g_cp - exit_begin);
        if (pad_removed_t2_exit)
            for (size_t off = 0; off < exit_bytes; off += 4)
                emit32(0xD503201Fu);
    }
    emit_a64_bus_stub();
    emit_a64_soft_stub();
    size_t emitted_bytes = (size_t)(g_cp - (uint8_t *)host);
    if (emitted_bytes >= (1u << 20))
        HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT,
                "large block guest=%#llx source=%#llx-%#llx bytes=%zu bus_sites=%u soft_sites=%u",
                (unsigned long long)start, (unsigned long long)guest_start, (unsigned long long)guest_end,
                emitted_bytes, g_bus_stub_patch_count, g_soft_stub_patch_count);
    // Only the REGION HEAD (start) is registered; intermediate inlined block-starts are left
    // unregistered so a later mid-region entry self-heals via re-translate + back-patch.
    // W4E tier-2: the promoter (g_tier2_build) recompiles in place and updates the EXISTING map entry
    // itself, so don't insert a duplicate. Expose the body for it.
    g_last_body = body;
    g_last_guest_start = guest_start;
    g_last_guest_end = guest_end;
    if (!g_tier2_build) {
        map_put(start, guest_start, guest_end, host, body);
        // SMC precise gate: record every guest page this block's SOURCE spans, so a later guest `ic ivau`
        // to one of these pages takes the full invalidation while a flush of any never-translated page is
        // skipped. `gpc` is the (exclusive) end of the decoded block here; `start` is its entry.
        txpg_mark(start, guest_end);
    }
    // patch_links_to is MOVED to the dispatcher, AFTER the new block's icache is invalidated:
    // chaining an existing block X -> this new block before its code is icache-coherent on a peer
    // core lets that core fetch stale instructions. Only chain to it once it's visible everywhere.
    return host;
}

#undef STITCH_OK

// W4E tier-2: promote a hot self-loop (its in-cache counter hit the threshold and exited R_TIER2 with
// pc == gpc). Recompile the block with the folded back-edge (no counter), then SWAP it in under live
// execution: emit+icache-flush the tier-2 code, repoint the live map entry, repoint any still-pending
// chains, and drop a stale IBTC entry so the indirect path refills to tier-2. The old tier-1 code is left
// in place (harmless dead bytes). Single-threaded only -- promotion mutates the cache outside the threaded
// translate-lock discipline, so it's skipped once a guest thread exists (loop keeps running tier-1, still
// correct). Caller is the dispatcher between block runs, so guest state is settled.
static void tier2_promote(uint64_t gpc) {
    /*
     * Promotion leaves predecessor bounces and a redirected tier-1 body.  The
     * first SMC wholesale drop makes older promotions unreachable; do not
     * create new cross-block ingress while translations are individually
     * invalidatable.
     */
    if (g_threaded || g_notier2 || smc_seen()) return;
    // Promotion emits a fresh (folded) block at g_cp, but it runs from the dispatcher's post-run reason
    // handling -- OUTSIDE the cache-full check, which only fires on a translate MISS (dispatch.c). A run of
    // hot loops that all reach threshold between two misses promotes back-to-back with NO intervening
    // headroom test, so near a nearly-full cache the emit runs past g_cache+CACHE_SZ and scribbles over
    // whatever the kernel mapped after the arena (guest heap/image) -> a corrupted guest pointer surfaces
    // much later as a wild store (window-gated, common only when the cache churns/flushes often).
    // Demand the same headroom a normal translate does; if it's not there, skip promotion. That is always
    // safe: the loop keeps running its correct tier-1 body (the spent down-counter simply wraps past 0 and
    // stops re-raising R_TIER2), and it can promote later once a flush has reset the arena.
    if (g_cp + CACHE_EMIT_HEADROOM > g_cache + CACHE_SZ) return;
    int mi = map_idx(gpc);
    if (mi < 0) return;
    if (!jit_wprot(0)) return;
    g_emit_start = g_cp;
    g_tier2_build = 1;
    void *nh = translate_block(gpc); // folded recompile; no counter, no map_put
    void *nb = g_last_body;
    g_tier2_build = 0;
    if (0) {
        fprintf(stderr, "[t2dump] gpc=%llx body+%ld:", (unsigned long long)gpc, (long)((uint8_t *)nb - (uint8_t *)nh));
        for (uint32_t *p = (uint32_t *)nb; (uint8_t *)p < g_cp; p++)
            fprintf(stderr, " %08x", *p);
        fprintf(stderr, "\n");
    }
    // make the tier-2 code coherent on all cores BEFORE anything can branch into it
    if (!jit_publish_code(g_emit_start, (size_t)(g_cp - g_emit_start))) {
        (void)jit_wprot(1);
        return;
    }
    // Redirect the OLD tier-1 body to tier-2: overwrite its first instruction with `b nb`. Chains from
    // predecessors were resolved to the old body when they were translated (patch_links_to only fixes
    // still-PENDING ones), so without this an outer loop re-entering this inner loop would keep hitting
    // the spent counter stub. The bounce costs one branch per loop ENTRY (negligible vs the loop body).
    void *old_body = g_map[mi].body;
    int64_t bd = ((uint8_t *)nb - (uint8_t *)old_body) / 4;
    *(uint32_t *)old_body = 0x14000000u | ((uint32_t)bd & 0x3FFFFFFu);
    // IRQSLIM: forward chains enter at body+8 (past the 2-insn poll) and would miss the body+0
    // bounce -- give the poll-skipping entry its own bounce to nb+8 (tier-2 has the same layout).
    if (g_fwdskip) {
        int64_t bd8 = (((uint8_t *)nb + 8) - ((uint8_t *)old_body + 8)) / 4;
        ((uint32_t *)old_body)[2] = 0x14000000u | ((uint32_t)bd8 & 0x3FFFFFFu);
    }
    if (!jit_publish_code(old_body, 4 + (g_fwdskip ? 8 : 0))) {
        (void)jit_wprot(1);
        return;
    }
    // swap the live map entry: future dispatcher lookups + IBTC fills resolve to tier-2 directly
    g_map[mi].host = nh;
    g_map[mi].body = nb;
    // repoint any still-unresolved chains to this gpc straight at the tier-2 body
    patch_links_to(gpc, nb);
    // drop a stale IBTC entry (if this block is an indirect-branch target) so it refills to tier-2
    uint32_t h = (uint32_t)((gpc >> 2) & (IBTC_N - 1));
    if (g_ibtc[h].target == gpc) {
        g_ibtc[h].target = 0;
        g_ibtc[h].body = NULL;
    }
    if (!jit_wprot(1)) return;
    g_prof_t2++;
}
