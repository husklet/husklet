#define _GNU_SOURCE
#include <stdio.h>
#include <sched.h>
int main(){
  cpu_set_t s; CPU_ZERO(&s);
  if(sched_getaffinity(0,sizeof s,&s)<0){perror("aff");return 1;}
  printf("AFFINITY=%s\n", CPU_COUNT(&s)>0?"nonzero":"zero"); return 0;
}
