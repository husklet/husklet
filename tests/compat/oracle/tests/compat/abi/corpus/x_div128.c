// 128/64 and 128/128 unsigned+signed division & remainder (x86 DIV/IDIV full-width, __divti3/__udivti3).
#include <stdio.h>
#include <stdint.h>
static void emit(const char*t,unsigned __int128 v,uint64_t*h){ *h=*h*1000003ULL+(uint64_t)(v>>64); *h=*h*1000003ULL+(uint64_t)v; (void)t; }
int main(void){
  uint64_t h=0;
  unsigned __int128 n=((unsigned __int128)0xDEADBEEFCAFEBABEULL<<64)|0x0123456789ABCDEFULL;
  for(uint64_t d=1; d<200000; d+=997){
    emit("u",n/d,&h); emit("r",n%d,&h);
    __int128 sn=(__int128)n; __int128 sd=(__int128)(int64_t)(d-100000);
    if(sd!=0){ emit("s",(unsigned __int128)(sn/sd),&h); emit("m",(unsigned __int128)(sn%sd),&h); }
  }
  // 128/128 case
  unsigned __int128 big=((unsigned __int128)0x00000000000000FFULL<<64)|0xFFFFFFFFFFFFFFFFULL;
  emit("bq",n/big,&h); emit("br",n%big,&h);
  printf("div=%016llx\n",(unsigned long long)h);
  return 0;
}
