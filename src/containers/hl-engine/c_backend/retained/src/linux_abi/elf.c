// hl/linux_abi -- the ELF loader (map PT_LOAD high; static-PIE + dynamic via ld.so; build stack).

#include "page.h"
#include "elf_protect.h" // the loader's protection contract, shared with linux_abi/x86.c
#include "image.h"
#include "goimage.h"

// ---------------- minimal ELF loader (load segments HIGH; PC-relative stays valid) ----------------
static uint16_t rd16(const uint8_t *p) {
    return p[0] | (p[1] << 8);
}

static uint32_t rd32(const uint8_t *p) {
    uint32_t v;
    memcpy(&v, p, 4);
    return v;
}

static uint64_t rd64(const uint8_t *p) {
    uint64_t v;
    memcpy(&v, p, 8);
    return v;
}

// Read PT_INTERP (the dynamic loader path) out of an ELF.
static int elf_interp(const char *path, char *out, size_t n) {
    hl_linux_image image;
    if (aarch64_image_read(path, &image) != 0) return -1;
    uint8_t *f = image.bytes;
    int r = -1;
    uint64_t phoff = rd64(f + 32);
    int phnum = rd16(f + 56), phent = rd16(f + 54);
    for (int i = 0; i < phnum; i++) {
        const uint8_t *ph = f + phoff + (size_t)i * phent;
        if (rd32(ph) == 3) {
            // PT_INTERP
            uint64_t off = rd64(ph + 8), fsz = rd64(ph + 32);
            size_t l = fsz < n ? fsz : n - 1;
            memcpy(out, f + off, l);
            out[l] = 0;
            r = 0;
            break;
        }
    }
    hl_linux_image_release(&image);
    return r;
}

// ---------------- non-PIE absolute-DATA fixup (SIGSEGV/SIGBUS guard) ----------------
// A non-PIE ET_EXEC links at a fixed low vaddr with absolute code AND data refs baked in. load_elf maps
// the image HIGH (macOS reserves the low 4 GB via __PAGEZERO) and records the original low link range
// [g_nonpie_lo,g_nonpie_hi) plus the bias to the real mapping. The dispatcher already redirects absolute
// *code* jumps (g_nonpie_*); this handler catches the absolute *data* accesses. On a fault whose access
// address lands in the original low range, it decodes the native arm64 load/store the JIT emitted -- the
// aarch64 frontend copies guest loads/stores nearly verbatim, so the full LDR/STR/LDP/STP/SIMD/LSE family
// can appear -- re-serves the access at addr+bias, applies any base-register writeback, advances the host
// PC past the faulting instruction, and resumes. Inert unless a non-PIE image is loaded (g_nonpie_lo == 0
// for PIE / static-PIE, the only state the test matrix ever sees). A form we cannot decode returns 0 so
// the guard re-raises = a clean abort, never silent wrong data.

// HOST-CPU GATE for the helpers below and nonpie_fixup: it decodes a 4-byte A64 word at HL_HOST_UC_PC, so
// a non-PIE ET_EXEC guest on another host needs the equivalent decoder for that host's ISA in the #else arm.
#if defined(HL_HOST_HAS_A64_CONTEXT)

static int64_t nonpie_sext(uint64_t v, int bits) {
    uint64_t m = 1ull << (bits - 1);
    return (int64_t)((v ^ m) - m);
}

// Atomic RMW helpers (truly atomic, width-typed) used by the LSE/CAS fixup paths below.
static int nonpie_lse_rmw(void *p, int size, int opc, uint64_t v, uint64_t *old) {
    // opc: 0=ADD 1=CLR(&~) 2=EOR 3=SET(|). Returns 1 if handled, 0 for an unsupported subform.
    switch (size) {
#define NP_RMW(TY)                                                                                                     \
    {                                                                                                                  \
        TY *a = (TY *)p, ov = (TY)v, o;                                                                                \
        switch (opc) {                                                                                                 \
        case 0: o = __atomic_fetch_add(a, ov, __ATOMIC_SEQ_CST); break;                                                \
        case 1: o = __atomic_fetch_and(a, (TY)~ov, __ATOMIC_SEQ_CST); break;                                           \
        case 2: o = __atomic_fetch_xor(a, ov, __ATOMIC_SEQ_CST); break;                                                \
        case 3: o = __atomic_fetch_or(a, ov, __ATOMIC_SEQ_CST); break;                                                 \
        default: return 0;                                                                                             \
        }                                                                                                              \
        *old = (uint64_t)o;                                                                                            \
        return 1;                                                                                                      \
    }
    case 0: NP_RMW(uint8_t)
    case 1: NP_RMW(uint16_t)
    case 2: NP_RMW(uint32_t)
    default: NP_RMW(uint64_t)
#undef NP_RMW
    }
}

static uint64_t nonpie_lse_swp(void *p, int size, uint64_t v) {
    switch (size) {
    case 0: return __atomic_exchange_n((uint8_t *)p, (uint8_t)v, __ATOMIC_SEQ_CST);
    case 1: return __atomic_exchange_n((uint16_t *)p, (uint16_t)v, __ATOMIC_SEQ_CST);
    case 2: return __atomic_exchange_n((uint32_t *)p, (uint32_t)v, __ATOMIC_SEQ_CST);
    default: return __atomic_exchange_n((uint64_t *)p, v, __ATOMIC_SEQ_CST);
    }
}

static uint64_t nonpie_cas(void *p, int size, uint64_t expected, uint64_t newv) {
    // Compare-and-swap; returns the pre-CAS memory value. __atomic_compare_exchange_n leaves the loaded
    // value in `e` on failure, and `e` unchanged (== old, since it matched) on success -> `e` is the old
    // value in both cases, which is what cas writes back into Rs.
    switch (size) {
    case 0: {
        uint8_t e = (uint8_t)expected;
        __atomic_compare_exchange_n((uint8_t *)p, &e, (uint8_t)newv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        return e;
    }
    case 1: {
        uint16_t e = (uint16_t)expected;
        __atomic_compare_exchange_n((uint16_t *)p, &e, (uint16_t)newv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        return e;
    }
    case 2: {
        uint32_t e = (uint32_t)expected;
        __atomic_compare_exchange_n((uint32_t *)p, &e, (uint32_t)newv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        return e;
    }
    default: {
        uint64_t e = expected;
        __atomic_compare_exchange_n((uint64_t *)p, &e, newv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        return e;
    }
    }
}

// Software LL/SC monitor for the exclusive-MONITOR pair (LDXR/LDAXR .. STXR/STLXR) served at +bias. The
// guest's verbatim ldxr/stxr execute the real exclusive monitor for HIGH (stack/heap/lib) addresses, but a
// LOW non-PIE image address faults here -- and the two halves arrive as SEPARATE fault handler invocations,
// so the hardware monitor cannot be carried between them. Emulate LL/SC in software, per thread: the load
// records {addr,value}; the store-exclusive linearizes through an atomic CAS(addr, recorded, new) -> it
// succeeds iff memory still holds the recorded value, exactly so two threads racing the same image lock
// cannot both "succeed" (the non-atomic memcpy that preceded this deadlocked musl's threaded a_cas). Cleared
// on any non-exclusive served store to the same granule so a stale reservation cannot wrongly succeed.
static __thread struct {
    uint64_t addr; // host (high) address reserved by the last LL, 0 = no reservation
    uint64_t val;  // value observed by the LL (size-masked); low word for a pair reservation
    uint64_t val2; // second word observed by a PAIR LL (LDXP/LDAXP)
    int size;      // access width log2 (0=B 1=H 2=W 3=X); 4 = 64-bit pair (16B), 5 = 32-bit pair (8B)
} g_llsc;

static int nonpie_sc(uint64_t real, int size, uint64_t newv, uint64_t *llval) {
    // returns 1 if the store-exclusive succeeds (status 0), 0 if it fails (status 1).
    if (g_llsc.addr != real || g_llsc.size != size) return 0;
    g_llsc.addr = 0; // a store-exclusive always closes the reservation, success or fail
    return nonpie_cas((void *)real, size, *llval, newv) == *llval;
}

// zero-extend a `size`-byte value to register width (matches W-register upper-32 clearing for size<8).
static uint64_t nonpie_zext(uint64_t v, int size) {
    return size >= 3 ? v : (v & ((1ull << (8 << size)) - 1));
}

static int nonpie_fixup(siginfo_t *si, void *ucv) {
    if (!g_nonpie_lo || !ucv || !si) return 0;
    uint64_t va = (uint64_t)si->si_addr;
    if (va < g_nonpie_lo || va >= g_nonpie_hi) return 0;
    ucontext_t *uc = (ucontext_t *)ucv;
    uint32_t insn = *(uint32_t *)(HL_HOST_UC_PC(uc));
    uint64_t real = va + g_nonpie_bias; // the datum's real (high) mapped location
    uint64_t *X = HL_HOST_UC_REGS(uc);
    __uint128_t *V = HL_HOST_UC_VREGS(uc);
    int rt = insn & 0x1F;

    // ---- DC ZVA (data-cache zero by VA): zero the DCZID_EL0-sized block containing the faulting addr.
    //      glibc's memset streams large zero-fills through `dc zva`; on the non-PIE image's .bss this
    //      faults at the low link address. The guest sized its loop from the host DCZID_EL0 (the frontend
    //      emits the mrs verbatim), so re-derive the same block size here and zero it at +bias.
    if ((insn & 0xFFFFFFE0u) == 0xD50B7420u) {
        uint64_t bs = 4ull << (g_aarch64_cpu_model.dczid_el0 & 0xf);
        memset((void *)(real & ~(bs - 1)), 0, (size_t)bs);
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- Load/store PAIR (LDP/STP, GPR + SIMD; no-alloc / offset / pre / post). (insn&0x3a000000)==0x28000000.
    if ((insn & 0x3a000000u) == 0x28000000u) {
        int v = (insn >> 26) & 1; // SIMD&FP pair
        int load = (insn >> 22) & 1;
        int op2 = (insn >> 23) & 3; // 00=no-alloc 01=post 10=offset 11=pre
        int opc = insn >> 30;       // GPR: 00=32b 01=LDPSW 10=64b ; SIMD: 00=S 01=D 10=Q
        int rt2 = (insn >> 10) & 0x1F;
        int bytes = v ? (4 << opc) : (opc == 2 ? 8 : 4);
        if (v) { // SIMD pair
            if (load) {
                __uint128_t a = 0, b = 0;
                memcpy(&a, (void *)real, (size_t)bytes);
                memcpy(&b, (void *)(real + bytes), (size_t)bytes);
                V[rt] = a;
                V[rt2] = b;
            } else {
                __uint128_t a = V[rt], b = V[rt2];
                memcpy((void *)real, &a, (size_t)bytes);
                memcpy((void *)(real + bytes), &b, (size_t)bytes);
            }
        } else { // GPR pair (LDPSW sign-extends each 32b element to 64b)
            if (load) {
                uint64_t a = 0, b = 0;
                memcpy(&a, (void *)real, (size_t)bytes);
                memcpy(&b, (void *)(real + bytes), (size_t)bytes);
                if (opc == 1) {
                    a = (uint64_t)nonpie_sext(a, 32);
                    b = (uint64_t)nonpie_sext(b, 32);
                }
                if (rt != 31) X[rt] = a;
                if (rt2 != 31) X[rt2] = b;
            } else {
                uint64_t a = (rt == 31) ? 0 : X[rt], b = (rt2 == 31) ? 0 : X[rt2];
                memcpy((void *)real, &a, (size_t)bytes);
                memcpy((void *)(real + bytes), &b, (size_t)bytes);
            }
        }
        if (op2 == 1 || op2 == 3) { // writeback: post -> Xn=va+off, pre -> Xn=va (keep guest addr, not biased)
            int rn = (insn >> 5) & 0x1F;
            int64_t off = nonpie_sext((insn >> 15) & 0x7F, 7) * bytes;
            X[rn] = (op2 == 1) ? va + off : va;
        }
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- LDAPR/LDAPRB/LDAPRH (Load-Acquire RCpc register): size[31:30] 111 0 00 1 0 1 11111 1 100 00 Rn Rt.
    //      C++20 std::atomic<T>::load(memory_order_acquire) on FEAT_LSE2/RCPC hardware (clickhouse and other
    //      modern binaries) lowers to LDAPR, not LDAR. Its encoding aliases the LSE atomic-RMW box below
    //      (opc==4/o3==1 there -> a bogus "min/max" abort), so decode it FIRST as the plain acquire load it
    //      is: [Xn] with no operand, Rt receives the value. Served as a SEQ_CST atomic load at +bias. ----
    if ((insn & 0x3FA0FC00u) == 0x38A0C000u) {
        int size = insn >> 30;
        uint64_t val = 0;
        switch (size) {
        case 0: val = __atomic_load_n((uint8_t *)real, __ATOMIC_ACQUIRE); break;
        case 1: val = __atomic_load_n((uint16_t *)real, __ATOMIC_ACQUIRE); break;
        case 2: val = __atomic_load_n((uint32_t *)real, __ATOMIC_ACQUIRE); break;
        default: val = __atomic_load_n((uint64_t *)real, __ATOMIC_ACQUIRE); break;
        }
        if (rt != 31) X[rt] = nonpie_zext(val, size);
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- LSE atomic RMW: size[31:30] 111 0 00 A R 1 Rs[20:16] o3[15] opc[14:12] 00 Rn[9:5] Rt[4:0] ----
    if ((insn & 0x3F200C00u) == 0x38200000u) {
        int size = insn >> 30, o3 = (insn >> 15) & 1, opc = (insn >> 12) & 7;
        int rs = (insn >> 16) & 0x1F;
        uint64_t operand = (rs == 31) ? 0 : X[rs], old;
        if (o3 && opc == 0) { // swp: x = [m]; [m] = operand
            old = nonpie_lse_swp((void *)real, size, operand);
        } else if (!o3 && opc < 4) { // ldadd / ldclr / ldeor / ldset
            if (!nonpie_lse_rmw((void *)real, size, opc, operand, &old)) return 0;
        } else {
            return 0; // signed/unsigned min/max -> clean abort
        }
        if (rt != 31) X[rt] = nonpie_zext(old, size); // Rt receives the old value
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- CAS/CASA/CASL/CASAL (B/H/W/X): size[31:30] 001000 1 A 1 Rs[20:16] R 11111 Rn[9:5] Rt[4:0].
    //      A(bit22)/R(bit15) are the acquire/release flavors (all served as SEQ_CST). Rs=cmp in / old out. ----
    if ((insn & 0x3FA07C00u) == 0x08A07C00u) {
        int size = insn >> 30, rs = (insn >> 16) & 0x1F;
        uint64_t expected = (rs == 31) ? 0 : X[rs], newv = (rt == 31) ? 0 : X[rt];
        uint64_t old = nonpie_cas((void *)real, size, expected, newv);
        if (rs != 31) X[rs] = nonpie_zext(old, size); // Rs receives the old value
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- Load/store exclusive + ordered, single register: LDXR/STXR/LDAXR/STLXR/LDAR/STLR/LDLAR/STLLR.
    //      bits[29:24]==001000, o1(bit21)==0 (the CAS o1==1 forms are handled above; pair STXP/LDXP/CASP,
    //      o1==1, are rare and left to clean-abort). The monitor pair (o2==0) is served as a software LL/SC
    //      (per-thread reservation + atomic CAS at +bias) so two threads racing the same low image lock can
    //      never both succeed; the ordered o2==1 forms are a plain atomic load/store (no reservation). ----
    if ((insn & 0x3F200000u) == 0x08000000u) {
        int size = insn >> 30, o2 = (insn >> 23) & 1, load = (insn >> 22) & 1, rs = (insn >> 16) & 0x1F;
        if (o2) { // LDAR/STLR/LDLAR/STLLR: ordered, NOT exclusive -> plain atomic load/store, no monitor.
            if (load) {
                uint64_t val = 0;
                memcpy(&val, (void *)real, (size_t)(1 << size));
                if (rt != 31) X[rt] = nonpie_zext(val, size);
            } else {
                uint64_t val = (rt == 31) ? 0 : X[rt];
                memcpy((void *)real, &val, (size_t)(1 << size));
            }
        } else if (load) { // LDXR/LDAXR: open a software reservation on the granule.
            uint64_t val = 0;
            switch (size) {
            case 0: val = __atomic_load_n((uint8_t *)real, __ATOMIC_ACQUIRE); break;
            case 1: val = __atomic_load_n((uint16_t *)real, __ATOMIC_ACQUIRE); break;
            case 2: val = __atomic_load_n((uint32_t *)real, __ATOMIC_ACQUIRE); break;
            default: val = __atomic_load_n((uint64_t *)real, __ATOMIC_ACQUIRE); break;
            }
            g_llsc.addr = real;
            g_llsc.val = val;
            g_llsc.size = size;
            if (rt != 31) X[rt] = nonpie_zext(val, size);
        } else { // STXR/STLXR: close the reservation via an atomic CAS -> status 0 (ok) / 1 (fail).
            uint64_t newv = (rt == 31) ? 0 : X[rt];
            int ok = nonpie_sc(real, size, newv, &g_llsc.val);
            if (rs != 31) X[rs] = ok ? 0 : 1;
        }
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- Exclusive PAIR: LDXP/LDAXP/STXP/STLXP. bit30=element width (0=32-bit pair/8B, 1=64-bit pair/16B),
    //      bit22=L (load). These fell through the single-register handler above (o1==0 there) and every other
    //      case -> return 0 -> the low non-PIE fault re-raised on the SAME instruction forever (the observed
    //      hang). Serve them as a software 128-bit (or 64-bit) LL/SC, mirroring the single-register monitor. ----
    if ((insn & 0xBFA00000u) == 0x88200000u) {
        int is64 = (insn >> 30) & 1, load = (insn >> 22) & 1, rs = (insn >> 16) & 0x1F, rt2 = (insn >> 10) & 0x1F;
        if (is64) {     // 16-byte pair
            if (load) { // LDXP/LDAXP: open a 128-bit reservation
                unsigned __int128 v = __atomic_load_n((unsigned __int128 *)real, __ATOMIC_ACQUIRE);
                g_llsc.addr = real, g_llsc.size = 4, g_llsc.val = (uint64_t)v, g_llsc.val2 = (uint64_t)(v >> 64);
                if (rt != 31) X[rt] = (uint64_t)v;
                if (rt2 != 31) X[rt2] = (uint64_t)(v >> 64);
            } else { // STXP/STLXP: CAS the whole 16 bytes against the reservation -> status 0 (ok) / 1 (fail)
                int ok = 0;
                if (g_llsc.addr == real && g_llsc.size == 4) {
                    g_llsc.addr = 0;
                    unsigned __int128 exp = ((unsigned __int128)g_llsc.val2 << 64) | g_llsc.val;
                    unsigned __int128 nv =
                        ((unsigned __int128)((rt2 == 31) ? 0 : X[rt2]) << 64) | ((rt == 31) ? 0 : X[rt]);
                    ok = __atomic_compare_exchange_n((unsigned __int128 *)real, &exp, nv, 0, __ATOMIC_SEQ_CST,
                                                     __ATOMIC_SEQ_CST);
                }
                if (rs != 31) X[rs] = ok ? 0 : 1;
            }
        } else {        // 8-byte pair (two 32-bit words: Rt=low, Rt2=high)
            if (load) { // LDXP/LDAXP
                uint64_t v = __atomic_load_n((uint64_t *)real, __ATOMIC_ACQUIRE);
                g_llsc.addr = real, g_llsc.size = 5, g_llsc.val = v;
                if (rt != 31) X[rt] = (uint32_t)v;
                if (rt2 != 31) X[rt2] = (uint32_t)(v >> 32);
            } else { // STXP/STLXP
                int ok = 0;
                if (g_llsc.addr == real && g_llsc.size == 5) {
                    g_llsc.addr = 0;
                    uint64_t exp = g_llsc.val;
                    uint64_t nv =
                        ((uint64_t)(uint32_t)((rt2 == 31) ? 0 : X[rt2]) << 32) | (uint32_t)((rt == 31) ? 0 : X[rt]);
                    ok = __atomic_compare_exchange_n((uint64_t *)real, &exp, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                }
                if (rs != 31) X[rs] = ok ? 0 : 1;
            }
        }
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- CASP/CASPA/CASPL/CASPAL (compare-and-swap PAIR): Rs:Rs+1 = comparison in / old out, Rt:Rt+1 = swap.
    //      bit30 = element width (0=32-bit pair, 1=64-bit pair). Same hang class as the exclusive pair above. ----
    if ((insn & 0xBFA07C00u) == 0x08207C00u) {
        int is64 = (insn >> 30) & 1, rs = (insn >> 16) & 0x1F;
#define NP_XR(n) (((n) == 31) ? 0 : X[(n)])
        if (is64) { // 16-byte pair
            unsigned __int128 exp = ((unsigned __int128)NP_XR(rs + 1) << 64) | NP_XR(rs);
            unsigned __int128 nv = ((unsigned __int128)NP_XR(rt + 1) << 64) | NP_XR(rt);
            __atomic_compare_exchange_n((unsigned __int128 *)real, &exp, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
            if (rs != 31) X[rs] = (uint64_t)exp; // old value written back to Rs:Rs+1
            if ((rs + 1) != 31) X[rs + 1] = (uint64_t)(exp >> 64);
        } else { // 8-byte pair (two 32-bit words)
            uint64_t exp = ((uint64_t)(uint32_t)NP_XR(rs + 1) << 32) | (uint32_t)NP_XR(rs);
            uint64_t nv = ((uint64_t)(uint32_t)NP_XR(rt + 1) << 32) | (uint32_t)NP_XR(rt);
            __atomic_compare_exchange_n((uint64_t *)real, &exp, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
            if (rs != 31) X[rs] = (uint32_t)exp;
            if ((rs + 1) != 31) X[rs + 1] = (uint32_t)(exp >> 32);
        }
#undef NP_XR
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- AdvSIMD load/store MULTIPLE structures (LD1/ST1 contiguous AND LD2/3/4 / ST2/3/4 interleaved).
    //      glibc's NEON memcpy/memmove/memset/strlen stream the non-PIE image's absolute data through these.
    //      Encoding: bit31=0, bits[29:24]=001100, bit23=post-index, bits[21:16]=Rm/0, opcode[15:12],
    //      size[11:10], Rn[9:5], Rt[4:0]. opcode selects the structure form + register count; the LDn/STn
    //      (n>1) forms de-interleave/interleave by element. Q=bit30 -> 16B regs else 8B.
    if ((insn & 0xBFBF0000u) == 0x0C000000u || (insn & 0xBFA00000u) == 0x0C800000u) {
        int post = (insn >> 23) & 1, q = (insn >> 30) & 1, load = (insn >> 22) & 1, opc = (insn >> 12) & 0xF;
        int regs, interleave;
        switch (opc) {
        case 0x7:
            regs = 1;
            interleave = 0;
            break; // LD1/ST1 x1
        case 0xA:
            regs = 2;
            interleave = 0;
            break; // LD1/ST1 x2
        case 0x6:
            regs = 3;
            interleave = 0;
            break; // LD1/ST1 x3
        case 0x2:
            regs = 4;
            interleave = 0;
            break; // LD1/ST1 x4
        case 0x8:
            regs = 2;
            interleave = 1;
            break; // LD2/ST2
        case 0x4:
            regs = 3;
            interleave = 1;
            break; // LD3/ST3
        case 0x0:
            regs = 4;
            interleave = 1;
            break;         // LD4/ST4
        default: return 0; // unallocated -> clean abort
        }
        int regbytes = q ? 16 : 8;
        if (!interleave) { // contiguous: whole registers back-to-back
            for (int i = 0; i < regs; i++) {
                int r = (rt + i) & 31;
                if (load) {
                    __uint128_t z = 0;
                    memcpy(&z, (void *)(real + (size_t)i * regbytes), (size_t)regbytes);
                    V[r] = z;
                } else {
                    __uint128_t s = V[r];
                    memcpy((void *)(real + (size_t)i * regbytes), &s, (size_t)regbytes);
                }
            }
        } else { // interleaved: element e of register r lives at memory slot (e*regs + r)
            int esize = 1 << ((insn >> 10) & 3);
            int nelem = regbytes / esize;
            if (load) {
                __uint128_t acc[4] = {0, 0, 0, 0};
                for (int e = 0; e < nelem; e++)
                    for (int r = 0; r < regs; r++)
                        memcpy((uint8_t *)&acc[r] + (size_t)e * esize, (void *)(real + (size_t)(e * regs + r) * esize),
                               (size_t)esize);
                for (int r = 0; r < regs; r++)
                    V[(rt + r) & 31] = acc[r];
            } else {
                for (int e = 0; e < nelem; e++)
                    for (int r = 0; r < regs; r++) {
                        __uint128_t s = V[(rt + r) & 31];
                        memcpy((void *)(real + (size_t)(e * regs + r) * esize), (uint8_t *)&s + (size_t)e * esize,
                               (size_t)esize);
                    }
            }
        }
        if (post) { // post-index writeback: Xn = guest addr + (Rm==31 ? bytes transferred : Xm)
            int rn = (insn >> 5) & 0x1F, rm = (insn >> 16) & 0x1F;
            X[rn] = va + (rm == 31 ? (uint64_t)(regs * regbytes) : X[rm]);
        }
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- AdvSIMD load/store SINGLE structure (one lane, or load-and-replicate): bit31=0, bits[29:24]=001101.
    //      Covers LD1/ST1..LD4/ST4 to/from one lane and LD1R/LD2R/LD3R/LD4R. Go's runtime broadcasts constants
    //      through `ld4r`/`ld1r` and indexes lanes via LD1[i]; on the non-PIE image these land on the low link
    //      address. selem (1..4 consecutive V regs) = ((opcode[0]<<1)|R)+1. The element size / lane index follow
    //      the ARM single-structure tables: replicate (opcode 11x) uses size as the element width; the lane
    //      forms key off opcode[2:1] (00=B 01=H 10=S/D) with the index packed into Q:S:size.
    if ((insn & 0xBF000000u) == 0x0D000000u) {
        int q = (insn >> 30) & 1, post = (insn >> 23) & 1, load = (insn >> 22) & 1, r = (insn >> 21) & 1;
        int opcode = (insn >> 13) & 7, s = (insn >> 12) & 1, size = (insn >> 10) & 3;
        int selem = ((opcode & 1) << 1 | r) + 1;
        int esize, index;
        if ((opcode >> 1) == 3) { // LD#R replicate (load only): element width = size, fills every lane
            if (!load) return 0;
            esize = 1 << size;
            int regbytes = q ? 16 : 8, lanes = regbytes / esize;
            for (int i = 0; i < selem; i++) {
                uint8_t elem[8] = {0};
                memcpy(elem, (void *)(real + (size_t)i * esize), (size_t)esize);
                __uint128_t acc = 0;
                for (int l = 0; l < lanes; l++)
                    memcpy((uint8_t *)&acc + (size_t)l * esize, elem, (size_t)esize);
                V[(rt + i) & 31] = acc;
            }
        } else { // single lane: opcode[2:1] selects width, the index is packed into Q:S:size
            switch (opcode >> 1) {
            case 0:
                esize = 1;
                index = (q << 3) | (s << 2) | size;
                break; // B
            case 1:
                esize = 2;
                index = (q << 2) | (s << 1) | (size >> 1);
                break; // H
            default:   // S (size==x0) or D (size==01)
                if ((size & 1) == 0) {
                    esize = 4;
                    index = (q << 1) | s;
                } else {
                    esize = 8;
                    index = q;
                }
                break;
            }
            for (int i = 0; i < selem; i++) {
                uint8_t *lane = (uint8_t *)&V[(rt + i) & 31] + (size_t)index * esize;
                void *mem = (void *)(real + (size_t)i * esize);
                if (load)
                    memcpy(lane, mem, (size_t)esize);
                else
                    memcpy(mem, lane, (size_t)esize);
            }
        }
        if (post) { // post-index writeback: Xn = guest addr + (Rm==31 ? bytes transferred : Xm)
            int rn = (insn >> 5) & 0x1F, rm = (insn >> 16) & 0x1F;
            X[rn] = va + (rm == 31 ? (uint64_t)(selem * esize) : X[rm]);
        }
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- Load/store register, single (unsigned-offset / unscaled / pre / post / unpriv / register-offset).
    //      bits[29:27]==111; V=bit26. Addressing: bit24=scaled-unsigned, else bit21=register-offset, and
    //      bits[11:10] select unscaled(00)/post(01)/unpriv(10)/pre(11). Reject anything else (clean abort).
    if (((insn >> 27) & 7) != 7) return 0;
    int v = (insn >> 26) & 1;
    int scaled = (insn >> 24) & 1;
    int regoff = (insn >> 21) & 1;
    int mode = (insn >> 10) & 3;
    if (!scaled && regoff && mode != 2) return 0; // unallocated / atomic subform we don't decode
    int size = insn >> 30, opc = (insn >> 22) & 3;

    if (v) { // SIMD&FP single. width = 1<<((opc[1]<<2)|size): B1 H2 S4 D8 Q16. opc[0]=load.
        int bytes = 1 << (((opc >> 1) << 2) | size);
        if (opc & 1) {
            __uint128_t z = 0;
            memcpy(&z, (void *)real, (size_t)bytes);
            V[rt] = z;
        } else {
            __uint128_t s = V[rt];
            memcpy((void *)real, &s, (size_t)bytes);
        }
    } else if (opc == 0) { // integer store: rt's low `size` bytes
        uint64_t val = (rt == 31) ? 0 : X[rt];
        memcpy((void *)real, &val, (size_t)(1 << size));
    } else if (size == 3 && opc == 2) { // PRFM (prefetch hint) -- no transfer
        // fall through to PC advance
    } else { // integer load: 01=zero-ext, 10=sign-ext to 64, 11=sign-ext to 32 (W, upper zeroed)
        uint64_t val = 0;
        memcpy(&val, (void *)real, (size_t)(1 << size));
        if (opc == 2)
            val = (uint64_t)nonpie_sext(val, 8 << size);
        else if (opc == 3)
            val = (uint32_t)nonpie_sext(val, 8 << size);
        if (rt != 31) X[rt] = val;
    }
    if (!scaled && !regoff && (mode == 1 || mode == 3)) { // pre/post writeback (imm9): keep the guest addr
        int rn = (insn >> 5) & 0x1F;
        int64_t off = nonpie_sext((insn >> 12) & 0x1FF, 9);
        X[rn] = (mode == 1) ? va + off : va; // post -> Xn=va+imm, pre -> Xn=va
    }
    HL_HOST_UC_PC(uc) += 4;
    return 1;
}

#else

// Not an AArch64 host: nothing to decode. 0 ("not handled") keeps nonpie_guard's crash path.
static int nonpie_fixup(siginfo_t *si, void *ucv) {
    (void)si;
    (void)ucv;
    return 0;
}

#endif

// SIGSEGV/SIGBUS guard installed on the normal aarch64 run path. Serves a non-PIE absolute data access at
// +bias (nonpie_fixup); anything else re-raises with the default action (a real crash). Inert for PIE.
static void nonpie_guard(int sig, siginfo_t *si, void *uc) {
    // host_range_mapped's fault-guarded probe (thread.c): a probe load on an unmapped guest page long-jumps
    // back to report "unmapped" -> the syscall returns -EFAULT. MUST run first: nonpie_fixup would otherwise
    // emulate a probe load of a low un-rebased pointer at +bias and resume, mis-reporting it as mapped.
    if (hrm_fault_hook(si)) return; // never actually returns on a claim (siglongjmp); shape-only
    if (nonpie_fixup(si, uc)) return;
    // The host representation may be narrower than the guest mapping when 4 KiB ELF segment edges share
    // a 16 KiB macOS page, or after fork-server protection restoration. Re-open only a logically writable,
    // tracked private-anonymous guest page; PROT_NONE/read-only and file-EOF ledgers remain authoritative.
    // This is a representation repair, not lazy allocation: the address must already belong to gmap.
    if (si && si->si_addr) {
        uint64_t address = (uint64_t)si->si_addr;
        int anonymous_protection = anon_prot_if_contained(address, 1);
        int bus_address = 0;
        for (int i = 0; i < g_ngbus; i++)
            if (address >= g_gbus[i].lo && address < g_gbus[i].hi) bus_address = 1;
        hl_host_region region;
        if ((anonymous_protection & PROT_WRITE) && hl_gmap_contains(address, 1) && !gna_hit(address, 1) &&
            !gro_hit(address, 1) && !bus_address && hl_host_region_query((uintptr_t)address, &region) &&
            (region.protection & HL_HOST_REGION_READ) && !(region.protection & HL_HOST_REGION_WRITE)) {
            size_t page = hl_host_page_size();
            if (page) {
                uintptr_t base = (uintptr_t)address & ~(uintptr_t)(page - 1);
                if (mprotect((void *)base, page, PROT_READ | PROT_WRITE) == 0) return;
            }
        }
    }
    // A genuine guest fault (wild pointer / null deref / stack overflow into the guard gap) with a
    // registered guest handler is the guest's to handle: synthesize+deliver the guest signal. nonpie_fixup
    // (absolute-data) already won above.
    if (deliver_guest_fault(sig, si, uc)) return;
    // DIAGNOSTIC (gated, async-signal-safe, NO user-pointer deref): every no-handler fault about to be
    // turned into a fatal guest termination or re-raised. Prints sig / host PC / fault addr / host SP +
    // whether the host PC is inside the RX code cache (1 = a wild jump in translated guest code).
    if (0) {
        extern int jit_pc_in_cache(uint64_t pc, uint64_t *base);
        ucontext_t *u = (ucontext_t *)uc;
        uint64_t hpc = u ? (uint64_t)HL_HOST_UC_PC(u) : 0;
        uint64_t hsp = u ? (uint64_t)HL_HOST_UC_SP(u) : 0;
        uint64_t rxb = 0;
        int inc = jit_pc_in_cache(hpc, &rxb);
        char b[224];
        int o = 0;
        const char *H = "0123456789abcdef";
        memcpy(b, "[HL-ENGINE-FAULT] sig=", 22);
        o = 22;
        b[o++] = '0' + (sig / 10 % 10);
        b[o++] = '0' + (sig % 10);

        struct {
            const char *l;
            uint64_t v;
        } F[] = {{" hpc=0x", hpc}, {" fault=0x", (uint64_t)si->si_addr}, {" hsp=0x", hsp}, {" rxbase=0x", rxb}};

        for (int f = 0; f < 4; f++) {
            for (const char *L = F[f].l; *L;)
                b[o++] = *L++;
            for (int i = 15; i >= 0; i--)
                b[o++] = H[(F[f].v >> (i * 4)) & 0xf];
        }
        memcpy(b + o, " incache=", 9);
        o += 9;
        b[o++] = '0' + inc;
        b[o++] = '\n';
        if (write(2, b, o) < 0) {}
        // Name the faulting engine function (dladdr is not strictly async-signal-safe, but we _exit right
        // after, so a rare lock stall is acceptable -- this only runs while engine-fault diagnostics are enabled).
        Dl_info di;
        if (hpc && dladdr((void *)hpc, &di) && di.dli_sname) {
            char c[160];
            int p = 0;
            memcpy(c, "[HL-ENGINE-FAULT] fn=", 21);
            p = 21;
            for (const char *s = di.dli_sname; *s && p < 140; s++)
                c[p++] = *s;
            memcpy(c + p, " +0x", 4);
            p += 4;
            uint64_t off = hpc - (uint64_t)di.dli_saddr;
            for (int i = 12; i >= 0; i -= 4)
                c[p++] = H[(off >> i) & 0xf];
            c[p++] = '\n';
            if (write(2, c, p) < 0) {}
        }
    }
    // no guest handler -> a fatal, unmaskable synchronous fault. Terminate the guest process through
    // hl's fatal-signal machinery so its parent's wait4 sees WIFSIGNALED/WTERMSIG=sig (a raw host raise()
    // degrades to exit(255) across hl's fork). Declines (returns 0) for a genuine ENGINE fault -> re-raise.
    if (deliver_guest_fatal_fault(sig, si, uc)) return;
    signal(sig, SIG_DFL);
    raise(sig);
}

// Synchronous CPU faults other than SIGSEGV/SIGBUS (which the Linux guest entry wires above): a guest
// may install a handler for SIGILL/SIGFPE/SIGTRAP and DELIBERATELY trigger it -- the canonical case is a
// CPU-feature probe (ring/OpenSSL/musl) that executes an optional instruction guarded by a SIGILL handler
// and falls back when it traps. The aarch64 frontend emits such instructions verbatim, so on a host CPU
// missing the extension (e.g. Apple Silicon has no SM3/SM4) they raise a real host SIGILL. rt_sigaction
// records the guest handler but does not install a host handler for synchronous signals (they are served by
// the guards installed here), so without this the trap is fatal instead of reaching the guest's handler.
// nonpie_guard already routes any signal to deliver_guest_fault (nonpie_fixup self-declines: its si_addr is
// the high faulting PC, never in the low link range), so reuse it. CRASHDBG handles these via its mach
// exception port + diag_crash instead, so leave its diagnostics untouched.
static void install_sync_fault_guards(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = nonpie_guard;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK; // share the guards' per-thread altstack (host SP==guest SP)
    sigaction(SIGILL, &sa, NULL);
    sigaction(SIGFPE, &sa, NULL);
    sigaction(SIGTRAP, &sa, NULL);
}

#ifndef HL_EMBEDDED_BUILD
__attribute__((constructor)) static void install_sync_fault_guards_constructor(void) {
    install_sync_fault_guards();
}
#endif

// ---------------- LOAD-path mapping hardening (never leave an unmapped hole in the image) -----
// A large non-PIE ET_EXEC (e.g. gcc's ~29MB cc1, first PT_LOAD at a fixed low vaddr) maps its whole image
// span in one anon reservation and then mprotects it per segment. An mmap here can fail nondeterministically
// (host-VM address-space fragmentation leaving no contiguous span, engine mmap-pool degradation, or memory
// pressure -> XNU returns ENOMEM). The loader must NEVER continue past such a failure: an unmapped hole in
// the image surfaces much later as a hard-to-diagnose SIGSEGV on the guest's own text/data(the
// fault landed INSIDE the first LOAD segment). Retry a bounded number of times (a transient shortage can
// clear within a few ms), then fail the exec LOUDLY with a clean diagnostic rather than leaving a hole.
#define ELF_MAP_RETRIES 6

// mmap (anon; fd=-1) with bounded retry under transient pressure. Never returns MAP_FAILED: on
// persistent failure it reports the exact request and aborts, so a valid exec dies with a clean error
// rather than continuing with an unmapped hole in the guest image.
static hl_host_memory_mapping elf_map_checked(void *hint, size_t len, uint32_t protection, uint32_t flags,
                                              const char *what) {
    const hl_host_services *host = effective_host_services();
    for (int t = 0;; t++) {
        hl_host_memory_mapping mapped = {HL_HOST_MEMORY_MAPPING_ABI, sizeof(mapped), 0, 0, 0, 0};
        hl_host_result result =
            host->memory->map_anonymous(host->context, (uint64_t)(uintptr_t)hint, len, protection, flags, &mapped);
        if (result.status == HL_STATUS_OK) return mapped;
        if (t >= ELF_MAP_RETRIES) {
            fprintf(stderr, "hl-engine: load_elf: cannot map %s (%zu bytes) for the guest image (host status %d)\n",
                    what, len, result.status);
            exit(1);
        }
        usleep(2000u << t); // back off and let transient pressure clear
    }
}

// Narrow a mapped range's protection -- BEST-EFFORT, never fatal. Used for the stack guard gap; the image
// segments go through hl_elf_protect_segments, which owes the read-only registry the same answer. A range
// left un-narrowed stays readable+writable = still fully backed, never a hole. Two failures are expected
// and benign: EINVAL (segment bounds round to 4 KB but
// Apple-Silicon mprotect needs 16 KB-aligned ranges, so adjacent segments share a host page) and, under
// memory pressure, ENOMEM (XNU cannot allocate a vm_map_entry to split the span). Retry only the
// transient ENOMEM a few times so the tightening still applies once pressure clears; give up quietly on
// anything else -- matching the original best-effort mprotect, so no working image regresses.
static void elf_mprotect_besteffort(const hl_host_memory_mapping *mapping, void *addr, size_t len, uint32_t protection,
                                    const char *what) {
    (void)what;
    size_t host_page = hl_host_page_size();
    uint64_t start = (uint64_t)(uintptr_t)addr;
    uint64_t end = start + len;
    // Guest ELF segments are described at guest-page granularity. A host protection change rounds to
    // macOS's larger VM pages, so an unaligned segment edge can make an adjacent writable .data/.bss
    // subpage read-only (CoreCLR performs such a relocation during startup). The old mprotect path rejected
    // these ranges with EINVAL. Preserve that safe behavior and tighten only independently protectable pages.
    if (!host_page || (start & (host_page - 1)) || (end & (host_page - 1))) return;
    const hl_host_services *host = effective_host_services();
    for (int t = 0;; t++) {
        uint64_t offset = (uint64_t)(uintptr_t)addr - mapping->address;
        hl_host_result result = host->memory->protect(host->context, mapping->handle, offset, len, protection);
        if (result.status == HL_STATUS_OK || result.status != HL_STATUS_OUT_OF_MEMORY || t >= ELF_MAP_RETRIES) return;
        usleep(2000u << t); // transient pressure: back off and re-tighten
    }
}

static void load_elf(const char *path, struct loaded *out) {
    hl_linux_image image;
    if (aarch64_image_read(path, &image) != 0) {
        fprintf(stderr, "hl-engine: cannot read guest ELF %s through host services\n", path);
        exit(1);
    }
    uint8_t *f = image.bytes;
    // Refuse a foreign-arch ELF up front: this engine only translates aarch64 (e_machine==EM_AARCH64).
    // Without this guard an x86-64 image's bytes are decoded as aarch64 instructions -- the translator
    // runs off into a zero/garbage region and dies deep inside translate_block with a cryptic SIGSEGV.
    // (The x86-64 image is the x86_64 engine's job; the daemon/test harness route by the rootfs's arch.)
    uint16_t e_machine = rd16(f + 18);
    if (e_machine != 0xB7) { // EM_AARCH64
        fprintf(stderr,
                "hl-engine: %s: ELF e_machine=0x%x is not aarch64 (EM_AARCH64=0xb7) -- wrong engine for this image\n",
                path, e_machine);
        exit(1);
    }
    uint64_t e_entry = rd64(f + 24), phoff = rd64(f + 32);
    int phnum = rd16(f + 56), phentsize = rd16(f + 54);
    uint64_t minv = ~0ull, maxv = 0;
    for (int i = 0; i < phnum; i++) {
        uint8_t *ph = f + phoff + (uint64_t)i * phentsize;
        // PT_LOAD
        if (rd32(ph) != 1) continue;
        uint64_t v = rd64(ph + 16), msz = rd64(ph + 40);
        if (v < minv) minv = v;
        if (v + msz > maxv) maxv = v + msz;
    }
    uint64_t basepage = minv & ~0xFFFull;
    uint64_t span = (maxv - basepage + 0xFFFF) & ~0xFFFFull;
    int etype = rd16(f + 16);
    // NULL: non-colliding (main + interp). A non-PIE ET_EXEC that could NOT take the link address gets
    // biased here; the dispatcher redirects its absolute code jumps (g_nonpie_*) and the nonpie_guard
    // SIGSEGV handler re-serves its absolute DATA refs to the low link vaddr at +bias (nonpie_fixup above).
    // Map the whole image span [basepage, basepage+span) in one anon reservation, then copy each PT_LOAD
    // and narrow protections per segment below. elf_map_checked retries under transient host memory
    // pressure and aborts loudly on persistent failure, so the full range is guaranteed backed here (a
    // partial/failed map never slips through to become a SIGSEGV on the guest's own text/data).
    hl_host_memory_mapping image_mapping = {HL_HOST_MEMORY_MAPPING_ABI, sizeof(image_mapping), 0, 0, 0, 0};
    uint8_t *base = NULL;
    // Placement: thread.c, "non-PIE image placement, per host". On Linux an ET_EXEC goes AT its link
    // address, so bias == 0 and nothing below arms. The link address is deterministic across runs, which
    // is all the g_force_base / snapshot-arena hints below exist to provide -- consume the one-shot.
    if (etype == 2) base = (uint8_t *)(uintptr_t)nonpie_place_at_link_address(basepage, span, &image_mapping);
    if (base != NULL) {
        g_force_base = 0; // one-shot, consumed by the link-address placement
    } else if (g_force_base) {
        // map this image at a FIXED VA (one-shot) so the translated arena -- block-map keys AND any
        // guest address baked into host code (pcrel_base literals, non-PIE ranges) -- is byte-identical
        // across runs, hence reusable from the persistent cache. On failure fall back to a kernel-chosen
        // base AND latch g_force_base_failed: this run's arena mixes bases, so the pcache must neither
        // serve it a fixed-base file (keys wouldn't match the live layout) nor persist it (a fixed-base
        // loader could later HIT a file whose block keys/baked guest addresses belong to a random base).
        void *want = (void *)(g_force_base + basepage);
        g_force_base = 0; // one-shot (consumed here for THIS load)
        hl_host_result fixed = effective_host_services()->memory->map_anonymous(
            effective_host_services()->context, (uint64_t)(uintptr_t)want, span,
            HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED, &image_mapping);
        if (fixed.status != HL_STATUS_OK) {
            g_force_base_failed = 1;
            image_mapping = elf_map_checked(NULL, span, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE,
                                            HL_HOST_MEMORY_PRIVATE, "image base");
        } else {
            pcache_note_fixed_img(image_mapping.address, span);
        }
        base = (uint8_t *)(uintptr_t)image_mapping.address;
    } else {
        /* Checkpointable launches must not let ASLR choose an image VA that a
         * fresh restore process may use for its own mappings.  Main image and
         * interpreter consume deterministic slots from the same high arena as
         * the stack, brk, and guest mmap allocations. */
        void *hint = (void *)(uintptr_t)hl_linux_snapshot_reserve(&g_ckpt_snapshot, span);
        image_mapping = elf_map_checked(hint, span, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_PRIVATE,
                                        "image base");
        base = (uint8_t *)(uintptr_t)image_mapping.address;
    }
    if (hl_exec_mapping_add((uint64_t)base, span, image_mapping.handle) != 0) {
        (void)effective_host_services()->memory->release(effective_host_services()->context, image_mapping.handle);
        fprintf(stderr, "hl-engine: loader mapping registry exhausted\n");
        exit(1);
    }
    hl_gmap_add((uint64_t)base, span);
    uint64_t bias = (uint64_t)base - basepage;
    // Armed on `bias != 0`, not on `etype == 2`: a link-address placement has one coordinate system, and
    // leaving lo/hi set with bias 0 keeps the fold's whole workaround family reachable for no reason.
    if (etype == 2 && bias != 0) {
        g_nonpie_lo = basepage;
        g_nonpie_hi = basepage + span;
        g_nonpie_bias = bias;
    }
    for (int i = 0; i < phnum; i++) {
        uint8_t *ph = f + phoff + (uint64_t)i * phentsize;
        if (rd32(ph) != 1) continue;
        uint64_t off = rd64(ph + 8), v = rd64(ph + 16), fsz = rd64(ph + 32);
        memcpy((void *)(v + bias), f + off, fsz);
    }
    // per-segment W^X from p_flags: .text R+X, .rodata R, .data R+W -- elf_protect.h, the contract.
    hl_elf_protect_segments(&image_mapping, f + phoff, phnum, phentsize, bias);
    // for a non-PIE ET_EXEC the engine maps the image HIGH (+bias) but keeps every GUEST-VISIBLE
    // address at its LOW link value (baked absolute pointers, un-biased `bl` return vaddrs, the dispatcher
    // re-biases only at execution). The auxv AT_ENTRY/AT_PHDR must therefore ALSO be LOW: glibc derives the
    // main map's l_addr from `AT_PHDR - PT_PHDR.p_vaddr`, so a HIGH AT_PHDR yields l_addr=bias and a HIGH
    // link_map [l_map_start,l_map_end). Then _dl_find_dso_for_object / dladdr / dlsym(RTLD_NEXT) compare a
    // LOW query (a baked &func or the un-biased return addr) against those HIGH ranges and MISS -> dladdr
    // fails, RTLD_NEXT returns NULL. clickhouse's sanitizer dl_iterate_phdr interceptor resolves the real
    // fn via dlsym(RTLD_NEXT,"dl_iterate_phdr"); the NULL makes it throw, and throwing captures a StackTrace
    // that unwinds through the same interceptor -> unbounded recursion -> guest stack overflow. Keeping the
    // auxv LOW makes glibc set l_addr=0 and the link_map ranges LOW, matching the guest's LOW addresses.
    // (ld.so's own reads of the phdrs at the LOW vaddr are served at +bias by the nonpie fold/fixup.) PIE is
    // unaffected: bias==0 there, so LOW==HIGH.
    int nonpie = (etype == 2);
    out->entry = nonpie ? e_entry : (e_entry + bias);
    out->base = (uint64_t)base;
    // phdrs live at file offset phoff in seg 0. Non-PIE: report the LOW link vaddr (base+phoff-bias) so the
    // main link_map is built LOW; PIE: the real HIGH address.
    out->phdr = nonpie ? ((uint64_t)base + phoff - bias) : ((uint64_t)base + phoff);
    out->phent = phentsize;
    out->phnum = phnum;
    // INTERIM: latch whether this is a Go image so signal delivery can auto-suppress Go's async-preempt
    // SIGURG for it (os/linux/signal.c, g_go_image). OR (never clear): load_elf runs for the main image THEN
    // the ld.so interpreter -- the interp is never Go, so '|=' keeps a main-image match from being clobbered
    // by the interp load. execve resets g_go_image to 0 before re-loading (proc.c) so a later non-Go image
    // starts clean. See the detailed rationale in signal.c.
    g_go_image |= elf_is_go_image(f, image.size);
    hl_linux_image_release(&image);
}

// Build the Linux process stack: [argc][argv..][NULL][envp..][NULL][auxv..][AT_NULL].
extern char **environ;
static char *g_guest_env[] = {
    // No TERM default: docker leaves TERM unset for a non-tty container and injects TERM=xterm (via the
    // daemon's HL_GUEST_ENV) for a `-t` one. A hardcoded TERM=dumb here shadowed both -> node/debconf/ncurses
    // degraded. Keep PATH/HOME/LANG as harmless fallbacks the image usually overrides.
    "PATH=/usr/bin:/bin", "HOME=/root", "LANG=C", "GLIBC_TUNABLES=glibc.cpu.aarch64_gcs=0", NULL,
};

static uint64_t build_stack(int argc, char **argv, struct loaded *lm, uint64_t at_base) {
    size_t SZ = 8u << 20;
    // stack-overflow safety: a PROT_NONE guard gap immediately BELOW the usable stack (Linux's
    // stack_guard_gap, 1MB). Without it the main stack sits adjacent-above the 64MB RX code cache, so a deep
    // recursion / huge frame runs off the stack bottom straight into the executable cache -> silent
    // corruption (the clickhouse crash) instead of a clean fault. A store past the bottom now hits PROT_NONE
    // -> a real host fault -> deliver_guest_fault -> SIGSEGV(si_addr=fault, SEGV_MAPERR), byte-exact with the
    // native oracle. gna_add registers it in the guest PROT_NONE registry so the fault guards treat the
    // overrun as a HARD fault (never growable memory). The 1MB guard is a bounded PROT_NONE reservation (no
    // committed pages) below g_stack_lo; not gmap-tracked so /proc/self/maps stays a clean [stack] + guard.
    size_t GUARD = 1u << 20;
    // checkpoint/restore: place the main stack in the deterministic high arena (0 hint => normal placement)
    hl_host_memory_mapping stack_mapping =
        elf_map_checked((void *)hl_linux_snapshot_reserve(&g_ckpt_snapshot, GUARD + SZ), GUARD + SZ,
                        HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_PRIVATE, "main stack");
    uint8_t *base = (uint8_t *)(uintptr_t)stack_mapping.address;
    elf_mprotect_besteffort(&stack_mapping, base, GUARD, 0, "stack guard");
    gna_add((uint64_t)base, (uint64_t)base + GUARD);
    uint8_t *stk = base + GUARD;
    if (hl_exec_mapping_add((uint64_t)base, GUARD + SZ, stack_mapping.handle) != 0) {
        (void)effective_host_services()->memory->release(effective_host_services()->context, stack_mapping.handle);
        fprintf(stderr, "hl-engine: loader mapping registry exhausted\n");
        exit(1);
    }
    hl_gmap_add((uint64_t)stk, SZ);
    // Publish the main-stack bounds so /proc/self/maps synthesizes a [stack] line (glibc's
    // pthread_getattr_np scans for it) and the maps/smaps builder can label the region.
    g_stack_lo = (uint64_t)stk;
    g_stack_hi = (uint64_t)(stk + SZ);
    uint8_t *top = stk + SZ;
    uint64_t argp[HL_MAXARGV], envp_[256]; // argv can be large post-exec (ARG_MAX); env stays small
    set_guest_cmdline(argc, argv);         // capture the full argv for /proc/self/cmdline (bare-mode fallback)
    int envc = 0;
    // Resolve the env string list WITHOUT placing it yet (the placement order below is what matters). The
    // container's env arrives as HL_GUEST_ENV="K=V\nK=V\n…" (set by the launcher) -- forward EXACTLY these to
    // the guest FIRST so they override the defaults, NOT the daemon/host environment. Then the built-in
    // defaults fill ONLY the keys the container didn't set.
    const char *estr[256];
    const char *ge = hl_option_get("HL_GUEST_ENV");
    char *gecopy = NULL;
    // execve() escape-encodes records (HL_GUEST_ENV_ESC=1) so a value's own newline isn't mistaken for a
    // record separator -- unescape "\\n"->'\n' and "\\\\"->'\\' after splitting. The daemon-launch path sets
    // HL_GUEST_ENV plain (no marker) and is left byte-for-byte unchanged.
    int env_escaped = (hl_option_get("HL_GUEST_ENV_ESC") != NULL);
    // A guest-initiated execve makes its envp AUTHORITATIVE (proc.c exec_forward_env sets this): forward
    // EXACTLY what the guest passed and inject NONE of the engine's fallback defaults below, so an empty
    // envp yields an empty environment and a curated envp is passed verbatim -- byte-exact with Linux. The
    // INITIAL container launch never sets this (the launcher's HL_GUEST_ENV path), so defaults still fill gaps.
    int env_exact = (hl_option_get("HL_GUEST_ENV_EXACT") != NULL);
    if (ge) {
        gecopy = strdup(ge);
        char *save = NULL;
        for (char *ln = strtok_r(gecopy, "\n", &save); ln && envc < 250; ln = strtok_r(NULL, "\n", &save)) {
            if (env_escaped) {
                char *r = ln, *w = ln; // unescape in place (only ever shrinks)
                while (*r) {
                    if (r[0] == '\\' && r[1] == 'n') {
                        *w++ = '\n';
                        r += 2;
                    } else if (r[0] == '\\' && r[1] == '\\') {
                        *w++ = '\\';
                        r += 2;
                    } else {
                        *w++ = *r++;
                    }
                }
                *w = 0;
            }
            estr[envc++] = ln;
        }
    }
    int guest_envc = envc; // [0..guest_envc) came from the container; the rest are defaults
    for (int i = 0; !env_exact && g_guest_env[i] && envc < 255; i++) {
        // Skip a default whose KEY the container already set: a duplicate "PATH=" would otherwise appear
        // in envp, and shells (bash) honor the LAST occurrence -> the default would shadow the image PATH
        // (this is what made `gosu` unresolvable in the postgres entrypoint). Match on the "KEY=" prefix.
        const char *eq = strchr(g_guest_env[i], '=');
        size_t klen = eq ? (size_t)(eq - g_guest_env[i]) + 1 : 0;
        int dup = 0;
        for (int j = 0; j < guest_envc && klen; j++)
            if (strncmp(estr[j], g_guest_env[i], klen) == 0) {
                dup = 1;
                break;
            }
        if (dup) continue;
        estr[envc++] = g_guest_env[i];
    }
    set_guest_environ(estr, envc); // capture the final env for /proc/self/environ (== getenv)
    // Place the arg/env strings top-down in the SAME memory order the Linux kernel uses, so that low->high
    // addresses hold argv[0], argv[1], …, argv[argc-1], env[0], …, env[envc-1] -- i.e. argv[0] sits at the
    // LOWEST address of the contiguous arg+env block, the last env string ends at the stack top. libuv
    // (node's process-title setup, used during mongosh/node bootstrap) RELIES on this: it treats argv[0] as
    // the block start and clears/overwrites FORWARD across the whole arg+env span. The naive top-down order
    // (argv[0] highest) put argv[0] at the stack-mapping top, so that forward fill ran off the end of the
    // mapping into unmapped memory -> SIGSEGV before any JS runs. Mirror the kernel: highest strings first.
    for (int i = envc - 1; i >= 0; i--) {
        size_t l = strlen(estr[i]) + 1;
        top -= l;
        memcpy(top, estr[i], l);
        envp_[i] = (uint64_t)top;
    }
    for (int i = argc - 1; i >= 0; i--) {
        size_t l = strlen(argv[i]) + 1;
        top -= l;
        memcpy(top, argv[i], l);
        argp[i] = (uint64_t)top;
    }
    free(gecopy); // the HL_GUEST_ENV tokens (estr[..]) were copied onto the stack above; safe to release now
    // AT_EXECFN string: Linux copies the execve PATHNAME (not argv[0]) near the stack top and points
    // AT_EXECFN at it. glibc dl-support, Rust std, and uutils' multicall dereference it; pointing it at
    // a relative argv[0] (e.g. a fork+exec'd `./x` or execve("/proc/self/exe")) diverged from the native
    // absolute path. g_exe_path holds the canonical guest exec path (set by the loader and by execve
    // before this call); fall back to argv[0] only if it is somehow unset.
    const char *execfn_str = (g_exe_path && g_exe_path[0]) ? g_exe_path : (argc ? argv[0] : "");
    size_t execfn_len = strlen(execfn_str) + 1;
    top -= execfn_len;
    memcpy(top, execfn_str, execfn_len);
    uint64_t execfn = (uint64_t)top;
    top -= 8;
    memcpy(top, "aarch64", 8);
    uint64_t plat = (uint64_t)top;
    top -= 16;
    arc4random_buf(top, 16);
    uint64_t rnd = (uint64_t)top;
    top = (uint8_t *)((uint64_t)top & ~15ull);
    // AT_PAGESZ describes the Linux GUEST ABI, never the host VM granularity.  The syscall mmap layer
    // separately reconciles 4 KiB guest MAP_FIXED requests with a 16 KiB Apple Silicon host, including
    // dynamic-linker segment placement and past-EOF tails.
    uint64_t aux[][2] = {
        {3, lm->phdr},
        {4, (uint64_t)lm->phent},
        {5, (uint64_t)lm->phnum},
        {6, HL_LINUX_GUEST_PAGE_SIZE},
        {7, at_base},
        {8, 0},
        {9, lm->entry},
        {11, (uint64_t)cuid()},
        {12, (uint64_t)cuid()},
        {13, (uint64_t)cgid()},
        {14, (uint64_t)cgid()},
        {16, g_aarch64_cpu_model.hwcap},
        // AT_HWCAP2 is where arm64 keeps BF16/I8MM/SVE2/MTE/BTI. arm64's ARCH_DLINFO emits it unconditionally,
        // so omitting it was not "advertise nothing" but a shape no kernel produces; the value stays 0 until
        // both backends implement one of those features outright (cpu.h).
        {26, g_aarch64_cpu_model.hwcap2},
        {17, 100},
        {15, plat},
        {25, rnd},
        {23, 0},
        {31, execfn},
        {0, 0},
    };
    int naux = (int)(sizeof aux / sizeof aux[0]);
    size_t nslots = 1 + (argc + 1) + (envc + 1) + (size_t)naux * 2;
    uint64_t *sp = (uint64_t *)top - nslots;
    sp = (uint64_t *)((uint64_t)sp & ~15ull);
    uint64_t *p = sp;
    *p++ = (uint64_t)argc;
    for (int i = 0; i < argc; i++)
        *p++ = argp[i];
    *p++ = 0;
    for (int i = 0; i < envc; i++)
        *p++ = envp_[i];
    *p++ = 0;
    for (int i = 0; i < naux; i++) {
        *p++ = aux[i][0];
        *p++ = aux[i][1];
    }
    // also serialize for /proc/self/auxv
    g_auxv_len = 0;
    for (int i = 0; i < naux && g_auxv_len + 16 <= (int)sizeof g_auxv_data; i++) {
        memcpy(g_auxv_data + g_auxv_len, &aux[i][0], 8);
        memcpy(g_auxv_data + g_auxv_len + 8, &aux[i][1], 8);
        g_auxv_len += 16;
    }
    return (uint64_t)sp;
}
