#ifndef HL_TRANSLATOR_GUEST_X86_64_X87STATE_H
#define HL_TRANSLATOR_GUEST_X86_64_X87STATE_H

#include "../../../host/host_cpu.h" // HL_HOST_CPU_*: which backend maintains the tag bits below
#include <stdint.h>

struct cpu;

// ---- x87 tag state.
//
// Hardware tags each PHYSICAL register valid(00)/zero(01)/special(10)/empty(11). The first three are
// functions of the stored value, so they are DERIVED (hl_x87_tag below) and only EMPTINESS is stored --
// and emptiness cannot be derived: FFREE punches an arbitrary hole and `fst %st(3)` fills an arbitrary
// slot, both measured on hardware. One bit per slot, carried in the UNUSED HIGH BITS of cpu->fptop
// (bits 2:0 are TOP), so sizeof(struct cpu) -- which IS the checkpoint format -- does not move.
//
// HL_X87_ARMED gates the whole model. A zero-initialised cpu reads as DISARMED = "no tag information",
// which is exactly the pre-existing "every slot occupied" behaviour; both backends arm it at the first x87
// instruction, where the stack is architecturally empty -- interp.c in interp_x87_arm(), the AArch64 JIT in
// lower/x87.c fp_tags(), which is why the tag bits survive hl_x86_x87_materialize()'s store of TOP.
#define HL_X87_EMPTY_SHIFT 8
#define HL_X87_EMPTY_ALL (UINT64_C(0xff) << HL_X87_EMPTY_SHIFT)
#define HL_X87_ARMED (UINT64_C(1) << 16)
#define HL_X87_STATE_BITS (HL_X87_EMPTY_ALL | HL_X87_ARMED)

#define hl_x87_tags_modelled() 1

// `slot` is PHYSICAL (an index into cpu->st[]), like hardware's tag word and FXSAVE tag byte.
static inline int hl_x87_phys_empty(uint64_t fptop, int slot) {
    if (!hl_x87_tags_modelled() || !(fptop & HL_X87_ARMED)) return 0;
    return (int)((fptop >> (HL_X87_EMPTY_SHIFT + (slot & 7))) & 1u);
}

static inline void hl_x87_phys_mark(uint64_t *fptop, int slot, int empty) {
    uint64_t bit = UINT64_C(1) << (HL_X87_EMPTY_SHIFT + (slot & 7));
    *fptop = empty ? (*fptop | bit) : (*fptop & ~bit);
}

// NOT !hl_x87_phys_empty(). Disarmed means "no tag information", so it must answer NEITHER empty nor live:
// a push overflow test written as !empty would fire on every push into a disarmed cpu, which is the
// pre-tag-word behaviour inverted rather than preserved.
static inline int hl_x87_phys_live(uint64_t fptop, int slot) {
    if (!hl_x87_tags_modelled() || !(fptop & HL_X87_ARMED)) return 0;
    return !hl_x87_phys_empty(fptop, slot);
}

// FXSAVE's abridged tag byte: bit i = physical register i is NOT empty. 0xff when unmodelled.
static inline uint8_t hl_x87_abridged_tag(uint64_t fptop) {
    if (!hl_x87_tags_modelled() || !(fptop & HL_X87_ARMED)) return 0xff;
    return (uint8_t)~(uint8_t)((fptop >> HL_X87_EMPTY_SHIFT) & 0xffu);
}

// The 2-bit tag hardware reports for a non-empty slot holding `value`. A double subnormal widens to a
// NORMAL 80-bit value, so it tags valid (measured: 5e-324 pushed and FNSAVEd tags 00, not 10).
static inline unsigned hl_x87_tag(uint64_t fptop, int slot, double value) {
    uint64_t bits;
    if (hl_x87_phys_empty(fptop, slot)) return 3u;
    __builtin_memcpy(&bits, &value, sizeof bits);
    if (((bits >> 52) & 0x7ff) == 0x7ff) return 2u; // NaN / infinity
    if (!(bits & ~(UINT64_C(1) << 63))) return 1u;  // +-0
    return 0u;
}

// The 16-bit tag word FNSTENV/FNSAVE write, and FLDENV/FRSTOR read back.
static inline uint16_t hl_x87_tag_word(uint64_t fptop, const double *st) {
    uint16_t word = 0;
    for (int slot = 0; slot < 8; ++slot)
        word |= (uint16_t)(hl_x87_tag(fptop, slot, st[slot]) << (2 * slot));
    return word;
}

// The QNaN "integer/real indefinite" every masked #IS delivers. m80 form: ffffc000000000000000.
#define HL_X87_INDEFINITE_BITS UINT64_C(0xfff8000000000000)

static inline double hl_x87_indefinite(void) {
    double value;
    uint64_t bits = HL_X87_INDEFINITE_BITS;
    __builtin_memcpy(&value, &bits, sizeof value);
    return value;
}

// FLDCW/FLDENV/FRSTOR/FXRSTOR do not store FCW verbatim: bit 6 reads back as 1, bit 7 and bits 15:13 as 0
// (measured: fldcw ffff -> fnstcw 1f7f, fldcw 0000 -> 0040).
#define HL_X87_FCW(v) ((uint64_t)(((v) & 0x1f3fu) | 0x40u))

// The host's sticky FP exception flags, in x87 FSW bit order (IE0 DE1 ZE2 OE3 UE4 PE5) on either host.
// Snapshot/restore them around a computation used only to DERIVE something: FPREM is exact by definition,
// so the host division its quotient bits need must not leave a #P behind.
unsigned hl_x87_exceptions_get(void);
void hl_x87_exceptions_set(unsigned flags);
void hl_x87_exceptions_raise(unsigned flags);

double hl_x86_ext80_load(const uint8_t image[10]);
void hl_x86_ext80_store(double value, uint8_t image[10]);
void hl_x86_x87_load_ext80(struct cpu *cpu);
void hl_x86_x87_store_ext80_pop(struct cpu *cpu);
void hl_x86_fxsave(struct cpu *cpu);
void hl_x86_fxrstor(struct cpu *cpu);
// FSW as FNSTSW/FNSTENV/FXSAVE report it: cpu->fpsw's codes + SF, TOP, and the host's live exception flags.
uint16_t hl_x86_x87_status_word(const struct cpu *cpu);
// FNSTENV/FLDENV/FNSAVE/FRSTOR, selected by cpu->divop (X87ENV_*) at the host EA in cpu->x87_ea.
void hl_x86_x87_environment(struct cpu *cpu);

#endif
