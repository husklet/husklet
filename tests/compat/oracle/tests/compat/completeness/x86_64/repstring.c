// x86 rep movs/stos/cmps completeness + ERMS-funnel correctness (oracle: qemu-x86_64).
// hl lowers `rep movs`/`rep stos` to ONE host memcpy/memset and now advertises ERMS+FSRM
// (cpuid 7:0 EBX[9]/EDX[4]) so glibc funnels bulk memcpy/memmove/memset through rep movsb/stosb.
// This guest exercises EVERY case that routing now reaches, byte-exact vs qemu:
//   - rep movs/stos all widths (b/w/l/q), incl 0-length, unaligned, sub-16, 16, large
//   - forward-overlap smear (dst>src) at each width (hl replays element-granular)
//   - backward (DF=1, std) overlapping copy (hl's fast path is forward-only -> scalar fallback)
//   - XMM PRESERVATION across rep movs/stos: guest xmm live in host v0..v15 and a host memcpy/memset
//     clobbers caller-saved v0..v7 (+ upper v8..v15), so the emitter's xmm spill/reload around the
//     host call is load-bearing; if it were dropped this checksum would diverge from qemu
//   - full glibc memcpy/memmove(both directions)/memset round-trip with checksums (ERMS live path)
#include <stdio.h>
#include <stdint.h>
#include <string.h>

static uint64_t H = 1469598103934665603ULL; // FNV-1a-ish running checksum
static void mix(uint64_t v) { H = (H ^ v) * 1099511628211ULL; }
static void mixbuf(const void *p, size_t n) {
    const unsigned char *b = (const unsigned char *)p;
    for (size_t i = 0; i < n; i++) mix(b[i]);
}

// direct `rep movs` (forward, DF=0) at guest element width -- covers disjoint AND forward-overlap smear.
static void rmovs(void *dst, const void *src, size_t elems, int w) {
    void *dp = dst; const void *sp = src; size_t c = elems;
    switch (w) {
    case 1: __asm__ volatile("cld; rep movsb" : "+D"(dp), "+S"(sp), "+c"(c)::"memory"); break;
    case 2: __asm__ volatile("cld; rep movsw" : "+D"(dp), "+S"(sp), "+c"(c)::"memory"); break;
    case 4: __asm__ volatile("cld; rep movsl" : "+D"(dp), "+S"(sp), "+c"(c)::"memory"); break;
    case 8: __asm__ volatile("cld; rep movsq" : "+D"(dp), "+S"(sp), "+c"(c)::"memory"); break;
    }
}
// direct `rep stos` (forward, DF=0).
static void rstos(void *dst, uint64_t val, size_t elems, int w) {
    void *dp = dst; size_t c = elems;
    switch (w) {
    case 1: __asm__ volatile("cld; rep stosb" : "+D"(dp), "+c"(c) : "a"(val) : "memory"); break;
    case 2: __asm__ volatile("cld; rep stosw" : "+D"(dp), "+c"(c) : "a"(val) : "memory"); break;
    case 4: __asm__ volatile("cld; rep stosl" : "+D"(dp), "+c"(c) : "a"(val) : "memory"); break;
    case 8: __asm__ volatile("cld; rep stosq" : "+D"(dp), "+c"(c) : "a"(val) : "memory"); break;
    }
}

static const int WID[4] = {1, 2, 4, 8};

int main(void) {
    static unsigned char src[4096], dst[4096];
    for (int i = 0; i < 4096; i++) src[i] = (unsigned char)(i * 31 + 7);
    const size_t bytes[] = {0, 1, 3, 15, 16, 17, 64, 255, 1024, 4096};

    // 1) rep movs, all widths, disjoint, sizes {0,1,3,15,16,17,64,255,1024,4096}, aligned + unaligned dst.
    for (int wi = 0; wi < 4; wi++) {
        int w = WID[wi];
        for (unsigned bi = 0; bi < sizeof(bytes) / sizeof(bytes[0]); bi++) {
            size_t nb = bytes[bi] & ~(size_t)(w - 1); // whole elements
            for (int off = 0; off <= 3; off++) {
                if (nb + off > 4096) continue;
                memset(dst, 0xAA, sizeof(dst));
                rmovs(dst + off, src, nb / w, w);
                mixbuf(dst + off, nb);
                mix((uint64_t)(nb + off + w));
            }
        }
    }

    // 2) rep stos, all widths, sizes incl 0, several fill values.
    for (int wi = 0; wi < 4; wi++) {
        int w = WID[wi];
        uint64_t vals[] = {0x00, 0x41, 0x5aa5u, 0xdeadbeefu, 0x0123456789abcdefULL};
        for (unsigned bi = 0; bi < sizeof(bytes) / sizeof(bytes[0]); bi++) {
            size_t nb = bytes[bi] & ~(size_t)(w - 1);
            for (unsigned vi = 0; vi < sizeof(vals) / sizeof(vals[0]); vi++) {
                memset(dst, 0x33, sizeof(dst));
                rstos(dst, vals[vi], nb / w, w);
                mixbuf(dst, nb);
                mix(nb ^ vals[vi]);
            }
        }
    }

    // 3) forward-overlap SMEAR (dst = src + shift elems, within [src, src+n)) at each width.
    for (int wi = 0; wi < 4; wi++) {
        int w = WID[wi];
        for (int shift = 1; shift <= 3; shift++) {
            unsigned char buf[512];
            for (int i = 0; i < 512; i++) buf[i] = (unsigned char)(i + 1);
            rmovs(buf + (size_t)shift * w, buf, 60, w);
            mixbuf(buf, 512);
            mix((uint64_t)(w * 1000 + shift));
        }
    }

    // 4) backward (DF=1, std) OVERLAPPING copy (dst>src, high->low). hl's fast path is forward-only,
    //    so this drives the byte-exact scalar fallback with a -w stride.
    for (int wi = 0; wi < 4; wi++) {
        int w = WID[wi];
        unsigned char buf[512];
        for (int i = 0; i < 512; i++) buf[i] = (unsigned char)(i * 7 + 3);
        size_t elems = 50;
        size_t doff = (size_t)2 * w; // dst overlaps src forward -> backward order is the safe one
        void *dp = buf + doff + (elems - 1) * w; // last dst element
        const void *sp = buf + (elems - 1) * w;  // last src element
        size_t c = elems;
        switch (w) {
        case 1: __asm__ volatile("std; rep movsb; cld" : "+D"(dp), "+S"(sp), "+c"(c)::"memory"); break;
        case 2: __asm__ volatile("std; rep movsw; cld" : "+D"(dp), "+S"(sp), "+c"(c)::"memory"); break;
        case 4: __asm__ volatile("std; rep movsl; cld" : "+D"(dp), "+S"(sp), "+c"(c)::"memory"); break;
        case 8: __asm__ volatile("std; rep movsq; cld" : "+D"(dp), "+S"(sp), "+c"(c)::"memory"); break;
        }
        mixbuf(buf, 512);
        mix((uint64_t)(w * 77 + 5));
    }

    // 5) XMM PRESERVATION across rep movs AND rep stos (guards the host-call SIMD clobber).
    {
        static uint64_t xin[32], xout[32];
        for (int i = 0; i < 32; i++) xin[i] = 0x1111111100000000ULL * (uint64_t)(i + 1) + (uint64_t)(i * 7 + 1);
        static unsigned char s2[64], d2[64];
        for (int i = 0; i < 64; i++) s2[i] = (unsigned char)(i * 13 + 2);
#define XLOAD                                                                                              \
    "movdqu 0x00(%[xi]),%%xmm0\n movdqu 0x10(%[xi]),%%xmm1\n movdqu 0x20(%[xi]),%%xmm2\n"                  \
    "movdqu 0x30(%[xi]),%%xmm3\n movdqu 0x40(%[xi]),%%xmm4\n movdqu 0x50(%[xi]),%%xmm5\n"                  \
    "movdqu 0x60(%[xi]),%%xmm6\n movdqu 0x70(%[xi]),%%xmm7\n movdqu 0x80(%[xi]),%%xmm8\n"                  \
    "movdqu 0x90(%[xi]),%%xmm9\n movdqu 0xa0(%[xi]),%%xmm10\n movdqu 0xb0(%[xi]),%%xmm11\n"                \
    "movdqu 0xc0(%[xi]),%%xmm12\n movdqu 0xd0(%[xi]),%%xmm13\n movdqu 0xe0(%[xi]),%%xmm14\n"               \
    "movdqu 0xf0(%[xi]),%%xmm15\n"
#define XSTORE                                                                                             \
    "movdqu %%xmm0,0x00(%[xo])\n movdqu %%xmm1,0x10(%[xo])\n movdqu %%xmm2,0x20(%[xo])\n"                  \
    "movdqu %%xmm3,0x30(%[xo])\n movdqu %%xmm4,0x40(%[xo])\n movdqu %%xmm5,0x50(%[xo])\n"                  \
    "movdqu %%xmm6,0x60(%[xo])\n movdqu %%xmm7,0x70(%[xo])\n movdqu %%xmm8,0x80(%[xo])\n"                  \
    "movdqu %%xmm9,0x90(%[xo])\n movdqu %%xmm10,0xa0(%[xo])\n movdqu %%xmm11,0xb0(%[xo])\n"                \
    "movdqu %%xmm12,0xc0(%[xo])\n movdqu %%xmm13,0xd0(%[xo])\n movdqu %%xmm14,0xe0(%[xo])\n"               \
    "movdqu %%xmm15,0xf0(%[xo])\n"
#define XCLOB                                                                                              \
    "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7", "xmm8", "xmm9", "xmm10", "xmm11",      \
        "xmm12", "xmm13", "xmm14", "xmm15", "memory"
        { // rep movsb bracketed by xmm load/store
            void *dp = d2; const void *sp = s2; size_t c = 64;
            __asm__ volatile(XLOAD "cld; rep movsb\n" XSTORE
                             : "+D"(dp), "+S"(sp), "+c"(c)
                             : [xi] "r"(xin), [xo] "r"(xout)
                             : XCLOB);
        }
        mixbuf(xout, sizeof(xout));
        mixbuf(d2, sizeof(d2));
        for (int i = 0; i < 32; i++) xout[i] = 0;
        { // rep stosb bracketed by xmm load/store
            void *dp = d2; size_t c = 64;
            __asm__ volatile(XLOAD "cld; rep stosb\n" XSTORE
                             : "+D"(dp), "+c"(c)
                             : "a"(0x77), [xi] "r"(xin), [xo] "r"(xout)
                             : XCLOB);
        }
        mixbuf(xout, sizeof(xout));
        mixbuf(d2, sizeof(d2));
    }

    // 6) glibc-level round-trip: with ERMS/FSRM live, memcpy/memmove/memset route through rep movsb/stosb.
    {
        static unsigned char big[65536], cpy[65536];
        for (int i = 0; i < 65536; i++) big[i] = (unsigned char)(i * 5 + 1);
        const size_t szs[] = {1, 7, 15, 16, 31, 32, 63, 64, 127, 200, 511, 2048, 4096, 8192, 65536};
        for (unsigned si = 0; si < sizeof(szs) / sizeof(szs[0]); si++) {
            size_t n = szs[si];
            memset(cpy, 0, n);
            memcpy(cpy, big, n);
            mixbuf(cpy, n);
            memcpy(cpy, big, n);
            if (n > 8) { memmove(cpy + 4, cpy, n - 4); mixbuf(cpy, n); } // forward-overlap
            memcpy(cpy, big, n);
            if (n > 8) { memmove(cpy, cpy + 4, n - 4); mixbuf(cpy, n); } // backward-overlap
            memset(cpy, (int)(n & 0xff), n);
            mixbuf(cpy, n);
            mix(n);
        }
    }

    // legacy cmps (kept from the original test)
    {
        char a[16] = "abcdefghij", b[16];
        memset(b, 0, 16);
        memcpy(b, a, 11);
        char *s1 = a, *s2 = b; size_t cn = 11; unsigned char eq;
        __asm__ volatile("cld; repe cmpsb; sete %0" : "=r"(eq), "+S"(s1), "+D"(s2), "+c"(cn)::"cc", "memory");
        mix(eq);
    }

    printf("repstring H=%016llx\n", (unsigned long long)H);
    return 0;
}
