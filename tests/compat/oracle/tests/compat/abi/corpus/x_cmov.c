// CMOVcc all conditions: branchless selects that the compiler lowers to conditional moves.
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint64_t acc=0; int64_t x=1,y=-1;
  for(int i=0;i<5000;i++){
    int64_t a=(int64_t)(i*2246822519u)-0x7fffffff, b=(int64_t)(i*3266489917u)-0x3fffffff;
    int64_t m1 = (a<b)? a : b;             // signed min
    uint64_t m2 = ((uint64_t)a<(uint64_t)b)? (uint64_t)a:(uint64_t)b; // unsigned min
    int64_t m3 = (a<0)? -a : a;            // abs via cmov
    int64_t m4 = ((a^b)<0)? a : b;         // sign-differs select
    x = (m1>y)? m1 : y; y = (m2>(uint64_t)x)? (int64_t)m2 : y;
    acc = acc*1000003ULL + (uint64_t)(m1^m3) + m2 + (uint64_t)m4;
  }
  printf("cmov=%016llx x=%lld y=%lld\n",(unsigned long long)acc,(long long)x,(long long)y);
  return 0;
}
