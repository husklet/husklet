#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
static void mark(const char *s){ write(2, s, strlen(s)); }
int main(void){
    long ps = sysconf(_SC_PAGESIZE);
    mark("A sbrk0\n");
    void *base = sbrk(0);
    mark("B grow\n");
    void *grown = sbrk(ps);
    if (grown==(void*)-1){ mark("grow-failed\n"); return 3; }
    mark("C read\n");
    volatile unsigned char *r = grown;
    unsigned char v = r[0]; (void)v;
    mark("D write\n");
    r[0] = 0xab; r[ps-1]=0xcd;
    mark("E verify\n");
    int ok = r[0]==0xab && r[ps-1]==0xcd;
    mark("F shrink\n");
    sbrk(-ps);
    (void)base;
    printf("brk_ok=%d\n", ok);
    mark("Z done\n");
    return 0;
}
