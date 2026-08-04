// Wide-character block ops: wmemset/wmemcpy/wmemmove/wcslen/wmemchr/wcscmp -> REP STOSD/SCASD/CMPSD
// (4-byte element string ops) plus overlap. Bit-exact across arches.
#include <stdio.h>
#include <wchar.h>
#include <stdint.h>
int main(void){
  static wchar_t a[4096], b[4096];
  uint64_t h=0;
  for(int i=0;i<4096;i++) a[i]=(wchar_t)(i*2654435761u ^ 0x5bd1e995u);
  wmemset(b, L'Z', 4096);
  wmemcpy(b+8, a, 3000);
  wmemmove(b+16, b, 2000);   // forward overlap
  wmemmove(b, b+16, 2000);   // backward overlap
  size_t n = wcsnlen(b, 4096);
  wchar_t *f = wmemchr(a, (wchar_t)(a[1234]), 4096);
  int c = wmemcmp(a, b, 1000);
  for(int i=0;i<4096;i++) h = h*1000003ULL + (uint32_t)b[i];
  h = h*1000003ULL + n;
  h = h*1000003ULL + (uint64_t)(f? (f-a):-1);
  h = h*1000003ULL + (uint64_t)((c>0)-(c<0));
  printf("wcs=%016llx n=%zu\n",(unsigned long long)h,n);
  return 0;
}
