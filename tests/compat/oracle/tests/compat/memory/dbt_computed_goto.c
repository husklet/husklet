// Threaded-code (labels-as-values / computed goto) bytecode interpreter -- the exact dispatch shape
// used by CPython/PyPy/Ruby interpreter loops. Each opcode ends with `goto *dispatch[*pc++]`, an
// indirect branch whose target is the next opcode: a torture test for the engine's indirect-branch
// target cache and block chaining. A deterministic program is run to a fixed step budget; a wrong
// dispatch corrupts the accumulator. Prints a checksum only.
#include <stdint.h>
#include <stdio.h>

enum { OP_ADD, OP_XOR, OP_MUL, OP_ROT, OP_LOAD, OP_JMPBACK, OP_HALT, NOPS };

int main(void) {
    // Deterministic pseudo-random program of 64 opcodes ending in a backward jump loop, then HALT.
    unsigned char prog[80];
    uint32_t s = 0x1234567u;
    for (int i = 0; i < 62; i++) {
        s = s * 1103515245u + 12345u;
        prog[i] = (unsigned char)((s >> 16) % OP_JMPBACK); // ops 0..4 (no jmp/halt inside body)
    }
    prog[62] = OP_JMPBACK;
    prog[63] = OP_HALT;

    static void *dispatch[NOPS] = {&&do_add, &&do_xor,     &&do_mul, &&do_rot,
                                   &&do_load, &&do_jmpback, &&do_halt};
    uint64_t acc = 0x9e3779b9ULL, aux = 0xdeadbeefULL;
    uint64_t steps = 0, budget = 20000000ULL;
    const unsigned char *pc = prog;

#define NEXT()                                                                 \
    do {                                                                       \
        if (steps++ >= budget) goto done;                                      \
        goto *dispatch[*pc++];                                                 \
    } while (0)

    NEXT();
do_add:
    acc += aux + 1;
    NEXT();
do_xor:
    acc ^= (aux << 7) | (aux >> 57);
    NEXT();
do_mul:
    acc = acc * 0x100000001b3ULL + 0xff;
    NEXT();
do_rot:
    acc = (acc << 13) | (acc >> 51);
    aux = acc ^ steps;
    NEXT();
do_load:
    aux = acc ^ 0x5555555555555555ULL;
    NEXT();
do_jmpback:
    pc = prog; // loop the program body
    NEXT();
do_halt:
    goto done;
done:
    printf("computed-goto acc=%llu steps=%llu\n", (unsigned long long)acc, (unsigned long long)steps);
    return 0;
}
