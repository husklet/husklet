/* Parity-consumer block-boundary probe (oracle: qemu-x86_64).
   jp/jnp and cmovp/cmovnp compute the real PF lane into a scratch ARM condition at translate time;
   this guest checks that doing so never corrupts the OTHER flags (CF/ZF/SF/OF) as observed by the
   SUCCESSOR blocks: a jp-terminated block followed by jb/jz/pushfq readers, both jp arms, and a
   cmovp immediately before a block-ending jump. Every result is deterministic and byte-diffed. */
#include <stdio.h>
#include <stdint.h>

static const uint64_t V[] = {0, 1, 2, 3, 0x7f, 0x80, 0xfe, 0xff, 0x8000000000000000ull, 0xffffffffffffffffull};
#define NV (sizeof V / sizeof V[0])

int main(void) {
    unsigned long acc = 0;
    for (unsigned i = 0; i < NV; i++)
        for (unsigned j = 0; j < NV; j++) {
            uint64_t a = V[i], b = V[j];
            unsigned long r1 = 0, r2 = 0, f = 0, r3 = 0;
            __asm__ volatile( /* cmp ; jp ; successor reads CF via jb */
                "cmpq %[b], %[a]\n\t"
                "jp 1f\n\t"
                "jb 2f\n\t movl $10, %k[r]\n\t jmp 4f\n\t"
                "2: movl $11, %k[r]\n\t jmp 4f\n\t"
                "1: jb 3f\n\t movl $12, %k[r]\n\t jmp 4f\n\t"
                "3: movl $13, %k[r]\n\t"
                "4:\n\t"
                : [r] "=r"(r1) : [a] "r"(a), [b] "r"(b) : "cc");
            __asm__ volatile( /* add ; jnp ; successor reads ZF/SF via jz/js and pushfq */
                "movq %[a], %%rax\n\t addq %[b], %%rax\n\t"
                "jnp 1f\n\t"
                "jz 2f\n\t movl $20, %k[r]\n\t jmp 4f\n\t"
                "2: movl $21, %k[r]\n\t jmp 4f\n\t"
                "1: js 3f\n\t movl $22, %k[r]\n\t jmp 4f\n\t"
                "3: movl $23, %k[r]\n\t"
                "4: pushfq\n\t pop %[f]\n\t"
                : [r] "=r"(r2), [f] "=r"(f) : [a] "r"(a), [b] "r"(b) : "rax", "cc");
            __asm__ volatile( /* cmovp right before a block end; successor reads CF/ZF */
                "movq %[a], %%rdx\n\t cmpq %[b], %[a]\n\t"
                "cmovpq %[b], %%rdx\n\t"
                "jmp 1f\n\t"
                "1: jbe 2f\n\t addq $100, %%rdx\n\t jmp 3f\n\t"
                "2: addq $200, %%rdx\n\t"
                "3: movq %%rdx, %[r]\n\t"
                : [r] "=r"(r3) : [a] "r"(a), [b] "r"(b) : "rdx", "cc");
            acc = acc * 1099511628211UL ^ r1 ^ (r2 << 4) ^ ((f & 0x8D5UL) << 8) ^ r3;
            if ((i + j) % 5 == 0)
                printf("P a=%016lx b=%016lx r1=%lu r2=%lu f=%03lx r3=%016lx\n", (unsigned long)a,
                       (unsigned long)b, r1, r2, f & 0x8D5UL, (unsigned long)r3);
        }
    printf("parity-edge acc=%016lx\n", acc);
    return 0;
}
