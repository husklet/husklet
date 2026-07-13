// MOV moffs (opcodes A0-A3): accumulator <-> a direct 64-bit absolute address. The ONLY x86-64 form
// that carries a full address immediate (no ModRM). glibc/V8 (node/mongosh) emit it; the DBT frontend
// must decode its 8-byte moffs and preserve the exact width/zero-extend/preserve semantics of AL/AX/EAX/RAX.
// Diffed byte-for-byte against qemu-x86_64. x86_64-only (the whole family is x86-specific).
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

// A fixed absolute address ABOVE 4 GiB so the 64-bit moffs address is exercised in full (a truncated
// or sign-extended address would map the wrong page / fault). Little-endian bytes below encode it.
#define ADDR 0x140000000UL
// LE encoding of 0x0000000140000000: 00 00 00 40 01 00 00 00
#define MOFFS_BYTES 0x00, 0x00, 0x00, 0x40, 0x01, 0x00, 0x00, 0x00

int main(void) {
    void *p = mmap((void *)ADDR, 4096, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    if (p != (void *)ADDR) { printf("mmap fail %p\n", p); return 1; }
    volatile uint64_t *m = (uint64_t *)ADDR;
    const uint64_t SENT = 0xAAAAAAAAAAAAAAAAUL; // sentinel to prove untouched upper bytes are preserved

    // ---- loads: accumulator <- [moffs] ----
    *m = 0x1122334455667788UL;
    uint64_t rq, rd, rw, rb;
    // A1 REX.W: mov rax,[ADDR]  -> full 64-bit load
    __asm__ volatile(".byte 0x48,0xA1," "0x00,0x00,0x00,0x40,0x01,0x00,0x00,0x00" : "=a"(rq) : "a"(SENT));
    // A1: mov eax,[ADDR]        -> 32-bit load, zero-extends bits 63:32
    __asm__ volatile(".byte 0xA1," "0x00,0x00,0x00,0x40,0x01,0x00,0x00,0x00" : "=a"(rd) : "a"(SENT));
    // 66 A1: mov ax,[ADDR]      -> 16-bit load, preserves bits 63:16
    __asm__ volatile(".byte 0x66,0xA1," "0x00,0x00,0x00,0x40,0x01,0x00,0x00,0x00" : "=a"(rw) : "a"(SENT));
    // A0: mov al,[ADDR]         -> 8-bit load, preserves bits 63:8
    __asm__ volatile(".byte 0xA0," "0x00,0x00,0x00,0x40,0x01,0x00,0x00,0x00" : "=a"(rb) : "a"(SENT));
    printf("ld rax=%016lx eax=%016lx ax=%016lx al=%016lx\n", rq, rd, rw, rb);

    // ---- stores: [moffs] <- accumulator ----
    uint64_t sq, sd, sw, sb;
    *m = 0; // A3 REX.W: mov [ADDR],rax
    __asm__ volatile(".byte 0x48,0xA3," "0x00,0x00,0x00,0x40,0x01,0x00,0x00,0x00" : : "a"(0xCAFEF00DDEADBEEFUL) : "memory");
    sq = *m;
    *m = 0xFFFFFFFFFFFFFFFFUL; // A3: mov [ADDR],eax (writes low 32; upper 32 of MEM untouched)
    __asm__ volatile(".byte 0xA3," "0x00,0x00,0x00,0x40,0x01,0x00,0x00,0x00" : : "a"(0x12345678UL) : "memory");
    sd = *m;
    *m = 0xFFFFFFFFFFFFFFFFUL; // 66 A3: mov [ADDR],ax (writes low 16)
    __asm__ volatile(".byte 0x66,0xA3," "0x00,0x00,0x00,0x40,0x01,0x00,0x00,0x00" : : "a"(0xBEEFUL) : "memory");
    sw = *m;
    *m = 0xFFFFFFFFFFFFFFFFUL; // A2: mov [ADDR],al (writes low 8)
    __asm__ volatile(".byte 0xA2," "0x00,0x00,0x00,0x40,0x01,0x00,0x00,0x00" : : "a"(0x42UL) : "memory");
    sb = *m;
    printf("st rax=%016lx eax=%016lx ax=%016lx al=%016lx\n", sq, sd, sw, sb);
    return 0;
}
