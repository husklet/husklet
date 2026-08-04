#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
static void mark(const char *s){ write(2, s, strlen(s)); }
int main(void){
    long ps = sysconf(_SC_PAGESIZE);
    mark("A both\n");
    void *p = mmap(NULL, ps, PROT_READ|PROT_WRITE, MAP_ANONYMOUS|MAP_PRIVATE|MAP_SHARED, -1, 0);
    if (p!=MAP_FAILED) munmap(p, ps);
    mark("B neither\n");
    p = mmap(NULL, ps, PROT_READ|PROT_WRITE, MAP_ANONYMOUS, -1, 0);
    if (p!=MAP_FAILED) munmap(p, ps);
    mark("C huge\n");
    p = mmap(NULL, (size_t)-1-ps, PROT_READ|PROT_WRITE, MAP_ANONYMOUS|MAP_PRIVATE, -1, 0);
    if (p!=MAP_FAILED) munmap(p, (size_t)-1-ps);
    mark("D misalign-fixed\n");
    p = mmap((void*)(uintptr_t)(ps+1), ps, PROT_READ|PROT_WRITE, MAP_ANONYMOUS|MAP_PRIVATE|MAP_FIXED, -1, 0);
    if (p!=MAP_FAILED) munmap(p, ps);
    mark("E anon-fd\n");
    p = mmap(NULL, ps, PROT_READ|PROT_WRITE, MAP_ANONYMOUS|MAP_PRIVATE, 99999, 0);
    if (p!=MAP_FAILED) munmap(p, ps);
    mark("Z done\n");
    printf("done\n");
    return 0;
}
