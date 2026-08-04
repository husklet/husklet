#define _GNU_SOURCE
#include <stdio.h>
#include <sys/random.h>
int main(){char b[16]; ssize_t n=getrandom(b,16,0); printf("GETRANDOM=%zd\n",n); return 0;}
