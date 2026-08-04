// Multi-word add/sub carry/borrow chains (x86 ADC/SBB codegen) via __int128 limb propagation.
#include <stdio.h>
#include <stdint.h>
#define N 24
int main(void){
  uint64_t a[N],b[N],s[N],d[N];
  for(int i=0;i<N;i++){ a[i]=0x9E3779B97F4A7C15ULL*(i+1); b[i]=0xC2B2AE3D27D4EB4FULL^(uint64_t)i*0x1000193ULL; }
  unsigned __int128 c=0; for(int i=0;i<N;i++){ unsigned __int128 t=(unsigned __int128)a[i]+b[i]+c; s[i]=(uint64_t)t; c=t>>64; }
  uint64_t addcarry=(uint64_t)c;
  __int128 br=0; for(int i=0;i<N;i++){ __int128 t=(__int128)a[i]-b[i]-br; d[i]=(uint64_t)t; br=(t<0)?1:0; }
  uint64_t h1=0,h2=0; for(int i=0;i<N;i++){ h1=h1*1000003ULL+s[i]; h2=h2*1000003ULL+d[i]; }
  printf("sum=%016llx dif=%016llx carry=%llu\n",(unsigned long long)h1,(unsigned long long)h2,(unsigned long long)addcarry);
  return 0;
}
