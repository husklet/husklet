/* x86 memory-DESTINATION variable shifts: SHL/SHR/SAR r/m, CL (opcodes D3 /4,/5,/7 at 32/64-bit).
   These are the forms the register allocator gets wrong most easily: the effective address is held in a
   reserved scratch across the whole lowering, and the by-CL path also needs the ORIGINAL operand saved
   for the exact x86 CF. If that save clobbers the EA register, the result is stored to a garbage address
   -> SIGSEGV (the jemalloc `bitmap_init` `shrq %cl,-0x8(%rax,%rdx,8)` crash that broke redis:7-alpine).
   Exercises base-only, base+disp, and full SIB (base+index*8+disp) addressing at both widths, plus the
   exact-CF boundary counts (1 / >1 / width-1). Run on LinuxX86_64, oracle-diffed vs qemu: a wrong
   value/flag OR a crash diverges and is caught. Only the WELL-DEFINED flags (CF ZF SF PF) are printed --
   x86 leaves AF undefined and OF undefined for a multi-bit shift, so those are masked out. */
#include <stdio.h>
#include <stdint.h>

/* CF(0) PF(2) ZF(6) SF(7) -- the flags shifts define for any nonzero count. */
#define FLMASK 0xC5UL

/* 64-bit memory-dest shift by CL. op is the mnemonic ("shrq"/"shlq"/"sarq"). */
#define MEMSH64(op, init, cnt) do {                                                       \
    uint64_t _m = (uint64_t)(init); unsigned long _f = 0;                                 \
    __asm__ volatile(                                                                     \
        "movq %[c], %%rcx\n\t"                                                            \
        op " %%cl, %[m]\n\t"                                                              \
        "pushfq\n\t pop %[f]\n\t"                                                          \
        : [m] "+m"(_m), [f] "=r"(_f) : [c] "r"((uint64_t)(cnt)) : "rcx", "cc");           \
    printf("%s.64 in=%016llx cnt=%2llu -> %016llx fl=%03lx\n", op,                        \
           (unsigned long long)(uint64_t)(init), (unsigned long long)(uint64_t)(cnt),     \
           (unsigned long long)_m, _f & FLMASK);                                          \
} while (0)

/* 32-bit memory-dest shift by CL. */
#define MEMSH32(op, init, cnt) do {                                                       \
    uint32_t _m = (uint32_t)(init); unsigned long _f = 0;                                 \
    __asm__ volatile(                                                                     \
        "movq %[c], %%rcx\n\t"                                                            \
        op " %%cl, %[m]\n\t"                                                              \
        "pushfq\n\t pop %[f]\n\t"                                                          \
        : [m] "+m"(_m), [f] "=r"(_f) : [c] "r"((uint64_t)(cnt)) : "rcx", "cc");           \
    printf("%s.32 in=%08x cnt=%2llu -> %08x fl=%03lx\n", op,                              \
           (unsigned)(uint32_t)(init), (unsigned long long)(uint64_t)(cnt),               \
           (unsigned)_m, _f & FLMASK);                                                    \
} while (0)

/* Full SIB form: shr/shl/sar [base + idx*8 - 8], cl -- the exact jemalloc bitmap_init encoding. */
#define SIBSH64(op, cnt) do {                                                             \
    uint64_t a[4] = {0xfedcba9876543210ULL, 0x0f1e2d3c4b5a6978ULL,                        \
                     0xffffffffffffffffULL, 0x8000000000000001ULL};                       \
    uint64_t idx = 3; /* -> a[idx-1] = a[2] */                                            \
    __asm__ volatile(                                                                     \
        "movq %[c], %%rcx\n\t"                                                            \
        "leaq %[base], %%rax\n\t"                                                         \
        "movq %[i], %%rdx\n\t"                                                            \
        op " %%cl, -0x8(%%rax,%%rdx,8)\n\t"                                               \
        : [base] "+m"(a) : [c] "r"((uint64_t)(cnt)), [i] "r"(idx) : "rax", "rcx", "rdx", "cc"); \
    printf("%s.sib cnt=%2llu -> a2=%016llx\n", op, (unsigned long long)(uint64_t)(cnt),    \
           (unsigned long long)a[2]);                                                      \
} while (0)

int main(void) {
    const uint64_t v64 = 0xF0F0F0F0F0F0F0F0ULL, s64 = 0x8000000000000000ULL;
    const uint32_t v32 = 0xF0F0F0F0u,           s32 = 0x80000000u;

    /* SHR: logical, brings a 0 into the top. count 1 / mid / width-1 / masked-to-0. */
    MEMSH64("shrq", v64, 1);  MEMSH64("shrq", v64, 5);  MEMSH64("shrq", v64, 62);
    MEMSH64("shrq", v64, 63); MEMSH64("shrq", v64, 64 /* &63 == 0: no change, no flags */);
    MEMSH32("shrl", v32, 1);  MEMSH32("shrl", v32, 5);  MEMSH32("shrl", v32, 31);
    MEMSH32("shrl", v32, 32 /* &31 == 0 */);

    /* SHL. */
    MEMSH64("shlq", v64, 1);  MEMSH64("shlq", v64, 4);  MEMSH64("shlq", v64, 63);
    MEMSH32("shll", v32, 1);  MEMSH32("shll", v32, 7);  MEMSH32("shll", v32, 31);

    /* SAR: arithmetic, replicates the sign bit. */
    MEMSH64("sarq", s64, 1);  MEMSH64("sarq", s64, 40); MEMSH64("sarq", v64, 8);
    MEMSH32("sarl", s32, 1);  MEMSH32("sarl", s32, 20); MEMSH32("sarl", v32, 8);

    /* Full SIB addressing (the jemalloc bitmap_init form). */
    SIBSH64("shrq", 62); SIBSH64("shlq", 5); SIBSH64("sarq", 3);
    return 0;
}
