#include <stdio.h>
int main(void){static char c[1000000];int n=0;for(long i=2;i<1000000;i++){if(!c[i]){n++;for(long j=i*i;j<1000000;j+=i)c[j]=1;}}printf("PRIMES %d\n",n);return 0;}
