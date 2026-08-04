#include <stdint.h>
#include <stdio.h>

/* AH/CH/DH/BH. Without a REX prefix, byte-register numbers 4..7 name the HIGH byte of the first four GPRs,
   not the low byte of rSP/rBP/rSI/rDI -- and a decoder that gets it wrong reads a completely unrelated
   register with no diagnostic. The engine's shared operand fetch handles it; every path that reaches past
   that helper for a raw GPR is a fresh chance to lose it, which is how CRC32 came to hash %spl instead of
   %ah (engine 968c7e88, hardware 86d2b9e7 -- the only wrong line in 14 kB of SSE4 output). So the CRC32
   byte form leads, and the ordinary movzx/movsx/ALU/setcc/shift/memory paths follow as the control that a
   fix cannot over-apply. "R" pins an operand to a legacy register: a REX-requiring one cannot be encoded
   alongside a high-byte operand at all. */
int main(void) {
    uint64_t seed = 0x1122334455667788ULL;

    { /* CRC32 r32, r/m8 with each of the four high-byte sources. */
        uint32_t out[4];
        __asm__ volatile("crc32b %%ah, %0" : "=R"(out[0]) : "0"(0xffffffffu), "a"(seed));
        __asm__ volatile("crc32b %%ch, %0" : "=R"(out[1]) : "0"(0xffffffffu), "c"(seed));
        __asm__ volatile("crc32b %%dh, %0" : "=R"(out[2]) : "0"(0xffffffffu), "d"(seed));
        __asm__ volatile("crc32b %%bh, %0" : "=R"(out[3]) : "0"(0xffffffffu), "b"(seed));
        printf("crc32-hi %08x %08x %08x %08x\n", out[0], out[1], out[2], out[3]);
        uint32_t lo, dil;
        __asm__ volatile("crc32b %%al, %0" : "=R"(lo) : "0"(0xffffffffu), "a"(seed));
        __asm__ volatile("crc32b %%dil, %0" : "=r"(dil) : "0"(0xffffffffu), "D"(seed));
        printf("crc32-lo %08x %08x\n", lo, dil);
        /* 16/32/64-bit CRC32 for the non-byte arm. */
        uint32_t c16, c32;
        uint64_t c64;
        __asm__ volatile("crc32w %%cx, %0" : "=r"(c16) : "0"(0xffffffffu), "c"(seed));
        __asm__ volatile("crc32l %%ecx, %0" : "=r"(c32) : "0"(0xffffffffu), "c"(seed));
        __asm__ volatile("crc32q %%rcx, %0" : "=r"(c64) : "0"(0xffffffffffffffffULL), "c"(seed));
        printf("crc32-w %08x %08x %016llx\n", c16, c32, (unsigned long long)c64);
    }

    { /* MOVZX / MOVSX from a high byte (32/16-bit destinations; a 64-bit one needs REX and cannot encode AH). */
        uint32_t z32, s32;
        uint16_t z16, s16;
        uint64_t a = 0x90abULL; /* ah = 0x90, negative as int8 */
        __asm__ volatile("movzbl %%ah, %0" : "=R"(z32) : "a"(a));
        __asm__ volatile("movsbl %%ah, %0" : "=R"(s32) : "a"(a));
        __asm__ volatile("movzbw %%ah, %0" : "=R"(z16) : "a"(a));
        __asm__ volatile("movsbw %%ah, %0" : "=R"(s16) : "a"(a));
        printf("movzx/sx %08x %08x %04x %04x\n", z32, s32, z16, s16);
    }

    { /* ALU with a high-byte source and a high-byte destination. */
        uint64_t r1 = 0x1234ULL;
        __asm__ volatile("addb %%ch, %%ah" : "+a"(r1) : "c"(0x5678ULL) : "cc");
        printf("add-hi %016llx\n", (unsigned long long)r1);
        uint64_t r3 = 0x1234ULL;
        unsigned char zf, cf;
        __asm__ volatile("cmpb %%ch, %%ah\n\tsetz %1\n\tsetc %2"
                         : "+a"(r3), "=R"(zf), "=R"(cf)
                         : "c"(0x5678ULL)
                         : "cc");
        printf("cmp-hi zf=%d cf=%d\n", zf, cf);
        uint64_t r4 = 0x00ff00ULL;
        __asm__ volatile("xchgb %%ah, %%al" : "+a"(r4));
        printf("xchg-hi %016llx\n", (unsigned long long)r4);
        uint64_t r5 = 0xffffffffffff0000ULL;
        __asm__ volatile("movb $0x5a, %%ah" : "+a"(r5));
        printf("mov-hi %016llx\n", (unsigned long long)r5);
    }

    { /* SETcc into a high byte, and shifts with a high-byte destination. */
        uint64_t r = 0xaaaaaaaaaaaaaaaaULL;
        __asm__ volatile("cmpb %%al, %%al\n\tsete %%ah" : "+a"(r)::"cc");
        printf("setcc-hi %016llx\n", (unsigned long long)r);
        uint64_t s = 0x0000ff00ULL;
        __asm__ volatile("shrb $3, %%ah" : "+a"(s)::"cc");
        printf("shr-hi %016llx\n", (unsigned long long)s);
        uint64_t t = 0x000081ffULL;
        __asm__ volatile("sarb $1, %%ah\n\trolb $3, %%ah" : "+a"(t)::"cc");
        printf("sar-hi %016llx\n", (unsigned long long)t);
    }

    { /* MOVBE has no byte form; the 16-bit form merges. */
        uint64_t r = 0xffffffffffffffffULL;
        uint16_t m = 0x1234;
        __asm__ volatile("movbew %1, %w0" : "+r"(r) : "m"(m));
        printf("movbe16 %016llx\n", (unsigned long long)r);
    }

    { /* A high byte through memory: TEST/OR against m8, and a high-byte STORE. */
        unsigned char buf[4] = {0xde, 0xad, 0xbe, 0xef};
        uint64_t a = 0x00007700ULL;
        unsigned char zf;
        __asm__ volatile("testb %%ah, %1\n\tsetz %0" : "=R"(zf) : "m"(buf[0]), "a"(a) : "cc");
        __asm__ volatile("movb %%ah, %0" : "=m"(buf[1]) : "a"(a));
        printf("mem-hi zf=%d buf=%02x %02x %02x %02x\n", zf, buf[0], buf[1], buf[2], buf[3]);
    }
    return 0;
}
