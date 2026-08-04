#include <stdio.h>
#include <sys/auxv.h>
int main(){
  unsigned long hw=getauxval(AT_HWCAP);   /* exercise AT_HWCAP read */
  unsigned long ps=getauxval(AT_PAGESZ);
  (void)hw;
  printf("AUXV=%lu\n", ps); return 0;
}
