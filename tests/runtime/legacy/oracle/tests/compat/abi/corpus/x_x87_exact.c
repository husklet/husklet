// x87 80-bit FP stack (x86 long double => FLD/FMUL/FADD/FISTP/FRNDINT) on exactly-representable integers.
#include <stdio.h>
#include <stdint.h>
#include <math.h>
int main(void){
  uint64_t h=0;
  long double acc=1.0L;
  for(int i=1;i<=20;i++){ acc*= (long double)i; }          // 20! overflows 64-bit but exact in both mantissas? 20!=2.4e18 < 2^63 ok
  long long fact=(long long)acc; h=h*1000003ULL+(uint64_t)fact;
  for(int i=0;i<5000;i++){
    long double x=(long double)((i*37)%1000) + 0.5L;
    long double y=(long double)((i*13)%997) + 0.25L;
    long double s=x+y, d=x-y, p=x*y;
    h=h*1000003ULL+(uint64_t)(long long)rintl(s*4.0L);   // exact quarters -> integers
    h=h*1000003ULL+(uint64_t)(long long)floorl(d);
    h=h*1000003ULL+(uint64_t)(long long)ceill(d);
    h=h*1000003ULL+(uint64_t)(long long)rintl(p*8.0L);
    long double q=truncl((long double)(i*i)/ (long double)((i%7)+1));
    h=h*1000003ULL+(uint64_t)(long long)q;
  }
  printf("x87=%016llx fact20=%lld\n",(unsigned long long)h,fact);
  return 0;
}
