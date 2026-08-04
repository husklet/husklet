// LEA / complex SIB addressing (base+index*scale+disp) and address-arithmetic identities.
#include <stdio.h>
#include <stdint.h>
int main(void){
  static int32_t arr[1024]; for(int i=0;i<1024;i++) arr[i]=i*7-500;
  uint64_t h=0; uint64_t x=3;
  for(int i=0;i<20000;i++){
    x=x*1103515245ULL+12345ULL;
    unsigned base=(x>>3)&511, idx=(x>>11)&255;
    int32_t v = arr[base + idx*2];          // scale-8 index
    int32_t w = *(arr + base + (idx&1?idx:0)); 
    uint64_t lea = (uint64_t)base*5 + idx*9 + 7;   // lea base + base*4 patterns
    h=h*1000003ULL+(uint32_t)v+(uint32_t)w+lea;
  }
  printf("lea=%016llx\n",(unsigned long long)h);
  return 0;
}
