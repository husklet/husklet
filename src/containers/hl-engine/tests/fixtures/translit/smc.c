// translit/smc -- a JIT inside the guest.
//
// The transliterator caches guest BYTES, which the interpreter does not, so an overwrite of already
// translated code is a stale-copy hazard that did not exist before. Three shapes: an RWX arena rewritten
// in place, the mmap(RW)->write->mprotect(RX) re-toggle, and an overwrite with no intervening syscall at
// all. Any PROT_EXEC mmap or mprotect latches g_rwx_guest, which makes translit_image_ok() false for the
// rest of the process -- so what this fixture actually proves is that the REFUSAL holds, not that a stale
// copy is invalidated.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
// A JIT inside the guest: build code at runtime, run it, OVERWRITE it, run again.
// The transliterator caches guest BYTES, so a stale copy is a silent wrong answer.
typedef long (*fn)(long);

// mov %rdi,%rax ; add $K,%rax ; ret
static void emit(unsigned char *p, int k) {
    unsigned char body[] = {0x48, 0x89, 0xf8, 0x48, 0x05, 0, 0, 0, 0, 0xc3};
    body[5] = (unsigned char)(k & 0xff);
    body[6] = (unsigned char)((k >> 8) & 0xff);
    body[7] = (unsigned char)((k >> 16) & 0xff);
    body[8] = (unsigned char)((k >> 24) & 0xff);
    memcpy(p, body, sizeof body);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0); // unbuffered: the ordering of a forked child\'s output is part of the comparison
    size_t sz = 4096;
    unsigned char *rwx = mmap(NULL, sz, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (rwx == MAP_FAILED) {
        perror("mmap rwx");
        return 1;
    }
    long acc = 0;
    for (int round = 0; round < 64; round++) {
        emit(rwx, round * 3 + 1);
        fn f = (fn)rwx;
        for (int i = 0; i < 16; i++)
            acc += f(i);
    }
    printf("rwx acc=%ld\n", acc);

    // The mmap(RW) -> write -> mprotect(RX) shape, re-toggled.
    unsigned char *rw = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (rw == MAP_FAILED) {
        perror("mmap rw");
        return 1;
    }
    long acc2 = 0;
    for (int round = 0; round < 64; round++) {
        if (mprotect(rw, sz, PROT_READ | PROT_WRITE) != 0) {
            perror("mprotect rw");
            return 1;
        }
        emit(rw, round * 5 + 2);
        if (mprotect(rw, sz, PROT_READ | PROT_EXEC) != 0) {
            perror("mprotect rx");
            return 1;
        }
        fn f = (fn)rw;
        for (int i = 0; i < 16; i++)
            acc2 += f(i);
    }
    printf("rx  acc=%ld\n", acc2);

    // Overwrite executable bytes with NO mprotect at all: the page is already RWX,
    // so nothing in the syscall layer observes the change.
    long acc3 = 0;
    for (int round = 0; round < 64; round++) {
        emit(rwx, round * 7 + 3);
        fn f = (fn)rwx;
        for (int i = 0; i < 16; i++)
            acc3 += f(i);
    }
    printf("nop acc=%ld\n", acc3);
    return 0;
}
