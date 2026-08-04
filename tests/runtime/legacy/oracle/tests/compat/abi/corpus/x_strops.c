// String block ops (x86 REP MOVS/STOS/SCAS/CMPS + direction flag) via libc + overlapping moves.
#include <stdio.h>
#include <string.h>
#include <stdint.h>
int main(void){
  static unsigned char buf[4096]; uint64_t h=0;
  for(int i=0;i<4096;i++) buf[i]=(unsigned char)(i*37+11);
  memmove(buf+16,buf,2000);          // forward overlap
  memmove(buf,buf+16,2000);          // backward overlap
  memset(buf+3000,0xAB,500);         // stos
  void*p=memchr(buf,0xAB,4096);      // scas
  int c=memcmp(buf,buf+2048,1024);   // cmps
  size_t n=strnlen((char*)buf,4096);
  for(int i=0;i<4096;i++) h=h*1000003ULL+buf[i];
  printf("str=%016llx off=%ld cmp=%d n=%zu\n",(unsigned long long)h,(long)((unsigned char*)p-buf),(c>0)-(c<0),n);
  return 0;
}
