#include <stdio.h>
#include <sys/prctl.h>
int main(){
  prctl(PR_SET_NAME,"hl-proc");
  char n[16]={0}; prctl(PR_GET_NAME,n);
  printf("PRCTL=%s\n",n); return 0;
}
