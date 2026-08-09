#ifndef HL_NATIVE_AARCH64_ASSEMBLER_H
#define HL_NATIVE_AARCH64_ASSEMBLER_H

#include <stddef.h>
#include <stdint.h>

typedef struct hl_a64_assembler {
    uint8_t *writable;
    uint8_t *executable;
    uint8_t *cursor;
    uint8_t *limit;
    uint32_t failed;
    uint32_t diagnostics;
    uint32_t write_reserve, write_commit;
    /* Emit the reservation behind a load of the store site's saturation byte
     * instead of deciding here. Emission stays a pure function of the pc, so a
     * discovery changes what the code reads and never what was translated. */
    uint32_t runtime_write_reserve;
    /* The dirty_overflow value hl_a64_guard_written reports for the store whose
     * reservation is currently being emitted. Published by hl_a64_guard_write_begin,
     * which always precedes it around the same store. */
    uint64_t site_report;
} hl_a64_assembler;

int hl_a64_assembler_begin(hl_a64_assembler *, void *, void *, size_t);
size_t hl_a64_assembler_size(const hl_a64_assembler *);
size_t hl_a64_assembler_remaining(const hl_a64_assembler *);
int hl_a64_assembler_ok(const hl_a64_assembler *);
int hl_a64_assembler_wrote(const hl_a64_assembler *, const void *);
void *hl_a64_assembler_rx(const hl_a64_assembler *, const void *);
void hl_a64_emit32(hl_a64_assembler *, uint32_t);
void hl_a64_str(hl_a64_assembler *, int, int, int);
void hl_a64_ldr(hl_a64_assembler *, int, int, int);
void hl_a64_mov_sp_from(hl_a64_assembler *, int);
void hl_a64_mov_from_sp(hl_a64_assembler *, int);
void hl_a64_movz(hl_a64_assembler *, int, uint32_t, int);
void hl_a64_movk(hl_a64_assembler *, int, uint32_t, int);
void hl_a64_br(hl_a64_assembler *, int);
void hl_a64_movconst(hl_a64_assembler *, int, uint64_t);
void hl_a64_stp(hl_a64_assembler *, int, int, int, int);
void hl_a64_ldp(hl_a64_assembler *, int, int, int, int);
void hl_a64_addi(hl_a64_assembler *, int, int, unsigned);
void hl_a64_addlsl4(hl_a64_assembler *, int, int, int);
void hl_a64_addlsl3(hl_a64_assembler *, int, int, int);
void hl_a64_movr(hl_a64_assembler *, int, int);
void hl_a64_subi(hl_a64_assembler *, int, int, unsigned);
void hl_a64_ret(hl_a64_assembler *);
void hl_a64_adrp_add(hl_a64_assembler *, int, uint64_t);
void hl_a64_stur(hl_a64_assembler *, int, int, int);
void hl_a64_ldur(hl_a64_assembler *, int, int, int);
void hl_a64_stp_q(hl_a64_assembler *, int, int, int, int);
void hl_a64_ldp_q(hl_a64_assembler *, int, int, int, int);

#endif
