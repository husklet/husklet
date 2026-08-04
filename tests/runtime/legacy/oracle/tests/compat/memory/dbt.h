// Shared helpers for DBT (dynamic-binary-translation) stress tests: self-modifying-code arenas and
// tiny per-arch code emitters. Arch-neutral guest source -- each guest builds only its own ISA's
// machine code under #ifdef. Deterministic by construction; tests print checksums only.
#ifndef HL_DBT_H
#define HL_DBT_H

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

// Allocate an RW arena and (optionally) flip to RX later via dbt_make_exec. RWX up front models the
// mmap-RWX JIT arena; callers that want the mprotect-toggle path allocate RW and toggle explicitly.
static inline unsigned char *dbt_alloc(size_t sz, int prot) {
    void *p = mmap(NULL, sz, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) {
        perror("mmap");
        exit(1);
    }
    return (unsigned char *)p;
}

static inline void dbt_flush(void *p, size_t sz) {
    __builtin___clear_cache((char *)p, (char *)p + sz);
}

static inline int dbt_make_exec(void *p, size_t sz) {
    return mprotect(p, sz, PROT_READ | PROT_EXEC);
}
static inline int dbt_make_write(void *p, size_t sz) {
    return mprotect(p, sz, PROT_READ | PROT_WRITE);
}

// Emit `uint64_t f(uint64_t x){ return x + imm; }` at buf; return byte length.
static inline int dbt_emit_add_imm(unsigned char *buf, uint32_t imm) {
#if defined(__aarch64__)
    uint32_t *w = (uint32_t *)buf;
    // x0 in, x0 out. add x0, x0, #imm  (imm must fit 12 bits for this encoding)
    w[0] = 0x91000000u | ((imm & 0xfffu) << 10); // add x0, x0, #imm12
    w[1] = 0xD65F03C0u;                           // ret
    return 8;
#elif defined(__x86_64__)
    // rdi in, rax out.  lea rax,[rdi+imm32]; ret
    buf[0] = 0x48;
    buf[1] = 0x8D;
    buf[2] = 0x87; // lea rax, [rdi + disp32]
    buf[3] = (unsigned char)(imm & 0xff);
    buf[4] = (unsigned char)((imm >> 8) & 0xff);
    buf[5] = (unsigned char)((imm >> 16) & 0xff);
    buf[6] = (unsigned char)((imm >> 24) & 0xff);
    buf[7] = 0xC3; // ret
    return 8;
#else
    (void)buf;
    (void)imm;
    return 0;
#endif
}

// Emit `int f(void){ return imm; }` (imm truncated to 16 bits on aarch64 movz).
static inline int dbt_emit_ret_imm(unsigned char *buf, int imm) {
#if defined(__aarch64__)
    uint32_t *w = (uint32_t *)buf;
    w[0] = 0x52800000u | ((uint32_t)(imm & 0xffff) << 5); // movz w0, #imm16
    w[1] = 0xD65F03C0u;                                   // ret
    return 8;
#elif defined(__x86_64__)
    buf[0] = 0xB8; // mov eax, imm32
    buf[1] = (unsigned char)(imm & 0xff);
    buf[2] = (unsigned char)((imm >> 8) & 0xff);
    buf[3] = (unsigned char)((imm >> 16) & 0xff);
    buf[4] = (unsigned char)((imm >> 24) & 0xff);
    buf[5] = 0xC3; // ret
    return 6;
#else
    (void)buf;
    (void)imm;
    return 0;
#endif
}

#endif // HL_DBT_H
