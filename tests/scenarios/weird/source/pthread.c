#include <stdio.h>
#include <pthread.h>
#include <stdatomic.h>
atomic_long c=0;
void*w(void*x){ (void)x; for(int i=0;i<100000;i++) atomic_fetch_add(&c,1); return 0; }
int main(){ pthread_t t[8]; for(int i=0;i<8;i++)pthread_create(&t[i],0,w,0);
  for(int i=0;i<8;i++)pthread_join(t[i],0);
  printf("THREADS=%ld\n",(long)c); return 0;}
