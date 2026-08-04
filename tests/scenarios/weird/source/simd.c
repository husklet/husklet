#include <stdio.h>
#if defined(__x86_64__)
#include <cpuid.h>
int main(){ unsigned a,b,c,d; __get_cpuid(1,&a,&b,&c,&d);
  printf("SIMD=%s\n",(d&(1u<<26))?"ok":"no"); return 0; }
#elif defined(__aarch64__)
#include <sys/auxv.h>
int main(){ unsigned long h=getauxval(AT_HWCAP);
  printf("SIMD=%s\n",(h&(1u<<1))?"ok":"no"); return 0; }
#endif
