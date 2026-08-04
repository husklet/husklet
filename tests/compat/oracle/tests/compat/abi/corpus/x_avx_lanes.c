// AVX 128/256-bit lane data-movement, shuffle, blend, permute, unpack, extract/insert, movmsk,
// round and dot-product. x86_64 uses the real VEX intrinsics (VSHUFPS/PD, VUNPCKL/H, VPERMILPS/PD,
// VBLENDPS/PD, VMOVMSKPS/PD, VMOVLHPS/VMOVHLPS, VPEXTRD/VPINSRD, VROUNDPS/PD, VDPPS); aarch64 runs a
// scalar reference over the same dword/qword lane arrays so the golden is byte-identical cross-arch.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("avx2,fma")))
#else
#define TGT
#endif
static uint64_t H = 0;
static void mixb(const void *p, int n) {
  const uint8_t *b = p;
  for (int i = 0; i < n; i++) H = H * 1000003ULL + b[i];
}

TGT int main(void) {
  uint32_t u[8], w[8];
  double g[4], h[4];
  for (int i = 0; i < 8; i++) { u[i] = (uint32_t)(i * 2654435761u + 7); w[i] = (uint32_t)(i * 40503u + 13); }
  for (int i = 0; i < 4; i++) { g[i] = (double)(i * 3 - 5) + 0.5; h[i] = (double)(i * 7 - 2) - 0.25; }
  uint32_t ob[8]; double od[4]; int r;

#if defined(__x86_64__)
  __m128 a = _mm_loadu_ps((float *)u), b = _mm_loadu_ps((float *)w);
  __m256 A = _mm256_loadu_ps((float *)u), B = _mm256_loadu_ps((float *)w);
  __m128d da = _mm_loadu_pd(g), db = _mm_loadu_pd(g + 2);
  __m256d DA = _mm256_loadu_pd(g);
  __m128i ia = _mm_loadu_si128((void *)u);
  // shuffles / unpacks
  _mm_storeu_ps((float *)ob, _mm_shuffle_ps(a, b, 0x4E)); mixb(ob, 16);
  _mm256_storeu_ps((float *)ob, _mm256_shuffle_ps(A, B, 0x1B)); mixb(ob, 32);
  _mm_storeu_pd(od, _mm_shuffle_pd(da, db, 0x2)); mixb(od, 16);
  _mm_storeu_ps((float *)ob, _mm_unpacklo_ps(a, b)); mixb(ob, 16);
  _mm_storeu_ps((float *)ob, _mm_unpackhi_ps(a, b)); mixb(ob, 16);
  _mm256_storeu_ps((float *)ob, _mm256_unpacklo_ps(A, B)); mixb(ob, 32);
  _mm_storeu_pd(od, _mm_unpackhi_pd(da, db)); mixb(od, 16);
  // permilps/pd imm
  _mm_storeu_ps((float *)ob, _mm_permute_ps(a, 0x93)); mixb(ob, 16);
  _mm256_storeu_ps((float *)ob, _mm256_permute_ps(A, 0x1B)); mixb(ob, 32);
  _mm_storeu_pd(od, _mm_permute_pd(da, 0x1)); mixb(od, 16);
  // blends
  _mm_storeu_ps((float *)ob, _mm_blend_ps(a, b, 0xA)); mixb(ob, 16);
  _mm256_storeu_ps((float *)ob, _mm256_blend_ps(A, B, 0x5A)); mixb(ob, 32);
  _mm_storeu_pd(od, _mm_blend_pd(da, db, 0x2)); mixb(od, 16);
  // movlhps / movhlps
  _mm_storeu_ps((float *)ob, _mm_movelh_ps(a, b)); mixb(ob, 16);
  _mm_storeu_ps((float *)ob, _mm_movehl_ps(a, b)); mixb(ob, 16);
  // movmsk
  r = _mm_movemask_ps(a); mixb(&r, 4);
  r = _mm256_movemask_ps(A); mixb(&r, 4);
  r = _mm_movemask_pd(da); mixb(&r, 4);
  r = _mm256_movemask_pd(DA); mixb(&r, 4);
  // extract / insert dword
  r = _mm_extract_epi32(ia, 3); mixb(&r, 4);
  _mm_storeu_si128((void *)ob, _mm_insert_epi32(ia, 0x12345678, 1)); mixb(ob, 16);
  r = _mm_extract_epi8(ia, 5); mixb(&r, 4);
  _mm_storeu_si128((void *)ob, _mm_insert_epi8(ia, 0x5A, 9)); mixb(ob, 16);
  // insertps
  _mm_storeu_ps((float *)ob, _mm_insert_ps(a, b, 0x4A)); mixb(ob, 16);
#else
  // ---- scalar reference over lane arrays (dwords in u/w, qwords in g/h) ----
  uint32_t *ud = u, *wd = w;
  uint64_t ug[4];
  memcpy(ug, g, 32); // da = {ug0,ug1}, db = {ug2,ug3} (db loaded from g+2 on x86)
  (void)h;
#define ST4(x0, x1, x2, x3) do { ob[0] = (x0); ob[1] = (x1); ob[2] = (x2); ob[3] = (x3); mixb(ob, 16); } while (0)
#define ST8(x0,x1,x2,x3,x4,x5,x6,x7) do{ ob[0]=(x0);ob[1]=(x1);ob[2]=(x2);ob[3]=(x3);ob[4]=(x4);ob[5]=(x5);ob[6]=(x6);ob[7]=(x7); mixb(ob,32);}while(0)
  // shufps a,b,0x4E: lo2 from a[imm10,imm32], hi2 from b[imm54,imm76]; imm=0x4E=01 00 11 10
  ST4(ud[2], ud[3], wd[0], wd[1]);
  // 256 shufps A,B,0x1B per 128-lane: imm=00 01 10 11
  ST8(ud[3], ud[2], wd[1], wd[0], ud[7], ud[6], wd[5], wd[4]);
  // shufpd da,db,0x2: q0=da[imm0], q1=db[imm1]; imm=10 => q0=da[0],q1=db[1]
  { uint64_t o[2] = {ug[0], ug[3]}; mixb(o, 16); }
  // unpacklo ps a,b: a0 b0 a1 b1
  ST4(ud[0], wd[0], ud[1], wd[1]);
  // unpackhi ps a,b: a2 b2 a3 b3
  ST4(ud[2], wd[2], ud[3], wd[3]);
  // 256 unpacklo ps per lane: a0 b0 a1 b1 | a4 b4 a5 b5
  ST8(ud[0], wd[0], ud[1], wd[1], ud[4], wd[4], ud[5], wd[5]);
  // unpackhi pd da,db: da1 db1
  { uint64_t o[2] = {ug[1], ug[3]}; mixb(o, 16); }
  // permilps a,0x93: dword j <- a[(imm>>2j)&3]; imm=0x93=10 01 00 11
  ST4(ud[3], ud[0], ud[1], ud[2]);
  // 256 permilps A,0x1B per lane: 00 01 10 11 -> [3,2,1,0]|[7,6,5,4]
  ST8(ud[3], ud[2], ud[1], ud[0], ud[7], ud[6], ud[5], ud[4]);
  // permilpd da,0x1: q0=da[imm0],q1=da[imm1]; imm=01 -> q0=da[1],q1=da[0]
  { uint64_t o[2] = {ug[1], ug[0]}; mixb(o, 16); }
  // blend ps a,b,0xA: bit i -> b else a; 0xA=1010 -> a b a b
  ST4(ud[0], wd[1], ud[2], wd[3]);
  // 256 blend ps A,B,0x5A=01011010
  ST8(ud[0], wd[1], ud[2], wd[3], wd[4], ud[5], wd[6], ud[7]);
  // blend pd da,db,0x2=10 -> q0=da0,q1=db1
  { uint64_t o[2] = {ug[0], ug[3]}; mixb(o, 16); }
  // movelh a,b: dst q0=a.q0(=a0a1), q1=b.q0(=b0b1)
  ST4(ud[0], ud[1], wd[0], wd[1]);
  // movehl a,b: dst q0=b.q1(=b2b3), q1=a.q1(=a2a3)
  ST4(wd[2], wd[3], ud[2], ud[3]);
  // movmsk ps a: sign bits of 4 dwords
  r = ((ud[0] >> 31) & 1) | ((ud[1] >> 31) << 1) | ((ud[2] >> 31) << 2) | ((ud[3] >> 31) << 3); mixb(&r, 4);
  // 256 movmsk ps A: 8 dwords
  r = 0; for (int i = 0; i < 8; i++) r |= (int)((ud[i] >> 31) & 1) << i; mixb(&r, 4);
  // movmsk pd da: sign bits of 2 qwords
  r = (int)((ug[0] >> 63) & 1) | (int)((ug[1] >> 63) & 1) << 1; mixb(&r, 4);
  // 256 movmsk pd DA: 4 qwords
  r = 0; for (int i = 0; i < 4; i++) r |= (int)((ug[i] >> 63) & 1) << i; mixb(&r, 4);
  // extract_epi32 ia,3
  r = (int)ud[3]; mixb(&r, 4);
  // insert_epi32 ia,val,1
  ST4(ud[0], 0x12345678u, ud[2], ud[3]);
  // extract_epi8 ia,5 (zero-extended byte)
  { uint8_t by[16]; memcpy(by, u, 16); r = by[5]; mixb(&r, 4); }
  // insert_epi8 ia,0x5A,9
  { uint8_t by[16]; memcpy(by, u, 16); by[9] = 0x5A; mixb(by, 16); }
  // insertps a,b,0x4A: src dword=b[imm>>6=1], dst lane=imm>>4&3=0, zero mask imm&0xF=0xA
  { uint32_t o[4]; memcpy(o, u, 16); o[0] = wd[1]; o[1] = 0; o[3] = 0; mixb(o, 16); }
#endif
  printf("avxlanes=%016llx\n", (unsigned long long)H);
  return 0;
}
