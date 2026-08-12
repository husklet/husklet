#include "repstr.h"

#include "../cpu.h"
#include "../encoding.h"
#include "primitives.h"

#include <stdint.h>

// Codegen for the idiom: spill guest state, marshal args, blr the host helper, then fix up
// RDI/RSI (+= completed*w) and RCX (-= completed) in the membank snapshot, and reload. Guest GPRs live in
// host x0..x15 (caller-saved) so the spill/reload around the call is mandatory; x28 (cpu) is
// callee-saved and survives; the host SP is untouched (guest RSP is x4), so ABI alignment holds.
// df_static is the block-static direction shadow. The helper takes the runtime direction as its fifth
// argument; for a dynamic direction we load cpu->df and negate the pointer delta at runtime when backward.
static void emit_rep_string(int movs, int w, int shift, enum hl_x86_direction df_static, uint64_t rip) {
    // emit_spill (below) clears cpu->vdirty and republishes cpu->V, and emit_reload restores host
    // v0..v15 FROM cpu->V, so this leaves cpu->V current -> no vdirty mark needed (a later syscall may slim).
    hl_x86_emit_spill();                   // x0..x15 + xmm0..15 + flags -> cpu (membank)
    e_ldr(0, 28, R_OFF(RDI));              // x0 = dst (rdi)
    e_ldr(1, 28, R_OFF(movs ? RSI : RAX)); // x1 = src (rsi) / fill value (rax)
    e_ldr(2, 28, R_OFF(RCX));              // x2 = element count (rcx)
    e_movconst(3, (uint64_t)w);            // x3 = element width
    if (df_static == HL_X86_DIRECTION_DYNAMIC)
        e_ldr(4, 28, OFF_DF); // x4 = cpu->df (0 fwd / 1 bwd) -- runtime direction
    else
        e_movconst(4, (uint64_t)(df_static == HL_X86_DIRECTION_BACKWARD)); // x4 = statically-known direction
    e_mov_rr(5, 28, 1); // x5 = cpu, used to publish a precise soft-memory fault
    e_movconst(6, rip); // x6 = faulting guest instruction
    if (movs) {
        if (shift) e_lsl_i(2, 2, shift, 1); // x2 = nbytes = count << shift
        hl_x86_emit_host_pointer(16, (uint64_t)(uintptr_t)&hl_x86_rep_movs);
    } else {
        hl_x86_emit_host_pointer(16, (uint64_t)(uintptr_t)&hl_x86_rep_stos);
    }
    emit32(0xD63F0000u | (16 << 5)); // blr x16
    e_mov_rr(21, 0, 1);              // x21 = completed element count returned by helper
    // membank still holds the pre-call rcx/rdi/rsi (the helper takes its args by value):
    e_ldr(17, 28, R_OFF(RCX)); // x17 = original element count
    if (shift)
        e_lsl_i(16, 21, shift, 1); // x16 = completed bytes
    else
        e_mov_rr(16, 21, 1);
    // signed pointer delta: forward => +nbytes, backward => -nbytes.
    if (df_static == HL_X86_DIRECTION_BACKWARD) {
        e_rrr(A_SUB, 16, 31, 16, 1, 0); // x16 = -nbytes
    } else if (df_static == HL_X86_DIRECTION_DYNAMIC) {
        e_ldr(20, 28, OFF_DF);               // x20 = cpu->df
        emit32(0x34000000u | (2 << 5) | 20); // cbz x20, .+8  (df==0 -> keep +nbytes)
        e_rrr(A_SUB, 16, 31, 16, 1, 0);      // df==1 -> x16 = -nbytes
    }
    e_ldr(19, 28, R_OFF(RDI));
    e_rrr(A_ADD, 19, 19, 16, 1, 0); // rdi += delta
    e_str(19, 28, R_OFF(RDI));
    if (movs) {
        e_ldr(19, 28, R_OFF(RSI));
        e_rrr(A_ADD, 19, 19, 16, 1, 0); // rsi += delta
        e_str(19, 28, R_OFF(RSI));
    }
    e_rrr(A_SUB, 17, 17, 21, 1, 0);
    e_str(17, 28, R_OFF(RCX)); // rcx -= completed; EFLAGS unchanged by movs/stos
    e_ldr(16, 28, OFF_RSN);
    uint32_t *complete = hl_x86_emit_cursor();
    emit32(0); // cbz x16, complete
    hl_x86_emit_block_return();
    uint32_t *done = hl_x86_emit_cursor();
    *complete = 0xB4000000u | (((uint32_t)(done - complete) & 0x7ffffu) << 5) | 16u;
    /*
     * The helper may have queued one or more executable-alias writes.  RDI,
     * RSI and RCX now describe the exact completed architectural instruction,
     * so this is the first safe point to leave at `g_emit_next`; draining
     * inside the helper would either replay or skip non-idempotent elements.
     */
    emit_soft_store_drain();
    hl_x86_emit_reload();
    // the emit_spill above cleared cpu->vdirty at RUNTIME, so the once-per-trace mark latch must
    // reset -- a later xmm write in this same region has to re-mark or a following slim syscall exit would
    // skip the xmm save with host v0..v15 newer than cpu->V.
    hl_x86_emit_vector_reset();
}

// ---- string ops dispatch: stos/movs/lods (AA/AB/A4/A5/AC/AD), cmps/scas (A6/A7/AE/AF), cld/std (FC/FD).
// Lifted VERBATIM out of translate_block's one-byte switch (behavior-preserving move). DF assumed 0 (fwd)
// for stos/movs/lods unless std set state->direction. Returns TX_FALL if `op` is not a string op (caller falls through
// to the next handler), TX_NEXT (caller: `gpc = next; continue;`), or TX_BREAK (block ends; caller: break).
int hl_x86_lower_repstr(struct insn *I, uint64_t next, hl_x86_repstr_state *state) {
    uint8_t op = I->op;
    if (op == 0xAA || op == 0xAB || op == 0xA4 || op == 0xA5 || op == 0xAC || op == 0xAD) {
        int w = (op & 1) ? I->opsize : 1;
        int movs = (op == 0xA4 || op == 0xA5), lods = (op == 0xAC || op == 0xAD);
        // opt5: `rep movs`/`rep stos` -> one optimized host memcpy/memset call (bit-exact with the scalar
        // loop below). The host helper is now direction-aware (takes cpu->df / the static DF as its 5th arg),
        // so this fast path serves BOTH forward and backward. Fall back to the scalar loop only for NOREP=1,
        // `lods` (result is RAX, not a bulk move), or a segment override / 32-bit address size (the scalar
        // loop ignores both too).
        if (I->rep && !lods && !I->seg && !I->addr32 && (w == 1 || w == 2 || w == 4 || w == 8) &&
            (state->optimize || emit_soft_memory_active())) {
            int shift = w == 1 ? 0 : w == 2 ? 1 : w == 4 ? 2 : 3;
            emit_rep_string(movs, w, shift, state->direction, next - (uint64_t)I->len);
            return TX_NEXT;
        }
        // Scalar element loop. DF stride: forward +w, backward -w. When DF is statically known
        // (HL_X86_DIRECTION_FORWARD/HL_X86_DIRECTION_BACKWARD) use an immediate add/sub; when HL_X86_DIRECTION_DYNAMIC
        // (block entry / after popfq) compute a runtime stride reg (x17) from cpu->df so a cross-block `std`/popfq
        // direction is honored.
        int dyn = (state->direction == HL_X86_DIRECTION_DYNAMIC);
        if (dyn) {
            e_movconst(17, (uint64_t)w);         // x17 = +w
            e_ldr(16, 28, OFF_DF);               // x16 = cpu->df
            emit32(0xB4000000u | (2 << 5) | 16); // cbz x16, .+8   (df==0 -> keep +w)
            e_rrr(A_SUB, 17, 31, 17, 1, 0);      // df==1 -> x17 = -w
        }
#define REP_STEP(reg)                                                                                                  \
    do {                                                                                                               \
        if (dyn)                                                                                                       \
            e_rrr(A_ADD, (reg), (reg), 17, 1, 0);                                                                      \
        else if (state->direction == HL_X86_DIRECTION_BACKWARD)                                                        \
            e_subi((reg), (reg), (unsigned)w, 1);                                                                      \
        else                                                                                                           \
            e_addi((reg), (reg), (unsigned)w, 1);                                                                      \
    } while (0)
        uint32_t *cbz = NULL, *top = NULL;
        if (I->rep) {
            top = hl_x86_emit_cursor();
            cbz = hl_x86_emit_cursor();
            emit32(0);
        } // cbz RCX,done
        if (movs) {
            if (emit_soft_memory_active()) {
                e_mov_rr(17, RSI, 1);
                emit_memory_guard(17, (uint64_t)w, next - (uint64_t)I->len, X86_SOFT_READ);
                e_mov_rr(26, 17, 1);
                e_mov_rr(17, RDI, 1);
                emit_memory_guard(17, (uint64_t)w, next - (uint64_t)I->len, X86_SOFT_WRITE);
                e_load(w, 16, 26);
                e_store(w, 16, 17);
                /*
                 * REP is one architectural instruction.  Record each
                 * completed element, but do not leave for SMC service until
                 * RCX/RSI/RDI describe the completed instruction; resuming at
                 * `next` after the first element would silently skip the
                 * remaining non-idempotent stores.
                 */
                emit_soft_store_observe((uint64_t)w);
            } else {
                e_load(w, 16, RSI);
                e_store(w, 16, RDI);
            }
            REP_STEP(RSI);
            REP_STEP(RDI);
        } else if (lods) {
            if (emit_soft_memory_active()) {
                e_mov_rr(17, RSI, 1);
                emit_memory_guard(17, (uint64_t)w, next - (uint64_t)I->len, X86_SOFT_READ);
                e_load(w, RAX, 17);
            } else
                e_load(w, RAX, RSI);
            REP_STEP(RSI);
        } else {
            if (emit_soft_memory_active()) {
                e_mov_rr(17, RDI, 1);
                emit_memory_guard(17, (uint64_t)w, next - (uint64_t)I->len, X86_SOFT_WRITE);
                e_store(w, RAX, 17);
                emit_soft_store_observe((uint64_t)w);
            } else
                e_store(w, RAX, RDI);
            REP_STEP(RDI);
        } // stos
#undef REP_STEP
        if (I->rep) {
            e_subi(RCX, RCX, 1, 1);
            int64_t back = (int64_t)(top - hl_x86_emit_cursor());
            emit32(0x14000000u | ((uint32_t)back & 0x3FFFFFFu)); // b top
            int64_t d = (hl_x86_emit_cursor() - cbz);
            *cbz = 0xB4000000u | (((uint32_t)d & 0x7FFFF) << 5) | RCX; // cbz x_rcx,done
        }
        if (movs || op == 0xAA || op == 0xAB) emit_soft_store_drain();
        return TX_NEXT;
    }
    // cmps (A6/A7) / scas (AE/AF): the whole (possibly REP/REPE/REPNE) compare+scan is done in ONE C round-
    // trip (like cpuid/div): bit-exact RCX/RSI/RDI + ZF/SF/CF/OF end-state, fast host memcmp/memchr inside on
    // the forward path (gate NOREPCMP for the naive per-element oracle loop; DF=1 uses that loop with a
    // decrementing stride). Descriptor (width | isscas | isrepne | isrep | df) -> cpu->divop.
    if (op == 0xA6 || op == 0xA7 || op == 0xAE || op == 0xAF) {
        int w = (op & 1) ? I->opsize : 1;
        int isscas = (op == 0xAE || op == 0xAF);
        int isrep = (I->rep || I->repne);
        uint64_t desc = (uint64_t)w | ((uint64_t)isscas << 8) | ((uint64_t)(I->repne ? 1 : 0) << 9) |
                        ((uint64_t)isrep << 10) |
                        ((uint64_t)(state->direction == HL_X86_DIRECTION_BACKWARD ? 1 : 0) << 11);
        e_movconst(16, desc);
        if (state->direction == HL_X86_DIRECTION_DYNAMIC) { // DF unknown statically -> OR in the runtime direction bit
            e_ldr(18, 28, OFF_DF);                          // x18 = cpu->df (0/1)
            e_rrr(A_ORR, 16, 16, 18, 1, 11);                // desc |= df << 11
        }
        e_str(16, 28, OFF_DIVOP);
        emit_exit_const(next, R_REPSTR); // spills regs+flags; do_repstr() resumes at `next`
        return TX_BREAK;                 // block ends here (helper runs, dispatcher continues)
    }
    if (op == 0xFC || op == 0xFD) { // cld (FC) / std (FD): update BOTH the runtime bit and the static shadow
        e_movconst(16, (uint64_t)(op == 0xFD));
        e_str(16, 28, OFF_DF); // cpu->df = 0 (fwd) / 1 (bwd) -- persists across blocks
        state->direction = (op == 0xFD)
                               ? HL_X86_DIRECTION_BACKWARD
                               : HL_X86_DIRECTION_FORWARD; // statically known for the rest of THIS block (fast stride)
        return TX_NEXT;
    }
    return TX_FALL;
}
