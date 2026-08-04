#include <stdio.h>
int main(void){ unsigned long long a=0,b=1; for(int i=0;i<50;i++){unsigned long long t=a+b;a=b;b=t;} printf("R=%llu\n",a); return 0; }
