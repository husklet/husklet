// SSE scalar/packed float with subnormals: default MXCSR (FTZ=0,DAZ=0) must preserve denormals,
// matching aarch64 default FPCR (FZ=0). A translator that forces flush-to-zero diverges.
#include <stdio.h>
#include <stdint.h>
#include <math.h>
#include <string.h>
static uint64_t bits64(double d){ uint64_t u; memcpy(&u,&d,8); return u; }
static uint32_t bits32(float f){ uint32_t u; memcpy(&u,&f,4); return u; }
int main(void){
  uint64_t h=0;
  float fmin = 1.17549435e-38f;          // smallest normal float
  double dmin = 2.2250738585072014e-308; // smallest normal double
  for(int i=1;i<=64;i++){
    float fs = fmin / (float)(1<<(i%24));   // subnormal floats
    double ds = dmin / (double)(1ULL<<(i%52));
    h = h*1000003ULL + bits32(fs);
    h = h*1000003ULL + bits64(ds);
    h = h*1000003ULL + bits32(fs*2.0f);
    h = h*1000003ULL + bits64(ds+ds);
    h = h*1000003ULL + bits32(fs+fmin);
    volatile float acc=0; for(int k=0;k<8;k++) acc+=fs; // packed accumulation of denormals
    h = h*1000003ULL + bits32(acc);
  }
  // vector of denormals summed
  float v[8]; for(int i=0;i<8;i++) v[i]=fmin/(float)(1<<(i+1));
  float sum=0; for(int i=0;i<8;i++) sum+=v[i];
  h = h*1000003ULL + bits32(sum);
  printf("denorm=%016llx\n",(unsigned long long)h);
  return 0;
}
